use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::Write,
    process::Command,
    sync::{Arc, Barrier},
    thread,
    time::Instant,
};

use anyhow::Result;
use fs2::FileExt;

use super::*;
use crate::{
    ModelMessage, MutationEventRecorder, MutationPrepared, MutationSubject, MutationSyncClass,
    Session, SnapshotCoverage,
};

fn audit_record(
    _event_type: DurableEventType,
    record_id: &str,
    correlation_id: &str,
) -> Result<DurableAuditRecord> {
    Ok(DurableAuditRecord::new(
        DurableEventType::MutationPrepared,
        serde_json::to_value(test_mutation_prepared(record_id, None))?,
        record_id,
        Some(correlation_id.to_owned()),
    )?)
}

fn record_expectation(
    _event_type: DurableEventType,
    record_id: &str,
    correlation_id: &str,
) -> Result<DurableAppendRecordExpectation> {
    Ok(DurableAppendRecordExpectation::new(
        DurableEventType::MutationPrepared,
        record_id,
        Some(correlation_id.to_owned()),
    )?)
}

fn linked_audit_record(
    record_id: &str,
    event_id: &str,
    correlation_id: &str,
    causation_id: Option<&str>,
) -> Result<DurableAuditRecord> {
    let record = DurableAuditRecord::new(
        DurableEventType::MutationPrepared,
        serde_json::to_value(test_mutation_prepared(record_id, None))?,
        record_id,
        Some(correlation_id.to_owned()),
    )?
    .with_event_id(event_id)?;
    match causation_id {
        Some(causation_id) => Ok(record.with_causation_id(causation_id)?),
        None => Ok(record),
    }
}

fn linked_record_expectation(
    record_id: &str,
    event_id: &str,
    correlation_id: &str,
    causation_id: Option<&str>,
) -> Result<DurableAppendRecordExpectation> {
    let expectation = DurableAppendRecordExpectation::new(
        DurableEventType::MutationPrepared,
        record_id,
        Some(correlation_id.to_owned()),
    )?
    .with_event_id(event_id)?;
    match causation_id {
        Some(causation_id) => Ok(expectation.with_causation_id(causation_id)?),
        None => Ok(expectation),
    }
}

fn audit_record_with_authorization(
    _event_type: DurableEventType,
    record_id: &str,
    correlation_id: Option<&str>,
    authorization_id: &str,
) -> Result<DurableAuditRecord> {
    Ok(DurableAuditRecord::new(
        DurableEventType::MutationPrepared,
        serde_json::to_value(test_mutation_prepared(record_id, Some(authorization_id)))?,
        record_id,
        correlation_id.map(str::to_owned),
    )?
    .with_authorization_id(authorization_id)?)
}

fn test_mutation_prepared(record_id: &str, authorization_id: Option<&str>) -> MutationPrepared {
    MutationPrepared {
        operation_id: record_id.to_owned(),
        batch_id: None,
        tool_call_id: authorization_id.map(str::to_owned),
        causation_event_id: format!("cause-{record_id}"),
        subject: MutationSubject::External {
            description: format!("strict audit fixture {record_id}"),
        },
        before_hash: None,
        intended_after_hash: None,
        snapshot_coverage: SnapshotCoverage::NoPriorContent,
        workspace_id: "workspace-writer-test".to_owned(),
        base_workspace_revision: 0,
        sync_class: MutationSyncClass::RecoveryCritical,
    }
}

fn store_session_id(store: &JsonlSessionStore) -> String {
    session_id_for_path(store.path())
}

#[test]
fn session_writer_hot_append_scans_existing_stream_once() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;

    for index in 0..64 {
        let event = store.append_event(
            DurableEventType::RunStatusChanged,
            EventClass::Critical,
            serde_json::json!({ "index": index }),
        )?;
        assert_eq!(event.stream_sequence, index + 1);
    }

    assert_eq!(store.writer_full_scan_count()?, 1);
    assert_eq!(
        JsonlSessionStore::read_event_records(store.path())?.len(),
        64
    );
    Ok(())
}

#[test]
#[ignore = "release-profile long-session performance evidence"]
fn session_writer_long_session_evidence() -> Result<()> {
    const EVENT_COUNT: usize = 10_000;

    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(store.clone());

    let append_started = Instant::now();
    for index in 0..EVENT_COUNT {
        session.append_user_message(ModelMessage::user(format!("long-session-{index}")))?;
    }
    let append_elapsed_ms = append_started.elapsed().as_millis();

    assert_eq!(store.writer_full_scan_count()?, 1);
    let replay_started = Instant::now();
    let records = JsonlSessionStore::read_event_records(store.path())?;
    let replay_elapsed_ms = replay_started.elapsed().as_millis();
    assert_eq!(records.len(), EVENT_COUNT);
    assert_eq!(
        records
            .iter()
            .map(SessionStreamRecord::stream_sequence)
            .collect::<Vec<_>>(),
        (1..=EVENT_COUNT as u64).collect::<Vec<_>>()
    );

    println!(
        "SIGIL_LONG_SESSION_EVIDENCE {}",
        serde_json::json!({
            "schema_version": 1,
            "scenario": "session_writer_10k",
            "scale": EVENT_COUNT,
            "elapsed_ms": append_elapsed_ms.saturating_add(replay_elapsed_ms),
            "facts": {
                "append_elapsed_ms": append_elapsed_ms,
                "replay_elapsed_ms": replay_elapsed_ms,
                "full_scan_count": store.writer_full_scan_count()?,
                "record_count": records.len(),
                "file_bytes": fs::metadata(store.path())?.len(),
            }
        })
    );
    Ok(())
}

#[test]
fn session_writer_cloned_mutation_recorders_share_one_linear_sequence() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let recorder = MutationEventRecorder::new(store.clone());
    let workers = 12;
    let barrier = Arc::new(Barrier::new(workers));
    let mut handles = Vec::new();

    for index in 0..workers {
        let recorder = recorder.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || -> Result<StoredEvent> {
            barrier.wait();
            recorder.append_batch_started(
                &format!("batch-{index}"),
                &format!("operation-{index}"),
                &[MutationSubject::Workspace {
                    scope_hash: format!("scope-{index}"),
                }],
            )
        }));
    }

    let mut events = handles
        .into_iter()
        .map(|handle| handle.join().expect("writer thread should not panic"))
        .collect::<Result<Vec<_>>>()?;
    events.sort_by_key(|event| event.stream_sequence);

    assert_eq!(
        events
            .iter()
            .map(|event| event.stream_sequence)
            .collect::<Vec<_>>(),
        (1..=workers as u64).collect::<Vec<_>>()
    );
    assert_eq!(
        events
            .iter()
            .map(|event| event.event_id.as_str())
            .collect::<BTreeSet<_>>()
            .len(),
        workers
    );
    assert_eq!(
        events
            .iter()
            .map(|event| event.record_checksum.as_str())
            .collect::<BTreeSet<_>>()
            .len(),
        workers
    );
    assert_eq!(store.writer_full_scan_count()?, 1);
    Ok(())
}

#[test]
fn session_writer_batch_receipt_binds_order_offsets_and_envelope_correlation() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let first = audit_record_with_authorization(
        DurableEventType::RunStatusChanged,
        "record-1",
        None,
        "auth-1",
    )?;
    let second = audit_record_with_authorization(
        DurableEventType::RunFinalized,
        "record-2",
        Some("corr-2"),
        "auth-2",
    )?;
    let receipt =
        store.append_batch_and_sync(DurableAuditBatch::new("batch-1", vec![first, second])?)?;

    assert_eq!(receipt.records().len(), 2);
    assert_eq!(receipt.records()[0].stream_sequence(), 1);
    assert_eq!(receipt.records()[1].stream_sequence(), 2);
    assert_eq!(
        receipt.records()[0].end_offset(),
        receipt.records()[1].start_offset()
    );
    assert_eq!(
        receipt.records()[1].end_offset(),
        receipt.durable_end_offset()
    );

    let records = JsonlSessionStore::read_event_records(store.path())?;
    let correlations = records
        .iter()
        .map(|record| match record {
            SessionStreamRecord::Stored(event) => event.correlation_id.as_deref(),
        })
        .collect::<Vec<_>>();
    assert_eq!(correlations, vec![None, Some("corr-2")]);

    let expectation = DurableAppendExpectation::new(
        receipt.session_id(),
        "batch-1",
        vec![
            DurableAppendRecordExpectation::new(
                DurableEventType::MutationPrepared,
                "record-1",
                None,
            )?
            .with_authorization_id("auth-1")?,
            record_expectation(DurableEventType::RunFinalized, "record-2", "corr-2")?
                .with_authorization_id("auth-2")?,
        ],
    )?;
    let permit = store.validate_and_consume(receipt, expectation)?;
    assert_eq!(permit.batch_id(), "batch-1");
    assert_eq!(
        permit.durable_end_offset(),
        fs::metadata(store.path())?.len()
    );
    Ok(())
}

#[test]
fn session_writer_preallocated_identity_and_causation_are_exactly_bound() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let root_event_id = "event-compaction-root";
    let child_event_id = "event-compaction-child";
    let receipt = store.append_batch_and_sync(DurableAuditBatch::new(
        "batch-linked",
        vec![
            linked_audit_record("record-linked-root", root_event_id, root_event_id, None)?,
            linked_audit_record(
                "record-linked-child",
                child_event_id,
                root_event_id,
                Some(root_event_id),
            )?,
        ],
    )?)?;

    assert_eq!(receipt.records()[0].event_id(), root_event_id);
    assert_eq!(receipt.records()[0].correlation_id(), Some(root_event_id));
    assert_eq!(receipt.records()[0].causation_id(), None);
    assert_eq!(receipt.records()[1].event_id(), child_event_id);
    assert_eq!(receipt.records()[1].correlation_id(), Some(root_event_id));
    assert_eq!(receipt.records()[1].causation_id(), Some(root_event_id));

    let records = JsonlSessionStore::read_event_records(store.path())?;
    let root = records[0].stored_event();
    let child = records[1].stored_event();
    assert_eq!(root.event_id, root_event_id);
    assert_eq!(root.correlation_id.as_deref(), Some(root_event_id));
    assert_eq!(root.causation_id, None);
    assert_eq!(child.event_id, child_event_id);
    assert_eq!(child.correlation_id.as_deref(), Some(root_event_id));
    assert_eq!(child.causation_id.as_deref(), Some(root_event_id));

    let expectation = DurableAppendExpectation::new(
        receipt.session_id(),
        "batch-linked",
        vec![
            linked_record_expectation("record-linked-root", root_event_id, root_event_id, None)?,
            linked_record_expectation(
                "record-linked-child",
                child_event_id,
                root_event_id,
                Some(root_event_id),
            )?,
        ],
    )?;
    store.validate_and_consume(receipt, expectation)?;

    let mismatch_receipt = store.append_and_sync(linked_audit_record(
        "record-linked-mismatch",
        "event-linked-mismatch",
        root_event_id,
        Some(child_event_id),
    )?)?;
    let mismatch_expectation = DurableAppendExpectation::new(
        mismatch_receipt.session_id(),
        "record-linked-mismatch",
        vec![linked_record_expectation(
            "record-linked-mismatch",
            "event-linked-mismatch",
            root_event_id,
            Some(root_event_id),
        )?],
    )?;
    assert!(matches!(
        store.validate_and_consume(mismatch_receipt, mismatch_expectation),
        Err(DurableAuditError::ReceiptMismatch)
    ));
    Ok(())
}

#[test]
fn session_writer_preallocated_receipt_requires_exact_event_id_expectation() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let receipt = store.append_and_sync(linked_audit_record(
        "record-receipt-event-id",
        "event-receipt-event-id",
        "event-receipt-event-id",
        None,
    )?)?;
    let expectation = DurableAppendExpectation::new(
        receipt.session_id(),
        "record-receipt-event-id",
        vec![DurableAppendRecordExpectation::new(
            DurableEventType::MutationPrepared,
            "record-receipt-event-id",
            Some("event-receipt-event-id".to_owned()),
        )?],
    )?;

    assert!(matches!(
        store.validate_and_consume(receipt, expectation),
        Err(DurableAuditError::ReceiptMismatch)
    ));
    Ok(())
}

#[test]
fn session_writer_rejects_orphaned_preallocated_correlation_root() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;

    assert!(matches!(
        store.append_and_sync(linked_audit_record(
            "record-orphaned-root",
            "event-orphaned-root",
            "not-a-durable-event-id",
            None,
        )?),
        Err(DurableAuditError::AppendFailed(_))
    ));
    assert!(JsonlSessionStore::read_event_records(store.path())?.is_empty());
    Ok(())
}

#[test]
fn session_writer_rejects_causal_chain_with_non_event_correlation() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let unanchored = store.append_and_sync(audit_record(
        DurableEventType::MutationPrepared,
        "record-unanchored-correlation",
        "external-correlation-id",
    )?)?;
    let predecessor = unanchored.records()[0].event_id().to_owned();

    assert!(matches!(
        store.append_and_sync(linked_audit_record(
            "record-unanchored-child",
            "event-unanchored-child",
            "external-correlation-id",
            Some(&predecessor),
        )?),
        Err(DurableAuditError::AppendFailed(_))
    ));
    Ok(())
}

#[test]
fn session_writer_preallocated_chain_reuses_event_link_index() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let root_event_id = "event-index-root";
    store.append_and_sync(linked_audit_record(
        "record-index-root",
        root_event_id,
        root_event_id,
        None,
    )?)?;

    let mut predecessor = root_event_id.to_owned();
    for index in 0..16 {
        let event_id = format!("event-index-child-{index}");
        store.append_and_sync(linked_audit_record(
            &format!("record-index-child-{index}"),
            &event_id,
            root_event_id,
            Some(&predecessor),
        )?)?;
        predecessor = event_id;
    }

    assert_eq!(store.writer_full_scan_count()?, 1);
    Ok(())
}

#[test]
fn session_writer_conditional_preallocated_append_reuses_loaded_link_index() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let receipt = store
        .append_audit_batch_if(
            DurableAuditBatch::new(
                "batch-conditional-index",
                vec![linked_audit_record(
                    "record-conditional-index",
                    "event-conditional-index",
                    "event-conditional-index",
                    None,
                )?],
            )?,
            |_| Ok(true),
        )?
        .expect("conditional append should produce a receipt");

    assert_eq!(receipt.records()[0].event_id(), "event-conditional-index");
    assert_eq!(store.writer_full_scan_count()?, 1);
    Ok(())
}

#[test]
fn session_writer_reconciles_exact_absent_and_conflicting_events() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let record = linked_audit_record(
        "record-ambiguous",
        "event-ambiguous",
        "event-ambiguous",
        None,
    )?;
    let exact = record.reconciliation_expectation(store_session_id(&store))?;
    store.inject_writer_fault(SessionWriterFault::BeforeSync)?;
    assert!(matches!(
        store.append_and_sync(record),
        Err(DurableAuditError::AppendFailed(_))
    ));
    assert!(matches!(
        store.append_and_sync(linked_audit_record(
            "record-ambiguous",
            "event-ambiguous",
            "event-ambiguous",
            None,
        )?),
        Err(DurableAuditError::AppendFailed(_))
    ));
    assert!(matches!(
        store.append_and_sync(linked_audit_record(
            "record-ambiguous-conflict",
            "event-ambiguous",
            "event-other-chain",
            None,
        )?),
        Err(DurableAuditError::AppendFailed(_))
    ));

    let DurableEventReconciliation::ExactPresent(event) = store.reconcile_durable_event(&exact)
    else {
        panic!("complete pre-sync write should reconcile as exact present");
    };
    assert_eq!(event.event_id, "event-ambiguous");
    assert_eq!(event.correlation_id.as_deref(), Some("event-ambiguous"));

    let absent_record = linked_audit_record("record-absent", "event-absent", "event-absent", None)?;
    let absent = absent_record.reconciliation_expectation(store_session_id(&store))?;
    assert!(matches!(
        store.reconcile_durable_event(&absent),
        DurableEventReconciliation::ConfirmedAbsent
    ));

    let conflicting = DurableEventReconciliationExpectation::new(
        store_session_id(&store),
        DurableEventType::MutationPrepared,
        "event-ambiguous",
        serde_json::to_value(test_mutation_prepared("record-conflict", None))?,
        Some("event-ambiguous".to_owned()),
        None,
    )?;
    assert!(matches!(
        store.reconcile_durable_event(&conflicting),
        DurableEventReconciliation::Conflict { .. }
    ));
    Ok(())
}

#[test]
fn session_writer_reconciliation_is_bound_to_the_expected_session() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let first = JsonlSessionStore::new(temp.path().join("first.jsonl"))?;
    let second = JsonlSessionStore::new(temp.path().join("second.jsonl"))?;
    let record = linked_audit_record(
        "record-session-bound",
        "event-session-bound",
        "event-session-bound",
        None,
    )?;
    let expectation = record.reconciliation_expectation(store_session_id(&first))?;
    first.append_and_sync(record)?;
    second.append_and_sync(linked_audit_record(
        "record-session-bound",
        "event-session-bound",
        "event-session-bound",
        None,
    )?)?;

    let writer: &dyn DurableAuditWriter = &second;
    assert!(matches!(
        writer.reconcile_event(&expectation),
        DurableEventReconciliation::Conflict { .. }
    ));
    Ok(())
}

#[test]
fn session_writer_reconciliation_recovers_partial_tail_as_confirmed_absent() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let record = linked_audit_record(
        "record-partial-reconcile",
        "event-partial-reconcile",
        "event-partial-reconcile",
        None,
    )?;
    let expectation = record.reconciliation_expectation(store_session_id(&store))?;
    store.inject_writer_fault(SessionWriterFault::PartialFirstRecord)?;
    assert!(matches!(
        store.append_and_sync(record),
        Err(DurableAuditError::AppendFailed(_))
    ));

    assert!(matches!(
        store.reconcile_durable_event(&expectation),
        DurableEventReconciliation::ConfirmedAbsent
    ));
    let records = JsonlSessionStore::read_event_records(store.path())?;
    assert!(
        records
            .iter()
            .all(|record| record.event_id() != expectation.event_id())
    );
    assert!(records.iter().any(|record| {
        matches!(
            record,
            SessionStreamRecord::Stored(event)
                if event.event_kind() == Some(DurableEventType::LogTailRecovered)
        )
    }));
    Ok(())
}

#[test]
fn session_writer_reconciliation_reports_prewrite_failure_as_confirmed_absent() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let record = linked_audit_record(
        "record-prewrite-reconcile",
        "event-prewrite-reconcile",
        "event-prewrite-reconcile",
        None,
    )?;
    let expectation = record.reconciliation_expectation(store_session_id(&store))?;
    store.inject_writer_fault(SessionWriterFault::BeforeWrite)?;
    assert!(matches!(
        store.append_and_sync(record),
        Err(DurableAuditError::AppendFailed(_))
    ));

    assert!(matches!(
        store.reconcile_durable_event(&expectation),
        DurableEventReconciliation::ConfirmedAbsent
    ));
    Ok(())
}

#[test]
fn session_writer_reconciliation_reports_unreadable_stream_as_indeterminate() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    let record = linked_audit_record(
        "record-indeterminate",
        "event-indeterminate",
        "event-indeterminate",
        None,
    )?;
    let expectation = record.reconciliation_expectation(store_session_id(&store))?;
    store.append_and_sync(record)?;
    fs::write(&path, b"not a durable event\n")?;

    assert!(matches!(
        store.reconcile_durable_event(&expectation),
        DurableEventReconciliation::Indeterminate { .. }
    ));
    Ok(())
}

#[test]
fn session_writer_rejects_invalid_causal_identity_requests() -> Result<()> {
    let no_correlation = DurableAuditRecord::new(
        DurableEventType::MutationPrepared,
        serde_json::to_value(test_mutation_prepared("record-no-correlation", None))?,
        "record-no-correlation",
        None,
    )?
    .with_causation_id("event-cause");
    assert!(matches!(
        no_correlation,
        Err(DurableAuditError::InvalidRequest { .. })
    ));

    let self_caused = linked_audit_record(
        "record-self-caused",
        "event-self-caused",
        "event-self-caused",
        Some("event-self-caused"),
    );
    assert!(self_caused.is_err());

    let duplicate_event_ids = DurableAuditBatch::new(
        "batch-duplicate-events",
        vec![
            linked_audit_record(
                "record-duplicate-1",
                "event-duplicate",
                "event-duplicate",
                None,
            )?,
            linked_audit_record(
                "record-duplicate-2",
                "event-duplicate",
                "event-duplicate",
                None,
            )?,
        ],
    );
    assert!(matches!(
        duplicate_event_ids,
        Err(DurableAuditError::InvalidRequest { .. })
    ));
    Ok(())
}

#[test]
fn session_writer_rejects_missing_forward_and_cross_chain_causes() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    store.append_and_sync(linked_audit_record(
        "record-cause-root",
        "event-cause-root",
        "event-cause-root",
        None,
    )?)?;

    assert!(matches!(
        store.append_and_sync(linked_audit_record(
            "record-missing-cause",
            "event-missing-cause",
            "event-missing-root",
            Some("event-does-not-exist"),
        )?),
        Err(DurableAuditError::AppendFailed(_))
    ));
    assert!(matches!(
        store.append_and_sync(linked_audit_record(
            "record-cross-chain",
            "event-cross-chain",
            "event-other-root",
            Some("event-cause-root"),
        )?),
        Err(DurableAuditError::AppendFailed(_))
    ));

    let forward_reference = DurableAuditBatch::new(
        "batch-forward-cause",
        vec![
            linked_audit_record(
                "record-forward-child",
                "event-forward-child",
                "event-forward-root",
                Some("event-forward-root"),
            )?,
            linked_audit_record(
                "record-forward-root",
                "event-forward-root",
                "event-forward-root",
                None,
            )?,
        ],
    )?;
    assert!(matches!(
        store.append_batch_and_sync(forward_reference),
        Err(DurableAuditError::AppendFailed(_))
    ));

    let records = JsonlSessionStore::read_event_records(store.path())?;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].event_id(), "event-cause-root");
    Ok(())
}

#[test]
fn session_writer_receipt_rejects_identity_mismatch_and_writer_reopen() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let (receipt, session_id) = {
        let store = JsonlSessionStore::new(&path)?;
        let receipt = store.append_and_sync(audit_record(
            DurableEventType::RunStatusChanged,
            "record-stale",
            "corr-stale",
        )?)?;
        let session_id = receipt.session_id().to_owned();
        (receipt, session_id)
    };

    let reopened = JsonlSessionStore::new(&path)?;
    let expectation = DurableAppendExpectation::new(
        session_id,
        "record-stale",
        vec![record_expectation(
            DurableEventType::RunStatusChanged,
            "record-stale",
            "corr-stale",
        )?],
    )?;
    assert!(matches!(
        reopened.validate_and_consume(receipt, expectation),
        Err(DurableAuditError::ReceiptMismatch)
    ));

    let receipt = reopened.append_and_sync(audit_record(
        DurableEventType::RunFinalized,
        "record-mismatch",
        "corr-current",
    )?)?;
    let wrong = DurableAppendExpectation::new(
        receipt.session_id(),
        "record-mismatch",
        vec![record_expectation(
            DurableEventType::RunFinalized,
            "record-mismatch",
            "corr-wrong",
        )?],
    )?;
    assert!(matches!(
        reopened.validate_and_consume(receipt, wrong),
        Err(DurableAuditError::ReceiptMismatch)
    ));
    Ok(())
}

#[test]
fn session_writer_reloads_valid_external_extension_before_next_append() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    let first = store.append_event(
        DurableEventType::RunStatusChanged,
        EventClass::Critical,
        serde_json::json!({ "source": "owner" }),
    )?;
    let external = StoredEvent::new(
        DurableEventType::RunStatusChanged,
        EventClass::Critical,
        uuid::Uuid::new_v4().to_string(),
        first.session_id.clone(),
        2,
        serde_json::json!({ "source": "external" }),
    )?;
    let mut file = OpenOptions::new().append(true).open(&path)?;
    file.write_all(external.to_json_line()?.as_bytes())?;
    file.sync_all()?;

    let appended = store.append_event(
        DurableEventType::RunFinalized,
        EventClass::Critical,
        serde_json::json!({ "source": "owner-reloaded" }),
    )?;

    assert_eq!(appended.stream_sequence, 3);
    assert_eq!(store.writer_full_scan_count()?, 2);
    Ok(())
}

#[test]
fn session_writer_rejects_external_prefix_rewrite_even_when_tail_record_is_unchanged() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    store.append_event(
        DurableEventType::RunStatusChanged,
        EventClass::Critical,
        serde_json::json!({ "value": "original" }),
    )?;
    store.append_event(
        DurableEventType::RunFinalized,
        EventClass::Critical,
        serde_json::json!({ "value": "unchanged-tail" }),
    )?;
    let records = JsonlSessionStore::read_event_records(&path)?;
    let original_first = records[0].stored_event();
    let original_second = records[1].stored_event();
    let rewritten_first = StoredEvent::new(
        DurableEventType::RunStatusChanged,
        EventClass::Critical,
        original_first.event_id.clone(),
        original_first.session_id.clone(),
        original_first.stream_sequence,
        serde_json::json!({ "value": "rewritten" }),
    )?;
    fs::write(
        &path,
        format!(
            "{}{}",
            rewritten_first.to_json_line()?,
            original_second.to_json_line()?
        ),
    )?;

    let error = store
        .append_event(
            DurableEventType::RunFinalized,
            EventClass::Critical,
            serde_json::json!({ "value": "must-not-append" }),
        )
        .expect_err("rewritten durable prefix must fail closed");
    assert!(error.to_string().contains("prefix changed"));
    assert_eq!(JsonlSessionStore::read_event_records(&path)?.len(), 2);
    Ok(())
}

#[cfg(unix)]
#[test]
fn session_writer_canonical_aliases_share_the_same_owner() -> Result<()> {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    fs::write(&path, "")?;
    let alias = temp.path().join("session-alias.jsonl");
    symlink(&path, &alias)?;
    let direct = JsonlSessionStore::new(&path)?;
    let aliased = JsonlSessionStore::new(&alias)?;

    direct.append_event(
        DurableEventType::RunStatusChanged,
        EventClass::Critical,
        serde_json::json!({ "owner": "direct" }),
    )?;
    aliased.append_event(
        DurableEventType::RunFinalized,
        EventClass::Critical,
        serde_json::json!({ "owner": "alias" }),
    )?;

    assert_eq!(direct.path(), aliased.path());
    assert_eq!(direct.writer_full_scan_count()?, 1);
    assert_eq!(aliased.writer_full_scan_count()?, 1);
    Ok(())
}

#[test]
fn session_writer_rejects_legacy_session_log_entries_without_mutating_the_stream() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let legacy = SessionLogEntry::User(ModelMessage::user("legacy"));
    let content = format!("\n{}\n{{unterminated-tail", serde_json::to_string(&legacy)?);
    fs::write(&path, &content)?;

    let read_error = JsonlSessionStore::read_event_records(&path)
        .expect_err("legacy SessionLogEntry lines must be rejected");
    let compatibility = read_error
        .downcast_ref::<SessionStreamCompatibilityError>()
        .expect("reader must return a structured compatibility error");
    assert_eq!(compatibility.path, path);
    assert_eq!(compatibility.physical_line, 2);

    let store = JsonlSessionStore::new(&path)?;
    let append_error = store
        .append_session_entry_event(&SessionLogEntry::Assistant(ModelMessage::assistant(
            Some("v2".to_owned()),
            Vec::new(),
        )))
        .expect_err("legacy stream must not accept v2 appends");
    assert!(
        append_error
            .downcast_ref::<SessionStreamCompatibilityError>()
            .is_some()
    );
    assert_eq!(fs::read_to_string(&path)?, content);
    assert!(!super::tail_recovery_intent_path(&path).exists());
    assert!(!temp.path().join(".sigil-recovery").exists());
    Ok(())
}

#[test]
fn session_writer_rejects_a_raw_legacy_compaction_tail_without_truncating_it() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let legacy = serde_json::json!({
        "control": {
            "compaction_applied": {
                "summary": "old compact summary",
                "compacted_message_count": 2,
            }
        }
    });
    let content = format!("{legacy}\n{{unterminated-tail");
    fs::write(&path, &content)?;

    let read_error = JsonlSessionStore::read_event_records(&path)
        .expect_err("raw legacy compaction payload must fail closed");
    let compatibility = read_error
        .downcast_ref::<SessionStreamCompatibilityError>()
        .expect("raw legacy payload must return a structured compatibility error");
    assert_eq!(compatibility.path, path);
    assert_eq!(compatibility.physical_line, 1);
    assert_eq!(compatibility.format_name, "legacy CompactionRecord payload");

    let store = JsonlSessionStore::new(&path)?;
    let append_error = store
        .append_session_entry_event(&SessionLogEntry::Assistant(ModelMessage::assistant(
            Some("v2".to_owned()),
            Vec::new(),
        )))
        .expect_err("legacy stream must not enter tail recovery before append");
    assert!(
        append_error
            .downcast_ref::<SessionStreamCompatibilityError>()
            .is_some()
    );
    assert_eq!(fs::read_to_string(&path)?, content);
    assert!(!super::tail_recovery_intent_path(&path).exists());
    assert!(!temp.path().join(".sigil-recovery").exists());
    Ok(())
}

#[test]
fn session_writer_partial_batch_failure_poison_reloads_before_retry() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    store.inject_writer_fault(SessionWriterFault::PartialFirstRecord)?;

    let failed = store.append_batch_and_sync(DurableAuditBatch::new(
        "batch-partial",
        vec![
            audit_record(
                DurableEventType::RunStatusChanged,
                "record-partial-1",
                "corr-partial-1",
            )?,
            audit_record(
                DurableEventType::RunFinalized,
                "record-partial-2",
                "corr-partial-2",
            )?,
        ],
    )?);
    assert!(matches!(failed, Err(DurableAuditError::AppendFailed(_))));

    let receipt = store.append_and_sync(audit_record(
        DurableEventType::RunStatusChanged,
        "record-after-reload",
        "corr-after-reload",
    )?)?;
    let records = JsonlSessionStore::read_event_records(store.path())?;
    assert!(records.iter().any(|record| {
        matches!(
            record,
            SessionStreamRecord::Stored(event)
                if event.event_type == DurableEventType::LogTailRecovered.as_str()
        )
    }));
    assert_eq!(receipt.records()[0].stream_sequence(), 2);
    assert_eq!(store.writer_full_scan_count()?, 2);
    Ok(())
}

#[test]
fn session_writer_pre_sync_failure_returns_no_receipt_and_reloads_sequence() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    store.inject_writer_fault(SessionWriterFault::BeforeSync)?;
    let failed = store.append_and_sync(audit_record(
        DurableEventType::RunStatusChanged,
        "record-unsynced",
        "corr-unsynced",
    )?);
    assert!(matches!(failed, Err(DurableAuditError::AppendFailed(_))));

    let receipt = store.append_and_sync(audit_record(
        DurableEventType::RunFinalized,
        "record-after-sync-failure",
        "corr-after-sync-failure",
    )?)?;
    assert_eq!(receipt.records()[0].stream_sequence(), 2);
    assert_eq!(store.writer_full_scan_count()?, 2);
    Ok(())
}

#[test]
fn session_writer_parent_directory_sync_failure_retries_before_receipt() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    store.inject_writer_fault(SessionWriterFault::ParentDirectorySync)?;

    let failed = store.append_and_sync(audit_record(
        DurableEventType::MutationPrepared,
        "record-parent-sync-failed",
        "corr-parent-sync-failed",
    )?);
    assert!(matches!(failed, Err(DurableAuditError::AppendFailed(_))));
    assert_eq!(store.writer_parent_sync_count()?, 0);

    let receipt = store.append_and_sync(audit_record(
        DurableEventType::MutationPrepared,
        "record-parent-sync-retry",
        "corr-parent-sync-retry",
    )?)?;
    assert_eq!(receipt.records()[0].stream_sequence(), 1);
    assert_eq!(store.writer_parent_sync_count()?, 1);
    Ok(())
}

#[test]
fn session_writer_parent_directory_sync_contract_is_cross_platform() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let session_path = temp.path().join("session.jsonl");
    let file = std::fs::File::create(&session_path)?;
    file.sync_all()?;

    super::sync_parent_dir(&session_path)?;

    assert!(session_path.is_file());
    Ok(())
}

#[test]
fn session_writer_receipt_offset_and_event_identity_tampering_fails_closed() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut offset_receipt = store.append_and_sync(audit_record(
        DurableEventType::MutationPrepared,
        "record-offset",
        "corr-offset",
    )?)?;
    let offset_expectation = DurableAppendExpectation::new(
        offset_receipt.session_id(),
        "record-offset",
        vec![record_expectation(
            DurableEventType::MutationPrepared,
            "record-offset",
            "corr-offset",
        )?],
    )?;
    offset_receipt.records[0].start_offset += 1;
    assert!(matches!(
        store.validate_and_consume(offset_receipt, offset_expectation),
        Err(DurableAuditError::ReceiptMismatch)
    ));

    let mut event_receipt = store.append_and_sync(audit_record(
        DurableEventType::MutationPrepared,
        "record-event",
        "corr-event",
    )?)?;
    let event_expectation = DurableAppendExpectation::new(
        event_receipt.session_id(),
        "record-event",
        vec![record_expectation(
            DurableEventType::MutationPrepared,
            "record-event",
            "corr-event",
        )?],
    )?;
    event_receipt.records[0].event_id = "tampered-event-id".to_owned();
    assert!(matches!(
        store.validate_and_consume(event_receipt, event_expectation),
        Err(DurableAuditError::ReceiptMismatch)
    ));
    Ok(())
}

#[test]
fn session_writer_append_failure_returns_no_durable_receipt() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    fs::write(&path, "")?;
    let store = JsonlSessionStore::new(&path)?;
    let lock_file = OpenOptions::new().read(true).write(true).open(&path)?;
    lock_file.lock_exclusive()?;

    let result = store.append_and_sync(audit_record(
        DurableEventType::RunStatusChanged,
        "record-locked",
        "corr-locked",
    )?);
    FileExt::unlock(&lock_file)?;

    assert!(matches!(result, Err(DurableAuditError::AppendFailed(_))));
    assert_eq!(fs::metadata(&path)?.len(), 0);
    Ok(())
}

#[test]
fn session_writer_rejects_normal_events_and_in_memory_receipts() -> Result<()> {
    assert!(matches!(
        DurableAuditRecord::new(
            DurableEventType::UserMessageRecorded,
            serde_json::json!({}),
            "record-normal",
            Some("corr-normal".to_owned())
        ),
        Err(DurableAuditError::InvalidRequest { .. })
    ));
    let session = Session::new("deepseek", "deepseek-v4");
    assert!(matches!(
        session.durable_audit_writer(),
        Err(DurableAuditError::MissingDurableStore)
    ));
    assert!(matches!(
        DurableAuditRecord::new(
            DurableEventType::RunStatusChanged,
            serde_json::json!({ "other": "identity-not-present" }),
            "record-missing",
            None,
        ),
        Err(DurableAuditError::InvalidRequest { .. })
    ));
    assert!(matches!(
        DurableAuditRecord::new(
            DurableEventType::ApprovalResolved,
            serde_json::json!({ "record_id": "record-wrong-schema" }),
            "record-wrong-schema",
            Some("corr-wrong-schema".to_owned()),
        ),
        Err(DurableAuditError::InvalidRequest { .. })
    ));
    let missing_authorization = audit_record(
        DurableEventType::MutationPrepared,
        "record-auth-missing",
        "corr-auth-missing",
    )?
    .with_authorization_id("auth-missing");
    assert!(matches!(
        missing_authorization,
        Err(DurableAuditError::InvalidRequest { .. })
    ));
    Ok(())
}

#[test]
fn session_writer_cross_process_child() -> Result<()> {
    let Ok(path) = std::env::var("SIGIL_SESSION_WRITER_CHILD_PATH") else {
        return Ok(());
    };
    let store = JsonlSessionStore::new(path)?;
    let result = store.append_and_sync(audit_record(
        DurableEventType::RunFinalized,
        "record-child",
        "corr-child",
    )?);
    assert!(matches!(result, Err(DurableAuditError::AppendFailed(_))));
    Ok(())
}

#[test]
fn session_writer_sidecar_lease_rejects_second_process_owner() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    store.append_and_sync(audit_record(
        DurableEventType::RunStatusChanged,
        "record-parent",
        "corr-parent",
    )?)?;

    let output = Command::new(std::env::current_exe()?)
        .arg("--exact")
        .arg("session::writer::tests::session_writer_cross_process_child")
        .arg("--nocapture")
        .env("SIGIL_SESSION_WRITER_CHILD_PATH", &path)
        .output()?;

    assert!(
        output.status.success(),
        "child writer fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(JsonlSessionStore::read_event_records(&path)?.len(), 1);
    Ok(())
}
