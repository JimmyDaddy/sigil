use std::{
    collections::BTreeMap,
    fmt,
    sync::Arc,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use serde_json::{Map, Value};
use tokio::sync::mpsc;

use crate::{
    FrozenProviderRequestMaterial, PlanId, RuntimeContextCandidates,
    approval::{
        APPROVAL_REQUEST_NO_EXPIRY_MS, ApprovalHandler, ApprovalRequestIdentityV2,
        AutoApproveHandler, ToolApproval, ToolApprovalContext,
    },
    cancellation::{RunCancellationHandle, RunEffectClass, RunEffectGuard, RunEffectKind},
    config::{CompactionConfig, MemoryConfig, TaskRoutingPolicy},
    conversation_route::{
        AutomaticRouteCapability, ConversationRouteDecisionId, PlanReviewAttemptId,
        PlanReviewDraftContext, PlanReviewHandoffBinding, PlanReviewId,
        REQUEST_PLAN_REVIEW_TOOL_NAME, SUBMIT_PLAN_DRAFT_TOOL_NAME,
        conversation_route_routing_contract_material,
        direct_conversation_continuation_prompt_contract_material, request_plan_review_tool_spec,
        submit_plan_draft_tool_spec,
    },
    event::{EventHandler, RunEvent},
    memory::{is_writable_memory_route_tool, writable_memory_route_tool_specs},
    permission::{
        ApprovalMode, InteractionMode, PathTrustZone, PermissionConfig,
        PermissionEvaluationContext, PermissionPolicyChain, ToolApprovalSessionGrantFacet,
        tool_approval_session_grant_availability_for_plan,
    },
    permission_plan::ToolPermissionPlanV2,
    provider::{ModelMessage, Provider, ToolCall},
    session::{
        ControlEntry, Session, SessionLogEntry, ToolApprovalAuditAction,
        ToolApprovalTerminalStatusV2, ToolApprovalUserDecision, ToolExecutionStatus,
    },
    task::{
        TASK_GUIDANCE_APPLY_TOOL_NAME, TASK_PLAN_UPDATE_TOOL_NAME, TaskGuidanceAssessmentContext,
        TaskId, TaskParticipantAttemptId, TaskPlanStatus, TaskPlanUpdateContext, TaskRunStatus,
        TaskStepId, task_guidance_apply_tool_spec, task_plan_update_tool_spec_for_worktree,
    },
    task_handoff::{
        CONTINUE_EXISTING_TASK_TOOL_NAME, CONTINUE_WITHOUT_TASK_PLANNING_TOOL_NAME,
        ConversationTurnRef, REQUEST_TASK_PLANNING_TOOL_NAME, TaskContinuationHandoffBinding,
        TaskHandoffId, TaskPlanningHandoffBinding, continue_existing_task_tool_spec,
        continue_without_task_planning_tool_spec, request_task_planning_tool_spec,
    },
    task_orchestrator::{
        task_participant_finalization_prompt_contract_material,
        task_participant_system_prompt_contract_material,
        task_planner_system_prompt_contract_material,
    },
    tool::{
        PreparedToolCall, ToolAccess, ToolCategory, ToolContext, ToolErrorKind, ToolProgressEvent,
        ToolProgressSink, ToolRegistry, ToolResult, ToolSpec, ToolSubject,
    },
};

#[cfg(test)]
use crate::permission::PermissionPolicy;

mod approval_policy;
mod assistant_messages;
mod plan_draft;
mod plan_review;
mod preview;
mod provider_stream;
mod readiness;
mod run_lifecycle;
mod task_guidance;
mod task_handoff;
mod task_plan;
pub(crate) mod tool_audit;
mod tool_results;
mod user_input;
use approval_policy::{
    active_plan_approval_authority, interactive_external_directory_approval_override,
    plan_approval_decision_override, tool_session_grant_decision_override,
};
use assistant_messages::{
    append_final_answer_message, append_tool_preamble_message, save_continuation_states,
};
use plan_draft::{
    append_tool_ignored_after_plan_draft, handle_submit_plan_draft_call,
    submit_plan_draft_call_is_accepted,
};
use plan_review::{
    append_tool_ignored_after_plan_review_decision, handle_request_plan_review_call,
    plan_review_call_is_accepted,
};
use preview::{
    approval_permission_signature, capture_tool_preview_for_decision,
    pending_interactive_approval_identity, preparation_plan_approval_identity,
    preparation_policy_approval_identity, preparation_policy_fingerprint,
    preparation_session_grant_identity, resolved_interactive_approval_identity,
};
use provider_stream::collect_provider_turn;
pub use readiness::projected_agent_run_readiness;
use run_lifecycle::{
    append_completed_run_lifecycle_events, append_failed_run_lifecycle_events,
    append_run_lifecycle_events,
};
use task_guidance::{
    append_tool_ignored_after_task_guidance_acceptance, handle_task_guidance_apply_call,
    task_guidance_apply_call_is_accepted,
};
use task_handoff::{
    append_tool_ignored_after_routing_decision, append_tool_ignored_after_task_handoff,
    append_tool_rejected_during_task_routing, continue_existing_task_call_is_accepted,
    continue_without_task_planning_call_is_accepted, handle_continue_existing_task_call,
    handle_continue_without_task_planning_call, handle_task_planning_request_call,
    task_planning_request_call_is_accepted,
};
use task_plan::{
    append_tool_ignored_after_task_plan_acceptance, handle_task_plan_update_call,
    task_plan_update_call_is_accepted,
};
pub use tool_audit::durable_tool_execution_entry;
use tool_audit::{
    append_terminal_task_control_from_result, append_tool_approval_audit,
    append_tool_approval_policy_audit, append_tool_approval_session_grant,
    append_tool_control_entries_from_result, append_tool_execution_audit,
    append_tool_execution_started_audit, append_tool_permission_plan_audit,
    attach_prepared_tool_audit_binding, attach_tool_call_context, duration_ms,
    reconcile_terminal_task_mutation_from_start, tool_egress_control_entry,
};
#[cfg(test)]
use tool_audit::{
    external_directory_preview, stable_json_hash, stable_text_hash, tool_call_context,
};
#[cfg(test)]
use tool_results::emit_tool_result;
use tool_results::{
    agent_tool_result_satisfies_delegation, append_invalid_tool_input_result,
    emit_tool_result_batch, record_tool_run_outcome,
};
use user_input::{
    RequestUserInputContext, append_request_user_input_error,
    append_tool_ignored_after_user_input_request, handle_request_user_input_call,
    request_user_input_call_is_accepted,
};

const TASK_PARTICIPANT_POST_MUTATION_READ_TAIL_LIMIT: usize = 6;
const MAX_FINAL_ANSWER_BLOCKER_RETRIES: usize = 3;

struct RoutingMicroturnEventFilter<'a, H> {
    inner: &'a mut H,
    suppress_narrative: bool,
}

impl<'a, H> RoutingMicroturnEventFilter<'a, H> {
    fn new(inner: &'a mut H, suppress_narrative: bool) -> Self {
        Self {
            inner,
            suppress_narrative,
        }
    }
}

impl<H> EventHandler for RoutingMicroturnEventFilter<'_, H>
where
    H: EventHandler,
{
    fn handle(&mut self, event: RunEvent) -> Result<()> {
        if self.suppress_narrative
            && matches!(
                event,
                RunEvent::TextDelta(_)
                    | RunEvent::ReasoningDelta(_)
                    | RunEvent::AssistantMessage(_)
            )
        {
            return Ok(());
        }
        self.inner.handle(event)
    }
}

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
    pub permission_mode_override: Option<crate::PermissionModeOverride>,
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

/// Host-owned purpose for one model run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentRunPurpose {
    Conversation(Box<ConversationPurposeContext>),
    PlanReview(PlanReviewPurposeContext),
    TaskPlanner(TaskPlannerContext),
    TaskParticipant(TaskParticipantContext),
    TaskSynthesis(TaskSynthesisContext),
}

/// Root conversation facts controlling internal tool visibility and handoff authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationPurposeContext {
    pub root_run_id: String,
    pub source_turn: ConversationTurnRef,
    pub routing_policy: TaskRoutingPolicy,
    /// Effective automatic route capability derived from exact provider/model/build evidence.
    pub route_capability: AutomaticRouteCapability,
    /// Effective writable-memory routing capability derived after the host assembles the exact
    /// tool registry for this run.
    pub writable_memory_routing: bool,
    /// Direct durable task handoff binding; present only when the capability allows DirectTask.
    pub task_handoff: Option<TaskPlanningHandoffBinding>,
    /// Plan review handoff binding; present whenever automatic routing may choose PlanReview.
    pub plan_review: Option<PlanReviewHandoffBinding>,
    /// Exact current resumable Task that may be selected by the routing microturn.
    pub task_continuation: Option<TaskContinuationHandoffBinding>,
}

/// Purpose binding for one read-only plan review run.
///
/// The run produces a `PlanDraftCreated` awaiting a user decision; it never creates a TaskRun,
/// executes write steps, or obtains parent final execution authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanReviewPurposeContext {
    pub plan_review_id: PlanReviewId,
    pub attempt_id: PlanReviewAttemptId,
    pub plan_id: PlanId,
    pub source_turn: ConversationTurnRef,
    pub route_decision_id: Option<ConversationRouteDecisionId>,
}

/// Purpose binding for the internal task planner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskPlannerContext {
    pub task_id: TaskId,
    pub attempt_id: Option<TaskParticipantAttemptId>,
}

/// Purpose binding for one task plan participant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskParticipantContext {
    pub task_id: TaskId,
    pub plan_version: u32,
    pub step_id: TaskStepId,
    pub attempt_id: TaskParticipantAttemptId,
}

/// Purpose binding for the single task synthesis run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskSynthesisContext {
    pub task_id: TaskId,
    pub plan_version: u32,
    pub attempt_id: TaskParticipantAttemptId,
}

/// Typed disposition that callers must inspect before finalizing a root run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentRunDisposition {
    FinalAnswer,
    AwaitingUserInput(crate::UserInputRequestRefV1),
    StartPlanReview(StartPlanReviewAction),
    PlanReviewDraftSubmitted(PlanReviewDraftSubmittedAction),
    StartDurableTask(StartDurableTaskAction),
    ContinueDurableTask(Box<ContinueDurableTaskAction>),
    TaskPlanAccepted,
    Interrupted,
    Blocked,
}

/// Stable action emitted after a PlanReview route decision is accepted.
///
/// Carries only host-bound identity/reference; the caller continues the read-only plan review
/// lifecycle through the shared runtime coordinator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartPlanReviewAction {
    pub decision_id: ConversationRouteDecisionId,
    pub plan_review_id: PlanReviewId,
    pub plan_id: PlanId,
    pub source_turn: ConversationTurnRef,
}

/// Stable action emitted when a plan review run submitted a validated typed draft.
///
/// The draft itself is recorded on the plan review run session; this action only carries identity
/// so the runtime coordinator can commit the draft and attempt status to the parent session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanReviewDraftSubmittedAction {
    pub plan_review_id: PlanReviewId,
    pub attempt_id: PlanReviewAttemptId,
    pub plan_id: PlanId,
}

/// Stable action emitted after a durable conversation-to-task handoff is accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartDurableTaskAction {
    pub handoff_id: TaskHandoffId,
    pub task_id: TaskId,
    pub source_turn: ConversationTurnRef,
}

/// Stable action emitted after semantic routing selected one exact existing durable Task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContinueDurableTaskAction {
    pub task_id: TaskId,
    pub source_turn: ConversationTurnRef,
    pub plan_version: Option<u32>,
    pub task_status: TaskRunStatus,
    pub plan_status: Option<TaskPlanStatus>,
    pub route_contract_fingerprint: String,
    /// Exact source prompt retained only in process memory for Task guidance review.
    pub guidance: crate::SecretString,
    /// Durable safe receipt that the adapter revalidates before dispatch.
    pub guidance_receipt: crate::TaskContinuationSelectedEntry,
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
    pub plan_review_draft: Option<PlanReviewDraftContext>,
    pub plan_review_submit_only: bool,
    pub task_guidance_assessment: Option<TaskGuidanceAssessmentContext>,
    pub agent_delegation: Option<AgentDelegationRequirement>,
    pub purpose: Option<AgentRunPurpose>,
    agent_invocation_grant: Option<crate::AgentInvocationGrant>,
    logical_run_id: Option<String>,
    source_thread_id: Option<crate::AgentThreadId>,
    user_input_root_logical_run_id: Option<String>,
    cancellation: Option<RunCancellationHandle>,
    cancellation_terminal_authority: bool,
    source_capability_nonce: Option<String>,
    url_capability_issued_at_ms: Option<u64>,
    user_url_capability_registrar: Option<Arc<dyn crate::UserUrlCapabilityRegistrar>>,
    hosted_tools: Vec<crate::HostedToolRequest>,
    hosted_evidence_processor: Option<Arc<dyn crate::HostedEvidenceProcessor>>,
    hosted_turn_preparer: Option<Arc<dyn AgentHostedTurnPreparer>>,
    pending_input_provider: Option<Arc<dyn PendingConversationInputProvider>>,
    initial_frozen_provider_request: Option<FrozenProviderRequestMaterial>,
    max_output_tokens: Option<u32>,
    suppressed_tool_names: Vec<String>,
    web_task_tree_budget: Option<Arc<crate::WebTaskTreeBudget>>,
    tool_artifact_read_budget: Option<crate::session::ToolArtifactReadBudgetV1>,
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
            .field(
                "tool_artifact_read_budget",
                &self.tool_artifact_read_budget.is_some(),
            )
            .field("task_plan_update", &self.task_plan_update)
            .field(
                "task_guidance_assessment",
                &self.task_guidance_assessment.as_ref().map(|context| {
                    (
                        context.queue_id.as_str(),
                        context.task_id.as_str(),
                        context.plan_version,
                        context.eligible_pending_step_ids.len(),
                    )
                }),
            )
            .field("agent_delegation", &self.agent_delegation)
            .field("purpose", &self.purpose)
            .field(
                "agent_invocation_grant",
                &self
                    .agent_invocation_grant
                    .as_ref()
                    .map(crate::AgentInvocationGrant::fingerprint),
            )
            .field("logical_run_id", &self.logical_run_id)
            .field("source_thread_id", &self.source_thread_id)
            .field(
                "user_input_root_logical_run_id",
                &self.user_input_root_logical_run_id,
            )
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
            plan_review_draft: None,
            plan_review_submit_only: false,
            task_guidance_assessment: None,
            agent_delegation: None,
            purpose: None,
            agent_invocation_grant: None,
            logical_run_id: None,
            source_thread_id: None,
            user_input_root_logical_run_id: None,
            cancellation: None,
            cancellation_terminal_authority: true,
            source_capability_nonce: Some(uuid::Uuid::new_v4().to_string()),
            url_capability_issued_at_ms: Some(unix_time_ms()),
            user_url_capability_registrar: None,
            hosted_tools: Vec::new(),
            hosted_evidence_processor: None,
            hosted_turn_preparer: None,
            pending_input_provider: None,
            initial_frozen_provider_request: None,
            max_output_tokens: None,
            suppressed_tool_names: Vec::new(),
            web_task_tree_budget: None,
            tool_artifact_read_budget: None,
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
            plan_review_draft: None,
            plan_review_submit_only: false,
            task_guidance_assessment: None,
            agent_delegation: None,
            purpose: None,
            agent_invocation_grant: None,
            logical_run_id: None,
            source_thread_id: None,
            user_input_root_logical_run_id: None,
            cancellation: None,
            cancellation_terminal_authority: true,
            source_capability_nonce: Some(uuid::Uuid::new_v4().to_string()),
            url_capability_issued_at_ms: Some(unix_time_ms()),
            user_url_capability_registrar: None,
            hosted_tools: Vec::new(),
            hosted_evidence_processor: None,
            hosted_turn_preparer: None,
            pending_input_provider: None,
            initial_frozen_provider_request: None,
            max_output_tokens: None,
            suppressed_tool_names: Vec::new(),
            web_task_tree_budget: None,
            tool_artifact_read_budget: None,
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
            plan_review_draft: None,
            plan_review_submit_only: false,
            task_guidance_assessment: None,
            agent_delegation: None,
            purpose: None,
            agent_invocation_grant: None,
            logical_run_id: None,
            source_thread_id: None,
            user_input_root_logical_run_id: None,
            cancellation: None,
            cancellation_terminal_authority: true,
            source_capability_nonce: None,
            url_capability_issued_at_ms: None,
            user_url_capability_registrar: None,
            hosted_tools: Vec::new(),
            hosted_evidence_processor: None,
            hosted_turn_preparer: None,
            pending_input_provider: None,
            initial_frozen_provider_request: None,
            max_output_tokens: None,
            suppressed_tool_names: Vec::new(),
            web_task_tree_budget: None,
            tool_artifact_read_budget: None,
        }
    }

    /// Adds process-local image bytes and durable metadata to the persisted user turn.
    #[must_use]
    pub fn with_image_attachments(mut self, attachments: Vec<crate::ImageAttachment>) -> Self {
        self.persisted_image_attachments = attachments;
        self
    }

    /// Computes the exact durable user message that this run will append, without staging live
    /// URL capabilities or mutating session state.
    ///
    /// Runtime admission uses this projection so a task handoff objective cannot drift from URL
    /// capability labels or image placeholders applied later by the agent loop.
    ///
    /// # Errors
    ///
    /// Returns an error when a persisted input is missing its identity/capability facts or its
    /// attachment metadata is invalid.
    pub fn durable_user_message_projection(&self) -> Result<Option<ModelMessage>> {
        let Some(message) = self.persisted_user_message.as_ref() else {
            return Ok(None);
        };
        let durable_message_id = self
            .persisted_user_message_id
            .as_ref()
            .ok_or_else(|| anyhow!("persisted user message is missing its durable entry id"))?;
        let issued_at_ms = self.url_capability_issued_at_ms.ok_or_else(|| {
            anyhow!("persisted user message is missing its URL capability issue time")
        })?;
        crate::project_user_message_with_attachments_for_persistence_with_nonce_and_issued_at(
            durable_message_id.clone(),
            message.clone(),
            self.persisted_image_attachments.clone(),
            self.source_capability_nonce.as_deref(),
            issued_at_ms,
            None,
        )
        .map(|projection| Some(projection.durable_message))
    }

    /// Returns the exact process-local prompt material for one bound source turn.
    ///
    /// Direct inputs retain it in the pending persisted message; queued inputs retain it in the
    /// caller-frozen provider request. The value is never serialized by this accessor.
    #[must_use]
    pub fn exact_user_prompt_for_source(&self, message_id: &str) -> Option<&str> {
        if self.persisted_user_message_id.as_deref() == Some(message_id) {
            return self.persisted_user_message.as_deref();
        }
        self.initial_frozen_provider_request
            .as_ref()
            .and_then(|frozen| {
                frozen
                    .request()
                    .messages
                    .iter()
                    .find(|message| {
                        message.id == message_id && message.role == crate::MessageRole::User
                    })
                    .and_then(|message| message.content.as_deref())
            })
    }

    pub fn with_task_plan_update(mut self, context: TaskPlanUpdateContext) -> Self {
        if self.purpose.is_none() {
            self.purpose = Some(AgentRunPurpose::TaskPlanner(TaskPlannerContext {
                task_id: context.task_id.clone(),
                attempt_id: None,
            }));
        }
        self.task_plan_update = Some(context);
        self
    }

    /// Binds the typed plan review draft context for a read-only plan review run.
    #[must_use]
    pub fn with_plan_review_draft(mut self, context: PlanReviewDraftContext) -> Self {
        self.plan_review_draft = Some(context);
        self
    }

    /// Restricts a fresh plan finalizer to the typed draft submission protocol.
    #[must_use]
    pub fn with_plan_review_submit_only(mut self) -> Self {
        self.plan_review_submit_only = true;
        self
    }

    /// Enables model-owned review of whether guidance supplements the accepted plan or replans it.
    #[must_use]
    pub fn with_task_guidance_assessment(mut self, context: TaskGuidanceAssessmentContext) -> Self {
        if self.purpose.is_none() {
            self.purpose = Some(AgentRunPurpose::TaskPlanner(TaskPlannerContext {
                task_id: context.task_id.clone(),
                attempt_id: None,
            }));
        }
        self.task_guidance_assessment = Some(context);
        self
    }

    /// Binds the host-owned run purpose used for internal protocol admission.
    #[must_use]
    pub fn with_run_purpose(mut self, purpose: AgentRunPurpose) -> Self {
        self.purpose = Some(purpose);
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

    /// Installs a runtime-owned hook that may inject queued follow-ups at safe turn boundaries.
    #[must_use]
    pub fn with_pending_input_provider(
        mut self,
        provider: Arc<dyn PendingConversationInputProvider>,
    ) -> Self {
        self.pending_input_provider = Some(provider);
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

    /// Binds artifact retrieval in this run and delegated descendants to one root-owned budget.
    #[must_use]
    pub fn with_tool_artifact_read_budget(
        mut self,
        budget: crate::session::ToolArtifactReadBudgetV1,
    ) -> Self {
        self.tool_artifact_read_budget = Some(budget);
        self
    }

    /// Returns the shared root artifact-read budget supplied by a parent run.
    #[must_use]
    pub fn tool_artifact_read_budget(&self) -> Option<crate::session::ToolArtifactReadBudgetV1> {
        self.tool_artifact_read_budget.clone()
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

    /// Binds every child tool effect to the exact process-local invocation capability.
    #[must_use]
    pub fn with_agent_invocation_grant(mut self, grant: crate::AgentInvocationGrant) -> Self {
        self.agent_invocation_grant = Some(grant);
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

    /// Binds host-owned user-input requests to the concrete agent thread that emitted them.
    #[must_use]
    pub fn with_source_thread_id(mut self, source_thread_id: crate::AgentThreadId) -> Self {
        self.source_thread_id = Some(source_thread_id);
        self
    }

    /// Re-enables host-owned user-input suspension for a resumed conversation without restoring
    /// automatic routing authority or synthesizing another user message.
    #[must_use]
    pub fn with_user_input_continuation_context(
        mut self,
        root_logical_run_id: impl Into<String>,
        source_thread_id: crate::AgentThreadId,
    ) -> Self {
        self.user_input_root_logical_run_id = Some(root_logical_run_id.into());
        self.source_thread_id = Some(source_thread_id);
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

#[allow(clippy::too_many_arguments)]
fn validate_initial_frozen_task_routing_request(
    session: &Session,
    frozen_request: &FrozenProviderRequestMaterial,
    binding: &TaskPlanningHandoffBinding,
    route_capability: AutomaticRouteCapability,
    options: &AgentRunOptions,
    max_output_tokens: Option<u32>,
    transient_context: &[ModelMessage],
    runtime_context: RuntimeContextCandidates,
    writable_memory_routing: bool,
    task_continuation_available: bool,
) -> Result<()> {
    validate_initial_frozen_routing_request(
        session,
        frozen_request,
        &binding.source_turn,
        &binding.objective,
        route_surface_tool_specs_for_context(
            route_capability,
            writable_memory_routing,
            task_continuation_available,
        ),
        options,
        max_output_tokens,
        transient_context,
        runtime_context,
    )
}

fn validate_initial_frozen_plan_review_routing_request(
    session: &Session,
    frozen_request: &FrozenProviderRequestMaterial,
    binding: &PlanReviewHandoffBinding,
    route_capability: AutomaticRouteCapability,
    options: &AgentRunOptions,
    max_output_tokens: Option<u32>,
    transient_context: &[ModelMessage],
    runtime_context: RuntimeContextCandidates,
    writable_memory_routing: bool,
    task_continuation_available: bool,
) -> Result<()> {
    validate_initial_frozen_routing_request(
        session,
        frozen_request,
        &binding.source_turn,
        &binding.objective,
        route_surface_tool_specs_for_context(
            route_capability,
            writable_memory_routing,
            task_continuation_available,
        ),
        options,
        max_output_tokens,
        transient_context,
        runtime_context,
    )
}

fn validate_initial_frozen_routing_request(
    session: &Session,
    frozen_request: &FrozenProviderRequestMaterial,
    source_turn: &ConversationTurnRef,
    objective: &str,
    tool_specs: Vec<ToolSpec>,
    options: &AgentRunOptions,
    max_output_tokens: Option<u32>,
    transient_context: &[ModelMessage],
    runtime_context: RuntimeContextCandidates,
) -> Result<()> {
    validate_initial_frozen_request(session, frozen_request)?;
    let durable_user = session
        .entries()
        .iter()
        .find_map(|entry| match entry {
            SessionLogEntry::User(message) if message.id == source_turn.message_id => {
                Some(message.clone())
            }
            _ => None,
        })
        .ok_or_else(|| {
            anyhow!("automatic routing frozen request source is not durable in the session")
        })?;
    if durable_user.role != crate::MessageRole::User
        || durable_user.content.as_deref() != Some(objective)
    {
        return Err(anyhow!(
            "automatic routing frozen request source conflicts with its routing binding"
        ));
    }

    let mut normalized_request = frozen_request.request().clone();
    let matching_indices = normalized_request
        .messages
        .iter()
        .enumerate()
        .filter_map(|(index, message)| (message.id == source_turn.message_id).then_some(index))
        .collect::<Vec<_>>();
    let [message_index] = matching_indices.as_slice() else {
        return Err(anyhow!(
            "automatic routing frozen request must contain its exact source turn once"
        ));
    };
    let exact_user = &normalized_request.messages[*message_index];
    if exact_user.role != durable_user.role
        || exact_user.tool_calls.len() != durable_user.tool_calls.len()
        || exact_user
            .tool_calls
            .iter()
            .zip(&durable_user.tool_calls)
            .any(|(left, right)| {
                left.id != right.id || left.name != right.name || left.args_json != right.args_json
            })
        || exact_user.tool_call_id != durable_user.tool_call_id
        || exact_user.assistant_kind != durable_user.assistant_kind
        || exact_user.image_attachments != durable_user.image_attachments
    {
        return Err(anyhow!(
            "automatic routing frozen request source shape conflicts with the durable turn"
        ));
    }
    normalized_request.messages[*message_index] = durable_user;
    normalize_routing_system_message_id(&mut normalized_request)?;

    let mut expected_request = session.build_pre_turn_candidate_request(
        &options.workspace_root,
        &options.memory_config,
        tool_specs,
        max_output_tokens,
        options.reasoning_effort.clone(),
        session.latest_response_handle(frozen_request.request().provider_name.as_str()),
        options.traffic_partition_key.clone(),
        transient_context,
        runtime_context,
        &[],
    )?;
    normalize_routing_system_message_id(&mut expected_request)?;
    let normalized_material =
        FrozenProviderRequestMaterial::freeze(session.session_scope_id(), normalized_request)?;
    let expected_material =
        FrozenProviderRequestMaterial::freeze(session.session_scope_id(), expected_request)?;
    if normalized_material.canonical_bytes_for_in_process_use()
        != expected_material.canonical_bytes_for_in_process_use()
    {
        return Err(anyhow!(
            "caller-frozen initial provider request does not match automatic routing"
        ));
    }
    Ok(())
}

/// Frozen tool surface for one automatic route capability.
///
/// `ReviewFirst` never exposes the direct durable task decision; `DirectTask` exposes all three.
#[must_use]
pub fn route_surface_tool_specs(capability: AutomaticRouteCapability) -> Vec<ToolSpec> {
    match capability {
        AutomaticRouteCapability::Unsupported => Vec::new(),
        AutomaticRouteCapability::ReviewFirst => vec![
            request_plan_review_tool_spec(),
            continue_without_task_planning_tool_spec(),
        ],
        AutomaticRouteCapability::DirectTask => vec![
            request_plan_review_tool_spec(),
            request_task_planning_tool_spec(),
            continue_without_task_planning_tool_spec(),
        ],
    }
}

/// Frozen automatic-routing surface, optionally including durable-memory write tools.
///
/// Memory calls remain ordinary previewed/approved tool executions. They may accompany exactly
/// one typed route decision so an explicit persistence request is not lost when the same user
/// turn is handed to plan review or durable task planning.
#[must_use]
pub fn route_surface_tool_specs_with_memory(
    capability: AutomaticRouteCapability,
    writable_memory: bool,
) -> Vec<ToolSpec> {
    route_surface_tool_specs_for_context(capability, writable_memory, false)
}

/// Frozen automatic-routing surface for exact host-bound route context.
#[must_use]
pub fn route_surface_tool_specs_for_context(
    capability: AutomaticRouteCapability,
    writable_memory: bool,
    task_continuation_available: bool,
) -> Vec<ToolSpec> {
    let mut specs = route_surface_tool_specs(capability);
    if task_continuation_available && capability.routes_automatically() {
        specs.push(continue_existing_task_tool_spec());
    }
    if writable_memory && capability.routes_automatically() {
        specs.extend(writable_memory_route_tool_specs());
    }
    specs
}

fn validate_writable_memory_route_registry(tools: &ToolRegistry) -> Result<()> {
    for expected in writable_memory_route_tool_specs() {
        let actual = tools.spec_for(&expected.name).ok_or_else(|| {
            anyhow!(
                "writable memory routing requires registered tool {}",
                expected.name
            )
        })?;
        if serde_json::to_value(&actual)? != serde_json::to_value(&expected)? {
            return Err(anyhow!(
                "writable memory routing tool contract drifted for {}",
                expected.name
            ));
        }
    }
    Ok(())
}

fn normalize_routing_system_message_id(request: &mut crate::CompletionRequest) -> Result<()> {
    let matching_indices = request
        .messages
        .iter()
        .enumerate()
        .filter_map(|(index, message)| {
            (message.role == crate::MessageRole::System
                && message.content.as_deref()
                    == Some(conversation_route_routing_contract_material()))
            .then_some(index)
        })
        .collect::<Vec<_>>();
    let [message_index] = matching_indices.as_slice() else {
        return Err(anyhow!(
            "automatic routing request must contain its system contract once"
        ));
    };
    let message = &mut request.messages[*message_index];
    if !message.tool_calls.is_empty()
        || message.tool_call_id.is_some()
        || message.assistant_kind.is_some()
        || !message.image_attachments.is_empty()
    {
        return Err(anyhow!(
            "automatic routing system contract has an invalid message shape"
        ));
    }
    message.id = "system:conversation-route-contract-v1".to_owned();
    Ok(())
}

fn append_chat_route_decision<H>(
    session: &mut Session,
    handler: &mut H,
    source_turn: &ConversationTurnRef,
    capability: AutomaticRouteCapability,
    route_contract_fingerprint: &str,
    decided_at_ms: u64,
) -> Result<()>
where
    H: EventHandler + Send,
{
    use crate::conversation_route::{
        ConversationRoute, ConversationRouteDecisionProjection,
        ConversationRouteDecisionRecordedEntry, conversation_route_decision_id_for_source,
    };
    let projection = ConversationRouteDecisionProjection::from_entries(session.entries());
    if projection.has_conflicts() {
        bail!("conversation route decision projection contains conflicting durable facts");
    }
    let decision_id = conversation_route_decision_id_for_source(source_turn);
    if let Some(existing) = projection.decision_id_for_source(source_turn)
        && existing != &decision_id
    {
        bail!("source turn is already bound to a different route decision");
    }
    let entry = ConversationRouteDecisionRecordedEntry {
        decision_id,
        source_turn: source_turn.clone(),
        route: ConversationRoute::Chat,
        reason_codes: Vec::new(),
        configured_policy: TaskRoutingPolicy::Auto,
        effective_capability: capability,
        policy_snapshot_hash: crate::conversation_route::plan_review_policy_snapshot_hash(),
        route_contract_fingerprint: route_contract_fingerprint.to_owned(),
        decided_at_ms,
    };
    match projection.decision(&entry.decision_id) {
        None => {
            let control = ControlEntry::ConversationRouteDecisionRecorded(entry);
            session.append_control(control.clone())?;
            handler.handle(RunEvent::Control(control))?;
        }
        Some(previous) if previous == &entry => {}
        Some(_) => {
            bail!(
                "route decision {} has conflicting durable facts",
                entry.decision_id.as_str()
            );
        }
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

/// Consults the pending-input provider at a safe turn boundary and emits the follow-up
/// injection notice when one was promoted.
async fn promote_pending_follow_up<H>(
    provider: &dyn PendingConversationInputProvider,
    session: &mut Session,
    logical_run_id: &str,
    handler: &mut H,
) -> Result<bool>
where
    H: EventHandler + Send,
{
    if provider
        .promote_next_pending_input(session, logical_run_id)
        .await?
        .is_some()
    {
        handler.handle(RunEvent::Notice(
            "queued follow-up injected at a safe point".to_owned(),
        ))?;
        return Ok(true);
    }
    Ok(false)
}

/// Records the durable chat route decision for a routing microturn whose free text was delivered
/// as an ordinary conversation answer, keeping the source-turn decision projection consistent.
fn record_fallback_chat_route_decision<H>(
    session: &mut Session,
    handler: &mut H,
    purpose: Option<&AgentRunPurpose>,
) -> Result<()>
where
    H: EventHandler + Send,
{
    let Some(AgentRunPurpose::Conversation(context)) = purpose else {
        return Ok(());
    };
    let conversation = context.as_ref();
    let fingerprint = conversation
        .plan_review
        .as_ref()
        .map(|binding| binding.route_contract_fingerprint.clone())
        .or_else(|| {
            conversation
                .task_handoff
                .as_ref()
                .map(|binding| binding.route_contract_fingerprint.clone())
        })
        .or_else(|| {
            conversation
                .task_continuation
                .as_ref()
                .map(|binding| binding.route_contract_fingerprint.clone())
        })
        .ok_or_else(|| anyhow!("fallback chat decision requires a route contract fingerprint"))?;
    let decided_at_ms = conversation
        .plan_review
        .as_ref()
        .map(|binding| binding.decided_at_ms)
        .or_else(|| {
            conversation
                .task_handoff
                .as_ref()
                .map(|binding| binding.decided_at_ms)
        })
        .or_else(|| {
            conversation
                .task_continuation
                .as_ref()
                .map(|binding| binding.decided_at_ms)
        })
        .unwrap_or_default();
    append_chat_route_decision(
        session,
        handler,
        &conversation.source_turn,
        conversation.route_capability,
        &fingerprint,
        decided_at_ms,
    )
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
    pub disposition: AgentRunDisposition,
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
    FinalAnswerBlocked,
    TaskRoutingUnsatisfied,
    RoutingFreeTextFallback,
    AwaitingUserInput,
    TaskHandoff,
    PlanReviewHandoff,
}

impl AgentRunTerminalReason {
    /// Returns true when the kernel deliberately refused to accept a model final answer.
    #[must_use]
    pub fn blocks_successful_completion(self) -> bool {
        matches!(
            self,
            Self::DelegationUnsatisfied | Self::FinalAnswerBlocked | Self::TaskRoutingUnsatisfied
        )
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::FinalAnswer => "final_answer",
            Self::MaxTurns => "max_turns",
            Self::DelegationUnsatisfied => "delegation_unsatisfied",
            Self::FinalAnswerBlocked => "final_answer_blocked",
            Self::TaskRoutingUnsatisfied => "task_routing_unsatisfied",
            Self::RoutingFreeTextFallback => "routing_free_text_fallback",
            Self::AwaitingUserInput => "awaiting_user_input",
            Self::TaskHandoff => "task_handoff",
            Self::PlanReviewHandoff => "plan_review_handoff",
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

    /// Binds the current root logical-run identity before delegated child work is admitted.
    ///
    /// The value is a provider-neutral host identity. Delegates may use it to derive stable,
    /// replay-safe child orchestration identities, but must not expose it to the model as a
    /// provider request handle.
    fn set_root_logical_run_id(&mut self, _logical_run_id: Option<&str>) {}

    /// Binds the host-owned source and authority from which a concrete child grant may be minted.
    ///
    /// The context is derived from the root run purpose. It is not exposed through model tool
    /// arguments and does not itself authorize a child until the runtime mints an exact invocation
    /// grant.
    fn set_agent_delegation_run_context(
        &mut self,
        _context: Option<&crate::AgentDelegationRunContext>,
    ) {
    }

    /// Binds the exact tool call to the approval that admitted it.
    ///
    /// `explicit_user_approval` is true only when the interactive approval handler actually
    /// resolved an `Ask` decision for this exact call. A policy `Allow`, session grant, automated
    /// approval handler, or model-authored argument cannot manufacture this fact.
    fn set_agent_tool_authorization(
        &mut self,
        _call: Option<&ToolCall>,
        _explicit_user_approval: bool,
    ) {
    }

    /// Binds the root-owned Web budget so delegated children cannot create a fresh owner.
    fn set_web_task_tree_budget(&mut self, _budget: Option<Arc<crate::WebTaskTreeBudget>>) {}

    /// Binds the root-owned artifact-read budget so delegated children cannot create a fresh one.
    fn set_tool_artifact_read_budget(
        &mut self,
        _budget: Option<crate::session::ToolArtifactReadBudgetV1>,
    ) {
    }

    /// Supplies the current provider tool batch for host-owned child-join admission.
    ///
    /// The delegate must fail closed and admit joining only for exact protocol calls it owns and
    /// can prove join-safe. A coarse tool category is not sufficient evidence because custom tools
    /// may use the Agent category while still mutating the workspace.
    fn set_join_batch_eligibility(&mut self, _calls: &[ToolCall]) {}

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

    /// Waits for host-owned join-before-final child work admitted by the current tool batch.
    ///
    /// Implementations return one bounded, stable context payload after every joined child has
    /// reached a terminal state and its durable parent projection has been committed. The kernel
    /// injects that payload before the next provider turn, so the model never needs to poll a
    /// child with `wait_agent` merely to satisfy the join barrier.
    ///
    /// # Errors
    ///
    /// Returns an error when a joined child cannot be committed to the parent session.
    async fn settle_join_dependencies(
        &mut self,
        _session: &mut Session,
        _handler: &mut (dyn EventHandler + Send),
    ) -> Result<Option<FinalAnswerContext>> {
        Ok(None)
    }

    /// Aborts join dependencies admitted by the current batch before the host reached settle.
    ///
    /// Kernel invokes this on post-delegate persistence or event-delivery failure so an admitted
    /// child cannot retain cancellation-task or supervisor ownership after the parent turn exits.
    fn abort_join_dependencies(
        &mut self,
        _session: &mut Session,
        _handler: &mut (dyn EventHandler + Send),
        _reason: &str,
    ) -> Result<()> {
        Ok(())
    }

    /// Confirms that one settled join context was accepted into the next-turn transient context.
    ///
    /// Runtime implementations use this second phase to durably close continuation delivery
    /// without claiming that a full child transcript was copied into the parent session.
    ///
    /// # Errors
    ///
    /// Returns an error when the durable delivery confirmation cannot be recorded.
    fn confirm_join_context_delivery(
        &mut self,
        _session: &mut Session,
        _handler: &mut (dyn EventHandler + Send),
        _context_key: &str,
    ) -> Result<()> {
        Ok(())
    }

    /// Cancels bounded join contexts that were committed but never dispatched to a provider.
    fn cancel_join_context_delivery(
        &mut self,
        _session: &mut Session,
        _handler: &mut (dyn EventHandler + Send),
        _context_keys: &[String],
        _reason: &str,
    ) -> Result<()> {
        Ok(())
    }

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
    /// Runtime-owned provisional authorization that becomes chargeable only after provider
    /// dispatch is known to have happened. The kernel drives this lifecycle independently from
    /// evidence finalization because a stream can fail after dispatch but before evidence exists.
    pub dispatch_lifecycle: Arc<dyn AgentHostedTurnDispatchLifecycle>,
}

/// Provider-neutral lifecycle for one provisional hosted-provider authorization.
///
/// Implementations must be idempotent: physical connect retries reuse the same authorization,
/// and error finalization can race with a processor-owned terminal append. `mark_dispatched`
/// commits the hosted-request budget exactly once; `finish` appends the unique durable hosted
/// outcome without changing whether the request was charged.
pub trait AgentHostedTurnDispatchLifecycle: Send + Sync {
    /// Marks that the provider request was dispatched or that the provider returned a response.
    fn mark_dispatched(&self) -> Result<(), crate::HostedTurnError>;

    /// Finishes the authorization with one durable terminal outcome.
    fn finish(&self, status: crate::HostedToolTerminalStatus)
    -> Result<(), crate::HostedTurnError>;
}

/// Runtime hook invoked immediately before every provider request in a multi-turn run.
#[async_trait]
pub trait AgentHostedTurnPreparer: Send + Sync {
    /// Returns the authorized hosted turn, or `None` when the hosted capability is unavailable
    /// for this request (soft skip, e.g. the run's web budget is exhausted). Hard failures
    /// (denied egress, invalid configuration) still return an error and fail the run.
    async fn prepare_turn(&self) -> Result<Option<AgentHostedTurn>>;
}

/// Runtime hook consulted at safe turn boundaries while a conversation run is active.
///
/// The host durably promotes the next queued follow-up, appends its user message to the
/// session, and returns the prompt text. The kernel then continues the same run with that
/// message as the next user turn instead of interrupting or finalizing.
#[async_trait]
pub trait PendingConversationInputProvider: Send + Sync {
    /// Durably promotes the next queued follow-up for this logical run and returns its
    /// prompt text, or `None` when no follow-up is ready to inject.
    async fn promote_next_pending_input(
        &self,
        session: &mut Session,
        logical_run_id: &str,
    ) -> Result<Option<String>>;
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

    /// Consumes the agent and returns its provider and tool registry.
    ///
    /// Runtime composition layers may use this to add a provider-neutral admission wrapper while
    /// preserving the exact tool surface. Conversation generation must still re-enter through an
    /// [`Agent`] run method.
    #[must_use]
    pub fn into_parts(self) -> (P, ToolRegistry) {
        (self.provider, self.tools)
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
        // Resolve the provider's canonical default output cap at the run boundary when the run
        // carries no explicit constraint, so requests are deterministic for providers whose wire
        // protocol requires max_tokens or pins a canonical output budget.
        let input = if input.max_output_tokens.is_some() {
            input
        } else {
            match self
                .provider
                .default_max_output_tokens(session.model_name())
            {
                Some(default) => input.with_max_output_tokens(default),
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
            plan_review_draft,
            plan_review_submit_only,
            task_guidance_assessment,
            agent_delegation,
            purpose,
            agent_invocation_grant,
            logical_run_id,
            source_thread_id,
            user_input_root_logical_run_id,
            cancellation,
            cancellation_terminal_authority,
            source_capability_nonce,
            url_capability_issued_at_ms,
            user_url_capability_registrar,
            hosted_tools,
            hosted_evidence_processor,
            hosted_turn_preparer,
            pending_input_provider,
            mut initial_frozen_provider_request,
            max_output_tokens,
            suppressed_tool_names,
            web_task_tree_budget,
            tool_artifact_read_budget,
        } = input;
        // An explicit per-run registrar is useful for constrained callers and tests; production
        // sessions fall back to their non-serializable session-scoped runtime attachment so live
        // capabilities survive normal multi-turn ownership moves.
        let user_url_capability_registrar =
            user_url_capability_registrar.or_else(|| session.user_url_capability_registrar());

        let (task_handoff_binding, plan_review_binding, task_continuation_binding) =
            match purpose.as_ref() {
                Some(AgentRunPurpose::Conversation(context))
                    if context.routing_policy == TaskRoutingPolicy::Auto =>
                {
                    if !context.route_capability.routes_automatically() {
                        return Err(anyhow!(
                            "automatic routing cannot carry handoff bindings with capability {}",
                            context.route_capability.as_str()
                        ));
                    }
                    (
                        context.task_handoff.clone(),
                        context.plan_review.clone(),
                        context.task_continuation.clone(),
                    )
                }
                Some(AgentRunPurpose::Conversation(context)) => {
                    if context.task_handoff.is_some()
                        || context.plan_review.is_some()
                        || context.task_continuation.is_some()
                    {
                        return Err(anyhow!(
                            "manual task routing cannot carry an automatic handoff binding"
                        ));
                    }
                    (None, None, None)
                }
                Some(
                    AgentRunPurpose::PlanReview(_)
                    | AgentRunPurpose::TaskPlanner(_)
                    | AgentRunPurpose::TaskParticipant(_)
                    | AgentRunPurpose::TaskSynthesis(_),
                )
                | None => (None, None, None),
            };
        let routing_decision_pending = task_handoff_binding.is_some()
            || plan_review_binding.is_some()
            || task_continuation_binding.is_some();
        if let Some(context) = purpose.as_ref().and_then(|purpose| match purpose {
            AgentRunPurpose::Conversation(context) => Some(context.as_ref()),
            _ => None,
        }) {
            if task_handoff_binding.is_some() && !context.route_capability.allows_direct_task() {
                return Err(anyhow!(
                    "direct task handoff binding requires the DirectTask route capability"
                ));
            }
            if plan_review_binding.is_some() && !context.route_capability.routes_automatically() {
                return Err(anyhow!(
                    "plan review binding requires an automatic route capability"
                ));
            }
            if task_continuation_binding.is_some()
                && !context.route_capability.routes_automatically()
            {
                return Err(anyhow!(
                    "task continuation binding requires an automatic route capability"
                ));
            }
        }
        let agent_delegation_run_context = match purpose.as_ref() {
            Some(AgentRunPurpose::Conversation(context)) => {
                Some(crate::AgentDelegationRunContext {
                    source: crate::AgentInvocationGrantSource::Conversation {
                        source_turn: context.source_turn.clone(),
                    },
                    authority: crate::DelegationAuthority::ModelProactive,
                })
            }
            Some(AgentRunPurpose::TaskParticipant(context)) => {
                Some(crate::AgentDelegationRunContext {
                    source: crate::AgentInvocationGrantSource::AcceptedTaskPlan {
                        task_id: context.task_id.clone(),
                        plan_version: context.plan_version,
                        step_id: context.step_id.clone(),
                    },
                    authority: crate::DelegationAuthority::AcceptedTaskPlan {
                        task_id: context.task_id.clone(),
                        plan_version: context.plan_version,
                        step_id: context.step_id.clone(),
                    },
                })
            }
            Some(AgentRunPurpose::TaskPlanner(_) | AgentRunPurpose::TaskSynthesis(_)) | None => {
                None
            }
            Some(AgentRunPurpose::PlanReview(_)) => None,
        };
        let is_task_participant =
            matches!(purpose.as_ref(), Some(AgentRunPurpose::TaskParticipant(_)));
        if tools
            .spec_for(crate::REQUEST_USER_INPUT_TOOL_NAME)
            .is_some()
        {
            return Err(anyhow!(
                "tool registry collides with reserved internal tool {}",
                crate::REQUEST_USER_INPUT_TOOL_NAME
            ));
        }
        if routing_decision_pending {
            for reserved in [
                REQUEST_TASK_PLANNING_TOOL_NAME,
                REQUEST_PLAN_REVIEW_TOOL_NAME,
                CONTINUE_EXISTING_TASK_TOOL_NAME,
            ] {
                if tools.spec_for(reserved).is_some() {
                    return Err(anyhow!(
                        "tool registry collides with reserved internal tool {reserved}"
                    ));
                }
            }
            transient_context.insert(
                0,
                ModelMessage::system(conversation_route_routing_contract_material()),
            );
        }
        if let Some(draft_context) = plan_review_draft.as_ref() {
            if !matches!(purpose.as_ref(), Some(AgentRunPurpose::PlanReview(_))) {
                return Err(anyhow!(
                    "plan review draft context requires the PlanReview run purpose"
                ));
            }
            if tools.spec_for(SUBMIT_PLAN_DRAFT_TOOL_NAME).is_some() {
                return Err(anyhow!(
                    "tool registry collides with reserved internal tool {SUBMIT_PLAN_DRAFT_TOOL_NAME}"
                ));
            }
            if let Some(AgentRunPurpose::PlanReview(context)) = purpose.as_ref()
                && (context.plan_review_id != draft_context.plan_review_id
                    || context.attempt_id != draft_context.attempt_id
                    || context.plan_id != draft_context.plan_id)
            {
                return Err(anyhow!(
                    "plan review draft context does not match its run purpose binding"
                ));
            }
        }
        let route_capability = purpose
            .as_ref()
            .and_then(|purpose| match purpose {
                AgentRunPurpose::Conversation(context) => Some(context.route_capability),
                _ => None,
            })
            .unwrap_or(AutomaticRouteCapability::Unsupported);
        let writable_memory_routing = purpose
            .as_ref()
            .and_then(|purpose| match purpose {
                AgentRunPurpose::Conversation(context) => Some(context.writable_memory_routing),
                _ => None,
            })
            .unwrap_or(false);
        if writable_memory_routing && !route_capability.routes_automatically() {
            return Err(anyhow!(
                "writable memory routing requires automatic conversation routing"
            ));
        }
        if writable_memory_routing && !options.memory_config.writable {
            return Err(anyhow!(
                "writable memory routing conflicts with disabled writable memory configuration"
            ));
        }
        if writable_memory_routing {
            validate_writable_memory_route_registry(tools)?;
        }
        if let (Some(binding), Some(frozen_request)) = (
            task_handoff_binding.as_ref(),
            initial_frozen_provider_request.as_ref(),
        ) {
            validate_initial_frozen_task_routing_request(
                session,
                frozen_request,
                binding,
                route_capability,
                &options,
                max_output_tokens,
                &transient_context,
                runtime_context.clone(),
                writable_memory_routing,
                task_continuation_binding.is_some(),
            )?;
        }
        if let (Some(binding), Some(frozen_request)) = (
            plan_review_binding.as_ref(),
            initial_frozen_provider_request.as_ref(),
        ) {
            validate_initial_frozen_plan_review_routing_request(
                session,
                frozen_request,
                binding,
                route_capability,
                &options,
                max_output_tokens,
                &transient_context,
                runtime_context.clone(),
                writable_memory_routing,
                task_continuation_binding.is_some(),
            )?;
        }
        match purpose.as_ref() {
            Some(AgentRunPurpose::TaskPlanner(_)) => transient_context.insert(
                0,
                ModelMessage::system(task_planner_system_prompt_contract_material()),
            ),
            Some(AgentRunPurpose::TaskParticipant(_)) => transient_context.insert(
                0,
                ModelMessage::system(task_participant_system_prompt_contract_material()),
            ),
            Some(AgentRunPurpose::Conversation(_))
            | Some(AgentRunPurpose::PlanReview(_))
            | Some(AgentRunPurpose::TaskSynthesis(_))
            | None => {}
        }
        if task_guidance_assessment.is_some()
            && tools.spec_for(TASK_GUIDANCE_APPLY_TOOL_NAME).is_some()
        {
            return Err(anyhow!(
                "tool registry collides with reserved internal tool {TASK_GUIDANCE_APPLY_TOOL_NAME}"
            ));
        }

        if cancellation
            .as_ref()
            .is_some_and(RunCancellationHandle::is_cancel_requested)
        {
            return Err(anyhow!("run cancellation requested before agent start"));
        }

        session.reconcile_prepared_mutations(&options.workspace_root)?;
        session.reconcile_unfinished_write_tool_executions(&options.workspace_root)?;
        session.reconcile_egress_lifecycle()?;

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

        let permission_policy = PermissionPolicyChain::new_with_context_and_mode_override(
            &options.permission_config,
            &options.permission_context,
            options.permission_mode_override.as_ref(),
        );
        let logical_run_id =
            logical_run_id.unwrap_or_else(|| format!("agent-run-{}", uuid::Uuid::new_v4()));
        if logical_run_id.trim().is_empty() {
            return Err(anyhow!("agent logical run id is empty"));
        }
        let source_thread_id = match source_thread_id {
            Some(source_thread_id) => source_thread_id,
            None => crate::AgentThreadId::new("main")?,
        };
        let root_logical_run_id = user_input_root_logical_run_id
            .clone()
            .or_else(|| {
                purpose.as_ref().and_then(|purpose| match purpose {
                    AgentRunPurpose::Conversation(context) => Some(context.root_run_id.clone()),
                    _ => None,
                })
            })
            .unwrap_or_else(|| logical_run_id.clone());
        if root_logical_run_id.trim().is_empty() {
            return Err(anyhow!("user-input root logical run id is empty"));
        }
        if let Some(delegate) = agent_delegate.as_deref_mut() {
            delegate.set_root_logical_run_id(Some(&logical_run_id));
        }
        let has_initial_frozen_provider_request = initial_frozen_provider_request.is_some();
        let mut previous_response_handle = session.latest_response_handle(self.provider.name());
        let mut total_tool_calls = 0usize;
        let mut outcome = AgentRunOutcome::default();
        if agent_delegation.is_some() && !tool_registry_has_agent_tools(tools) {
            return Err(anyhow!(
                "agent delegation is required, but this run has no agent tools"
            ));
        }
        let agent_delegation_enforced = agent_delegation;
        let mut satisfied_agent_tool_calls = 0usize;
        let mut delegation_retry_used = false;
        let mut task_routing_decision_pending = routing_decision_pending;
        let mut task_routing_retry_used = false;
        let mut final_answer_context_key: Option<String> = None;
        let mut final_answer_context_message_index: Option<usize> = None;
        let mut final_answer_blocker_prompt: Option<String> = None;
        let mut final_answer_blocker_message_index: Option<usize> = None;
        let mut final_answer_blocker_retries = 0usize;
        let mut pending_join_context_keys: Vec<String> = Vec::new();
        let mut participant_post_mutation_read_calls = 0usize;
        let mut participant_finalization_pending = false;
        let mut participant_finalization_prompt_injected = false;
        let mut participant_finalization_dispatched = false;
        let tool_artifact_read_budget = tool_artifact_read_budget.unwrap_or_default();

        let mut model_turns = 0usize;
        let mut hosted_unavailable_noticed = false;
        loop {
            // RFC-0059 §10.3: per-model-turn window; no-op for delegated children.
            tool_artifact_read_budget.reset_turn();
            if cancellation
                .as_ref()
                .is_some_and(RunCancellationHandle::is_cancel_requested)
            {
                if !pending_join_context_keys.is_empty()
                    && let Some(delegate) = agent_delegate.as_deref_mut()
                {
                    delegate.cancel_join_context_delivery(
                        session,
                        handler,
                        &pending_join_context_keys,
                        "root run cancelled before joined result context dispatch",
                    )?;
                    pending_join_context_keys.clear();
                }
                return Err(anyhow!("run cancellation requested before next model turn"));
            }
            if is_task_participant
                && !participant_finalization_dispatched
                && !outcome.changed_files.is_empty()
                && options.max_turns.is_some_and(|max_turns| {
                    max_turns > 0 && model_turns.saturating_add(1) >= max_turns
                })
            {
                participant_finalization_pending = true;
            }
            if participant_finalization_pending && !participant_finalization_prompt_injected {
                transient_context.push(ModelMessage::system(
                    task_participant_finalization_prompt_contract_material(),
                ));
                participant_finalization_prompt_injected = true;
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
                    disposition: AgentRunDisposition::Interrupted,
                });
            }
            if initial_frozen_provider_request.is_none()
                && let Some(delegate) = agent_delegate.as_deref_mut()
            {
                match delegate.final_answer_context(session, &options, &outcome)? {
                    Some(context)
                        if final_answer_context_key.as_deref() != Some(context.key.as_str()) =>
                    {
                        final_answer_context_key = Some(context.key);
                        let message = ModelMessage::user(context.prompt);
                        if let Some(index) = final_answer_context_message_index {
                            transient_context[index] = message;
                        } else {
                            final_answer_context_message_index = Some(transient_context.len());
                            transient_context.push(message);
                        }
                    }
                    None => {
                        if let Some(index) = final_answer_context_message_index.take() {
                            transient_context.remove(index);
                            if let Some(blocker_index) = final_answer_blocker_message_index.as_mut()
                                && *blocker_index > index
                            {
                                *blocker_index = blocker_index.saturating_sub(1);
                            }
                        }
                        final_answer_context_key = None;
                    }
                    Some(_) => {}
                }
            }
            model_turns = model_turns.saturating_add(1);

            // Safe-point follow-up injection: after the first provider turn, a queued
            // follow-up is promoted into the session and answered by the same run, without
            // interrupting it. Routing microturns and non-conversation runs are exempt.
            if model_turns >= 2
                && !task_routing_decision_pending
                && matches!(purpose.as_ref(), Some(AgentRunPurpose::Conversation(_)))
                && let Some(provider) = pending_input_provider.as_ref()
                && promote_pending_follow_up(
                    provider.as_ref(),
                    &mut *session,
                    &logical_run_id,
                    handler,
                )
                .await?
            {
                continue;
            }

            let participant_finalization_turn =
                participant_finalization_pending && !participant_finalization_dispatched;
            let mut tool_specs = if participant_finalization_turn {
                Vec::new()
            } else if task_routing_decision_pending {
                route_surface_tool_specs_for_context(
                    route_capability,
                    writable_memory_routing,
                    task_continuation_binding.is_some(),
                )
            } else {
                tools
                    .specs()
                    .into_iter()
                    .filter(|spec| !suppressed_tool_names.contains(&spec.name))
                    .collect::<Vec<_>>()
            };
            if !task_routing_decision_pending && !participant_finalization_turn {
                if matches!(
                    purpose.as_ref(),
                    Some(
                        AgentRunPurpose::Conversation(_)
                            | AgentRunPurpose::PlanReview(_)
                            | AgentRunPurpose::TaskPlanner(_)
                    )
                ) && !plan_review_submit_only
                    || user_input_root_logical_run_id.is_some()
                {
                    tool_specs.push(crate::request_user_input_tool_spec());
                }
                if let Some(context) = task_plan_update.as_ref() {
                    tool_specs.push(task_plan_update_tool_spec_for_worktree(
                        context.worktree_availability,
                    ));
                }
                if plan_review_draft.is_some() {
                    tool_specs.push(submit_plan_draft_tool_spec());
                }
                if task_guidance_assessment.is_some() {
                    tool_specs.push(task_guidance_apply_tool_spec());
                }
            }
            if participant_finalization_turn {
                participant_finalization_pending = false;
                participant_finalization_dispatched = true;
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
            let (request, current_hosted_processor, current_hosted_dispatch_lifecycle) =
                match initial_frozen_request.as_ref() {
                    Some(frozen_request) => {
                        validate_initial_frozen_request(session, frozen_request)?;
                        (
                            frozen_request.request().clone(),
                            hosted_evidence_processor.clone(),
                            None,
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
                        let prepared_hosted_turn = match (
                            participant_finalization_turn,
                            hosted_turn_preparer.as_ref(),
                        ) {
                            (true, _) | (false, None) => None,
                            (false, Some(preparer)) => match preparer.prepare_turn().await? {
                                Some(turn) => Some(turn),
                                None => {
                                    // The run-wide hosted budget stays exhausted for the rest of the
                                    // run; surface the soft skip once instead of every provider turn.
                                    if !hosted_unavailable_noticed {
                                        hosted_unavailable_noticed = true;
                                        handler.handle(RunEvent::Notice(
                                        "hosted web search is unavailable for this request (web budget exhausted)".to_owned(),
                                    ))?;
                                    }
                                    None
                                }
                            },
                        };
                        let current_hosted_tools = if participant_finalization_turn {
                            &[][..]
                        } else {
                            prepared_hosted_turn
                                .as_ref()
                                .map_or(hosted_tools.as_slice(), |turn| {
                                    turn.hosted_tools.as_slice()
                                })
                        };
                        request.hosted_tools = current_hosted_tools.to_vec();
                        let current_hosted_processor = prepared_hosted_turn
                            .as_ref()
                            .map(|turn| Arc::clone(&turn.evidence_processor))
                            .or_else(|| hosted_evidence_processor.clone());
                        let current_hosted_dispatch_lifecycle = prepared_hosted_turn
                            .as_ref()
                            .map(|turn| Arc::clone(&turn.dispatch_lifecycle));
                        (
                            request,
                            current_hosted_processor,
                            current_hosted_dispatch_lifecycle,
                        )
                    }
                };

            let provider_effect =
                match begin_run_effect(cancellation.as_ref(), RunEffectKind::ProviderRequest) {
                    Ok(effect) => effect,
                    Err(error) => {
                        if let Some(lifecycle) = current_hosted_dispatch_lifecycle.as_ref() {
                            let status = if cancellation
                                .as_ref()
                                .is_some_and(RunCancellationHandle::is_cancel_requested)
                            {
                                crate::HostedToolTerminalStatus::Cancelled
                            } else {
                                crate::HostedToolTerminalStatus::RequestFailed
                            };
                            lifecycle.finish(status)?;
                        }
                        if cancellation
                            .as_ref()
                            .is_some_and(RunCancellationHandle::is_cancel_requested)
                            && !pending_join_context_keys.is_empty()
                            && let Some(delegate) = agent_delegate.as_deref_mut()
                        {
                            delegate.cancel_join_context_delivery(
                                session,
                                handler,
                                &pending_join_context_keys,
                                "root run cancelled before joined result provider dispatch",
                            )?;
                            pending_join_context_keys.clear();
                        }
                        return Err(error);
                    }
                };
            let provider_turn_result = {
                let mut provider_event_handler =
                    RoutingMicroturnEventFilter::new(handler, task_routing_decision_pending);
                match initial_frozen_request {
                    Some(frozen_request) => {
                        provider_stream::collect_frozen_provider_turn(
                            &self.provider,
                            session,
                            frozen_request,
                            &provider_logical_run_id,
                            &mut previous_response_handle,
                            total_tool_calls,
                            &mut provider_event_handler,
                            cancellation.as_ref(),
                            current_hosted_processor.as_ref(),
                            current_hosted_dispatch_lifecycle.as_ref(),
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
                            &mut provider_event_handler,
                            cancellation.as_ref(),
                            current_hosted_processor.as_ref(),
                            current_hosted_dispatch_lifecycle.as_ref(),
                        )
                        .await
                    }
                }
            };
            drop(provider_effect);
            let provider_turn = match provider_turn_result {
                Ok(provider_turn) => provider_turn,
                Err(error) => {
                    if cancellation
                        .as_ref()
                        .is_some_and(RunCancellationHandle::is_cancel_requested)
                        && !pending_join_context_keys.is_empty()
                        && let Some(delegate) = agent_delegate.as_deref_mut()
                    {
                        delegate.cancel_join_context_delivery(
                            session,
                            handler,
                            &pending_join_context_keys,
                            "root run cancelled during joined result provider dispatch",
                        )?;
                        pending_join_context_keys.clear();
                    }
                    append_failed_run_lifecycle_events(
                        session,
                        "provider_stream_error",
                        total_tool_calls,
                        "provider turn failed before a safe terminal result",
                    )?;
                    return Err(error);
                }
            };
            if !pending_join_context_keys.is_empty()
                && let Some(delegate) = agent_delegate.as_deref_mut()
            {
                for context_key in std::mem::take(&mut pending_join_context_keys) {
                    delegate.confirm_join_context_delivery(session, handler, &context_key)?;
                }
            }
            let assistant_text = if task_routing_decision_pending {
                String::new()
            } else {
                provider_turn.assistant_text
            };
            let completed_calls = provider_turn
                .completed_calls
                .into_iter()
                .map(crate::ToolCallPersistenceProjection::into_exact_call)
                .collect::<Vec<_>>();
            let pending_states = provider_turn.pending_states;
            let hosted_finalized = provider_turn.hosted_finalized;

            if !task_routing_decision_pending {
                append_reasoning_trace(session, &provider_turn.reasoning_trace)?;
            }

            if !completed_calls.is_empty() {
                let changed_files_before_batch = outcome.changed_files.len();
                let tool_call_ids_before_batch = outcome.tool_call_ids.len();
                let declaration_order = completed_calls
                    .iter()
                    .enumerate()
                    .map(|(ordinal, call)| (call.id.clone(), ordinal))
                    .collect::<BTreeMap<_, _>>();
                let participant_read_calls_in_batch = if is_task_participant {
                    completed_calls
                        .iter()
                        .filter(|call| {
                            tools
                                .spec_for(&call.name)
                                .is_some_and(|spec| spec.access == ToolAccess::Read)
                        })
                        .count()
                } else {
                    0
                };
                total_tool_calls += completed_calls.len();
                let tool_preamble_overlay = append_tool_preamble_message(
                    session,
                    &mut RoutingMicroturnEventFilter::new(handler, false),
                    tools,
                    &assistant_text,
                    &completed_calls,
                    pending_states,
                )?;
                current_run_overlays.push(tool_preamble_overlay);

                let mut tool_ctx =
                    ToolContext::new(options.workspace_root.clone(), options.tool_timeout_secs)
                        .with_session_scope_id(session.session_scope_id().to_owned())
                        .with_network_authorization(
                            options.permission_context.network_policy,
                            false,
                        );
                tool_ctx = tool_ctx.with_logical_run_id(logical_run_id.clone());
                if let Some(grant) = agent_invocation_grant.as_ref() {
                    tool_ctx = tool_ctx.with_agent_invocation_grant(grant.clone());
                }
                if let Some(cancellation) = cancellation.as_ref() {
                    tool_ctx = tool_ctx.with_cancellation(cancellation.clone());
                }
                if let Some(recorder) = session.mutation_event_recorder() {
                    tool_ctx = tool_ctx.with_mutation_recorder(recorder);
                }
                if let Some(store) = session.tool_artifact_store() {
                    let active = session.active_projection_snapshot()?.ok_or_else(|| {
                        anyhow!("active artifact source projection is unavailable")
                    })?;
                    let pressure = active.tool_output_pressure();
                    let source_bindings = pressure.artifact_source_bindings()?;
                    tool_ctx = tool_ctx
                        .with_tool_artifact_reader(
                            store,
                            tool_artifact_read_budget.clone(),
                            pressure.active_epoch_id,
                        )
                        .with_tool_artifact_source_bindings(source_bindings);
                }
                if let Ok(recorder) = session.egress_audit_recorder() {
                    tool_ctx = tool_ctx.with_egress_audit_recorder(recorder);
                }
                if let Some(registrar) = user_url_capability_registrar.as_ref() {
                    tool_ctx = tool_ctx.with_user_url_capability_registrar(Arc::clone(registrar));
                }
                if let Some(budget) = web_task_tree_budget.as_ref() {
                    tool_ctx = tool_ctx.with_web_task_tree_budget(Arc::clone(budget));
                }
                let accepted_task_plan_in_batch = completed_calls.iter().any(|call| {
                    task_plan_update
                        .as_ref()
                        .is_some_and(|context| task_plan_update_call_is_accepted(context, call))
                });
                let accepted_plan_draft_in_batch = !accepted_task_plan_in_batch
                    && plan_review_draft.is_some()
                    && completed_calls.iter().any(|call| {
                        plan_review_draft.as_ref().is_some_and(|context| {
                            submit_plan_draft_call_is_accepted(context, call)
                        })
                    });
                let accepted_task_continuation_in_batch = task_routing_decision_pending
                    && task_continuation_binding.is_some()
                    && completed_calls
                        .iter()
                        .any(continue_existing_task_call_is_accepted);
                let accepted_task_handoff_in_batch = task_routing_decision_pending
                    && !accepted_task_continuation_in_batch
                    && task_handoff_binding.is_some()
                    && completed_calls
                        .iter()
                        .any(task_planning_request_call_is_accepted);
                let accepted_plan_review_in_batch = task_routing_decision_pending
                    && !accepted_task_handoff_in_batch
                    && plan_review_binding.is_some()
                    && completed_calls.iter().any(|call| {
                        plan_review_binding
                            .as_ref()
                            .is_some_and(|binding| plan_review_call_is_accepted(binding, call))
                    });
                let accepted_direct_conversation_in_batch = task_routing_decision_pending
                    && !accepted_task_handoff_in_batch
                    && !accepted_plan_review_in_batch
                    && completed_calls
                        .iter()
                        .any(continue_without_task_planning_call_is_accepted);
                let accepted_task_guidance_in_batch = !accepted_task_plan_in_batch
                    && task_guidance_assessment.is_some()
                    && completed_calls.iter().any(|call| {
                        task_guidance_assessment.as_ref().is_some_and(|context| {
                            task_guidance_apply_call_is_accepted(context, call)
                        })
                    });
                let accepted_user_input_in_batch = !task_routing_decision_pending
                    && !accepted_task_plan_in_batch
                    && !accepted_plan_draft_in_batch
                    && !accepted_task_guidance_in_batch
                    && completed_calls
                        .iter()
                        .any(request_user_input_call_is_accepted);
                if let Some(delegate) = agent_delegate.as_deref_mut() {
                    delegate.set_join_batch_eligibility(&completed_calls);
                }
                let mut accepted_task_plan = false;
                let mut accepted_plan_draft = false;
                let mut accepted_task_handoff = None;
                let mut accepted_task_continuation = None;
                let mut accepted_plan_review = None;
                let mut accepted_direct_conversation = false;
                let mut accepted_task_guidance = false;
                let mut accepted_user_input = None;
                let mut assistant_batch_results: Vec<(crate::ToolCall, ToolResult)> = Vec::new();
                let mut execution_calls = completed_calls;
                if task_routing_decision_pending && writable_memory_routing {
                    // A route decision hands this turn to another runtime immediately after the
                    // batch settles. Execute approved memory writes first even when the provider
                    // emitted the route call first, so a crash during handoff cannot durably
                    // record the route while silently losing the user's explicit memory intent.
                    execution_calls.sort_by_key(|call| !is_writable_memory_route_tool(&call.name));
                }
                for call in execution_calls {
                    let safe_call =
                        crate::project_tool_call_for_persistence(call.clone())?.durable_call;
                    if plan_review_submit_only && call.name != SUBMIT_PLAN_DRAFT_TOOL_NAME {
                        let mut result = ToolResult::error(
                            call.id.clone(),
                            call.name.clone(),
                            ToolErrorKind::Protocol,
                            "submit_only_protocol_violation: plan finalization accepts only submit_plan_draft",
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
                        assistant_batch_results.push((call.clone(), result));
                        continue;
                    }
                    if accepted_user_input_in_batch
                        && (call.name != crate::REQUEST_USER_INPUT_TOOL_NAME
                            || accepted_user_input.is_some())
                    {
                        append_tool_ignored_after_user_input_request(
                            session,
                            &mut outcome,
                            &call,
                            &mut assistant_batch_results,
                        )?;
                        continue;
                    }
                    if accepted_task_continuation_in_batch
                        && !(writable_memory_routing && is_writable_memory_route_tool(&call.name))
                        && (call.name != CONTINUE_EXISTING_TASK_TOOL_NAME
                            || accepted_task_continuation.is_some())
                    {
                        append_tool_ignored_after_task_handoff(
                            session,
                            &mut outcome,
                            &call,
                            &mut assistant_batch_results,
                        )?;
                        continue;
                    }
                    if accepted_task_handoff_in_batch
                        && !(writable_memory_routing && is_writable_memory_route_tool(&call.name))
                        && (call.name != REQUEST_TASK_PLANNING_TOOL_NAME
                            || accepted_task_handoff.is_some())
                    {
                        append_tool_ignored_after_task_handoff(
                            session,
                            &mut outcome,
                            &call,
                            &mut assistant_batch_results,
                        )?;
                        continue;
                    }
                    if accepted_plan_review_in_batch
                        && !(writable_memory_routing && is_writable_memory_route_tool(&call.name))
                        && (call.name != REQUEST_PLAN_REVIEW_TOOL_NAME
                            || accepted_plan_review.is_some())
                    {
                        append_tool_ignored_after_plan_review_decision(
                            session,
                            &mut outcome,
                            &call,
                            &mut assistant_batch_results,
                        )?;
                        continue;
                    }
                    if accepted_direct_conversation_in_batch
                        && !(writable_memory_routing && is_writable_memory_route_tool(&call.name))
                        && (call.name != CONTINUE_WITHOUT_TASK_PLANNING_TOOL_NAME
                            || accepted_direct_conversation)
                    {
                        append_tool_ignored_after_routing_decision(
                            session,
                            &mut outcome,
                            &call,
                            &mut assistant_batch_results,
                        )?;
                        continue;
                    }
                    if accepted_task_plan_in_batch && call.name != TASK_PLAN_UPDATE_TOOL_NAME {
                        append_tool_ignored_after_task_plan_acceptance(
                            session,
                            &mut outcome,
                            &call,
                            &mut assistant_batch_results,
                        )?;
                        continue;
                    }
                    if accepted_plan_draft_in_batch && call.name != SUBMIT_PLAN_DRAFT_TOOL_NAME {
                        append_tool_ignored_after_plan_draft(
                            session,
                            &mut outcome,
                            &call,
                            &mut assistant_batch_results,
                        )?;
                        continue;
                    }
                    if accepted_task_guidance_in_batch
                        && (call.name != TASK_GUIDANCE_APPLY_TOOL_NAME || accepted_task_guidance)
                    {
                        append_tool_ignored_after_task_guidance_acceptance(
                            session,
                            &mut outcome,
                            &call,
                            &mut assistant_batch_results,
                        )?;
                        continue;
                    }
                    if call.name == CONTINUE_EXISTING_TASK_TOOL_NAME {
                        if !task_routing_decision_pending {
                            let mut result = ToolResult::error(
                                call.id.clone(),
                                call.name.clone(),
                                ToolErrorKind::Unsupported,
                                "continue_existing_task is not available after the routing microturn",
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
                            assistant_batch_results.push((call.clone(), result));
                            continue;
                        }
                        let Some(binding) = task_continuation_binding.as_ref() else {
                            let mut result = ToolResult::error(
                                call.id.clone(),
                                call.name.clone(),
                                ToolErrorKind::Unsupported,
                                "continue_existing_task is not available for this run",
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
                            assistant_batch_results.push((call.clone(), result));
                            continue;
                        };
                        accepted_task_continuation = handle_continue_existing_task_call(
                            session,
                            &mut RoutingMicroturnEventFilter::new(handler, false),
                            &mut outcome,
                            &call,
                            binding,
                            cancellation.as_ref().map(RunCancellationHandle::scope_id),
                            &mut assistant_batch_results,
                        )?;
                        continue;
                    }
                    if call.name == CONTINUE_WITHOUT_TASK_PLANNING_TOOL_NAME {
                        if !task_routing_decision_pending {
                            let mut result = ToolResult::error(
                                call.id.clone(),
                                call.name.clone(),
                                ToolErrorKind::Unsupported,
                                "continue_without_task_planning is not available after the routing microturn",
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
                            assistant_batch_results.push((call.clone(), result));
                            continue;
                        }
                        let accepted = handle_continue_without_task_planning_call(
                            session,
                            &mut outcome,
                            &call,
                            &mut assistant_batch_results,
                        )?;
                        accepted_direct_conversation = accepted_direct_conversation || accepted;
                        continue;
                    }
                    if call.name == REQUEST_TASK_PLANNING_TOOL_NAME {
                        if !task_routing_decision_pending {
                            let mut result = ToolResult::error(
                                call.id.clone(),
                                call.name.clone(),
                                ToolErrorKind::Unsupported,
                                "request_task_planning is not available after the routing microturn",
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
                            assistant_batch_results.push((call.clone(), result));
                            continue;
                        }
                        let Some(binding) = task_handoff_binding.as_ref() else {
                            let mut result = ToolResult::error(
                                call.id.clone(),
                                call.name.clone(),
                                ToolErrorKind::Unsupported,
                                "request_task_planning is not available for this run",
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
                            assistant_batch_results.push((call.clone(), result));
                            continue;
                        };
                        accepted_task_handoff = handle_task_planning_request_call(
                            session,
                            &mut RoutingMicroturnEventFilter::new(handler, false),
                            &mut outcome,
                            &call,
                            binding,
                            cancellation
                                .as_ref()
                                .ok_or_else(|| {
                                    anyhow!("task handoff requires a root cancellation scope")
                                })?
                                .scope_id(),
                            &mut assistant_batch_results,
                        )?;
                        continue;
                    }
                    if call.name == REQUEST_PLAN_REVIEW_TOOL_NAME {
                        if !task_routing_decision_pending {
                            let mut result = ToolResult::error(
                                call.id.clone(),
                                call.name.clone(),
                                ToolErrorKind::Unsupported,
                                "request_plan_review is not available after the routing microturn",
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
                            assistant_batch_results.push((call.clone(), result));
                            continue;
                        }
                        let Some(binding) = plan_review_binding.as_ref() else {
                            let mut result = ToolResult::error(
                                call.id.clone(),
                                call.name.clone(),
                                ToolErrorKind::Unsupported,
                                "request_plan_review is not available for this run",
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
                            assistant_batch_results.push((call.clone(), result));
                            continue;
                        };
                        accepted_plan_review = handle_request_plan_review_call(
                            session,
                            &mut RoutingMicroturnEventFilter::new(handler, false),
                            &mut outcome,
                            &call,
                            binding,
                            cancellation
                                .as_ref()
                                .ok_or_else(|| {
                                    anyhow!(
                                        "plan review handoff requires a root cancellation scope"
                                    )
                                })?
                                .scope_id(),
                            &mut assistant_batch_results,
                        )?;
                        continue;
                    }
                    if task_routing_decision_pending
                        && !(writable_memory_routing && is_writable_memory_route_tool(&call.name))
                    {
                        append_tool_rejected_during_task_routing(
                            session,
                            &mut outcome,
                            &call,
                            &mut assistant_batch_results,
                        )?;
                        continue;
                    }
                    if call.name == crate::REQUEST_USER_INPUT_TOOL_NAME {
                        let model_name = session.model_name().to_owned();
                        match handle_request_user_input_call(
                            session,
                            handler,
                            &call,
                            RequestUserInputContext {
                                root_logical_run_id: &root_logical_run_id,
                                source_thread_id: &source_thread_id,
                                provider_name: self.provider.name(),
                                model_name: &model_name,
                                source: match purpose.as_ref() {
                                    Some(AgentRunPurpose::PlanReview(context)) => {
                                        crate::UserInputSourceV1::PlanReviewResearch {
                                            plan_review_id: context.plan_review_id.clone(),
                                            attempt_id: context.attempt_id.clone(),
                                        }
                                    }
                                    Some(AgentRunPurpose::TaskPlanner(context)) => {
                                        crate::UserInputSourceV1::Planner {
                                            task_id: context.task_id.clone(),
                                        }
                                    }
                                    _ => crate::UserInputSourceV1::Agent,
                                },
                            },
                        ) {
                            Ok(request) => accepted_user_input = Some(request),
                            Err(error) => append_request_user_input_error(
                                session,
                                &mut outcome,
                                &call,
                                &error,
                                &mut assistant_batch_results,
                            )?,
                        }
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
                            assistant_batch_results.push((call.clone(), result));
                            continue;
                        };
                        let accepted = handle_task_plan_update_call(
                            session,
                            handler,
                            &mut outcome,
                            &call,
                            context,
                            &mut assistant_batch_results,
                        )?;
                        accepted_task_plan = accepted_task_plan || accepted;
                        continue;
                    }
                    if call.name == SUBMIT_PLAN_DRAFT_TOOL_NAME {
                        let Some(context) = plan_review_draft.as_ref() else {
                            let mut result = ToolResult::error(
                                call.id.clone(),
                                call.name.clone(),
                                ToolErrorKind::Unsupported,
                                "submit_plan_draft is not available for this run",
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
                            assistant_batch_results.push((call.clone(), result));
                            continue;
                        };
                        let now_ms = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64;
                        let accepted = handle_submit_plan_draft_call(
                            session,
                            handler,
                            &mut outcome,
                            &call,
                            context,
                            now_ms,
                            &mut assistant_batch_results,
                        )?;
                        accepted_plan_draft = accepted_plan_draft || accepted;
                        continue;
                    }
                    if call.name == TASK_GUIDANCE_APPLY_TOOL_NAME {
                        let Some(context) = task_guidance_assessment.as_ref() else {
                            let mut result = ToolResult::error(
                                call.id.clone(),
                                call.name.clone(),
                                ToolErrorKind::Unsupported,
                                "task_guidance_apply is not available for this run",
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
                            assistant_batch_results.push((call.clone(), result));
                            continue;
                        };
                        let accepted = handle_task_guidance_apply_call(
                            session,
                            handler,
                            &mut outcome,
                            &call,
                            context,
                            &mut assistant_batch_results,
                        )?;
                        accepted_task_guidance = accepted_task_guidance || accepted;
                        continue;
                    }
                    let tool_call_context = ToolCallProcessingContext {
                        session,
                        handler,
                        tools,
                        options: &options,
                        permission_policy: &permission_policy,
                        tool_ctx: tool_ctx.clone(),
                        cancellation: cancellation.clone(),
                        root_logical_run_id: &logical_run_id,
                        agent_delegation_run_context: agent_delegation_run_context.as_ref(),
                        agent_delegate: &mut agent_delegate,
                        approval_handler,
                        outcome: &mut outcome,
                        satisfied_agent_tool_calls: &mut satisfied_agent_tool_calls,
                        transient_context: &mut transient_context,
                        web_task_tree_budget: web_task_tree_budget.clone(),
                        tool_artifact_read_budget: tool_artifact_read_budget.clone(),
                        assistant_batch_results: &mut assistant_batch_results,
                    };
                    if let Err(error) = process_tool_call(tool_call_context, call, safe_call).await
                    {
                        if let Some(delegate) = agent_delegate.as_deref_mut()
                            && let Err(cleanup_error) = delegate.abort_join_dependencies(
                                session,
                                handler,
                                "parent tool result failed before host join settle",
                            )
                        {
                            return Err(error.context(format!(
                                "host join cleanup also failed: {cleanup_error:#}"
                            )));
                        }
                        return Err(error);
                    }
                }
                if is_task_participant {
                    if outcome.changed_files.len() > changed_files_before_batch {
                        participant_post_mutation_read_calls = 0;
                    } else if !outcome.changed_files.is_empty() {
                        participant_post_mutation_read_calls = participant_post_mutation_read_calls
                            .saturating_add(participant_read_calls_in_batch);
                        if participant_post_mutation_read_calls
                            >= TASK_PARTICIPANT_POST_MUTATION_READ_TAIL_LIMIT
                            && !participant_finalization_dispatched
                        {
                            participant_finalization_pending = true;
                        }
                    }
                }
                assistant_batch_results.sort_by_key(|(call, _)| {
                    declaration_order
                        .get(&call.id)
                        .copied()
                        .unwrap_or(usize::MAX)
                });
                outcome.tool_call_ids[tool_call_ids_before_batch..].sort_by_key(|call_id| {
                    declaration_order
                        .get(call_id)
                        .copied()
                        .unwrap_or(usize::MAX)
                });
                // RFC-0062 11.2/11.5: settle the whole assistant tool-call batch with the
                // deterministic two-phase preview allocator before the next provider request.
                // A settlement failure keeps the same cleanup contract as a per-tool emit
                // failure: join dependencies are aborted before the error propagates.
                if let Err(error) = emit_tool_result_batch(
                    session,
                    &mut RoutingMicroturnEventFilter::new(handler, false),
                    &mut outcome,
                    std::mem::take(&mut assistant_batch_results),
                ) {
                    if let Some(delegate) = agent_delegate.as_deref_mut()
                        && let Err(cleanup_error) = delegate.abort_join_dependencies(
                            session,
                            handler,
                            "tool result settlement failed before host join settle",
                        )
                    {
                        return Err(error
                            .context(format!("host join cleanup also failed: {cleanup_error:#}")));
                    }
                    return Err(error);
                }
                if let Some(request) = accepted_user_input {
                    outcome.terminal_reason = AgentRunTerminalReason::AwaitingUserInput;
                    outcome.tool_calls = total_tool_calls;
                    return Ok(AgentRunOutput {
                        result: AgentRunResult {
                            final_text: String::new(),
                            tool_calls: total_tool_calls,
                            final_message_id: None,
                        },
                        outcome,
                        disposition: AgentRunDisposition::AwaitingUserInput(request),
                    });
                }
                let settled_join_context = match agent_delegate.as_deref_mut() {
                    Some(delegate) => delegate.settle_join_dependencies(session, handler).await?,
                    None => None,
                };
                if let Some(context) = settled_join_context {
                    let context_key = context.key.clone();
                    transient_context.push(ModelMessage::user(context.prompt));
                    pending_join_context_keys.push(context_key);
                }
                if let Some(action) = accepted_task_handoff {
                    outcome.terminal_reason = AgentRunTerminalReason::TaskHandoff;
                    outcome.tool_calls = total_tool_calls;
                    return Ok(AgentRunOutput {
                        result: AgentRunResult {
                            final_text: String::new(),
                            tool_calls: total_tool_calls,
                            final_message_id: None,
                        },
                        outcome,
                        disposition: AgentRunDisposition::StartDurableTask(action),
                    });
                }
                if let Some(action) = accepted_task_continuation {
                    outcome.terminal_reason = AgentRunTerminalReason::TaskHandoff;
                    outcome.tool_calls = total_tool_calls;
                    return Ok(AgentRunOutput {
                        result: AgentRunResult {
                            final_text: String::new(),
                            tool_calls: total_tool_calls,
                            final_message_id: None,
                        },
                        outcome,
                        disposition: AgentRunDisposition::ContinueDurableTask(Box::new(action)),
                    });
                }
                if let Some(action) = accepted_plan_review {
                    outcome.terminal_reason = AgentRunTerminalReason::PlanReviewHandoff;
                    outcome.tool_calls = total_tool_calls;
                    return Ok(AgentRunOutput {
                        result: AgentRunResult {
                            final_text: String::new(),
                            tool_calls: total_tool_calls,
                            final_message_id: None,
                        },
                        outcome,
                        disposition: AgentRunDisposition::StartPlanReview(action),
                    });
                }
                if accepted_direct_conversation {
                    task_routing_decision_pending = false;
                    if let Some(conversation) = purpose.as_ref().and_then(|purpose| match purpose {
                        AgentRunPurpose::Conversation(context) => Some(context.as_ref()),
                        _ => None,
                    }) {
                        let fingerprint = conversation
                            .plan_review
                            .as_ref()
                            .map(|binding| binding.route_contract_fingerprint.clone())
                            .or_else(|| {
                                conversation
                                    .task_handoff
                                    .as_ref()
                                    .map(|binding| binding.route_contract_fingerprint.clone())
                            })
                            .ok_or_else(|| {
                                anyhow!(
                                    "accepted chat decision requires a route contract fingerprint"
                                )
                            })?;
                        append_chat_route_decision(
                            session,
                            handler,
                            &conversation.source_turn,
                            conversation.route_capability,
                            &fingerprint,
                            conversation
                                .plan_review
                                .as_ref()
                                .map(|binding| binding.decided_at_ms)
                                .or_else(|| {
                                    conversation
                                        .task_handoff
                                        .as_ref()
                                        .map(|binding| binding.decided_at_ms)
                                })
                                .unwrap_or_default(),
                        )?;
                    }
                    transient_context.push(ModelMessage::system(
                        direct_conversation_continuation_prompt_contract_material(),
                    ));
                } else if task_routing_decision_pending {
                    if !task_routing_retry_used {
                        task_routing_retry_used = true;
                        handler.handle(RunEvent::Notice(
                            "routing decision was invalid; retrying the typed routing microturn"
                                .to_owned(),
                        ))?;
                        transient_context.push(ModelMessage::system(
                            conversation_route_routing_contract_material(),
                        ));
                    } else {
                        // The model twice failed to produce a typed routing decision. Degrade to
                        // an ordinary conversation instead of blocking: the user message is
                        // answered by the same run under the direct-conversation contract.
                        handler.handle(RunEvent::Notice(
                            "routing decision was not produced; continuing as an ordinary conversation"
                                .to_owned(),
                        ))?;
                        record_fallback_chat_route_decision(session, handler, purpose.as_ref())?;
                        task_routing_decision_pending = false;
                        transient_context.push(ModelMessage::system(
                            direct_conversation_continuation_prompt_contract_material(),
                        ));
                    }
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
                        disposition: AgentRunDisposition::TaskPlanAccepted,
                    });
                }
                if accepted_plan_draft {
                    let Some(draft_context) = plan_review_draft.as_ref() else {
                        return Err(anyhow!(
                            "plan review draft accepted without a bound draft context"
                        ));
                    };
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
                            final_text: "plan draft submitted; awaiting your decision".to_owned(),
                            tool_calls: total_tool_calls,
                            final_message_id: None,
                        },
                        outcome,
                        disposition: AgentRunDisposition::PlanReviewDraftSubmitted(
                            PlanReviewDraftSubmittedAction {
                                plan_review_id: draft_context.plan_review_id.clone(),
                                attempt_id: draft_context.attempt_id.clone(),
                                plan_id: draft_context.plan_id.clone(),
                            },
                        ),
                    });
                }
                if accepted_task_guidance {
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
                            final_text:
                                "task guidance accepted for pending steps; orchestration will continue"
                                    .to_owned(),
                            tool_calls: total_tool_calls,
                            final_message_id: None,
                        },
                        outcome,
                        disposition: AgentRunDisposition::TaskPlanAccepted,
                    });
                }
                continue;
            }

            if task_routing_decision_pending {
                if !task_routing_retry_used {
                    task_routing_retry_used = true;
                    handler.handle(RunEvent::Notice(
                        "routing microturn returned free text; retrying with a typed decision"
                            .to_owned(),
                    ))?;
                    transient_context.push(ModelMessage::system(
                        conversation_route_routing_contract_material(),
                    ));
                    continue;
                }
                // The model twice failed to produce a typed routing decision. Degrade to an
                // ordinary conversation instead of blocking: the user message is answered by
                // the same run under the direct-conversation contract.
                handler.handle(RunEvent::Notice(
                    "routing decision was not produced; continuing as an ordinary conversation"
                        .to_owned(),
                ))?;
                record_fallback_chat_route_decision(session, handler, purpose.as_ref())?;
                task_routing_decision_pending = false;
                transient_context.push(ModelMessage::system(
                    direct_conversation_continuation_prompt_contract_material(),
                ));
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
                    disposition: AgentRunDisposition::Blocked,
                });
            }

            let blocker_prompt = agent_delegate
                .as_deref_mut()
                .map(|delegate| delegate.final_answer_blocker(session))
                .transpose()?
                .flatten();
            if let Some(blocker_prompt) = blocker_prompt {
                if final_answer_blocker_retries >= MAX_FINAL_ANSWER_BLOCKER_RETRIES {
                    handler.handle(RunEvent::Notice(
                        "pending agent state still blocks final answer; ending this run without another provider retry"
                            .to_owned(),
                    ))?;
                    outcome.terminal_reason = AgentRunTerminalReason::FinalAnswerBlocked;
                    outcome.tool_calls = total_tool_calls;
                    claim_natural_run_terminal(
                        cancellation.as_ref(),
                        cancellation_terminal_authority,
                    )?;
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
                        disposition: AgentRunDisposition::Blocked,
                    });
                }
                final_answer_blocker_retries = final_answer_blocker_retries.saturating_add(1);
                handler.handle(RunEvent::Notice(
                    "pending agent state blocks final answer; continuing".to_owned(),
                ))?;
                if final_answer_blocker_prompt.as_deref() != Some(blocker_prompt.as_str()) {
                    let message = ModelMessage::user(blocker_prompt.clone());
                    if let Some(index) = final_answer_blocker_message_index {
                        transient_context[index] = message;
                    } else {
                        final_answer_blocker_message_index = Some(transient_context.len());
                        transient_context.push(message);
                    }
                    final_answer_blocker_prompt = Some(blocker_prompt);
                }
                continue;
            }
            if let Some(index) = final_answer_blocker_message_index.take() {
                transient_context.remove(index);
                if let Some(context_index) = final_answer_context_message_index.as_mut()
                    && *context_index > index
                {
                    *context_index = context_index.saturating_sub(1);
                }
            }
            final_answer_blocker_prompt = None;
            if participant_finalization_dispatched && assistant_text.trim().is_empty() {
                handler.handle(RunEvent::Notice(
                    "task participant finalization returned no bounded result".to_owned(),
                ))?;
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
                    disposition: AgentRunDisposition::Interrupted,
                });
            }
            // Final-answer gate: a queued follow-up keeps the run alive instead of finalizing.
            // The pending assistant text is persisted so the follow-up answer continues the
            // transcript instead of replacing it.
            if !task_routing_decision_pending
                && matches!(purpose.as_ref(), Some(AgentRunPurpose::Conversation(_)))
                && let Some(provider) = pending_input_provider.as_ref()
                && promote_pending_follow_up(
                    provider.as_ref(),
                    &mut *session,
                    &logical_run_id,
                    handler,
                )
                .await?
            {
                if !assistant_text.trim().is_empty() {
                    let exact_message = ModelMessage::assistant_with_kind(
                        Some(assistant_text),
                        Vec::new(),
                        crate::AssistantMessageKind::FinalAnswer,
                    );
                    let (message, _) = crate::project_message_for_persistence(exact_message)?;
                    let message_id = message.id.clone();
                    session.append_assistant_message(message.clone())?;
                    handler.handle(RunEvent::AssistantMessage(message))?;
                    save_continuation_states(session, handler, pending_states, &message_id)?;
                }
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
            let readiness =
                projected_agent_run_readiness(session, &options, &final_message_id, &outcome)?;
            append_completed_run_lifecycle_events(
                session,
                outcome.terminal_reason,
                &final_message_id,
                total_tool_calls,
                readiness.clone(),
            )?;
            handler.handle(RunEvent::Control(ControlEntry::ReadinessEvaluated(
                readiness,
            )))?;
            return Ok(AgentRunOutput {
                result: AgentRunResult {
                    final_text: assistant_text,
                    tool_calls: total_tool_calls,
                    final_message_id: Some(final_message_id),
                },
                outcome,
                disposition: AgentRunDisposition::FinalAnswer,
            });
        }
    }
}

struct AuthorizedToolCall {
    call: ToolCall,
    execution_spec: Option<ToolSpec>,
    /// Subjects resolved directly from the tool at authorization time. Prepared tools may replace
    /// these with artifact-exact subjects for policy evaluation, but execution must still prove
    /// that the live canonical targets have not drifted in the meantime.
    resolved_subjects: Vec<ToolSubject>,
    resolved_subject_zones: Vec<PathTrustZone>,
    execution_subjects: Vec<ToolSubject>,
    permission_plan: Option<ToolPermissionPlanV2>,
    approval_identity: Option<ApprovalRequestIdentityV2>,
    prepared_tool_call: Option<PreparedToolCall>,
    explicit_network_approval: bool,
    explicit_user_approval: bool,
}

struct ToolCallProcessingContext<'run, 'policy, 'delegate, H, A> {
    session: &'run mut Session,
    handler: &'run mut H,
    tools: &'run ToolRegistry,
    options: &'run AgentRunOptions,
    permission_policy: &'run PermissionPolicyChain<'policy>,
    tool_ctx: ToolContext,
    cancellation: Option<RunCancellationHandle>,
    root_logical_run_id: &'run str,
    agent_delegation_run_context: Option<&'run crate::AgentDelegationRunContext>,
    agent_delegate: &'run mut Option<&'delegate mut (dyn AgentToolDelegate + Send)>,
    approval_handler: &'run mut A,
    outcome: &'run mut AgentRunOutcome,
    satisfied_agent_tool_calls: &'run mut usize,
    transient_context: &'run mut Vec<ModelMessage>,
    web_task_tree_budget: Option<Arc<crate::WebTaskTreeBudget>>,
    tool_artifact_read_budget: crate::session::ToolArtifactReadBudgetV1,
    /// RFC-0062 11.2: collected results of the current assistant tool-call batch. Ordinary tool
    /// executions and their error branches settle here instead of emitting per result.
    assistant_batch_results: &'run mut Vec<(crate::ToolCall, ToolResult)>,
}

async fn process_tool_call<H, A>(
    context: ToolCallProcessingContext<'_, '_, '_, H, A>,
    mut call: ToolCall,
    safe_call: ToolCall,
) -> Result<()>
where
    H: EventHandler + Send,
    A: ApprovalHandler + Send,
{
    let ToolCallProcessingContext {
        session,
        handler,
        tools,
        options,
        permission_policy,
        tool_ctx,
        cancellation,
        root_logical_run_id,
        agent_delegation_run_context,
        agent_delegate,
        approval_handler,
        outcome,
        satisfied_agent_tool_calls,
        transient_context,
        web_task_tree_budget,
        tool_artifact_read_budget,
        assistant_batch_results,
    } = context;
    let mut explicit_network_approval = false;
    let mut explicit_user_approval = false;
    let _tool_effect = begin_run_effect(cancellation.as_ref(), RunEffectKind::Tool)?;
    let mut execution_subjects = Vec::new();
    let mut resolved_subjects = Vec::new();
    let mut resolved_subject_zones = Vec::new();
    let mut execution_permission_plan = None;
    let mut execution_approval_identity = None;
    let mut prepared_tool_call = None;
    let execution_spec = tools.spec_for(&call.name);
    if let Some(spec) = execution_spec.as_ref() {
        let preparation_draft = match tools.prepare(tool_ctx.clone(), call.clone()).await {
            Ok(preparation) => preparation,
            Err(error) => {
                append_invalid_tool_input_result(
                    session,
                    outcome,
                    &call,
                    &[],
                    error,
                    assistant_batch_results,
                )?;
                return Ok(());
            }
        };
        let prepared_subjects = preparation_draft.as_ref().map(|draft| draft.subjects());
        let permission_plan =
            match tools.permission_plan_with_subjects(&tool_ctx, &call, prepared_subjects) {
                Ok(plan) => plan,
                Err(error) => {
                    append_invalid_tool_input_result(
                        session,
                        outcome,
                        &call,
                        prepared_subjects.unwrap_or_default(),
                        error,
                        assistant_batch_results,
                    )?;
                    return Ok(());
                }
            };
        let resolved_permission_plan = if prepared_subjects.is_some() {
            Some(match tools.permission_plan(&tool_ctx, &call) {
                Ok(plan) => plan,
                Err(error) => {
                    append_invalid_tool_input_result(
                        session,
                        outcome,
                        &call,
                        prepared_subjects.unwrap_or_default(),
                        error,
                        assistant_batch_results,
                    )?;
                    return Ok(());
                }
            })
        } else {
            None
        };
        let decision = permission_policy.decide_plan(spec, &permission_plan)?;
        if let Some(resolved_plan) = resolved_permission_plan {
            resolved_subjects = resolved_plan.subjects.clone();
            resolved_subject_zones = permission_policy
                .decide_plan(spec, &resolved_plan)?
                .subject_zones;
        } else {
            resolved_subjects = permission_plan.subjects.clone();
            resolved_subject_zones = decision.subject_zones.clone();
        }
        append_tool_permission_plan_audit(session, handler, &call, &permission_plan)?;
        execution_permission_plan = Some(permission_plan.clone());
        let pre_plan_decision = interactive_external_directory_approval_override(options, decision);
        let plan_authority = active_plan_approval_authority(session, spec, &pre_plan_decision);
        let binding_decision = plan_approval_decision_override(session, spec, pre_plan_decision);
        let policy_fingerprint = preparation_policy_fingerprint(&binding_decision)?;
        let (decision, session_grant_source) = tool_session_grant_decision_override(
            session,
            &permission_plan,
            &policy_fingerprint,
            binding_decision.clone(),
        );
        explicit_network_approval = session_grant_source.as_ref().is_some_and(|grant| {
            grant
                .facets
                .contains(&ToolApprovalSessionGrantFacet::Network)
        }) || decision.danger_full_access_network_authorized();
        let approval_identity = if decision.mode == ApprovalMode::Ask
            && options.interaction_mode == InteractionMode::Interactive
        {
            let session_id = tool_ctx
                .session_scope_id()
                .ok_or_else(|| anyhow!("interactive approval requires a bound session scope"))?;
            Some(ApprovalRequestIdentityV2 {
                session_id: session_id.to_owned(),
                run_id: root_logical_run_id.to_owned(),
                call_id: call.id.clone(),
                approval_request_id: uuid::Uuid::new_v4().to_string(),
                plan_hash: permission_plan.plan_hash.clone(),
                policy_version: policy_fingerprint.clone(),
                execution_binding_hash: permission_plan.plan_hash.clone(),
                expires_at_ms: APPROVAL_REQUEST_NO_EXPIRY_MS,
            })
        } else {
            None
        };
        execution_approval_identity = approval_identity.clone();
        prepared_tool_call =
            match preparation_draft {
                Some(draft) => {
                    let approval_identity = if let Some(grant) = session_grant_source.as_ref() {
                        preparation_session_grant_identity(grant)?
                    } else if let Some(authority) = plan_authority.as_ref() {
                        preparation_plan_approval_identity(authority)?
                    } else if let Some(identity) = approval_identity.as_ref() {
                        pending_interactive_approval_identity(&identity.approval_request_id)
                    } else {
                        preparation_policy_approval_identity(&policy_fingerprint)
                    };
                    Some(draft.bind_with_approval_identity(
                        policy_fingerprint.clone(),
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
            handler,
            &call,
            &decision,
            &permission_plan,
            &policy_fingerprint,
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
            approval_identity.as_ref(),
            &permission_plan,
            prepared_tool_call.take(),
        )
        .await?;
        prepared_tool_call = preview_capture.prepared;
        execution_subjects = decision.subjects.clone();

        match decision.mode {
            ApprovalMode::Allow => {}
            ApprovalMode::Ask if options.interaction_mode == InteractionMode::Headless => {
                let reason = format!("tool {} requires approval in headless mode", call.name);
                let mut result = ToolResult::error(
                    call.id.clone(),
                    call.name.clone(),
                    ToolErrorKind::ApprovalRequired,
                    reason,
                );
                attach_tool_call_context(&mut result, &call, &decision.subjects);
                assistant_batch_results.push((call.clone(), result));
                return Ok(());
            }
            ApprovalMode::Ask => {
                let approval_identity = approval_identity
                    .as_ref()
                    .expect("interactive Ask decisions must bind an approval identity");
                let preview = preview_capture.preview.clone();
                let preview_hash = preview_capture.preview_hash.clone();
                let requested_at_ms = unix_time_ms();
                let approval_context = ToolApprovalContext {
                    identity: approval_identity.clone(),
                    permission_signature: approval_permission_signature(
                        &call,
                        spec,
                        &permission_plan.plan_hash,
                        &policy_fingerprint,
                        preview_hash.as_deref(),
                    )?,
                    policy_fingerprint: policy_fingerprint.clone(),
                    requested_at_ms,
                    expires_at_ms: approval_identity.expires_at_ms,
                };
                append_tool_approval_audit(
                    session,
                    &call,
                    &decision,
                    approval_identity,
                    &permission_plan,
                    ToolApprovalAuditAction::Requested,
                    None,
                    None,
                    None,
                    preview_hash.clone(),
                )?;
                let session_grant_availability =
                    tool_approval_session_grant_availability_for_plan(&decision, &permission_plan);
                let session_grant_available = session_grant_availability.is_available();
                let session_grant_unavailable_reason =
                    session_grant_availability.unavailable_reason();
                if approval_handler.should_present_tool_approval(
                    &safe_call,
                    spec,
                    &approval_context,
                )? {
                    let presentation = handler.handle(RunEvent::ToolApprovalRequested {
                        approval_identity: approval_identity.clone(),
                        effects: permission_plan.effects.clone(),
                        analysis: permission_plan.analysis.clone(),
                        containment: permission_plan.containment.clone(),
                        safe_summary: permission_plan.safe_summary.clone(),
                        decision_reasons: decision.reasons.clone(),
                        session_grant_available,
                        session_grant_unavailable_reason,
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
                        command_permission_matches: decision.command_permission_matches.clone(),
                        preview,
                    });
                    if let Err(error) = presentation {
                        approval_handler.tool_approval_presentation_failed(
                            &safe_call,
                            spec,
                            &approval_context,
                            &format!("{error:#}"),
                        )?;
                        append_tool_approval_audit(
                            session,
                            &call,
                            &decision,
                            approval_identity,
                            &permission_plan,
                            ToolApprovalAuditAction::Resolved,
                            None,
                            Some(format!("approval presentation cancelled: {error:#}")),
                            Some(ToolApprovalTerminalStatusV2::Cancelled),
                            preview_hash.clone(),
                        )?;
                        return Err(error);
                    }
                }
                let approval = match approval_handler.approve_tool_call_with_context(
                    &safe_call,
                    spec,
                    &approval_context,
                ) {
                    Ok(approval) => approval,
                    Err(error) => {
                        append_tool_approval_audit(
                            session,
                            &call,
                            &decision,
                            approval_identity,
                            &permission_plan,
                            ToolApprovalAuditAction::Resolved,
                            None,
                            Some(format!("approval route cancelled: {error:#}")),
                            Some(ToolApprovalTerminalStatusV2::Cancelled),
                            preview_hash.clone(),
                        )?;
                        return Err(error);
                    }
                };
                let accepted_decision = match &approval {
                    ToolApproval::Approve | ToolApproval::ApproveWithArgs { .. } => {
                        Some(ToolApprovalUserDecision::Approved)
                    }
                    ToolApproval::ApproveForSession => {
                        Some(ToolApprovalUserDecision::ApprovedForSession)
                    }
                    ToolApproval::Deny { .. } => Some(ToolApprovalUserDecision::Denied),
                    ToolApproval::Expired { .. }
                    | ToolApproval::Cancelled { .. }
                    | ToolApproval::Stale { .. } => None,
                };
                if let Some(accepted_decision) = accepted_decision {
                    append_tool_approval_audit(
                        session,
                        &call,
                        &decision,
                        approval_identity,
                        &permission_plan,
                        ToolApprovalAuditAction::DecisionAccepted,
                        Some(accepted_decision),
                        None,
                        None,
                        preview_hash.clone(),
                    )?;
                }
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
                    let reason = "network approval requires an explicit user action".to_owned();
                    append_tool_approval_audit(
                        session,
                        &call,
                        &decision,
                        approval_identity,
                        &permission_plan,
                        ToolApprovalAuditAction::Resolved,
                        None,
                        Some(reason.clone()),
                        Some(ToolApprovalTerminalStatusV2::Denied),
                        preview_hash,
                    )?;
                    handler.handle(RunEvent::ToolApprovalResolved {
                        call_id: call.id.clone(),
                        approval_request_id: approval_identity.approval_request_id.clone(),
                        approved: false,
                        reason: Some(reason.clone()),
                    })?;
                    let mut result = ToolResult::error(
                        call.id.clone(),
                        call.name.clone(),
                        ToolErrorKind::ApprovalDenied,
                        reason,
                    );
                    attach_tool_call_context(&mut result, &call, &decision.subjects);
                    assistant_batch_results.push((call.clone(), result));
                    return Ok(());
                }
                let approval_is_explicit_network_user_action = approval_is_explicit_user_action
                    && decision.network_effect.is_some()
                    && decision.network_policy_decision == ApprovalMode::Ask;
                match approval {
                    ToolApproval::Approve => {
                        explicit_user_approval = approval_is_explicit_user_action;
                        explicit_network_approval = approval_is_explicit_network_user_action;
                        append_tool_approval_audit(
                            session,
                            &call,
                            &decision,
                            approval_identity,
                            &permission_plan,
                            ToolApprovalAuditAction::Resolved,
                            Some(ToolApprovalUserDecision::Approved),
                            None,
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
                            approval_request_id: approval_identity.approval_request_id.clone(),
                            approved: true,
                            reason: None,
                        })?;
                    }
                    ToolApproval::ApproveForSession => {
                        if !session_grant_available {
                            let reason =
                                "session approval grant is not available for this tool call"
                                    .to_owned();
                            append_tool_approval_audit(
                                session,
                                &call,
                                &decision,
                                approval_identity,
                                &permission_plan,
                                ToolApprovalAuditAction::Resolved,
                                None,
                                Some(reason.clone()),
                                Some(ToolApprovalTerminalStatusV2::Stale),
                                preview_hash,
                            )?;
                            handler.handle(RunEvent::ToolApprovalResolved {
                                call_id: call.id.clone(),
                                approval_request_id: approval_identity.approval_request_id.clone(),
                                approved: false,
                                reason: Some(reason.clone()),
                            })?;
                            let mut result = ToolResult::error(
                                call.id.clone(),
                                call.name.clone(),
                                ToolErrorKind::ApprovalDenied,
                                format!("tool execution denied by user: {reason}"),
                            );
                            attach_tool_call_context(&mut result, &call, &decision.subjects);
                            assistant_batch_results.push((call.clone(), result));
                            return Ok(());
                        }
                        explicit_user_approval = approval_is_explicit_user_action;
                        explicit_network_approval = approval_is_explicit_network_user_action;
                        append_tool_approval_audit(
                            session,
                            &call,
                            &decision,
                            approval_identity,
                            &permission_plan,
                            ToolApprovalAuditAction::Resolved,
                            Some(ToolApprovalUserDecision::ApprovedForSession),
                            None,
                            None,
                            preview_hash,
                        )?;
                        authorize_prepared_tool_from_resolved_approval(
                            session,
                            &call,
                            &mut prepared_tool_call,
                        )?;
                        append_tool_approval_session_grant(
                            session,
                            handler,
                            &call,
                            &decision,
                            approval_identity,
                            &permission_plan,
                        )?;
                        handler.handle(RunEvent::ToolApprovalResolved {
                            call_id: call.id.clone(),
                            approval_request_id: approval_identity.approval_request_id.clone(),
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
                                approval_identity,
                                &permission_plan,
                                ToolApprovalAuditAction::Resolved,
                                None,
                                Some(reason.clone()),
                                Some(ToolApprovalTerminalStatusV2::Stale),
                                preview_hash,
                            )?;
                            handler.handle(RunEvent::ToolApprovalResolved {
                                call_id: call.id.clone(),
                                approval_request_id: approval_identity.approval_request_id.clone(),
                                approved: false,
                                reason: Some(reason.clone()),
                            })?;
                            let mut result = ToolResult::error(
                                call.id.clone(),
                                call.name.clone(),
                                ToolErrorKind::StalePreparedMutation,
                                reason,
                            );
                            attach_tool_call_context(&mut result, &call, &decision.subjects);
                            assistant_batch_results.push((call.clone(), result));
                            return Ok(());
                        }
                        let mut approved_call = call.clone();
                        approved_call.args_json = args_json;
                        let reevaluate_approved_call = || -> Result<_> {
                            let approved_plan = tools.permission_plan(&tool_ctx, &approved_call)?;
                            if approved_plan.plan_hash != permission_plan.plan_hash {
                                return Err(anyhow!(
                                    "approval-time argument changes altered the permission plan"
                                ));
                            }
                            let approved_decision =
                                permission_policy.decide_plan(spec, &approved_plan)?;
                            let approved_decision =
                                interactive_external_directory_approval_override(
                                    options,
                                    approved_decision,
                                );
                            let approved_decision =
                                plan_approval_decision_override(session, spec, approved_decision);
                            Ok((approved_decision, approved_plan))
                        };
                        let approved = reevaluate_approved_call();
                        let (approved_decision, approved_plan) = match approved {
                            Ok(result) => result,
                            Err(error) => {
                                let reason = format!(
                                    "approval-time argument changes could not be re-evaluated: {error}"
                                );
                                append_tool_approval_audit(
                                    session,
                                    &approved_call,
                                    &decision,
                                    approval_identity,
                                    &permission_plan,
                                    ToolApprovalAuditAction::Resolved,
                                    None,
                                    Some(reason.clone()),
                                    Some(ToolApprovalTerminalStatusV2::Stale),
                                    preview_hash,
                                )?;
                                handler.handle(RunEvent::ToolApprovalResolved {
                                    call_id: approved_call.id.clone(),
                                    approval_request_id: approval_identity
                                        .approval_request_id
                                        .clone(),
                                    approved: false,
                                    reason: Some(reason),
                                })?;
                                append_invalid_tool_input_result(
                                    session,
                                    outcome,
                                    &approved_call,
                                    &decision.subjects,
                                    error,
                                    assistant_batch_results,
                                )?;
                                return Ok(());
                            }
                        };
                        if approved_decision != decision {
                            let reason = "approval-time argument changes altered the permission scope; preview and approval must be repeated"
                                .to_owned();
                            append_tool_approval_audit(
                                session,
                                &approved_call,
                                &approved_decision,
                                approval_identity,
                                &permission_plan,
                                ToolApprovalAuditAction::Resolved,
                                None,
                                Some(reason.clone()),
                                Some(ToolApprovalTerminalStatusV2::Stale),
                                preview_hash,
                            )?;
                            handler.handle(RunEvent::ToolApprovalResolved {
                                call_id: approved_call.id.clone(),
                                approval_request_id: approval_identity.approval_request_id.clone(),
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
                            assistant_batch_results.push((call.clone(), result));
                            return Ok(());
                        }
                        execution_permission_plan = Some(approved_plan);
                        call = approved_call;
                        // Argument overrides are a different call than the one the user previewed.
                        // They may still be safe to execute after the permission scope is
                        // re-evaluated above, but they cannot mint exact-call authority for an
                        // agent delegation proposal without a fresh preview and confirmation.
                        explicit_user_approval = false;
                        execution_subjects = approved_decision.subjects.clone();
                        explicit_network_approval = approval_is_explicit_user_action
                            && approved_decision.network_effect.is_some()
                            && approved_decision.network_policy_decision == ApprovalMode::Ask;
                        append_tool_approval_audit(
                            session,
                            &call,
                            &decision,
                            approval_identity,
                            &permission_plan,
                            ToolApprovalAuditAction::Resolved,
                            Some(ToolApprovalUserDecision::Approved),
                            None,
                            None,
                            preview_hash,
                        )?;
                        handler.handle(RunEvent::ToolApprovalResolved {
                            call_id: call.id.clone(),
                            approval_request_id: approval_identity.approval_request_id.clone(),
                            approved: true,
                            reason: None,
                        })?;
                    }
                    ToolApproval::Deny { reason } => {
                        append_tool_approval_audit(
                            session,
                            &call,
                            &decision,
                            approval_identity,
                            &permission_plan,
                            ToolApprovalAuditAction::Resolved,
                            Some(ToolApprovalUserDecision::Denied),
                            Some(reason.clone()),
                            None,
                            preview_hash,
                        )?;
                        handler.handle(RunEvent::ToolApprovalResolved {
                            call_id: call.id.clone(),
                            approval_request_id: approval_identity.approval_request_id.clone(),
                            approved: false,
                            reason: Some(reason.clone()),
                        })?;
                        let mut result = ToolResult::error(
                            call.id.clone(),
                            call.name.clone(),
                            ToolErrorKind::ApprovalDenied,
                            format!("tool execution denied by user: {reason}"),
                        );
                        attach_tool_call_context(&mut result, &call, &decision.subjects);
                        assistant_batch_results.push((call.clone(), result));
                        return Ok(());
                    }
                    ToolApproval::Expired { reason } => {
                        append_tool_approval_route_terminal(
                            session,
                            handler,
                            outcome,
                            &call,
                            &decision,
                            approval_identity,
                            &permission_plan,
                            preview_hash,
                            ToolApprovalTerminalStatusV2::Expired,
                            ToolErrorKind::Timeout,
                            reason,
                            assistant_batch_results,
                        )?;
                        return Ok(());
                    }
                    ToolApproval::Cancelled { reason } => {
                        append_tool_approval_route_terminal(
                            session,
                            handler,
                            outcome,
                            &call,
                            &decision,
                            approval_identity,
                            &permission_plan,
                            preview_hash,
                            ToolApprovalTerminalStatusV2::Cancelled,
                            ToolErrorKind::Interrupted,
                            reason,
                            assistant_batch_results,
                        )?;
                        return Ok(());
                    }
                    ToolApproval::Stale { reason } => {
                        append_tool_approval_route_terminal(
                            session,
                            handler,
                            outcome,
                            &call,
                            &decision,
                            approval_identity,
                            &permission_plan,
                            preview_hash,
                            ToolApprovalTerminalStatusV2::Stale,
                            ToolErrorKind::ApprovalDenied,
                            reason,
                            assistant_batch_results,
                        )?;
                        return Ok(());
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
                let mut result =
                    ToolResult::error(call.id.clone(), call.name.clone(), error_kind, reason);
                attach_tool_call_context(&mut result, &call, &decision.subjects);
                assistant_batch_results.push((call.clone(), result));
                return Ok(());
            }
        }
        let egress_audit = match tools.egress_audit(&tool_ctx, &call) {
            Ok(audit) => audit,
            Err(error) => {
                append_invalid_tool_input_result(
                    session,
                    outcome,
                    &call,
                    &decision.subjects,
                    error,
                    assistant_batch_results,
                )?;
                return Ok(());
            }
        };
        if let Some(egress_audit) = egress_audit {
            let control = tool_egress_control_entry(&call, &decision.subjects, egress_audit);
            session.append_control(control.clone())?;
            handler.handle(RunEvent::Control(control))?;
        }
    }

    let authorized = AuthorizedToolCall {
        call,
        execution_spec,
        resolved_subjects,
        resolved_subject_zones,
        execution_subjects,
        permission_plan: execution_permission_plan,
        approval_identity: execution_approval_identity,
        prepared_tool_call,
        explicit_network_approval,
        explicit_user_approval,
    };
    execute_authorized_tool_call(
        ToolCallProcessingContext {
            session,
            handler,
            tools,
            options,
            permission_policy,
            tool_ctx,
            cancellation,
            root_logical_run_id,
            agent_delegation_run_context,
            agent_delegate,
            approval_handler,
            outcome,
            satisfied_agent_tool_calls,
            transient_context,
            web_task_tree_budget,
            tool_artifact_read_budget,
            assistant_batch_results,
        },
        authorized,
    )
    .await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn append_tool_approval_route_terminal<H: EventHandler>(
    session: &mut Session,
    handler: &mut H,
    _outcome: &mut AgentRunOutcome,
    call: &ToolCall,
    decision: &crate::PermissionDecision,
    approval_identity: &ApprovalRequestIdentityV2,
    permission_plan: &ToolPermissionPlanV2,
    preview_hash: Option<String>,
    terminal_status: ToolApprovalTerminalStatusV2,
    error_kind: ToolErrorKind,
    reason: String,
    assistant_batch_results: &mut Vec<(crate::ToolCall, ToolResult)>,
) -> Result<()> {
    append_tool_approval_audit(
        session,
        call,
        decision,
        approval_identity,
        permission_plan,
        ToolApprovalAuditAction::Resolved,
        None,
        Some(reason.clone()),
        Some(terminal_status),
        preview_hash,
    )?;
    handler.handle(RunEvent::ToolApprovalResolved {
        call_id: call.id.clone(),
        approval_request_id: approval_identity.approval_request_id.clone(),
        approved: false,
        reason: Some(reason.clone()),
    })?;
    let mut result = ToolResult::error(
        call.id.clone(),
        call.name.clone(),
        error_kind,
        format!("tool approval did not complete: {reason}"),
    );
    attach_tool_call_context(&mut result, call, &decision.subjects);
    assistant_batch_results.push((call.clone(), result));
    Ok(())
}

async fn execute_authorized_tool_call<H, A>(
    context: ToolCallProcessingContext<'_, '_, '_, H, A>,
    authorized: AuthorizedToolCall,
) -> Result<()>
where
    H: EventHandler + Send,
    A: ApprovalHandler + Send,
{
    let ToolCallProcessingContext {
        session,
        handler,
        tools,
        options,
        permission_policy,
        tool_ctx,
        cancellation,
        root_logical_run_id,
        agent_delegation_run_context,
        agent_delegate,
        approval_handler,
        outcome,
        satisfied_agent_tool_calls,
        transient_context,
        web_task_tree_budget,
        tool_artifact_read_budget,
        assistant_batch_results,
    } = context;
    let AuthorizedToolCall {
        call,
        execution_spec,
        resolved_subjects,
        resolved_subject_zones,
        execution_subjects,
        permission_plan,
        approval_identity,
        prepared_tool_call,
        explicit_network_approval,
        explicit_user_approval,
    } = authorized;
    let tool_is_agent_category = execution_spec
        .as_ref()
        .is_some_and(|spec| spec.category == ToolCategory::Agent);
    let execution_mutation_profile = execution_spec
        .as_ref()
        .map(|_| tools.execution_mutation_profile(&tool_ctx, &call))
        .transpose()?
        .flatten();
    let current_permission_plan = if let Some(spec) = execution_spec.as_ref() {
        let current_resolved = tools.permission_plan(&tool_ctx, &call)?;
        let current_resolved_subject_zones = permission_policy
            .decide_plan(spec, &current_resolved)?
            .subject_zones;
        let Some(bound) = permission_plan.as_ref() else {
            return Err(anyhow!(
                "authorized tool call is missing its permission plan"
            ));
        };
        if current_resolved.subjects != resolved_subjects
            || current_resolved_subject_zones != resolved_subject_zones
        {
            let mut result = ToolResult::error(
                call.id.clone(),
                call.name.clone(),
                ToolErrorKind::StalePreparedMutation,
                "tool permission subjects or trust zones changed after authorization; approval must be repeated",
            );
            attach_tool_call_context(&mut result, &call, &execution_subjects);
            assistant_batch_results.push((call.clone(), result));
            return Ok(());
        }
        let current = if prepared_tool_call.is_some() {
            tools.permission_plan_with_subjects(&tool_ctx, &call, Some(&execution_subjects))?
        } else {
            current_resolved
        };
        if current.plan_hash != bound.plan_hash {
            let mut result = ToolResult::error(
                call.id.clone(),
                call.name.clone(),
                ToolErrorKind::StalePreparedMutation,
                "tool permission plan changed after authorization; approval must be repeated",
            );
            attach_tool_call_context(&mut result, &call, &execution_subjects);
            assistant_batch_results.push((call.clone(), result));
            return Ok(());
        }
        Some(current)
    } else {
        None
    };
    let prepared_current_authority = if let Some(prepared) = prepared_tool_call.as_ref() {
        let spec = execution_spec
            .as_ref()
            .expect("prepared tools must retain their execution spec");
        let current_plan = current_permission_plan
            .as_ref()
            .expect("prepared tools must retain their permission plan");
        let current_decision = permission_policy.decide_plan(spec, current_plan)?;
        let current_pre_plan_decision =
            interactive_external_directory_approval_override(options, current_decision);
        let current_plan_authority =
            active_plan_approval_authority(session, spec, &current_pre_plan_decision);
        let current_decision =
            plan_approval_decision_override(session, spec, current_pre_plan_decision);
        let current_policy_fingerprint = preparation_policy_fingerprint(&current_decision)?;
        let bound_identity = &prepared.binding().approval_identity;
        let current_approval_identity = if bound_identity.starts_with("session-grant:") {
            let (_, current_grant) = tool_session_grant_decision_override(
                session,
                current_plan,
                &current_policy_fingerprint,
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
            resolved_interactive_approval_identity(session, &call.id, prepared.prepared_digest())?
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
        handler,
        &call,
        &execution_subjects,
        current_permission_plan.as_ref(),
        approval_identity.as_ref(),
        execution_mutation_profile.as_ref(),
        prepared_audit_binding.as_ref(),
    )?;
    let execution_started = Instant::now();
    let mut execution_tool_ctx = tool_ctx
        .clone()
        .with_network_authorization(
            options.permission_context.network_policy,
            explicit_network_approval,
        )
        .with_approved_subjects(execution_subjects.clone());
    if let Some(plan) = current_permission_plan.clone() {
        execution_tool_ctx = execution_tool_ctx.with_prepared_permission_plan(plan);
    }
    let mut result = if let Some(prepared) = prepared_tool_call {
        let (current_policy_fingerprint, current_approval_identity) = prepared_current_authority
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
            .filter(|_| execution_spec.is_some() && tool_is_agent_category)
        {
            Some(delegate) => {
                delegate.set_run_cancellation(cancellation.clone());
                delegate.set_root_logical_run_id(Some(root_logical_run_id));
                delegate.set_agent_delegation_run_context(agent_delegation_run_context);
                delegate.set_agent_tool_authorization(Some(&call), explicit_user_approval);
                delegate.set_web_task_tree_budget(web_task_tree_budget.clone());
                // Children inherit the remaining counters and must not reset them per turn.
                delegate.set_tool_artifact_read_budget(Some(
                    tool_artifact_read_budget.without_turn_reset(),
                ));
                let result = delegate
                    .handle_agent_tool_call(session, &call, options, handler, approval_handler)
                    .await;
                delegate.set_agent_tool_authorization(None, false);
                match result {
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
    if let Some(entry) = append_terminal_task_control_from_result(session, handler, &result)? {
        reconcile_terminal_task_mutation_from_start(session, &options.workspace_root, &entry)?;
    }
    record_tool_run_outcome(outcome, &result);
    if tool_is_agent_category && agent_tool_result_satisfies_delegation(&result) {
        *satisfied_agent_tool_calls = (*satisfied_agent_tool_calls).saturating_add(1);
    }
    let tool_transient_context = std::mem::take(&mut result.transient_context);
    assistant_batch_results.push((call.clone(), result));
    transient_context.extend(tool_transient_context);
    Ok(())
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
