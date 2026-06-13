use std::sync::{Arc, Mutex, mpsc};

use anyhow::{Context, Result};
use async_trait::async_trait;
use sigil_kernel::{ControlEntry, McpElicitationDecision, McpElicitationEntry, RunEvent};
use sigil_runtime::{
    McpElicitationAction, McpElicitationHandler, McpElicitationRequest, McpElicitationResponse,
};
use tokio::sync::oneshot;

use super::protocol::WorkerMessage;

pub(super) type McpElicitationAuditBuffer = Arc<Mutex<Vec<ControlEntry>>>;

#[derive(Debug, Clone)]
pub(super) struct ChannelMcpElicitationHandler {
    message_tx: mpsc::Sender<WorkerMessage>,
    audit_buffer: Arc<Mutex<Option<McpElicitationAuditBuffer>>>,
}

impl ChannelMcpElicitationHandler {
    pub(super) fn new(message_tx: mpsc::Sender<WorkerMessage>) -> Self {
        Self {
            message_tx,
            audit_buffer: Arc::new(Mutex::new(None)),
        }
    }

    pub(super) fn set_audit_buffer(&self, audit_buffer: Option<McpElicitationAuditBuffer>) {
        if let Ok(mut slot) = self.audit_buffer.lock() {
            *slot = audit_buffer;
        }
    }

    fn record_audit(&self, control: ControlEntry) {
        let recorded = self
            .audit_buffer
            .lock()
            .ok()
            .and_then(|slot| slot.as_ref().cloned())
            .and_then(|buffer| {
                buffer.lock().ok().map(|mut controls| {
                    controls.push(control.clone());
                })
            })
            .is_some();
        if recorded {
            let _ = self
                .message_tx
                .send(WorkerMessage::Event(Box::new(RunEvent::Control(control))));
        }
    }
}

#[async_trait]
impl McpElicitationHandler for ChannelMcpElicitationHandler {
    fn supports_elicitation(&self) -> bool {
        true
    }

    async fn elicit(&self, request: McpElicitationRequest) -> Result<McpElicitationResponse> {
        let (response_tx, response_rx) = oneshot::channel();
        let request_for_audit = request.clone();
        self.message_tx
            .send(WorkerMessage::McpElicitationRequest {
                request,
                response_tx,
            })
            .context("failed to send MCP elicitation request to TUI")?;
        let response = response_rx
            .await
            .context("MCP elicitation response channel closed")?;
        self.record_audit(mcp_elicitation_control_entry(&request_for_audit, &response));
        Ok(response)
    }
}

fn mcp_elicitation_control_entry(
    request: &McpElicitationRequest,
    response: &McpElicitationResponse,
) -> ControlEntry {
    ControlEntry::McpElicitation(Box::new(McpElicitationEntry::new(
        request.server_name.clone(),
        &request.message,
        &request.requested_schema,
        match response.action {
            McpElicitationAction::Accept => McpElicitationDecision::Accepted,
            McpElicitationAction::Decline => McpElicitationDecision::Declined,
            McpElicitationAction::Cancel => McpElicitationDecision::Cancelled,
        },
        response.content.as_ref(),
    )))
}
