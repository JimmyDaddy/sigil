use std::{
    sync::mpsc,
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow};
use sigil_kernel::{ApprovalHandler, ToolApproval, ToolCall, ToolSpec};

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

#[cfg(test)]
mod tests {
    use std::{sync::mpsc, time::Duration};

    use anyhow::Result;
    use sigil_kernel::{ApprovalHandler, ToolApproval, ToolCall};

    use super::*;

    fn test_call(id: &str) -> ToolCall {
        ToolCall {
            id: id.to_owned(),
            name: "test_tool".to_owned(),
            args_json: "{}".to_owned(),
        }
    }

    fn test_spec() -> ToolSpec {
        ToolSpec {
            name: "test_tool".to_owned(),
            description: "test".to_owned(),
            input_schema: serde_json::json!({}),
            category: sigil_kernel::ToolCategory::Custom,
            access: sigil_kernel::ToolAccess::Read,
            preview: sigil_kernel::ToolPreviewCapability::None,
        }
    }

    #[test]
    fn new_uses_default_five_minute_timeout() {
        let (_tx, rx) = mpsc::channel();
        let handler = ChannelApprovalHandler::new(rx);
        assert_eq!(handler.timeout, Duration::from_secs(5 * 60));
    }

    #[test]
    fn with_timeout_uses_configured_value() {
        let (_tx, rx) = mpsc::channel();
        let timeout = Duration::from_secs(10);
        let handler = ChannelApprovalHandler::with_timeout(rx, timeout);
        assert_eq!(handler.timeout, timeout);
    }

    #[test]
    fn approves_matching_call_id() -> Result<()> {
        let (tx, rx) = mpsc::channel();
        let mut handler = ChannelApprovalHandler::with_timeout(rx, Duration::from_secs(10));
        let call = test_call("call-1");

        let handle = std::thread::spawn(move || {
            let approval = handler.approve_tool_call(&call, &test_spec());
            (approval, call)
        });

        tx.send(ApprovalSignal::Decision {
            call_id: "call-1".to_owned(),
            approval: ToolApproval::Approve,
        })?;

        let (result, _) = handle
            .join()
            .map_err(|_| anyhow::anyhow!("approval thread panicked"))?;
        assert!(matches!(result?, ToolApproval::Approve));
        Ok(())
    }

    #[test]
    fn ignores_decision_for_different_call_id() -> Result<()> {
        let (tx, rx) = mpsc::channel();
        let mut handler = ChannelApprovalHandler::with_timeout(rx, Duration::from_secs(10));
        let call = test_call("call-2");

        let handle = std::thread::spawn(move || {
            let approval = handler.approve_tool_call(&call, &test_spec());
            (approval, call)
        });

        // Send a decision for a different call first; it should be ignored.
        tx.send(ApprovalSignal::Decision {
            call_id: "call-other".to_owned(),
            approval: ToolApproval::Approve,
        })?;

        // Then send the real decision.
        tx.send(ApprovalSignal::Decision {
            call_id: "call-2".to_owned(),
            approval: ToolApproval::Deny {
                reason: "nope".to_owned(),
            },
        })?;

        let (result, _) = handle
            .join()
            .map_err(|_| anyhow::anyhow!("approval thread panicked"))?;
        match result? {
            ToolApproval::Deny { ref reason } if reason == "nope" => Ok(()),
            other => panic!("expected denial with 'nope', got {other:?}"),
        }
    }

    #[test]
    fn cancel_returns_denial() -> Result<()> {
        let (tx, rx) = mpsc::channel();
        let mut handler = ChannelApprovalHandler::with_timeout(rx, Duration::from_secs(10));
        let call = test_call("call-3");

        let handle = std::thread::spawn(move || {
            let approval = handler.approve_tool_call(&call, &test_spec());
            (approval, call)
        });

        tx.send(ApprovalSignal::Cancel)?;

        let (result, _) = handle
            .join()
            .map_err(|_| anyhow::anyhow!("approval thread panicked"))?;
        match result? {
            ToolApproval::Deny { ref reason } if reason.contains("cancelled") => Ok(()),
            other => panic!("expected cancellation denial, got {other:?}"),
        }
    }

    #[test]
    fn timeout_produces_denial_with_reason() -> Result<()> {
        let (_tx, rx) = mpsc::channel(); // drop tx immediately so channel is empty
        let timeout = Duration::from_millis(1);
        let mut handler = ChannelApprovalHandler::with_timeout(rx, timeout);
        let call = test_call("call-4");

        let result = handler.approve_tool_call(&call, &test_spec())?;
        match result {
            ToolApproval::Deny { ref reason } if reason.contains("timed out") => Ok(()),
            other => panic!("expected timeout denial, got {other:?}"),
        }
    }

    #[test]
    fn sender_disconnect_returns_error() {
        let (tx, rx) = mpsc::channel();
        drop(tx); // sender gone, channel disconnected
        let mut handler = ChannelApprovalHandler::with_timeout(rx, Duration::from_secs(10));
        let call = test_call("call-5");

        let result = handler.approve_tool_call(&call, &test_spec());
        let Err(error) = result else {
            panic!("expected approval channel close error");
        };
        assert!(error.to_string().contains("channel closed"));
    }
}
