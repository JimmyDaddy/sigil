use std::time::{Duration, Instant};

/// Returns elapsed time without panicking if the stored instant is later than now.
pub fn saturating_elapsed(started_at: Instant) -> Duration {
    Instant::now()
        .checked_duration_since(started_at)
        .unwrap_or_default()
}

#[cfg(test)]
#[path = "tests/time_tests.rs"]
mod tests;
