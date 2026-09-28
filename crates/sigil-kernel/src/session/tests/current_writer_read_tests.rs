use super::*;

#[cfg(unix)]
#[test]
fn current_writer_reads_and_linked_appends_reuse_one_validated_stream() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("current.jsonl"))?;
    store.append_and_sync(linked_audit_record("root", "root", "root", None)?)?;
    let scans = store.writer_full_scan_count()?;
    let mut predecessor = "root".to_owned();
    for index in 0..34 {
        let records = store.read_current_event_records_writer()?;
        assert_eq!(records.len(), index + 1);
        let id = format!("current-{index}");
        store.append_and_sync(linked_audit_record(&id, &id, "root", Some(&predecessor))?)?;
        predecessor = id;
    }
    assert_eq!(store.writer_full_scan_count()?, scans);
    assert_eq!(store.read_event_records_writer()?.len(), 35);
    assert_eq!(store.writer_full_scan_count()?, scans + 1);
    store.append_and_sync(linked_audit_record(
        "after-reconciliation",
        "after-reconciliation",
        "root",
        Some(&predecessor),
    )?)?;
    assert_eq!(
        store.writer_full_scan_count()?,
        scans + 1,
        "the replay also rebuilds event links"
    );
    Ok(())
}

#[test]
fn current_writer_read_observes_valid_external_extension() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("extended.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    let first = store.append_event(
        DurableEventType::RunStatusChanged,
        EventClass::Critical,
        serde_json::json!({"run_status":"running","terminal_reason":"in_progress","tool_calls":0}),
    )?;
    store.read_current_event_records_writer()?;
    let external = StoredEvent::new(
        DurableEventType::RunStatusChanged,
        EventClass::Critical,
        uuid::Uuid::new_v4().to_string(),
        first.session_id.clone(),
        2,
        serde_json::json!({"run_status":"running","terminal_reason":"in_progress","tool_calls":1}),
    )?;
    let mut file = OpenOptions::new().append(true).open(&path)?;
    file.write_all(external.to_json_line()?.as_bytes())?;
    file.sync_all()?;
    let records = store.read_current_event_records_writer()?;
    assert_eq!(records.len(), 2);
    assert_eq!(records[1].event_id(), external.event_id);
    assert_eq!(
        store
            .active_projection_snapshot()?
            .frontier()
            .cursor()
            .expect("current cursor")
            .last_applied_stream_sequence,
        2
    );
    Ok(())
}

#[test]
fn current_writer_read_rejects_same_length_prefix_rewrite_with_restored_mtime() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("rewritten.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    store.append_event(
        DurableEventType::RunStatusChanged,
        EventClass::Critical,
        serde_json::json!({"run_status":"running","terminal_reason":"in_progress","tool_calls":0,"value":"original"}),
    )?;
    store.append_event(
        DurableEventType::RunStatusChanged,
        EventClass::Critical,
        serde_json::json!({"run_status":"running","terminal_reason":"in_progress","tool_calls":0,"value":"unchanged-tail"}),
    )?;
    let records = store.read_current_event_records_writer()?;
    let before = fs::read(&path)?;
    let modified = fs::metadata(&path)?.modified()?;
    let mut first = records[0].stored_event().clone();
    first.payload["value"] = "rewrites".into();
    first.record_checksum = first.compute_record_checksum()?;
    let after = format!(
        "{}{}",
        first.to_json_line()?,
        records[1].stored_event().to_json_line()?
    );
    assert_eq!(before.len(), after.len());
    fs::write(&path, &after)?;
    OpenOptions::new()
        .write(true)
        .open(&path)?
        .set_times(std::fs::FileTimes::new().set_modified(modified))?;
    assert_eq!(fs::metadata(&path)?.modified()?, modified);
    let error = store
        .read_current_event_records_writer()
        .expect_err("changed validated prefix must fail");
    assert!(error.to_string().contains("prefix changed"), "{error:#}");
    assert_eq!(fs::read(&path)?, after.as_bytes());
    Ok(())
}

#[test]
fn current_writer_read_rejects_truncation_and_invalid_checksum() -> Result<()> {
    for checksum in [false, true] {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("invalid.jsonl");
        let store = JsonlSessionStore::new(&path)?;
        store.append_event(
            DurableEventType::RunStatusChanged,
            EventClass::Critical,
            serde_json::json!({"run_status":"running","terminal_reason":"in_progress","tool_calls":0,"value":"original"}),
        )?;
        store.append_event(
            DurableEventType::RunStatusChanged,
            EventClass::Critical,
            serde_json::json!({"run_status":"running","terminal_reason":"in_progress","tool_calls":0,"value":"unchanged-tail"}),
        )?;
        let records = store.read_current_event_records_writer()?;
        if checksum {
            let text = fs::read_to_string(&path)?;
            assert!(text.contains("original"));
            fs::write(&path, text.replacen("original", "rewrites", 1))?;
        } else {
            fs::write(&path, records[0].stored_event().to_json_line()?)?;
        }
        assert!(store.read_current_event_records_writer().is_err());
    }
    Ok(())
}

#[test]
fn current_writer_read_recovers_uncertain_bundle_and_reopen_still_replays() -> Result<()> {
    for fault in [
        SessionWriterFault::BeforeWrite,
        SessionWriterFault::PartialSecondRecord,
        SessionWriterFault::BeforeSync,
    ] {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("recover.jsonl");
        let store = JsonlSessionStore::new(&path)?;
        append_current_test_session_identity(&store)?;
        let before = fs::read(&path)?;
        let entries = (0..2)
            .map(|index| {
                SessionLogEntry::Control(ControlEntry::Note {
                    kind: "current_read_recovery".to_owned(),
                    data: serde_json::json!({"index":index}),
                })
            })
            .collect::<Vec<_>>();
        store.inject_writer_fault(fault)?;
        assert!(store.append_session_entry_events(&entries).is_err());
        let records = store.read_current_event_records_writer()?;
        let observed = session_entries_from_records(&records)?;
        assert_eq!(
            serde_json::to_value(&observed[1..])?,
            serde_json::to_value(&entries)?
        );
        assert!(fs::read(&path)?.starts_with(&before));
        let scans = store.writer_full_scan_count()?;
        let reopened = JsonlSessionStore::open_existing(&path)?;
        assert_eq!(
            store.writer_full_scan_count()?,
            scans + 1,
            "explicit reopen must replay disk"
        );
        assert_eq!(
            reopened
                .active_projection_snapshot()?
                .durable_session_entry_count(),
            3
        );
    }
    Ok(())
}
