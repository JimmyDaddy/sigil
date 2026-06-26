use std::{
    collections::BTreeMap,
    path::Path,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::{
    PlanApprovalExpiry,
    approval::{ApprovalHandler, AutoApproveHandler, ToolApproval},
    config::{CompactionConfig, MemoryConfig},
    event::{DurableEventType, EventClass, EventHandler, RunEvent},
    permission::{
        ApprovalMode, InteractionMode, PermissionConfig, PermissionDecision,
        PermissionEvaluationContext, PermissionPolicy, PermissionRisk,
    },
    provider::{ModelMessage, Provider, ProviderChunk, ProviderContinuationState, ToolCall},
    session::{
        ControlEntry, Session, ToolApprovalAuditAction, ToolApprovalEntry,
        ToolApprovalUserDecision, ToolEgressEntry, ToolExecutionEntry, ToolExecutionStatus,
        ToolSubjectAudit,
    },
    task::{
        TASK_PLAN_UPDATE_TOOL_NAME, TaskPlanUpdateContext, task_plan_update_entry,
        task_plan_update_result_content, task_plan_update_tool_spec,
    },
    terminal_task::TerminalTaskEntry,
    time::saturating_elapsed,
    tool::{
        ToolCategory, ToolContext, ToolDiffBudget, ToolEgressAudit, ToolErrorKind, ToolPreview,
        ToolPreviewSnapshot, ToolRegistry, ToolResult, ToolResultMeta, ToolResultStatus, ToolSpec,
        ToolSubject, ToolSubjectScope,
    },
};

/// Runtime knobs for one agent run.
#[derive(Debug, Clone)]
pub struct AgentRunOptions {
    pub workspace_root: std::path::PathBuf,
    pub max_turns: Option<usize>,
    pub tool_timeout_secs: u64,
    pub reasoning_effort: Option<crate::provider::ReasoningEffort>,
    pub traffic_partition_key: Option<String>,
    pub interaction_mode: InteractionMode,
    pub permission_config: PermissionConfig,
    pub permission_context: PermissionEvaluationContext,
    pub memory_config: MemoryConfig,
    pub compaction_config: CompactionConfig,
}

/// Final aggregate result from one completed agent run.
#[derive(Debug, Clone)]
pub struct AgentRunResult {
    pub final_text: String,
    pub tool_calls: usize,
    pub final_message_id: Option<String>,
}

/// Input contract for one agent run.
#[derive(Debug, Clone)]
pub struct AgentRunInput {
    pub persisted_user_message: Option<String>,
    pub transient_context: Vec<ModelMessage>,
    pub task_plan_update: Option<TaskPlanUpdateContext>,
    pub agent_delegation: Option<AgentDelegationRequirement>,
}

impl AgentRunInput {
    pub fn user(prompt: impl Into<String>) -> Self {
        Self {
            persisted_user_message: Some(prompt.into()),
            transient_context: Vec::new(),
            task_plan_update: None,
            agent_delegation: None,
        }
    }

    pub fn transient(prompt: impl Into<String>, transient_context: Vec<ModelMessage>) -> Self {
        Self {
            persisted_user_message: Some(prompt.into()),
            transient_context,
            task_plan_update: None,
            agent_delegation: None,
        }
    }

    pub fn without_persisted_user_message(transient_context: Vec<ModelMessage>) -> Self {
        Self {
            persisted_user_message: None,
            transient_context,
            task_plan_update: None,
            agent_delegation: None,
        }
    }

    pub fn with_task_plan_update(mut self, context: TaskPlanUpdateContext) -> Self {
        self.task_plan_update = Some(context);
        self
    }

    pub fn with_agent_delegation_requirement(
        mut self,
        requirement: AgentDelegationRequirement,
    ) -> Self {
        self.agent_delegation = Some(requirement);
        self
    }
}

/// A per-run guard that requires at least one successful model-visible agent-thread tool result
/// before a final answer can be accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentDelegationRequirement {
    pub reason: String,
}

impl AgentDelegationRequirement {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }

    fn retry_prompt(&self) -> String {
        format!(
            "Delegation requirement not yet satisfied: {}. Before giving a final answer, call an agent-thread tool such as spawn_agent for the delegated scope, wait for the result when needed, then summarize.",
            self.reason
        )
    }
}

/// Complete result and state summary for task orchestration callers.
#[derive(Debug, Clone)]
pub struct AgentRunOutput {
    pub result: AgentRunResult,
    pub outcome: AgentRunOutcome,
}

/// Outcome summary derived from provider chunks, approvals, and tool results.
#[derive(Debug, Clone, Default)]
pub struct AgentRunOutcome {
    pub terminal_reason: AgentRunTerminalReason,
    pub tool_calls: usize,
    pub tool_errors: Vec<crate::tool::ToolError>,
    pub approval_denials: usize,
    pub changed_files: Vec<String>,
    pub interrupted_tool_calls: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AgentRunTerminalReason {
    #[default]
    FinalAnswer,
    MaxTurns,
    DelegationUnsatisfied,
}

impl AgentRunTerminalReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::FinalAnswer => "final_answer",
            Self::MaxTurns => "max_turns",
            Self::DelegationUnsatisfied => "delegation_unsatisfied",
        }
    }
}

/// Runtime hook for model-visible agent-thread tools.
///
/// Kernel owns the provider-neutral tool-call loop and permission audit. Runtime adapters can
/// implement this hook to connect approved `spawn_agent` / `wait_agent` style calls to an
/// agent supervisor without making kernel depend on runtime.
#[async_trait]
pub trait AgentToolDelegate: Send {
    /// Handles one agent tool call after normal permission approval has resolved.
    ///
    /// Return `Ok(None)` when the call is not an agent-thread tool and should continue through the
    /// regular tool registry. Returned tool results may include durable control entries.
    ///
    /// # Errors
    ///
    /// Returns an error when the delegated agent action fails before it can be represented as a
    /// structured [`ToolResult`].
    async fn handle_agent_tool_call(
        &mut self,
        session: &mut Session,
        call: &ToolCall,
        options: &AgentRunOptions,
        handler: &mut (dyn EventHandler + Send),
        approval_handler: &mut (dyn ApprovalHandler + Send),
    ) -> Result<Option<ToolResult>>;
}

/// Provider-backed agent loop with a registered tool surface.
pub struct Agent<P> {
    provider: P,
    tools: ToolRegistry,
}

impl<P> Agent<P>
where
    P: Provider,
{
    /// Creates a new agent from one provider implementation and tool registry.
    pub fn new(provider: P, tools: ToolRegistry) -> Self {
        Self { provider, tools }
    }

    /// Returns the registered tool surface used by this agent.
    pub fn tool_registry(&self) -> &ToolRegistry {
        &self.tools
    }

    /// Returns the provider capability flags for this agent.
    pub fn provider_capabilities(&self) -> crate::provider::ProviderCapabilities {
        self.provider.capabilities()
    }

    /// Returns the mutable registered tool surface used by this agent.
    pub fn tool_registry_mut(&mut self) -> &mut ToolRegistry {
        &mut self.tools
    }

    /// Runs the agent with automatic tool approval.
    ///
    /// # Errors
    ///
    /// Returns an error when session persistence fails, request building fails, the provider
    /// stream errors, the event sink fails, or a tool execution path fails before it can be
    /// surfaced as a structured tool result.
    pub async fn run(
        &self,
        session: &mut Session,
        prompt: impl Into<String>,
        options: AgentRunOptions,
        handler: &mut (impl EventHandler + Send),
    ) -> Result<AgentRunResult> {
        let mut approval_handler = AutoApproveHandler;
        self.run_with_approval(session, prompt, options, handler, &mut approval_handler)
            .await
    }

    /// Runs the agent with an explicit approval handler for mutating tools.
    ///
    /// # Errors
    ///
    /// Returns an error when session persistence fails, request building fails, the provider
    /// stream errors, the event sink fails, or the approval handler itself errors.
    pub async fn run_with_approval<H, A>(
        &self,
        session: &mut Session,
        prompt: impl Into<String>,
        options: AgentRunOptions,
        handler: &mut H,
        approval_handler: &mut A,
    ) -> Result<AgentRunResult>
    where
        H: EventHandler + Send,
        A: ApprovalHandler + Send,
    {
        Ok(self
            .run_with_approval_input(
                session,
                AgentRunInput::user(prompt),
                options,
                handler,
                approval_handler,
            )
            .await?
            .result)
    }

    /// Runs the agent from an explicit input contract with automatic approval.
    ///
    /// # Errors
    ///
    /// Returns an error when the underlying run fails.
    pub async fn run_with_input<H>(
        &self,
        session: &mut Session,
        input: AgentRunInput,
        options: AgentRunOptions,
        handler: &mut H,
    ) -> Result<AgentRunOutput>
    where
        H: EventHandler + Send,
    {
        let mut approval_handler = AutoApproveHandler;
        self.run_with_approval_input(session, input, options, handler, &mut approval_handler)
            .await
    }

    /// Runs the agent from an explicit input contract with an explicit approval handler.
    ///
    /// # Errors
    ///
    /// Returns an error when session persistence fails, request building fails, the provider
    /// stream errors, or the approval handler itself errors.
    pub async fn run_with_approval_input<H, A>(
        &self,
        session: &mut Session,
        input: AgentRunInput,
        options: AgentRunOptions,
        handler: &mut H,
        approval_handler: &mut A,
    ) -> Result<AgentRunOutput>
    where
        H: EventHandler + Send,
        A: ApprovalHandler + Send,
    {
        self.run_with_approval_input_and_tools(
            session,
            input,
            options,
            &self.tools,
            handler,
            approval_handler,
            None,
        )
        .await
    }

    /// Runs the agent from an explicit input contract with runtime-handled agent tools.
    ///
    /// # Errors
    ///
    /// Returns an error when the underlying run or delegation hook fails.
    pub async fn run_with_approval_input_and_agent_delegate<H, A>(
        &self,
        session: &mut Session,
        input: AgentRunInput,
        options: AgentRunOptions,
        handler: &mut H,
        approval_handler: &mut A,
        agent_delegate: &mut (dyn AgentToolDelegate + Send),
    ) -> Result<AgentRunOutput>
    where
        H: EventHandler + Send,
        A: ApprovalHandler + Send,
    {
        self.run_with_approval_input_and_tools(
            session,
            input,
            options,
            &self.tools,
            handler,
            approval_handler,
            Some(agent_delegate),
        )
        .await
    }

    /// Runs the agent with a temporary tool registry view.
    ///
    /// # Errors
    ///
    /// Returns an error when session persistence fails, request building fails, the provider
    /// stream errors, or the approval handler itself errors.
    pub async fn run_with_approval_input_and_tool_registry<H, A>(
        &self,
        session: &mut Session,
        input: AgentRunInput,
        options: AgentRunOptions,
        tools: ToolRegistry,
        handler: &mut H,
        approval_handler: &mut A,
    ) -> Result<AgentRunOutput>
    where
        H: EventHandler + Send,
        A: ApprovalHandler + Send,
    {
        self.run_with_approval_input_and_tools(
            session,
            input,
            options,
            &tools,
            handler,
            approval_handler,
            None,
        )
        .await
    }

    /// Runs the agent with a temporary tool registry and runtime-handled agent tools.
    ///
    /// # Errors
    ///
    /// Returns an error when the underlying run or delegation hook fails.
    #[allow(clippy::too_many_arguments)]
    pub async fn run_with_approval_input_tool_registry_and_agent_delegate<H, A>(
        &self,
        session: &mut Session,
        input: AgentRunInput,
        options: AgentRunOptions,
        tools: ToolRegistry,
        handler: &mut H,
        approval_handler: &mut A,
        agent_delegate: &mut (dyn AgentToolDelegate + Send),
    ) -> Result<AgentRunOutput>
    where
        H: EventHandler + Send,
        A: ApprovalHandler + Send,
    {
        self.run_with_approval_input_and_tools(
            session,
            input,
            options,
            &tools,
            handler,
            approval_handler,
            Some(agent_delegate),
        )
        .await
    }

    async fn run_with_approval_input_and_tools<H, A>(
        &self,
        session: &mut Session,
        input: AgentRunInput,
        options: AgentRunOptions,
        tools: &ToolRegistry,
        handler: &mut H,
        approval_handler: &mut A,
        mut agent_delegate: Option<&mut (dyn AgentToolDelegate + Send)>,
    ) -> Result<AgentRunOutput>
    where
        H: EventHandler + Send,
        A: ApprovalHandler + Send,
    {
        let AgentRunInput {
            persisted_user_message,
            mut transient_context,
            task_plan_update,
            agent_delegation,
        } = input;

        session.reconcile_prepared_mutations(&options.workspace_root)?;

        if let Some(message) = persisted_user_message {
            session.append_user_message(ModelMessage::user(message))?;
        }

        let permission_policy = PermissionPolicy::new_with_context(
            &options.permission_config,
            &options.permission_context,
        );
        let mut previous_response_handle = session.latest_response_handle(self.provider.name());
        let mut total_tool_calls = 0usize;
        let mut outcome = AgentRunOutcome::default();
        let agent_delegation_enforced =
            agent_delegation.filter(|_| tool_registry_has_agent_tools(tools));
        let mut satisfied_agent_tool_calls = 0usize;
        let mut delegation_retry_used = false;

        let mut model_turns = 0usize;
        loop {
            if let Some(max_turns) = options.max_turns
                && model_turns >= max_turns
            {
                handler.handle(RunEvent::Notice(format!(
                    "Stopped after {model_turns} model turns: the model kept requesting tools and did not return a final answer. Send another message to continue from the recorded tool results."
                )))?;
                outcome.terminal_reason = AgentRunTerminalReason::MaxTurns;
                outcome.tool_calls = total_tool_calls;
                append_run_lifecycle_events(
                    session,
                    "interrupted",
                    outcome.terminal_reason,
                    None,
                    total_tool_calls,
                )?;
                return Ok(AgentRunOutput {
                    result: AgentRunResult {
                        final_text: String::new(),
                        tool_calls: total_tool_calls,
                        final_message_id: None,
                    },
                    outcome,
                });
            }
            model_turns = model_turns.saturating_add(1);

            let mut tool_specs = tools.specs();
            if task_plan_update.is_some() {
                tool_specs.push(task_plan_update_tool_spec());
            }
            let request = session.build_request_with_transient_messages(
                &options.workspace_root,
                &options.memory_config,
                tool_specs,
                options.reasoning_effort.clone(),
                previous_response_handle.clone(),
                options.traffic_partition_key.clone(),
                &transient_context,
            )?;

            let mut stream = match self.provider.stream(request).await {
                Ok(stream) => stream,
                Err(error) => {
                    let error_message = format!("{error:#}");
                    append_failed_run_lifecycle_events(
                        session,
                        "provider_request_error",
                        total_tool_calls,
                        &error_message,
                    )?;
                    return Err(error);
                }
            };
            let mut assistant_text = String::new();
            let mut reasoning_buffer = String::new();
            let mut reasoning_trace_buffer = String::new();
            let mut tool_parts: BTreeMap<String, (String, String)> = BTreeMap::new();
            let mut completed_calls: Vec<ToolCall> = Vec::new();
            let mut pending_states: Vec<ProviderContinuationState> = Vec::new();

            while let Some(chunk) = stream.next().await {
                let chunk = match chunk.context("provider stream failed") {
                    Ok(chunk) => chunk,
                    Err(error) => {
                        let error_message = format!("{error:#}");
                        append_failed_run_lifecycle_events(
                            session,
                            "provider_stream_error",
                            total_tool_calls,
                            &error_message,
                        )?;
                        return Err(error);
                    }
                };
                match chunk {
                    ProviderChunk::TextDelta(delta) => {
                        assistant_text.push_str(&delta);
                        handler.handle(RunEvent::TextDelta(delta))?;
                    }
                    ProviderChunk::ReasoningDelta(delta) => {
                        reasoning_buffer.push_str(&delta);
                        reasoning_trace_buffer.push_str(&delta);
                        handler.handle(RunEvent::ReasoningDelta(delta))?;
                    }
                    ProviderChunk::ReasoningSummaryDelta(delta) => {
                        reasoning_trace_buffer.push_str(&delta);
                        handler.handle(RunEvent::ReasoningDelta(delta))?;
                    }
                    ProviderChunk::ToolCallStart { id, name } => {
                        tool_parts.insert(id.clone(), (name.clone(), String::new()));
                        handler.handle(RunEvent::ToolCallStarted(ToolCall {
                            id,
                            name,
                            args_json: String::new(),
                        }))?;
                    }
                    ProviderChunk::ToolCallArgsDelta { id, delta } => {
                        if let Some((_, args_json)) = tool_parts.get_mut(&id) {
                            args_json.push_str(&delta);
                        }
                        handler.handle(RunEvent::ToolCallArgsDelta { id, delta })?;
                    }
                    ProviderChunk::ToolCallComplete(call) => {
                        completed_calls.push(call.clone());
                        handler.handle(RunEvent::ToolCallCompleted(call))?;
                    }
                    ProviderChunk::Usage(usage) => {
                        session.stats_mut().apply_usage(&usage);
                        session.append_control(ControlEntry::UsageSnapshot(usage.clone()))?;
                        handler.handle(RunEvent::Usage(usage))?;
                    }
                    ProviderChunk::ResponseHandle(handle) => {
                        previous_response_handle = Some(handle.clone());
                        let control = ControlEntry::ResponseHandleTracked(handle);
                        session.append_control(control.clone())?;
                        handler.handle(RunEvent::Control(control))?;
                    }
                    ProviderChunk::BackgroundTaskAccepted(handle) => {
                        let control = ControlEntry::BackgroundTaskTracked(handle);
                        session.append_control(control.clone())?;
                        handler.handle(RunEvent::Control(control))?;
                    }
                    ProviderChunk::BackgroundTaskStatus(status) => {
                        handler.handle(RunEvent::Notice(format!(
                            "background task {} status {}",
                            status.task_id, status.status
                        )))?;
                    }
                    ProviderChunk::ReasoningArtifact(_) => {}
                    ProviderChunk::ContinuationState(state) => {
                        pending_states.push(state.clone());
                        handler.handle(RunEvent::ContinuationState(state))?;
                    }
                    ProviderChunk::Done => break,
                }
            }

            append_reasoning_trace(session, &reasoning_trace_buffer)?;

            if !completed_calls.is_empty() {
                total_tool_calls += completed_calls.len();
                let completed_agent_tool_calls = count_agent_tool_calls(tools, &completed_calls);
                let assistant_content = if completed_agent_tool_calls > 0 {
                    None
                } else {
                    (!assistant_text.trim().is_empty()).then(|| assistant_text.clone())
                };
                let assistant_message =
                    ModelMessage::assistant(assistant_content, completed_calls.clone());
                let assistant_message_id = assistant_message.id.clone();
                session.append_assistant_message(assistant_message.clone())?;
                handler.handle(RunEvent::AssistantMessage(assistant_message))?;

                for state in &mut pending_states {
                    if state.message_id.is_none() {
                        state.message_id = Some(assistant_message_id.clone());
                    }
                }

                for state in pending_states {
                    let control = ControlEntry::ContinuationStateSaved(state);
                    session.append_control(control.clone())?;
                    handler.handle(RunEvent::Control(control))?;
                }

                let mut tool_ctx =
                    ToolContext::new(options.workspace_root.clone(), options.tool_timeout_secs);
                if let Some(recorder) = session.mutation_event_recorder() {
                    tool_ctx = tool_ctx.with_mutation_recorder(recorder);
                }
                for mut call in completed_calls {
                    if call.name == TASK_PLAN_UPDATE_TOOL_NAME {
                        let Some(context) = task_plan_update.as_ref() else {
                            let mut result = ToolResult::error(
                                call.id.clone(),
                                call.name.clone(),
                                ToolErrorKind::Unsupported,
                                "task_plan_update is not available for this run",
                            );
                            attach_tool_call_context(&mut result, &call, &[]);
                            append_tool_execution_audit(
                                session,
                                &call,
                                &[],
                                ToolExecutionStatus::Failed,
                                None,
                                Some(&result),
                            )?;
                            record_tool_run_outcome(&mut outcome, &result);
                            session.append_tool_message(result.to_model_message())?;
                            handler.handle(RunEvent::ToolResult(result))?;
                            continue;
                        };
                        handle_task_plan_update_call(
                            session,
                            handler,
                            &mut outcome,
                            &call,
                            context,
                        )?;
                        continue;
                    }
                    if let Some(mut result) =
                        direct_task_tool_guidance_result(&call, task_plan_update.is_some())
                    {
                        attach_tool_call_context(&mut result, &call, &[]);
                        append_tool_execution_audit(
                            session,
                            &call,
                            &[],
                            ToolExecutionStatus::Started,
                            None,
                            None,
                        )?;
                        append_tool_execution_audit(
                            session,
                            &call,
                            &[],
                            ToolExecutionStatus::Completed,
                            None,
                            Some(&result),
                        )?;
                        session.append_tool_message(result.to_model_message())?;
                        handler.handle(RunEvent::ToolResult(result))?;
                        continue;
                    }
                    let mut execution_subjects = Vec::new();
                    let mut tool_registered = false;
                    let mut tool_is_agent_category = false;
                    if let Some(spec) = tools.spec_for(&call.name) {
                        tool_registered = true;
                        tool_is_agent_category = spec.category == ToolCategory::Agent;
                        let subjects = match tools.permission_subjects(&tool_ctx, &call) {
                            Ok(subjects) => subjects,
                            Err(error) => {
                                let mut result = ToolResult::error(
                                    call.id.clone(),
                                    call.name.clone(),
                                    ToolErrorKind::InvalidInput,
                                    format!("invalid tool arguments for {}: {error}", call.name),
                                );
                                attach_tool_call_context(&mut result, &call, &[]);
                                append_tool_execution_audit(
                                    session,
                                    &call,
                                    &[],
                                    ToolExecutionStatus::Failed,
                                    None,
                                    Some(&result),
                                )?;
                                record_tool_run_outcome(&mut outcome, &result);
                                session.append_tool_message(result.to_model_message())?;
                                handler.handle(RunEvent::ToolResult(result))?;
                                continue;
                            }
                        };
                        let access = match tools.permission_access(&tool_ctx, &call) {
                            Ok(access) => access,
                            Err(error) => {
                                let mut result = ToolResult::error(
                                    call.id.clone(),
                                    call.name.clone(),
                                    ToolErrorKind::InvalidInput,
                                    format!("invalid tool arguments for {}: {error}", call.name),
                                );
                                attach_tool_call_context(&mut result, &call, &subjects);
                                append_tool_execution_audit(
                                    session,
                                    &call,
                                    &subjects,
                                    ToolExecutionStatus::Failed,
                                    None,
                                    Some(&result),
                                )?;
                                record_tool_run_outcome(&mut outcome, &result);
                                session.append_tool_message(result.to_model_message())?;
                                handler.handle(RunEvent::ToolResult(result))?;
                                continue;
                            }
                        };
                        let operation = match tools.permission_operation(&tool_ctx, &call) {
                            Ok(operation) => operation,
                            Err(error) => {
                                let mut result = ToolResult::error(
                                    call.id.clone(),
                                    call.name.clone(),
                                    ToolErrorKind::InvalidInput,
                                    format!("invalid tool arguments for {}: {error}", call.name),
                                );
                                attach_tool_call_context(&mut result, &call, &subjects);
                                append_tool_execution_audit(
                                    session,
                                    &call,
                                    &subjects,
                                    ToolExecutionStatus::Failed,
                                    None,
                                    Some(&result),
                                )?;
                                record_tool_run_outcome(&mut outcome, &result);
                                session.append_tool_message(result.to_model_message())?;
                                handler.handle(RunEvent::ToolResult(result))?;
                                continue;
                            }
                        };
                        let tool_default_mode = match tools
                            .permission_default_mode(&tool_ctx, &call)
                        {
                            Ok(mode) => mode,
                            Err(error) => {
                                let mut result = ToolResult::error(
                                    call.id.clone(),
                                    call.name.clone(),
                                    ToolErrorKind::InvalidInput,
                                    format!("invalid tool arguments for {}: {error}", call.name),
                                );
                                attach_tool_call_context(&mut result, &call, &subjects);
                                append_tool_execution_audit(
                                    session,
                                    &call,
                                    &subjects,
                                    ToolExecutionStatus::Failed,
                                    None,
                                    Some(&result),
                                )?;
                                record_tool_run_outcome(&mut outcome, &result);
                                session.append_tool_message(result.to_model_message())?;
                                handler.handle(RunEvent::ToolResult(result))?;
                                continue;
                            }
                        };
                        let decision = permission_policy.decide_with_operation_and_default(
                            &spec,
                            &call.name,
                            access,
                            operation,
                            subjects.clone(),
                            tool_default_mode,
                        )?;
                        let decision = plan_approval_decision_override(session, &spec, decision);
                        let subject_label = if decision.subjects.is_empty() {
                            "-".to_owned()
                        } else {
                            decision
                                .subjects
                                .iter()
                                .map(|subject| subject.normalized.as_str())
                                .collect::<Vec<_>>()
                                .join(",")
                        };
                        handler.handle(RunEvent::Notice(format!(
                            "permission {} subject={} mode={}",
                            call.name,
                            subject_label,
                            decision.mode.as_str()
                        )))?;
                        append_tool_approval_audit(
                            session,
                            &call,
                            &decision,
                            ToolApprovalAuditAction::PolicyEvaluated,
                            None,
                            None,
                            None,
                        )?;
                        execution_subjects = decision.subjects.clone();

                        match decision.mode {
                            ApprovalMode::Allow => {}
                            ApprovalMode::Ask
                                if options.interaction_mode == InteractionMode::Headless =>
                            {
                                let reason = format!(
                                    "tool {} requires approval in headless mode",
                                    call.name
                                );
                                append_tool_approval_audit(
                                    session,
                                    &call,
                                    &decision,
                                    ToolApprovalAuditAction::Resolved,
                                    Some(ToolApprovalUserDecision::Denied),
                                    Some(reason.clone()),
                                    None,
                                )?;
                                let mut result = ToolResult::error(
                                    call.id.clone(),
                                    call.name.clone(),
                                    ToolErrorKind::ApprovalRequired,
                                    reason,
                                );
                                attach_tool_call_context(&mut result, &call, &decision.subjects);
                                record_tool_run_outcome(&mut outcome, &result);
                                session.append_tool_message(result.to_model_message())?;
                                handler.handle(RunEvent::ToolResult(result))?;
                                continue;
                            }
                            ApprovalMode::Ask => {
                                let mut preview_error = None;
                                let preview = if has_external_subject(&decision.subjects) {
                                    Some(external_directory_preview(&call.name, &decision.subjects))
                                } else {
                                    match tools.preview(tool_ctx.clone(), call.clone()).await {
                                        Ok(preview) => preview,
                                        Err(error) => {
                                            let error = error.to_string();
                                            preview_error = Some(error.clone());
                                            Some(ToolPreview {
                                                title: format!(
                                                    "Preview unavailable for {}",
                                                    call.name
                                                ),
                                                summary: "The tool preview could not be generated automatically."
                                                    .to_owned(),
                                                body: error,
                                                changed_files: Vec::new(),
                                                file_diffs: Vec::new(),
                                            })
                                        }
                                    }
                                };
                                if let Some(error) = preview_error.as_ref() {
                                    append_tool_approval_audit(
                                        session,
                                        &call,
                                        &decision,
                                        ToolApprovalAuditAction::PreviewFailed,
                                        None,
                                        Some(error.clone()),
                                        None,
                                    )?;
                                }
                                let preview_hash =
                                    preview.as_ref().map(stable_json_hash).transpose()?;
                                if preview_error.is_none()
                                    && let Some(preview) = preview.as_ref()
                                {
                                    let control = ControlEntry::ToolPreviewCaptured(
                                        ToolPreviewSnapshot::from_preview(
                                            call.id.clone(),
                                            call.name.clone(),
                                            preview,
                                            ToolDiffBudget::default(),
                                            preview_hash.clone(),
                                        ),
                                    );
                                    session.append_control(control.clone())?;
                                    handler.handle(RunEvent::Control(control))?;
                                }
                                append_tool_approval_audit(
                                    session,
                                    &call,
                                    &decision,
                                    ToolApprovalAuditAction::Requested,
                                    None,
                                    None,
                                    preview_hash.clone(),
                                )?;
                                handler.handle(RunEvent::ToolApprovalRequested {
                                    call: call.clone(),
                                    spec: spec.clone(),
                                    subjects: decision.subjects.clone(),
                                    operation: decision.operation,
                                    risk: decision.risk,
                                    subject_zones: decision.subject_zones.clone(),
                                    confirmation: decision.confirmation.clone(),
                                    snapshot_required: decision.snapshot_required,
                                    preview,
                                })?;
                                if let Some(confirmation) = decision.confirmation.as_ref() {
                                    let reason = format!(
                                        "tool {} requires typed confirmation ({confirmation:?}) before execution",
                                        call.name
                                    );
                                    append_tool_approval_audit(
                                        session,
                                        &call,
                                        &decision,
                                        ToolApprovalAuditAction::Resolved,
                                        Some(ToolApprovalUserDecision::Denied),
                                        Some(reason.clone()),
                                        preview_hash,
                                    )?;
                                    let mut result = ToolResult::error(
                                        call.id.clone(),
                                        call.name.clone(),
                                        ToolErrorKind::ApprovalRequired,
                                        reason.clone(),
                                    );
                                    attach_tool_call_context(
                                        &mut result,
                                        &call,
                                        &decision.subjects,
                                    );
                                    record_tool_run_outcome(&mut outcome, &result);
                                    session.append_tool_message(result.to_model_message())?;
                                    handler.handle(RunEvent::ToolApprovalResolved {
                                        call_id: call.id.clone(),
                                        approved: false,
                                        reason: Some(reason),
                                    })?;
                                    handler.handle(RunEvent::ToolResult(result))?;
                                    continue;
                                }
                                let approval = approval_handler.approve_tool_call(&call, &spec)?;
                                match approval {
                                    ToolApproval::Approve => {
                                        append_tool_approval_audit(
                                            session,
                                            &call,
                                            &decision,
                                            ToolApprovalAuditAction::Resolved,
                                            Some(ToolApprovalUserDecision::Approved),
                                            None,
                                            preview_hash,
                                        )?;
                                        handler.handle(RunEvent::ToolApprovalResolved {
                                            call_id: call.id.clone(),
                                            approved: true,
                                            reason: None,
                                        })?;
                                    }
                                    ToolApproval::ApproveWithArgs { args_json } => {
                                        call.args_json = args_json;
                                        append_tool_approval_audit(
                                            session,
                                            &call,
                                            &decision,
                                            ToolApprovalAuditAction::Resolved,
                                            Some(ToolApprovalUserDecision::Approved),
                                            None,
                                            preview_hash,
                                        )?;
                                        handler.handle(RunEvent::ToolApprovalResolved {
                                            call_id: call.id.clone(),
                                            approved: true,
                                            reason: None,
                                        })?;
                                    }
                                    ToolApproval::Deny { reason } => {
                                        append_tool_approval_audit(
                                            session,
                                            &call,
                                            &decision,
                                            ToolApprovalAuditAction::Resolved,
                                            Some(ToolApprovalUserDecision::Denied),
                                            Some(reason.clone()),
                                            preview_hash,
                                        )?;
                                        handler.handle(RunEvent::ToolApprovalResolved {
                                            call_id: call.id.clone(),
                                            approved: false,
                                            reason: Some(reason.clone()),
                                        })?;
                                        let mut result = ToolResult::error(
                                            call.id.clone(),
                                            call.name.clone(),
                                            ToolErrorKind::ApprovalDenied,
                                            format!("tool execution denied by user: {reason}"),
                                        );
                                        attach_tool_call_context(
                                            &mut result,
                                            &call,
                                            &decision.subjects,
                                        );
                                        record_tool_run_outcome(&mut outcome, &result);
                                        session.append_tool_message(result.to_model_message())?;
                                        handler.handle(RunEvent::ToolResult(result))?;
                                        continue;
                                    }
                                }
                            }
                            ApprovalMode::Deny => {
                                let (error_kind, reason) = if decision.external_directory_required {
                                    (
                                        ToolErrorKind::ExternalDirectoryRequired,
                                        format!(
                                            "external directory access requires permission.external_directory.enabled for {}. For scratch files, use $SIGIL_SCRATCH_DIR from bash or terminal_start.",
                                            if subject_label == "-" {
                                                call.name.as_str()
                                            } else {
                                                subject_label.as_str()
                                            }
                                        ),
                                    )
                                } else {
                                    (
                                        ToolErrorKind::PermissionDenied,
                                        format!(
                                            "denied by permission policy for {}",
                                            if subject_label == "-" {
                                                call.name.as_str()
                                            } else {
                                                subject_label.as_str()
                                            }
                                        ),
                                    )
                                };
                                append_tool_approval_audit(
                                    session,
                                    &call,
                                    &decision,
                                    ToolApprovalAuditAction::Resolved,
                                    Some(ToolApprovalUserDecision::Denied),
                                    Some(reason.clone()),
                                    None,
                                )?;
                                handler.handle(RunEvent::ToolApprovalResolved {
                                    call_id: call.id.clone(),
                                    approved: false,
                                    reason: Some(reason.clone()),
                                })?;
                                let mut result = ToolResult::error(
                                    call.id.clone(),
                                    call.name.clone(),
                                    error_kind,
                                    reason,
                                );
                                attach_tool_call_context(&mut result, &call, &decision.subjects);
                                record_tool_run_outcome(&mut outcome, &result);
                                session.append_tool_message(result.to_model_message())?;
                                handler.handle(RunEvent::ToolResult(result))?;
                                continue;
                            }
                        }
                        let egress_audit = match tools.egress_audit(&tool_ctx, &call) {
                            Ok(audit) => audit,
                            Err(error) => {
                                let mut result = ToolResult::error(
                                    call.id.clone(),
                                    call.name.clone(),
                                    ToolErrorKind::InvalidInput,
                                    format!("invalid tool arguments for {}: {error}", call.name),
                                );
                                attach_tool_call_context(&mut result, &call, &decision.subjects);
                                append_tool_execution_audit(
                                    session,
                                    &call,
                                    &decision.subjects,
                                    ToolExecutionStatus::Failed,
                                    None,
                                    Some(&result),
                                )?;
                                record_tool_run_outcome(&mut outcome, &result);
                                session.append_tool_message(result.to_model_message())?;
                                handler.handle(RunEvent::ToolResult(result))?;
                                continue;
                            }
                        };
                        if let Some(egress_audit) = egress_audit {
                            let control =
                                tool_egress_control_entry(&call, &decision.subjects, egress_audit);
                            session.append_control(control.clone())?;
                            handler.handle(RunEvent::Control(control))?;
                        }
                    }

                    append_tool_execution_audit(
                        session,
                        &call,
                        &execution_subjects,
                        ToolExecutionStatus::Started,
                        None,
                        None,
                    )?;
                    let execution_started = Instant::now();
                    let mut result = match agent_delegate
                        .as_deref_mut()
                        .filter(|_| tool_registered && tool_is_agent_category)
                    {
                        Some(delegate) => match delegate
                            .handle_agent_tool_call(
                                session,
                                &call,
                                &options,
                                handler,
                                approval_handler,
                            )
                            .await
                        {
                            Ok(Some(result)) => result,
                            Ok(None) => match tools.execute(tool_ctx.clone(), call.clone()).await {
                                Ok(result) => result,
                                Err(error) => ToolResult::error(
                                    call.id.clone(),
                                    call.name.clone(),
                                    ToolErrorKind::Internal,
                                    error.to_string(),
                                ),
                            },
                            Err(error) => ToolResult::error(
                                call.id.clone(),
                                call.name.clone(),
                                ToolErrorKind::Internal,
                                error.to_string(),
                            ),
                        },
                        None => match tools.execute(tool_ctx.clone(), call.clone()).await {
                            Ok(result) => result,
                            Err(error) => ToolResult::error(
                                call.id.clone(),
                                call.name.clone(),
                                ToolErrorKind::Internal,
                                error.to_string(),
                            ),
                        },
                    };
                    attach_tool_call_context(&mut result, &call, &execution_subjects);
                    let duration_ms = Some(duration_ms(execution_started));
                    let status = if result.is_error() {
                        ToolExecutionStatus::Failed
                    } else {
                        ToolExecutionStatus::Completed
                    };
                    append_tool_execution_audit(
                        session,
                        &call,
                        &execution_subjects,
                        status,
                        duration_ms,
                        Some(&result),
                    )?;
                    append_tool_control_entries_from_result(session, handler, &mut result)?;
                    append_terminal_task_control_from_result(session, handler, &result)?;
                    record_tool_run_outcome(&mut outcome, &result);
                    if tool_is_agent_category && agent_tool_result_satisfies_delegation(&result) {
                        satisfied_agent_tool_calls = satisfied_agent_tool_calls.saturating_add(1);
                    }
                    let tool_transient_context = std::mem::take(&mut result.transient_context);
                    session.append_tool_message(result.to_model_message())?;
                    handler.handle(RunEvent::ToolResult(result))?;
                    transient_context.extend(tool_transient_context);
                }
                continue;
            }

            if let Some(requirement) = agent_delegation_enforced.as_ref()
                && satisfied_agent_tool_calls == 0
            {
                if !delegation_retry_used {
                    delegation_retry_used = true;
                    handler.handle(RunEvent::Notice(
                        "agent delegation required before final answer; retrying with explicit agent-tool instruction"
                            .to_owned(),
                    ))?;
                    transient_context.push(ModelMessage::user(requirement.retry_prompt()));
                    continue;
                }
                handler.handle(RunEvent::Notice(
                    "agent delegation requirement was not satisfied; no final answer was recorded"
                        .to_owned(),
                ))?;
                outcome.terminal_reason = AgentRunTerminalReason::DelegationUnsatisfied;
                outcome.tool_calls = total_tool_calls;
                append_run_lifecycle_events(
                    session,
                    "blocked",
                    outcome.terminal_reason,
                    None,
                    total_tool_calls,
                )?;
                return Ok(AgentRunOutput {
                    result: AgentRunResult {
                        final_text: String::new(),
                        tool_calls: total_tool_calls,
                        final_message_id: None,
                    },
                    outcome,
                });
            }

            let assistant_message =
                ModelMessage::assistant(Some(assistant_text.clone()), Vec::new());
            let final_message_id = assistant_message.id.clone();
            session.append_assistant_message(assistant_message.clone())?;
            handler.handle(RunEvent::AssistantMessage(assistant_message))?;

            if !pending_states.is_empty() {
                for mut state in pending_states {
                    state.message_id = Some(
                        session
                            .messages()
                            .last()
                            .map(|m| m.id.clone())
                            .unwrap_or_default(),
                    );
                    let control = ControlEntry::ContinuationStateSaved(state);
                    session.append_control(control.clone())?;
                    handler.handle(RunEvent::Control(control))?;
                }
            }

            outcome.tool_calls = total_tool_calls;
            append_run_lifecycle_events(
                session,
                "completed",
                outcome.terminal_reason,
                Some(&final_message_id),
                total_tool_calls,
            )?;
            return Ok(AgentRunOutput {
                result: AgentRunResult {
                    final_text: assistant_text,
                    tool_calls: total_tool_calls,
                    final_message_id: Some(final_message_id),
                },
                outcome,
            });
        }
    }
}

fn direct_task_tool_guidance_result(
    call: &ToolCall,
    task_plan_update_available: bool,
) -> Option<ToolResult> {
    if !matches!(call.name.as_str(), "task" | "subagent" | "sub_agent") {
        return None;
    }
    let content = if task_plan_update_available {
        "direct task/subagent tool calls are not supported in the planner; delegate work by calling task_plan_update with an accepted plan and step roles subagent_read or subagent_write"
    } else {
        "direct task/subagent tool calls are legacy aliases; use the model-visible agent tools spawn_agent, wait_agent, read_agent_result, message_agent, and close_agent when the user explicitly asks for delegation; message_agent only sends follow-up instructions to an active background child-agent mailbox at the next safe point"
    };
    Some(ToolResult::ok(
        call.id.clone(),
        call.name.clone(),
        content,
        ToolResultMeta::default(),
    ))
}

fn count_agent_tool_calls(tools: &ToolRegistry, calls: &[ToolCall]) -> usize {
    calls
        .iter()
        .filter(|call| {
            tools
                .spec_for(&call.name)
                .is_some_and(|spec| spec.category == ToolCategory::Agent)
        })
        .count()
}

fn tool_registry_has_agent_tools(tools: &ToolRegistry) -> bool {
    tools
        .specs()
        .iter()
        .any(|spec| spec.category == ToolCategory::Agent)
}

fn agent_tool_result_satisfies_delegation(result: &ToolResult) -> bool {
    if result.is_error() {
        return false;
    }
    let details = &result.metadata.details;
    if details
        .get("result_available")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return true;
    }
    details
        .get("status")
        .and_then(Value::as_str)
        .is_some_and(is_terminal_agent_status)
}

fn is_terminal_agent_status(status: &str) -> bool {
    matches!(
        status,
        "completed" | "failed" | "cancelled" | "interrupted" | "closed"
    )
}

fn plan_approval_decision_override(
    session: &Session,
    spec: &ToolSpec,
    mut decision: PermissionDecision,
) -> PermissionDecision {
    if decision.mode != ApprovalMode::Ask
        || decision.external_directory_required
        || !plan_approval_can_auto_allow_decision(&decision)
    {
        return decision;
    }
    let Some(approval) = active_plan_approval(session) else {
        return decision;
    };
    if approval.permission.covers_tool(spec)
        && plan_approval_covers_subjects(&approval.scope.workspace_paths, &decision.subjects)
    {
        decision.mode = ApprovalMode::Allow;
    }
    decision
}

fn plan_approval_can_auto_allow_decision(decision: &PermissionDecision) -> bool {
    matches!(decision.risk, PermissionRisk::Low | PermissionRisk::Medium)
}

fn active_plan_approval(session: &Session) -> Option<crate::PlanApprovedEntry> {
    let entries = session.entries();
    let (approval_index, approval) =
        entries
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, entry)| match entry {
                crate::SessionLogEntry::Control(ControlEntry::PlanApproved(approval)) => {
                    Some((index, approval.clone()))
                }
                _ => None,
            })?;
    match approval.expires {
        PlanApprovalExpiry::NextUserPrompt => {
            let user_messages_after_approval = entries
                .iter()
                .skip(approval_index.saturating_add(1))
                .filter(|entry| matches!(entry, crate::SessionLogEntry::User(_)))
                .count();
            (user_messages_after_approval == 1).then_some(approval)
        }
        PlanApprovalExpiry::Session => Some(approval),
        PlanApprovalExpiry::AtUnixMs(expires_at_ms) => {
            (unix_time_ms() <= expires_at_ms).then_some(approval)
        }
    }
}

fn plan_approval_covers_subjects(workspace_paths: &[String], subjects: &[ToolSubject]) -> bool {
    if subjects.is_empty() {
        return false;
    }
    subjects.iter().all(|subject| {
        subject.scope == ToolSubjectScope::Workspace
            && plan_approval_covers_subject(workspace_paths, subject)
    })
}

fn plan_approval_covers_subject(workspace_paths: &[String], subject: &ToolSubject) -> bool {
    // Empty scope means the accepted plan did not name a concrete workspace target. Keep the
    // write behind normal approval instead of widening an ambiguous plan to the full workspace.
    if workspace_paths.is_empty() {
        return false;
    }
    workspace_paths
        .iter()
        .any(|scope_path| path_is_within_scope(&subject.normalized, scope_path))
}

fn path_is_within_scope(path: &str, scope_path: &str) -> bool {
    let path_components = Path::new(path).components().collect::<Vec<_>>();
    let scope_components = Path::new(scope_path).components().collect::<Vec<_>>();
    !scope_components.is_empty()
        && path_components.len() >= scope_components.len()
        && path_components
            .iter()
            .zip(scope_components.iter())
            .all(|(left, right)| left == right)
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().try_into().unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn record_tool_run_outcome(outcome: &mut AgentRunOutcome, result: &ToolResult) {
    if !result.metadata.changed_files.is_empty() {
        for file in &result.metadata.changed_files {
            if !outcome.changed_files.contains(file) {
                outcome.changed_files.push(file.clone());
            }
        }
    }
    let ToolResultStatus::Error(error) = &result.status else {
        return;
    };
    if error.kind == ToolErrorKind::ApprovalDenied {
        outcome.approval_denials += 1;
    }
    if error.kind == ToolErrorKind::Interrupted {
        outcome.interrupted_tool_calls.push(result.call_id.clone());
    }
    outcome.tool_errors.push(error.clone());
}

fn handle_task_plan_update_call<H>(
    session: &mut Session,
    handler: &mut H,
    outcome: &mut AgentRunOutcome,
    call: &ToolCall,
    context: &TaskPlanUpdateContext,
) -> Result<()>
where
    H: EventHandler + Send,
{
    append_tool_execution_audit(session, call, &[], ToolExecutionStatus::Started, None, None)?;
    let result = match task_plan_update_entry(context, call) {
        Ok(entry) => {
            let control = ControlEntry::TaskPlan(entry.clone());
            session.append_control(control.clone())?;
            handler.handle(RunEvent::Control(control))?;
            let result = ToolResult::ok(
                call.id.clone(),
                call.name.clone(),
                task_plan_update_result_content(&entry),
                ToolResultMeta::default(),
            );
            append_tool_execution_audit(
                session,
                call,
                &[],
                ToolExecutionStatus::Completed,
                None,
                Some(&result),
            )?;
            result
        }
        Err(error) => {
            let result = ToolResult::error(
                call.id.clone(),
                call.name.clone(),
                ToolErrorKind::InvalidInput,
                error.to_string(),
            );
            append_tool_execution_audit(
                session,
                call,
                &[],
                ToolExecutionStatus::Failed,
                None,
                Some(&result),
            )?;
            result
        }
    };
    record_tool_run_outcome(outcome, &result);
    session.append_tool_message(result.to_model_message())?;
    handler.handle(RunEvent::ToolResult(result))?;
    Ok(())
}

fn has_external_subject(subjects: &[ToolSubject]) -> bool {
    subjects
        .iter()
        .any(|subject| subject.scope == ToolSubjectScope::External)
}

fn append_reasoning_trace(session: &mut Session, trace: &str) -> Result<()> {
    if trace.is_empty() {
        return Ok(());
    }
    session.append_control(reasoning_trace_note(trace.to_owned()))
}

fn reasoning_trace_note(trace: String) -> ControlEntry {
    let mut data = Map::new();
    data.insert("text".to_owned(), Value::String(trace));
    ControlEntry::Note {
        kind: "reasoning_trace".to_owned(),
        data: Value::Object(data),
    }
}

fn attach_tool_call_context(result: &mut ToolResult, call: &ToolCall, subjects: &[ToolSubject]) {
    let Some(context) = tool_call_context(call, subjects) else {
        return;
    };
    match &mut result.metadata.details {
        Value::Object(details) => {
            details.insert("call".to_owned(), context);
        }
        Value::Null => {
            let mut details = Map::new();
            details.insert("call".to_owned(), context);
            result.metadata.details = Value::Object(details);
        }
        existing => {
            let previous = std::mem::replace(existing, Value::Null);
            let mut details = Map::new();
            details.insert("call".to_owned(), context);
            details.insert("tool".to_owned(), previous);
            *existing = Value::Object(details);
        }
    }
}

fn tool_call_context(call: &ToolCall, subjects: &[ToolSubject]) -> Option<Value> {
    let args = serde_json::from_str::<Value>(&call.args_json).ok();
    let object = args.as_ref().and_then(Value::as_object);
    let mut context = Map::new();
    let mut summary_parts = Vec::new();

    if let Some(command) = object
        .and_then(|object| object.get("command"))
        .and_then(Value::as_str)
    {
        let command = truncate_context_value(command);
        context.insert("command".to_owned(), Value::String(command.clone()));
        summary_parts.push(format!("command={command}"));
    }
    if let Some(path) = object
        .and_then(|object| object.get("path"))
        .and_then(Value::as_str)
    {
        let path = truncate_context_value(path);
        context.insert("path".to_owned(), Value::String(path.clone()));
        summary_parts.push(format!("path={path}"));
    }
    if let Some(pattern) = object
        .and_then(|object| object.get("pattern"))
        .and_then(Value::as_str)
    {
        let pattern = truncate_context_value(pattern);
        context.insert("pattern".to_owned(), Value::String(pattern.clone()));
        summary_parts.push(format!("pattern={pattern}"));
    }

    let subject_labels = subjects
        .iter()
        .take(6)
        .map(tool_subject_context_label)
        .collect::<Vec<_>>();
    if !subject_labels.is_empty() {
        context.insert(
            "subjects".to_owned(),
            Value::Array(subject_labels.iter().cloned().map(Value::String).collect()),
        );
        if summary_parts.is_empty() {
            summary_parts.push(format!("subject={}", subject_labels.join(",")));
        }
    }

    if !summary_parts.is_empty() {
        context.insert(
            "summary".to_owned(),
            Value::String(truncate_context_value(&summary_parts.join(" "))),
        );
    }

    (!context.is_empty()).then_some(Value::Object(context))
}

fn tool_subject_context_label(subject: &ToolSubject) -> String {
    let target = subject
        .canonical_path
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| subject.normalized.clone());
    truncate_context_value(&format!(
        "{}:{}:{}",
        subject.scope.as_str(),
        subject.kind.as_str(),
        target
    ))
}

fn truncate_context_value(value: &str) -> String {
    const MAX_CHARS: usize = 180;
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= MAX_CHARS {
        return normalized;
    }
    let truncated = normalized.chars().take(MAX_CHARS).collect::<String>();
    format!("{truncated}...")
}

fn external_directory_preview(tool_name: &str, subjects: &[ToolSubject]) -> ToolPreview {
    let external_subjects = subjects
        .iter()
        .filter(|subject| subject.scope == ToolSubjectScope::External)
        .map(|subject| {
            subject
                .canonical_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| subject.normalized.clone())
        })
        .collect::<Vec<_>>();
    let body = if external_subjects.is_empty() {
        "No external path subjects were reported.".to_owned()
    } else {
        external_subjects.join("\n")
    };
    ToolPreview {
        title: format!("External directory access for {tool_name}"),
        summary: "This tool call touches a path outside the workspace.".to_owned(),
        body,
        changed_files: Vec::new(),
        file_diffs: Vec::new(),
    }
}

fn append_tool_approval_audit(
    session: &mut Session,
    call: &ToolCall,
    decision: &PermissionDecision,
    action: ToolApprovalAuditAction,
    user_decision: Option<ToolApprovalUserDecision>,
    reason: Option<String>,
    preview_hash: Option<String>,
) -> Result<()> {
    session.append_control(ControlEntry::ToolApproval(ToolApprovalEntry {
        action,
        call_id: call.id.clone(),
        tool_name: call.name.clone(),
        access: decision.access,
        operation: Some(decision.operation),
        risk: Some(decision.risk),
        subjects: audit_subjects(&decision.subjects),
        subject_zones: decision.subject_zones.clone(),
        policy_decision: decision.mode,
        external_directory_required: decision.external_directory_required,
        confirmation: decision.confirmation.clone(),
        snapshot_required: decision.snapshot_required,
        user_decision,
        reason,
        preview_hash,
    }))
}

fn append_run_lifecycle_events(
    session: &mut Session,
    run_status: &'static str,
    terminal_reason: AgentRunTerminalReason,
    final_message_id: Option<&str>,
    tool_calls: usize,
) -> Result<()> {
    append_run_lifecycle_event_payload(
        session,
        run_status,
        terminal_reason.as_str(),
        final_message_id,
        tool_calls,
        None,
    )
}

fn append_failed_run_lifecycle_events(
    session: &mut Session,
    terminal_reason: &'static str,
    tool_calls: usize,
    error: &str,
) -> Result<()> {
    append_run_lifecycle_event_payload(
        session,
        "failed",
        terminal_reason,
        None,
        tool_calls,
        Some(error),
    )
}

fn append_run_lifecycle_event_payload(
    session: &mut Session,
    run_status: &'static str,
    terminal_reason: &'static str,
    final_message_id: Option<&str>,
    tool_calls: usize,
    error: Option<&str>,
) -> Result<()> {
    session.append_durable_event(
        DurableEventType::RunStatusChanged,
        EventClass::Critical,
        json!({
            "run_status": run_status,
            "terminal_reason": terminal_reason,
            "final_message_id": final_message_id,
            "tool_calls": tool_calls,
            "error": error,
        }),
    )?;
    session.append_durable_event(
        DurableEventType::RunFinalized,
        EventClass::Critical,
        json!({
            "run_status": run_status,
            "terminal_reason": terminal_reason,
            "final_message_id": final_message_id,
            "tool_calls": tool_calls,
            "error": error,
        }),
    )?;
    Ok(())
}

fn append_tool_execution_audit(
    session: &mut Session,
    call: &ToolCall,
    subjects: &[ToolSubject],
    status: ToolExecutionStatus,
    duration_ms: Option<u64>,
    result: Option<&ToolResult>,
) -> Result<()> {
    let (changed_files, metadata, error, model_content_hash) = if let Some(result) = result {
        let error = match &result.status {
            ToolResultStatus::Ok => None,
            ToolResultStatus::Error(error) => Some(error.clone()),
        };
        (
            result.metadata.changed_files.clone(),
            result.metadata.clone(),
            error,
            Some(stable_text_hash(&result.to_model_content())),
        )
    } else {
        let mut metadata = ToolResultMeta::default();
        if let Some(context) = tool_call_context(call, subjects) {
            let mut details = Map::new();
            details.insert("call".to_owned(), context);
            metadata.details = Value::Object(details);
        }
        (Vec::new(), metadata, None, None)
    };

    session.append_control(ControlEntry::ToolExecution(Box::new(ToolExecutionEntry {
        call_id: call.id.clone(),
        tool_name: call.name.clone(),
        status,
        duration_ms,
        subjects: audit_subjects(subjects),
        changed_files,
        metadata,
        error,
        model_content_hash,
    })))
}

fn append_terminal_task_control_from_result(
    session: &mut Session,
    handler: &mut impl EventHandler,
    result: &ToolResult,
) -> Result<()> {
    let Some(entry) = TerminalTaskEntry::from_tool_result_details(&result.metadata.details)? else {
        return Ok(());
    };
    let control = ControlEntry::TerminalTask(entry);
    session.append_control(control.clone())?;
    handler.handle(RunEvent::Control(control))
}

fn append_tool_control_entries_from_result(
    session: &mut Session,
    handler: &mut impl EventHandler,
    result: &mut ToolResult,
) -> Result<()> {
    for control in std::mem::take(&mut result.control_entries) {
        session.append_control(control.clone())?;
        handler.handle(RunEvent::Control(control))?;
    }
    Ok(())
}

fn tool_egress_control_entry(
    call: &ToolCall,
    subjects: &[ToolSubject],
    audit: ToolEgressAudit,
) -> ControlEntry {
    ControlEntry::ToolEgress(Box::new(ToolEgressEntry {
        call_id: call.id.clone(),
        tool_name: call.name.clone(),
        destination: audit.destination,
        operation: audit.operation,
        subjects: audit_subjects(subjects),
        payload: audit.payload,
        redacted: audit.redacted,
    }))
}

fn audit_subjects(subjects: &[ToolSubject]) -> Vec<ToolSubjectAudit> {
    subjects.iter().map(ToolSubjectAudit::from).collect()
}

fn stable_json_hash<T: serde::Serialize>(value: &T) -> Result<String> {
    let serialized = serde_json::to_string(value).context("failed to serialize audit payload")?;
    Ok(stable_text_hash(&serialized))
}

fn stable_text_hash(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    format!("{digest:x}")
}

fn duration_ms(started_at: Instant) -> u64 {
    saturating_elapsed(started_at)
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
#[path = "tests/agent_tests.rs"]
mod tests;
