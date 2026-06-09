use std::{
    sync::mpsc,
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow};
use termquill_kernel::{ApprovalHandler, ToolApproval, ToolCall, ToolSpec};

const DEFAULT_APPROVAL_TIMEOUT: Duration = Duration::from_secs(5 * 60);

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
    timeout: Duration,
}

impl ChannelApprovalHandler {
    pub(super) fn new(decision_rx: mpsc::Receiver<ApprovalSignal>) -> Self {
        Self::with_timeout(decision_rx, DEFAULT_APPROVAL_TIMEOUT)
    }

    pub(super) fn with_timeout(
        decision_rx: mpsc::Receiver<ApprovalSignal>,
        timeout: Duration,
    ) -> Self {
        Self {
            decision_rx,
            timeout,
        }
    }
}

impl ApprovalHandler for ChannelApprovalHandler {
    fn approve_tool_call(&mut self, call: &ToolCall, _spec: &ToolSpec) -> Result<ToolApproval> {
        let started = Instant::now();
        loop {
            let Some(remaining) = self.timeout.checked_sub(started.elapsed()) else {
                return Ok(timeout_denial(self.timeout));
            };
            match self.decision_rx.recv_timeout(remaining) {
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
                    if matches!(error, mpsc::RecvTimeoutError::Timeout) {
                        return Ok(timeout_denial(self.timeout));
                    }
                    return Err(anyhow!("approval channel closed: {error}"));
                }
            }
        }
    }
}

fn timeout_denial(timeout: Duration) -> ToolApproval {
    ToolApproval::Deny {
        reason: format!("approval timed out after {} seconds", timeout.as_secs()),
    }
}
