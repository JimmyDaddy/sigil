use std::sync::mpsc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use termquill_runtime::{McpElicitationHandler, McpElicitationRequest, McpElicitationResponse};
use tokio::sync::oneshot;

use super::protocol::WorkerMessage;

#[derive(Debug, Clone)]
pub(super) struct ChannelMcpElicitationHandler {
    message_tx: mpsc::Sender<WorkerMessage>,
}

impl ChannelMcpElicitationHandler {
    pub(super) fn new(message_tx: mpsc::Sender<WorkerMessage>) -> Self {
        Self { message_tx }
    }
}

#[async_trait]
impl McpElicitationHandler for ChannelMcpElicitationHandler {
    fn supports_elicitation(&self) -> bool {
        true
    }

    async fn elicit(&self, request: McpElicitationRequest) -> Result<McpElicitationResponse> {
        let (response_tx, response_rx) = oneshot::channel();
        self.message_tx
            .send(WorkerMessage::McpElicitationRequest {
                request,
                response_tx,
            })
            .context("failed to send MCP elicitation request to TUI")?;
        response_rx
            .await
            .context("MCP elicitation response channel closed")
    }
}
