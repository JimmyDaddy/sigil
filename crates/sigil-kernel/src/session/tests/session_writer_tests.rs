use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::Write,
    process::Command,
    sync::{Arc, Barrier},
    thread,
};

use anyhow::Result;
use fs2::FileExt;

use super::*;
use crate::{
    MutationEventRecorder, MutationPrepared, MutationSubject, MutationSyncClass, SnapshotCoverage,
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
            SessionStreamRecord::Legacy { .. } => None,
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
    let SessionStreamRecord::Stored(original_first) = &records[0] else {
        panic!("owner stream should contain v2 events");
    };
    let SessionStreamRecord::Stored(original_second) = &records[1] else {
        panic!("owner stream should contain v2 events");
    };
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
fn session_writer_preserves_legacy_prefix_when_appending_v2() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let legacy = SessionLogEntry::User(ModelMessage::user("legacy"));
    fs::write(&path, format!("{}\n", serde_json::to_string(&legacy)?))?;
    let before = JsonlSessionStore::read_event_records(&path)?;
    let legacy_event_id = before[0].event_id().to_owned();
    let legacy_session_id = before[0].session_id().to_owned();
    let store = JsonlSessionStore::new(&path)?;

    let appended = store.append_session_entry_event(&SessionLogEntry::Assistant(
        ModelMessage::assistant(Some("v2".to_owned()), Vec::new()),
    ))?;
    let records = JsonlSessionStore::read_event_records(&path)?;

    assert_eq!(appended.stream_sequence, 2);
    assert!(matches!(records[0], SessionStreamRecord::Legacy { .. }));
    assert!(matches!(records[1], SessionStreamRecord::Stored(_)));
    assert_eq!(records[0].event_id(), legacy_event_id);
    assert_eq!(records[0].session_id(), legacy_session_id);
    let legacy_domain = records[0]
        .domain_event_record()?
        .expect("legacy record should upcast to a domain event");
    assert!(matches!(legacy_domain.event, DomainEvent::Legacy(_)));
    assert!(records[0].typed_domain_event_record()?.is_none());
    let entries = JsonlSessionStore::read_entries(&path)?;
    assert_eq!(entries.len(), 2);
    assert!(matches!(entries[0], SessionLogEntry::User(_)));
    assert!(matches!(entries[1], SessionLogEntry::Assistant(_)));
    Ok(())
}

#[test]
fn session_writer_legacy_blank_lines_keep_effective_ordinals_stable() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let first = SessionLogEntry::User(ModelMessage::user("first"));
    let second = SessionLogEntry::Assistant(ModelMessage::assistant(
        Some("second".to_owned()),
        Vec::new(),
    ));
    fs::write(
        &path,
        format!(
            "\n{}\n\n{}\n\n",
            serde_json::to_string(&first)?,
            serde_json::to_string(&second)?
        ),
    )?;

    let before = JsonlSessionStore::read_event_records(&path)?;
    assert_eq!(before[0].stream_sequence(), 1);
    assert_eq!(before[1].stream_sequence(), 2);
    let ids = before
        .iter()
        .map(|record| record.event_id().to_owned())
        .collect::<Vec<_>>();
    let store = JsonlSessionStore::new(&path)?;
    let appended =
        store.append_session_entry_event(&SessionLogEntry::User(ModelMessage::user("third")))?;
    let after = JsonlSessionStore::read_event_records(&path)?;

    assert_eq!(appended.stream_sequence, 3);
    assert_eq!(after[0].event_id(), ids[0]);
    assert_eq!(after[1].event_id(), ids[1]);
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
