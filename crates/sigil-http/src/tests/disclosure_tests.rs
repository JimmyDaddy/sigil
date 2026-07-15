use std::sync::Arc;

use sigil_kernel::{
    DisclosurePresentationError, EgressDataCategory, EgressDisclosureKind,
    EgressDisclosurePresenter, EgressNetworkRoute, PreEgressDisclosure,
};

use super::{
    HTTP_EGRESS_DISCLOSURE_SCHEMA_VERSION, HttpDisclosureReplayError, HttpDurableDisclosureError,
    HttpDurableEgressDisclosureJournal, HttpDurableEgressDisclosurePresenter,
    HttpEgressDisclosureReplayBuffer, HttpReplayEgressDisclosurePresenter,
    MAX_HTTP_DISCLOSURE_JOURNAL_BYTES, MAX_HTTP_DISCLOSURE_JOURNAL_RECORDS,
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

#[tokio::test]
async fn production_presenter_durably_replays_after_reopen() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let path = temp.path().join("disclosures.json");
    let sink_fingerprint;
    {
        let journal = Arc::new(
            HttpDurableEgressDisclosureJournal::open(&path, 8)
                .expect("production journal should initialize"),
        );
        sink_fingerprint = journal.sink_fingerprint().to_owned();
        let presenter = HttpDurableEgressDisclosurePresenter::new(journal);
        let receipt = presenter
            .present(disclosure(Some("query-durable")))
            .await
            .expect("durable publication should acknowledge");
        assert_eq!(receipt.sink_fingerprint(), sink_fingerprint);
    }

    let reopened = HttpDurableEgressDisclosureJournal::open(path, 8)
        .expect("production journal should reopen");
    let replay = reopened
        .replay_after(None)
        .expect("durable disclosure should replay after restart");

    assert_eq!(replay.len(), 1);
    assert_eq!(replay[0].sequence, 1);
    assert_eq!(replay[0].replay_id, "sigil-http-disclosure-v1:1");
    assert_eq!(replay[0].disclosure.correlation_id(), Some("query-durable"));
}

#[test]
fn production_disclosure_retention_expires_old_cursors_explicitly() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let journal = HttpDurableEgressDisclosureJournal::open(temp.path().join("disclosures.json"), 2)
        .expect("production journal should initialize");
    for index in 1..=4 {
        journal
            .publish(disclosure(Some(&format!("query-{index}"))))
            .expect("disclosure should persist");
    }

    assert_eq!(
        journal
            .replay_after(Some("sigil-http-disclosure-v1:1"))
            .expect_err("trimmed disclosure cursor must not return a false suffix"),
        HttpDisclosureReplayError::CursorExpired
    );
}

#[test]
fn production_disclosure_accepts_the_exact_eviction_boundary_cursor() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let journal = HttpDurableEgressDisclosureJournal::open(temp.path().join("disclosures.json"), 2)
        .expect("production journal should initialize");
    for index in 1..=3 {
        journal
            .publish(disclosure(Some(&format!("query-{index}"))))
            .expect("disclosure should persist");
    }

    let replay = journal
        .replay_after(Some("sigil-http-disclosure-v1:1"))
        .expect("exact eviction boundary has a complete suffix");
    assert_eq!(
        replay
            .iter()
            .map(|record| record.sequence)
            .collect::<Vec<_>>(),
        vec![2, 3]
    );
}

#[test]
fn production_disclosure_rejects_invalid_deserialized_payloads_without_mutation() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let journal = HttpDurableEgressDisclosureJournal::open(temp.path().join("disclosures.json"), 8)
        .expect("production journal should initialize");
    let mut value = serde_json::to_value(disclosure(Some("query-invalid")))
        .expect("disclosure should serialize");
    value["disclosure_content_sha256"] = serde_json::Value::String("0".repeat(64));
    let invalid = serde_json::from_value(value).expect("serde should reconstruct the invalid DTO");

    assert!(matches!(
        journal.publish(invalid),
        Err(HttpDurableDisclosureError::InvalidDisclosure { .. })
    ));
    assert!(
        journal
            .replay_after(None)
            .expect("replay should remain available")
            .is_empty()
    );
}

#[test]
fn production_disclosure_reopen_rejects_tampered_kernel_integrity() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let path = temp.path().join("disclosures.json");
    {
        let journal = HttpDurableEgressDisclosureJournal::open(&path, 8)
            .expect("production journal should initialize");
        journal
            .publish(disclosure(Some("query-tampered")))
            .expect("disclosure should persist");
    }
    let mut file: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).expect("journal should remain readable"))
            .expect("journal should parse");
    file["records"][0]["disclosure"]["display_name"] =
        serde_json::Value::String("tampered display".to_owned());
    std::fs::write(
        &path,
        serde_json::to_vec(&file).expect("tampered journal should serialize"),
    )
    .expect("tampered journal should write");

    assert!(matches!(
        HttpDurableEgressDisclosureJournal::open(path, 8),
        Err(HttpDurableDisclosureError::Corrupt { .. })
    ));
}

#[test]
fn production_disclosure_reopen_rejects_a_forged_high_watermark_or_suffix_gap() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let path = temp.path().join("disclosures.json");
    {
        let journal = HttpDurableEgressDisclosureJournal::open(&path, 8)
            .expect("production journal should initialize");
        journal
            .publish(disclosure(Some("query-1")))
            .expect("first disclosure should persist");
        journal
            .publish(disclosure(Some("query-2")))
            .expect("second disclosure should persist");
    }
    let original = std::fs::read(&path).expect("journal should remain readable");
    let mut forged_watermark: serde_json::Value =
        serde_json::from_slice(&original).expect("journal should parse");
    forged_watermark["next_sequence"] = serde_json::Value::from(3_u64);
    std::fs::write(
        &path,
        serde_json::to_vec(&forged_watermark).expect("fixture should serialize"),
    )
    .expect("fixture should write");
    assert!(matches!(
        HttpDurableEgressDisclosureJournal::open(&path, 8),
        Err(HttpDurableDisclosureError::Corrupt { .. })
    ));

    std::fs::write(&path, original).expect("original journal should restore");
    let mut forged_gap: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).expect("journal should remain readable"))
            .expect("journal should parse");
    forged_gap["records"][1]["sequence"] = serde_json::Value::from(3_u64);
    forged_gap["records"][1]["replay_id"] =
        serde_json::Value::String("sigil-http-disclosure-v1:3".to_owned());
    forged_gap["next_sequence"] = serde_json::Value::from(3_u64);
    std::fs::write(
        &path,
        serde_json::to_vec(&forged_gap).expect("fixture should serialize"),
    )
    .expect("fixture should write");
    assert!(matches!(
        HttpDurableEgressDisclosureJournal::open(path, 8),
        Err(HttpDurableDisclosureError::Corrupt { .. })
    ));
}

#[test]
fn production_disclosure_rejects_invalid_capacity_and_oversized_files() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    assert!(matches!(
        HttpDurableEgressDisclosureJournal::open(temp.path().join("zero.json"), 0),
        Err(HttpDurableDisclosureError::InvalidCapacity { .. })
    ));
    assert!(matches!(
        HttpDurableEgressDisclosureJournal::open(
            temp.path().join("too-many.json"),
            MAX_HTTP_DISCLOSURE_JOURNAL_RECORDS + 1,
        ),
        Err(HttpDurableDisclosureError::InvalidCapacity { .. })
    ));

    let oversized = temp.path().join("oversized.json");
    std::fs::write(
        &oversized,
        vec![b' '; MAX_HTTP_DISCLOSURE_JOURNAL_BYTES + 1],
    )
    .expect("oversized fixture should write");
    assert!(matches!(
        HttpDurableEgressDisclosureJournal::open(oversized, 8),
        Err(HttpDurableDisclosureError::Io { .. })
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn production_presenter_returns_no_receipt_when_durable_sink_becomes_unwritable() {
    let temp = tempfile::tempdir().expect("temporary directory should exist");
    let directory = temp.path().join("journal-dir");
    let path = directory.join("disclosures.json");
    let journal = Arc::new(
        HttpDurableEgressDisclosureJournal::open(&path, 8)
            .expect("production journal should initialize"),
    );
    let moved_directory = temp.path().join("journal-dir-moved");
    std::fs::rename(&directory, &moved_directory)
        .expect("open lease should move with its directory on Unix");
    std::fs::write(&directory, "not a directory")
        .expect("blocking file should replace journal directory");
    let presenter = HttpDurableEgressDisclosurePresenter::new(journal);

    assert!(matches!(
        presenter.present(disclosure(Some("query-fail"))).await,
        Err(DisclosurePresentationError::WriteFailed)
    ));
}
