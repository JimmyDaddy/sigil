use sigil_kernel::run_diagnostics::{RunTimingPhase, correlate_run_timings, record_run_timing};
use std::time::{Duration, Instant};

#[derive(Debug)]
pub(crate) struct SubmissionTiming {
    id: String,
    started: Instant,
    feedback_recorded: bool,
    linked: bool,
    cancel_started: Option<Instant>,
}

impl SubmissionTiming {
    pub(super) fn new() -> Self {
        let id = uuid::Uuid::new_v4().to_string();
        record_run_timing(&id, RunTimingPhase::InputAccepted, Duration::ZERO);
        Self {
            id,
            started: Instant::now(),
            feedback_recorded: false,
            linked: false,
            cancel_started: None,
        }
    }

    pub(crate) fn feedback_presented(&mut self) {
        if !self.feedback_recorded {
            self.feedback_recorded = true;
            record_run_timing(
                &self.id,
                RunTimingPhase::FirstFeedbackFrame,
                self.started.elapsed(),
            );
        }
    }

    pub(super) fn observe_run(&mut self, run_id: &str) {
        if !self.linked {
            correlate_run_timings(&self.id, run_id);
            self.id = run_id.to_owned();
            self.linked = true;
            record_run_timing(run_id, RunTimingPhase::Admission, self.started.elapsed());
        }
    }

    pub(crate) fn cancellation_requested(&mut self) {
        if self.cancel_started.is_none() {
            self.cancel_started = Some(Instant::now());
            record_run_timing(
                &self.id,
                RunTimingPhase::CancellationRequested,
                Duration::ZERO,
            );
        }
    }

    pub(super) fn cancellation_settled(&mut self) {
        if let Some(started) = self.cancel_started.take() {
            record_run_timing(
                &self.id,
                RunTimingPhase::CancellationSettled,
                started.elapsed(),
            );
        }
    }
}

#[cfg(test)]
#[path = "tests/run_timing_tests.rs"]
mod tests;
