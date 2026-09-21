//! Background ownership for session rebind and worker cleanup.

use super::*;

/// Worker-only lifecycle actions share the same background ownership boundary. Session rebind
/// intentionally creates a new application identity; a route switch above preserves its identity.
pub(crate) struct RuntimeMaintenanceOwner {
    pub(super) workers: Arc<Mutex<Option<WorkerRuntime>>>,
    exit: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
    result: mpsc::Receiver<Result<()>>,
    completed: bool,
    retired: bool,
    retired_cleanup_started: bool,
    shutdown_requested: bool,
    cleanup_failure: Arc<Mutex<Option<String>>>,
}

impl std::fmt::Debug for RuntimeMaintenanceOwner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuntimeMaintenanceOwner")
            .field("running", &self.is_running())
            .field("completed", &self.completed)
            .finish_non_exhaustive()
    }
}

impl RuntimeMaintenanceOwner {
    pub(crate) fn invalidate_view(&mut self) {
        self.retired = true;
        self.request_exit();
        if !self.retired_cleanup_started {
            self.retired_cleanup_started =
                spawn_retired_cleanup(&mut self.handle, &self.workers, &self.cleanup_failure);
        }
    }
    pub(crate) fn is_running(&self) -> bool {
        self.handle
            .as_ref()
            .is_some_and(|handle| !handle.is_finished())
    }
    pub(crate) fn request_exit(&self) {
        self.exit.store(true, Ordering::Release);
    }

    pub(crate) fn poll_shutdown(&mut self) -> ShutdownPass {
        self.request_exit();
        let mut pass = ShutdownPass::default();
        pass.observe("runtime-lifecycle", poll_owned_thread(&mut self.handle));
        if self.handle.is_some() {
            return pass;
        }
        if let Some(error) = self
            .cleanup_failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            pass.observe(
                "retired-runtime-cleanup",
                ShutdownPoll::Failed(anyhow::anyhow!(error)),
            );
        }
        if !self.completed {
            self.completed = true;
            match self.result.try_recv() {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    // Actual stop failures were captured at their source. Provider/rebind
                    // failures remain operation results rather than shutdown failures.
                    tracing::warn!(%error, "runtime maintenance operation ended before exit");
                }
                Err(_) => pass.observe(
                    "runtime-maintenance",
                    ShutdownPoll::Failed(anyhow::anyhow!("runtime maintenance result unavailable")),
                ),
            }
        }
        let mut workers = self
            .workers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !self.shutdown_requested {
            shutdown::request_worker_shutdown(&workers);
            self.shutdown_requested = true;
        }
        pass.append(shutdown::poll_worker_shutdown(&mut workers));
        pass
    }
}

pub(in crate::launcher) fn maintain(
    app: &mut AppState,
    worker: &mut Option<WorkerRuntime>,
    config: Option<RootConfig>,
) -> Result<()> {
    if app
        .runtime_transition
        .as_ref()
        .is_some_and(|owner| owner.is_running())
    {
        app.set_last_notice("session transition is in progress; draft retained");
        return Ok(());
    }
    if app.runtime_transition.as_ref().is_some_and(|owner| {
        owner
            .workers
            .lock()
            .map_or(true, |workers| workers.is_some())
    }) {
        app.set_last_notice("retry the pending model switch to finish owned cleanup");
        return Ok(());
    }
    if app
        .runtime_maintenance
        .as_ref()
        .is_some_and(|owner| owner.is_running())
    {
        app.set_last_notice("runtime cleanup is in progress; draft retained");
        return Ok(());
    }
    if let Some(previous) = app.runtime_maintenance.as_ref() {
        let retained = previous
            .workers
            .lock()
            .map_err(|_| anyhow::anyhow!("retained runtime owner poisoned"))?;
        anyhow::ensure!(
            retained.is_none() || worker.is_none(),
            "cannot replace concurrent runtime owners"
        );
    }
    let inputs = config
        .as_ref()
        .map(|_| TuiApplicationBindingInputs::from_app(app))
        .transpose()?;
    let directive = runner::WorkerSessionRouteDirective {
        runtime_ready: None,
        recovery_confirmation: app
            .pending_session_route_confirmation_binding()
            .map(str::to_owned),
        explicit_selection: app.pending_session_route_selection().cloned(),
    };
    let attachment = app.worker_session_attachment();
    let reasoning = app.runtime.reasoning_effort.clone();
    let cleanup_failure = app
        .runtime_maintenance
        .as_ref()
        .map(|owner| Arc::clone(&owner.cleanup_failure))
        .unwrap_or_default();
    let task_failure = Arc::clone(&cleanup_failure);
    let workers = Arc::new(Mutex::new(None));
    let owned_workers = Arc::clone(&workers);
    let exit = Arc::new(AtomicBool::new(false));
    let exiting = Arc::clone(&exit);
    let (sender, result) = mpsc::sync_channel(1);
    let (begin, begin_rx) = mpsc::sync_channel(1);
    let handle = std::thread::Builder::new()
        .name("sigil-runtime-lifecycle".to_owned())
        .spawn(move || {
            if begin_rx.recv().is_err() {
                return;
            }
            let result = (|| {
                let mut workers = owned_workers
                    .lock()
                    .map_err(|_| anyhow::anyhow!("runtime owner poisoned"))?;
                stop_owned_worker(&mut workers, &task_failure)?;
                if let Some((config, inputs)) = config.zip(inputs) {
                    if exiting.load(Ordering::Acquire) {
                        return Ok(());
                    }
                    let spawned = runner::spawn_agent_worker_with_route_directive_and_attachment(
                        config,
                        inputs.config_path.clone(),
                        inputs.session_log_path.clone(),
                        inputs.workspace_root.clone(),
                        sigil_kernel::InteractionMode::Interactive,
                        directive,
                        Some(Arc::clone(&inputs.composition)),
                        Some(Arc::clone(&inputs.cutover)),
                        attachment,
                    )?;
                    #[cfg(not(test))]
                    let inbox = WorkerMessageInbox::forward_from(spawned.message_rx);
                    #[cfg(not(test))]
                    let inbox_error = inbox.as_ref().err().map(ToString::to_string);
                    *workers = Some(WorkerRuntime {
                        worker_tx: spawned.command_tx,
                        application: None,
                        pending_admission: None,
                        pending_interactions: Vec::new(),
                        #[cfg(not(test))]
                        worker_rx: inbox.unwrap_or_else(|_| WorkerMessageInbox::empty()),
                        #[cfg(test)]
                        worker_rx: spawned.message_rx,
                        join_handle: Some(spawned.join_handle),
                        ready: false,
                    });
                    let attached: Result<()> = (|| {
                        #[cfg(not(test))]
                        if let Some(error) = inbox_error {
                            anyhow::bail!(error);
                        }
                        let runtime = workers
                            .as_mut()
                            .expect("worker installed before application wiring");
                        runtime.application =
                            Some(Arc::new(application_bridge::build_from_inputs(
                                inputs,
                                runtime.worker_tx.clone(),
                                reasoning,
                                spawned.projection_owner,
                            )?));
                        if exiting.load(Ordering::Acquire) {
                            stop_owned_worker(&mut workers, &task_failure)?;
                        }
                        Ok(())
                    })();
                    if attached.is_err() {
                        stop_owned_worker(&mut workers, &task_failure)?;
                    }
                    attached?;
                }
                Ok(())
            })();
            let _ = sender.send(result);
        })?;
    let retained = app.runtime_maintenance.as_mut().and_then(|previous| {
        previous
            .workers
            .lock()
            .expect("completed owner checked before thread creation")
            .take()
    });
    *workers
        .lock()
        .map_err(|_| anyhow::anyhow!("runtime owner poisoned"))? =
        retained.or_else(|| worker.take());
    app.runtime_maintenance = Some(RuntimeMaintenanceOwner {
        workers,
        exit,
        handle: Some(handle),
        result,
        completed: false,
        retired: false,
        retired_cleanup_started: false,
        shutdown_requested: false,
        cleanup_failure,
    });
    let _ = begin.send(());
    app.mark_worker_not_ready();
    Ok(())
}

pub(in crate::launcher) fn poll_maintenance(
    app: &mut AppState,
    worker: &mut Option<WorkerRuntime>,
) -> Result<bool> {
    let Some(mut owner) = app.runtime_maintenance.take() else {
        return Ok(false);
    };
    if owner.retired {
        owner.invalidate_view();
        let finished = owner.retired_cleanup_started && !owner.is_running();
        let changed = finished && !owner.completed;
        if finished {
            if let Some(handle) = owner.handle.take()
                && handle.join().is_err()
            {
                *owner
                    .cleanup_failure
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                    Some("retired runtime owner panicked".to_owned());
            }
            owner.completed = true;
        }
        app.runtime_maintenance = Some(owner);
        return Ok(changed);
    }
    if owner.is_running() || owner.completed {
        app.runtime_maintenance = Some(owner);
        return Ok(false);
    }
    if let Some(handle) = owner.handle.take()
        && handle.join().is_err()
    {
        *owner
            .cleanup_failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some("runtime lifecycle owner panicked".to_owned());
        owner.completed = true;
        app.runtime_maintenance = Some(owner);
        report_worker_unavailable(app, "runtime lifecycle panicked; worker ownership retained")?;
        return Ok(true);
    }
    match owner
        .result
        .try_recv()
        .unwrap_or_else(|_| Err(anyhow::anyhow!("runtime lifecycle result unavailable")))
    {
        Ok(()) if !owner.exit.load(Ordering::Acquire) => {
            *worker = owner
                .workers
                .lock()
                .map_err(|_| anyhow::anyhow!("runtime owner poisoned"))?
                .take();
        }
        Ok(()) => {}
        Err(error) => {
            report_worker_unavailable(app, &format!("runtime lifecycle incomplete: {error:#}"))?;
        }
    }
    owner.completed = true;
    app.runtime_maintenance = Some(owner);
    Ok(true)
}

impl Drop for RuntimeMaintenanceOwner {
    fn drop(&mut self) {
        self.request_exit();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        if let Err(error) = cleanup_retained_worker(&self.workers) {
            tracing::error!(%error, "runtime owner fallback cleanup failed");
        }
    }
}
