use std::sync::mpsc;

use sigil_kernel::{
    DisclosurePresentationError, EgressDataCategory, EgressDisclosureKind,
    EgressDisclosurePresenter, EgressNetworkRoute, PreEgressDisclosure,
};

use super::ChannelEgressDisclosurePresenter;
use crate::runner::WorkerMessage;

fn disclosure() -> PreEgressDisclosure {
    PreEgressDisclosure::new(
        EgressDisclosureKind::Query,
        Some("query-1".to_owned()),
        "exa-anonymous-2026-06-29",
        "tui",
        "Exa no-key free tier",
        "route-fingerprint",
        "profile-fingerprint",
        "https://mcp.exa.ai/",
        "https://mcp.exa.ai/",
        EgressNetworkRoute::Direct,
        vec![EgressDataCategory::SearchQuery],
    )
    .expect("valid safe disclosure")
}

#[tokio::test]
async fn bridge_waits_for_the_tui_receipt_instead_of_acknowledging_enqueue() {
    let (message_tx, message_rx) = mpsc::channel();
    let presenter = ChannelEgressDisclosurePresenter::new(message_tx);
    let pending = tokio::spawn(async move { presenter.present(disclosure()).await });

    let message = tokio::task::spawn_blocking(move || message_rx.recv())
        .await
        .expect("receive task should join")
        .expect("bridge request");
    let WorkerMessage::EgressDisclosureRequested {
        disclosure,
        receipt_tx,
    } = message
    else {
        panic!("expected egress disclosure request");
    };
    assert!(!pending.is_finished());
    receipt_tx
        .send(Ok(disclosure
            .presentation_receipt("tui-active-card-frame-v1")
            .expect("safe receipt")))
        .expect("worker is waiting");

    let receipt = pending
        .await
        .expect("task should join")
        .expect("receipt should pass through");
    assert_eq!(receipt.correlation_id(), Some("query-1"));
    assert_eq!(receipt.sink_fingerprint(), "tui-active-card-frame-v1");
}

#[tokio::test]
async fn closed_tui_channel_fails_closed() {
    let (message_tx, message_rx) = mpsc::channel();
    drop(message_rx);
    let presenter = ChannelEgressDisclosurePresenter::new(message_tx);

    assert!(matches!(
        presenter.present(disclosure()).await,
        Err(DisclosurePresentationError::SinkClosed)
    ));
}
