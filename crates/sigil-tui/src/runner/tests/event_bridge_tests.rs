use std::sync::mpsc;

use anyhow::Result;
use sigil_kernel::{EventHandler, RunEvent};

use super::super::{event_bridge::ChannelEventHandler, protocol::WorkerMessage};

#[test]
fn handle_forwards_event_to_channel() -> Result<()> {
    let (tx, rx) = mpsc::channel();
    let mut handler = ChannelEventHandler::new(tx);

    handler.handle(RunEvent::Notice("test notice".to_owned()))?;

    let WorkerMessage::Event(event) = rx.try_recv()? else {
        unreachable!("event handler only sends Event messages");
    };
    assert!(matches!(*event, RunEvent::Notice(ref text) if text == "test notice"));
    Ok(())
}

#[test]
fn handle_returns_error_when_receiver_dropped() {
    let (tx, rx) = mpsc::channel();
    drop(rx);
    let mut handler = ChannelEventHandler::new(tx);

    let error = handler
        .handle(RunEvent::Notice("lost".to_owned()))
        .expect_err("dropped receiver should surface forwarding failure");

    assert!(error.to_string().contains("failed to forward"));
}

#[test]
fn clones_share_same_sender() -> Result<()> {
    let (tx, rx) = mpsc::channel();
    let handler1 = ChannelEventHandler::new(tx);
    let mut handler2 = handler1.clone();

    handler2.handle(RunEvent::Notice("from clone".to_owned()))?;

    let WorkerMessage::Event(event) = rx.try_recv()? else {
        unreachable!("event handler only sends Event messages");
    };
    assert!(matches!(*event, RunEvent::Notice(ref text) if text == "from clone"));
    Ok(())
}
