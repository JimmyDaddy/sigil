use anyhow::Result;

use super::*;

const SESSION_ID: &str = "session-compaction";

fn started() -> CompactionStartedEntry {
    CompactionStartedEntry {
        attempt_id: "attempt-1".to_owned(),
        fallback_parent: CompactionFallbackParent::Root,
        initiation: CompactionInitiation::Manual,
        base_projection_revision: "projection-r1".to_owned(),
        started_at_unix_ms: 1,
    }
}

fn memory() -> TaskMemoryV1 {
    TaskMemoryV1 {
        memory_id: "memory-1".to_owned(),
        branch_id: Some("main".to_owned()),
        valid_for_snapshot: "snapshot-1".to_owned(),
        supersedes: None,
        source_event_ids: vec!["event-source".to_owned()],
        objective: "Keep the durable compaction contract narrow".to_owned(),
        active_plan: None,
        constraints: Vec::new(),
        decisions: Vec::new(),
        files_changed: Vec::new(),
        commands_run: Vec::new(),
        verification_results: Vec::new(),
        failed_attempts: Vec::new(),
        risks: Vec::new(),
        unresolved_issues: Vec::new(),
    }
}

fn event(
    event_type: DurableEventType,
    event_id: &str,
    sequence: u64,
    payload: serde_json::Value,
    correlation_id: Option<&str>,
    causation_id: Option<&str>,
) -> StoredEvent {
    let mut event = StoredEvent::new(
        event_type,
        event_type
            .expected_event_class()
            .expect("known event class"),
        event_id.to_owned(),
        SESSION_ID.to_owned(),
        sequence,
        payload,
    )
    .expect("valid sidecar event");
    event.correlation_id = correlation_id.map(str::to_owned);
    event.causation_id = causation_id.map(str::to_owned);
    event.record_checksum = event.compute_record_checksum().expect("checksum");
    event
}

fn applied(memory_id: Option<&str>) -> CompactionAppliedV2 {
    CompactionAppliedV2 {
        compaction_id: "compaction-1".to_owned(),
        attempt_id: "attempt-1".to_owned(),
        parent_compaction_id: None,
        branch_id: Some("main".to_owned()),
        valid_for_snapshot: Some("snapshot-1".to_owned()),
        task_memory_id: memory_id.map(str::to_owned),
        checkpoint: match memory_id {
            Some(memory_id) => ContinuationCheckpointV1::bound_to(memory_id, "snapshot-1"),
            None => ContinuationCheckpointV1::empty(),
        },
        base_projection_revision: "projection-r1".to_owned(),
        folded_through: CompactionCursor {
            session_id: SESSION_ID.to_owned(),
            through_stream_sequence: 1,
            through_event_id: "event-start".to_owned(),
        },
        applied_at_unix_ms: 3,
    }
}

fn recorded() -> TaskMemoryRecordedV1 {
    TaskMemoryRecordedV1::new(
        CompactionCursor {
            session_id: SESSION_ID.to_owned(),
            through_stream_sequence: 1,
            through_event_id: "event-start".to_owned(),
        },
        memory(),
    )
    .expect("valid memory sidecar")
}

#[test]
fn sidecar_is_inactive_until_matching_applied_v2_then_resolves_by_branch() -> Result<()> {
    let start = event(
        DurableEventType::CompactionStarted,
        "event-start",
        1,
        serde_json::to_value(started())?,
        Some("event-start"),
        None,
    );
    let memory = event(
        DurableEventType::TaskMemoryRecordedV1,
        "event-memory",
        2,
        serde_json::to_value(recorded())?,
        Some("event-start"),
        Some("event-start"),
    );

    let TypedStoredEventDecode::Known(typed) = decode_typed_stored_event(memory.clone())? else {
        panic!("known task memory sidecar should decode");
    };
    assert!(matches!(*typed, TypedDomainEvent::TaskMemoryRecordedV1(_)));

    let inactive = CompactionSidecarProjection::from_records(&[
        SessionStreamRecord::Stored(start.clone()),
        SessionStreamRecord::Stored(memory.clone()),
    ])?;
    assert!(inactive.latest_for_branch(Some("main")).is_none());

    let applied = event(
        DurableEventType::CompactionAppliedV2,
        "event-applied",
        3,
        serde_json::to_value(applied(Some("memory-1")))?,
        Some("event-start"),
        Some("event-start"),
    );
    let projection = CompactionSidecarProjection::from_records(&[
        SessionStreamRecord::Stored(start),
        SessionStreamRecord::Stored(memory),
        SessionStreamRecord::Stored(applied),
    ])?;
    let resolved = projection
        .latest_for_branch(Some("main"))
        .expect("AppliedV2 activates matching memory");
    assert_eq!(resolved.task_memory.memory_id, "memory-1");
    assert_eq!(
        resolved.checkpoint.task_memory_id.as_deref(),
        Some("memory-1")
    );
    Ok(())
}

#[test]
fn orphan_task_memory_does_not_activate_and_invalidated_memory_is_not_resolved() -> Result<()> {
    let start = event(
        DurableEventType::CompactionStarted,
        "event-start",
        1,
        serde_json::to_value(started())?,
        Some("event-start"),
        None,
    );
    let orphan = event(
        DurableEventType::TaskMemoryRecordedV1,
        "event-memory",
        2,
        serde_json::to_value(recorded())?,
        Some("event-start"),
        Some("event-start"),
    );
    let applied = event(
        DurableEventType::CompactionAppliedV2,
        "event-applied",
        3,
        serde_json::to_value(applied(Some("memory-1")))?,
        Some("event-start"),
        Some("event-start"),
    );
    let invalidated = event(
        DurableEventType::TaskMemoryInvalidated,
        "event-invalidated",
        4,
        serde_json::to_value(TaskMemoryInvalidatedEntry {
            task_memory_id: "memory-1".to_owned(),
            reason: TaskMemoryInvalidationReason::Explicit,
            invalidated_by_event_id: "event-applied".to_owned(),
        })?,
        Some("event-start"),
        Some("event-start"),
    );

    let projection = CompactionSidecarProjection::from_records(&[
        SessionStreamRecord::Stored(start.clone()),
        SessionStreamRecord::Stored(orphan.clone()),
    ])?;
    assert!(projection.latest_for_branch(Some("main")).is_none());

    let projection = CompactionSidecarProjection::from_records(&[
        SessionStreamRecord::Stored(start),
        SessionStreamRecord::Stored(orphan),
        SessionStreamRecord::Stored(applied),
        SessionStreamRecord::Stored(invalidated),
    ])?;
    assert!(projection.latest_for_branch(Some("main")).is_none());
    Ok(())
}

#[test]
fn applied_v2_referencing_missing_or_wrong_lineage_memory_fails_closed() -> Result<()> {
    let start = event(
        DurableEventType::CompactionStarted,
        "event-start",
        1,
        serde_json::to_value(started())?,
        Some("event-start"),
        None,
    );
    let applied = event(
        DurableEventType::CompactionAppliedV2,
        "event-applied",
        2,
        serde_json::to_value(applied(Some("memory-missing")))?,
        Some("event-start"),
        Some("event-start"),
    );
    let error = CompactionSidecarProjection::from_records(&[
        SessionStreamRecord::Stored(start),
        SessionStreamRecord::Stored(applied),
    ])
    .expect_err("AppliedV2 cannot activate an absent sidecar");
    assert!(error.to_string().contains("missing task memory"));
    Ok(())
}

#[test]
fn sidecar_rejects_tampered_payload_identity_and_branch_mismatch() -> Result<()> {
    let start = event(
        DurableEventType::CompactionStarted,
        "event-start",
        1,
        serde_json::to_value(started())?,
        Some("event-start"),
        None,
    );
    let mut tampered_record = recorded();
    tampered_record.content_hash = "sha256:jcs-v1:tampered".to_owned();
    let tampered = event(
        DurableEventType::TaskMemoryRecordedV1,
        "event-memory",
        2,
        serde_json::to_value(tampered_record)?,
        Some("event-start"),
        Some("event-start"),
    );
    let error = decode_typed_stored_event(tampered)
        .expect_err("typed task memory decode must verify the payload identity");
    assert!(format!("{error:#}").contains("content hash"));

    let memory = event(
        DurableEventType::TaskMemoryRecordedV1,
        "event-memory",
        2,
        serde_json::to_value(recorded())?,
        Some("event-start"),
        Some("event-start"),
    );
    let mut wrong_branch = applied(Some("memory-1"));
    wrong_branch.branch_id = Some("other".to_owned());
    let applied = event(
        DurableEventType::CompactionAppliedV2,
        "event-applied",
        3,
        serde_json::to_value(wrong_branch)?,
        Some("event-start"),
        Some("event-start"),
    );
    let error = CompactionSidecarProjection::from_records(&[
        SessionStreamRecord::Stored(start),
        SessionStreamRecord::Stored(memory),
        SessionStreamRecord::Stored(applied),
    ])
    .expect_err("sidecar activation must not cross branches");
    assert!(error.to_string().contains("branch does not match"));
    Ok(())
}

#[test]
fn store_writes_sidecar_before_applied_and_invalidation_keeps_audit_lineage() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("compaction-sidecar.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    let start = store.append_compaction_started(started())?;
    let record = store.append_task_memory_recorded_v1(
        "attempt-1",
        TaskMemoryRecordedV1::new(
            CompactionCursor {
                session_id: start.session_id.clone(),
                through_stream_sequence: start.stream_sequence,
                through_event_id: start.event_id.clone(),
            },
            memory(),
        )?,
    )?;
    assert_eq!(
        record.correlation_id.as_deref(),
        Some(start.event_id.as_str())
    );
    assert_eq!(
        record.causation_id.as_deref(),
        Some(start.event_id.as_str())
    );

    let applied = store.append_compaction_applied_v2(CompactionAppliedV2 {
        folded_through: CompactionCursor {
            session_id: start.session_id.clone(),
            through_stream_sequence: start.stream_sequence,
            through_event_id: start.event_id.clone(),
        },
        ..applied(Some("memory-1"))
    })?;
    let invalidated = store.append_task_memory_invalidated(TaskMemoryInvalidatedEntry {
        task_memory_id: "memory-1".to_owned(),
        reason: TaskMemoryInvalidationReason::Explicit,
        invalidated_by_event_id: applied.event_id,
    })?;
    assert_eq!(
        invalidated.correlation_id.as_deref(),
        Some(start.event_id.as_str())
    );
    assert_eq!(
        invalidated.causation_id.as_deref(),
        Some(start.event_id.as_str())
    );

    let records = JsonlSessionStore::read_event_records(&path)?;
    let projection = CompactionSidecarProjection::from_records(&records)?;
    assert!(projection.latest_for_branch(Some("main")).is_none());
    Ok(())
}

#[test]
fn superseding_memory_cannot_follow_an_earlier_parent_invalidation() -> Result<()> {
    let parent_start_entry = CompactionStartedEntry {
        attempt_id: "attempt-parent".to_owned(),
        fallback_parent: CompactionFallbackParent::Root,
        initiation: CompactionInitiation::Manual,
        base_projection_revision: "projection-r1".to_owned(),
        started_at_unix_ms: 1,
    };
    let parent_start = event(
        DurableEventType::CompactionStarted,
        "event-parent-start",
        1,
        serde_json::to_value(parent_start_entry)?,
        Some("event-parent-start"),
        None,
    );
    let parent_memory = event(
        DurableEventType::TaskMemoryRecordedV1,
        "event-parent-memory",
        2,
        serde_json::to_value(TaskMemoryRecordedV1::new(
            CompactionCursor {
                session_id: SESSION_ID.to_owned(),
                through_stream_sequence: 1,
                through_event_id: "event-parent-start".to_owned(),
            },
            TaskMemoryV1 {
                memory_id: "memory-parent".to_owned(),
                ..memory()
            },
        )?)?,
        Some("event-parent-start"),
        Some("event-parent-start"),
    );
    let parent_applied = event(
        DurableEventType::CompactionAppliedV2,
        "event-parent-applied",
        3,
        serde_json::to_value(CompactionAppliedV2 {
            compaction_id: "compaction-parent".to_owned(),
            attempt_id: "attempt-parent".to_owned(),
            parent_compaction_id: None,
            branch_id: Some("main".to_owned()),
            valid_for_snapshot: Some("snapshot-1".to_owned()),
            task_memory_id: Some("memory-parent".to_owned()),
            checkpoint: ContinuationCheckpointV1::bound_to("memory-parent", "snapshot-1"),
            base_projection_revision: "projection-r1".to_owned(),
            folded_through: CompactionCursor {
                session_id: SESSION_ID.to_owned(),
                through_stream_sequence: 1,
                through_event_id: "event-parent-start".to_owned(),
            },
            applied_at_unix_ms: 3,
        })?,
        Some("event-parent-start"),
        Some("event-parent-start"),
    );
    let parent_invalidated = event(
        DurableEventType::TaskMemoryInvalidated,
        "event-parent-invalidated",
        4,
        serde_json::to_value(TaskMemoryInvalidatedEntry {
            task_memory_id: "memory-parent".to_owned(),
            reason: TaskMemoryInvalidationReason::Explicit,
            invalidated_by_event_id: "event-parent-applied".to_owned(),
        })?,
        Some("event-parent-start"),
        Some("event-parent-start"),
    );
    let child_start_entry = CompactionStartedEntry {
        attempt_id: "attempt-child".to_owned(),
        fallback_parent: CompactionFallbackParent::Root,
        initiation: CompactionInitiation::Manual,
        base_projection_revision: "projection-r1".to_owned(),
        started_at_unix_ms: 5,
    };
    let child_start = event(
        DurableEventType::CompactionStarted,
        "event-child-start",
        5,
        serde_json::to_value(child_start_entry)?,
        Some("event-child-start"),
        None,
    );
    let child_memory = event(
        DurableEventType::TaskMemoryRecordedV1,
        "event-child-memory",
        6,
        serde_json::to_value(TaskMemoryRecordedV1::new(
            CompactionCursor {
                session_id: SESSION_ID.to_owned(),
                through_stream_sequence: 5,
                through_event_id: "event-child-start".to_owned(),
            },
            TaskMemoryV1 {
                memory_id: "memory-child".to_owned(),
                supersedes: Some("memory-parent".to_owned()),
                ..memory()
            },
        )?)?,
        Some("event-child-start"),
        Some("event-child-start"),
    );
    let child_applied = event(
        DurableEventType::CompactionAppliedV2,
        "event-child-applied",
        7,
        serde_json::to_value(CompactionAppliedV2 {
            compaction_id: "compaction-child".to_owned(),
            attempt_id: "attempt-child".to_owned(),
            parent_compaction_id: Some("compaction-parent".to_owned()),
            branch_id: Some("main".to_owned()),
            valid_for_snapshot: Some("snapshot-1".to_owned()),
            task_memory_id: Some("memory-child".to_owned()),
            checkpoint: ContinuationCheckpointV1::bound_to("memory-child", "snapshot-1"),
            base_projection_revision: "projection-r1".to_owned(),
            folded_through: CompactionCursor {
                session_id: SESSION_ID.to_owned(),
                through_stream_sequence: 5,
                through_event_id: "event-child-start".to_owned(),
            },
            applied_at_unix_ms: 7,
        })?,
        Some("event-child-start"),
        Some("event-child-start"),
    );

    let error = CompactionSidecarProjection::from_records(&[
        SessionStreamRecord::Stored(parent_start),
        SessionStreamRecord::Stored(parent_memory),
        SessionStreamRecord::Stored(parent_applied),
        SessionStreamRecord::Stored(parent_invalidated),
        SessionStreamRecord::Stored(child_start),
        SessionStreamRecord::Stored(child_memory),
        SessionStreamRecord::Stored(child_applied),
    ])
    .expect_err("invalidated parent sidecar cannot satisfy a later supersedes lineage");
    assert!(error.to_string().contains("parent has no active sidecar"));
    Ok(())
}
