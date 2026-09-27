//! Phase observations use the bounded in-memory support buffer without formatting or I/O.
//! `SIGIL_TUI_PHASE_TIMINGS` independently enables legacy R70 profiler lines on stderr.

use std::{env, time::Instant};

pub(crate) struct PhaseTimer {
    name: &'static str,
    started: Instant,
    enabled: bool,
    diagnostic: Option<(String, sigil_kernel::run_diagnostics::RunTimingPhase)>,
}

impl PhaseTimer {
    pub(crate) fn new(name: &'static str) -> Self {
        Self {
            name,
            started: Instant::now(),
            enabled: env::var_os("SIGIL_TUI_PHASE_TIMINGS").is_some(),
            diagnostic: None,
        }
    }

    pub(crate) fn observed(
        name: &'static str,
        diagnostic_id: &str,
        phase: sigil_kernel::run_diagnostics::RunTimingPhase,
    ) -> Self {
        Self {
            diagnostic: Some((diagnostic_id.to_owned(), phase)),
            ..Self::new(name)
        }
    }
}

impl Drop for PhaseTimer {
    fn drop(&mut self) {
        let elapsed = self.started.elapsed();
        if let Some((diagnostic_id, phase)) = &self.diagnostic {
            sigil_kernel::run_diagnostics::record_run_timing(diagnostic_id, *phase, elapsed);
        }
        if self.enabled {
            eprintln!(
                "SIGIL_R70_PHASE name={} elapsed_ns={}",
                self.name,
                elapsed.as_nanos()
            );
        }
    }
}
