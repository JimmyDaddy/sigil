use std::sync::mpsc;

use anyhow::{Result, anyhow};
use termquill_kernel::{EventHandler, RunEvent};

use super::protocol::WorkerMessage;

#[derive(Clone)]
pub(super) struct ChannelEventHandler {
    sender: mpsc::Sender<WorkerMessage>,
}

impl ChannelEventHandler {
    pub(super) fn new(sender: mpsc::Sender<WorkerMessage>) -> Self {
        Self { sender }
    }
}

impl EventHandler for ChannelEventHandler {
    fn handle(&mut self, event: RunEvent) -> Result<()> {
        self.sender
            .send(WorkerMessage::Event(Box::new(event)))
            .map_err(|error| anyhow!("failed to forward run event: {error}"))
    }
}
