use std::{
    sync::{Arc, Condvar, Mutex},
    thread::JoinHandle,
    time::Instant,
};

use sigil_kernel::managed_execution::ManagedExecutionErrorV1;

struct DeadlineState {
    stopped: bool,
    outcome: Option<Result<bool, ()>>,
}

struct DeadlineControl {
    state: Mutex<DeadlineState>,
    wake: Condvar,
    stop_process: Box<dyn Fn() -> Result<bool, ()> + Send + Sync>,
}

/// One owned, joined watchdog per persistent process. Its clock continues while input, output,
/// a model turn, or an adapter is blocked. No asynchronous future owns the only deadline poll.
pub(super) struct PersistentRuntimeDeadline {
    at: Instant,
    control: Arc<DeadlineControl>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl PersistentRuntimeDeadline {
    pub(super) fn new(
        at: Instant,
        stop: impl Fn() -> Result<bool, ()> + Send + Sync + 'static,
    ) -> Self {
        Self {
            at,
            control: Arc::new(DeadlineControl {
                state: Mutex::new(DeadlineState {
                    stopped: false,
                    outcome: None,
                }),
                wake: Condvar::new(),
                stop_process: Box::new(stop),
            }),
            worker: Mutex::new(None),
        }
    }

    /// Called only after the handle owns the child and its authority claim, so thread creation
    /// failure can cancel and finalize that same handle before returning an admission failure.
    pub(super) fn start(&self) -> std::io::Result<()> {
        let control = Arc::clone(&self.control);
        let at = self.at;
        let worker = std::thread::Builder::new()
            .name("sigil-process-deadline".to_owned())
            .spawn(move || {
                let mut state = match control.state.lock() {
                    Ok(state) => state,
                    Err(error) => error.into_inner(),
                };
                while !state.stopped && state.outcome.is_none() {
                    let remaining = at.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        state.outcome = Some((control.stop_process)());
                        break;
                    }
                    state = match control.wake.wait_timeout(state, remaining) {
                        Ok((state, _)) => state,
                        Err(error) => error.into_inner().0,
                    };
                }
            })?;
        *self
            .worker
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(worker);
        Ok(())
    }

    pub(super) fn expire(&self) -> Result<(), ManagedExecutionErrorV1> {
        let mut state = self
            .control
            .state
            .lock()
            .map_err(|_| ManagedExecutionErrorV1::OutcomeUncertain)?;
        let stopped = *state
            .outcome
            .get_or_insert_with(|| (self.control.stop_process)());
        self.control.wake.notify_all();
        stopped
            .map(|_| ())
            .map_err(|()| ManagedExecutionErrorV1::OutcomeUncertain)
    }

    pub(super) fn expired(&self) -> Result<bool, ManagedExecutionErrorV1> {
        match self
            .control
            .state
            .lock()
            .map_err(|_| ManagedExecutionErrorV1::OutcomeUncertain)?
            .outcome
        {
            Some(Err(())) => Err(ManagedExecutionErrorV1::OutcomeUncertain),
            Some(Ok(timed_out)) => Ok(timed_out),
            None => Ok(false),
        }
    }

    pub(super) fn finish(&self) -> Result<(), ManagedExecutionErrorV1> {
        self.control
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .stopped = true;
        self.control.wake.notify_all();
        if let Some(worker) = self
            .worker
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            worker
                .join()
                .map_err(|_| ManagedExecutionErrorV1::OutcomeUncertain)?;
        }
        Ok(())
    }
}

impl Drop for PersistentRuntimeDeadline {
    fn drop(&mut self) {
        // Joining is unconditional even if finalization failed; no deadline thread is detached.
        let _ = self.finish();
    }
}
