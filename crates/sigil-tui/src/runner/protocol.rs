use std::{
    fmt,
    path::PathBuf,
    sync::{Arc, mpsc},
};

use sigil_kernel::{
    AgentRunResult, AgentThreadId, AgentThreadStatusChangedEntry, CompactionEconomicsV2,
    ControlledCheckpointRestorePreview, ControlledCheckpointRestoreRequest, ConversationInputKind,
    ConversationInputQueueId, ConversationInputTarget, ConversationQueueItemProjection,
    DisclosurePresentationError, DisclosurePresentationReceipt, ImageAttachment,
    IntentDropRequestV1, IntentOperationExecutionV1, IntentOperationPreviewV1, IntentVersionRef,
    MutationArtifactCleanupTarget, PlanApprovalPermission, PlanDecisionRecordedEntry,
    PlanTaskStartMode, PreEgressDisclosure, PublicIntentStackStateV1, PublicRouteRecoveryAction,
    PublicRouteRecoveryCode, ReasoningEffort, ResolvedModelRoute, RunEvent, SessionLogEntry,
    TaskCreatedFromPlanEntry, TaskIntegrationReviewRequest, TaskPauseRequest, TaskRunStatus,
    TaskVerificationRerunRequest, TerminalTaskEntry, V2CompactionPreview,
};
use sigil_runtime::{
    BalanceSnapshot, LocalSessionCatalogEntry, McpElicitationRequest, McpElicitationResponse,
    McpListChangedNotification, McpProgressNotification, ProviderStatusConfig, SessionDeleteOutput,
    SessionDeletePreview, SessionExportOutput, SessionRetentionOutput, SessionRetentionPolicy,
    SessionRetentionPreview, TaskProviderRouteDiagnosticsSnapshot,
    provider_connections::{ModelCatalogRequest, ModelCatalogResult, PreparedCredential},
};
use tokio::sync::oneshot;

use super::worker_event::WorkerEvent;

pub(crate) type McpElicitationResponseTx = oneshot::Sender<McpElicitationResponse>;
pub(crate) type EgressDisclosureReceiptTx =
    oneshot::Sender<Result<DisclosurePresentationReceipt, DisclosurePresentationError>>;

pub(crate) const WORKER_COMMAND_PROTOCOL_VERSION: u16 = 2;

/// Local admission state for a reviewed V2 portable compaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum V2CompactionAdmission {
    /// Local-only prepare completed. No provider request has been sent and no durable projection
    /// has changed; the user may keep the epoch, apply standalone shrink, or request the billed
    /// semantic-summary stage.
    Prepared {
        standalone_tool_output_shrink_available: bool,
    },
    Ready {
        before_input_tokens: u64,
        input_tokens: u64,
        context_window_tokens: u64,
        output_tokens: u64,
        safety_buffer_tokens: u64,
        savings_tokens: u64,
        savings_ratio_ppm: u32,
        minimum_savings_tokens: u64,
        minimum_savings_ratio_ppm: u32,
        summary_usage_observed: bool,
        deterministic_emergency_fallback: bool,
        summary_cache_read_tokens: u64,
        summary_uncached_input_tokens: u64,
        summary_output_tokens: u64,
        summary_cost_nano_usd: Option<u64>,
        economics_v2: Option<Box<CompactionEconomicsV2>>,
    },
    Unavailable {
        reason: String,
    },
}

/// User-visible source of an activated portable V2 compaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum V2CompactionApplySource {
    DirectCommand,
    ManualConfirmation,
    IdleAutomatic,
    PreTurnPressure,
    OverflowRecovery,
}

/// Safe metadata for one next-epoch recoverable tool-output preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutputShrinkPreview {
    pub(crate) tool_name: String,
    pub(crate) tool_call_id: String,
    pub(crate) status: String,
    pub(crate) original_content_bytes: u64,
    pub(crate) original_content_token_upper_bound: u64,
    pub(crate) head_excerpt: String,
    pub(crate) tail_excerpt: String,
    pub(crate) content_sha256: String,
    pub(crate) artifact_ref: String,
    pub(crate) reason: String,
    pub(crate) recovery_instruction: String,
}

/// A read-only fold plan paired with the result of local target-request admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V2CompactionReview {
    pub(crate) request_id: u64,
    pub(crate) strategy: sigil_kernel::CompactionStrategy,
    pub(crate) preview: V2CompactionPreview,
    pub(crate) admission: V2CompactionAdmission,
    pub(crate) tool_output_shrink_candidates: Vec<ToolOutputShrinkPreview>,
    pub(crate) continuity: Option<V2ContinuityPreview>,
    pub(crate) native_carrier_requested: bool,
}

/// Safe authority/continuity evidence rendered before compaction activation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V2ContinuityPreview {
    pub(crate) root_objective: String,
    pub(crate) active_constraints: Vec<V2ConstraintPreview>,
    pub(crate) active_constraint_count: usize,
    pub(crate) authorization_boundary_count: usize,
    pub(crate) recoverable_attachment_count: usize,
    pub(crate) pending_work_count: usize,
    pub(crate) unresolved_question_count: usize,
    pub(crate) source_ref_count: usize,
}

/// Bounded exact constraint and durable source rendered in the confirmation modal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V2ConstraintPreview {
    pub(crate) text: String,
    pub(crate) source_event_id: String,
    pub(crate) source_field_path: String,
}

/// Read-only outcome of a V2 compaction preview request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum V2CompactionPreviewState {
    Review(Box<V2CompactionReview>),
    NoFoldableHistory {
        durable_message_count: usize,
        minimum_tail_turn_count: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerCommandEnvelope<T> {
    pub(crate) protocol_version: u16,
    pub(crate) command_id: String,
    pub(crate) client_id: String,
    pub(crate) session_id: String,
    pub(crate) expected_stream_sequence: Option<u64>,
    pub(crate) correlation_id: Option<String>,
    pub(crate) payload: T,
}

impl<T> WorkerCommandEnvelope<T> {
    pub(crate) fn new(
        command_id: impl Into<String>,
        client_id: impl Into<String>,
        session_id: impl Into<String>,
        payload: T,
    ) -> Self {
        Self {
            protocol_version: WORKER_COMMAND_PROTOCOL_VERSION,
            command_id: command_id.into(),
            client_id: client_id.into(),
            session_id: session_id.into(),
            expected_stream_sequence: None,
            correlation_id: None,
            payload,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerApprovalCommand {
    Decision {
        call_id: String,
        approval_request_id: String,
        approved: bool,
    },
    DecisionForSession {
        call_id: String,
        approval_request_id: String,
    },
    DecisionForFamily {
        call_id: String,
        approval_request_id: String,
        pattern: String,
    },
    DecisionWithArgs {
        call_id: String,
        approval_request_id: String,
        args_json: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerApprovalDecision {
    ApproveOnce,
    ApproveForSession,
    ApproveForFamily,
    ApproveWithArgs,
    Deny,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerApprovalRouteState {
    DecisionAccepted,
    Rejected,
    DeliveryUncertain,
}

/// Typed acknowledgement for one exact TUI approval command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerApprovalCommandReceipt {
    pub command_id: String,
    pub approval_request_id: String,
    pub call_id: String,
    pub decision: WorkerApprovalDecision,
    pub route_state: WorkerApprovalRouteState,
    pub replayed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueMoveDirection {
    Up,
    Down,
}

/// Identity of one local queue mutation; it never represents the foreground run outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueueOperation {
    Enqueue {
        prompt_hash: String,
        kind: ConversationInputKind,
        target: ConversationInputTarget,
    },
    Cancel {
        queue_id: ConversationInputQueueId,
    },
    Edit {
        queue_id: ConversationInputQueueId,
        prompt_hash: String,
    },
    Move {
        queue_id: ConversationInputQueueId,
        direction: QueueMoveDirection,
    },
    Promote {
        queue_id: ConversationInputQueueId,
    },
    SendNow {
        queue_id: ConversationInputQueueId,
    },
    SetPaused {
        paused: bool,
    },
}

impl QueueOperation {
    pub(crate) fn queue_id(&self) -> Option<&ConversationInputQueueId> {
        match self {
            Self::Cancel { queue_id }
            | Self::Edit { queue_id, .. }
            | Self::Move { queue_id, .. }
            | Self::Promote { queue_id }
            | Self::SendNow { queue_id } => Some(queue_id),
            Self::Enqueue { .. } | Self::SetPaused { .. } => None,
        }
    }

    pub(crate) fn pending_label(&self) -> &'static str {
        match self {
            Self::Enqueue { .. } => "saving follow-up",
            Self::Cancel { .. } => "removing follow-up",
            Self::Edit { .. } => "saving follow-up edit",
            Self::Move { .. } => "moving follow-up",
            Self::Promote { .. } | Self::SendNow { .. } => "scheduling follow-up next",
            Self::SetPaused { paused: true } => "pausing follow-ups",
            Self::SetPaused { paused: false } => "resuming follow-ups",
        }
    }

    pub(crate) fn success_label(&self) -> &'static str {
        match self {
            Self::Enqueue { .. } => "follow-up saved",
            Self::Cancel { .. } => "follow-up removed",
            Self::Edit { .. } => "follow-up edited",
            Self::Move { .. } => "follow-up moved",
            Self::Promote { .. } | Self::SendNow { .. } => "follow-up will run next",
            Self::SetPaused { paused: true } => "queue paused",
            Self::SetPaused { paused: false } => "queue resumed",
        }
    }
}

/// A local queue refusal, classified from durable status rather than error text.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum QueueOperationFailure {
    #[error("follow-up {} not found", .queue_id.as_str())]
    UnknownItem { queue_id: ConversationInputQueueId },
    #[error("follow-up {} is already {status:?}", .queue_id.as_str())]
    ItemUnavailable {
        queue_id: ConversationInputQueueId,
        status: sigil_kernel::ConversationInputStatus,
    },
    #[error("{message}")]
    InvalidInput { message: String },
    #[error("{message}")]
    Unavailable { message: String },
    #[error("{message}")]
    Storage { message: String },
}

impl From<String> for QueueOperationFailure {
    fn from(message: String) -> Self {
        Self::Storage { message }
    }
}

#[derive(Debug, Clone)]
pub enum McpOAuthUserAction {
    Inspect,
    SignIn,
    ManualCallback(sigil_kernel::SecretString),
    Cancel,
    Refresh,
    Revoke,
    ClearLocal,
}

#[derive(Debug)]
pub enum WorkerCommand {
    ResumeCommittedUserInput {
        original_operation: Box<sigil_kernel::ApplicationOperationBindingV1>,
    },

    QueryApplicationOperation {
        binding: Box<sigil_kernel::ApplicationOperationBindingV1>,
        reply: std::sync::mpsc::Sender<
            Result<
                (
                    sigil_kernel::ApplicationOperationBindingV1,
                    Option<sigil_kernel::session::ApplicationOperationCommitProofV1>,
                ),
                String,
            >,
        >,
    },
    FindCommittedApplicationOperation {
        target: Box<sigil_kernel::ApplicationOperationTargetV1>,
        key_digest: String,
        reply: std::sync::mpsc::Sender<
            Result<Option<sigil_kernel::ApplicationOperationBindingV1>, String>,
        >,
    },
    PrepareApplicationOperation {
        binding: Box<sigil_kernel::ApplicationOperationBindingV1>,
        reply: mpsc::Sender<Result<(), String>>,
    },
    ApplicationDispatch {
        binding: Option<Box<sigil_kernel::ApplicationOperationBindingV1>>,
        run_admission: Option<WorkerRunAdmission>,
        command: Box<WorkerCommand>,
        reply: mpsc::Sender<Result<WorkerApplicationDispatchOutcome, String>>,
    },
    SubmitPrompt {
        prompt: String,
        reasoning_effort: ReasoningEffort,
    },
    SubmitPromptWithAttachments {
        prompt: String,
        attachments: Vec<ImageAttachment>,
        reasoning_effort: ReasoningEffort,
    },
    SubmitReviewedPrompt {
        prompt: String,
        attachments: Vec<ImageAttachment>,
        reasoning_effort: ReasoningEffort,
        expected_session_id: String,
        annotations: Vec<sigil_application::ReviewAnnotation>,
    },
    QueueConversationInput {
        prompt: String,
        kind: ConversationInputKind,
        target: ConversationInputTarget,
        reasoning_effort: ReasoningEffort,
    },
    CancelQueuedConversationInput {
        queue_id: ConversationInputQueueId,
    },
    EditQueuedConversationInput {
        queue_id: ConversationInputQueueId,
        prompt: String,
        reasoning_effort: ReasoningEffort,
    },
    MoveQueuedConversationInput {
        queue_id: ConversationInputQueueId,
        direction: QueueMoveDirection,
    },
    PromoteQueuedConversationInput {
        queue_id: ConversationInputQueueId,
    },
    SendQueuedConversationInputNow {
        queue_id: ConversationInputQueueId,
    },
    SetConversationQueuePaused {
        paused: bool,
    },
    SubmitPlanPrompt {
        prompt: String,
        reasoning_effort: ReasoningEffort,
    },
    /// Runtime switch of the primary permission mode for the active run (and the persisted
    /// default). Consulted at decision time via the shared mode override.
    UpdateActiveRunPermissionMode {
        mode: sigil_kernel::PermissionMode,
    },
    CreateTaskFromPlan {
        plan_id: String,
        expected_plan_hash: String,
        start_mode: PlanTaskStartMode,
        permission_grant: Option<PlanApprovalPermission>,
    },
    RejectPlan {
        plan_id: String,
        expected_plan_hash: String,
    },
    SavePlan {
        plan_id: String,
        expected_plan_hash: String,
    },
    RevisePlan {
        plan_id: String,
        expected_plan_hash: String,
    },
    AdoptPlanCandidate {
        plan_id: String,
        expected_candidate_hash: String,
    },
    RetryPlanReview {
        plan_id: String,
        expected_candidate_hash: Option<String>,
    },
    SubmitUserInputDecision {
        command_id: Option<String>,
        request_id: String,
        generation: u32,
        expected_request_hash: String,
        decision: sigil_kernel::UserInputDecisionV1,
    },
    /// Private TUI composition recovery for one already accepted user input.
    ///
    /// This intentionally carries no decision payload. The worker re-reads the exact accepted
    /// receipt under its current session/child authority before feeding the existing user-input
    /// dispatcher. It is not an application command or transport DTO.
    ResumeRecoveredUserInput {
        command_id: String,
        request_id: String,
        generation: u32,
        expected_request_hash: String,
    },
    InvokeInlineSkill {
        skill_id: String,
        arguments: String,
        attachments: Vec<sigil_kernel::ImageAttachment>,
        reasoning_effort: ReasoningEffort,
    },
    InvokeChildSessionSkill {
        skill_id: String,
        arguments: String,
    },
    InvokeAgentProfile {
        profile_id: String,
        prompt: String,
        parent_prompt: String,
    },
    SubmitTask {
        prompt: String,
    },
    ContinueTask {
        task_id: Option<String>,
        guidance: Option<String>,
    },
    PauseTask {
        request: TaskPauseRequest,
    },
    ApprovalCommand(WorkerCommandEnvelope<WorkerApprovalCommand>),
    BackgroundActiveAgent,
    CancelRun,
    CancelTerminalTask {
        identity: TerminalTaskControlIdentity,
    },
    CloseAgent {
        thread_id: AgentThreadId,
        reason: Option<String>,
    },
    CancelAgent {
        thread_id: AgentThreadId,
        reason: Option<String>,
    },
    MessageAgent {
        thread_id: AgentThreadId,
        prompt: String,
    },
    StartV2Compaction,
    PreviewV2Compaction,
    ApplyV2Compaction {
        request_id: u64,
    },
    ApplyStandaloneToolOutputShrink {
        request_id: u64,
    },
    CancelV2CompactionReview {
        request_id: u64,
    },
    CheckChangedFilesDiagnostics,
    CleanMutationArtifacts {
        target: MutationArtifactCleanupTarget,
    },
    DeleteMutationArtifact {
        artifact_id: String,
    },
    ApproveVerificationCheck {
        check_spec_id: String,
    },
    SandboxVerificationCheck {
        check_spec_id: String,
    },
    RerunTaskVerification {
        request: TaskVerificationRerunRequest,
    },
    ReviewTaskIntegration {
        request: TaskIntegrationReviewRequest,
    },
    AcceptTaskIntegration {
        request: TaskIntegrationReviewRequest,
    },
    PreviewCheckpointRestore {
        request_id: u64,
        request: ControlledCheckpointRestoreRequest,
    },
    ExecuteCheckpointRestore {
        request_id: u64,
        request: ControlledCheckpointRestoreRequest,
    },
    ForkConversationAtCheckpoint {
        request_id: u64,
        request: ControlledCheckpointRestoreRequest,
    },
    LoadIntentStack {
        request_id: u64,
    },
    PreviewIntentDrop {
        request_id: u64,
        intent_ref: IntentVersionRef,
    },
    ExecuteIntentDrop {
        request_id: u64,
        request: IntentDropRequestV1,
    },
    LoadConversationForkPoints {
        request_id: u64,
        source_session_id: String,
    },
    ForkConversation {
        request_id: u64,
        source_session_id: String,
        source_turn_digest: String,
        target_model_ref: sigil_kernel::ModelRef,
    },
    InspectLocalSession {
        request_id: u64,
        source_path: PathBuf,
    },
    ReadToolArtifactPage {
        request_id: u64,
        artifact_ref: sigil_kernel::ToolArtifactRefV1,
        selector: sigil_kernel::ToolArtifactSelectorV1,
    },
    ForkLocalSession {
        request_id: u64,
        source_path: PathBuf,
        current_model_route: ResolvedModelRoute,
    },
    ExportLocalSession {
        request_id: u64,
        source_path: PathBuf,
    },
    SetLocalSessionPin {
        request_id: u64,
        source_path: PathBuf,
        pinned: bool,
    },
    PreviewLocalSessionDelete {
        request_id: u64,
        source_path: PathBuf,
    },
    ApplyLocalSessionDelete {
        request_id: u64,
        preview: SessionDeletePreview,
    },
    PreviewSessionRetention {
        request_id: u64,
        policy: SessionRetentionPolicy,
    },
    ApplySessionRetention {
        request_id: u64,
        preview: SessionRetentionPreview,
    },
    RefreshProviderBalance {
        request_id: u64,
        provider_config: ProviderStatusConfig,
    },
    RefreshProviderModels {
        request_id: u64,
        provider_config: ProviderStatusConfig,
    },
    RefreshConnectionModels {
        cache_root: PathBuf,
        root_config: Box<sigil_kernel::RootConfig>,
        request: ModelCatalogRequest,
        prepared_credential: Option<PreparedCredential>,
    },
    CancelProviderModelsRefresh {
        request_id: u64,
    },
    ActivateLazyMcp {
        server_name: Option<String>,
    },
    RefreshMcpServer {
        server_name: String,
    },
    McpOAuth {
        server_name: String,
        action: McpOAuthUserAction,
    },
    StartNewSession {
        session_log_path: PathBuf,
    },
    SwitchSession {
        session_log_path: PathBuf,
        attachment_recovery_binding: Option<String>,
    },
    Shutdown,
}

/// Exact immutable owner identity required to stop one persistent terminal task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalTaskControlIdentity {
    pub session_scope_id: String,
    pub run_id: String,
    pub task_id: String,
    pub expected_generation: u64,
}

pub(in crate::runner) fn is_urgent_worker_command(command: &WorkerCommand) -> bool {
    if let WorkerCommand::ApplicationDispatch { command, .. } = command {
        return is_urgent_worker_command(command);
    }
    if matches!(
        command,
        WorkerCommand::PrepareApplicationOperation { .. }
            | WorkerCommand::QueryApplicationOperation { .. }
            | WorkerCommand::FindCommittedApplicationOperation { .. }
    ) {
        return true;
    }
    matches!(
        command,
        WorkerCommand::ApprovalCommand(_)
            | WorkerCommand::PauseTask { .. }
            | WorkerCommand::CancelRun
            | WorkerCommand::CancelTerminalTask { .. }
            | WorkerCommand::CloseAgent { .. }
            | WorkerCommand::CancelAgent { .. }
            | WorkerCommand::Shutdown
    )
}

/// In-memory bridge to capabilities issued by the actual run owners. No path or cached ID can
/// create a stop target. Binding and closing share one lock, including runs admitted concurrently
/// with shutdown, and no durable operation holds this lock.
#[derive(Clone, Default)]
pub(in crate::runner) struct WorkerStopControl(Arc<std::sync::Mutex<WorkerStopState>>);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum WorkerShutdownStage {
    #[default]
    WorkerLoop,
    OwnedThreadJoin,
    CancellationRequest,
    RunQuiescence,
    CancellationFinalization,
    SessionReload,
    Compaction,
    ArtifactGc,
    McpOAuth,
    ProviderStatus,
    SessionMaintenance,
    TerminalTasks,
    Runtime,
}

impl WorkerShutdownStage {
    fn label(self) -> &'static str {
        match self {
            Self::WorkerLoop => "worker-loop",
            Self::OwnedThreadJoin => "owned-thread-join",
            Self::CancellationRequest => "cancellation-request-persist",
            Self::RunQuiescence => "run-quiescence",
            Self::CancellationFinalization => "cancellation-finalization-persist",
            Self::SessionReload => "session-reload",
            Self::Compaction => "compaction-drain",
            Self::ArtifactGc => "artifact-gc-drain",
            Self::McpOAuth => "mcp-oauth-drain",
            Self::ProviderStatus => "provider-status-drain",
            Self::SessionMaintenance => "session-maintenance-drain",
            Self::TerminalTasks => "terminal-tasks-drain",
            Self::Runtime => "runtime-drain",
        }
    }
}

struct ObservedRunStop {
    capability: sigil_kernel::RunStopCapability,
    handle: sigil_kernel::RunCancellationHandle,
}

#[derive(Default)]
struct WorkerStopState {
    closing: bool,
    stop_generation: u64,
    dispatch_generation: Option<u64>,
    dispatch_run_observed: Option<Arc<std::sync::atomic::AtomicBool>>,
    active: Option<ObservedRunStop>,
    retired: Vec<ObservedRunStop>,
    started: Option<std::time::Instant>,
    stage: WorkerShutdownStage,
    failed_stage: Option<WorkerShutdownStage>,
    owned_threads_joined: bool,
    stage_started: Option<std::time::Instant>,
    stage_timings: [std::time::Duration; 13],
}

impl WorkerStopControl {
    pub(in crate::runner) fn bind(&self, owner: &sigil_kernel::RunCancellationOwner) {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let capability = owner.stop_capability();
        if state.closing
            || state
                .dispatch_generation
                .is_some_and(|generation| generation != state.stop_generation)
        {
            capability.reserve();
        }
        // A later admission cannot erase an earlier owner's incomplete cleanup evidence.
        state
            .retired
            .retain(|run| !run.capability.cleanup_complete());
        if let Some(previous) = state.active.take()
            && !previous.capability.cleanup_complete()
        {
            state.retired.push(previous);
        }
        state.active = Some(ObservedRunStop {
            capability,
            handle: owner.handle(),
        });
        if let Some(observed) = &state.dispatch_run_observed {
            observed.store(true, std::sync::atomic::Ordering::Release);
        }
    }

    pub(in crate::runner) fn reserve(&self, closing: bool) {
        if closing {
            self.begin_shutdown();
            return;
        }
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.stop_generation = state.stop_generation.saturating_add(1);
        if let Some(run) = &state.active {
            run.capability.reserve();
        }
    }

    pub(in crate::runner) fn begin_shutdown(&self) {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.closing = true;
        state.started.get_or_insert_with(std::time::Instant::now);
        state
            .stage_started
            .get_or_insert_with(std::time::Instant::now);
        for run in state.active.iter().chain(&state.retired) {
            run.capability.reserve();
        }
    }

    pub(in crate::runner) fn stage(&self, stage: WorkerShutdownStage) {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.stage != stage {
            if let Some(started) = state.stage_started {
                let index = state.stage as usize;
                state.stage_timings[index] += started.elapsed();
                state.stage_started = Some(std::time::Instant::now());
            }
            state.stage = stage;
        }
    }

    pub(in crate::runner) fn fail_stage(&self, stage: WorkerShutdownStage) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .failed_stage
            .get_or_insert(stage);
    }

    fn active_counts(&self) -> (usize, usize) {
        let state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state
            .active
            .iter()
            .chain(&state.retired)
            .map(|run| (run.handle.active_effects(), run.handle.active_tasks()))
            .fold((0, 0), |(effects, tasks), (e, t)| (effects + e, tasks + t))
    }

    fn shutdown_diagnostic(&self, owned_thread: &str) -> String {
        let state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (effects, tasks) = state
            .active
            .iter()
            .chain(&state.retired)
            .map(|run| (run.handle.active_effects(), run.handle.active_tasks()))
            .fold((0, 0), |(effects, tasks), (e, t)| (effects + e, tasks + t));
        let elapsed_ms = state
            .started
            .map_or(0, |started| started.elapsed().as_millis());
        let stage_elapsed_ms = state
            .stage_started
            .map_or(0, |started| started.elapsed().as_millis());
        let complete = state.owned_threads_joined
            && state.failed_stage.is_none()
            && state
                .active
                .iter()
                .chain(&state.retired)
                .all(|run| run.capability.cleanup_complete());
        let stages = [
            WorkerShutdownStage::WorkerLoop,
            WorkerShutdownStage::OwnedThreadJoin,
            WorkerShutdownStage::CancellationRequest,
            WorkerShutdownStage::RunQuiescence,
            WorkerShutdownStage::CancellationFinalization,
            WorkerShutdownStage::SessionReload,
            WorkerShutdownStage::Compaction,
            WorkerShutdownStage::ArtifactGc,
            WorkerShutdownStage::McpOAuth,
            WorkerShutdownStage::ProviderStatus,
            WorkerShutdownStage::SessionMaintenance,
            WorkerShutdownStage::TerminalTasks,
            WorkerShutdownStage::Runtime,
        ];
        let timings = stages
            .iter()
            .filter_map(|stage| {
                let mut duration = state.stage_timings[*stage as usize];
                if *stage == state.stage
                    && let Some(started) = state.stage_started
                {
                    duration += started.elapsed();
                }
                (!duration.is_zero())
                    .then(|| format!("{}:{}ms", stage.label(), duration.as_millis()))
            })
            .collect::<Vec<_>>()
            .join(",");
        let mut execution = std::collections::BTreeMap::new();
        for run in state.active.iter().chain(&state.retired) {
            for stage in run.capability.cleanup_progress() {
                let entry = execution
                    .entry(stage.stage.as_str())
                    .or_insert((0_u64, 0_u64, 0_u64));
                entry.0 = entry.0.saturating_add(stage.elapsed_ms);
                entry.1 = entry.1.saturating_add(stage.active_elapsed_ms);
                entry.2 = entry.2.saturating_add(stage.active as u64);
            }
        }
        let execution = execution
            .into_iter()
            .map(|(stage, (elapsed, active_elapsed, active))| {
                format!("{stage}:{elapsed}ms+{active_elapsed}ms(active={active})")
            })
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "owned_thread={owned_thread}; stage={}; elapsed_ms={elapsed_ms}; stage_elapsed_ms={stage_elapsed_ms}; active_effects={effects}; active_tasks={tasks}; cleanup_complete={complete}; stage_timings=[{timings}]; execution_timings=[{execution}]",
            state.failed_stage.unwrap_or(state.stage).label()
        )
    }

    fn cleanup_complete(&self) -> bool {
        let state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.failed_stage.is_none()
            && state
                .active
                .iter()
                .chain(&state.retired)
                .all(|run| run.capability.cleanup_complete())
    }

    pub(in crate::runner) fn is_closing(&self) -> bool {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .closing
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerApplicationDispatchOutcome {
    Dispatched,
    CancelledBeforeDispatch,
}

/// Freezes the stop frontier before asynchronous admission. It grants no execution authority;
/// the application still admits the command and the worker binds the actual run owner.
#[derive(Clone)]
pub struct WorkerRunAdmission {
    control: WorkerStopControl,
    generation: u64,
    run_observed: Arc<std::sync::atomic::AtomicBool>,
}

impl fmt::Debug for WorkerRunAdmission {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkerRunAdmission")
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl WorkerRunAdmission {
    #[cfg(test)]
    pub(crate) fn bind_test_owner(&self, owner: &sigil_kernel::RunCancellationOwner) {
        let _dispatch = self.enter().expect("fixture admission must be current");
        self.control.bind(owner);
    }
    pub(crate) fn run_observed(&self) -> bool {
        self.run_observed.load(std::sync::atomic::Ordering::Acquire)
    }
    pub(in crate::runner) fn enter(&self) -> Option<WorkerRunAdmissionDispatch> {
        let mut state = self
            .control
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.closing || state.stop_generation != self.generation {
            return None;
        }
        let previous = state.dispatch_generation.replace(self.generation);
        let previous_observed = state
            .dispatch_run_observed
            .replace(Arc::clone(&self.run_observed));
        Some(WorkerRunAdmissionDispatch {
            control: self.control.clone(),
            previous,
            previous_observed,
        })
    }
}

pub(in crate::runner) struct WorkerRunAdmissionDispatch {
    control: WorkerStopControl,
    previous: Option<u64>,
    previous_observed: Option<Arc<std::sync::atomic::AtomicBool>>,
}

impl Drop for WorkerRunAdmissionDispatch {
    fn drop(&mut self) {
        let mut state = self
            .control
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.dispatch_generation = self.previous;
        state.dispatch_run_observed = self.previous_observed.take();
    }
}

/// Cloneable public command handle backed by the worker's unified event inbox.
#[derive(Clone)]
pub struct WorkerCommandSender {
    inner: Arc<WorkerCommandSenderInner>,
}

struct WorkerCommandSenderInner {
    stop_control: WorkerStopControl,
    sink: WorkerCommandSink,
}

enum WorkerCommandSink {
    Event {
        event_tx: mpsc::Sender<WorkerEvent>,
        urgent_tx: mpsc::Sender<WorkerCommand>,
    },
    #[cfg(test)]
    Direct(mpsc::Sender<WorkerCommand>),
}

impl WorkerCommandSender {
    pub(crate) fn reserve_run_admission(&self) -> WorkerRunAdmission {
        let state = self
            .inner
            .stop_control
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        WorkerRunAdmission {
            control: self.inner.stop_control.clone(),
            generation: state.stop_generation,
            run_observed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }
    pub(in crate::runner) fn new(
        event_tx: mpsc::Sender<WorkerEvent>,
        urgent_tx: mpsc::Sender<WorkerCommand>,
    ) -> Self {
        Self {
            inner: Arc::new(WorkerCommandSenderInner {
                stop_control: WorkerStopControl::default(),
                sink: WorkerCommandSink::Event {
                    event_tx,
                    urgent_tx,
                },
            }),
        }
    }

    pub(in crate::runner) fn stop_control(&self) -> WorkerStopControl {
        self.inner.stop_control.clone()
    }

    pub(crate) fn cleanup_complete(&self) -> bool {
        self.inner.stop_control.cleanup_complete()
    }

    pub(crate) fn reserve_stop(&self, closing: bool) {
        self.inner.stop_control.reserve(closing);
    }

    pub(crate) fn begin_shutdown(&self) {
        self.inner.stop_control.begin_shutdown();
    }

    pub(crate) fn shutdown_active_counts(&self) -> (usize, usize) {
        self.inner.stop_control.active_counts()
    }

    pub(crate) fn shutdown_diagnostic(&self, owned_thread: &str) -> String {
        self.inner.stop_control.shutdown_diagnostic(owned_thread)
    }

    pub(crate) fn record_shutdown_joins_complete(&self) {
        let mut state = self
            .inner
            .stop_control
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.owned_threads_joined = true;
        if let Some(started) = state.stage_started.take() {
            let index = state.stage as usize;
            state.stage_timings[index] += started.elapsed();
        }
    }

    pub(crate) fn record_shutdown_join_panic(&self) {
        self.inner
            .stop_control
            .fail_stage(WorkerShutdownStage::OwnedThreadJoin);
    }

    pub fn send(&self, command: WorkerCommand) -> Result<(), WorkerCommandSendError> {
        let payload = match &command {
            WorkerCommand::ApplicationDispatch { command, .. } => command.as_ref(),
            other => other,
        };
        if matches!(
            payload,
            WorkerCommand::Shutdown | WorkerCommand::CancelRun | WorkerCommand::PauseTask { .. }
        ) {
            self.reserve_stop(matches!(payload, WorkerCommand::Shutdown));
        } else if !is_urgent_worker_command(&command) && self.inner.stop_control.is_closing() {
            return Err(WorkerCommandSendError(Box::new(command)));
        }
        match &self.inner.sink {
            WorkerCommandSink::Event {
                event_tx,
                urgent_tx,
            } if is_urgent_worker_command(&command) => {
                urgent_tx
                    .send(command)
                    .map_err(|error| WorkerCommandSendError(Box::new(error.0)))?;
                // The control command is already durably owned by the independent lane. This
                // token only releases an idle worker blocked on the ordinary event inbox.
                let _ = event_tx.send(WorkerEvent::ControlWake);
                Ok(())
            }
            WorkerCommandSink::Event { event_tx, .. } => {
                match event_tx.send(WorkerEvent::Command(command)) {
                    Ok(()) => Ok(()),
                    Err(mpsc::SendError(WorkerEvent::Command(command))) => {
                        Err(WorkerCommandSendError(Box::new(command)))
                    }
                    Err(_) => unreachable!("command sender only publishes command events"),
                }
            }
            #[cfg(test)]
            WorkerCommandSink::Direct(command_tx) => command_tx
                .send(command)
                .map_err(|error| WorkerCommandSendError(Box::new(error.0))),
        }
    }

    #[cfg(test)]
    pub(crate) fn test_channel() -> (Self, mpsc::Receiver<WorkerCommand>) {
        let (command_tx, command_rx) = mpsc::channel();
        (
            Self {
                inner: Arc::new(WorkerCommandSenderInner {
                    stop_control: WorkerStopControl::default(),
                    sink: WorkerCommandSink::Direct(command_tx),
                }),
            },
            command_rx,
        )
    }
}

impl fmt::Debug for WorkerCommandSender {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkerCommandSender")
            .finish_non_exhaustive()
    }
}

impl Drop for WorkerCommandSenderInner {
    fn drop(&mut self) {
        match &self.sink {
            WorkerCommandSink::Event {
                event_tx,
                urgent_tx,
            } => {
                let _ = urgent_tx.send(WorkerCommand::Shutdown);
                let _ = event_tx.send(WorkerEvent::ControlWake);
            }
            #[cfg(test)]
            WorkerCommandSink::Direct(_) => {}
        }
    }
}

#[derive(Debug)]
pub struct WorkerCommandSendError(pub Box<WorkerCommand>);

impl fmt::Display for WorkerCommandSendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("worker command receiver is disconnected")
    }
}

impl std::error::Error for WorkerCommandSendError {}

#[derive(Debug)]
pub enum WorkerMessage {
    /// One source attachment per run; provider deltas remain in its bounded latest slots.
    LivePreviewSource {
        source: sigil_runtime::RuntimeLivePreviewSource,
    },
    LivePreviewDurableFrontier {
        session_id: String,
        run_id: String,
        sequence: u64,
    },
    WorkerReady,
    RuntimeReady(sigil_kernel::SessionRuntimeReadyV1),
    SessionAttachmentTransferred {
        session_log_path: PathBuf,
        attachment:
            Arc<sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease>,
    },
    SessionRouteRecoveryRequired {
        code: PublicRouteRecoveryCode,
        actions: Vec<PublicRouteRecoveryAction>,
        recovery_binding: String,
        retryable: bool,
        target_session: Option<WorkerRouteRecoverySessionTarget>,
    },
    Event(Box<RunEvent>),
    ApprovalCommandReceipt(WorkerApprovalCommandReceipt),
    LocalOperationOutcome(LocalOperationOutcome),
    Notice(String),
    RunStarted {
        prompt: String,
    },
    SkillRunStarted {
        skill_id: String,
        prompt: String,
    },
    PlanRunStarted {
        prompt: String,
    },
    AgentRunStarted {
        profile_id: String,
        prompt: String,
    },
    AgentResultContinuationStarted {
        thread_ids: Vec<AgentThreadId>,
    },
    ConversationQueueUpdated {
        items: Vec<ConversationQueueItemProjection>,
        paused: bool,
        entries: Vec<SessionLogEntry>,
    },
    ConversationQueueOperationCompleted {
        session_log_path: PathBuf,
        operation: QueueOperation,
        result: Result<(), QueueOperationFailure>,
    },
    ConversationQueueDispatchStarted {
        queue_id: ConversationInputQueueId,
        prompt: String,
    },
    AgentThreadEvent {
        thread_id: AgentThreadId,
        event: Box<RunEvent>,
    },
    AgentThreadStatusLive {
        entry: AgentThreadStatusChangedEntry,
    },
    AgentRunFinished {
        profile_id: String,
        result: AgentRunResult,
        entries: Vec<SessionLogEntry>,
    },
    TaskRunStarted {
        task_id: String,
        objective: String,
    },
    TaskProviderRouteDiagnosticsUpdated {
        snapshot: TaskProviderRouteDiagnosticsSnapshot,
    },
    RunFinished {
        result: AgentRunResult,
        entries: Vec<SessionLogEntry>,
    },
    PlanRunFinished {
        result: AgentRunResult,
        entries: Vec<SessionLogEntry>,
    },
    /// A plan-review-local terminal that deliberately does not own the foreground run's
    /// generic failure state. The durable attempt remains the source of truth.
    PlanReviewBlocked {
        reason: String,
        paused: bool,
        entries: Vec<SessionLogEntry>,
    },
    UserInputRequested {
        request: sigil_kernel::PublicUserInputRequestV1,
        entries: Vec<SessionLogEntry>,
    },
    /// Private worker-to-App handoff for an already accepted durable input.  It is never
    /// serialized into a public projection or emitted across a transport boundary; the App only
    /// uses it to select the existing Resume action for the matching public request.
    RecoveredUserInputAttention {
        command: sigil_kernel::UserInputDecisionCommandV1,
        entries: Vec<SessionLogEntry>,
    },
    UserInputDecisionApplied {
        request: sigil_kernel::PublicUserInputRequestV1,
        continuation_started: bool,
        entries: Vec<SessionLogEntry>,
    },
    UserInputDecisionFailed {
        request_id: String,
        generation: u32,
        expected_request_hash: String,
        message: String,
        /// Latest owner entries after a failed preparation may contain a durable resolution.
        entries: Option<Vec<SessionLogEntry>>,
    },
    PlanRejected {
        entry: PlanDecisionRecordedEntry,
        entries: Vec<SessionLogEntry>,
    },
    PlanSaved {
        entry: PlanDecisionRecordedEntry,
        entries: Vec<SessionLogEntry>,
    },
    PlanActionFailed {
        action: sigil_kernel::PublicPlanAction,
        plan_id: String,
        expected_plan_hash: String,
        message: String,
        entries: Option<Vec<SessionLogEntry>>,
    },
    TaskCreatedFromPlan {
        entry: TaskCreatedFromPlanEntry,
        start_mode: PlanTaskStartMode,
        entries: Vec<SessionLogEntry>,
    },
    TaskRunFinished {
        task_id: String,
        status: TaskRunStatus,
        entries: Vec<SessionLogEntry>,
    },
    RunCancellationRequested,
    TaskPauseRequested {
        task_id: String,
    },
    TaskRunPaused {
        session_id: String,
        task_id: String,
        session_log_path: PathBuf,
        provider_name: String,
        model_name: String,
        entries: Vec<SessionLogEntry>,
    },
    RunCancelled {
        session_id: String,
        session_log_path: PathBuf,
        provider_name: String,
        model_name: String,
        entries: Vec<SessionLogEntry>,
    },
    RunInterrupted {
        session_id: String,
        session_log_path: PathBuf,
        provider_name: String,
        model_name: String,
        reason: String,
        entries: Vec<SessionLogEntry>,
    },
    TerminalTaskUpdated {
        identity: TerminalTaskControlIdentity,
        entry: TerminalTaskEntry,
        entries: Vec<SessionLogEntry>,
    },
    AgentThreadClosed {
        thread_id: AgentThreadId,
        entries: Vec<SessionLogEntry>,
    },
    AgentThreadCancelled {
        thread_id: AgentThreadId,
        entries: Vec<SessionLogEntry>,
    },
    SessionSwitched {
        session_id: String,
        session_log_path: PathBuf,
        provider_name: String,
        model_name: String,
        entries: Vec<SessionLogEntry>,
    },
    NewSessionStarted {
        session_id: String,
        session_log_path: PathBuf,
        provider_name: String,
        model_name: String,
        entries: Vec<SessionLogEntry>,
    },
    V2CompactionPreviewed {
        state: V2CompactionPreviewState,
    },
    V2CompactionApplied {
        request_id: u64,
        source: V2CompactionApplySource,
        compaction_id: String,
        folded_event_count: usize,
        entries: Vec<SessionLogEntry>,
    },
    StandaloneToolOutputShrinkApplied {
        request_id: u64,
        context_epoch_id: String,
        projected_output_count: usize,
        entries: Vec<SessionLogEntry>,
    },
    V2CompactionApplyFailed {
        request_id: u64,
        error: String,
    },
    CheckpointRestorePreviewed {
        request_id: u64,
        preview: ControlledCheckpointRestorePreview,
    },
    TaskIntegrationReviewLoaded {
        request: TaskIntegrationReviewRequest,
        aggregate_diff: String,
    },
    TaskIntegrationReviewFailed {
        request: TaskIntegrationReviewRequest,
        error: String,
    },
    TaskIntegrationAccepted {
        request: TaskIntegrationReviewRequest,
        promotion_status: sigil_kernel::IntegrationPromotionStatus,
        parent_verdict: Option<sigil_kernel::VerificationVerdict>,
        entries: Vec<SessionLogEntry>,
    },
    TaskIntegrationAcceptanceFailed {
        request: TaskIntegrationReviewRequest,
        error: String,
        entries: Vec<SessionLogEntry>,
    },
    CheckpointRestoreCompleted {
        request_id: u64,
        preview: ControlledCheckpointRestorePreview,
        batch_id: String,
        entries: Vec<SessionLogEntry>,
    },
    IntentStackLoaded {
        request_id: u64,
        stack_state: PublicIntentStackStateV1,
    },
    IntentDropPreviewed {
        request_id: u64,
        preview: IntentOperationPreviewV1,
    },
    IntentDropCompleted {
        request_id: u64,
        execution: IntentOperationExecutionV1,
        stack_state: PublicIntentStackStateV1,
        entries: Vec<SessionLogEntry>,
    },
    IntentStackOperationFailed {
        request_id: u64,
        error: String,
    },
    ConversationForked {
        session_id: String,
        request_id: u64,
        session_log_path: PathBuf,
        provider_name: String,
        model_name: String,
        copied_message_count: usize,
        entries: Vec<SessionLogEntry>,
    },
    ConversationForkPointsLoaded {
        request_id: u64,
        source_session_id: String,
        points: Vec<sigil_runtime::application_recovery::ApplicationConversationForkPointView>,
    },
    LocalSessionInspected {
        request_id: u64,
        entry: LocalSessionCatalogEntry,
    },
    ToolArtifactPageRead {
        request_id: u64,
        page: sigil_kernel::ToolArtifactPageV1,
        entries: Vec<SessionLogEntry>,
    },
    ToolArtifactPageReadFailed {
        request_id: u64,
        artifact_ref: sigil_kernel::ToolArtifactRefV1,
        failure: ToolArtifactDisplayReadFailure,
        entries: Vec<SessionLogEntry>,
    },
    LocalSessionForked {
        session_id: String,
        request_id: u64,
        session_log_path: PathBuf,
        provider_name: String,
        model_name: String,
        copied_message_count: usize,
        entries: Vec<SessionLogEntry>,
    },
    LocalSessionExported {
        request_id: u64,
        output: SessionExportOutput,
    },
    LocalSessionPinChanged {
        request_id: u64,
        entry: LocalSessionCatalogEntry,
    },
    LocalSessionDeletePreviewed {
        request_id: u64,
        preview: SessionDeletePreview,
    },
    LocalSessionDeleted {
        request_id: u64,
        output: SessionDeleteOutput,
    },
    SessionRetentionPreviewed {
        request_id: u64,
        preview: SessionRetentionPreview,
    },
    SessionRetentionApplied {
        request_id: u64,
        output: SessionRetentionOutput,
    },
    LocalSessionLifecycleFailed {
        request_id: u64,
        error: String,
    },
    CheckpointOperationFailed {
        request_id: u64,
        error: String,
    },
    McpActivationStatus {
        server_name: Option<String>,
        status: McpActivationStatus,
    },
    McpOAuthStatus {
        status: sigil_runtime::McpOAuthAuthStatus,
        revocation: Option<sigil_runtime::McpOAuthRevocationOutcome>,
    },
    McpProgress {
        notification: McpProgressNotification,
    },
    McpListChanged {
        notification: McpListChangedNotification,
    },
    ProviderBalanceRefreshed {
        request_id: u64,
        snapshot: BalanceSnapshot,
    },
    ProviderModelsRefreshed {
        request_id: u64,
        base_url: String,
        result: Result<Vec<String>, String>,
    },
    ConnectionModelsRefreshed {
        result: ModelCatalogResult,
    },
    McpElicitationRequest {
        request: McpElicitationRequest,
        response_tx: McpElicitationResponseTx,
    },
    EgressDisclosureRequested {
        disclosure: PreEgressDisclosure,
        receipt_tx: EgressDisclosureReceiptTx,
    },
    RunFailed(String),
}

/// A bounded outcome for a user-triggered operation that does not own run terminal authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalOperationOutcome {
    pub operation_id: String,
    pub kind: LocalOperationKind,
    pub status: LocalOperationStatus,
    pub retryable: bool,
    pub safe_summary: String,
}

impl LocalOperationOutcome {
    pub(crate) fn rejected(
        operation_id: impl Into<String>,
        kind: LocalOperationKind,
        safe_summary: impl Into<String>,
    ) -> Self {
        Self::new(
            operation_id,
            kind,
            LocalOperationStatus::Rejected,
            false,
            safe_summary,
        )
    }

    pub(crate) fn deferred(
        operation_id: impl Into<String>,
        kind: LocalOperationKind,
        safe_summary: impl Into<String>,
    ) -> Self {
        Self::new(
            operation_id,
            kind,
            LocalOperationStatus::Deferred,
            true,
            safe_summary,
        )
    }

    pub(crate) fn failed(
        operation_id: impl Into<String>,
        kind: LocalOperationKind,
        retryable: bool,
        safe_summary: impl Into<String>,
    ) -> Self {
        Self::new(
            operation_id,
            kind,
            LocalOperationStatus::Failed,
            retryable,
            safe_summary,
        )
    }

    fn new(
        operation_id: impl Into<String>,
        kind: LocalOperationKind,
        status: LocalOperationStatus,
        retryable: bool,
        safe_summary: impl Into<String>,
    ) -> Self {
        Self {
            operation_id: operation_id.into(),
            kind,
            status,
            retryable,
            safe_summary: safe_summary.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalOperationKind {
    McpActivation,
    McpRefresh,
    McpAuthentication,
    ChangedFilesDiagnostics,
    MutationArtifactCleanup,
    MutationArtifactDeletion,
    VerificationCheckApproval,
    VerificationCheckSandboxing,
    TaskVerificationRerun,
}

impl LocalOperationKind {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::McpActivation => "MCP activation",
            Self::McpRefresh => "MCP refresh",
            Self::McpAuthentication => "MCP authentication",
            Self::ChangedFilesDiagnostics => "changed-files diagnostics",
            Self::MutationArtifactCleanup => "mutation artifact cleanup",
            Self::MutationArtifactDeletion => "mutation artifact deletion",
            Self::VerificationCheckApproval => "verification check approval",
            Self::VerificationCheckSandboxing => "verification check sandboxing",
            Self::TaskVerificationRerun => "task verification rerun",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalOperationStatus {
    Succeeded,
    Rejected,
    Deferred,
    Retrying,
    Failed,
}

impl LocalOperationStatus {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Rejected => "rejected",
            Self::Deferred => "deferred",
            Self::Retrying => "retrying",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone)]
pub struct WorkerRouteRecoverySessionTarget {
    pub(crate) session_id: String,
    pub(crate) session_log_path: PathBuf,
    pub(crate) provider_name: String,
    pub(crate) model_name: String,
    pub(crate) entries: Vec<SessionLogEntry>,
}

/// Bounded, path-free failure surface for user-initiated artifact inspection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolArtifactDisplayReadFailure {
    BudgetExhausted,
    Unavailable(sigil_kernel::ToolArtifactAvailability),
    Rejected,
    AuditUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpActivationStatus {
    Activating,
    Refreshing,
    Deferred,
    AuthenticationRequired,
    Stale {
        capability: String,
    },
    Ready {
        added_tools: usize,
        process_coverage: Option<String>,
    },
    Failed {
        error: String,
    },
}

impl McpActivationStatus {
    pub(crate) fn from_error(error: String) -> Self {
        if error.contains("remote MCP authentication is required")
            || error.contains("remote MCP OAuth authentication is required")
        {
            Self::AuthenticationRequired
        } else {
            Self::Failed { error }
        }
    }
}
