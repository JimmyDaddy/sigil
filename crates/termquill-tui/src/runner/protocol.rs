use std::path::PathBuf;

use termquill_kernel::{
    AgentRunResult, CompactionRecord, ReasoningEffort, RunEvent, SessionLogEntry,
};

#[derive(Debug)]
pub enum WorkerCommand {
    SubmitPrompt {
        prompt: String,
        reasoning_effort: ReasoningEffort,
    },
    ApprovalDecision {
        call_id: String,
        approved: bool,
    },
    CancelRun,
    CompactNow,
    CheckChangedFilesDiagnostics,
    ActivateLazyMcp {
        server_name: Option<String>,
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
    RunFinished {
        result: AgentRunResult,
        entries: Vec<SessionLogEntry>,
    },
    RunCancelled {
        session_log_path: PathBuf,
        provider_name: String,
        model_name: String,
        entries: Vec<SessionLogEntry>,
    },
    SessionSwitched {
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
    Deferred,
    Ready { added_tools: usize },
    Failed { error: String },
}
