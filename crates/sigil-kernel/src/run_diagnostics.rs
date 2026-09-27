//! Bounded, process-local timing observations. These are never execution or recovery authority.

use std::{
    collections::VecDeque,
    sync::{
        LazyLock, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Maximum observations retained in memory, independent of conversation size.
pub const MAX_RUN_TIMINGS: usize = 256;

/// Closed vocabulary: diagnostic callers cannot export payloads as arbitrary labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunTimingPhase {
    InputAccepted,
    FirstFeedbackFrame,
    Admission,
    Preparation,
    SessionPreparation,
    ProviderConstruction,
    ToolSurface,
    RequestContext,
    ProviderDispatch,
    ProviderStreamReady,
    ProviderFirstChunk,
    ProviderFirstContent,
    ToolExecution,
    CancellationRequested,
    CancellationSettled,
}

/// One same-process measurement. Durations have a phase-specific origin, not a remote clock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunTimingObservation {
    pub sequence: u64,
    pub run_key: String,
    pub phase: RunTimingPhase,
    pub observed_at_ms: u64,
    pub elapsed_us: u64,
}

/// Volatile diagnostic snapshot; eviction and contention are explicit, never run failures.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunTimingSnapshot {
    pub process_instance: String,
    pub available: bool,
    pub dropped: u64,
    pub observations: Vec<RunTimingObservation>,
}

struct RunTimings {
    instance: String,
    started: Instant,
    dropped: AtomicU64,
    next_sequence: AtomicU64,
    observations: Mutex<VecDeque<RunTimingObservation>>,
}

static TIMINGS: LazyLock<RunTimings> = LazyLock::new(RunTimings::new);

impl RunTimings {
    fn new() -> Self {
        Self {
            instance: uuid::Uuid::new_v4().to_string(),
            started: Instant::now(),
            dropped: AtomicU64::new(0),
            next_sequence: AtomicU64::new(1),
            observations: Mutex::new(VecDeque::with_capacity(MAX_RUN_TIMINGS)),
        }
    }

    fn record(&self, run_id: &str, phase: RunTimingPhase, elapsed: Duration) {
        // Avoid hashing unbounded input and never wait behind a diagnostic reader/writer.
        if run_id.len() > 256 {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let Ok(mut observations) = self.observations.try_lock() else {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        };
        if observations.len() == MAX_RUN_TIMINGS {
            observations.pop_front();
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
        observations.push_back(RunTimingObservation {
            sequence: self.next_sequence.fetch_add(1, Ordering::Relaxed),
            run_key: run_timing_key(run_id),
            phase,
            observed_at_ms: self
                .started
                .elapsed()
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX),
            elapsed_us: elapsed.as_micros().try_into().unwrap_or(u64::MAX),
        });
    }

    fn snapshot(&self) -> RunTimingSnapshot {
        let observations = self.observations.try_lock().ok();
        RunTimingSnapshot {
            process_instance: self.instance.clone(),
            available: observations.is_some(),
            dropped: self.dropped.load(Ordering::Relaxed),
            observations: observations
                .as_ref()
                .map(|items| items.iter().cloned().collect())
                .unwrap_or_default(),
        }
    }
}

/// Correlates host-issued run identities across adapters without exporting the raw identity.
/// This is a label hash, not authentication, anonymization, or execution authority.
#[must_use]
pub fn run_timing_key(run_id: &str) -> String {
    format!("{:x}", Sha256::digest(run_id.as_bytes()))
}

/// Records a best-effort measurement without I/O, subscriber configuration, or lock waiting.
pub fn record_run_timing(run_id: &str, phase: RunTimingPhase, elapsed: Duration) {
    TIMINGS.record(run_id, phase, elapsed);
}

/// Associates pre-admission observations with the observed host run. It changes diagnostics
/// only, never the local submission identity or the durable run identity.
pub fn correlate_run_timings(local_id: &str, run_id: &str) {
    if local_id.len() > 256 || run_id.len() > 256 {
        TIMINGS.dropped.fetch_add(1, Ordering::Relaxed);
        return;
    }
    let Ok(mut observations) = TIMINGS.observations.try_lock() else {
        TIMINGS.dropped.fetch_add(1, Ordering::Relaxed);
        return;
    };
    let local_key = run_timing_key(local_id);
    let run_key = run_timing_key(run_id);
    for observation in observations
        .iter_mut()
        .filter(|entry| entry.run_key == local_key)
    {
        observation.run_key.clone_from(&run_key);
    }
}

/// Copies the bounded process buffer for an explicitly requested support export.
#[must_use]
pub fn run_timing_snapshot() -> RunTimingSnapshot {
    TIMINGS.snapshot()
}

#[cfg(test)]
#[path = "tests/run_diagnostics_tests.rs"]
mod tests;
