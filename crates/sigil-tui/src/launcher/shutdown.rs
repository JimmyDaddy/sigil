//! Explicit exit ownership. A slow wait is progress, never a cleanup failure.

use super::*;
use std::task::Poll;

pub(crate) enum ShutdownPoll {
    Pending,
    Complete,
    Failed(anyhow::Error),
}

#[derive(Default)]
pub(crate) struct ShutdownPass {
    pending: Vec<String>,
    failures: Vec<anyhow::Error>,
}

impl ShutdownPass {
    pub(crate) fn observe(&mut self, component: &str, poll: ShutdownPoll) {
        match poll {
            ShutdownPoll::Pending => self.pending.push(component.to_owned()),
            ShutdownPoll::Complete => {}
            ShutdownPoll::Failed(error) => self.failures.push(error.context(component.to_owned())),
        }
    }

    pub(crate) fn append(&mut self, mut other: Self) {
        self.pending.append(&mut other.pending);
        self.failures.append(&mut other.failures);
    }
}

fn poll_join<T>(owned: &mut Option<std::thread::JoinHandle<T>>) -> Poll<Result<T>> {
    let handle = owned.as_ref().expect("join requires an owned handle");
    if !handle.is_finished() {
        return Poll::Pending;
    }
    let name = handle
        .thread()
        .name()
        .unwrap_or("unnamed-owned-thread")
        .to_owned();
    let started = Instant::now();
    let result = owned.take().expect("finished thread remains owned").join().map_err(|payload| {
        anyhow::anyhow!("owned_thread={name}; stage=thread-join; worker panicked during shutdown: {}; cleanup_complete=false", format_panic_payload(payload.as_ref()))
    });
    tracing::debug!(
        owned_thread = name,
        elapsed_ms = started.elapsed().as_millis(),
        success = result.is_ok(),
        "owned shutdown thread joined"
    );
    Poll::Ready(result)
}

pub(crate) fn poll_owned_thread(owned: &mut Option<std::thread::JoinHandle<()>>) -> ShutdownPoll {
    if owned.is_none() {
        return ShutdownPoll::Complete;
    }
    match poll_join(owned) {
        Poll::Pending => ShutdownPoll::Pending,
        Poll::Ready(Ok(())) => ShutdownPoll::Complete,
        Poll::Ready(Err(error)) => ShutdownPoll::Failed(error),
    }
}

fn poll_runtime_thread(
    handle: &mut Option<std::thread::JoinHandle<()>>,
    worker_tx: &runner::WorkerCommandSender,
    component: &str,
) -> ShutdownPoll {
    match poll_owned_thread(handle) {
        ShutdownPoll::Failed(error) => {
            worker_tx.record_shutdown_join_panic();
            ShutdownPoll::Failed(error.context(worker_tx.shutdown_diagnostic(component)))
        }
        poll => poll,
    }
}

pub(super) fn request_worker_shutdown(worker: &Option<WorkerRuntime>) {
    if let Some(runtime) = worker {
        runtime.worker_tx.begin_shutdown();
        let _ = runtime.worker_tx.send(AppState::shutdown_command());
    }
}

pub(super) fn poll_worker_shutdown(worker: &mut Option<WorkerRuntime>) -> ShutdownPass {
    let mut pass = ShutdownPass::default();
    let Some(runtime) = worker.as_mut() else {
        return pass;
    };
    pass.observe(
        "sigil-agent-worker",
        poll_runtime_thread(
            &mut runtime.join_handle,
            &runtime.worker_tx,
            "sigil-agent-worker",
        ),
    );
    if let Some(pending) = runtime.pending_admission.as_mut() {
        pass.observe(
            "application-admission",
            poll_runtime_thread(
                &mut pending.handle,
                &runtime.worker_tx,
                "application-admission",
            ),
        );
    }
    for pending in &mut runtime.pending_interactions {
        pass.observe(
            "application-interaction",
            poll_runtime_thread(
                &mut pending.handle,
                &runtime.worker_tx,
                "application-interaction",
            ),
        );
    }
    #[cfg(not(test))]
    {
        runtime
            .worker_rx
            .stopped
            .store(true, std::sync::atomic::Ordering::Release);
        pass.observe(
            "sigil-tui-worker-events",
            poll_runtime_thread(
                &mut runtime.worker_rx.handle,
                &runtime.worker_tx,
                "sigil-tui-worker-events",
            ),
        );
    }
    if runtime
        .application
        .as_ref()
        .is_some_and(|application| application.pending_observations() > 0)
    {
        pass.observe("application-observation", ShutdownPoll::Pending);
    }
    let (effects, tasks) = runtime.worker_tx.shutdown_active_counts();
    if effects != 0 || tasks != 0 {
        pass.observe(
            &runtime.worker_tx.shutdown_diagnostic("run-cleanup"),
            ShutdownPoll::Pending,
        );
    }
    if pass.pending.is_empty() {
        if !runtime.worker_tx.cleanup_complete() {
            pass.observe(
                "run-cleanup",
                ShutdownPoll::Failed(anyhow::anyhow!(
                    runtime.worker_tx.shutdown_diagnostic("sigil-agent-worker")
                )),
            );
        }
        runtime.worker_tx.record_shutdown_joins_complete();
        tracing::info!(
            diagnostic = runtime.worker_tx.shutdown_diagnostic("sigil-agent-worker"),
            "worker shutdown settled"
        );
        worker.take();
    } else {
        pass.pending
            .push(runtime.worker_tx.shutdown_diagnostic("sigil-agent-worker"));
    }
    pass
}

pub(super) fn shutdown_worker_to_completion(worker: &mut Option<WorkerRuntime>) -> Result<()> {
    request_worker_shutdown(worker);
    drain_shutdown(
        Instant::now(),
        WORKER_SHUTDOWN_TIMEOUT,
        || poll_worker_shutdown(worker),
        |notice| tracing::warn!("{notice}"),
    )
}

/// Every owner is polled even after another has failed. Errors are returned only after the last
/// actual owner has joined; a hint never becomes an error retained for a later successful join.
pub(super) fn drain_shutdown(
    started: Instant,
    hint_after: Duration,
    mut poll: impl FnMut() -> ShutdownPass,
    mut notice: impl FnMut(&str),
) -> Result<()> {
    let mut failures = Vec::new();
    let mut next_notice = started + hint_after;
    let mut slow = false;
    loop {
        let mut pass = poll();
        failures.append(&mut pass.failures);
        if pass.pending.is_empty() {
            let success = failures.is_empty();
            tracing::info!(
                elapsed_ms = started.elapsed().as_millis(),
                success,
                "TUI shutdown owners settled"
            );
            if slow {
                notice(&format!(
                    "Cleanup {} after {} ms.",
                    if success {
                        "completed"
                    } else {
                        "finished with errors"
                    },
                    started.elapsed().as_millis()
                ));
            }
            return finish_tui_shutdown(Ok(()), failures.into_iter().map(Err));
        }
        let now = Instant::now();
        if now >= next_notice {
            slow = true;
            notice(&format!(
                "Still cleaning up ({} ms): {}",
                started.elapsed().as_millis(),
                pass.pending
                    .iter()
                    .take(8)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
            next_notice = now + Duration::from_secs(1);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Transfer runtime destruction to a real joined thread. Retain the runtime on spawn failure so
/// retrying cannot lose its ownership or block the terminal thread inside a failed spawn's Drop.
pub(super) fn start_event_runtime_shutdown(
    runtime: &mut Option<tokio::runtime::Runtime>,
) -> Result<Option<std::thread::JoinHandle<()>>> {
    let Some(owned) = runtime.take() else {
        return Ok(None);
    };
    let retained = Arc::new(std::sync::Mutex::new(Some(owned)));
    let task = Arc::clone(&retained);
    match std::thread::Builder::new()
        .name("sigil-tui-event-runtime-shutdown".to_owned())
        .spawn(move || {
            let runtime = task
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            drop(runtime);
        }) {
        Ok(handle) => Ok(Some(handle)),
        Err(error) => {
            *runtime = retained
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            Err(error.into())
        }
    }
}

pub(super) fn shutdown_tui_owners(
    app: &mut AppState,
    worker: &mut Option<WorkerRuntime>,
    shutdown: &mut TuiShutdownState,
    event_runtime: &mut Option<tokio::runtime::Runtime>,
    cleanup_bootstrap: bool,
    mut notice: impl FnMut(&str),
) -> Result<()> {
    let started = *shutdown.started.get_or_insert_with(Instant::now);
    request_worker_shutdown(worker);
    app.cancel_session_auxiliary();
    if let Some(owner) = app.runtime_transition.as_ref() {
        owner.request_exit();
    }
    if let Some(owner) = app.runtime_maintenance.as_ref() {
        owner.request_exit();
    }
    abort_projection_observations(&shutdown.projection_observation_owners);
    let mut runtime_handle = None;
    let mut runtime_spawn_error = false;
    let mut primary_owners_settled = false;
    let mut bootstrap_handle = None;
    let mut failed = false;
    drain_shutdown(
        started,
        shutdown.hint_after,
        || {
            let mut pass = ShutdownPass::default();
            if !primary_owners_settled {
                if event_runtime.is_some() {
                    match start_event_runtime_shutdown(event_runtime) {
                        Ok(handle) => runtime_handle = handle,
                        Err(error) => {
                            if !runtime_spawn_error {
                                pass.observe(
                                    "event-runtime-shutdown-spawn",
                                    ShutdownPoll::Failed(error),
                                );
                                runtime_spawn_error = true;
                            }
                            pass.observe("event-runtime-shutdown-spawn", ShutdownPoll::Pending);
                        }
                    }
                }
                pass.observe("event-runtime", poll_owned_thread(&mut runtime_handle));
                pass.append(poll_worker_shutdown(worker));
                pass.append(app.poll_session_auxiliary_shutdown());
                pass.observe(
                    "control-log-recovery",
                    app.control_log_recovery.poll_shutdown(),
                );
                release_finished_projection_observations(
                    &mut shutdown.projection_observation_owners,
                );
                if !shutdown.projection_observation_owners.is_empty() {
                    pass.observe(
                        "current-and-retired-projection-observations",
                        ShutdownPoll::Pending,
                    );
                }
                if let Some(owner) = app.runtime_transition.as_mut() {
                    pass.append(owner.poll_shutdown());
                }
                if let Some(owner) = app.runtime_maintenance.as_mut() {
                    pass.append(owner.poll_shutdown());
                }
                failed |= !pass.failures.is_empty();
                if pass.pending.is_empty() {
                    primary_owners_settled = true;
                    app.runtime_transition.take();
                    app.runtime_maintenance.take();
                    app.release_worker_session_attachment();
                    if cleanup_bootstrap && !failed {
                        match app.start_bootstrap_session_cleanup() {
                            Ok(handle) => bootstrap_handle = Some(handle),
                            Err(error) => pass.observe(
                                "bootstrap-cleanup-spawn",
                                ShutdownPoll::Failed(error.into()),
                            ),
                        }
                    }
                }
            }
            if bootstrap_handle.is_some() {
                pass.observe(
                    "bootstrap-session-cleanup",
                    match poll_join(&mut bootstrap_handle) {
                        Poll::Pending => ShutdownPoll::Pending,
                        Poll::Ready(Ok(Ok(_))) => ShutdownPoll::Complete,
                        Poll::Ready(Ok(Err(error))) | Poll::Ready(Err(error)) => {
                            ShutdownPoll::Failed(error)
                        }
                    },
                );
            }
            pass
        },
        &mut notice,
    )
}

#[cfg(test)]
#[path = "shutdown_tests.rs"]
mod tests;
