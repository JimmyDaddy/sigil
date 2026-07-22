use anyhow::Result;
use serde_json::{Value, json};

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
    assert!(recorder.append_finalized(&final_entry)?);
    assert!(!recorder.append_finalized(&final_entry)?);

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
fn recorder_rejects_missing_start_and_conflicting_reuse() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let session = Session::new("provider", "model").with_store(store);
    let recorder = session.conversation_run_lifecycle_recorder()?;

    let missing = succeeded("missing-run", 20)?;
    assert!(
        recorder
            .append_finalized(&missing)
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

    assert!(recorder.append_finalized(&succeeded("run-1", 20)?)?);
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
            .append_finalized(&conflict)
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
    let mut session = Session::new("provider", "model").with_store(store.clone());
    session.append_user_message(ModelMessage::user("durable request"))?;
    let recorder = session.conversation_run_lifecycle_recorder()?;
    let start = started("run-reopen", 10)?;
    let final_entry = succeeded("run-reopen", 20)?;
    assert!(recorder.append_started(&start)?);
    assert!(recorder.append_finalized(&final_entry)?);
    drop(recorder);
    drop(session);

    let reopened = Session::load_from_store("fallback-provider", "fallback-model", store.clone())?;
    let recorder = reopened.conversation_run_lifecycle_recorder()?;
    assert!(!recorder.append_started(&start)?);
    assert!(!recorder.append_finalized(&final_entry)?);

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
