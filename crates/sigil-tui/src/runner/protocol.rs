use std::path::PathBuf;

use sigil_kernel::{
    AgentRunResult, AgentThreadId, AgentThreadStatusChangedEntry, CompactionRecord,
    ConversationInputKind, ConversationInputQueueId, ConversationInputTarget,
    ConversationQueueItemProjection, MutationArtifactCleanupTarget, PlanApprovalPermission,
    PlanApprovedEntry, PlanDecisionRecordedEntry, PlanTaskStartMode, ReasoningEffort, RunEvent,
    SessionLogEntry, TaskCreatedFromPlanEntry, TaskRunStatus, TerminalTaskEntry,
};
use sigil_runtime::{
    BalanceSnapshot, McpElicitationRequest, McpElicitationResponse, McpListChangedNotification,
    McpProgressNotification, ProviderStatusConfig,
};
use tokio::sync::oneshot;

pub(crate) type McpElicitationResponseTx = oneshot::Sender<McpElicitationResponse>;

pub(crate) const WORKER_COMMAND_PROTOCOL_VERSION: u16 = 1;

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

#[derive(Debug)]
pub enum WorkerCommand {
    SubmitPrompt {
        prompt: String,
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
    CompactNow,
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
    RefreshProviderBalance {
        request_id: u64,
        provider_config: ProviderStatusConfig,
    },
    RefreshProviderModels {
        request_id: u64,
        provider_config: ProviderStatusConfig,
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
    StartNewSession {
        session_log_path: PathBuf,
    },
    SwitchSession {
        session_log_path: PathBuf,
    },
    Shutdown,
}

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
    SessionCompacted {
        session_log_path: PathBuf,
        provider_name: String,
        model_name: String,
        record: Box<CompactionRecord>,
        trigger: CompactionTrigger,
        entries: Vec<SessionLogEntry>,
    },
    McpActivationStatus {
        server_name: Option<String>,
        status: McpActivationStatus,
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
    McpElicitationRequest {
        request: McpElicitationRequest,
        response_tx: McpElicitationResponseTx,
    },
    RunFailed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionTrigger {
    Manual,
    AutomaticHardThreshold,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpActivationStatus {
    Activating,
    Refreshing,
    Deferred,
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
