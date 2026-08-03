use std::sync::mpsc;

use anyhow::{Result, anyhow};
use sigil_kernel::{ApprovalHandler, ToolApproval, ToolApprovalContext, ToolCall, ToolSpec};

#[derive(Debug, Clone)]
pub(super) enum ApprovalSignal {
    Decision {
        call_id: String,
        approval_request_id: String,
        approval: ToolApproval,
        acknowledgement_tx: mpsc::SyncSender<ApprovalDeliveryAcknowledgement>,
    },
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ApprovalDeliveryAcknowledgement {
    pub(super) call_id: String,
    pub(super) approval_request_id: String,
    pub(super) accepted: bool,
}

pub(super) struct ChannelApprovalHandler {
    decision_rx: mpsc::Receiver<ApprovalSignal>,
}

impl ChannelApprovalHandler {
    pub(super) fn new(decision_rx: mpsc::Receiver<ApprovalSignal>) -> Self {
        Self { decision_rx }
    }
}

impl ApprovalHandler for ChannelApprovalHandler {
    fn approval_is_explicit_user_action(&self) -> bool {
        true
    }

    fn approve_tool_call(&mut self, call: &ToolCall, _spec: &ToolSpec) -> Result<ToolApproval> {
        self.await_decision(call, None)
    }

    fn approve_tool_call_with_context(
        &mut self,
        call: &ToolCall,
        _spec: &ToolSpec,
        context: &ToolApprovalContext,
    ) -> Result<ToolApproval> {
        self.await_decision(call, Some(&context.identity.approval_request_id))
    }
}

impl ChannelApprovalHandler {
    fn await_decision(
        &mut self,
        call: &ToolCall,
        expected_approval_request_id: Option<&str>,
    ) -> Result<ToolApproval> {
        loop {
            match self.decision_rx.recv() {
                Ok(ApprovalSignal::Decision {
                    call_id,
                    approval_request_id,
                    approval,
                    acknowledgement_tx,
                }) if call_id == call.id
                    && expected_approval_request_id
                        .is_none_or(|expected| expected == approval_request_id) =>
                {
                    let _ = acknowledgement_tx.send(ApprovalDeliveryAcknowledgement {
                        call_id,
                        approval_request_id,
                        accepted: true,
                    });
                    return Ok(approval);
                }
                Ok(ApprovalSignal::Decision {
                    call_id,
                    approval_request_id,
                    acknowledgement_tx,
                    ..
                }) => {
                    let _ = acknowledgement_tx.send(ApprovalDeliveryAcknowledgement {
                        call_id,
                        approval_request_id,
                        accepted: false,
                    });
                }
                Ok(ApprovalSignal::Cancel) => {
                    return Ok(ToolApproval::Cancelled {
                        reason: "run cancelled from TUI".to_owned(),
                    });
                }
                Err(error) => return Err(anyhow!("approval channel closed: {error}")),
            }
        }
    }
}
