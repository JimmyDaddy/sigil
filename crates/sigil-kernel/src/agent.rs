use std::{
    sync::Arc,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde_json::{Map, Value};
use tokio::sync::mpsc;

use crate::{
    RuntimeContextCandidates,
    approval::{ApprovalHandler, AutoApproveHandler, ToolApproval},
    config::{CompactionConfig, MemoryConfig},
    event::{EventHandler, RunEvent},
    permission::{
        ApprovalMode, InteractionMode, PermissionConfig, PermissionEvaluationContext,
        PermissionPolicy, tool_approval_session_grant_available,
    },
    provider::{ModelMessage, Provider, ToolCall},
    session::{
        ControlEntry, Session, SessionLogEntry, ToolApprovalAuditAction, ToolApprovalUserDecision,
        ToolExecutionStatus,
    },
    task::{TASK_PLAN_UPDATE_TOOL_NAME, TaskPlanUpdateContext, task_plan_update_tool_spec},
    tool::{
        PreparedToolCall, ToolCategory, ToolContext, ToolErrorKind, ToolProgressEvent,
        ToolProgressSink, ToolRegistry, ToolResult,
    },
};

mod approval_policy;
mod assistant_messages;
mod preview;
mod provider_stream;
mod readiness;
mod run_lifecycle;
mod task_plan;
mod tool_audit;
mod tool_results;
#[cfg(test)]
use approval_policy::active_plan_approval;
use approval_policy::{
    active_plan_approval_authority, interactive_external_directory_approval_override,
    plan_approval_decision_override, tool_session_grant_decision_override,
};
use assistant_messages::{append_final_answer_message, append_tool_preamble_message};
use preview::{
    capture_tool_preview_for_decision, pending_interactive_approval_identity,
    preparation_plan_approval_identity, preparation_policy_approval_identity,
    preparation_policy_fingerprint, preparation_session_grant_identity,
    resolved_interactive_approval_identity,
};
use provider_stream::collect_provider_turn;
use readiness::append_agent_run_readiness;
pub use readiness::projected_agent_run_readiness;
use run_lifecycle::append_run_lifecycle_events;
use task_plan::{
    append_tool_ignored_after_task_plan_acceptance, handle_task_plan_update_call,
    task_plan_update_call_is_accepted,
};
use tool_audit::{
    append_terminal_task_control_from_result, append_tool_approval_audit,
    append_tool_approval_policy_audit, append_tool_approval_session_grant,
    append_tool_control_entries_from_result, append_tool_execution_audit,
    append_tool_execution_started_audit, attach_prepared_tool_audit_binding,
    attach_tool_call_context, duration_ms, reconcile_terminal_task_mutation_from_start,
    tool_egress_control_entry,
};
#[cfg(test)]
use tool_audit::{
    external_directory_preview, stable_json_hash, stable_text_hash, tool_call_context,
};
use tool_results::{
    agent_tool_result_satisfies_delegation, append_invalid_tool_input_result, emit_tool_result,
    record_and_emit_tool_result, record_tool_run_outcome,
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

struct ChannelToolProgressSink {
    sender: mpsc::UnboundedSender<ToolProgressEvent>,
}

impl ToolProgressSink for ChannelToolProgressSink {
    fn emit(&self, event: ToolProgressEvent) -> Result<()> {
        self.sender
            .send(event)
            .map_err(|error| anyhow!("failed to forward tool progress: {error}"))
    }
}

async fn execute_after_started_audit_with_progress(
    tools: &ToolRegistry,
    ctx: ToolContext,
    call: ToolCall,
    handler: &mut (impl EventHandler + Send),
) -> Result<ToolResult> {
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let ctx = ctx.with_progress_sink(Arc::new(ChannelToolProgressSink { sender }));
    let execution = tools.execute_after_started_audit(ctx, call);
    tokio::pin!(execution);

    loop {
        tokio::select! {
            result = &mut execution => {
                while let Ok(progress) = receiver.try_recv() {
                    handler.handle(RunEvent::ToolProgress(progress))?;
                }
                return result;
            }
            Some(progress) = receiver.recv() => {
                handler.handle(RunEvent::ToolProgress(progress))?;
            }
        }
    }
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
    pub runtime_context: RuntimeContextCandidates,
    pub task_plan_update: Option<TaskPlanUpdateContext>,
    pub agent_delegation: Option<AgentDelegationRequirement>,
}

impl AgentRunInput {
    pub fn user(prompt: impl Into<String>) -> Self {
        Self {
            persisted_user_message: Some(prompt.into()),
            transient_context: Vec::new(),
            runtime_context: RuntimeContextCandidates::default(),
            task_plan_update: None,
            agent_delegation: None,
        }
    }

    pub fn transient(prompt: impl Into<String>, transient_context: Vec<ModelMessage>) -> Self {
        Self {
            persisted_user_message: Some(prompt.into()),
            transient_context,
            runtime_context: RuntimeContextCandidates::default(),
            task_plan_update: None,
            agent_delegation: None,
        }
    }

    pub fn without_persisted_user_message(transient_context: Vec<ModelMessage>) -> Self {
        Self {
            persisted_user_message: None,
            transient_context,
            runtime_context: RuntimeContextCandidates::default(),
            task_plan_update: None,
            agent_delegation: None,
        }
    }

    pub fn with_task_plan_update(mut self, context: TaskPlanUpdateContext) -> Self {
        self.task_plan_update = Some(context);
        self
    }

    pub fn with_runtime_context(mut self, context: RuntimeContextCandidates) -> Self {
        self.runtime_context = context;
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

/// Model-visible context that should be injected before accepting a final answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalAnswerContext {
    pub key: String,
    pub prompt: String,
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
    pub tool_call_ids: Vec<String>,
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

    /// Returns a model-visible continuation prompt when a final answer must wait for delegated
    /// agent work. The default keeps non-agent runtimes unchanged.
    ///
    /// # Errors
    ///
    /// Returns an error if the delegate cannot inspect its durable state.
    fn final_answer_blocker(&mut self, _session: &mut Session) -> Result<Option<String>> {
        Ok(None)
    }

    /// Returns model-visible factual context that should be present before the final answer.
    ///
    /// This is advisory context, not a hard quality gate. Implementations should return a stable
    /// key for the facts they provide so the agent loop can avoid repeated retries.
    ///
    /// # Errors
    ///
    /// Returns an error if the delegate cannot inspect its durable state.
    fn final_answer_context(
        &mut self,
        _session: &Session,
        _options: &AgentRunOptions,
        _outcome: &AgentRunOutcome,
    ) -> Result<Option<FinalAnswerContext>> {
        Ok(None)
    }
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
            runtime_context,
            task_plan_update,
            agent_delegation,
        } = input;

        session.reconcile_prepared_mutations(&options.workspace_root)?;
        session.reconcile_unfinished_write_tool_executions(&options.workspace_root)?;

        if let Some(message) = persisted_user_message
            && !last_provider_visible_user_message_matches(session, &message)
        {
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
        let mut final_answer_context_key: Option<String> = None;

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
            let request = session.build_request_with_transient_messages_and_context(
                &options.workspace_root,
                &options.memory_config,
                tool_specs,
                options.reasoning_effort.clone(),
                previous_response_handle.clone(),
                options.traffic_partition_key.clone(),
                &transient_context,
                runtime_context.clone(),
            )?;

            let provider_turn = collect_provider_turn(
                &self.provider,
                session,
                request,
                &mut previous_response_handle,
                total_tool_calls,
                handler,
            )
            .await?;
            let assistant_text = provider_turn.assistant_text;
            let completed_calls = provider_turn.completed_calls;
            let pending_states = provider_turn.pending_states;

            append_reasoning_trace(session, &provider_turn.reasoning_trace)?;

            if !completed_calls.is_empty() {
                total_tool_calls += completed_calls.len();
                append_tool_preamble_message(
                    session,
                    handler,
                    tools,
                    &assistant_text,
                    &completed_calls,
                    pending_states,
                )?;

                let mut tool_ctx =
                    ToolContext::new(options.workspace_root.clone(), options.tool_timeout_secs);
                if let Some(recorder) = session.mutation_event_recorder() {
                    tool_ctx = tool_ctx.with_mutation_recorder(recorder);
                }
                let accepted_task_plan_in_batch = completed_calls.iter().any(|call| {
                    task_plan_update
                        .as_ref()
                        .is_some_and(|context| task_plan_update_call_is_accepted(context, call))
                });
                let mut accepted_task_plan = false;
                for mut call in completed_calls {
                    if accepted_task_plan_in_batch && call.name != TASK_PLAN_UPDATE_TOOL_NAME {
                        append_tool_ignored_after_task_plan_acceptance(
                            session,
                            handler,
                            &mut outcome,
                            &call,
                        )?;
                        continue;
                    }
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
                            record_and_emit_tool_result(session, handler, &mut outcome, result)?;
                            continue;
                        };
                        let accepted = handle_task_plan_update_call(
                            session,
                            handler,
                            &mut outcome,
                            &call,
                            context,
                        )?;
                        accepted_task_plan = accepted_task_plan || accepted;
                        continue;
                    }
                    let mut execution_subjects = Vec::new();
                    let mut prepared_tool_call = None;
                    let mut tool_registered = false;
                    let mut tool_is_agent_category = false;
                    let execution_spec = tools.spec_for(&call.name);
                    if let Some(spec) = execution_spec.as_ref() {
                        tool_registered = true;
                        tool_is_agent_category = spec.category == ToolCategory::Agent;
                        let preparation_draft =
                            match tools.prepare(tool_ctx.clone(), call.clone()).await {
                                Ok(preparation) => preparation,
                                Err(error) => {
                                    append_invalid_tool_input_result(
                                        session,
                                        handler,
                                        &mut outcome,
                                        &call,
                                        &[],
                                        error,
                                    )?;
                                    continue;
                                }
                            };
                        let subjects = if let Some(draft) = preparation_draft.as_ref() {
                            draft.subjects().to_vec()
                        } else {
                            match tools.permission_subjects(&tool_ctx, &call) {
                                Ok(subjects) => subjects,
                                Err(error) => {
                                    append_invalid_tool_input_result(
                                        session,
                                        handler,
                                        &mut outcome,
                                        &call,
                                        &[],
                                        error,
                                    )?;
                                    continue;
                                }
                            }
                        };
                        let access = match tools.permission_access(&tool_ctx, &call) {
                            Ok(access) => access,
                            Err(error) => {
                                append_invalid_tool_input_result(
                                    session,
                                    handler,
                                    &mut outcome,
                                    &call,
                                    &subjects,
                                    error,
                                )?;
                                continue;
                            }
                        };
                        let operation = match tools.permission_operation(&tool_ctx, &call) {
                            Ok(operation) => operation,
                            Err(error) => {
                                append_invalid_tool_input_result(
                                    session,
                                    handler,
                                    &mut outcome,
                                    &call,
                                    &subjects,
                                    error,
                                )?;
                                continue;
                            }
                        };
                        let tool_default_mode =
                            match tools.permission_default_mode(&tool_ctx, &call) {
                                Ok(mode) => mode,
                                Err(error) => {
                                    append_invalid_tool_input_result(
                                        session,
                                        handler,
                                        &mut outcome,
                                        &call,
                                        &subjects,
                                        error,
                                    )?;
                                    continue;
                                }
                            };
                        let decision = permission_policy.decide_with_operation_and_default(
                            spec,
                            &call.name,
                            access,
                            operation,
                            subjects.clone(),
                            tool_default_mode,
                        )?;
                        let pre_plan_decision =
                            interactive_external_directory_approval_override(&options, decision);
                        let plan_authority =
                            active_plan_approval_authority(session, spec, &pre_plan_decision);
                        let binding_decision =
                            plan_approval_decision_override(session, spec, pre_plan_decision);
                        let (decision, session_grant_source) = tool_session_grant_decision_override(
                            session,
                            &call.name,
                            binding_decision.clone(),
                        );
                        prepared_tool_call = match preparation_draft {
                            Some(draft) => {
                                let policy_fingerprint =
                                    preparation_policy_fingerprint(&binding_decision)?;
                                let approval_identity =
                                    if let Some(grant) = session_grant_source.as_ref() {
                                        preparation_session_grant_identity(grant)?
                                    } else if let Some(authority) = plan_authority.as_ref() {
                                        preparation_plan_approval_identity(authority)?
                                    } else if binding_decision.mode == ApprovalMode::Ask {
                                        pending_interactive_approval_identity(&call.id)
                                    } else {
                                        preparation_policy_approval_identity(&policy_fingerprint)
                                    };
                                Some(draft.bind_with_approval_identity(
                                    policy_fingerprint,
                                    approval_identity,
                                )?)
                            }
                            None => None,
                        };
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
                        append_tool_approval_policy_audit(
                            session,
                            &call,
                            &decision,
                            session_grant_source.as_ref(),
                            prepared_tool_call
                                .as_ref()
                                .map(|prepared| prepared.prepared_digest().to_owned()),
                        )?;
                        let preview_capture = capture_tool_preview_for_decision(
                            session,
                            handler,
                            tools,
                            tool_ctx.clone(),
                            &call,
                            spec,
                            &decision,
                            prepared_tool_call.take(),
                        )
                        .await?;
                        prepared_tool_call = preview_capture.prepared;
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
                                record_and_emit_tool_result(
                                    session,
                                    handler,
                                    &mut outcome,
                                    result,
                                )?;
                                continue;
                            }
                            ApprovalMode::Ask => {
                                let preview = preview_capture.preview.clone();
                                let preview_hash = preview_capture.preview_hash.clone();
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
                                    command_permission_matches: decision
                                        .command_permission_matches
                                        .clone(),
                                    preview,
                                })?;
                                let approval = approval_handler.approve_tool_call(&call, spec)?;
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
                                        authorize_prepared_tool_from_resolved_approval(
                                            session,
                                            &call,
                                            &mut prepared_tool_call,
                                        )?;
                                        handler.handle(RunEvent::ToolApprovalResolved {
                                            call_id: call.id.clone(),
                                            approved: true,
                                            reason: None,
                                        })?;
                                    }
                                    ToolApproval::ApproveForSession => {
                                        if !tool_approval_session_grant_available(&decision) {
                                            let reason =
                                                "session approval grant is not available for this tool call"
                                                    .to_owned();
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
                                            record_and_emit_tool_result(
                                                session,
                                                handler,
                                                &mut outcome,
                                                result,
                                            )?;
                                            continue;
                                        }
                                        append_tool_approval_audit(
                                            session,
                                            &call,
                                            &decision,
                                            ToolApprovalAuditAction::Resolved,
                                            Some(ToolApprovalUserDecision::ApprovedForSession),
                                            None,
                                            preview_hash,
                                        )?;
                                        authorize_prepared_tool_from_resolved_approval(
                                            session,
                                            &call,
                                            &mut prepared_tool_call,
                                        )?;
                                        append_tool_approval_session_grant(
                                            session, handler, &call, &decision,
                                        )?;
                                        handler.handle(RunEvent::ToolApprovalResolved {
                                            call_id: call.id.clone(),
                                            approved: true,
                                            reason: Some("allowed for this session".to_owned()),
                                        })?;
                                    }
                                    ToolApproval::ApproveWithArgs { args_json } => {
                                        if prepared_tool_call.is_some() {
                                            let reason = "prepared mutations do not allow approval-time argument changes; preview and approval must be repeated"
                                                .to_owned();
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
                                                ToolErrorKind::StalePreparedMutation,
                                                reason,
                                            );
                                            attach_tool_call_context(
                                                &mut result,
                                                &call,
                                                &decision.subjects,
                                            );
                                            record_and_emit_tool_result(
                                                session,
                                                handler,
                                                &mut outcome,
                                                result,
                                            )?;
                                            continue;
                                        }
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
                                        record_and_emit_tool_result(
                                            session,
                                            handler,
                                            &mut outcome,
                                            result,
                                        )?;
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
                                record_and_emit_tool_result(
                                    session,
                                    handler,
                                    &mut outcome,
                                    result,
                                )?;
                                continue;
                            }
                        }
                        let egress_audit = match tools.egress_audit(&tool_ctx, &call) {
                            Ok(audit) => audit,
                            Err(error) => {
                                append_invalid_tool_input_result(
                                    session,
                                    handler,
                                    &mut outcome,
                                    &call,
                                    &decision.subjects,
                                    error,
                                )?;
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

                    let execution_mutation_profile = execution_spec
                        .as_ref()
                        .map(|_| tools.execution_mutation_profile(&tool_ctx, &call))
                        .transpose()?
                        .flatten();
                    let prepared_current_authority = if let Some(prepared) =
                        prepared_tool_call.as_ref()
                    {
                        let spec = execution_spec
                            .as_ref()
                            .expect("prepared tools must retain their execution spec");
                        let access = tools.permission_access(&tool_ctx, &call)?;
                        let operation = tools.permission_operation(&tool_ctx, &call)?;
                        let tool_default_mode = tools.permission_default_mode(&tool_ctx, &call)?;
                        let current_decision = permission_policy
                            .decide_with_operation_and_default(
                                spec,
                                &call.name,
                                access,
                                operation,
                                execution_subjects.clone(),
                                tool_default_mode,
                            )?;
                        let current_pre_plan_decision =
                            interactive_external_directory_approval_override(
                                &options,
                                current_decision,
                            );
                        let current_plan_authority = active_plan_approval_authority(
                            session,
                            spec,
                            &current_pre_plan_decision,
                        );
                        let current_decision = plan_approval_decision_override(
                            session,
                            spec,
                            current_pre_plan_decision,
                        );
                        let current_policy_fingerprint =
                            preparation_policy_fingerprint(&current_decision)?;
                        let bound_identity = &prepared.binding().approval_identity;
                        let current_approval_identity = if bound_identity
                            .starts_with("session-grant:")
                        {
                            let (_, current_grant) = tool_session_grant_decision_override(
                                session,
                                &call.name,
                                current_decision.clone(),
                            );
                            match current_grant.as_ref() {
                                Some(grant) => preparation_session_grant_identity(grant)?,
                                None => "session-grant:missing".to_owned(),
                            }
                        } else if bound_identity.starts_with("plan:") {
                            match current_plan_authority.as_ref() {
                                Some(authority) => preparation_plan_approval_identity(authority)?,
                                None => "plan:missing".to_owned(),
                            }
                        } else if bound_identity.starts_with("interactive:") {
                            resolved_interactive_approval_identity(
                                session,
                                &call.id,
                                prepared.prepared_digest(),
                            )?
                            .unwrap_or_else(|| "interactive:missing".to_owned())
                        } else {
                            preparation_policy_approval_identity(&current_policy_fingerprint)
                        };
                        Some((current_policy_fingerprint, current_approval_identity))
                    } else {
                        None
                    };
                    let prepared_audit_binding = prepared_tool_call
                        .as_ref()
                        .map(|prepared| prepared.audit_binding());
                    append_tool_execution_started_audit(
                        session,
                        &call,
                        &execution_subjects,
                        execution_mutation_profile.as_ref(),
                        prepared_audit_binding.as_ref(),
                    )?;
                    let execution_started = Instant::now();
                    let execution_tool_ctx = tool_ctx
                        .clone()
                        .with_approved_subjects(execution_subjects.clone());
                    let mut result = if let Some(prepared) = prepared_tool_call {
                        let (current_policy_fingerprint, current_approval_identity) =
                            prepared_current_authority
                                .as_ref()
                                .expect("prepared tools must retain their approval authority");
                        match tools
                            .execute_prepared_after_started_audit(
                                execution_tool_ctx,
                                call.clone(),
                                prepared,
                                current_policy_fingerprint,
                                current_approval_identity,
                            )
                            .await
                        {
                            Ok(result) => result,
                            Err(error) => ToolResult::error(
                                call.id.clone(),
                                call.name.clone(),
                                ToolErrorKind::Internal,
                                error.to_string(),
                            ),
                        }
                    } else {
                        match agent_delegate
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
                                Ok(None) => match execute_after_started_audit_with_progress(
                                    tools,
                                    execution_tool_ctx.clone(),
                                    call.clone(),
                                    handler,
                                )
                                .await
                                {
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
                            None => match execute_after_started_audit_with_progress(
                                tools,
                                execution_tool_ctx,
                                call.clone(),
                                handler,
                            )
                            .await
                            {
                                Ok(result) => result,
                                Err(error) => ToolResult::error(
                                    call.id.clone(),
                                    call.name.clone(),
                                    ToolErrorKind::Internal,
                                    error.to_string(),
                                ),
                            },
                        }
                    };
                    if let Some(binding) = prepared_audit_binding.as_ref() {
                        attach_prepared_tool_audit_binding(&mut result, binding)?;
                    }
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
                    if let Some(entry) =
                        append_terminal_task_control_from_result(session, handler, &result)?
                    {
                        reconcile_terminal_task_mutation_from_start(
                            session,
                            &options.workspace_root,
                            &entry,
                        )?;
                    }
                    record_tool_run_outcome(&mut outcome, &result);
                    if tool_is_agent_category && agent_tool_result_satisfies_delegation(&result) {
                        satisfied_agent_tool_calls = satisfied_agent_tool_calls.saturating_add(1);
                    }
                    let tool_transient_context = std::mem::take(&mut result.transient_context);
                    emit_tool_result(session, handler, result)?;
                    transient_context.extend(tool_transient_context);
                }
                if accepted_task_plan {
                    outcome.tool_calls = total_tool_calls;
                    append_run_lifecycle_events(
                        session,
                        "completed",
                        outcome.terminal_reason,
                        None,
                        total_tool_calls,
                    )?;
                    return Ok(AgentRunOutput {
                        result: AgentRunResult {
                            final_text: "task plan accepted; orchestration will continue"
                                .to_owned(),
                            tool_calls: total_tool_calls,
                            final_message_id: None,
                        },
                        outcome,
                    });
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

            if let Some(blocker_prompt) = agent_delegate
                .as_deref_mut()
                .map(|delegate| delegate.final_answer_blocker(session))
                .transpose()?
                .flatten()
            {
                handler.handle(RunEvent::Notice(
                    "pending agent state blocks final answer; continuing".to_owned(),
                ))?;
                transient_context.push(ModelMessage::user(blocker_prompt));
                continue;
            }
            if let Some(context) = agent_delegate
                .as_deref_mut()
                .map(|delegate| delegate.final_answer_context(session, &options, &outcome))
                .transpose()?
                .flatten()
                && final_answer_context_key.as_deref() != Some(context.key.as_str())
            {
                final_answer_context_key = Some(context.key);
                handler.handle(RunEvent::Notice(
                    "recorded run facts added before final answer; continuing".to_owned(),
                ))?;
                transient_context.push(ModelMessage::user(context.prompt));
                continue;
            }

            let final_message_id =
                append_final_answer_message(session, handler, &assistant_text, pending_states)?;

            outcome.tool_calls = total_tool_calls;
            append_run_lifecycle_events(
                session,
                "completed",
                outcome.terminal_reason,
                Some(&final_message_id),
                total_tool_calls,
            )?;
            append_agent_run_readiness(session, handler, &options, &final_message_id, &outcome)?;
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

fn authorize_prepared_tool_from_resolved_approval(
    session: &Session,
    call: &ToolCall,
    prepared: &mut Option<PreparedToolCall>,
) -> Result<()> {
    let Some(pending) = prepared.take() else {
        return Ok(());
    };
    let identity =
        resolved_interactive_approval_identity(session, &call.id, pending.prepared_digest())?
            .ok_or_else(|| {
                anyhow!("approved prepared tool is missing its durable approval receipt")
            })?;
    *prepared = Some(pending.authorize(identity)?);
    Ok(())
}

fn tool_registry_has_agent_tools(tools: &ToolRegistry) -> bool {
    tools
        .specs()
        .iter()
        .any(|spec| spec.category == ToolCategory::Agent)
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().try_into().unwrap_or(u64::MAX))
        .unwrap_or(0)
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

fn last_provider_visible_user_message_matches(session: &Session, message: &str) -> bool {
    session
        .entries()
        .iter()
        .rev()
        .find_map(|entry| match entry {
            SessionLogEntry::User(user) => Some(user.content.as_deref() == Some(message)),
            SessionLogEntry::Assistant(_) | SessionLogEntry::ToolResult(_) => Some(false),
            SessionLogEntry::Control(_) => None,
        })
        .unwrap_or(false)
}

#[cfg(test)]
#[path = "tests/agent_tests.rs"]
mod tests;
