use super::*;
use std::{sync::mpsc, time::Duration};

fn populated_store(path: PathBuf) -> Result<JsonlSessionStore> {
    let store = JsonlSessionStore::new(path)?;
    store.append_event(
        DurableEventType::MutationPrepared,
        EventClass::Critical,
        serde_json::to_value(test_mutation_prepared("coordinated-read-baseline", None))?,
    )?;
    Ok(store)
}

#[test]
fn coordinated_session_reader_waits_for_owned_writer_without_changing_bytes() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = populated_store(temp.path().join("session.jsonl"))?;
    let before = fs::read(store.path())?;
    let expected = JsonlSessionStore::read_event_records(store.path())?;
    let records = thread::scope(|threads| -> Result<Vec<SessionStreamRecord>> {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let writer_store = store.clone();
        let writer = threads.spawn(move || {
            writer_store.append_event_if(
                DurableEventType::MutationPrepared,
                EventClass::Critical,
                serde_json::to_value(test_mutation_prepared("unused-conditional-record", None))?,
                |_| {
                    // The real conditional writer owns the coordinator. Extend its file-lock
                    // interval until the owned reader has attempted that same coordinator.
                    let owner = OpenOptions::new()
                        .read(true)
                        .write(true)
                        .open(writer_store.path())?;
                    fs2::FileExt::try_lock_exclusive(&owner)?;
                    entered_tx.send(())?;
                    release_rx.recv_timeout(Duration::from_secs(5))?;
                    drop(owner);
                    Ok(false)
                },
            )
        });
        entered_rx.recv_timeout(Duration::from_secs(5))?;
        let writer_attempts = store.active_projection_metrics().writer_lock_attempt_total;
        let (read_tx, read_rx) = mpsc::channel();
        let read_handle = store.read_handle();
        let reader = threads.spawn(move || {
            let result = read_handle.read_event_records();
            read_tx.send(result).expect("reader result receiver");
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match read_rx.try_recv() {
                Err(mpsc::TryRecvError::Empty) => {}
                result => {
                    anyhow::bail!("owned reader returned before waiting for the writer: {result:?}")
                }
            }
            if store.active_projection_metrics().writer_lock_attempt_total > writer_attempts {
                break;
            }
            anyhow::ensure!(
                Instant::now() < deadline,
                "owned reader did not attempt the shared writer coordinator"
            );
            thread::yield_now();
        }
        release_tx.send(())?;
        assert!(!writer.join().expect("writer thread")?);
        let records = read_rx.recv_timeout(Duration::from_secs(5))??;
        reader.join().expect("reader thread");
        Ok(records)
    })?;
    let observed: Vec<_> = records
        .iter()
        .map(SessionStreamRecord::stored_event)
        .collect();
    let expected: Vec<_> = expected
        .iter()
        .map(SessionStreamRecord::stored_event)
        .collect();
    assert_eq!(
        serde_json::to_value(observed)?,
        serde_json::to_value(expected)?
    );
    assert_eq!(fs::read(store.path())?, before);
    Ok(())
}

#[test]
fn coordinated_session_reader_preserves_external_reader_busy_without_recovery() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = populated_store(temp.path().join("session.jsonl"))?;
    let before = fs::read(store.path())?;
    let external = OpenOptions::new()
        .read(true)
        .write(true)
        .open(store.path())?;
    fs2::FileExt::try_lock_exclusive(&external)?;
    for result in [
        store.read_handle().read_event_records(),
        store.read_event_records_coordinated(),
        JsonlSessionStore::read_event_records(store.path()),
    ] {
        let error = result.expect_err("uncoordinated owner must retain its file lock");
        let busy = error
            .downcast_ref::<SessionIoBusyError>()
            .expect("typed reader busy error");
        assert_eq!(busy.kind, SessionIoBusyKind::Reader);
        assert_eq!(busy.path, store.path());
    }
    assert_eq!(fs::read(store.path())?, before);
    drop(external);
    assert_eq!(store.read_event_records_coordinated()?.len(), 1);
    assert_eq!(fs::read(store.path())?, before);
    Ok(())
}

#[test]
fn coordinated_session_reader_rejects_checksum_corruption_and_partial_tail_without_repair()
-> Result<()> {
    for partial_tail in [false, true] {
        let temp = tempfile::tempdir()?;
        let store = populated_store(temp.path().join("session.jsonl"))?;
        let original = fs::read(store.path())?;
        let invalid = if partial_tail {
            let mut bytes = original.clone();
            bytes.extend_from_slice(b"{\"incomplete\":");
            bytes
        } else {
            let mut event: serde_json::Value = serde_json::from_slice(&original)?;
            event["record_checksum"] = "0".repeat(64).into();
            let mut bytes = serde_json::to_vec(&event)?;
            bytes.push(b'\n');
            bytes
        };
        fs::write(store.path(), &invalid)?;
        let artifacts_before = fs::read_dir(temp.path())?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<std::io::Result<BTreeSet<_>>>()?;
        let error = store
            .read_handle()
            .read_event_records()
            .expect_err("strict owned reads cannot repair malformed durable bytes");
        assert!(error.downcast_ref::<SessionIoBusyError>().is_none());
        if !partial_tail {
            assert!(format!("{error:#}").contains("checksum"));
        }
        assert_eq!(fs::read(store.path())?, invalid);
        let artifacts_after = fs::read_dir(temp.path())?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<std::io::Result<BTreeSet<_>>>()?;
        assert_eq!(artifacts_after, artifacts_before);
    }
    Ok(())
}
