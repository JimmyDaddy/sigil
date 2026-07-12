use std::sync::Arc;

use sigil_kernel::{
    DisclosurePresentationError, EgressDataCategory, EgressDisclosureKind,
    EgressDisclosurePresenter, EgressNetworkRoute, PreEgressDisclosure,
};

use super::{
    HTTP_EGRESS_DISCLOSURE_SCHEMA_VERSION, HttpEgressDisclosureReplayBuffer,
    HttpReplayEgressDisclosurePresenter,
};

fn disclosure(correlation_id: Option<&str>) -> PreEgressDisclosure {
    PreEgressDisclosure::new(
        if correlation_id.is_some() {
            EgressDisclosureKind::Query
        } else {
            EgressDisclosureKind::Transport
        },
        correlation_id.map(ToOwned::to_owned),
        "exa-anonymous-2026-06-29",
        "http",
        "Exa no-key free tier",
        "route-fingerprint",
        "profile-fingerprint",
        "https://mcp.exa.ai/",
        "https://mcp.exa.ai/",
        EgressNetworkRoute::Direct,
        if correlation_id.is_some() {
            vec![EgressDataCategory::SearchQuery]
        } else {
            vec![EgressDataCategory::ConnectionMetadata]
        },
    )
    .expect("valid safe disclosure")
}

#[tokio::test]
async fn presenter_publishes_a_dedicated_structured_replay_event_before_acknowledging() {
    let replay = Arc::new(HttpEgressDisclosureReplayBuffer::new());
    let presenter = HttpReplayEgressDisclosurePresenter::new(replay.clone());
    let pending = disclosure(Some("query-1"));

    let receipt = presenter
        .present(pending.clone())
        .await
        .expect("synthetic replay publication should acknowledge");
    let events = replay.events();

    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].schema_version,
        HTTP_EGRESS_DISCLOSURE_SCHEMA_VERSION
    );
    assert_eq!(events[0].event_type, "egress_disclosure");
    assert_eq!(events[0].disclosure, pending);
    assert_eq!(receipt.disclosure_id(), "exa-anonymous-2026-06-29");
    assert_eq!(receipt.correlation_id(), Some("query-1"));
    assert_eq!(
        receipt.sink_fingerprint(),
        "http-synthetic-replay-buffer-v1"
    );
}

#[tokio::test]
async fn closed_or_failed_replay_sink_never_returns_a_receipt() {
    let replay = Arc::new(HttpEgressDisclosureReplayBuffer::new());
    let presenter = HttpReplayEgressDisclosurePresenter::new(replay.clone());
    replay.fail_next_publish();

    assert!(matches!(
        presenter.present(disclosure(None)).await,
        Err(DisclosurePresentationError::WriteFailed)
    ));
    assert!(replay.events().is_empty());

    replay.close();
    assert!(matches!(
        presenter.present(disclosure(None)).await,
        Err(DisclosurePresentationError::SinkClosed)
    ));
}
