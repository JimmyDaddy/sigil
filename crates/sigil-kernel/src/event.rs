use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    ControlEntry, ModelMessage, ProviderContinuationState, ToolCall, ToolPreview, ToolResult,
    ToolSpec, ToolSubject, UsageStats,
};

/// Current schema version for public run events consumed by external adapters.
pub const PUBLIC_RUN_EVENT_SCHEMA_VERSION: u32 = 1;

/// Structured runtime events emitted by the agent loop for UI, logging, and orchestration.
#[derive(Debug, Clone)]
pub enum RunEvent {
    TextDelta(String),
    ReasoningDelta(String),
    ToolCallStarted(ToolCall),
    ToolCallArgsDelta {
        id: String,
        delta: String,
    },
    ToolCallCompleted(ToolCall),
    ToolApprovalRequested {
        call: ToolCall,
        spec: ToolSpec,
        subjects: Vec<ToolSubject>,
        preview: Option<ToolPreview>,
    },
    ToolApprovalResolved {
        call_id: String,
        approved: bool,
        reason: Option<String>,
    },
    ToolResult(ToolResult),
    Usage(UsageStats),
    ContinuationState(ProviderContinuationState),
    Control(ControlEntry),
    AssistantMessage(ModelMessage),
    Notice(String),
}

/// Stable, versioned event envelope for TUI, CLI, HTTP, and future adapter surfaces.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PublicRunEvent {
    pub schema_version: u32,
    pub session_id: String,
    pub run_id: String,
    pub sequence: u64,
    pub event: PublicRunEventKind,
}

impl PublicRunEvent {
    /// Creates a public run event with the current schema version.
    pub fn new(
        session_id: impl Into<String>,
        run_id: impl Into<String>,
        sequence: u64,
        event: PublicRunEventKind,
    ) -> Self {
        Self {
            schema_version: PUBLIC_RUN_EVENT_SCHEMA_VERSION,
            session_id: session_id.into(),
            run_id: run_id.into(),
            sequence,
            event,
        }
    }

    /// Projects one internal run event into the stable public envelope.
    pub fn from_run_event(
        session_id: impl Into<String>,
        run_id: impl Into<String>,
        sequence: u64,
        event: RunEvent,
    ) -> Self {
        Self::new(session_id, run_id, sequence, event.into())
    }
}

/// Public event payloads exposed to external run consumers.
///
/// Lifecycle events are owned by adapters because the kernel's internal [`RunEvent`] stream only
/// represents events produced inside an already-running agent loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PublicRunEventKind {
    RunStarted {
        prompt: String,
    },
    TaskRunStarted {
        task_id: String,
        objective: String,
    },
    RunFinished {
        final_text: String,
    },
    TaskRunFinished {
        task_id: String,
        status: String,
    },
    RunFailed {
        error: String,
    },
    RunCancelled,
    TextDelta {
        text: String,
    },
    ReasoningDelta {
        text: String,
    },
    ToolCallStarted {
        call: ToolCall,
    },
    ToolCallArgsDelta {
        id: String,
        delta: String,
    },
    ToolCallCompleted {
        call: ToolCall,
    },
    ApprovalRequested {
        call: ToolCall,
        spec: ToolSpec,
        subjects: Vec<ToolSubject>,
        preview: Option<ToolPreview>,
    },
    ApprovalResolved {
        call_id: String,
        approved: bool,
        reason: Option<String>,
    },
    ToolResult {
        result: ToolResult,
    },
    Usage {
        usage: UsageStats,
    },
    ContinuationState {
        state: ProviderContinuationState,
    },
    Control {
        control: PublicControlEvent,
    },
    AssistantMessage {
        message: PublicAssistantMessage,
    },
    Notice {
        message: String,
    },
}

/// Public projection of a control-plane event.
///
/// The `kind` field is the stable routing surface. `payload` is an opaque JSON projection for
/// adapters that need diagnostic detail before a dedicated public event variant exists.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PublicControlEvent {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
}

impl From<ControlEntry> for PublicControlEvent {
    fn from(entry: ControlEntry) -> Self {
        let kind = control_entry_kind(&entry).to_owned();
        let payload = serde_json::to_value(&entry).ok();
        Self { kind, payload }
    }
}

/// Public projection of a completed assistant message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PublicAssistantMessage {
    pub id: String,
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
}

impl From<ModelMessage> for PublicAssistantMessage {
    fn from(message: ModelMessage) -> Self {
        Self {
            id: message.id,
            content: message.content,
            tool_calls: message.tool_calls,
        }
    }
}

impl From<RunEvent> for PublicRunEventKind {
    fn from(event: RunEvent) -> Self {
        match event {
            RunEvent::TextDelta(text) => Self::TextDelta { text },
            RunEvent::ReasoningDelta(text) => Self::ReasoningDelta { text },
            RunEvent::ToolCallStarted(call) => Self::ToolCallStarted { call },
            RunEvent::ToolCallArgsDelta { id, delta } => Self::ToolCallArgsDelta { id, delta },
            RunEvent::ToolCallCompleted(call) => Self::ToolCallCompleted { call },
            RunEvent::ToolApprovalRequested {
                call,
                spec,
                subjects,
                preview,
            } => Self::ApprovalRequested {
                call,
                spec,
                subjects,
                preview,
            },
            RunEvent::ToolApprovalResolved {
                call_id,
                approved,
                reason,
            } => Self::ApprovalResolved {
                call_id,
                approved,
                reason,
            },
            RunEvent::ToolResult(result) => Self::ToolResult { result },
            RunEvent::Usage(usage) => Self::Usage { usage },
            RunEvent::ContinuationState(state) => Self::ContinuationState { state },
            RunEvent::Control(entry) => Self::Control {
                control: entry.into(),
            },
            RunEvent::AssistantMessage(message) => Self::AssistantMessage {
                message: message.into(),
            },
            RunEvent::Notice(message) => Self::Notice { message },
        }
    }
}

fn control_entry_kind(entry: &ControlEntry) -> &'static str {
    match entry {
        ControlEntry::SessionIdentity { .. } => "session_identity",
        ControlEntry::ContinuationStateSaved(_) => "continuation_state_saved",
        ControlEntry::ResponseHandleTracked(_) => "response_handle_tracked",
        ControlEntry::BackgroundTaskTracked(_) => "background_task_tracked",
        ControlEntry::PrefixSnapshotCaptured(_) => "prefix_snapshot_captured",
        ControlEntry::MemorySnapshotCaptured(_) => "memory_snapshot_captured",
        ControlEntry::UsageSnapshot(_) => "usage_snapshot",
        ControlEntry::ToolApproval(_) => "tool_approval",
        ControlEntry::ToolExecution(_) => "tool_execution",
        ControlEntry::ToolEgress(_) => "tool_egress",
        ControlEntry::McpElicitation(_) => "mcp_elicitation",
        ControlEntry::ToolPreviewCaptured(_) => "tool_preview_captured",
        ControlEntry::ChangeSetProposed(_) => "change_set_proposed",
        ControlEntry::ChangeSetApplied(_) => "change_set_applied",
        ControlEntry::TerminalTask(_) => "terminal_task",
        ControlEntry::CompactionApplied(_) => "compaction_applied",
        ControlEntry::TaskRun(_) => "task_run",
        ControlEntry::TaskPlan(_) => "task_plan",
        ControlEntry::TaskStep(_) => "task_step",
        ControlEntry::TaskChildSession(_) => "task_child_session",
        ControlEntry::TaskSubagentApprovalRoute(_) => "task_subagent_approval_route",
        ControlEntry::TaskSubagentElicitationRoute(_) => "task_subagent_elicitation_route",
        ControlEntry::Note { .. } => "note",
    }
}

/// Sink for run events emitted by the agent loop.
pub trait EventHandler {
    /// Handles one run event.
    ///
    /// # Errors
    ///
    /// Returns an error when the downstream event consumer fails and the current run should stop.
    fn handle(&mut self, event: RunEvent) -> Result<()>;
}

/// Event handler that ignores every incoming event.
pub struct NoopEventHandler;

impl EventHandler for NoopEventHandler {
    fn handle(&mut self, _event: RunEvent) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
#[path = "tests/event_tests.rs"]
mod tests;
