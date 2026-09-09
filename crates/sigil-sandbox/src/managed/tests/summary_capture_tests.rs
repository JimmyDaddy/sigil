use super::*;

#[test]
fn reader_error_preserves_observed_bytes_and_is_not_retention_truncation() {
    let mut delivered = false;
    let capture = spawn_capture_reader(
        move |buffer| {
            if delivered {
                return Err(io::Error::other("injected pipe failure"));
            }
            delivered = true;
            buffer[..6].copy_from_slice(b"prefix");
            Ok(AvailableRead::Bytes(6))
        },
        16,
        ManagedProcessOutputChannelV1::Stdout,
        None,
    )
    .expect("reader");
    capture.mark_leader_finished();
    let summary = futures::executor::block_on(capture.finish()).summary;
    assert_eq!(summary.source, ManagedOutputSourceV1::ReadFailed);
    assert_eq!(summary.observed_bytes, 6);
    assert_eq!(summary.retained_payload, b"prefix");
    assert!(!summary.truncated);
}

#[test]
fn interrupted_read_retries_until_actual_eof() {
    let mut interrupted = false;
    let capture = spawn_capture_reader(
        move |_| {
            if interrupted {
                Ok(AvailableRead::Eof)
            } else {
                interrupted = true;
                Err(io::Error::from(io::ErrorKind::Interrupted))
            }
        },
        16,
        ManagedProcessOutputChannelV1::Stdout,
        None,
    )
    .expect("reader");
    capture.mark_leader_finished();
    let summary = futures::executor::block_on(capture.finish()).summary;
    assert_eq!(summary.source, ManagedOutputSourceV1::Complete);
    assert_eq!(summary.observed_bytes, 0);
    assert!(!summary.truncated);
}

struct ReaderDropProbe(Arc<AtomicBool>);

impl Drop for ReaderDropProbe {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

#[test]
fn dropping_capture_stops_and_joins_its_reader() {
    let dropped = Arc::new(AtomicBool::new(false));
    let probe = ReaderDropProbe(Arc::clone(&dropped));
    let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
    let mut started_tx = Some(started_tx);
    let capture = spawn_capture_reader(
        move |_| {
            let _ = &probe;
            if let Some(started_tx) = started_tx.take() {
                started_tx.send(()).expect("reader owner still waiting");
            }
            Ok(AvailableRead::Pending)
        },
        16,
        ManagedProcessOutputChannelV1::Stdout,
        None,
    )
    .expect("reader");
    started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("reader started");
    let started = Instant::now();
    drop(capture);
    assert!(started.elapsed() < Duration::from_millis(500));
    assert!(
        dropped.load(Ordering::Acquire),
        "reader lifetime must end before owner drop returns"
    );
}

#[test]
fn continuously_readable_post_leader_source_stops_at_total_deadline_and_joins() {
    let dropped = Arc::new(AtomicBool::new(false));
    let probe = ReaderDropProbe(Arc::clone(&dropped));
    let capture = spawn_capture_reader(
        move |buffer| {
            let _ = &probe;
            buffer.fill(b'x');
            Ok(AvailableRead::Bytes(buffer.len()))
        },
        16,
        ManagedProcessOutputChannelV1::Stdout,
        None,
    )
    .expect("reader");
    let started = Instant::now();
    capture.mark_leader_finished();
    let summary = futures::executor::block_on(capture.finish()).summary;
    assert!(started.elapsed() >= POST_LEADER_TOTAL_BUDGET);
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(summary.source, ManagedOutputSourceV1::Incomplete);
    assert!(summary.observed_bytes > summary.retained_bytes);
    assert_eq!(summary.retained_bytes, 16);
    assert!(summary.truncated);
    assert!(dropped.load(Ordering::Acquire));
}

#[cfg(unix)]
#[test]
fn idle_inherited_writer_is_incomplete_without_fabricating_retention_truncation() {
    use std::io::Write;
    use std::os::unix::net::UnixStream;

    let (reader, mut writer) = UnixStream::pair().expect("owned pipe fixture");
    writer.write_all(b"prefix").expect("prefix");
    let capture = spawn_summary_capture(reader, 16, ManagedProcessOutputChannelV1::Stdout, None)
        .expect("reader");
    let started = Instant::now();
    capture.mark_leader_finished();
    let summary = futures::executor::block_on(capture.finish()).summary;
    assert!(started.elapsed() >= POST_LEADER_IDLE_BUDGET);
    assert!(started.elapsed() < Duration::from_millis(500));
    assert_eq!(summary.source, ManagedOutputSourceV1::Incomplete);
    assert_eq!(summary.observed_bytes, 6);
    assert_eq!(summary.retained_payload, b"prefix");
    assert!(!summary.truncated);
    assert!(writer.write_all(b"reader must be closed").is_err());
}

#[cfg(unix)]
#[test]
fn post_leader_progress_resets_idle_budget_until_actual_eof() {
    use std::io::Write;
    use std::os::unix::net::UnixStream;

    let (reader, mut writer) = UnixStream::pair().expect("owned pipe fixture");
    let writer = std::thread::spawn(move || {
        for _ in 0..6 {
            writer.write_all(b"progress\n").expect("write progress");
            std::thread::sleep(Duration::from_millis(20));
        }
    });
    let capture = spawn_summary_capture(reader, 128, ManagedProcessOutputChannelV1::Stdout, None)
        .expect("reader");
    capture.mark_leader_finished();
    let summary = futures::executor::block_on(capture.finish()).summary;
    writer.join().expect("writer");
    assert_eq!(summary.source, ManagedOutputSourceV1::Complete);
    assert_eq!(summary.retained_payload, b"progress\n".repeat(6));
    assert_eq!(summary.observed_bytes, 54);
    assert!(!summary.truncated);
}
