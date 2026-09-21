//! Bounded, process-local cleanup observations. These timings never establish quiescence,
//! resource release or durable cancellation authority.

use std::{
    sync::{Arc, Mutex},
    time::Instant,
};

/// Closed cleanup boundaries, without command text, paths, process IDs or caller labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunCleanupStage {
    CancellationActivation,
    ProcessStop,
    OutputDrain,
    ResourceSettlement,
    AuditPersistence,
    ThreadJoin,
}

impl RunCleanupStage {
    const ALL: [Self; 6] = [
        Self::CancellationActivation,
        Self::ProcessStop,
        Self::OutputDrain,
        Self::ResourceSettlement,
        Self::AuditPersistence,
        Self::ThreadJoin,
    ];

    /// Stable diagnostic label; never an authority or routing decision.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CancellationActivation => "cancellation-activation",
            Self::ProcessStop => "process-stop",
            Self::OutputDrain => "output-drain",
            Self::ResourceSettlement => "resource-settlement",
            Self::AuditPersistence => "audit-persistence",
            Self::ThreadJoin => "thread-join",
        }
    }
}

/// One of six aggregate observations. `active_elapsed_ms` measures the current continuous
/// active window; `elapsed_ms` sums completed attempts and can overlap across concurrent owners.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunCleanupStageSnapshot {
    pub stage: RunCleanupStage,
    pub active: usize,
    pub completed: u64,
    pub failed: u64,
    pub abandoned: u64,
    pub elapsed_ms: u64,
    pub active_elapsed_ms: u64,
}

#[derive(Default)]
struct StageObservation {
    active: usize,
    active_since: Option<Instant>,
    completed: u64,
    failed: u64,
    abandoned: u64,
    elapsed_ms: u64,
}

#[derive(Clone, Default)]
pub(super) struct CleanupProgress {
    stages: Arc<Mutex<[StageObservation; 6]>>,
}

impl CleanupProgress {
    pub(super) fn begin(&self, stage: RunCleanupStage) -> RunCleanupStageGuard {
        let started = Instant::now();
        let mut stages = self
            .stages
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let entry = &mut stages[stage as usize];
        entry.active += 1;
        entry.active_since.get_or_insert(started);
        drop(stages);
        tracing::debug!(
            stage = stage.as_str(),
            phase = "started",
            "run cleanup stage"
        );
        RunCleanupStageGuard {
            progress: self.clone(),
            stage,
            started,
            ended: false,
        }
    }

    pub(super) fn snapshot(&self) -> Vec<RunCleanupStageSnapshot> {
        let stages = self
            .stages
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        RunCleanupStage::ALL
            .into_iter()
            .map(|stage| {
                let entry = &stages[stage as usize];
                RunCleanupStageSnapshot {
                    stage,
                    active: entry.active,
                    completed: entry.completed,
                    failed: entry.failed,
                    abandoned: entry.abandoned,
                    elapsed_ms: entry.elapsed_ms,
                    active_elapsed_ms: entry.active_since.map_or(0, |at| millis(at.elapsed())),
                }
            })
            .collect()
    }
}

/// A real operation's timing scope. Explicit completion records its reported outcome; dropping
/// an unfinished scope records abandonment and cannot turn it into successful cleanup.
pub struct RunCleanupStageGuard {
    progress: CleanupProgress,
    stage: RunCleanupStage,
    started: Instant,
    ended: bool,
}

impl RunCleanupStageGuard {
    /// Finishes observation after the owning operation actually returned.
    pub fn finish(mut self, success: bool) {
        self.end(Some(success));
    }

    fn end(&mut self, success: Option<bool>) {
        let elapsed_ms = millis(self.started.elapsed());
        let mut stages = self
            .progress
            .stages
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let entry = &mut stages[self.stage as usize];
        entry.active -= 1;
        if entry.active == 0 {
            entry.active_since = None;
        }
        entry.elapsed_ms = entry.elapsed_ms.saturating_add(elapsed_ms);
        match success {
            Some(true) => entry.completed = entry.completed.saturating_add(1),
            Some(false) => entry.failed = entry.failed.saturating_add(1),
            None => entry.abandoned = entry.abandoned.saturating_add(1),
        }
        self.ended = true;
        drop(stages);
        tracing::debug!(
            stage = self.stage.as_str(),
            phase = "finished",
            elapsed_ms,
            outcome = match success {
                Some(true) => "completed",
                Some(false) => "failed",
                None => "abandoned",
            },
            "run cleanup stage"
        );
    }
}

impl Drop for RunCleanupStageGuard {
    fn drop(&mut self) {
        if !self.ended {
            self.end(None);
        }
    }
}

fn millis(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
#[path = "tests/cleanup_progress_tests.rs"]
mod tests;
