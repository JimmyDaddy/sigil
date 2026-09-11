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
                "shutdown deadline exceeded; pending_tasks={pending_tasks}; cleanup_complete=false"
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

pub(super) fn shutdown_worker_state(
    state: &mut WorkerLoopState,
    runtime: &tokio::runtime::Runtime,
    root_config: &RootConfig,
    message_tx: &mpsc::Sender<WorkerMessage>,
    elicitation_handler: &Arc<ChannelMcpElicitationHandler>,
) {
    state.stop_control.reserve(true);
    let deadline = state
        .stop_control
        .shutdown_deadline()
        .expect("closing worker has a deadline");
    // Request every independent owner to stop before spending the shared budget on any join.
    state.refresh.provider_status_tasks.abort_all();
    state.compaction.preparation_tasks.abort_all();
    state.artifact_gc.tasks.abort_all();
    cancel_all_mcp_oauth_flows(state);
    for task in &state.run.retired {
        task.abort();
    }
    state.session_maintenance.request_stop();

    if let Some(active_run) = state.run.active.take() {
        cancel_active_run(
            active_run,
            runtime,
            root_config,
            &state.session.log_path,
            &mut state.session.current,
            &mut state.session.detached_durable_controls,
            message_tx,
            elicitation_handler,
            state.agent.supervisor.as_ref(),
            &mut state.run.discarded_ids,
            &mut state.run.retired,
            &state.stop_control,
            ActiveRunStopDisposition::Cancel,
            "run interrupted by TUI shutdown",
        );
    }

    let stop_control = state.stop_control.clone();
    let drain = |stage, result: Result<(), String>| {
        if let Err(error) = result {
            stop_control.fail_stage(stage);
            let _ = message_tx.send(WorkerMessage::Notice(error));
        }
    };
    stop_control.stage(WorkerShutdownStage::RunQuiescence);
    drain(
        WorkerShutdownStage::RunQuiescence,
        drain_owned_tasks_until(
            &mut state.run.retired,
            &mut state.run.task_panicked,
            runtime,
            deadline,
        )
        .map_err(|error| error.to_string()),
    );
    stop_control.stage(WorkerShutdownStage::Compaction);
    drain(
        WorkerShutdownStage::Compaction,
        state
            .compaction
            .preparation_tasks
            .shutdown_until(runtime, deadline)
            .map_err(|error| error.to_string()),
    );
    stop_control.stage(WorkerShutdownStage::ArtifactGc);
    drain(
        WorkerShutdownStage::ArtifactGc,
        state
            .artifact_gc
            .tasks
            .shutdown_until(runtime, deadline)
            .map_err(|error| error.to_string()),
    );
    stop_control.stage(WorkerShutdownStage::McpOAuth);
    drain(
        WorkerShutdownStage::McpOAuth,
        drain_owned_tasks_until(
            &mut state.mcp_oauth.retired,
            &mut state.mcp_oauth.task_panicked,
            runtime,
            deadline,
        )
        .map_err(|error| error.to_string()),
    );
    stop_control.stage(WorkerShutdownStage::ProviderStatus);
    drain(
        WorkerShutdownStage::ProviderStatus,
        runtime
            .block_on(state.refresh.provider_status_tasks.shutdown_until(deadline))
            .map_err(|error| error.to_string()),
    );
    stop_control.stage(WorkerShutdownStage::SessionMaintenance);
    drain(
        WorkerShutdownStage::SessionMaintenance,
        state
            .session_maintenance
            .shutdown_until(runtime, deadline)
            .map_err(|error| error.to_string()),
    );
    stop_control.stage(WorkerShutdownStage::Runtime);
    // Runtime destruction remains owned by the worker thread. A blocking task still executing
    // here keeps that thread unfinished, so the launcher reports uncertainty at its deadline.
}

#[cfg(test)]
#[path = "tests/shutdown_tests.rs"]
mod tests;
