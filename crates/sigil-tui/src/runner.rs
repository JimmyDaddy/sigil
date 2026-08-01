mod approval_bridge;
mod diagnostics;
pub mod egress_disclosure_bridge;
mod elicitation_bridge;
mod event_bridge;
mod mcp_event_bridge;
mod protocol;
mod session_flow;
mod spawn;
mod terminal_lifecycle_bridge;
mod worker_event;
mod worker_loop;

#[cfg(test)]
pub(crate) use protocol::WorkerApprovalDecision;
pub(crate) use protocol::{
    EgressDisclosureReceiptTx, McpElicitationResponseTx, WorkerApprovalCommand,
    WorkerApprovalRouteState, WorkerCommandEnvelope,
};
pub use protocol::{
    McpActivationStatus, McpOAuthUserAction, QueueMoveDirection, TerminalTaskControlIdentity,
    ToolArtifactDisplayReadFailure, ToolOutputShrinkPreview, V2CompactionAdmission,
    V2CompactionApplySource, V2CompactionPreviewState, V2CompactionReview, V2ContinuityPreview,
    WorkerApprovalCommandReceipt, WorkerCommand, WorkerCommandSender, WorkerMessage,
};
pub use spawn::spawn_agent_worker;
pub use worker_loop::{
    ArtifactGcTaskMetricsSnapshot, WorkerReactorMetricsSnapshot, artifact_gc_task_metrics,
    worker_reactor_metrics,
};

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
mod tests;
