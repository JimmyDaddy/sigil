//! Temporary R70.0 phase timing instrumentation.
//!
//! Timing is opt-in so normal TUI runs do not pay for formatting or I/O. When
//! `SIGIL_TUI_PHASE_TIMINGS` is set, each completed phase emits one stable line to stderr. The
//! R70.0 profiler aggregates these lines and keeps the raw logs as evidence.

use std::{env, time::Instant};

pub(crate) struct PhaseTimer {
    name: &'static str,
    started: Instant,
    enabled: bool,
}

impl PhaseTimer {
    pub(crate) fn new(name: &'static str) -> Self {
        Self {
            name,
            started: Instant::now(),
            enabled: env::var_os("SIGIL_TUI_PHASE_TIMINGS").is_some(),
        }
    }
}

impl Drop for PhaseTimer {
    fn drop(&mut self) {
        if self.enabled {
            eprintln!(
                "SIGIL_R70_PHASE name={} elapsed_ns={}",
                self.name,
                self.started.elapsed().as_nanos()
            );
        }
    }
}
