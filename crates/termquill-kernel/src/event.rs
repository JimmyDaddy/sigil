use anyhow::Result;

use crate::{
    ControlEntry, ModelMessage, ProviderContinuationState, ToolCall, ToolPreview, ToolResult,
    ToolSpec, UsageStats,
};

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
