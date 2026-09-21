use super::*;

#[derive(Debug)]
pub(in crate::runner) enum OwnedTaskDrainFailure {
    DeadlineExceeded { pending_tasks: usize },
    TaskPanicked,
}

impl std::fmt::Display for OwnedTaskDrainFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DeadlineExceeded { pending_tasks } => write!(
                formatter,
                "shutdown still pending; pending_tasks={pending_tasks}"
            ),
            Self::TaskPanicked => {
                formatter.write_str("owned task panicked during shutdown; cleanup_complete=false")
            }
        }
    }
}

/// A finished task still owns a join result. Reap without blocking, and keep panic evidence
/// on the manager even after the handle itself has been consumed.
pub(in crate::runner) fn reap_finished_owned_tasks(
    handles: &mut Vec<tokio::task::JoinHandle<()>>,
    task_panicked: &mut bool,
) {
    handles.retain_mut(|handle| {
        if !handle.is_finished() {
            return true;
        }
        match futures::FutureExt::now_or_never(handle) {
            Some(result) => {
                *task_panicked |= result.is_err_and(|error| error.is_panic());
                false
            }
            None => true,
        }
    });
}

/// Abort only requests cancellation. Handles remain in the owner until their join completes.
pub(in crate::runner) fn drain_owned_tasks_until(
    handles: &mut Vec<tokio::task::JoinHandle<()>>,
    task_panicked: &mut bool,
    runtime: &tokio::runtime::Runtime,
    deadline: Instant,
) -> Result<(), OwnedTaskDrainFailure> {
    for handle in handles.iter() {
        handle.abort();
    }
    reap_finished_owned_tasks(handles, task_panicked);
    while let Some(handle) = handles.last_mut() {
        match runtime.block_on(async {
            tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), handle).await
        }) {
            Ok(result) => {
                *task_panicked |= result.is_err_and(|error| error.is_panic());
                handles.pop();
            }
            Err(_) => {
                return Err(OwnedTaskDrainFailure::DeadlineExceeded {
                    pending_tasks: handles.len(),
                });
            }
        }
    }
    if *task_panicked {
        Err(OwnedTaskDrainFailure::TaskPanicked)
    } else {
        Ok(())
    }
}

/// Request all independent owners before any join. This is also called while a foreground
/// cancellation is pending if the UI sets closing, so Shutdown need not wait in the inbox.
pub(super) fn request_independent_worker_stops(state: &mut WorkerLoopState) {
    if let Some(terminal_control) = &state.terminal_control {
        terminal_control.request_stop_all();
    }
    state.refresh.provider_status_tasks.abort_all();
    state.compaction.preparation_tasks.abort_all();
    state.artifact_gc.tasks.abort_all();
    cancel_all_mcp_oauth_flows(state);
    state.session_maintenance.request_stop();
    // Retired run roots have already reached their natural terminal or received cooperative
    // cancellation. Aborting them here could interrupt the cleanup they still own.
}

fn join_owned_tasks(
    handles: &mut Vec<tokio::task::JoinHandle<()>>,
    task_panicked: &mut bool,
    runtime: &tokio::runtime::Runtime,
) -> Result<(), OwnedTaskDrainFailure> {
    while let Some(handle) = handles.last_mut() {
        *task_panicked |= runtime
            .block_on(handle)
            .is_err_and(|error| error.is_panic());
        handles.pop();
    }
    if *task_panicked {
        Err(OwnedTaskDrainFailure::TaskPanicked)
    } else {
        Ok(())
    }
}

/// Bounded manager polls retain their handles. A poll deadline is progress only; keep the same
/// owner and retry until every join is consumed, preserving a previously observed panic.
fn drain_stopping_owner(
    stage: WorkerShutdownStage,
    stop_control: &super::super::protocol::WorkerStopControl,
    message_tx: &mpsc::Sender<WorkerMessage>,
    mut drain: impl FnMut(Instant) -> Result<(), OwnedTaskDrainFailure>,
) {
    stop_control.stage(stage);
    loop {
        match drain(Instant::now() + Duration::from_secs(1)) {
            Ok(()) => return,
            Err(OwnedTaskDrainFailure::DeadlineExceeded { .. }) => continue,
            Err(error @ OwnedTaskDrainFailure::TaskPanicked) => {
                stop_control.fail_stage(stage);
                let _ = message_tx.send(WorkerMessage::Notice(error.to_string()));
                return;
            }
        }
    }
}

pub(super) fn shutdown_worker_state(
    state: &mut WorkerLoopState,
    runtime: &tokio::runtime::Runtime,
    root_config: &RootConfig,
    message_tx: &mpsc::Sender<WorkerMessage>,
    elicitation_handler: &Arc<ChannelMcpElicitationHandler>,
) {
    state.stop_control.reserve(true);
    request_independent_worker_stops(state);

    if let Some(active_run) = state.run.active.take() {
        cancel_active_run(
            active_run,
            runtime,
            root_config,
            state,
            message_tx,
            elicitation_handler,
            ActiveRunStopDisposition::Cancel,
            "run interrupted by TUI shutdown",
        );
    }

    let stop_control = state.stop_control.clone();
    stop_control.stage(WorkerShutdownStage::TerminalTasks);
    if let Some(terminal_control) = &state.terminal_control
        && let Err(error) = runtime.block_on(terminal_control.shutdown_owned())
    {
        stop_control.fail_stage(WorkerShutdownStage::TerminalTasks);
        let _ = message_tx.send(WorkerMessage::Notice(format!(
            "terminal execution shutdown failed: {error:#}"
        )));
    }
    drain_stopping_owner(
        WorkerShutdownStage::RunQuiescence,
        &stop_control,
        message_tx,
        |_| {
            join_owned_tasks(
                &mut state.run.retired,
                &mut state.run.task_panicked,
                runtime,
            )
        },
    );
    drain_stopping_owner(
        WorkerShutdownStage::Compaction,
        &stop_control,
        message_tx,
        |deadline| {
            state
                .compaction
                .preparation_tasks
                .shutdown_until(runtime, deadline)
        },
    );
    drain_stopping_owner(
        WorkerShutdownStage::ArtifactGc,
        &stop_control,
        message_tx,
        |deadline| state.artifact_gc.tasks.shutdown_until(runtime, deadline),
    );
    drain_stopping_owner(
        WorkerShutdownStage::McpOAuth,
        &stop_control,
        message_tx,
        |_| {
            join_owned_tasks(
                &mut state.mcp_oauth.retired,
                &mut state.mcp_oauth.task_panicked,
                runtime,
            )
        },
    );
    drain_stopping_owner(
        WorkerShutdownStage::ProviderStatus,
        &stop_control,
        message_tx,
        |deadline| {
            runtime.block_on(state.refresh.provider_status_tasks.shutdown_until(deadline))
            .map_err(|error| match error {
                sigil_runtime::provider_status::ProviderStatusShutdownError::DeadlineExceeded { pending_tasks } =>
                    OwnedTaskDrainFailure::DeadlineExceeded { pending_tasks },
                sigil_runtime::provider_status::ProviderStatusShutdownError::TaskPanicked => OwnedTaskDrainFailure::TaskPanicked,
            })
        },
    );
    drain_stopping_owner(
        WorkerShutdownStage::SessionMaintenance,
        &stop_control,
        message_tx,
        |deadline| state.session_maintenance.shutdown_until(runtime, deadline),
    );
    stop_control.stage(WorkerShutdownStage::Runtime);
    // Runtime destruction remains on the explicitly joined worker. The launcher keeps waiting
    // and observing this phase until the real thread finishes, even beyond its slow threshold.
}

#[cfg(test)]
#[path = "tests/shutdown_tests.rs"]
mod tests;
