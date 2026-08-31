use anyhow::Result;
use serde_json::{Value, json};

use super::test_fixtures::terminal_outbox;
use super::*;
use crate::{
    DurableEventType, EventClass, JsonlSessionStore, MessageRole, ModelMessage, SecretRedactor,
    Session, SessionLogEntry, SessionStreamRecord, StoredEvent, safe_persistence_text,
};

fn lifecycle_stream_record(
    event_type: DurableEventType,
    payload: Value,
    sequence: u64,
) -> Result<SessionStreamRecord> {
    Ok(SessionStreamRecord::Stored(StoredEvent::new(
        event_type,
        EventClass::Critical,
        format!("event-{sequence}"),
        "session-1".to_owned(),
        sequence,
        payload,
    )?))
}

fn started(run_id: &str, started_at_ms: u64) -> Result<ConversationRunStartedEntryV1> {
    ConversationRunStartedEntryV1::new(run_id, started_at_ms)
}

fn succeeded(run_id: &str, finalized_at_ms: u64) -> Result<ConversationRunFinalizedEntryV1> {
    ConversationRunFinalizedEntryV1::new(
        run_id,
        ConversationRunTerminalStatusV1::Succeeded,
        Some(format!("message-{run_id}")),
        Some("completed"),
        finalized_at_ms,
        &SecretRedactor::empty(),
    )
}

#[test]
fn recorder_retries_exact_start_and_terminal_as_no_ops() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let session = Session::new("provider", "model").with_store(store.clone());
    let recorder = session.conversation_run_lifecycle_recorder()?;
    let start = started("run-1", 10)?;
    let final_entry = succeeded("run-1", 20)?;

    assert!(recorder.append_started(&start)?);
    assert!(!recorder.append_started(&start)?);
    let outbox = terminal_outbox(session.session_scope_id(), &final_entry)?;
    assert!(recorder.append_finalized_with_outbox(&final_entry, &outbox)?);
    assert!(!recorder.append_finalized_with_outbox(&final_entry, &outbox)?);
    assert!(
        !recorder.append_finalized_with_outbox(&succeeded("run-1", 21)?, &outbox)?,
        "a retry of the same terminal intent must keep the first durable timestamp"
    );

    let lifecycle = JsonlSessionStore::read_event_records(store.path())?
        .iter()
        .filter_map(|record| conversation_run_lifecycle_record_from_stream(record).transpose())
        .collect::<Result<Vec<_>>>()?;
    assert_eq!(
        lifecycle,
        vec![
            ConversationRunLifecycleRecordV1::ConversationRunStartedV1(start),
            ConversationRunLifecycleRecordV1::ConversationRunFinalizedV1(final_entry),
        ]
    );
    Ok(())
}

#[test]
fn recorder_recovers_one_unfinished_run_and_rejects_overlapping_history() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let session = Session::new("provider", "model").with_store(store.clone());
    let recorder = session.conversation_run_lifecycle_recorder()?;

    assert!(!recorder.reconcile_unfinished(10)?);
    assert!(recorder.append_started(&started("run-1", 11)?)?);
    assert!(recorder.reconcile_unfinished(12)?);
    assert!(!recorder.reconcile_unfinished(13)?);

    let lifecycle = JsonlSessionStore::read_event_records(store.path())?
        .iter()
        .filter_map(|record| conversation_run_lifecycle_record_from_stream(record).transpose())
        .collect::<Result<Vec<_>>>()?;
    assert!(matches!(
        lifecycle.as_slice(),
        [
            ConversationRunLifecycleRecordV1::ConversationRunStartedV1(_),
            ConversationRunLifecycleRecordV1::ConversationRunFinalizedV1(finalized),
        ] if finalized.status() == ConversationRunTerminalStatusV1::Interrupted
    ));

    let overlapping = tempfile::tempdir()?;
    let overlapping_store = JsonlSessionStore::new(overlapping.path().join("session.jsonl"))?;
    let overlapping_session =
        Session::new("provider", "model").with_store(overlapping_store.clone());
    let overlapping_recorder = overlapping_session.conversation_run_lifecycle_recorder()?;
    assert!(overlapping_recorder.append_started(&started("run-1", 20)?)?);
    assert!(overlapping_recorder.append_started(&started("run-2", 21)?)?);
    assert!(
        overlapping_recorder
            .reconcile_unfinished(22)
            .expect_err("overlapping active runs must fail closed")
            .to_string()
            .contains("overlapping active runs")
    );
    Ok(())
}

#[test]
fn recorder_rejects_missing_start_and_conflicting_reuse() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let session = Session::new("provider", "model").with_store(store);
    let recorder = session.conversation_run_lifecycle_recorder()?;

    let missing = succeeded("missing-run", 20)?;
    assert!(
        recorder
            .append_finalized_with_outbox(
                &missing,
                &terminal_outbox(session.session_scope_id(), &missing)?
            )
            .expect_err("terminal without start must fail")
            .to_string()
            .contains("matching durable start")
    );

    assert!(recorder.append_started(&started("run-1", 10)?)?);
    assert!(
        recorder
            .append_started(&started("run-1", 11)?)
            .expect_err("conflicting start must fail")
            .to_string()
            .contains("conflicting start")
    );

    let terminal = succeeded("run-1", 20)?;
    assert!(recorder.append_finalized_with_outbox(
        &terminal,
        &terminal_outbox(session.session_scope_id(), &terminal)?
    )?);
    let conflict = ConversationRunFinalizedEntryV1::new(
        "run-1",
        ConversationRunTerminalStatusV1::Failed,
        None,
        Some("failed"),
        21,
        &SecretRedactor::empty(),
    )?;
    assert!(
        recorder
            .append_finalized_with_outbox(
                &conflict,
                &terminal_outbox(session.session_scope_id(), &conflict)?
            )
            .expect_err("conflicting terminal must fail")
            .to_string()
            .contains("conflicting terminal")
    );
    Ok(())
}

#[test]
fn decoder_is_strict_for_unknown_fields_tags_and_phase_mismatch() -> Result<()> {
    let start = started("run-1", 10)?;
    let mut payload = serde_json::to_value(
        ConversationRunLifecycleRecordV1::ConversationRunStartedV1(start.clone()),
    )?;
    payload
        .as_object_mut()
        .expect("lifecycle payload should be an object")
        .insert("unexpected".to_owned(), json!(true));
    let record = lifecycle_stream_record(DurableEventType::RunStatusChanged, payload, 1)?;
    assert!(
        format!(
            "{:#}",
            conversation_run_lifecycle_record_from_stream(&record)
                .expect_err("unknown lifecycle field must fail")
        )
        .contains("unknown field")
    );

    let unknown = lifecycle_stream_record(
        DurableEventType::RunStatusChanged,
        json!({"record": "future_conversation_run_v2"}),
        1,
    )?;
    assert!(
        conversation_run_lifecycle_record_from_stream(&unknown)
            .expect_err("unknown critical lifecycle tag must fail")
            .to_string()
            .contains("unknown critical run lifecycle record")
    );

    let wrong_phase = lifecycle_stream_record(
        DurableEventType::RunFinalized,
        serde_json::to_value(ConversationRunLifecycleRecordV1::ConversationRunStartedV1(
            start,
        ))?,
        1,
    )?;
    assert!(
        conversation_run_lifecycle_record_from_stream(&wrong_phase)
            .expect_err("start in terminal event must fail")
            .to_string()
            .contains("start must use run_status_changed")
    );
    Ok(())
}

#[test]
fn decoder_preserves_existing_kernel_and_cancellation_lifecycle_payloads() -> Result<()> {
    let existing = lifecycle_stream_record(
        DurableEventType::RunFinalized,
        json!({
            "run_status": "completed",
            "terminal_reason": "completed",
            "final_message_id": "message-1",
            "tool_calls": 0,
            "error": null,
        }),
        1,
    )?;
    assert!(conversation_run_lifecycle_record_from_stream(&existing)?.is_none());

    let cancellation = lifecycle_stream_record(
        DurableEventType::RunStatusChanged,
        json!({
            "record": "requested",
            "request_id": "cancel-1",
            "run_scope_id": "run-1",
            "target": {"kind": "run"},
            "reason": "user request",
            "requested_at_ms": 10,
            "quiescence_deadline_ms": 20,
        }),
        1,
    )?;
    assert!(conversation_run_lifecycle_record_from_stream(&cancellation)?.is_none());
    Ok(())
}

#[test]
fn unknown_critical_envelopes_fail_all_canonical_decoders() -> Result<()> {
    let record = SessionStreamRecord::Stored(StoredEvent::new_raw(
        "future_recovery_boundary",
        EventClass::Critical,
        "event-1".to_owned(),
        "session-1".to_owned(),
        1,
        json!({"value": true}),
    )?);

    assert!(
        conversation_run_lifecycle_record_from_stream(&record)
            .expect_err("conversation lifecycle decoder must reject unknown critical events")
            .to_string()
            .contains("unknown critical event")
    );
    assert!(
        record
            .session_log_entry()
            .expect_err("session entry decoder must reject unknown critical events")
            .to_string()
            .contains("unknown critical event")
    );
    Ok(())
}

#[test]
fn canonical_decoders_reject_tampered_envelopes_before_payload_projection() -> Result<()> {
    let mut record = lifecycle_stream_record(
        DurableEventType::RunStatusChanged,
        serde_json::to_value(ConversationRunLifecycleRecordV1::ConversationRunStartedV1(
            started("run-1", 10)?,
        ))?,
        1,
    )?;
    let SessionStreamRecord::Stored(event) = &mut record;
    event.record_checksum = "sha256:jcs-v1:tampered".to_owned();

    assert!(
        conversation_run_lifecycle_record_from_stream(&record)
            .expect_err("conversation lifecycle decoder must verify the envelope")
            .to_string()
            .contains("checksum mismatch")
    );
    assert!(
        record
            .session_log_entry()
            .expect_err("session entry decoder must verify the envelope")
            .to_string()
            .contains("checksum mismatch")
    );
    Ok(())
}

#[test]
fn terminal_summary_is_redacted_bounded_and_utf8_safe() -> Result<()> {
    let secret = "super-secret-token";
    let redactor = SecretRedactor::from_values([secret]);
    let summary = format!(
        "authorization=Bearer {secret} result={} https://example.com/?token={secret}",
        "界".repeat(2_000)
    );
    let entry = ConversationRunFinalizedEntryV1::new(
        "run-1",
        ConversationRunTerminalStatusV1::Failed,
        None,
        Some(&summary),
        20,
        &redactor,
    )?;

    let safe_summary = entry.safe_summary().expect("summary should remain");
    assert!(!safe_summary.contains(secret));
    assert!(safe_summary.len() <= MAX_CONVERSATION_RUN_SUMMARY_BYTES);
    assert!(entry.summary_truncated());
    assert_eq!(safe_persistence_text(safe_summary), safe_summary);
    assert!(safe_summary.is_char_boundary(safe_summary.len()));
    Ok(())
}

#[test]
fn persisted_reopen_keeps_lifecycle_idempotent_and_session_decoding_canonical() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    crate::session::append_current_test_session_identity(&store)?;
    let mut session = Session::new("provider", "model").with_store(store.clone());
    session.append_user_message(ModelMessage::user("durable request"))?;
    let recorder = session.conversation_run_lifecycle_recorder()?;
    let start = started("run-reopen", 10)?;
    let final_entry = succeeded("run-reopen", 20)?;
    assert!(recorder.append_started(&start)?);
    let outbox = terminal_outbox(session.session_scope_id(), &final_entry)?;
    assert!(recorder.append_finalized_with_outbox(&final_entry, &outbox)?);
    drop(recorder);
    drop(session);

    let reopened = Session::load_from_store("fallback-provider", "fallback-model", store.clone())?;
    let recorder = reopened.conversation_run_lifecycle_recorder()?;
    assert!(!recorder.append_started(&start)?);
    assert!(!recorder.append_finalized_with_outbox(&final_entry, &outbox)?);

    let records = JsonlSessionStore::read_event_records(store.path())?;
    let lifecycle = records
        .iter()
        .filter_map(|record| conversation_run_lifecycle_record_from_stream(record).transpose())
        .collect::<Result<Vec<_>>>()?;
    assert_eq!(lifecycle.len(), 2);

    let decoded_entries = records
        .iter()
        .filter_map(|record| record.session_log_entry().transpose())
        .collect::<Result<Vec<_>>>()?;
    assert!(decoded_entries.iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::User(message)
                if message.role == MessageRole::User
                    && message.content.as_deref() == Some("durable request")
        )
    }));
    Ok(())
}

#[test]
fn terminal_bundle_recovers_exact_domain_and_outbox_after_torn_append() -> Result<()> {
    use crate::session::SessionWriterFault;
    for fault in [
        SessionWriterFault::BeforeWrite,
        SessionWriterFault::PartialFirstRecord,
        SessionWriterFault::PartialSecondRecord,
        SessionWriterFault::BeforeSync,
    ] {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("session.jsonl");
        let store = JsonlSessionStore::new(&path)?;
        let session = Session::new("provider", "model").with_store(store.clone());
        let recorder = session.conversation_run_lifecycle_recorder()?;
        recorder.append_started(&started("run-atomic", 10)?)?;
        let terminal = succeeded("run-atomic", 20)?;
        let outbox = terminal_outbox(session.session_scope_id(), &terminal)?;
        store.inject_writer_fault(fault)?;
        assert!(
            recorder
                .append_finalized_with_outbox(&terminal, &outbox)
                .is_err(),
            "{fault:?}"
        );
        drop(recorder);
        drop(session);
        drop(store);

        let reopened = JsonlSessionStore::new(&path)?;
        let session = Session::new("provider", "model").with_store(reopened.clone());
        let recorder = session.conversation_run_lifecycle_recorder()?;
        assert_eq!(
            recorder.finalized_for_run("run-atomic")?,
            Some(terminal.clone()),
            "durable query must recover the original prepared terminal: {fault:?}"
        );
        assert!(
            !recorder.reconcile_unfinished(30)?,
            "must recover the committed outcome, not invent Interrupted: {fault:?}"
        );
        assert!(!recorder.append_finalized_with_outbox(&terminal, &outbox)?);
        let records = JsonlSessionStore::read_event_records(&path)?;
        assert_eq!(
            records
                .iter()
                .filter(|record| record.stored_event().event_id == outbox.domain_event_id)
                .count(),
            1
        );
        assert_eq!(
            records
                .iter()
                .filter(|record| record.stored_event().event_id == outbox.public_event_id)
                .count(),
            1
        );
        let projection = PublicEventOutboxProjectionV1::from_records(&records)?;
        assert_eq!(
            serde_json::to_value(projection.entry(&outbox.public_event_id))?,
            serde_json::to_value(Some(&outbox))?
        );
        assert_eq!(projection.pending_for_adapter("http").len(), 1);
    }
    Ok(())
}

#[test]
fn terminal_bundle_rejects_split_writes_wrong_outcome_and_cross_session() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let session = Session::new("provider", "model").with_store(store.clone());
    let recorder = session.conversation_run_lifecycle_recorder()?;
    recorder.append_started(&started("run-1", 10)?)?;
    let terminal = succeeded("run-1", 20)?;
    let outbox = terminal_outbox(session.session_scope_id(), &terminal)?;
    assert!(
        crate::PublicEventOutboxRecorder::new(store.clone())
            .append_outbox(&outbox)
            .is_err()
    );
    let wrong_session = terminal_outbox("different-session", &terminal)?;
    assert!(
        recorder
            .append_finalized_with_outbox(&terminal, &wrong_session)
            .is_err()
    );
    let failed = ConversationRunFinalizedEntryV1::new(
        "run-1",
        ConversationRunTerminalStatusV1::Failed,
        None,
        Some("failure"),
        20,
        &SecretRedactor::empty(),
    )?;
    assert!(
        recorder
            .append_finalized_with_outbox(&failed, &outbox)
            .is_err()
    );

    // A valid, old split record is rejected, never used to fabricate public history.
    store.append_event(
        DurableEventType::RunFinalized,
        EventClass::Critical,
        serde_json::to_value(
            ConversationRunLifecycleRecordV1::ConversationRunFinalizedV1(terminal.clone()),
        )?,
    )?;
    assert!(
        recorder
            .append_finalized_with_outbox(&terminal, &outbox)
            .is_err()
    );
    assert!(
        PublicEventOutboxProjectionV1::from_records(&JsonlSessionStore::read_event_records(
            store.path()
        )?)
        .is_err()
    );
    Ok(())
}

#[test]
fn all_terminal_outcomes_commit_without_collapsing_statuses() -> Result<()> {
    for status in [
        ConversationRunTerminalStatusV1::Succeeded,
        ConversationRunTerminalStatusV1::Failed,
        ConversationRunTerminalStatusV1::Cancelled,
        ConversationRunTerminalStatusV1::Interrupted,
        ConversationRunTerminalStatusV1::Paused,
        ConversationRunTerminalStatusV1::Blocked,
        ConversationRunTerminalStatusV1::AwaitingUserInput,
    ] {
        let temp = tempfile::tempdir()?;
        let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
        let session = Session::new("provider", "model").with_store(store.clone());
        let recorder = session.conversation_run_lifecycle_recorder()?;
        recorder.append_started(&started("run-1", 10)?)?;
        let terminal = ConversationRunFinalizedEntryV1::new(
            "run-1",
            status,
            (status == ConversationRunTerminalStatusV1::Succeeded)
                .then(|| "final-message".to_owned()),
            Some("terminal summary"),
            20,
            &SecretRedactor::empty(),
        )?;
        let outbox = terminal_outbox(session.session_scope_id(), &terminal)?;
        assert!(recorder.append_finalized_with_outbox(&terminal, &outbox)?);
        assert!(!recorder.append_finalized_with_outbox(&terminal, &outbox)?);
        let records = JsonlSessionStore::read_event_records(store.path())?;
        let lifecycle = conversation_run_lifecycle_state(&records)?;
        assert_eq!(
            lifecycle["run-1"]
                .finalized
                .as_ref()
                .map(|entry| entry.status()),
            Some(status)
        );
    }
    Ok(())
}

#[test]
fn replay_projection_rejects_outbox_only_and_mismatched_terminal_pairs() -> Result<()> {
    let terminal = succeeded("run-1", 20)?;
    let outbox = terminal_outbox("session-1", &terminal)?;
    let start = StoredEvent::new(
        DurableEventType::RunStatusChanged,
        EventClass::Critical,
        "start-1".to_owned(),
        "session-1".to_owned(),
        1,
        serde_json::to_value(ConversationRunLifecycleRecordV1::ConversationRunStartedV1(
            started("run-1", 10)?,
        ))?,
    )?;
    let public = StoredEvent::new(
        DurableEventType::PublicEventOutbox,
        EventClass::Critical,
        outbox.public_event_id.clone(),
        "session-1".to_owned(),
        3,
        serde_json::to_value(&outbox)?,
    )?;
    assert!(
        PublicEventOutboxProjectionV1::from_records(&[SessionStreamRecord::Stored(public.clone())])
            .is_err()
    );
    let domain = StoredEvent::new(
        DurableEventType::RunFinalized,
        EventClass::Critical,
        outbox.domain_event_id.clone(),
        "session-1".to_owned(),
        2,
        serde_json::to_value(
            ConversationRunLifecycleRecordV1::ConversationRunFinalizedV1(terminal.clone()),
        )?,
    )?;
    let exact = vec![
        SessionStreamRecord::Stored(start.clone()),
        SessionStreamRecord::Stored(domain.clone()),
        SessionStreamRecord::Stored(public.clone()),
    ];
    assert_eq!(
        PublicEventOutboxProjectionV1::from_records(&exact)?
            .pending_for_adapter("http")
            .len(),
        1
    );
    let mut duplicate_domain = domain.clone();
    duplicate_domain.event_id = "second-domain".to_owned();
    duplicate_domain.stream_sequence = 4;
    duplicate_domain.record_checksum = duplicate_domain.compute_record_checksum()?;
    let mut duplicate_outbox = outbox.clone();
    duplicate_outbox.domain_event_id = duplicate_domain.event_id.clone();
    duplicate_outbox.public_event_id = "second-public".to_owned();
    duplicate_outbox.sequence = 2;
    duplicate_outbox.event.sequence = 2;
    duplicate_outbox.payload_digest =
        crate::stable_event_hash(serde_json::to_vec(&duplicate_outbox.event)?);
    let duplicate_public = StoredEvent::new(
        DurableEventType::PublicEventOutbox,
        EventClass::Critical,
        duplicate_outbox.public_event_id.clone(),
        "session-1".to_owned(),
        5,
        serde_json::to_value(&duplicate_outbox)?,
    )?;
    let mut duplicate_terminals = exact.clone();
    duplicate_terminals.extend([
        SessionStreamRecord::Stored(duplicate_domain),
        SessionStreamRecord::Stored(duplicate_public),
    ]);
    assert!(PublicEventOutboxProjectionV1::from_records(&duplicate_terminals).is_err());
    for mutation in 0..3 {
        let mut wrong = domain.clone();
        match mutation {
            0 => wrong.event_id = "different-domain-id".to_owned(),
            1 => wrong.session_id = "different-session".to_owned(),
            _ => {
                let failed = ConversationRunFinalizedEntryV1::new(
                    "run-1",
                    ConversationRunTerminalStatusV1::Failed,
                    None,
                    Some("failure"),
                    20,
                    &SecretRedactor::empty(),
                )?;
                wrong.payload = serde_json::to_value(
                    ConversationRunLifecycleRecordV1::ConversationRunFinalizedV1(failed),
                )?;
            }
        }
        wrong.record_checksum = wrong.compute_record_checksum()?;
        assert!(
            PublicEventOutboxProjectionV1::from_records(&[
                SessionStreamRecord::Stored(start.clone()),
                SessionStreamRecord::Stored(wrong),
                SessionStreamRecord::Stored(public.clone())
            ])
            .is_err()
        );
    }
    Ok(())
}
