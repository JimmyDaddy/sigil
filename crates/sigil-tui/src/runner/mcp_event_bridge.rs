use anyhow::{Context, Result};
use async_trait::async_trait;
use sigil_runtime::{McpListChangedNotification, McpProgressNotification, McpRuntimeEventHandler};

use super::worker_event::WorkerMcpRuntimeEventSender;

#[derive(Clone)]
pub(super) struct ChannelMcpRuntimeEventHandler {
    event_sink: McpRuntimeEventSink,
}

#[derive(Clone)]
enum McpRuntimeEventSink {
    Worker(WorkerMcpRuntimeEventSender),
    #[cfg(test)]
    Direct(std::sync::mpsc::Sender<McpRuntimeEvent>),
}

impl ChannelMcpRuntimeEventHandler {
    pub(super) fn new(event_tx: WorkerMcpRuntimeEventSender) -> Self {
        Self {
            event_sink: McpRuntimeEventSink::Worker(event_tx),
        }
    }

    #[cfg(test)]
    pub(super) fn new_test(event_tx: std::sync::mpsc::Sender<McpRuntimeEvent>) -> Self {
        Self {
            event_sink: McpRuntimeEventSink::Direct(event_tx),
        }
    }

    fn send(&self, event: McpRuntimeEvent) -> Result<()> {
        match &self.event_sink {
            McpRuntimeEventSink::Worker(event_tx) => event_tx
                .send(event)
                .context("failed to send MCP event to worker"),
            #[cfg(test)]
            McpRuntimeEventSink::Direct(event_tx) => event_tx
                .send(event)
                .context("failed to send MCP event to test receiver"),
        }
    }
}

impl std::fmt::Debug for ChannelMcpRuntimeEventHandler {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChannelMcpRuntimeEventHandler")
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl McpRuntimeEventHandler for ChannelMcpRuntimeEventHandler {
    async fn progress(&self, notification: McpProgressNotification) -> Result<()> {
        self.send(McpRuntimeEvent::Progress(notification))
    }

    async fn list_changed(&self, notification: McpListChangedNotification) -> Result<()> {
        self.send(McpRuntimeEvent::ListChanged(notification))
    }
}

#[derive(Debug, Clone)]
pub(super) enum McpRuntimeEvent {
    Progress(McpProgressNotification),
    ListChanged(McpListChangedNotification),
}
