use std::{io::Cursor, sync::atomic::Ordering};

use super::*;

#[test]
fn bounded_output_collector_preserves_head_tail_and_exact_totals() {
    let limits = OutputCollectionLimits {
        retained_bytes_per_stream: 8,
        hard_bytes_per_stream: 64,
        hard_bytes_combined: 128,
    };
    let stream_total = Arc::new(AtomicU64::new(0));
    let combined_total = Arc::new(AtomicU64::new(0));
    let (alert_tx, alert_rx) = std_mpsc::sync_channel(4);

    let collected = collect_blocking_pipe(
        Cursor::new(b"abcdEFGHijklMNOP".to_vec()),
        ExecutionOutputStream::Stdout,
        limits,
        Arc::clone(&stream_total),
        Arc::clone(&combined_total),
        alert_tx,
    );

    assert_eq!(collected.bytes, b"abcdMNOP");
    assert_eq!(collected.evidence.total_bytes, 16);
    assert_eq!(collected.evidence.returned_bytes, 8);
    assert_eq!(collected.evidence.omitted_bytes, 8);
    assert_eq!(collected.evidence.retained_head_bytes, 4);
    assert_eq!(collected.evidence.retained_tail_bytes, 4);
    assert!(collected.evidence.truncated);
    assert_eq!(combined_total.load(Ordering::Relaxed), 16);
    assert_eq!(stream_total.load(Ordering::Relaxed), 16);
    assert!(alert_rx.try_recv().is_err());
}

#[test]
fn bounded_output_collector_reports_stream_hard_limit_once() {
    let limits = OutputCollectionLimits {
        retained_bytes_per_stream: 8,
        hard_bytes_per_stream: 10,
        hard_bytes_combined: 64,
    };
    let stream_total = Arc::new(AtomicU64::new(0));
    let combined_total = Arc::new(AtomicU64::new(0));
    let (alert_tx, alert_rx) = std_mpsc::sync_channel(4);

    let collected = collect_blocking_pipe(
        Cursor::new(b"0123456789abcdef".to_vec()),
        ExecutionOutputStream::Stderr,
        limits,
        stream_total,
        combined_total,
        alert_tx,
    );

    let alert = alert_rx
        .try_recv()
        .expect("hard limit should emit an alert");
    assert!(matches!(
        alert,
        OutputAlert::OutputLimit {
            stream: ExecutionOutputStream::Stderr,
            limit_bytes: 10,
            observed_bytes: 16,
        }
    ));
    assert!(alert_rx.try_recv().is_err());
    assert_eq!(collected.evidence.total_bytes, 16);
}

#[tokio::test]
async fn bounded_output_async_collector_tracks_lines_without_full_allocation() {
    let limits = OutputCollectionLimits {
        retained_bytes_per_stream: 6,
        hard_bytes_per_stream: 64,
        hard_bytes_combined: 128,
    };
    let (mut writer, reader) = tokio::io::duplex(64);
    let writer_task = tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        writer.write_all(b"a\nb\nc").await.expect("duplex write");
    });
    let stream_total = Arc::new(AtomicU64::new(0));
    let combined_total = Arc::new(AtomicU64::new(0));
    let (alert_tx, mut alert_rx) = mpsc::channel(4);

    let collected = collect_async_pipe(
        reader,
        ExecutionOutputStream::Stdout,
        limits,
        stream_total,
        combined_total,
        alert_tx,
    )
    .await;
    writer_task.await.expect("writer task");

    assert_eq!(collected.bytes, b"a\nb\nc");
    assert_eq!(collected.evidence.total_lines, 3);
    assert!(alert_rx.try_recv().is_err());
}
