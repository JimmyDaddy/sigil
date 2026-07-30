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
    MutationArtifactCleanupTarget, PlanApprovalPermission, PlanApprovedEntry,
    PlanDecisionRecordedEntry, PlanTaskStartMode, PreEgressDisclosure, PublicIntentStackStateV1,
    ReasoningEffort, ResolvedModelRoute, RunEvent, SessionLogEntry, TaskCreatedFromPlanEntry,
    TaskIntegrationReviewRequest, TaskPauseRequest, TaskRunStatus, TaskVerificationRerunRequest,
    TerminalTaskEntry, V2CompactionPreview,
};
use sigil_runtime::{
    BalanceSnapshot, LocalSessionCatalogEntry, McpElicitationRequest, McpElicitationResponse,
    McpListChangedNotification, McpProgressNotification, ProviderStatusConfig, SessionDeleteOutput,
    SessionDeletePreview, SessionExportOutput, SessionRetentionOutput, SessionRetentionPolicy,
    SessionRetentionPreview, TaskCompletionProgressSnapshot, TaskProviderRouteDiagnosticsSnapshot,
    provider_connections::{ModelCatalogRequest, ModelCatalogResult, PreparedCredential},
};
use tokio::sync::oneshot;

use super::worker_event::WorkerEvent;

pub(crate) type McpElicitationResponseTx = oneshot::Sender<McpElicitationResponse>;
pub(crate) type EgressDisclosureReceiptTx =
    oneshot::Sender<Result<DisclosurePresentationReceipt, DisclosurePresentationError>>;

pub(crate) const WORKER_COMMAND_PROTOCOL_VERSION: u16 = 1;

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
        configured_tail_message_count: usize,
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
    Decision { call_id: String, approved: bool },
    DecisionForSession { call_id: String },
    DecisionWithArgs { call_id: String, args_json: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueMoveDirection {
    Up,
    Down,
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
    SubmitPrompt {
        prompt: String,
        reasoning_effort: ReasoningEffort,
    },
    SubmitPromptWithAttachments {
        prompt: String,
        attachments: Vec<ImageAttachment>,
        reasoning_effort: ReasoningEffort,
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
    ApprovePlan {
        plan_text: String,
        permission: PlanApprovalPermission,
        scope_summary: String,
        clear_planning_context: bool,
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
    InvokeInlineSkill {
        skill_id: String,
        arguments: String,
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
    ApprovalDecision {
        call_id: String,
        approved: bool,
    },
    ApprovalSessionDecision {
        call_id: String,
    },
    ApprovalDecisionWithArgs {
        call_id: String,
        args_json: String,
    },
    ApprovalCommand(WorkerCommandEnvelope<WorkerApprovalCommand>),
    BackgroundActiveAgent,
    CancelRun,
    CancelTerminalTask {
        task_id: String,
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
    },
    Shutdown,
}

pub(in crate::runner) fn is_urgent_worker_command(command: &WorkerCommand) -> bool {
    matches!(
        command,
        WorkerCommand::ApprovalDecision { .. }
            | WorkerCommand::ApprovalSessionDecision { .. }
            | WorkerCommand::ApprovalDecisionWithArgs { .. }
            | WorkerCommand::ApprovalCommand(_)
            | WorkerCommand::PauseTask { .. }
            | WorkerCommand::CancelRun
            | WorkerCommand::CancelTerminalTask { .. }
            | WorkerCommand::CloseAgent { .. }
            | WorkerCommand::CancelAgent { .. }
            | WorkerCommand::Shutdown
    )
}

/// Cloneable public command handle backed by the worker's unified event inbox.
#[derive(Clone)]
pub struct WorkerCommandSender {
    inner: Arc<WorkerCommandSenderInner>,
}

struct WorkerCommandSenderInner {
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
    pub(in crate::runner) fn new(
        event_tx: mpsc::Sender<WorkerEvent>,
        urgent_tx: mpsc::Sender<WorkerCommand>,
    ) -> Self {
        Self {
            inner: Arc::new(WorkerCommandSenderInner {
                sink: WorkerCommandSink::Event {
                    event_tx,
                    urgent_tx,
                },
            }),
        }
    }

    pub fn send(&self, command: WorkerCommand) -> Result<(), WorkerCommandSendError> {
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
    WorkerReady,
    Event(Box<RunEvent>),
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
    TaskCompletionProgressUpdated {
        snapshot: TaskCompletionProgressSnapshot,
    },
    RunFinished {
        result: AgentRunResult,
        entries: Vec<SessionLogEntry>,
    },
    PlanRunFinished {
        result: AgentRunResult,
        entries: Vec<SessionLogEntry>,
    },
    PlanApproved {
        entry: PlanApprovedEntry,
        entries: Vec<SessionLogEntry>,
    },
    PlanRejected {
        entry: PlanDecisionRecordedEntry,
        entries: Vec<SessionLogEntry>,
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
        task_id: String,
        session_log_path: PathBuf,
        provider_name: String,
        model_name: String,
        entries: Vec<SessionLogEntry>,
    },
    RunCancelled {
        session_log_path: PathBuf,
        provider_name: String,
        model_name: String,
        entries: Vec<SessionLogEntry>,
    },
    RunInterrupted {
        session_log_path: PathBuf,
        provider_name: String,
        model_name: String,
        reason: String,
        entries: Vec<SessionLogEntry>,
    },
    TerminalTaskUpdated {
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
        session_log_path: PathBuf,
        provider_name: String,
        model_name: String,
        entries: Vec<SessionLogEntry>,
    },
    NewSessionStarted {
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
        request_id: u64,
        session_log_path: PathBuf,
        provider_name: String,
        model_name: String,
        copied_message_count: usize,
        entries: Vec<SessionLogEntry>,
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
