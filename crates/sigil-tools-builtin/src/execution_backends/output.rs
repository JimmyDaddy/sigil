use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

#[cfg(test)]
use std::{io::Read, sync::mpsc as std_mpsc};

use sigil_kernel::{ExecutionOutputStream, ExecutionStreamCapture, ExecutionTerminationCause};
use tokio::{io::AsyncRead, io::AsyncReadExt, sync::mpsc};

pub(super) const EXECUTION_RETAINED_BYTES_PER_STREAM: u64 = 64 * 1024;
pub(super) const EXECUTION_HARD_BYTES_PER_STREAM: u64 = 8 * 1024 * 1024;
pub(super) const EXECUTION_HARD_BYTES_COMBINED: u64 = 16 * 1024 * 1024;
pub(super) const PREFLIGHT_RETAINED_BYTES_PER_STREAM: u64 = 16 * 1024;
pub(super) const PREFLIGHT_HARD_BYTES_PER_STREAM: u64 = 256 * 1024;
pub(super) const PREFLIGHT_HARD_BYTES_COMBINED: u64 = 512 * 1024;

const OUTPUT_READ_BUFFER_BYTES: usize = 8 * 1024;
const MAX_READER_ERROR_CHARS: usize = 512;

#[derive(Debug, Clone, Copy)]
pub(super) struct OutputCollectionLimits {
    pub(super) retained_bytes_per_stream: u64,
    pub(super) hard_bytes_per_stream: u64,
    pub(super) hard_bytes_combined: u64,
}

impl OutputCollectionLimits {
    pub(super) const fn execution() -> Self {
        Self {
            retained_bytes_per_stream: EXECUTION_RETAINED_BYTES_PER_STREAM,
            hard_bytes_per_stream: EXECUTION_HARD_BYTES_PER_STREAM,
            hard_bytes_combined: EXECUTION_HARD_BYTES_COMBINED,
        }
    }

    pub(super) const fn preflight() -> Self {
        Self {
            retained_bytes_per_stream: PREFLIGHT_RETAINED_BYTES_PER_STREAM,
            hard_bytes_per_stream: PREFLIGHT_HARD_BYTES_PER_STREAM,
            hard_bytes_combined: PREFLIGHT_HARD_BYTES_COMBINED,
        }
    }
}

#[derive(Debug, Clone)]
pub(super) enum OutputAlert {
    OutputLimit {
        stream: ExecutionOutputStream,
        limit_bytes: u64,
        observed_bytes: u64,
    },
    ReaderFailed {
        stream: ExecutionOutputStream,
        reason: String,
    },
}

impl OutputAlert {
    pub(super) fn termination(&self) -> ExecutionTerminationCause {
        match self {
            Self::OutputLimit {
                stream,
                limit_bytes,
                observed_bytes,
            } => ExecutionTerminationCause::OutputLimit {
                stream: *stream,
                limit_bytes: *limit_bytes,
                observed_bytes: *observed_bytes,
            },
            Self::ReaderFailed { stream, reason } => ExecutionTerminationCause::ReaderFailed {
                stream: *stream,
                reason: reason.clone(),
            },
        }
    }
}

#[derive(Debug)]
pub(super) struct CollectedPipe {
    pub(super) bytes: Vec<u8>,
    pub(super) evidence: ExecutionStreamCapture,
}

pub(super) async fn collect_async_pipe<R>(
    mut reader: R,
    stream: ExecutionOutputStream,
    limits: OutputCollectionLimits,
    stream_total: Arc<AtomicU64>,
    combined_total: Arc<AtomicU64>,
    alert_tx: mpsc::Sender<OutputAlert>,
) -> CollectedPipe
where
    R: AsyncRead + Unpin,
{
    let mut collector = HeadTailCollector::new(limits.retained_bytes_per_stream);
    let mut read_buffer = [0u8; OUTPUT_READ_BUFFER_BYTES];
    let mut limit_reported = false;
    loop {
        match reader.read(&mut read_buffer).await {
            Ok(0) => break,
            Ok(read) => {
                let stream_observed = add_to_total(&stream_total, read as u64);
                let combined = add_to_total(&combined_total, read as u64);
                collector.push(&read_buffer[..read]);
                if !limit_reported
                    && let Some(alert) =
                        output_limit_alert(stream, stream_observed, combined, limits)
                {
                    let _ = alert_tx.try_send(alert);
                    limit_reported = true;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => {
                let _ = alert_tx.try_send(OutputAlert::ReaderFailed {
                    stream,
                    reason: bounded_reader_error(&error),
                });
                break;
            }
        }
    }
    collector.finish(limits.hard_bytes_per_stream)
}

#[cfg(test)]
pub(super) fn collect_blocking_pipe<R>(
    mut reader: R,
    stream: ExecutionOutputStream,
    limits: OutputCollectionLimits,
    stream_total: Arc<AtomicU64>,
    combined_total: Arc<AtomicU64>,
    alert_tx: std_mpsc::SyncSender<OutputAlert>,
) -> CollectedPipe
where
    R: Read,
{
    let mut collector = HeadTailCollector::new(limits.retained_bytes_per_stream);
    let mut read_buffer = [0u8; OUTPUT_READ_BUFFER_BYTES];
    let mut limit_reported = false;
    loop {
        match reader.read(&mut read_buffer) {
            Ok(0) => break,
            Ok(read) => {
                let stream_observed = add_to_total(&stream_total, read as u64);
                let combined = add_to_total(&combined_total, read as u64);
                collector.push(&read_buffer[..read]);
                if !limit_reported
                    && let Some(alert) =
                        output_limit_alert(stream, stream_observed, combined, limits)
                {
                    let _ = alert_tx.try_send(alert);
                    limit_reported = true;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => {
                let _ = alert_tx.try_send(OutputAlert::ReaderFailed {
                    stream,
                    reason: bounded_reader_error(&error),
                });
                break;
            }
        }
    }
    collector.finish(limits.hard_bytes_per_stream)
}

fn output_limit_alert(
    stream: ExecutionOutputStream,
    stream_total: u64,
    combined_total: u64,
    limits: OutputCollectionLimits,
) -> Option<OutputAlert> {
    if stream_total > limits.hard_bytes_per_stream {
        Some(OutputAlert::OutputLimit {
            stream,
            limit_bytes: limits.hard_bytes_per_stream,
            observed_bytes: stream_total,
        })
    } else if combined_total > limits.hard_bytes_combined {
        Some(OutputAlert::OutputLimit {
            stream: ExecutionOutputStream::Combined,
            limit_bytes: limits.hard_bytes_combined,
            observed_bytes: combined_total,
        })
    } else {
        None
    }
}

fn add_to_total(total: &AtomicU64, amount: u64) -> u64 {
    let previous = total
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            Some(current.saturating_add(amount))
        })
        .unwrap_or_else(|current| current);
    previous.saturating_add(amount)
}

fn bounded_reader_error(error: &std::io::Error) -> String {
    error
        .to_string()
        .chars()
        .take(MAX_READER_ERROR_CHARS)
        .collect()
}

#[derive(Debug)]
struct HeadTailCollector {
    head: Vec<u8>,
    tail: Vec<u8>,
    head_limit: usize,
    tail_limit: usize,
    total_bytes: u64,
    newline_count: u64,
    ends_with_newline: bool,
}

impl HeadTailCollector {
    fn new(retained_limit: u64) -> Self {
        let retained_limit = usize::try_from(retained_limit).unwrap_or(usize::MAX);
        let head_limit = retained_limit / 2;
        Self {
            head: Vec::with_capacity(head_limit),
            tail: Vec::with_capacity(retained_limit.saturating_sub(head_limit)),
            head_limit,
            tail_limit: retained_limit.saturating_sub(head_limit),
            total_bytes: 0,
            newline_count: 0,
            ends_with_newline: false,
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.total_bytes = self.total_bytes.saturating_add(bytes.len() as u64);
        self.newline_count = self
            .newline_count
            .saturating_add(bytes.iter().filter(|byte| **byte == b'\n').count() as u64);
        self.ends_with_newline = bytes.ends_with(b"\n");

        let head_remaining = self.head_limit.saturating_sub(self.head.len());
        let head_take = head_remaining.min(bytes.len());
        self.head.extend_from_slice(&bytes[..head_take]);
        self.push_tail(&bytes[head_take..]);
    }

    fn push_tail(&mut self, bytes: &[u8]) {
        if bytes.is_empty() || self.tail_limit == 0 {
            return;
        }
        if bytes.len() >= self.tail_limit {
            self.tail.clear();
            self.tail
                .extend_from_slice(&bytes[bytes.len() - self.tail_limit..]);
            return;
        }
        let excess = self
            .tail
            .len()
            .saturating_add(bytes.len())
            .saturating_sub(self.tail_limit);
        if excess > 0 {
            self.tail.drain(..excess);
        }
        self.tail.extend_from_slice(bytes);
    }

    fn finish(self, hard_limit_bytes: u64) -> CollectedPipe {
        let head_bytes = self.head.len() as u64;
        let tail_bytes = self.tail.len() as u64;
        let returned_bytes = head_bytes.saturating_add(tail_bytes);
        let total_lines = self
            .newline_count
            .saturating_add(u64::from(self.total_bytes > 0 && !self.ends_with_newline));
        let mut bytes = self.head;
        bytes.extend_from_slice(&self.tail);
        CollectedPipe {
            bytes,
            evidence: ExecutionStreamCapture {
                total_bytes: self.total_bytes,
                returned_bytes,
                omitted_bytes: self.total_bytes.saturating_sub(returned_bytes),
                retained_head_bytes: head_bytes,
                retained_tail_bytes: tail_bytes,
                retained_limit_bytes: (self.head_limit.saturating_add(self.tail_limit)) as u64,
                hard_limit_bytes,
                total_lines,
                truncated: self.total_bytes > returned_bytes,
            },
        }
    }
}

#[cfg(test)]
#[path = "tests/output_tests.rs"]
mod tests;
