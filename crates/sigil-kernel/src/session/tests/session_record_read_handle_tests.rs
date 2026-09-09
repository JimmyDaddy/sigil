use super::*;
use std::{
    sync::{TryLockError, mpsc},
    time::{Duration, Instant},
};

#[test]
fn session_record_read_handle_serializes_writer_and_releases_returned_snapshot() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    crate::session::append_current_test_session_identity(&store)?;
    let read_handle = store.read_handle();
    let snapshot = thread::scope(|threads| -> Result<Vec<SessionStreamRecord>> {
        // Pause the actual strict reader after it takes its coordinator, before it can read
        // bytes. A competing real append must wait on that coordinator, not exhaust its OS
        // lock budget. Always release this barrier before joining either thread, even on error.
        let barrier = SESSION_LOG_IO_LOCK
            .lock()
            .map_err(|_| anyhow::anyhow!("session log I/O lock poisoned"))?;
        let (read_tx, read_rx) = mpsc::channel();
        let reader = read_handle.clone();
        let reader_thread = threads.spawn(move || {
            read_tx
                .send(reader.read_event_records())
                .expect("read snapshot receiver");
        });
        let waiting_reader = (|| -> Result<()> {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                match store.writer.try_lock() {
                    Err(TryLockError::WouldBlock) => return Ok(()),
                    Err(TryLockError::Poisoned(_)) => anyhow::bail!("writer lock poisoned"),
                    Ok(guard) => drop(guard),
                }
                anyhow::ensure!(
                    Instant::now() < deadline,
                    "reader did not acquire coordinator"
                );
                thread::yield_now();
            }
        })();
        if let Err(error) = waiting_reader {
            drop(barrier);
            reader_thread.join().expect("reader thread");
            return Err(error);
        }

        let writer_attempts = store.active_projection_metrics().writer_lock_attempt_total;
        let (write_tx, write_rx) = mpsc::channel();
        let writer = store.clone();
        let writer_thread = threads.spawn(move || {
            write_tx
                .send(
                    writer.append(&SessionLogEntry::User(crate::ModelMessage::user(
                        "written after the read snapshot",
                    ))),
                )
                .expect("append result receiver");
        });
        let waiting_writer = (|| -> Result<()> {
            let deadline = Instant::now() + Duration::from_secs(5);
            while store.active_projection_metrics().writer_lock_attempt_total <= writer_attempts {
                anyhow::ensure!(
                    Instant::now() < deadline,
                    "writer did not attempt coordinator"
                );
                thread::yield_now();
            }
            anyhow::ensure!(
                matches!(write_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
                "writer completed while the read held its coordinator"
            );
            Ok(())
        })();
        drop(barrier);
        waiting_writer?;

        let snapshot = read_rx.recv_timeout(Duration::from_secs(5))??;
        write_rx.recv_timeout(Duration::from_secs(5))??;
        reader_thread.join().expect("reader thread");
        writer_thread.join().expect("writer thread");
        Ok(snapshot)
    })?;
    assert_eq!(snapshot.len(), 1);
    assert_eq!(snapshot[0].stream_sequence(), 1);
    assert_eq!(read_handle.read_event_records()?.len(), 2);

    // Neither the read capability nor a returned snapshot carries a shared file lock.
    store.append(&SessionLogEntry::User(crate::ModelMessage::user(
        "written while prior snapshots remain alive",
    )))?;
    assert_eq!(snapshot.len(), 1);
    assert_eq!(read_handle.read_event_records()?.len(), 3);
    Ok(())
}

#[test]
fn session_record_read_handle_derivation_does_not_create_or_recover_a_stream() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    let before = fs::read_dir(temp.path())?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<std::io::Result<Vec<_>>>()?;
    let handle = store.read_handle();
    let clone = handle.clone();
    drop(store);
    assert!(handle.read_event_records()?.is_empty());
    assert!(clone.read_event_records()?.is_empty());
    assert!(!path.exists());
    let after = fs::read_dir(temp.path())?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<std::io::Result<Vec<_>>>()?;
    assert_eq!(after, before);
    Ok(())
}

#[test]
fn session_record_read_handle_reports_offsets_and_reads_only_the_appended_tail() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    crate::session::append_current_test_session_identity(&store)?;
    store.append(&SessionLogEntry::User(crate::ModelMessage::user("first")))?;
    let reader = store.read_handle();
    let initial = reader.read_event_records_with_offsets()?;
    assert_eq!(initial.records().len(), 2);
    assert_eq!(initial.record_offsets().len(), initial.records().len());
    assert_eq!(initial.start_offset(), 0);
    assert_eq!(initial.record_offsets()[0], 0);
    assert_eq!(initial.end_offset(), std::fs::metadata(store.path())?.len());
    assert!(!initial.has_more());

    store.append(&SessionLogEntry::User(crate::ModelMessage::user("second")))?;
    let tail = reader.read_event_record_range(
        initial.end_offset(),
        initial
            .records()
            .last()
            .expect("initial stream has a tail")
            .stream_sequence(),
        Some(initial.records()[0].session_id()),
        4,
        64 * 1024,
    )?;
    assert_eq!(tail.records().len(), 1);
    assert_eq!(tail.record_offsets(), &[initial.end_offset()]);
    assert_eq!(tail.records()[0].stream_sequence(), 3);
    assert_eq!(tail.end_offset(), std::fs::metadata(store.path())?.len());
    assert!(!tail.has_more());
    assert!(
        reader
            .read_event_record_range(initial.end_offset() + 1, 2, None, 4, 64 * 1024)
            .is_err()
    );
    Ok(())
}

#[test]
fn session_record_read_handle_caps_raw_lines_and_batch_before_json_decode() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(fixture.path().join("oversized.jsonl"))?;
    let bytes = vec![b'x'; MAX_SESSION_RAW_RECORD_BYTES + 128 * 1024];
    fs::write(store.path(), &bytes)?;
    let reader = store.read_handle();
    let error = reader
        .read_event_record_range(0, 0, None, 1, 4 * 1024 * 1024)
        .expect_err("unterminated oversized record");
    assert!(
        error.to_string().contains("raw record exceeds"),
        "{error:#}"
    );
    let error = reader
        .read_event_record_range(0, 0, None, 1, 16 * 1024)
        .expect_err("small byte budget");
    assert!(
        error.to_string().contains("range exceeds its byte bound"),
        "{error:#}"
    );
    assert_eq!(fs::read(store.path())?, bytes);
    Ok(())
}

#[test]
fn session_record_read_handle_accepts_large_writer_record_and_exact_offsets() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(fixture.path().join("large.jsonl"))?;
    crate::session::append_current_test_session_identity(&store)?;
    let initial = store.read_handle().read_event_records_with_offsets()?;
    store.append(&SessionLogEntry::User(crate::ModelMessage::user(
        "中".repeat(170_000),
    )))?;
    let reader = store.read_handle();
    let range = reader.read_event_record_range(
        initial.end_offset(),
        1,
        Some(initial.records()[0].session_id()),
        1,
        MAX_SESSION_RAW_RECORD_BYTES,
    )?;
    assert_eq!(range.records().len(), 1);
    assert_eq!(range.record_end_offsets(), &[range.end_offset()]);
    let exact = reader.read_event_record_range(
        range.record_offsets()[0],
        1,
        Some(initial.records()[0].session_id()),
        1,
        (range.end_offset() - range.start_offset()) as usize,
    )?;
    assert_eq!(exact.end_offset(), range.end_offset());
    Ok(())
}

#[test]
fn session_record_read_handle_cancel_and_deadline_stop_waiting_for_coordinator() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(fixture.path().join("blocked.jsonl"))?;
    crate::session::append_current_test_session_identity(&store)?;
    let held = store
        .writer
        .lock()
        .map_err(|_| anyhow::anyhow!("writer lock poisoned"))?;
    for deadline in [false, true] {
        let reader = store.read_handle();
        let budget =
            SessionReadBudget::new(deadline.then(|| Instant::now() + Duration::from_millis(30)));
        let cancel = budget.clone();
        let (tx, rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            tx.send(reader.read_event_record_range_with_budget(0, 0, None, 1, 8192, &budget))
                .expect("test receiver");
        });
        if !deadline {
            cancel.cancel();
        }
        assert!(rx.recv_timeout(Duration::from_secs(1))?.is_err());
        worker
            .join()
            .expect("cancelled reader stopped while coordinator remains held");
    }
    drop(held);
    assert_eq!(store.read_handle().read_event_records()?.len(), 1);
    Ok(())
}

#[test]
fn session_record_read_handle_cold_observer_never_creates_writer_state_or_repairs() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let source = JsonlSessionStore::new(fixture.path().join("source/source.jsonl"))?;
    crate::session::append_current_test_session_identity(&source)?;
    let cold = fixture.path().join("cold");
    fs::create_dir(&cold)?;
    let path = cold.join("history.jsonl");
    fs::copy(source.path(), &path)?;
    let original = fs::read(&path)?;
    let reader = SessionRecordReadHandle::open_existing_observer(&path)?;
    let budget = SessionReadBudget::default();
    let source = reader.source_snapshot(&budget)?;
    let range =
        reader.read_event_record_range_with_budget(0, 0, None, 256, 4 * 1024 * 1024, &budget)?;
    assert_eq!(range.records().len(), 1);
    assert!(range.source_snapshot().same_source(&source));
    assert_eq!(
        fs::read_dir(&cold)?.count(),
        1,
        "cold query must not create reserve or writer sidecars"
    );
    assert_eq!(fs::read(&path)?, original);
    let mut corrupt = original;
    corrupt.extend_from_slice(b"{\"partial\":");
    fs::write(&path, &corrupt)?;
    assert!(
        reader
            .read_event_record_range(0, 0, None, 256, 4 * 1024 * 1024)
            .is_err()
    );
    assert_eq!(fs::read(&path)?, corrupt);
    assert_eq!(fs::read_dir(&cold)?.count(), 1);
    let missing = fixture.path().join("absent/history.jsonl");
    assert!(SessionRecordReadHandle::open_existing_observer(&missing).is_err());
    assert!(!missing.parent().expect("parent").exists());
    #[cfg(unix)]
    {
        let link = cold.join("alias.jsonl");
        std::os::unix::fs::symlink(&path, &link)?;
        assert!(SessionRecordReadHandle::open_existing_observer(link).is_err());
    }
    Ok(())
}
