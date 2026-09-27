//! TUI platform host for the runtime attachment controller. The AppState handle owns the
//! admission thread independently of either worker, so stopping a worker never joins itself.

use super::*;
use application_bridge::{TuiApplicationBindingInputs, TuiApplicationSession, TuiRouteOperation};
use sigil_application::{
    ApplicationCommandReceipt, ApplicationCommandRequest, ApplicationError, CommandEffectBinding,
    CommandReservationKey,
};
use sigil_runtime::{
    RuntimeApplicationDispatch,
    session_runtime_controller::{RuntimeSessionController, RuntimeSessionWorkerHost},
};
use std::sync::{
    Mutex, OnceLock, Weak,
    atomic::{AtomicBool, Ordering},
    mpsc,
};

pub(crate) struct RuntimeTransitionOwner {
    application: Arc<TuiApplicationSession>,
    controller: Arc<ControllerSlot>,
    workers: Arc<Mutex<Option<WorkerRuntime>>>,
    exit: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
    result: mpsc::Receiver<RuntimeTransitionResult>,
    request: ApplicationCommandRequest,
    route: sigil_kernel::ResolvedModelRoute,
    completed: bool,
    activation_acknowledged: bool,
    intent_bound: Option<bool>,
    retired: bool,
    retired_cleanup_started: bool,
    shutdown_requested: bool,
    cleanup_failure: Arc<Mutex<Option<String>>>,
}

struct RuntimeTransitionResult {
    receipt: Result<ApplicationCommandReceipt, ApplicationError>,
    intent_bound: Option<bool>,
}

impl std::fmt::Debug for RuntimeTransitionOwner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuntimeTransitionOwner")
            .field("running", &self.is_running())
            .field("completed", &self.completed)
            .finish_non_exhaustive()
    }
}

struct WorkerHost {
    application: Weak<TuiApplicationSession>,
    workers: Arc<Mutex<Option<WorkerRuntime>>>,
    inputs: TuiApplicationBindingInputs,
    attachment:
        Arc<sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease>,
    exit: Arc<AtomicBool>,
    projection: Mutex<Option<sigil_runtime::RuntimeSessionProjectionOwner>>,
    ready_identity: Mutex<Option<sigil_kernel::SessionRuntimeReadyV1>>,
    ready_config: Mutex<Option<(RootConfig, String)>>,
    cleanup_failure: Arc<Mutex<Option<String>>>,
}

struct ControllerSlot {
    controller: OnceLock<Arc<RuntimeSessionController>>,
    inputs: TuiApplicationBindingInputs,
    attachment:
        Arc<sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease>,
    config: RootConfig,
    host: Arc<WorkerHost>,
    generation: u64,
    exit: Arc<AtomicBool>,
    cleanup_failure: Arc<Mutex<Option<String>>>,
}
impl ControllerSlot {
    fn get(&self) -> Result<Arc<RuntimeSessionController>> {
        if let Some(controller) = self.controller.get() {
            return Ok(Arc::clone(controller));
        }
        let store = sigil_runtime::application_host::guarded_session_open(
            &self.inputs.session_log_path,
            &self.inputs.cutover,
            sigil_kernel::cutover_manifest::StartupEpochV1::NewCurrentSchema,
        )?;
        let controller = Arc::new(RuntimeSessionController::new(
            store,
            Arc::clone(&self.attachment),
            self.config.clone(),
            self.host.clone(),
            self.generation,
            Arc::clone(&self.exit),
        )?);
        let _ = self.controller.set(controller);
        Ok(Arc::clone(
            self.controller
                .get()
                .expect("successful controller initialization"),
        ))
    }
}

struct RouteAdapter(Arc<ControllerSlot>);
impl TuiRouteOperation for RouteAdapter {
    fn resume_binding(
        &self,
        request: &ApplicationCommandRequest,
    ) -> Result<CommandEffectBinding, ApplicationError> {
        self.0
            .get()
            .map_err(|error| self.0.public_error(error))?
            .resume_effect_binding(request)
            .map_err(|error| self.0.public_error(error))
    }
    fn bind_effect(
        &self,
        request: &ApplicationCommandRequest,
        route: &sigil_kernel::ResolvedModelRoute,
        key: CommandReservationKey,
        fingerprint: String,
    ) -> Result<CommandEffectBinding, ApplicationError> {
        self.0
            .get()
            .map_err(|error| self.0.public_error(error))?
            .bind_effect(request, route, key, fingerprint)
            .map_err(|error| self.0.public_error(error))
    }
    fn dispatch(
        &self,
        request: &ApplicationCommandRequest,
        _route: &sigil_kernel::ResolvedModelRoute,
    ) -> Result<RuntimeApplicationDispatch, ApplicationError> {
        self.0
            .get()
            .map_err(|error| self.0.public_error(error))?
            .dispatch(request)
            .map_err(|error| self.0.public_error(error))
    }
    fn reconcile(
        &self,
        request: &ApplicationCommandRequest,
    ) -> Result<Option<RuntimeApplicationDispatch>, ApplicationError> {
        self.0
            .get()
            .map_err(|error| self.0.public_error(error))?
            .reconcile(request)
            .map_err(|error| self.0.public_error(error))
    }
}

impl ControllerSlot {
    fn public_error(&self, error: anyhow::Error) -> ApplicationError {
        if self.exit.load(Ordering::Acquire)
            && !error
                .is::<sigil_runtime::session_runtime_controller::RuntimeSessionControllerExiting>()
        {
            *self
                .cleanup_failure
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(format!("{error:#}"));
        }
        tracing::warn!(%error, "session runtime transition remains unavailable");
        ApplicationError::Unavailable
    }
}

impl RuntimeSessionWorkerHost for WorkerHost {
    fn close_gate(&self) -> Result<()> {
        self.application
            .upgrade()
            .context("application controller missing")?
            .endpoint
            .close_gate()?;
        Ok(())
    }
    fn stop(&self) -> Result<()> {
        let mut worker = self
            .workers
            .lock()
            .map_err(|_| anyhow::anyhow!("runtime worker owner poisoned"))?;
        if let Some(runtime) = worker.as_mut() {
            runtime.ready = false;
        }
        stop_owned_worker(&mut worker, &self.cleanup_failure)?;
        self.ready_identity
            .lock()
            .map_err(|_| anyhow::anyhow!("runtime Ready identity poisoned"))?
            .take();
        self.ready_config
            .lock()
            .map_err(|_| anyhow::anyhow!("runtime Ready configuration poisoned"))?
            .take();
        Ok(())
    }
    fn ready(
        &self,
        config: RootConfig,
        expected: &sigil_kernel::SessionRuntimeReadyV1,
    ) -> Result<sigil_kernel::SessionRuntimeReadyV1> {
        if self.exit.load(Ordering::Acquire) {
            return Err(
                sigil_runtime::session_runtime_controller::RuntimeSessionControllerExiting.into(),
            );
        }
        let mut workers = self
            .workers
            .lock()
            .map_err(|_| anyhow::anyhow!("runtime worker owner poisoned"))?;
        anyhow::ensure!(workers.is_none(), "previous runtime worker still owned");
        let application = self
            .application
            .upgrade()
            .context("application controller missing")?;
        let mut pending_projection = self
            .projection
            .lock()
            .map_err(|_| anyhow::anyhow!("projection transfer poisoned"))?;
        let model_ref = sigil_kernel::ModelRef::new(
            config
                .agent
                .connection
                .clone()
                .context("runtime connection missing")?,
            config.agent.model.clone(),
        )?;
        let (provider, _, _) =
            sigil_runtime::provider_connections::ResolvedRouteConfigSnapshot::from_root_config(
                &config,
            )
            .resolved_route(&model_ref)
            .context("runtime route missing")?;
        let ready_config = config.clone();
        let spawned = runner::spawn_agent_worker_with_route_directive_and_attachment(
            config,
            self.inputs.config_path.clone(),
            self.inputs.session_log_path.clone(),
            self.inputs.workspace_root.clone(),
            sigil_kernel::InteractionMode::Interactive,
            runner::WorkerSessionRouteDirective {
                runtime_ready: Some(expected.clone()),
                recovery_confirmation: None,
                explicit_selection: None,
            },
            Some(Arc::clone(&self.inputs.composition)),
            Some(Arc::clone(&self.inputs.cutover)),
            Some(Arc::clone(&self.attachment)),
        )?;
        *pending_projection = Some(spawned.projection_owner);
        drop(pending_projection);
        #[cfg(not(test))]
        let inbox = WorkerMessageInbox::forward_from(spawned.message_rx);
        #[cfg(not(test))]
        let inbox_error = inbox.as_ref().err().map(ToString::to_string);
        *workers = Some(WorkerRuntime {
            worker_tx: spawned.command_tx,
            application: Some(application),
            pending_admission: None,
            pending_interactions: Vec::new(),
            #[cfg(not(test))]
            worker_rx: inbox.unwrap_or_else(|_| WorkerMessageInbox::empty()),
            #[cfg(test)]
            worker_rx: spawned.message_rx,
            join_handle: Some(spawned.join_handle),
            ready: false,
        });
        #[cfg(not(test))]
        if let Some(error) = inbox_error {
            anyhow::bail!(error);
        }
        let runtime = workers
            .as_mut()
            .expect("worker was installed before waiting");
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if self.exit.load(Ordering::Acquire) {
                return Err(
                    sigil_runtime::session_runtime_controller::RuntimeSessionControllerExiting
                        .into(),
                );
            }
            anyhow::ensure!(
                Instant::now() < deadline,
                "runtime startup deadline exceeded"
            );
            #[cfg(test)]
            let message = runtime.worker_rx.try_recv().ok();
            #[cfg(not(test))]
            let message = runtime.worker_rx.try_recv();
            match message {
                Some(WorkerMessage::RuntimeReady(ready)) => {
                    anyhow::ensure!(
                        &ready == expected,
                        "runtime Ready belongs to another generation"
                    );
                    let application = self
                        .application
                        .upgrade()
                        .context("application controller missing")?;
                    let projection = self
                        .projection
                        .lock()
                        .map_err(|_| anyhow::anyhow!("projection transfer poisoned"))?
                        .take()
                        .context("new runtime projection missing")?;
                    application.replace_projection_owner(projection)?;
                    runtime.ready = true;
                    *self
                        .ready_identity
                        .lock()
                        .map_err(|_| anyhow::anyhow!("runtime Ready identity poisoned"))? =
                        Some(ready.clone());
                    *self
                        .ready_config
                        .lock()
                        .map_err(|_| anyhow::anyhow!("runtime Ready configuration poisoned"))? =
                        Some((ready_config, provider));
                    return Ok(ready);
                }
                Some(WorkerMessage::RunFailed(error)) => anyhow::bail!(error),
                Some(WorkerMessage::SessionRouteRecoveryRequired { code, .. }) => {
                    anyhow::bail!("runtime startup recovery required: {code:?}")
                }
                Some(_) => {} // Startup progress is advisory; only typed Ready can open the gate.
                None => {
                    anyhow::ensure!(
                        !runtime
                            .join_handle
                            .as_ref()
                            .is_some_and(std::thread::JoinHandle::is_finished),
                        "runtime stopped before Ready"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
    }
    fn open_gate(&self, ready: &sigil_kernel::SessionRuntimeReadyV1) -> Result<()> {
        anyhow::ensure!(
            !self.exit.load(Ordering::Acquire),
            "runtime host exiting before gate publication"
        );
        anyhow::ensure!(
            self.ready_identity
                .lock()
                .map_err(|_| anyhow::anyhow!("runtime Ready identity poisoned"))?
                .as_ref()
                == Some(ready),
            "historical activation has no matching live worker"
        );
        let application = self
            .application
            .upgrade()
            .context("application controller missing")?;
        if application
            .endpoint
            .is_open_generation(ready.worker_generation)?
        {
            return Ok(());
        }
        let workers = self
            .workers
            .lock()
            .map_err(|_| anyhow::anyhow!("runtime worker owner poisoned"))?;
        let runtime = workers
            .as_ref()
            .filter(|worker| worker.ready)
            .context("activated worker missing")?;
        anyhow::ensure!(
            !runtime
                .join_handle
                .as_ref()
                .is_some_and(std::thread::JoinHandle::is_finished),
            "activated worker exited"
        );
        self.application
            .upgrade()
            .context("application controller missing")?
            .endpoint
            .publish(ready.worker_generation, runtime.worker_tx.clone())?;
        Ok(())
    }
    fn is_live(&self, ready: &sigil_kernel::SessionRuntimeReadyV1) -> Result<bool> {
        if self.exit.load(Ordering::Acquire) {
            return Ok(false);
        }
        let workers = self
            .workers
            .lock()
            .map_err(|_| anyhow::anyhow!("runtime worker owner poisoned"))?;
        Ok(workers.as_ref().is_some_and(|runtime| {
            runtime.ready
                && runtime
                    .join_handle
                    .as_ref()
                    .is_some_and(|handle| !handle.is_finished())
        }) && self
            .ready_identity
            .lock()
            .map_err(|_| anyhow::anyhow!("runtime Ready identity poisoned"))?
            .as_ref()
            == Some(ready)
            && self
                .application
                .upgrade()
                .context("application controller missing")?
                .endpoint
                .is_open_generation(ready.worker_generation)?)
    }
}

impl RuntimeTransitionOwner {
    /// Retains every owner while making a replacement UI unable to consume this operation's
    /// route/config result. Cleanup joins the old admission on a separate background thread.
    pub(crate) fn invalidate_view(&mut self) {
        self.retired = true;
        self.request_exit();
        if !self.retired_cleanup_started {
            self.retired_cleanup_started =
                spawn_retired_cleanup(&mut self.handle, &self.workers, &self.cleanup_failure);
        }
    }
    pub(crate) fn application(&self) -> Option<Arc<TuiApplicationSession>> {
        (!self.retired).then(|| Arc::clone(&self.application))
    }
    pub(crate) fn observation_application(&self) -> Option<Arc<TuiApplicationSession>> {
        self.completed.then(|| self.application()).flatten()
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

    fn launch(&mut self, resume: bool) -> Result<()> {
        let application = Arc::clone(&self.application);
        let controller = Arc::clone(&self.controller);
        let request = self.request.clone();
        let (sender, receiver) = mpsc::sync_channel(1);
        self.handle = Some(
            std::thread::Builder::new()
                .name("sigil-session-controller".to_owned())
                .spawn(move || {
                    // The application service revalidates the original Intent, command generation
                    // and durable effect gate before resuming this retained operation.
                    let result = if resume {
                        futures::executor::block_on(
                            application.resume_session_runtime_transition(request.clone()),
                        )
                    } else {
                        futures::executor::block_on(application.execute_prepared(request.clone()))
                    };
                    let intent_bound = controller
                        .controller
                        .get()
                        .map_or(Some(false), |owner| owner.has_bound_intent(&request).ok());
                    // Refresh the bounded client frontier independently after the domain result. A
                    // failed activation still permits a later settings repair using the same service.
                    let _ = futures::executor::block_on(application.refresh());
                    let _ = sender.send(RuntimeTransitionResult {
                        receipt: result,
                        intent_bound,
                    });
                })
                .context("failed to start session controller")?,
        );
        self.result = receiver;
        self.completed = false;
        Ok(())
    }
}

/// Every route action enters here. The UI only freezes values and transfers the worker owner;
/// store reads, shutdown, CAS, spawn and Ready waiting all run on the controller thread.
pub(super) fn start(
    app: &mut AppState,
    worker: &mut Option<WorkerRuntime>,
    route: sigil_kernel::ResolvedModelRoute,
) -> Result<()> {
    retain_idle_application_admissions(app, worker)?;
    if app
        .runtime_maintenance
        .as_ref()
        .is_some_and(|owner| owner.is_running())
    {
        app.set_last_notice("runtime lifecycle is in progress; draft retained");
        return Ok(());
    }
    let mut retained_application = None;
    let mut retained_failure = None;
    let mut next_attempt_generation = 0;
    if let Some(mut previous) = app.runtime_transition.take() {
        if previous.is_running() {
            app.runtime_transition = Some(previous);
            app.set_last_notice("model switch is in progress; draft retained");
            return Ok(());
        }
        if previous.route == route
            && !previous.retired
            && previous.intent_bound != Some(false)
            && !previous.activation_acknowledged
        {
            if worker.is_some() {
                let mut owned = previous
                    .workers
                    .lock()
                    .map_err(|_| anyhow::anyhow!("runtime owner poisoned"))?;
                anyhow::ensure!(owned.is_none(), "runtime owner already holds a worker");
                *owned = worker.take();
            }
            let started = previous.launch(true);
            app.runtime_transition = Some(previous);
            app.mark_worker_not_ready();
            app.set_last_notice("model switch in progress; draft retained");
            return started;
        }
        // A failed cleanup still owns its old worker and cannot be replaced by another operation.
        if previous
            .workers
            .lock()
            .map_err(|_| anyhow::anyhow!("runtime owner poisoned"))?
            .is_some()
        {
            app.runtime_transition = Some(previous);
            anyhow::bail!("previous runtime cleanup must complete before another route operation");
        }
        next_attempt_generation = previous
            .controller
            .controller
            .get()
            .map_or(0, |controller| controller.next_worker_generation());
        retained_application = previous.application();
        retained_failure = Some(Arc::clone(&previous.cleanup_failure));
    }
    let application = worker
        .as_ref()
        .and_then(|runtime| runtime.application.clone())
        .or(retained_application)
        .context("route selection requires the application port")?;
    let request = application
        .prepare_action(
            &AppAction::SessionRuntimeRouteUpdated {
                route: route.clone(),
            },
            None,
            None,
        )?
        .context("route action could not be prepared")?;
    let config = app
        .persisted_config_snapshot()
        .cloned()
        .context("route selection requires saved configuration")?;
    let inputs = TuiApplicationBindingInputs::from_app(app)?;
    let attachment = app
        .worker_session_attachment()
        .context("route selection requires the original session attachment")?;
    let workers = Arc::new(Mutex::new(None));
    let exit = Arc::new(AtomicBool::new(false));
    let cleanup_failure = retained_failure.unwrap_or_default();
    let host = Arc::new(WorkerHost {
        application: Arc::downgrade(&application),
        workers: Arc::clone(&workers),
        inputs: inputs.clone(),
        attachment: Arc::clone(&attachment),
        exit: Arc::clone(&exit),
        projection: Mutex::new(None),
        ready_identity: Mutex::new(None),
        ready_config: Mutex::new(None),
        cleanup_failure: Arc::clone(&cleanup_failure),
    });
    let generation = application
        .endpoint
        .generation()?
        .checked_add(1)
        .context("worker generation exhausted")?
        .max(next_attempt_generation);
    let controller = Arc::new(ControllerSlot {
        controller: OnceLock::new(),
        inputs,
        attachment,
        config: config.clone(),
        host,
        generation,
        exit: Arc::clone(&exit),
        cleanup_failure: Arc::clone(&cleanup_failure),
    });
    application
        .endpoint
        .install_route_operation(Arc::new(RouteAdapter(Arc::clone(&controller))))?;
    let (_, result) = mpsc::channel();
    let mut owner = RuntimeTransitionOwner {
        application,
        controller,
        workers,
        exit,
        handle: None,
        result,
        request,
        route,
        completed: false,
        activation_acknowledged: false,
        intent_bound: None,
        retired: false,
        retired_cleanup_started: false,
        shutdown_requested: false,
        cleanup_failure,
    };
    *owner
        .workers
        .lock()
        .map_err(|_| anyhow::anyhow!("runtime owner poisoned"))? = worker.take();
    let started = owner.launch(false);
    app.runtime_transition = Some(owner);
    started?;
    app.mark_worker_not_ready();
    app.set_last_notice("model switch in progress; draft retained");
    Ok(())
}

pub(super) fn poll(app: &mut AppState, worker: &mut Option<WorkerRuntime>) -> Result<bool> {
    let Some(mut owner) = app.runtime_transition.take() else {
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
        app.runtime_transition = Some(owner);
        return Ok(changed);
    }
    if owner.is_running() || owner.completed {
        app.runtime_transition = Some(owner);
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
        app.runtime_transition = Some(owner);
        report_worker_unavailable(
            app,
            "session controller panicked; worker ownership retained",
        )?;
        return Ok(true);
    }
    let result = owner.result.try_recv().unwrap_or(RuntimeTransitionResult {
        receipt: Err(ApplicationError::Unavailable),
        intent_bound: None,
    });
    owner.intent_bound = result.intent_bound;
    let result = result.receipt;
    let ready_generation = owner
        .controller
        .host
        .ready_identity
        .lock()
        .map_err(|_| anyhow::anyhow!("runtime Ready identity poisoned"))?
        .as_ref()
        .map(|ready| ready.worker_generation);
    let gate_open = match ready_generation {
        Some(generation) => owner.application.endpoint.is_open_generation(generation)?,
        None => false,
    };
    let succeeded = matches!(
        result,
        Ok(ApplicationCommandReceipt::Settled(_) | ApplicationCommandReceipt::Replayed(_))
    ) && gate_open;
    if succeeded && !owner.exit.load(Ordering::Acquire) {
        let mut runtime = owner
            .workers
            .lock()
            .map_err(|_| anyhow::anyhow!("runtime owner poisoned"))?;
        if runtime.as_ref().is_some_and(|runtime| {
            runtime.ready
                && runtime
                    .join_handle
                    .as_ref()
                    .is_some_and(|handle| !handle.is_finished())
        }) {
            let (config, provider) = owner
                .controller
                .host
                .ready_config
                .lock()
                .map_err(|_| anyhow::anyhow!("runtime Ready configuration poisoned"))?
                .take()
                .context("activated runtime configuration missing")?;
            app.apply_activated_session_route(config, provider, owner.route.clone());
            *worker = runtime.take();
            owner.activation_acknowledged = true;
            app.handle_worker_message(WorkerMessage::WorkerReady)?;
        } else {
            app.mark_worker_not_ready();
            app.set_last_notice("activated worker exited; retry the selected model");
        }
    } else {
        // Preflight failures leave the original ready endpoint usable. Once stop begins the
        // retained handle stays with this operation until retry confirms cleanup.
        let mut runtime = owner
            .workers
            .lock()
            .map_err(|_| anyhow::anyhow!("runtime owner poisoned"))?;
        if runtime.as_ref().is_some_and(|runtime| {
            runtime
                .join_handle
                .as_ref()
                .is_none_or(|handle| !handle.is_finished())
        }) {
            // Gate state, rather than model equality, distinguishes untouched preflight rejection.
            if owner.application.endpoint.is_open()? {
                *worker = runtime.take();
                if worker.as_ref().is_some_and(|runtime| runtime.ready) {
                    app.handle_worker_message(WorkerMessage::WorkerReady)?;
                }
            }
        }
        app.set_last_notice("model switch incomplete; retry the selected model");
        if worker.is_none() {
            app.mark_worker_not_ready();
        }
    }
    if let Ok(receipt) = result
        && (owner.activation_acknowledged
            || !matches!(
                receipt,
                ApplicationCommandReceipt::Settled(_) | ApplicationCommandReceipt::Replayed(_)
            ))
    {
        report_application_receipt(app, &receipt)?;
    }
    owner.completed = true;
    app.runtime_transition = Some(owner);
    Ok(true)
}

fn stop_owned_worker(
    worker: &mut Option<WorkerRuntime>,
    failure: &Arc<Mutex<Option<String>>>,
) -> Result<()> {
    let result = shutdown::shutdown_worker_to_completion(worker);
    if let Err(error) = &result {
        let mut failure = failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let message = format!("{error:#}");
        *failure = Some(failure.take().map_or_else(
            || message.clone(),
            |previous| format!("{previous}; {message}"),
        ));
    }
    result
}

fn cleanup_retained_worker(workers: &Mutex<Option<WorkerRuntime>>) -> Result<()> {
    let mut worker = workers
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    shutdown::shutdown_worker_to_completion(&mut worker)
}

fn spawn_retired_cleanup(
    handle: &mut Option<std::thread::JoinHandle<()>>,
    workers: &Arc<Mutex<Option<WorkerRuntime>>>,
    failure: &Arc<Mutex<Option<String>>>,
) -> bool {
    // A failed spawn must return the original JoinHandle, never drop its unique owner.
    let original = Arc::new(Mutex::new(handle.take()));
    let retained = Arc::clone(&original);
    let workers = Arc::clone(workers);
    let failure = Arc::clone(failure);
    match std::thread::Builder::new()
        .name("sigil-retired-runtime".to_owned())
        .spawn(move || {
            if let Some(previous) = retained
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
                && previous.join().is_err()
            {
                *failure
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                    Some("retired runtime lifecycle panicked".to_owned());
            }
            if let Err(error) = cleanup_retained_worker(&workers) {
                let mut failure = failure
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let message = format!("{error:#}");
                *failure = Some(failure.take().map_or_else(
                    || message.clone(),
                    |previous| format!("{previous}; {message}"),
                ));
            }
        }) {
        Ok(cleanup) => {
            *handle = Some(cleanup);
            true
        }
        Err(error) => {
            *handle = original
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            tracing::error!(%error, "retired runtime cleanup thread unavailable; owner retained");
            false
        }
    }
}

impl Drop for RuntimeTransitionOwner {
    fn drop(&mut self) {
        self.request_exit();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        // Drop is the final ownership boundary, used after terminal restoration on exit. Never
        // detach a worker just because the interactive shutdown budget has elapsed.
        if let Err(error) = cleanup_retained_worker(&self.workers) {
            tracing::error!(%error, "runtime owner fallback cleanup failed");
        }
    }
}

#[path = "runtime_maintenance.rs"]
mod maintenance;
pub(crate) use maintenance::RuntimeMaintenanceOwner;
pub(super) use maintenance::{maintain, poll_maintenance};

#[cfg(test)]
#[path = "runtime_transition_tests.rs"]
mod tests;
