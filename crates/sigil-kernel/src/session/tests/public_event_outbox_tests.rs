use super::*;

fn event(sequence: u64) -> crate::PublicRunEvent {
    crate::PublicRunEvent::new(
        "session-1".to_owned(),
        "run-1".to_owned(),
        sequence,
        crate::PublicRunEventKind::Notice {
            message: "safe".to_owned(),
        },
    )
}

fn entry(sequence: u64) -> PublicEventOutboxEntryV1 {
    let event = event(sequence);
    let public_event_id = format!("event-{sequence}");
    PublicEventOutboxEntryV1 {
        schema_version: PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
        public_event_id: public_event_id.clone(),
        domain_event_id: public_event_id,
        run_id: event.run_id.clone(),
        sequence,
        payload_digest: crate::stable_event_hash(
            serde_json::to_vec(&event).expect("public event encodes"),
        ),
        event,
    }
}

fn entry_for(
    session_id: &str,
    run_id: &str,
    sequence: u64,
    public_event_id: &str,
) -> PublicEventOutboxEntryV1 {
    let event = crate::PublicRunEvent::new(
        session_id.to_owned(),
        run_id.to_owned(),
        sequence,
        crate::PublicRunEventKind::Notice {
            message: format!("safe-{public_event_id}"),
        },
    );
    PublicEventOutboxEntryV1 {
        schema_version: PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
        public_event_id: public_event_id.to_owned(),
        domain_event_id: public_event_id.to_owned(),
        run_id: run_id.to_owned(),
        sequence,
        payload_digest: crate::stable_event_hash(
            serde_json::to_vec(&event).expect("public event encodes"),
        ),
        event,
    }
}

fn session_id(store: &JsonlSessionStore) -> Result<String> {
    Ok(store
        .active_projection_snapshot()?
        .frontier()
        .session_id()
        .to_owned())
}

#[test]
fn malformed_lifecycle_degrades_admission_without_rejecting_the_raw_append() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = crate::Session::new("test", "model").with_store(store.clone());
    session
        .conversation_run_lifecycle_recorder()?
        .append_started(&crate::ConversationRunStartedEntryV1::new("run-1", 1)?)?;

    // A generic raw append keeps its existing delayed strict-validation boundary. The
    // admission cache must fail closed for later public-control work instead of turning this
    // unrelated append into an eager lifecycle parser error.
    store.append_event(
        DurableEventType::RunStatusChanged,
        EventClass::Critical,
        serde_json::json!({"not": "a_conversation_run_lifecycle"}),
    )?;
    assert!(
        session
            .append_controls_with_public_outbox(
                vec![crate::ControlEntry::Note {
                    kind: "private_audit".to_owned(),
                    data: serde_json::Value::Null,
                }],
                "run-1",
                1,
            )
            .is_err()
    );
    Ok(())
}

#[test]
fn outbox_projection_keeps_failed_delivery_pending_without_changing_domain_event() -> Result<()> {
    let entry = entry(1);
    let mut projection = PublicEventOutboxProjectionV1::default();
    projection.apply_outbox(entry.clone())?;
    assert_eq!(projection.pending_for_adapter("http").len(), 1);
    projection.apply_delivery(PublicEventDeliveryReceiptV1 {
        schema_version: PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
        public_event_id: entry.public_event_id.clone(),
        adapter: "http".to_owned(),
        delivered_at_unix_ms: 1,
    })?;
    assert!(projection.pending_for_adapter("http").is_empty());
    assert_eq!(projection.pending_for_adapter("desktop").len(), 1);
    Ok(())
}

#[test]
fn outbox_projection_orders_state_events_independently_of_delivery() -> Result<()> {
    let first = entry(1);
    let second = entry(2);
    let mut projection = PublicEventOutboxProjectionV1::default();
    projection.apply_outbox(first.clone())?;
    projection.apply_outbox(second)?;
    projection.apply_delivery(PublicEventDeliveryReceiptV1 {
        schema_version: PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
        public_event_id: first.public_event_id,
        adapter: "tui".to_owned(),
        delivered_at_unix_ms: 1,
    })?;

    let events = projection.events_in_order();
    assert_eq!(
        events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        [1, 2]
    );
    assert_eq!(events.len(), 2);
    assert!(projection.pending_for_adapter("tui").len() == 1);
    Ok(())
}

#[test]
fn outbox_projection_rejects_same_run_sequence_conflicts_and_regressions() -> Result<()> {
    let first = entry_for("session-1", "run-1", 2, "event-2");
    let mut projection = PublicEventOutboxProjectionV1::default();
    projection.apply_outbox(entry_for("session-1", "run-1", 1, "event-1"))?;
    projection.apply_outbox(first)?;
    assert_eq!(projection.durable_sequence("run-1"), 2);
    assert!(
        projection
            .apply_outbox(entry_for("session-1", "run-1", 2, "event-2-conflict"))
            .is_err()
    );
    assert!(
        projection
            .apply_outbox(entry_for("session-1", "run-1", 1, "event-1-regression"))
            .is_err()
    );
    Ok(())
}

#[test]
fn recorder_persists_exact_nonterminal_entry_and_recovers_watermark() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    let recorder = PublicEventOutboxRecorder::new(store.clone());
    let session_id = session_id(&store)?;
    let first = entry_for(&session_id, "run-1", 1, "event-1");

    assert_eq!(recorder.durable_sequence("run-1")?, 0);
    assert!(recorder.append_outbox(&first)?);
    assert_eq!(recorder.durable_sequence("run-1")?, 1);
    assert!(!recorder.append_outbox(&first)?);

    let mut conflicting = first.clone();
    conflicting.event = crate::PublicRunEvent::new(
        session_id.clone(),
        "run-1".to_owned(),
        1,
        crate::PublicRunEventKind::Notice {
            message: "different".to_owned(),
        },
    );
    conflicting.payload_digest = crate::stable_event_hash(serde_json::to_vec(&conflicting.event)?);
    assert!(recorder.append_outbox(&conflicting).is_err());
    assert!(
        recorder
            .append_outbox(&entry_for(&session_id, "run-1", 1, "event-1-conflict"))
            .is_err()
    );
    assert!(
        recorder
            .append_outbox(&entry_for(&session_id, "run-1", 3, "event-3-gap"))
            .is_err()
    );

    drop(recorder);
    drop(store);
    let reopened = PublicEventOutboxRecorder::new(JsonlSessionStore::open_existing(&path)?);
    assert_eq!(reopened.durable_sequence("run-1")?, 1);
    assert!(!reopened.append_outbox(&first)?);
    assert!(reopened.append_outbox(&entry_for(&session_id, "run-1", 2, "event-2"))?);
    assert_eq!(reopened.durable_sequence("run-1")?, 2);
    Ok(())
}

#[test]
fn recorder_rejects_foreign_or_wrong_schema_public_event() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder = PublicEventOutboxRecorder::new(store.clone());
    let local_session_id = session_id(&store)?;
    assert!(
        recorder
            .append_outbox(&entry_for("other-session", "run-1", 1, "event-foreign"))
            .is_err()
    );

    let mut wrong_schema = entry_for(&local_session_id, "run-1", 1, "event-wrong-schema");
    wrong_schema.event.schema_version = crate::PUBLIC_RUN_EVENT_SCHEMA_VERSION + 1;
    wrong_schema.payload_digest =
        crate::stable_event_hash(serde_json::to_vec(&wrong_schema.event)?);
    assert!(recorder.append_outbox(&wrong_schema).is_err());
    Ok(())
}

#[test]
fn recorder_rejects_noncurrent_outbox_and_receipt_versions() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder = PublicEventOutboxRecorder::new(store.clone());
    let original = entry_for(&session_id(&store)?, "run-1", 1, "event-1");
    for version in [0, PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION + 1] {
        let mut unsupported = original.clone();
        unsupported.schema_version = version;
        let before = std::fs::read(store.path())?;
        assert!(recorder.append_outbox(&unsupported).is_err());
        assert_eq!(std::fs::read(store.path())?, before);
    }
    assert!(recorder.append_outbox(&original)?);
    for version in [0, PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION + 1] {
        let receipt = PublicEventDeliveryReceiptV1 {
            schema_version: version,
            public_event_id: original.public_event_id.clone(),
            adapter: "http".into(),
            delivered_at_unix_ms: 1,
        };
        let before = std::fs::read(store.path())?;
        assert!(recorder.append_delivery(&receipt).is_err());
        assert_eq!(std::fs::read(store.path())?, before);
    }
    Ok(())
}

#[test]
fn durable_outbox_rejects_transient_events_without_rewriting_source() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    let recorder = PublicEventOutboxRecorder::new(store.clone());
    let mut transient = entry_for(&session_id(&store)?, "run-1", 1, "transient-1");
    transient.event.event = crate::PublicRunEventKind::TextDelta {
        text: "unsupported durable preview".to_owned(),
    };
    transient.payload_digest = crate::stable_event_hash(serde_json::to_vec(&transient.event)?);
    let before = std::fs::read(&path)?;
    assert!(recorder.append_outbox(&transient).is_err());
    assert_eq!(std::fs::read(&path)?, before);
    let raw = StoredEvent::new(
        DurableEventType::PublicEventOutbox,
        EventClass::Critical,
        transient.public_event_id.clone(),
        transient.event.session_id.clone(),
        1,
        serde_json::to_value(transient)?,
    )?;
    assert!(
        PublicEventOutboxProjectionV1::from_records(&[SessionStreamRecord::Stored(raw.clone()),])
            .is_err()
    );
    drop(recorder);
    drop(store);
    let original = raw.to_json_line()?;
    std::fs::write(&path, original.as_bytes())?;
    let reopened = JsonlSessionStore::new(&path)?;
    assert!(reopened.read_event_records_writer().is_err());
    assert_eq!(std::fs::read(&path)?, original.as_bytes());
    Ok(())
}

#[test]
fn public_event_gap_is_rejected_by_bundle_writer_and_persisted_recovery() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    let gap = entry_for(&session_id(&store)?, "run-1", 2, "event-2");
    assert!(
        store
            .append_crash_safe_events_if(
                vec![(
                    gap.public_event_id.clone(),
                    DurableEventType::PublicEventOutbox,
                    EventClass::Critical,
                    serde_json::to_value(&gap)?,
                )],
                |_| Ok(true)
            )
            .is_err()
    );
    drop(store);
    // A structurally valid but semantically incomplete current-schema stream must not be
    // upcast by treating its first observed public sequence as an implicit initial watermark.
    let raw = StoredEvent::new(
        DurableEventType::PublicEventOutbox,
        EventClass::Critical,
        gap.public_event_id.clone(),
        gap.event.session_id.clone(),
        1,
        serde_json::to_value(gap)?,
    )?;
    let original = raw.to_json_line()?;
    std::fs::write(&path, original.as_bytes())?;
    let reopened = JsonlSessionStore::new(&path)?;
    assert!(reopened.read_event_records_writer().is_err());
    assert_eq!(std::fs::read(&path)?, original.as_bytes());
    assert!(
        PublicEventOutboxProjectionV1::from_records(&[SessionStreamRecord::Stored(raw)]).is_err()
    );
    Ok(())
}

#[test]
fn outbox_projection_rejects_mismatched_durable_envelope_identity() -> Result<()> {
    let entry = entry_for("session-1", "run-1", 1, "event-1");
    let mismatched_id = crate::StoredEvent::new(
        crate::DurableEventType::PublicEventOutbox,
        crate::EventClass::Critical,
        "other-envelope-id".to_owned(),
        "session-1".to_owned(),
        1,
        serde_json::to_value(entry.clone())?,
    )?;
    assert!(
        PublicEventOutboxProjectionV1::from_records(&[crate::SessionStreamRecord::Stored(
            mismatched_id
        ),])
        .is_err()
    );

    let mismatched_session = crate::StoredEvent::new(
        crate::DurableEventType::PublicEventOutbox,
        crate::EventClass::Critical,
        entry.public_event_id.clone(),
        "other-session".to_owned(),
        1,
        serde_json::to_value(entry)?,
    )?;
    assert!(
        PublicEventOutboxProjectionV1::from_records(&[crate::SessionStreamRecord::Stored(
            mismatched_session
        ),])
        .is_err()
    );
    Ok(())
}

#[test]
fn delivery_receipt_retry_retains_outbox_history() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder = PublicEventOutboxRecorder::new(store.clone());
    let session_id = session_id(&store)?;
    let event = entry_for(&session_id, "run-1", 1, "event-1");
    assert!(recorder.append_outbox(&event)?);

    // A failed adapter delivery leaves the entry pending. The later durable receipt must not
    // compact or otherwise remove it from the state-history projection.
    let before_receipt =
        PublicEventOutboxProjectionV1::from_records(&store.read_event_records_writer()?)?;
    assert_eq!(before_receipt.pending_for_adapter("http").len(), 1);
    let receipt = PublicEventDeliveryReceiptV1 {
        schema_version: PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
        public_event_id: event.public_event_id.clone(),
        adapter: "http".to_owned(),
        delivered_at_unix_ms: 1,
    };
    assert!(recorder.append_delivery(&receipt)?);
    assert!(!recorder.append_delivery(&receipt)?);

    let projection =
        PublicEventOutboxProjectionV1::from_records(&store.read_event_records_writer()?)?;
    assert!(projection.pending_for_adapter("http").is_empty());
    assert_eq!(projection.events_in_order().len(), 1);
    assert_eq!(
        projection.events_in_order()[0].public_event_id,
        event.public_event_id
    );
    Ok(())
}

#[test]
fn single_event_outbox_intent_recovers_exact_payload_after_each_write_fault() -> Result<()> {
    for fault in [
        SessionWriterFault::BeforeWrite,
        SessionWriterFault::PartialFirstRecord,
        SessionWriterFault::BeforeSync,
    ] {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("session.jsonl");
        let store = JsonlSessionStore::new(&path)?;
        let original = entry_for(&session_id(&store)?, "run-1", 1, "event-1");
        let recorder = PublicEventOutboxRecorder::new(store.clone());
        store.inject_writer_fault(fault)?;
        assert!(recorder.append_outbox(&original).is_err(), "{fault:?}");
        drop(recorder);
        drop(store);

        let reopened = JsonlSessionStore::new(&path)?;
        let recorder = PublicEventOutboxRecorder::new(reopened.clone());
        assert_eq!(recorder.durable_sequence("run-1")?, 1);
        assert!(!recorder.append_outbox(&original)?);
        let records = reopened.read_event_records_writer()?;
        let projection = PublicEventOutboxProjectionV1::from_records(&records)?;
        assert_eq!(records.len(), 1);
        assert_eq!(
            serde_json::to_value(projection.entry("event-1").expect("original event"))?,
            serde_json::to_value(&original)?
        );
        assert_eq!(projection.pending_for_adapter("http").len(), 1);
    }
    Ok(())
}

#[test]
fn single_event_receipt_intent_recovers_ack_without_rewriting_history() -> Result<()> {
    for fault in [
        SessionWriterFault::BeforeWrite,
        SessionWriterFault::PartialFirstRecord,
        SessionWriterFault::BeforeSync,
    ] {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("session.jsonl");
        let store = JsonlSessionStore::new(&path)?;
        let original = entry_for(&session_id(&store)?, "run-1", 1, "event-1");
        let recorder = PublicEventOutboxRecorder::new(store.clone());
        recorder.append_outbox(&original)?;
        let receipt = PublicEventDeliveryReceiptV1 {
            schema_version: PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
            public_event_id: original.public_event_id.clone(),
            adapter: "http".to_owned(),
            delivered_at_unix_ms: 10,
        };
        store.inject_writer_fault(fault)?;
        assert!(recorder.append_delivery(&receipt).is_err(), "{fault:?}");
        drop(recorder);
        drop(store);

        let reopened = JsonlSessionStore::open_existing(&path)?;
        let recorder = PublicEventOutboxRecorder::new(reopened.clone());
        assert!(!recorder.append_delivery(&receipt)?);
        let records = reopened.read_event_records_writer()?;
        let projection = PublicEventOutboxProjectionV1::from_records(&records)?;
        assert_eq!(records.len(), 2);
        assert_eq!(projection.events_in_order().len(), 1);
        assert!(projection.pending_for_adapter("http").is_empty());
        assert_eq!(projection.pending_for_adapter("tui").len(), 1);
        assert_eq!(
            serde_json::to_value(projection.entry("event-1").expect("original event"))?,
            serde_json::to_value(&original)?
        );
    }
    Ok(())
}

#[test]
fn recorder_incrementally_admits_public_event_deltas() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder = PublicEventOutboxRecorder::new(store.clone());
    let session_id = session_id(&store)?;
    assert!(recorder.append_outbox(&entry_for(&session_id, "run-1", 1, "event-1"))?);
    let scans_after_initial_append = store.writer_full_scan_count()?;

    for sequence in 2..=32 {
        assert!(recorder.append_outbox(&entry_for(
            &session_id,
            "run-1",
            sequence,
            &format!("event-{sequence}"),
        ))?);
    }

    assert_eq!(store.writer_full_scan_count()?, scans_after_initial_append);
    Ok(())
}

#[test]
fn failed_outbox_index_rebuild_cannot_reuse_a_stale_admission_cache() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    let session = crate::Session::new("provider", "model").with_store(store.clone());
    let lifecycle = session.conversation_run_lifecycle_recorder()?;
    lifecycle.append_started(&crate::ConversationRunStartedEntryV1::new("run-1", 1)?)?;
    let terminal = crate::ConversationRunFinalizedEntryV1::new(
        "run-1",
        crate::ConversationRunTerminalStatusV1::Succeeded,
        Some("message-1".to_owned()),
        None,
        2,
        &crate::SecretRedactor::empty(),
    )?;
    store.append_event(
        crate::DurableEventType::RunFinalized,
        crate::EventClass::Critical,
        serde_json::to_value(
            crate::ConversationRunLifecycleRecordV1::ConversationRunFinalizedV1(terminal),
        )?,
    )?;

    // The raw terminal is structurally valid but lacks its exact public-outbox pair. Reloading
    // must clear the previous index before pair validation fails; otherwise the next query could
    // silently admit against a stale pre-terminal cache.
    assert!(JsonlSessionStore::open_existing(&path).is_err());
    let recorder = PublicEventOutboxRecorder::new(store);
    assert!(recorder.durable_sequence("run-1").is_err());
    Ok(())
}
