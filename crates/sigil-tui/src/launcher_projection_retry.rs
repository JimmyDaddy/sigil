use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use sigil_application::{ApplicationError, ApplicationFrontier};

const INITIAL_RETRY_DELAY: Duration = Duration::from_millis(250);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(2);
const DELAYED_NOTICE_AFTER: Duration = Duration::from_secs(2);

#[derive(Debug)]
pub(super) enum ProjectionFailure {
    Application(ApplicationError),
    Task(String),
}

impl std::fmt::Display for ProjectionFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Application(error) => std::fmt::Display::fmt(error, formatter),
            Self::Task(error) => formatter.write_str(error),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureNotice {
    Delayed,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct FailureAction {
    pub retry: bool,
    pub notify: bool,
}

/// Only typed Unavailable schedules automatic replay. Projection reads and idempotent ACKs
/// retain their ownership and cursor; unrelated wakeups cannot reset the epoch or deadline.
#[derive(Debug)]
pub(super) struct ProjectionRetry {
    epoch: u64,
    retry_at: Option<Instant>,
    first_failure: Option<Instant>,
    failure_count: u32,
    notice: Option<FailureNotice>,
}

impl ProjectionRetry {
    pub fn new(epoch: u64) -> Self {
        Self {
            epoch,
            retry_at: None,
            first_failure: None,
            failure_count: 0,
            notice: None,
        }
    }

    pub fn reset(&mut self, epoch: u64) {
        *self = Self::new(epoch);
    }

    pub fn can_start(&self, epoch: u64, now: Instant) -> bool {
        self.epoch == epoch && self.retry_at.is_none_or(|deadline| now >= deadline)
    }

    pub fn remaining(&self, now: Instant) -> Option<Duration> {
        self.retry_at
            .map(|deadline| deadline.saturating_duration_since(now))
    }

    pub fn failed(
        &mut self,
        epoch: u64,
        now: Instant,
        error: &ProjectionFailure,
    ) -> Option<FailureAction> {
        if epoch != self.epoch {
            return None;
        }
        if matches!(
            error,
            ProjectionFailure::Application(ApplicationError::Unavailable)
        ) {
            let first_failure = *self.first_failure.get_or_insert(now);
            self.failure_count = self.failure_count.saturating_add(1);
            let factor = 1_u32 << self.failure_count.saturating_sub(1).min(3);
            let delay = (INITIAL_RETRY_DELAY * factor).min(MAX_RETRY_DELAY);
            self.retry_at = Some(now + delay);
            let notify = now.saturating_duration_since(first_failure) >= DELAYED_NOTICE_AFTER
                && self.notice != Some(FailureNotice::Delayed);
            if notify {
                self.notice = Some(FailureNotice::Delayed);
            }
            Some(FailureAction {
                retry: true,
                notify,
            })
        } else {
            self.retry_at = None;
            self.first_failure = None;
            self.failure_count = 0;
            let notify = self.notice != Some(FailureNotice::Stopped);
            self.notice = Some(FailureNotice::Stopped);
            Some(FailureAction {
                retry: false,
                notify,
            })
        }
    }

    /// Returns whether an earlier visible failure should receive one recovery notice.
    pub fn succeeded(&mut self, epoch: u64) -> bool {
        if epoch != self.epoch {
            return false;
        }
        let notify = self.notice.is_some();
        self.reset(epoch);
        notify
    }
}

/// A worker replacement can retain the same session while replacing its application port.
/// Track the actual port owner so boot changes and disconnects invalidate the same retry state
/// as a session transition, without deriving ownership from projected values.
pub(super) fn reconcile_projection_owner<T>(
    owner: &mut Option<Arc<T>>,
    candidate: Option<Arc<T>>,
    epoch: &mut u64,
    refresh: &mut ProjectionRetry,
    acknowledgement: &mut ProjectionRetry,
) -> bool {
    let unchanged = match (owner.as_ref(), candidate.as_ref()) {
        (Some(current), Some(candidate)) => Arc::ptr_eq(current, candidate),
        (None, None) => true,
        _ => false,
    };
    if unchanged {
        return false;
    }
    *owner = candidate;
    *epoch = epoch.wrapping_add(1).max(1);
    refresh.reset(*epoch);
    acknowledgement.reset(*epoch);
    true
}

pub(super) fn projection_wake_deadline(
    app: Option<Duration>,
    refresh: Option<Duration>,
    acknowledgement: Option<Duration>,
) -> Option<Duration> {
    [app, refresh, acknowledgement].into_iter().flatten().min()
}

/// A failed ACK may finish after a newer projection has already been applied. Retain the
/// newer cut and never let an old task reintroduce an acknowledgement for another session.
pub(super) fn should_replace_pending_ack(
    current_epoch: u64,
    pending: Option<(u64, &ApplicationFrontier)>,
    candidate_epoch: u64,
    candidate: &ApplicationFrontier,
) -> bool {
    if candidate_epoch != current_epoch {
        return false;
    }
    let Some((pending_epoch, pending)) = pending else {
        return true;
    };
    pending_epoch != current_epoch
        || (candidate.schema_version == pending.schema_version
            && candidate.scope == pending.scope
            && candidate.writer_generation == pending.writer_generation
            && candidate.stream_generation == pending.stream_generation
            && candidate.through_sequence >= pending.through_sequence)
}

#[cfg(test)]
#[path = "tests/launcher_projection_retry_tests.rs"]
mod tests;
