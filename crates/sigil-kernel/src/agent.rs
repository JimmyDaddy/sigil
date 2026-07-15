use std::{
    fmt,
    sync::Arc,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde_json::{Map, Value};
use tokio::sync::mpsc;

use crate::{
    FrozenProviderRequestMaterial, RuntimeContextCandidates,
    approval::{ApprovalHandler, AutoApproveHandler, ToolApproval},
    cancellation::{RunCancellationHandle, RunEffectClass, RunEffectGuard, RunEffectKind},
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
#[cfg(test)]
use approval_policy::session_grant_covers_decision;
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
use run_lifecycle::{append_failed_run_lifecycle_events, append_run_lifecycle_events};
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
#[derive(Clone)]
pub struct AgentRunInput {
    pub persisted_user_message: Option<String>,
    pub persisted_user_message_id: Option<String>,
    pub persisted_image_attachments: Vec<crate::ImageAttachment>,
    pub transient_context: Vec<ModelMessage>,
    pub runtime_context: RuntimeContextCandidates,
    pub task_plan_update: Option<TaskPlanUpdateContext>,
    pub agent_delegation: Option<AgentDelegationRequirement>,
    logical_run_id: Option<String>,
    cancellation: Option<RunCancellationHandle>,
    cancellation_terminal_authority: bool,
    source_capability_nonce: Option<String>,
    url_capability_issued_at_ms: Option<u64>,
    user_url_capability_registrar: Option<Arc<dyn crate::UserUrlCapabilityRegistrar>>,
    hosted_tools: Vec<crate::HostedToolRequest>,
    hosted_evidence_processor: Option<Arc<dyn crate::HostedEvidenceProcessor>>,
    hosted_turn_preparer: Option<Arc<dyn AgentHostedTurnPreparer>>,
    initial_frozen_provider_request: Option<FrozenProviderRequestMaterial>,
    max_output_tokens: Option<u32>,
    suppressed_tool_names: Vec<String>,
    web_task_tree_budget: Option<Arc<crate::WebTaskTreeBudget>>,
}

impl fmt::Debug for AgentRunInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentRunInput")
            .field(
                "persisted_user_message",
                &self.persisted_user_message.as_ref().map(|_| "[redacted]"),
            )
            .field("persisted_user_message_id", &self.persisted_user_message_id)
            .field(
                "persisted_image_attachment_count",
                &self.persisted_image_attachments.len(),
            )
            .field("transient_context_count", &self.transient_context.len())
            .field("runtime_context", &self.runtime_context)
            .field("task_plan_update", &self.task_plan_update)
            .field("agent_delegation", &self.agent_delegation)
            .field("logical_run_id", &self.logical_run_id)
            .field("cancellation", &self.cancellation)
            .field(
                "user_url_capability_registrar",
                &self
                    .user_url_capability_registrar
                    .as_ref()
                    .map(|_| "configured"),
            )
            .field("hosted_tools", &self.hosted_tools)
            .field(
                "hosted_turn_preparer",
                &self.hosted_turn_preparer.as_ref().map(|_| "configured"),
            )
            .field(
                "initial_frozen_provider_request",
                &self
                    .initial_frozen_provider_request
                    .as_ref()
                    .map(|request| request.fingerprint()),
            )
            .field("max_output_tokens", &self.max_output_tokens)
            .field("suppressed_tool_names", &self.suppressed_tool_names)
            .field(
                "web_task_tree_budget",
                &self.web_task_tree_budget.as_ref().map(|_| "configured"),
            )
            .field(
                "hosted_evidence_processor",
                &self
                    .hosted_evidence_processor
                    .as_ref()
                    .map(|_| "configured"),
            )
            .finish()
    }
}

impl AgentRunInput {
    pub fn user(prompt: impl Into<String>) -> Self {
        let message_id = uuid::Uuid::new_v4().to_string();
        Self {
            persisted_user_message: Some(prompt.into()),
            persisted_user_message_id: Some(message_id),
            persisted_image_attachments: Vec::new(),
            transient_context: Vec::new(),
            runtime_context: RuntimeContextCandidates::default(),
            task_plan_update: None,
            agent_delegation: None,
            logical_run_id: None,
            cancellation: None,
            cancellation_terminal_authority: true,
            source_capability_nonce: Some(uuid::Uuid::new_v4().to_string()),
            url_capability_issued_at_ms: Some(unix_time_ms()),
            user_url_capability_registrar: None,
            hosted_tools: Vec::new(),
            hosted_evidence_processor: None,
            hosted_turn_preparer: None,
            initial_frozen_provider_request: None,
            max_output_tokens: None,
            suppressed_tool_names: Vec::new(),
            web_task_tree_budget: None,
        }
    }

    pub fn transient(prompt: impl Into<String>, transient_context: Vec<ModelMessage>) -> Self {
        let message_id = uuid::Uuid::new_v4().to_string();
        Self {
            persisted_user_message: Some(prompt.into()),
            persisted_user_message_id: Some(message_id),
            persisted_image_attachments: Vec::new(),
            transient_context,
            runtime_context: RuntimeContextCandidates::default(),
            task_plan_update: None,
            agent_delegation: None,
            logical_run_id: None,
            cancellation: None,
            cancellation_terminal_authority: true,
            source_capability_nonce: Some(uuid::Uuid::new_v4().to_string()),
            url_capability_issued_at_ms: Some(unix_time_ms()),
            user_url_capability_registrar: None,
            hosted_tools: Vec::new(),
            hosted_evidence_processor: None,
            hosted_turn_preparer: None,
            initial_frozen_provider_request: None,
            max_output_tokens: None,
            suppressed_tool_names: Vec::new(),
            web_task_tree_budget: None,
        }
    }

    pub fn without_persisted_user_message(transient_context: Vec<ModelMessage>) -> Self {
        Self {
            persisted_user_message: None,
            persisted_user_message_id: None,
            persisted_image_attachments: Vec::new(),
            transient_context,
            runtime_context: RuntimeContextCandidates::default(),
            task_plan_update: None,
            agent_delegation: None,
            logical_run_id: None,
            cancellation: None,
            cancellation_terminal_authority: true,
            source_capability_nonce: None,
            url_capability_issued_at_ms: None,
            user_url_capability_registrar: None,
            hosted_tools: Vec::new(),
            hosted_evidence_processor: None,
            hosted_turn_preparer: None,
            initial_frozen_provider_request: None,
            max_output_tokens: None,
            suppressed_tool_names: Vec::new(),
            web_task_tree_budget: None,
        }
    }

    /// Adds process-local image bytes and durable metadata to the persisted user turn.
    #[must_use]
    pub fn with_image_attachments(mut self, attachments: Vec<crate::ImageAttachment>) -> Self {
        self.persisted_image_attachments = attachments;
        self
    }

    pub fn with_task_plan_update(mut self, context: TaskPlanUpdateContext) -> Self {
        self.task_plan_update = Some(context);
        self
    }

    /// Applies one provider-neutral output-token ceiling to every model turn in this run.
    #[must_use]
    pub fn with_max_output_tokens(mut self, max_output_tokens: u32) -> Self {
        self.max_output_tokens = Some(max_output_tokens);
        self
    }

    /// Enables provider-hosted tools with a mandatory process-local evidence finalizer.
    #[must_use]
    pub fn with_hosted_tools(
        mut self,
        hosted_tools: Vec<crate::HostedToolRequest>,
        processor: Arc<dyn crate::HostedEvidenceProcessor>,
    ) -> Self {
        self.hosted_tools = hosted_tools;
        self.hosted_evidence_processor = Some(processor);
        self
    }

    /// Declares hosted kinds independently so missing finalizer injection fails closed.
    #[must_use]
    pub fn with_hosted_tool_requests(
        mut self,
        hosted_tools: Vec<crate::HostedToolRequest>,
    ) -> Self {
        self.hosted_tools = hosted_tools;
        self
    }

    /// Injects the process-local hosted finalizer independently from request selection.
    #[must_use]
    pub fn with_hosted_evidence_processor(
        mut self,
        processor: Arc<dyn crate::HostedEvidenceProcessor>,
    ) -> Self {
        self.hosted_evidence_processor = Some(processor);
        self
    }

    /// Suppresses one otherwise registered client tool for this exact provider run.
    #[must_use]
    pub fn suppress_tool(mut self, name: impl Into<String>) -> Self {
        let name = name.into();
        if !self.suppressed_tool_names.contains(&name) {
            self.suppressed_tool_names.push(name);
        }
        self
    }

    /// Installs a runtime-owned per-provider-turn hosted authorization factory.
    #[must_use]
    pub fn with_hosted_turn_preparer(mut self, preparer: Arc<dyn AgentHostedTurnPreparer>) -> Self {
        self.hosted_turn_preparer = Some(preparer);
        self
    }

    /// Uses this already-frozen request for exactly the first provider turn of the run.
    ///
    /// This is intentionally narrow: the normal run-input preparer is skipped, the material is
    /// session/provider/model-bound before dispatch, and later turns use ordinary assembly.
    /// It lets a durable pre-send barrier hand off one proven request without rebuilding it.
    #[must_use]
    pub fn with_initial_frozen_provider_request(
        mut self,
        request: FrozenProviderRequestMaterial,
    ) -> Self {
        self.initial_frozen_provider_request = Some(request);
        self
    }

    /// Binds every Web effect in this run to the root-owned task-tree budget handle.
    #[must_use]
    pub fn with_web_task_tree_budget(mut self, budget: Arc<crate::WebTaskTreeBudget>) -> Self {
        self.web_task_tree_budget = Some(budget);
        self
    }

    /// Returns the already-bound root Web budget, when a parent/task runner supplied one.
    #[must_use]
    pub fn web_task_tree_budget(&self) -> Option<Arc<crate::WebTaskTreeBudget>> {
        self.web_task_tree_budget.clone()
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

    /// Binds this run to a caller-provided durable correlation id.
    ///
    /// Queued promotion uses this to bind its queue CAS to the first provider physical attempt.
    /// An empty identifier is rejected before the run can dispatch a provider request.
    #[must_use]
    pub fn with_logical_run_id(mut self, logical_run_id: impl Into<String>) -> Self {
        self.logical_run_id = Some(logical_run_id.into());
        self
    }

    /// Binds the runtime-owned live URL capability store to this kernel projection boundary.
    #[must_use]
    pub fn with_user_url_capability_registrar(
        mut self,
        registrar: Arc<dyn crate::UserUrlCapabilityRegistrar>,
    ) -> Self {
        self.user_url_capability_registrar = Some(registrar);
        self
    }

    /// Binds this run and all effects admitted by its agent loop to one cancellation owner.
    #[must_use]
    pub fn with_cancellation(mut self, cancellation: RunCancellationHandle) -> Self {
        self.cancellation = Some(cancellation);
        self.cancellation_terminal_authority = true;
        self
    }

    #[must_use]
    pub fn with_child_cancellation(mut self, cancellation: RunCancellationHandle) -> Self {
        self.cancellation = Some(cancellation);
        self.cancellation_terminal_authority = false;
        self
    }
}

fn begin_run_effect(
    cancellation: Option<&RunCancellationHandle>,
    kind: RunEffectKind,
) -> Result<Option<RunEffectGuard>> {
    cancellation
        .map(|handle| handle.begin_effect(RunEffectClass::Forward, kind))
        .transpose()
        .map_err(Into::into)
}

fn validate_initial_frozen_request(
    session: &Session,
    frozen_request: &FrozenProviderRequestMaterial,
) -> Result<()> {
    if frozen_request.session_scope_id() != session.session_scope_id() {
        return Err(anyhow!(
            "initial frozen provider request belongs to a different session scope"
        ));
    }
    let request = frozen_request.request();
    if request.provider_name != session.provider_name() {
        return Err(anyhow!(
            "initial frozen provider request provider does not match the session"
        ));
    }
    if request.model_name != session.model_name() {
        return Err(anyhow!(
            "initial frozen provider request model does not match the session"
        ));
    }
    Ok(())
}

fn claim_natural_run_terminal(
    cancellation: Option<&RunCancellationHandle>,
    terminal_authority: bool,
) -> Result<()> {
    if terminal_authority && cancellation.is_some_and(|handle| !handle.try_finalize_naturally()) {
        return Err(anyhow!("run cancellation won the terminal-state race"));
    }
    Ok(())
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
    /// Binds the current root run cancellation scope before delegated child work is admitted.
    fn set_run_cancellation(&mut self, _cancellation: Option<RunCancellationHandle>) {}

    /// Binds the root-owned Web budget so delegated children cannot create a fresh owner.
    fn set_web_task_tree_budget(&mut self, _budget: Option<Arc<crate::WebTaskTreeBudget>>) {}

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

/// Runtime-owned resolver for per-run capabilities that require the live provider and session.
#[async_trait]
pub trait AgentRunInputPreparer: Send + Sync {
    async fn prepare(
        &self,
        provider: &dyn Provider,
        session: &Session,
        input: AgentRunInput,
    ) -> Result<AgentRunInput>;
}

/// One independently authorized provider-hosted turn.
pub struct AgentHostedTurn {
    pub hosted_tools: Vec<crate::HostedToolRequest>,
    pub evidence_processor: Arc<dyn crate::HostedEvidenceProcessor>,
}

/// Runtime hook invoked immediately before every provider request in a multi-turn run.
#[async_trait]
pub trait AgentHostedTurnPreparer: Send + Sync {
    async fn prepare_turn(&self) -> Result<AgentHostedTurn>;
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

    /// Returns the provider implementation backing this agent.
    ///
    /// Callers must preserve the normal agent-run boundary for conversation generation. This
    /// accessor exists for adjacent provider-neutral admission capabilities that have their own
    /// durable lifecycle, such as a pre-send portable-compaction target proof.
    #[must_use]
    pub fn provider(&self) -> &P {
        &self.provider
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
        let input = if input.initial_frozen_provider_request.is_some() {
            input
        } else {
            match tools.run_input_preparer() {
                Some(preparer) => preparer.prepare(&self.provider, session, input).await?,
                None => input,
            }
        };
        let AgentRunInput {
            persisted_user_message,
            persisted_user_message_id,
            persisted_image_attachments,
            mut transient_context,
            runtime_context,
            task_plan_update,
            agent_delegation,
            logical_run_id,
            cancellation,
            cancellation_terminal_authority,
            source_capability_nonce,
            url_capability_issued_at_ms,
            user_url_capability_registrar,
            hosted_tools,
            hosted_evidence_processor,
            hosted_turn_preparer,
            mut initial_frozen_provider_request,
            max_output_tokens,
            suppressed_tool_names,
            web_task_tree_budget,
        } = input;
        // An explicit per-run registrar is useful for constrained callers and tests; production
        // sessions fall back to their non-serializable session-scoped runtime attachment so live
        // capabilities survive normal multi-turn ownership moves.
        let user_url_capability_registrar =
            user_url_capability_registrar.or_else(|| session.user_url_capability_registrar());

        if cancellation
            .as_ref()
            .is_some_and(RunCancellationHandle::is_cancel_requested)
        {
            return Err(anyhow!("run cancellation requested before agent start"));
        }

        session.reconcile_prepared_mutations(&options.workspace_root)?;
        session.reconcile_unfinished_write_tool_executions(&options.workspace_root)?;

        let mut current_run_overlays = Vec::new();
        if let Some(message) = persisted_user_message {
            let durable_message_id = persisted_user_message_id
                .ok_or_else(|| anyhow!("persisted user message is missing its durable entry id"))?;
            let projection = crate::project_user_message_with_attachments_for_persistence_with_nonce_and_issued_at(
                durable_message_id,
                message.clone(),
                persisted_image_attachments,
                source_capability_nonce.as_deref(),
                url_capability_issued_at_ms.ok_or_else(|| {
                    anyhow!("persisted user message is missing its URL capability issue time")
                })?,
                user_url_capability_registrar.as_ref(),
            )?;
            let existing_by_id = session.entries().iter().find_map(|entry| match entry {
                SessionLogEntry::User(existing) if existing.id == projection.durable_message.id => {
                    Some(existing)
                }
                _ => None,
            });
            if let Some(existing) = existing_by_id {
                if existing.content != projection.durable_message.content
                    || existing.image_attachments != projection.durable_message.image_attachments
                {
                    rollback_user_capabilities(
                        user_url_capability_registrar.as_ref(),
                        &projection.durable_message.id,
                    )?;
                    return Err(anyhow!(
                        "durable user message id already exists with different safe content"
                    ));
                }
            } else if let Err(error) =
                session.append_user_message(projection.durable_message.clone())
            {
                let rollback_error = rollback_user_capabilities(
                    user_url_capability_registrar.as_ref(),
                    &projection.durable_message.id,
                )
                .err();
                return Err(error.context(match rollback_error {
                    Some(rollback_error) => format!(
                        "failed to append safe user message; capability rollback also failed: {rollback_error:#}"
                    ),
                    None => "failed to append safe user message".to_owned(),
                }));
            }

            for registration in &projection.capability_registrations {
                let descriptor = registration.durable_descriptor(session.session_scope_id());
                descriptor.validate()?;
                let already_recorded = session.entries().iter().any(|entry| {
                    matches!(
                        entry,
                        SessionLogEntry::Control(ControlEntry::WebUrlCapabilityDescriptor(existing))
                            if existing == &descriptor
                    )
                });
                if !already_recorded
                    && let Err(error) =
                        session.append_control(ControlEntry::WebUrlCapabilityDescriptor(descriptor))
                {
                    let rollback_error = rollback_user_capabilities(
                        user_url_capability_registrar.as_ref(),
                        &projection.durable_message.id,
                    )
                    .err();
                    return Err(error.context(match rollback_error {
                            Some(rollback_error) => format!(
                                "failed to append URL capability descriptor; rollback also failed: {rollback_error:#}"
                            ),
                            None => "failed to append URL capability descriptor".to_owned(),
                        }));
                }
            }
            if let Some(registrar) = user_url_capability_registrar.as_ref()
                && let Err(error) = registrar.commit_message(&projection.durable_message.id)
            {
                let rollback_error = registrar
                    .rollback_message(&projection.durable_message.id)
                    .err();
                return Err(error.context(match rollback_error {
                        Some(rollback_error) => format!(
                            "failed to commit URL capabilities; rollback also failed: {rollback_error:#}"
                        ),
                        None => "failed to commit URL capabilities".to_owned(),
                    }));
            }
            current_run_overlays.push(projection.overlay);
        }

        let permission_policy = PermissionPolicy::new_with_context(
            &options.permission_config,
            &options.permission_context,
        );
        let logical_run_id =
            logical_run_id.unwrap_or_else(|| format!("agent-run-{}", uuid::Uuid::new_v4()));
        if logical_run_id.trim().is_empty() {
            return Err(anyhow!("agent logical run id is empty"));
        }
        let has_initial_frozen_provider_request = initial_frozen_provider_request.is_some();
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
            if cancellation
                .as_ref()
                .is_some_and(RunCancellationHandle::is_cancel_requested)
            {
                return Err(anyhow!("run cancellation requested before next model turn"));
            }
            if let Some(max_turns) = options.max_turns
                && model_turns >= max_turns
            {
                handler.handle(RunEvent::Notice(format!(
                    "Stopped after {model_turns} model turns: the model kept requesting tools and did not return a final answer. Send another message to continue from the recorded tool results."
                )))?;
                outcome.terminal_reason = AgentRunTerminalReason::MaxTurns;
                outcome.tool_calls = total_tool_calls;
                claim_natural_run_terminal(cancellation.as_ref(), cancellation_terminal_authority)?;
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

            let mut tool_specs = tools
                .specs()
                .into_iter()
                .filter(|spec| !suppressed_tool_names.contains(&spec.name))
                .collect::<Vec<_>>();
            if task_plan_update.is_some() {
                tool_specs.push(task_plan_update_tool_spec());
            }
            let initial_frozen_request = initial_frozen_provider_request.take();
            let provider_logical_run_id = if initial_frozen_request.is_some() {
                logical_run_id.clone()
            } else if has_initial_frozen_provider_request {
                // The caller-provided id is the durable handoff for the frozen first request
                // only. A later tool-follow-up turn is a distinct provider request and must not
                // create a second physical-attempt match for that queue promotion.
                format!(
                    "agent-run-{}:continuation:{model_turns}",
                    uuid::Uuid::new_v4()
                )
            } else {
                logical_run_id.clone()
            };
            let (request, current_hosted_processor) = match initial_frozen_request.as_ref() {
                Some(frozen_request) => {
                    validate_initial_frozen_request(session, frozen_request)?;
                    (
                        frozen_request.request().clone(),
                        hosted_evidence_processor.clone(),
                    )
                }
                None => {
                    let mut request = session
                        .build_request_with_transient_messages_context_overlays_and_max_tokens(
                            &options.workspace_root,
                            &options.memory_config,
                            tool_specs,
                            max_output_tokens,
                            options.reasoning_effort.clone(),
                            previous_response_handle.clone(),
                            options.traffic_partition_key.clone(),
                            &transient_context,
                            runtime_context.clone(),
                            &current_run_overlays,
                        )?;
                    let prepared_hosted_turn = match hosted_turn_preparer.as_ref() {
                        Some(preparer) => Some(preparer.prepare_turn().await?),
                        None => None,
                    };
                    let current_hosted_tools = prepared_hosted_turn
                        .as_ref()
                        .map_or(hosted_tools.as_slice(), |turn| turn.hosted_tools.as_slice());
                    request.hosted_tools = current_hosted_tools.to_vec();
                    let current_hosted_processor = prepared_hosted_turn
                        .as_ref()
                        .map(|turn| Arc::clone(&turn.evidence_processor))
                        .or_else(|| hosted_evidence_processor.clone());
                    (request, current_hosted_processor)
                }
            };

            let provider_effect =
                begin_run_effect(cancellation.as_ref(), RunEffectKind::ProviderRequest)?;
            let provider_turn_result = match initial_frozen_request {
                Some(frozen_request) => {
                    provider_stream::collect_frozen_provider_turn(
                        &self.provider,
                        session,
                        frozen_request,
                        &provider_logical_run_id,
                        &mut previous_response_handle,
                        total_tool_calls,
                        handler,
                        cancellation.as_ref(),
                        current_hosted_processor.as_ref(),
                    )
                    .await
                }
                None => {
                    collect_provider_turn(
                        &self.provider,
                        session,
                        request,
                        &provider_logical_run_id,
                        &mut previous_response_handle,
                        total_tool_calls,
                        handler,
                        cancellation.as_ref(),
                        current_hosted_processor.as_ref(),
                    )
                    .await
                }
            };
            drop(provider_effect);
            let provider_turn = match provider_turn_result {
                Ok(provider_turn) => provider_turn,
                Err(error) => {
                    append_failed_run_lifecycle_events(
                        session,
                        "provider_stream_error",
                        total_tool_calls,
                        "provider turn failed before a safe terminal result",
                    )?;
                    return Err(error);
                }
            };
            let assistant_text = provider_turn.assistant_text;
            let completed_calls = provider_turn
                .completed_calls
                .into_iter()
                .map(crate::ToolCallPersistenceProjection::into_exact_call)
                .collect::<Vec<_>>();
            let pending_states = provider_turn.pending_states;
            let hosted_finalized = provider_turn.hosted_finalized;

            append_reasoning_trace(session, &provider_turn.reasoning_trace)?;

            if !completed_calls.is_empty() {
                total_tool_calls += completed_calls.len();
                let tool_preamble_overlay = append_tool_preamble_message(
                    session,
                    handler,
                    tools,
                    &assistant_text,
                    &completed_calls,
                    pending_states,
                )?;
                current_run_overlays.push(tool_preamble_overlay);

                let mut tool_ctx =
                    ToolContext::new(options.workspace_root.clone(), options.tool_timeout_secs)
                        .with_network_authorization(
                            options.permission_context.network_policy,
                            false,
                        );
                if let Some(cancellation) = cancellation.as_ref() {
                    tool_ctx = tool_ctx.with_cancellation(cancellation.clone());
                }
                if let Some(recorder) = session.mutation_event_recorder() {
                    tool_ctx = tool_ctx.with_mutation_recorder(recorder);
                }
                if let Ok(recorder) = session.egress_audit_recorder() {
                    tool_ctx = tool_ctx.with_egress_audit_recorder(recorder);
                }
                if let Some(registrar) = user_url_capability_registrar.as_ref() {
                    tool_ctx = tool_ctx
                        .with_user_url_capability_registrar(Arc::clone(registrar))
                        .with_session_scope_id(session.session_scope_id().to_owned());
                }
                if let Some(budget) = web_task_tree_budget.as_ref() {
                    tool_ctx = tool_ctx.with_web_task_tree_budget(Arc::clone(budget));
                }
                let accepted_task_plan_in_batch = completed_calls.iter().any(|call| {
                    task_plan_update
                        .as_ref()
                        .is_some_and(|context| task_plan_update_call_is_accepted(context, call))
                });
                let mut accepted_task_plan = false;
                for mut call in completed_calls {
                    let safe_call =
                        crate::project_tool_call_for_persistence(call.clone())?.durable_call;
                    let mut explicit_network_approval = false;
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
                    let _tool_effect =
                        begin_run_effect(cancellation.as_ref(), RunEffectKind::Tool)?;
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
                        let network_effect = match tools.permission_network_effect(&tool_ctx, &call)
                        {
                            Ok(network_effect) => network_effect,
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
                        let decision = permission_policy
                            .decide_with_operation_network_effect_and_default(
                                spec,
                                &call.name,
                                access,
                                operation,
                                network_effect,
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
                                    call: safe_call.clone(),
                                    spec: spec.clone(),
                                    subjects: decision.subjects.clone(),
                                    network_effect: decision.network_effect,
                                    local_policy_decision: decision.local_policy_decision,
                                    network_policy_decision: decision.network_policy_decision,
                                    source_policy_decision: decision.source_policy_decision,
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
                                let approval =
                                    approval_handler.approve_tool_call(&safe_call, spec)?;
                                let approval_is_explicit_user_action =
                                    approval_handler.approval_is_explicit_user_action();
                                let approval_would_allow = matches!(
                                    &approval,
                                    ToolApproval::Approve
                                        | ToolApproval::ApproveForSession
                                        | ToolApproval::ApproveWithArgs { .. }
                                );
                                if approval_would_allow
                                    && decision.network_policy_decision == ApprovalMode::Ask
                                    && !approval_is_explicit_user_action
                                {
                                    let reason =
                                        "network approval requires an explicit user action"
                                            .to_owned();
                                    append_tool_approval_audit(
                                        session,
                                        &call,
                                        &decision,
                                        ToolApprovalAuditAction::Resolved,
                                        None,
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
                                let approval_is_explicit_network_user_action =
                                    approval_is_explicit_user_action
                                        && decision.network_effect.is_some()
                                        && decision.network_policy_decision == ApprovalMode::Ask;
                                match approval {
                                    ToolApproval::Approve => {
                                        explicit_network_approval =
                                            approval_is_explicit_network_user_action;
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
                                        explicit_network_approval =
                                            approval_is_explicit_network_user_action;
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
                                        let mut approved_call = call.clone();
                                        approved_call.args_json = args_json;
                                        let reevaluate_approved_call = || -> Result<_> {
                                            let approved_subjects = tools
                                                .permission_subjects(&tool_ctx, &approved_call)?;
                                            let approved_access = tools
                                                .permission_access(&tool_ctx, &approved_call)?;
                                            let approved_network_effect = tools
                                                .permission_network_effect(
                                                    &tool_ctx,
                                                    &approved_call,
                                                )?;
                                            let approved_operation = tools
                                                .permission_operation(&tool_ctx, &approved_call)?;
                                            let approved_default_mode = tools
                                                .permission_default_mode(
                                                    &tool_ctx,
                                                    &approved_call,
                                                )?;
                                            let approved_decision = permission_policy
                                                .decide_with_operation_network_effect_and_default(
                                                    spec,
                                                    &approved_call.name,
                                                    approved_access,
                                                    approved_operation,
                                                    approved_network_effect,
                                                    approved_subjects,
                                                    approved_default_mode,
                                                )?;
                                            let approved_decision =
                                                interactive_external_directory_approval_override(
                                                    &options,
                                                    approved_decision,
                                                );
                                            let approved_decision = plan_approval_decision_override(
                                                session,
                                                spec,
                                                approved_decision,
                                            );
                                            Ok(tool_session_grant_decision_override(
                                                session,
                                                &approved_call.name,
                                                approved_decision,
                                            )
                                            .0)
                                        };
                                        let approved_decision = reevaluate_approved_call();
                                        let approved_decision = match approved_decision {
                                            Ok(decision) => decision,
                                            Err(error) => {
                                                let reason = format!(
                                                    "approval-time argument changes could not be re-evaluated: {error}"
                                                );
                                                append_tool_approval_audit(
                                                    session,
                                                    &approved_call,
                                                    &decision,
                                                    ToolApprovalAuditAction::Resolved,
                                                    Some(ToolApprovalUserDecision::Denied),
                                                    Some(reason.clone()),
                                                    preview_hash,
                                                )?;
                                                handler.handle(RunEvent::ToolApprovalResolved {
                                                    call_id: approved_call.id.clone(),
                                                    approved: false,
                                                    reason: Some(reason),
                                                })?;
                                                append_invalid_tool_input_result(
                                                    session,
                                                    handler,
                                                    &mut outcome,
                                                    &approved_call,
                                                    &decision.subjects,
                                                    error,
                                                )?;
                                                continue;
                                            }
                                        };
                                        if approved_decision != decision {
                                            let reason = "approval-time argument changes altered the permission scope; preview and approval must be repeated"
                                                .to_owned();
                                            append_tool_approval_audit(
                                                session,
                                                &approved_call,
                                                &approved_decision,
                                                ToolApprovalAuditAction::Resolved,
                                                Some(ToolApprovalUserDecision::Denied),
                                                Some(reason.clone()),
                                                preview_hash,
                                            )?;
                                            handler.handle(RunEvent::ToolApprovalResolved {
                                                call_id: approved_call.id.clone(),
                                                approved: false,
                                                reason: Some(reason.clone()),
                                            })?;
                                            let mut result = ToolResult::error(
                                                approved_call.id.clone(),
                                                approved_call.name.clone(),
                                                ToolErrorKind::ApprovalDenied,
                                                reason,
                                            );
                                            attach_tool_call_context(
                                                &mut result,
                                                &approved_call,
                                                &approved_decision.subjects,
                                            );
                                            record_and_emit_tool_result(
                                                session,
                                                handler,
                                                &mut outcome,
                                                result,
                                            )?;
                                            continue;
                                        }
                                        call = approved_call;
                                        execution_subjects = approved_decision.subjects.clone();
                                        explicit_network_approval = approval_is_explicit_user_action
                                            && approved_decision.network_effect.is_some()
                                            && approved_decision.network_policy_decision
                                                == ApprovalMode::Ask;
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
                        let network_effect = tools.permission_network_effect(&tool_ctx, &call)?;
                        let operation = tools.permission_operation(&tool_ctx, &call)?;
                        let tool_default_mode = tools.permission_default_mode(&tool_ctx, &call)?;
                        let current_decision = permission_policy
                            .decide_with_operation_network_effect_and_default(
                                spec,
                                &call.name,
                                access,
                                operation,
                                network_effect,
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
                        .with_network_authorization(
                            options.permission_context.network_policy,
                            explicit_network_approval,
                        )
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
                            Some(delegate) => {
                                delegate.set_run_cancellation(cancellation.clone());
                                delegate.set_web_task_tree_budget(web_task_tree_budget.clone());
                                match delegate
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
                                }
                            }
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
                    claim_natural_run_terminal(
                        cancellation.as_ref(),
                        cancellation_terminal_authority,
                    )?;
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
                claim_natural_run_terminal(cancellation.as_ref(), cancellation_terminal_authority)?;
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

            claim_natural_run_terminal(cancellation.as_ref(), cancellation_terminal_authority)?;
            let mut hosted_finalized = hosted_finalized;
            let url_capability_registrations = hosted_finalized
                .as_mut()
                .map(|finalized| std::mem::take(&mut finalized.url_capability_registrations))
                .unwrap_or_default();
            let final_message_id = append_final_answer_message(
                session,
                handler,
                &assistant_text,
                pending_states,
                url_capability_registrations,
            )?;
            if let Some(finalized) = hosted_finalized {
                let final_safe_text = session
                    .entries()
                    .iter()
                    .rev()
                    .find_map(|entry| match entry {
                        SessionLogEntry::Assistant(message) if message.id == final_message_id => {
                            message.content.as_deref()
                        }
                        _ => None,
                    })
                    .unwrap_or_default()
                    .to_owned();
                let provenance = finalized.to_provenance(
                    session.session_scope_id().to_owned(),
                    final_message_id.clone(),
                    &final_safe_text,
                );
                if !provenance.sources.is_empty() || !provenance.citations.is_empty() {
                    session.append_external_provenance(provenance)?;
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

fn rollback_user_capabilities(
    registrar: Option<&Arc<dyn crate::UserUrlCapabilityRegistrar>>,
    durable_message_id: &str,
) -> Result<()> {
    registrar.map_or(Ok(()), |registrar| {
        registrar.rollback_message(durable_message_id)
    })
}

#[cfg(test)]
#[path = "tests/network_approval_override_tests.rs"]
mod network_approval_tests;
#[cfg(test)]
#[path = "tests/agent_tests.rs"]
mod tests;
