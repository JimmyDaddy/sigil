use std::sync::mpsc;

use anyhow::{Result, anyhow};
use sigil_kernel::{EventHandler, RunEvent};

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

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use sigil_kernel::{EventHandler, RunEvent};

    use super::super::protocol::WorkerMessage;
    use super::*;

    #[test]
    fn handle_forwards_event_to_channel() -> Result<()> {
        let (tx, rx) = mpsc::channel();
        let mut handler = ChannelEventHandler::new(tx);

        handler.handle(RunEvent::Notice("test notice".to_owned()))?;

        let message = rx.try_recv()?;
        match message {
            WorkerMessage::Event(event) => {
                assert!(matches!(*event, RunEvent::Notice(ref text) if text == "test notice"));
                Ok(())
            }
            _ => panic!("expected Event message"),
        }
    }

    #[test]
    fn handle_returns_error_when_receiver_dropped() {
        let (tx, rx) = mpsc::channel();
        drop(rx);
        let mut handler = ChannelEventHandler::new(tx);

        let result = handler.handle(RunEvent::Notice("lost".to_owned()));
        let Err(error) = result else {
            panic!("expected channel forwarding error");
        };
        assert!(error.to_string().contains("failed to forward"));
    }

    #[test]
    fn clones_share_same_sender() -> Result<()> {
        let (tx, rx) = mpsc::channel();
        let handler1 = ChannelEventHandler::new(tx);
        let mut handler2 = handler1.clone();

        handler2.handle(RunEvent::Notice("from clone".to_owned()))?;

        let message = rx.try_recv()?;
        match message {
            WorkerMessage::Event(event) => {
                assert!(matches!(*event, RunEvent::Notice(ref text) if text == "from clone"));
                Ok(())
            }
            _ => panic!("expected Event message"),
        }
    }
}
