//! Bounded transfer from pipe readers to artifact storage. Readers never perform disk I/O.

use super::*;
use sigil_kernel::{ExecutionCaptureHandle, RunCancellationHandle, RunTaskGuard};

const WRITE_BATCH_BYTES: usize = 256 * 1024;

#[derive(Default)]
struct PendingStream {
    chunks: Vec<Vec<u8>>,
    retained_bytes: usize,
    observed_bytes: u64,
}

impl PendingStream {
    fn push(&mut self, bytes: &[u8], limit: usize) {
        self.observed_bytes = self.observed_bytes.saturating_add(bytes.len() as u64);
        let retained = bytes.len().min(limit.saturating_sub(self.retained_bytes));
        let mut bytes = &bytes[..retained];
        while !bytes.is_empty() {
            if self
                .chunks
                .last()
                .is_none_or(|chunk| chunk.len() == WRITE_BATCH_BYTES)
            {
                self.chunks.push(Vec::with_capacity(
                    WRITE_BATCH_BYTES.min(limit.saturating_sub(self.retained_bytes)),
                ));
            }
            if let Some(chunk) = self.chunks.last_mut() {
                let count = bytes
                    .len()
                    .min(WRITE_BATCH_BYTES.saturating_sub(chunk.len()));
                chunk.extend_from_slice(&bytes[..count]);
                self.retained_bytes += count;
                bytes = &bytes[count..];
            }
        }
    }

    fn flush(self, capture: &mut ExecutionCaptureHandle, stream: ToolOutputStreamV1) {
        for chunk in self.chunks {
            if capture.sink.write_stream(stream, &chunk).is_err() {
                capture.sink.mark_process_write_failed();
                break;
            }
        }
        if capture
            .sink
            .record_process_stream_observation(stream, self.observed_bytes)
            .is_err()
        {
            capture.sink.mark_process_write_failed();
        }
    }
}

struct PendingCapture {
    capture: ExecutionCaptureHandle,
    stdout: PendingStream,
    stderr: PendingStream,
    // Registered before process dispatch; cancellation cannot declare quiescence during flush.
    _task_guard: Option<RunTaskGuard>,
}

pub(super) struct ManagedOutputCaptureBridge {
    pending: Mutex<Option<PendingCapture>>,
    failed: std::sync::atomic::AtomicBool,
}

impl ManagedOutputCaptureBridge {
    pub(super) fn new(
        capture: ExecutionCaptureHandle,
        cancellation: Option<&RunCancellationHandle>,
    ) -> Result<Self> {
        let task_guard = cancellation
            .map(RunCancellationHandle::register_task)
            .transpose()?;
        Ok(Self {
            pending: Mutex::new(Some(PendingCapture {
                capture,
                stdout: PendingStream::default(),
                stderr: PendingStream::default(),
                _task_guard: task_guard,
            })),
            failed: std::sync::atomic::AtomicBool::new(false),
        })
    }

    pub(super) async fn finish(&self) -> Result<ExecutionCaptureHandle> {
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| anyhow!("managed capture buffer lock poisoned"))?
            .take()
            .ok_or_else(|| anyhow!("managed capture buffer already closed"))?;
        if self.failed.load(std::sync::atomic::Ordering::SeqCst) {
            pending.capture.sink.mark_process_write_failed();
        }
        // This is finite work over an already closed, bounded buffer. The runtime owns the
        // blocking task, and its root guard survives cancellation of the awaiting future.
        let finished = tokio::task::spawn_blocking(move || {
            std::mem::take(&mut pending.stdout)
                .flush(&mut pending.capture, ToolOutputStreamV1::Stdout);
            std::mem::take(&mut pending.stderr)
                .flush(&mut pending.capture, ToolOutputStreamV1::Stderr);
            // Preserve field drop order (capture before guard), including panic or a dropped
            // awaiting future: physical staging cleanup must finish before quiescence.
            pending
        })
        .await
        .map_err(|error| anyhow!("managed capture storage task failed: {error}"))?;
        Ok(finished.capture)
    }
}

impl sigil_sandbox::managed::ManagedOutputCaptureSinkV1 for ManagedOutputCaptureBridge {
    fn write_chunk(
        &self,
        channel: ManagedProcessOutputChannelV1,
        bytes: &[u8],
    ) -> Result<(), sigil_kernel::managed_execution::ManagedExecutionErrorV1> {
        use sigil_kernel::managed_execution::ManagedExecutionErrorV1;
        let mut pending = self.pending.lock().map_err(|_| {
            self.failed.store(true, std::sync::atomic::Ordering::SeqCst);
            ManagedExecutionErrorV1::OutcomeUncertain
        })?;
        let Some(pending) = pending.as_mut() else {
            self.failed.store(true, std::sync::atomic::Ordering::SeqCst);
            return Err(ManagedExecutionErrorV1::OutcomeUncertain);
        };
        let limit = pending
            .capture
            .config
            .artifact_staging_limit_bytes_per_stream
            .min(sigil_kernel::session::TOOL_ARTIFACT_MAX_BYTES as u64)
            as usize;
        match channel {
            ManagedProcessOutputChannelV1::Stdout => pending.stdout.push(bytes, limit),
            ManagedProcessOutputChannelV1::Stderr => pending.stderr.push(bytes, limit),
            ManagedProcessOutputChannelV1::Pty => {
                self.failed.store(true, std::sync::atomic::Ordering::SeqCst);
                return Err(ManagedExecutionErrorV1::OutcomeUncertain);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "../tests/managed_output_capture_tests.rs"]
mod tests;
