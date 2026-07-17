mod approval_bridge;
mod diagnostics;
pub mod egress_disclosure_bridge;
mod elicitation_bridge;
mod event_bridge;
mod mcp_event_bridge;
mod protocol;
mod session_flow;
mod spawn;
mod worker_loop;

pub(crate) use protocol::{
    EgressDisclosureReceiptTx, McpElicitationResponseTx, WorkerApprovalCommand,
    WorkerCommandEnvelope,
};
pub use protocol::{
    McpActivationStatus, McpOAuthUserAction, QueueMoveDirection, V2CompactionAdmission,
    V2CompactionApplySource, V2CompactionPreviewState, V2CompactionReview, WorkerCommand,
    WorkerMessage,
};
pub use spawn::spawn_agent_worker;

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
mod tests;
