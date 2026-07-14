use anyhow::Result;
use serde_json::json;

use super::*;
use crate::ConversationInputQueueId;

fn started(attempt_id: &str, parent: CompactionFallbackParent) -> CompactionStartedEntry {
    CompactionStartedEntry {
        attempt_id: attempt_id.to_owned(),
        fallback_parent: parent,
        initiation: CompactionInitiation::Manual,
        base_projection_revision: "projection-r1".to_owned(),
        started_at_unix_ms: 1,
    }
}

fn applied(attempt_id: &str, compaction_id: &str) -> CompactionAppliedV2 {
    CompactionAppliedV2 {
        compaction_id: compaction_id.to_owned(),
        attempt_id: attempt_id.to_owned(),
        parent_compaction_id: None,
        branch_id: None,
        valid_for_snapshot: None,
        task_memory_id: None,
        checkpoint: ContinuationCheckpointV1::empty(),
        base_projection_revision: "projection-r1".to_owned(),
        folded_through: CompactionCursor {
            session_id: "session-compaction".to_owned(),
            through_stream_sequence: 1,
            through_event_id: "event-start".to_owned(),
        },
        applied_at_unix_ms: 2,
    }
}

fn failed(attempt_id: &str) -> CompactionFailureEntry {
    CompactionFailureEntry {
        attempt_id: attempt_id.to_owned(),
        reason: CompactionFailureReason::ValidationFailed,
        failed_at_unix_ms: 2,
    }
}

fn lifecycle_event(
    event_type: DurableEventType,
    event_id: &str,
    stream_sequence: u64,
    payload: serde_json::Value,
    correlation_id: Option<&str>,
    causation_id: Option<&str>,
) -> StoredEvent {
    let mut event = StoredEvent::new(
        event_type,
        event_type
            .expected_event_class()
            .expect("known lifecycle event has a class"),
        event_id.to_owned(),
        "session-compaction".to_owned(),
        stream_sequence,
        payload,
    )
    .expect("lifecycle event should build");
    event.correlation_id = correlation_id.map(str::to_owned);
    event.causation_id = causation_id.map(str::to_owned);
    event.record_checksum = event
        .compute_record_checksum()
        .expect("lifecycle checksum should compute");
    event
}

#[test]
fn compaction_v2_projection_accepts_one_started_and_applied_terminal() -> Result<()> {
    let start = lifecycle_event(
        DurableEventType::CompactionStarted,
        "event-start",
        1,
        serde_json::to_value(started("attempt-1", CompactionFallbackParent::Root))?,
        Some("event-start"),
        None,
    );
    let applied = lifecycle_event(
        DurableEventType::CompactionAppliedV2,
        "event-applied",
        2,
        serde_json::to_value(applied("attempt-1", "compaction-1"))?,
        Some("event-start"),
        Some("event-start"),
    );

    let projection = CompactionLifecycleProjection::from_records(&[
        SessionStreamRecord::Stored(start),
        SessionStreamRecord::Stored(applied),
    ])?;

    let attempt = projection
        .attempt("attempt-1")
        .expect("started attempt should project");
    assert!(matches!(
        attempt.terminal,
        Some(CompactionAttemptTerminal::Applied { .. })
    ));
    assert_eq!(
        projection
            .cursor()
            .map(|cursor| cursor.last_applied_stream_sequence),
        Some(2)
    );
    Ok(())
}

#[test]
fn idle_automatic_failure_latch_survives_projection_reload_for_the_same_scope() -> Result<()> {
    let scope_fingerprint = "idle-scope-v1";
    let started = CompactionStartedEntry {
        attempt_id: "idle-attempt-1".to_owned(),
        fallback_parent: CompactionFallbackParent::Root,
        initiation: CompactionInitiation::IdleAutomatic {
            scope_fingerprint: scope_fingerprint.to_owned(),
        },
        base_projection_revision: "projection-r1".to_owned(),
        started_at_unix_ms: 1,
    };
    let failed = failed("idle-attempt-1");
    let records = vec![
        SessionStreamRecord::Stored(lifecycle_event(
            DurableEventType::CompactionStarted,
            "event-idle-start",
            1,
            serde_json::to_value(started)?,
            Some("event-idle-start"),
            None,
        )),
        SessionStreamRecord::Stored(lifecycle_event(
            DurableEventType::CompactionFailed,
            "event-idle-failed",
            2,
            serde_json::to_value(failed)?,
            Some("event-idle-start"),
            Some("event-idle-start"),
        )),
    ];

    let reloaded = CompactionLifecycleProjection::from_records(&records)?;
    assert!(reloaded.has_failed_idle_automatic_scope(scope_fingerprint));
    assert!(!reloaded.has_failed_idle_automatic_scope("different-fold-material"));
    Ok(())
}

#[test]
fn pre_turn_pressure_initiation_is_content_free_and_validates_its_queue_id() -> Result<()> {
    let started = CompactionStartedEntry {
        attempt_id: "pre-turn-attempt-1".to_owned(),
        fallback_parent: CompactionFallbackParent::Root,
        initiation: CompactionInitiation::PreTurnPressure {
            queue_id: ConversationInputQueueId::new("queue_1")?,
        },
        base_projection_revision: "projection-r1".to_owned(),
        started_at_unix_ms: 1,
    };
    let payload = serde_json::to_value(&started)?;
    assert_eq!(payload["initiation"]["kind"], "pre_turn_pressure");
    assert_eq!(payload["initiation"]["queue_id"], "queue_1");

    let projection = CompactionLifecycleProjection::from_records(&[SessionStreamRecord::Stored(
        lifecycle_event(
            DurableEventType::CompactionStarted,
            "event-pre-turn-start",
            1,
            payload,
            Some("event-pre-turn-start"),
            None,
        ),
    )])?;
    assert!(projection.attempt("pre-turn-attempt-1").is_some());

    let malformed = json!({
        "attempt_id": "pre-turn-attempt-2",
        "fallback_parent": {"kind": "root"},
        "initiation": {"kind": "pre_turn_pressure", "queue_id": "not/a-queue-id"},
        "base_projection_revision": "projection-r1",
        "started_at_unix_ms": 1,
    });
    let malformed: CompactionStartedEntry = serde_json::from_value(malformed)?;
    assert!(malformed.validate_shape().is_err());
    Ok(())
}

#[test]
fn started_entry_without_initiation_is_rejected_without_a_legacy_default() {
    let error = serde_json::from_value::<CompactionStartedEntry>(serde_json::json!({
        "attempt_id": "legacy-attempt",
        "fallback_parent": { "kind": "root" },
        "base_projection_revision": "projection-r1",
        "started_at_unix_ms": 1
    }))
    .expect_err("pre-release V2 does not accept a legacy started payload");
    assert!(error.to_string().contains("initiation"));
}

#[test]
fn compaction_v2_allows_a_fold_cursor_from_before_the_started_barrier() -> Result<()> {
    let source = StoredEvent::new(
        DurableEventType::UserMessageRecorded,
        EventClass::Critical,
        "event-source".to_owned(),
        "session-compaction".to_owned(),
        1,
        serde_json::json!({
            "session_log_entry": SessionLogEntry::User(ModelMessage::user("old request")),
        }),
    )?;
    let start = lifecycle_event(
        DurableEventType::CompactionStarted,
        "event-start",
        2,
        serde_json::to_value(started("attempt-1", CompactionFallbackParent::Root))?,
        Some("event-start"),
        None,
    );
    let mut entry = applied("attempt-1", "compaction-1");
    entry.folded_through = CompactionCursor {
        session_id: "session-compaction".to_owned(),
        through_stream_sequence: source.stream_sequence,
        through_event_id: source.event_id.clone(),
    };
    let applied = lifecycle_event(
        DurableEventType::CompactionAppliedV2,
        "event-applied",
        3,
        serde_json::to_value(entry)?,
        Some("event-start"),
        Some("event-start"),
    );

    let projection = CompactionLifecycleProjection::from_records(&[
        SessionStreamRecord::Stored(source),
        SessionStreamRecord::Stored(start),
        SessionStreamRecord::Stored(applied),
    ])?;
    assert!(matches!(
        projection
            .attempt("attempt-1")
            .expect("attempt is present")
            .terminal,
        Some(CompactionAttemptTerminal::Applied { .. })
    ));
    Ok(())
}

#[test]
fn compaction_v2_projection_rejects_second_terminal() -> Result<()> {
    let start = lifecycle_event(
        DurableEventType::CompactionStarted,
        "event-start",
        1,
        serde_json::to_value(started("attempt-1", CompactionFallbackParent::Root))?,
        Some("event-start"),
        None,
    );
    let applied = lifecycle_event(
        DurableEventType::CompactionAppliedV2,
        "event-applied",
        2,
        serde_json::to_value(applied("attempt-1", "compaction-1"))?,
        Some("event-start"),
        Some("event-start"),
    );
    let failed = lifecycle_event(
        DurableEventType::CompactionFailed,
        "event-failed",
        3,
        serde_json::to_value(failed("attempt-1"))?,
        Some("event-start"),
        Some("event-start"),
    );

    let error = CompactionLifecycleProjection::from_records(&[
        SessionStreamRecord::Stored(start),
        SessionStreamRecord::Stored(applied),
        SessionStreamRecord::Stored(failed),
    ])
    .expect_err("a second terminal must fail closed");

    assert!(error.to_string().contains("already has a terminal"));
    Ok(())
}

#[test]
fn compaction_v2_fallback_requires_a_prior_failed_attempt() -> Result<()> {
    let parent_start = lifecycle_event(
        DurableEventType::CompactionStarted,
        "event-parent-start",
        1,
        serde_json::to_value(started("attempt-parent", CompactionFallbackParent::Root))?,
        Some("event-parent-start"),
        None,
    );
    let parent_failed = lifecycle_event(
        DurableEventType::CompactionFailed,
        "event-parent-failed",
        2,
        serde_json::to_value(failed("attempt-parent"))?,
        Some("event-parent-start"),
        Some("event-parent-start"),
    );
    let child_start = lifecycle_event(
        DurableEventType::CompactionStarted,
        "event-child-start",
        3,
        serde_json::to_value(started(
            "attempt-child",
            CompactionFallbackParent::InitiatedAttempt {
                attempt_id: "attempt-parent".to_owned(),
            },
        ))?,
        Some("event-child-start"),
        None,
    );

    let projection = CompactionLifecycleProjection::from_records(&[
        SessionStreamRecord::Stored(parent_start),
        SessionStreamRecord::Stored(parent_failed),
        SessionStreamRecord::Stored(child_start),
    ])?;
    assert!(projection.attempt("attempt-child").is_some());

    let missing_parent = lifecycle_event(
        DurableEventType::CompactionStarted,
        "event-orphan-start",
        1,
        serde_json::to_value(started(
            "attempt-orphan",
            CompactionFallbackParent::InitiatedAttempt {
                attempt_id: "attempt-missing".to_owned(),
            },
        ))?,
        Some("event-orphan-start"),
        None,
    );
    let error =
        CompactionLifecycleProjection::from_records(&[SessionStreamRecord::Stored(missing_parent)])
            .expect_err("missing fallback parent must fail closed");
    assert!(
        error
            .to_string()
            .contains("fallback parent attempt-missing is missing")
    );
    Ok(())
}

#[test]
fn compaction_v2_typed_decode_uses_direct_payload_schema() -> Result<()> {
    let event = lifecycle_event(
        DurableEventType::CompactionStarted,
        "event-start",
        1,
        serde_json::to_value(started("attempt-1", CompactionFallbackParent::Root))?,
        Some("event-start"),
        None,
    );

    let TypedStoredEventDecode::Known(event) = decode_typed_stored_event(event)? else {
        panic!("known compaction start should decode");
    };
    let TypedDomainEvent::CompactionStarted(entry) = *event else {
        panic!("compaction start must not use the generic event fallback");
    };
    assert_eq!(entry.attempt_id, "attempt-1");
    Ok(())
}

#[test]
fn compaction_v2_direct_payload_schema_rejects_unknown_fields() -> Result<()> {
    let mut payload = serde_json::to_value(started("attempt-1", CompactionFallbackParent::Root))?;
    payload["future_compatibility_field"] = json!(true);

    let error = decode_typed_stored_event(lifecycle_event(
        DurableEventType::CompactionStarted,
        "event-start",
        1,
        payload,
        Some("event-start"),
        None,
    ))
    .expect_err("strict compaction payloads must reject unknown fields");

    assert!(format!("{error:#}").contains("unknown field"));

    let mut payload = serde_json::to_value(started(
        "attempt-1",
        CompactionFallbackParent::InitiatedAttempt {
            attempt_id: "attempt-parent".to_owned(),
        },
    ))?;
    payload["fallback_parent"]["unexpected_nested_field"] = json!(true);
    let error = decode_typed_stored_event(lifecycle_event(
        DurableEventType::CompactionStarted,
        "event-start",
        1,
        payload,
        Some("event-start"),
        None,
    ))
    .expect_err("strict compaction payloads must reject nested unknown fields");
    assert!(format!("{error:#}").contains("unknown field"));
    Ok(())
}

#[test]
fn compaction_v2_projection_rejects_broken_lifecycle_correlation_chain() -> Result<()> {
    let start = lifecycle_event(
        DurableEventType::CompactionStarted,
        "event-start",
        1,
        serde_json::to_value(started("attempt-1", CompactionFallbackParent::Root))?,
        Some("event-start"),
        None,
    );
    let applied = lifecycle_event(
        DurableEventType::CompactionAppliedV2,
        "event-applied",
        2,
        serde_json::to_value(applied("attempt-1", "compaction-1"))?,
        Some("wrong-root"),
        Some("wrong-root"),
    );

    let error = CompactionLifecycleProjection::from_records(&[
        SessionStreamRecord::Stored(start),
        SessionStreamRecord::Stored(applied),
    ])
    .expect_err("a terminal must bind to the durable start root");

    assert!(error.to_string().contains("correlation id must reference"));
    Ok(())
}

#[test]
fn compaction_v2_read_projection_is_read_only_and_explicit_recovery_is_idempotent() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("compaction.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    store.append_compaction_started(started("attempt-1", CompactionFallbackParent::Root))?;

    let before_read = std::fs::read(&path)?;
    let records = JsonlSessionStore::read_event_records(&path)?;
    let projection = CompactionLifecycleProjection::from_records(&records)?;
    assert_eq!(projection.unfinished_attempts().len(), 1);
    assert_eq!(std::fs::read(&path)?, before_read);

    assert_eq!(store.recover_unfinished_compaction_attempts(50)?, 1);
    let after_recovery = std::fs::read(&path)?;
    assert_eq!(store.recover_unfinished_compaction_attempts(60)?, 0);
    assert_eq!(std::fs::read(&path)?, after_recovery);

    let records = JsonlSessionStore::read_event_records(&path)?;
    let projection = CompactionLifecycleProjection::from_records(&records)?;
    let attempt = projection
        .attempt("attempt-1")
        .expect("recovered attempt should remain projected");
    assert!(matches!(
        attempt.terminal,
        Some(CompactionAttemptTerminal::Failed {
            entry: CompactionFailureEntry {
                reason: CompactionFailureReason::RecoveryInterrupted,
                ..
            },
            ..
        })
    ));
    Ok(())
}

#[test]
fn compaction_v2_store_binds_terminal_to_started_correlation_chain() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("compaction-chain.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    let start =
        store.append_compaction_started(started("attempt-1", CompactionFallbackParent::Root))?;
    let applied = store.append_compaction_applied_v2(CompactionAppliedV2 {
        folded_through: CompactionCursor {
            session_id: start.session_id.clone(),
            through_stream_sequence: start.stream_sequence,
            through_event_id: start.event_id.clone(),
        },
        ..applied("attempt-1", "compaction-1")
    })?;

    assert_eq!(
        applied.correlation_id.as_deref(),
        Some(start.event_id.as_str())
    );
    assert_eq!(
        applied.causation_id.as_deref(),
        Some(start.event_id.as_str())
    );
    assert!(
        JsonlSessionStore::read_event_records(&path)?
            .iter()
            .any(|record| {
                record.stored_event().event_type == DurableEventType::CompactionAppliedV2.as_str()
            })
    );
    Ok(())
}

#[test]
fn compaction_v2_direct_payloads_do_not_accept_session_entry_wrappers() {
    let error = decode_typed_stored_event(lifecycle_event(
        DurableEventType::CompactionStarted,
        "event-start",
        1,
        json!({ "session_log_entry": { "control": {} } }),
        Some("event-start"),
        None,
    ))
    .expect_err("compaction lifecycle must reject generic session entry wrappers");

    assert!(error.to_string().contains("typed payload"));
}
