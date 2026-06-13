use std::sync::mpsc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use sigil_runtime::{McpListChangedNotification, McpProgressNotification, McpRuntimeEventHandler};

#[derive(Debug, Clone)]
pub(super) struct ChannelMcpRuntimeEventHandler {
    event_tx: mpsc::Sender<McpRuntimeEvent>,
}

impl ChannelMcpRuntimeEventHandler {
    pub(super) fn new(event_tx: mpsc::Sender<McpRuntimeEvent>) -> Self {
        Self { event_tx }
    }
}

#[async_trait]
impl McpRuntimeEventHandler for ChannelMcpRuntimeEventHandler {
    async fn progress(&self, notification: McpProgressNotification) -> Result<()> {
        self.event_tx
            .send(McpRuntimeEvent::Progress(notification))
            .context("failed to send MCP progress event to worker")
    }

    async fn list_changed(&self, notification: McpListChangedNotification) -> Result<()> {
        self.event_tx
            .send(McpRuntimeEvent::ListChanged(notification))
            .context("failed to send MCP listChanged event to worker")
    }
}

#[derive(Debug, Clone)]
pub(super) enum McpRuntimeEvent {
    Progress(McpProgressNotification),
    ListChanged(McpListChangedNotification),
}
