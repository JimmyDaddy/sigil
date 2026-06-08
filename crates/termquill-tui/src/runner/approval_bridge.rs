use std::sync::mpsc;

use anyhow::{Result, anyhow};
use termquill_kernel::{ApprovalHandler, ToolApproval, ToolCall, ToolSpec};

#[derive(Debug, Clone)]
pub(super) enum ApprovalSignal {
    Decision {
        call_id: String,
        approval: ToolApproval,
    },
    Cancel,
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
    fn approve_tool_call(&mut self, call: &ToolCall, _spec: &ToolSpec) -> Result<ToolApproval> {
        loop {
            match self.decision_rx.recv() {
                Ok(ApprovalSignal::Decision { call_id, approval }) if call_id == call.id => {
                    return Ok(approval);
                }
                Ok(ApprovalSignal::Decision { .. }) => {}
                Ok(ApprovalSignal::Cancel) => {
                    return Ok(ToolApproval::Deny {
                        reason: "run cancelled from TUI".to_owned(),
                    });
                }
                Err(error) => {
                    return Err(anyhow!("approval channel closed: {error}"));
                }
            }
        }
    }
}
