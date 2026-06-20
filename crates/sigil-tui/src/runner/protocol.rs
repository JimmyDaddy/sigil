use std::path::PathBuf;

use sigil_kernel::{
    AgentRunResult, AgentThreadId, CompactionRecord, PlanApprovalPermission, PlanApprovedEntry,
    ReasoningEffort, RunEvent, SessionLogEntry, TaskRunStatus, TerminalTaskEntry,
};
use sigil_provider_deepseek::DeepSeekProviderConfig;
use sigil_runtime::{
    McpElicitationRequest, McpElicitationResponse, McpListChangedNotification,
    McpProgressNotification,
};
use tokio::sync::oneshot;

use crate::provider_status::BalanceSnapshot;

pub(crate) type McpElicitationResponseTx = oneshot::Sender<McpElicitationResponse>;

#[derive(Debug)]
pub enum WorkerCommand {
    SubmitPrompt {
        prompt: String,
        reasoning_effort: ReasoningEffort,
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
    CancelRun,
    CancelTerminalTask {
        task_id: String,
    },
    CloseAgent {
        thread_id: AgentThreadId,
        reason: Option<String>,
    },
    CompactNow,
    CheckChangedFilesDiagnostics,
    RefreshProviderBalance {
        request_id: u64,
        provider_config: DeepSeekProviderConfig,
    },
    RefreshProviderModels {
        request_id: u64,
        provider_config: DeepSeekProviderConfig,
    },
    CancelProviderModelsRefresh {
        request_id: u64,
    },
    ActivateLazyMcp {
        server_name: Option<String>,
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
    Event(Box<RunEvent>),
    Notice(String),
    RunStarted {
        prompt: String,
    },
    PlanRunStarted {
        prompt: String,
    },
    AgentRunStarted {
        profile_id: String,
        prompt: String,
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
    TaskRunFinished {
        task_id: String,
        status: TaskRunStatus,
        entries: Vec<SessionLogEntry>,
    },
    RunCancelled {
        session_log_path: PathBuf,
        provider_name: String,
        model_name: String,
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
        record: CompactionRecord,
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
    Stale { capability: String },
    Ready { added_tools: usize },
    Failed { error: String },
}
