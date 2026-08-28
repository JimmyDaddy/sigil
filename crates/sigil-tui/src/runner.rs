mod approval_bridge;
mod diagnostics;
pub mod egress_disclosure_bridge;
mod elicitation_bridge;
mod event_bridge;
mod mcp_event_bridge;
mod protocol;
mod route_recovery;
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
#[cfg(test)]
pub(crate) use protocol::{
    LocalOperationKind, LocalOperationOutcome, LocalOperationStatus, ToolOutputShrinkPreview,
    V2ContinuityPreview, WorkerApprovalCommandReceipt,
};
pub(crate) use protocol::{
    McpActivationStatus, McpOAuthUserAction, QueueMoveDirection, TerminalTaskControlIdentity,
    ToolArtifactDisplayReadFailure, V2CompactionAdmission, V2CompactionApplySource,
    V2CompactionPreviewState, V2CompactionReview, WorkerCommand, WorkerCommandSender,
    WorkerMessage, WorkerRouteRecoverySessionTarget,
};
pub(crate) use route_recovery::worker_session_route_recovery_message;
pub(in crate::runner) use session_flow::ManagedTuiArtifactStoreLease;
#[cfg(test)]
pub(crate) use spawn::spawn_agent_worker;
#[cfg(not(test))]
pub(crate) use spawn::{
    WorkerSessionRouteDirective, spawn_agent_worker_with_route_directive_and_attachment,
};
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use worker_loop::{
    ArtifactGcTaskMetricsSnapshot, WorkerReactorMetricsSnapshot, artifact_gc_task_metrics,
    worker_reactor_metrics,
};

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
mod tests;
