use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, OnceLock, Weak, mpsc as std_mpsc},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use sha2::{Digest, Sha256};
use sigil_kernel::{
    ApprovalHandler, NetworkEffect, PublicRunEvent, PublicRunEventKind, SessionRef, ToolAccess,
    ToolApproval, ToolApprovalUserDecision, ToolCall, ToolSpec,
};
use sigil_runtime::application_run::{
    ApplicationRunControl, ApplicationRunEventHandler, ApplicationRunInteraction,
    ApplicationRunOutput, ApplicationRunRequest, ApplicationRunServices,
    ApplicationRunTerminalStatus, PreparedApplicationRun, application_verification_view,
    bind_application_session, bind_existing_application_session, prepare_application_run,
    record_application_preparation_cancellation, rerun_application_verification,
};
use sigil_runtime::{LocalSessionLifecycleService, LocalSessionReopenError};
use tokio::{runtime::Handle, sync::mpsc};

use crate::{
    HTTP_APPROVAL_POLICY_VERSION, HttpApprovalDecisionRecord, HttpDurableCommandStore,
    HttpDurableEgressDisclosureJournal, HttpDurableEgressDisclosurePresenter, HttpLiveEventBus,
    HttpPendingApproval, HttpRunApprovalMode, HttpRunDriver, HttpRunDriverApproval,
    HttpRunDriverCancel, HttpRunDriverError, HttpRunDriverStart, HttpRunTerminalOutcome,
    HttpSessionBinding, HttpSessionOpenBindingError, HttpSessionRunRegistry,
    HttpVerificationRerunRequest, HttpVerificationView,
};

const DEFAULT_HTTP_APPROVAL_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const DEFAULT_HTTP_CANCELLATION_TIMEOUT: Duration = Duration::from_secs(5);

/// Runtime inputs and bounded waits owned by the production HTTP driver.
#[derive(Debug, Clone)]
pub struct HttpProductionRunDriverOptions {
    /// Resolved Sigil configuration path.
    pub config_path: PathBuf,
    /// Process launch working directory used for workspace resolution.
    pub launch_cwd: PathBuf,
    /// Maximum time an externally interactive approval may remain pending.
    pub approval_timeout: Duration,
    /// Maximum time allowed for cooperative cancellation quiescence.
    pub cancellation_timeout: Duration,
    /// Workspace-bound lifecycle truth used to authorize historical session reopen.
    pub session_lifecycle: Option<LocalSessionLifecycleService>,
}

impl HttpProductionRunDriverOptions {
    /// Creates production defaults for one config/workspace pair.
    #[must_use]
    pub fn new(config_path: impl Into<PathBuf>, launch_cwd: impl Into<PathBuf>) -> Self {
        Self {
            config_path: config_path.into(),
            launch_cwd: launch_cwd.into(),
            approval_timeout: DEFAULT_HTTP_APPROVAL_TIMEOUT,
            cancellation_timeout: DEFAULT_HTTP_CANCELLATION_TIMEOUT,
            session_lifecycle: None,
        }
    }

    /// Attaches workspace-bound lifecycle truth for durable session reopen.
    #[must_use]
    pub fn with_session_lifecycle(
        mut self,
        session_lifecycle: LocalSessionLifecycleService,
    ) -> Self {
        self.session_lifecycle = Some(session_lifecycle);
        self
    }
}

#[async_trait]
trait HttpApplicationRunPreparer: Send + Sync {
    async fn prepare(
        &self,
        request: ApplicationRunRequest,
        services: ApplicationRunServices,
    ) -> Result<PreparedApplicationRun>;
}

struct HttpSharedApplicationRunPreparer;

#[async_trait]
impl HttpApplicationRunPreparer for HttpSharedApplicationRunPreparer {
    async fn prepare(
        &self,
        request: ApplicationRunRequest,
        services: ApplicationRunServices,
    ) -> Result<PreparedApplicationRun> {
        prepare_application_run(request, &services)
            .await
            .map_err(anyhow::Error::new)
    }
}

/// Production run driver backed by the shared runtime application service.
pub struct HttpProductionRunDriver {
    options: HttpProductionRunDriverOptions,
    services: ApplicationRunServices,
    preparer: Arc<dyn HttpApplicationRunPreparer>,
    event_bus: Arc<HttpLiveEventBus>,
    runtime: Handle,
    registry: OnceLock<Weak<HttpSessionRunRegistry>>,
    active_runs: Arc<Mutex<BTreeMap<String, Arc<HttpProductionActiveRun>>>>,
    active_runs_ready: Arc<Condvar>,
}

impl std::fmt::Debug for HttpProductionRunDriver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HttpProductionRunDriver")
            .field("options", &self.options)
            .field("services", &self.services)
            .field("preparer", &"configured")
            .field("event_bus", &"configured")
            .finish_non_exhaustive()
    }
}

impl HttpProductionRunDriver {
    /// Creates a production driver. Call `build_registry` before starting runs.
    ///
    /// # Errors
    ///
    /// Returns an error when the event bus has no durable protocol journal.
    pub fn new(
        options: HttpProductionRunDriverOptions,
        disclosure_journal: Arc<HttpDurableEgressDisclosureJournal>,
        event_bus: Arc<HttpLiveEventBus>,
        runtime: Handle,
    ) -> Result<Self, HttpRunDriverError> {
        Self::new_with_preparer(
            options,
            disclosure_journal,
            event_bus,
            runtime,
            Arc::new(HttpSharedApplicationRunPreparer),
        )
    }

    fn new_with_preparer(
        options: HttpProductionRunDriverOptions,
        disclosure_journal: Arc<HttpDurableEgressDisclosureJournal>,
        event_bus: Arc<HttpLiveEventBus>,
        runtime: Handle,
        preparer: Arc<dyn HttpApplicationRunPreparer>,
    ) -> Result<Self, HttpRunDriverError> {
        if !event_bus.has_durable_journal() {
            return Err(HttpRunDriverError::new(
                "production driver requires a durable protocol journal",
            ));
        }
        let services = ApplicationRunServices::new(Arc::new(
            HttpDurableEgressDisclosurePresenter::new(disclosure_journal),
        ));
        Ok(Self {
            options,
            services,
            preparer,
            event_bus,
            runtime,
            registry: OnceLock::new(),
            active_runs: Arc::new(Mutex::new(BTreeMap::new())),
            active_runs_ready: Arc::new(Condvar::new()),
        })
    }

    /// Builds and attaches the one process-local registry driven by this instance.
    ///
    /// # Errors
    ///
    /// Returns an error when the driver was already attached to another registry.
    pub fn build_registry(
        self: &Arc<Self>,
        command_store: Arc<HttpDurableCommandStore>,
    ) -> Result<Arc<HttpSessionRunRegistry>, HttpRunDriverError> {
        let driver: Arc<dyn HttpRunDriver> = self.clone();
        let registry = Arc::new(HttpSessionRunRegistry::with_durable_command_store(
            driver,
            command_store,
        ));
        self.registry
            .set(Arc::downgrade(&registry))
            .map_err(|_| HttpRunDriverError::new("production driver registry already attached"))?;
        Ok(registry)
    }

    /// Returns the number of owned run supervisors that have not completed cleanup.
    ///
    /// # Errors
    ///
    /// Returns an error when the active-run state is unavailable.
    pub fn active_run_count(&self) -> Result<usize, HttpRunDriverError> {
        self.active_runs
            .lock()
            .map(|runs| runs.len())
            .map_err(|_| HttpRunDriverError::new("production active-run state unavailable"))
    }

    fn attached_registry(&self) -> Result<Arc<HttpSessionRunRegistry>, HttpRunDriverError> {
        self.registry
            .get()
            .and_then(Weak::upgrade)
            .ok_or_else(|| HttpRunDriverError::new("production driver registry is not attached"))
    }
}

impl HttpRunDriver for HttpProductionRunDriver {
    fn bind_session(&self, session_id: &str) -> Result<HttpSessionBinding, HttpRunDriverError> {
        let binding =
            bind_application_session(&self.options.config_path, &self.options.launch_cwd, None)
                .map_err(|error| {
                    HttpRunDriverError::new(format!(
                        "failed to bind durable session for {session_id}: {error}"
                    ))
                })?;
        Ok(HttpSessionBinding {
            session_scope_id: binding.session_scope_id,
            session_log_path: binding.session_log_path.display().to_string(),
        })
    }

    fn bind_existing_session(
        &self,
        session_ref: &SessionRef,
        expected_session_id: &str,
    ) -> Result<HttpSessionBinding, HttpSessionOpenBindingError> {
        let lifecycle = self
            .options
            .session_lifecycle
            .as_ref()
            .ok_or(HttpSessionOpenBindingError::Unavailable)?;
        let candidate = lifecycle
            .resolve_session_for_reopen(session_ref, expected_session_id)
            .map_err(|error| match error {
                LocalSessionReopenError::NotFound => HttpSessionOpenBindingError::NotFound,
                LocalSessionReopenError::NotReady { .. } => HttpSessionOpenBindingError::NotReady,
                LocalSessionReopenError::IdentityChanged => {
                    HttpSessionOpenBindingError::IdentityChanged
                }
                LocalSessionReopenError::CatalogUnavailable { .. } => {
                    HttpSessionOpenBindingError::Unavailable
                }
            })?;
        let binding = bind_existing_application_session(
            &self.options.config_path,
            &candidate.session_log_path,
        )
        .map_err(|_| HttpSessionOpenBindingError::Unavailable)?;
        if binding.session_scope_id != candidate.session_id
            || binding.session_scope_id != expected_session_id
            || binding.session_log_path != candidate.session_log_path
        {
            return Err(HttpSessionOpenBindingError::IdentityChanged);
        }
        Ok(HttpSessionBinding {
            session_scope_id: binding.session_scope_id,
            session_log_path: binding.session_log_path.display().to_string(),
        })
    }

    fn start_run(&self, start: HttpRunDriverStart) -> Result<(), HttpRunDriverError> {
        let registry = self.attached_registry()?;
        let broker = Arc::new(HttpApprovalBroker::default());
        let (cancel_sender, cancel_receiver) = mpsc::unbounded_channel();
        let active = Arc::new(HttpProductionActiveRun {
            session_id: start.session.id.clone(),
            broker: Arc::clone(&broker),
            cancel_sender,
        });
        {
            let mut runs = self
                .active_runs
                .lock()
                .map_err(|_| HttpRunDriverError::new("production active-run state unavailable"))?;
            if runs.contains_key(&start.run.id) {
                return Err(HttpRunDriverError::new(format!(
                    "production run already active: {}",
                    start.run.id
                )));
            }
            runs.insert(start.run.id.clone(), active);
        }

        let supervisor = HttpRunSupervisor {
            options: self.options.clone(),
            services: self.services.clone(),
            preparer: Arc::clone(&self.preparer),
            event_bus: Arc::clone(&self.event_bus),
            registry: Arc::downgrade(&registry),
            broker: Arc::clone(&broker),
            start: start.clone(),
            cancel_receiver,
        };
        let task = self.runtime.spawn(supervisor.run());
        let active_runs = Arc::clone(&self.active_runs);
        let active_runs_ready = Arc::clone(&self.active_runs_ready);
        let registry = Arc::downgrade(&registry);
        let run_id = start.run.id;
        self.runtime.spawn(async move {
            let uncertain = match task.await {
                Ok(Ok(())) => false,
                Ok(Err(_)) | Err(_) => true,
            };
            broker.cancel_all();
            if uncertain && let Some(registry) = registry.upgrade() {
                let _ = registry.record_run_execution_uncertain(&run_id);
            }
            if let Ok(mut runs) = active_runs.lock() {
                runs.remove(&run_id);
                active_runs_ready.notify_all();
            }
        });
        Ok(())
    }

    fn cancel_run(&self, cancel: HttpRunDriverCancel) -> Result<(), HttpRunDriverError> {
        let runs = self
            .active_runs
            .lock()
            .map_err(|_| HttpRunDriverError::new("production active-run state unavailable"))?;
        let run = runs.get(&cancel.run_id).ok_or_else(|| {
            HttpRunDriverError::new(format!("production run is not active: {}", cancel.run_id))
        })?;
        if run.session_id != cancel.session_id {
            return Err(HttpRunDriverError::new(
                "production cancel session mismatch",
            ));
        }
        let (acknowledgement, acknowledged) = std_mpsc::sync_channel(1);
        run.cancel_sender
            .send(HttpProductionCancellationCommand {
                reason: cancel
                    .reason
                    .unwrap_or_else(|| "HTTP client requested cancellation".to_owned()),
                acknowledgement,
            })
            .map_err(|_| HttpRunDriverError::new("production cancellation owner is closed"))?;
        acknowledged.recv().map_err(|_| {
            HttpRunDriverError::new(
                "production cancellation owner stopped before durable acknowledgement",
            )
        })?
    }

    fn submit_approval(&self, approval: HttpRunDriverApproval) -> Result<(), HttpRunDriverError> {
        if approval.call_id != approval.decision.call_id
            || approval.run_id != approval.decision.run_id
        {
            return Err(HttpRunDriverError::new(
                "production approval decision identity mismatch",
            ));
        }
        let runs = self
            .active_runs
            .lock()
            .map_err(|_| HttpRunDriverError::new("production active-run state unavailable"))?;
        let run = runs.get(&approval.run_id).ok_or_else(|| {
            HttpRunDriverError::new(format!("production run is not active: {}", approval.run_id))
        })?;
        if run.session_id != approval.session_id {
            return Err(HttpRunDriverError::new(
                "production approval session mismatch",
            ));
        }
        run.broker.resolve(&approval.call_id, approval.decision)
    }

    fn verification_view(
        &self,
        session: &crate::HttpSessionSnapshot,
    ) -> Result<Option<HttpVerificationView>, HttpRunDriverError> {
        application_verification_view(Path::new(&session.session_log_path)).map_err(|error| {
            HttpRunDriverError::new(format!("failed to project verification state: {error}"))
        })
    }

    fn rerun_verification(
        &self,
        session: &crate::HttpSessionSnapshot,
        request: &HttpVerificationRerunRequest,
    ) -> Result<HttpVerificationView, HttpRunDriverError> {
        self.runtime
            .block_on(rerun_application_verification(
                &self.options.config_path,
                &self.options.launch_cwd,
                Path::new(&session.session_log_path),
                &session.durable_session_scope_id,
                &self.services,
                request,
            ))
            .map_err(|error| HttpRunDriverError::new(format!("verification rerun failed: {error}")))
    }

    fn wait_for_idle(&self, timeout: Duration) -> Result<(), HttpRunDriverError> {
        let deadline = Instant::now() + timeout;
        let mut runs = self
            .active_runs
            .lock()
            .map_err(|_| HttpRunDriverError::new("production active-run state unavailable"))?;
        while !runs.is_empty() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(HttpRunDriverError::new(format!(
                    "production shutdown timed out with {} owned run supervisor(s)",
                    runs.len()
                )));
            }
            let (next, wait) = self
                .active_runs_ready
                .wait_timeout(runs, remaining)
                .map_err(|_| HttpRunDriverError::new("production active-run state unavailable"))?;
            runs = next;
            if wait.timed_out() && !runs.is_empty() {
                return Err(HttpRunDriverError::new(format!(
                    "production shutdown timed out with {} owned run supervisor(s)",
                    runs.len()
                )));
            }
        }
        Ok(())
    }
}

struct HttpProductionActiveRun {
    session_id: String,
    broker: Arc<HttpApprovalBroker>,
    cancel_sender: mpsc::UnboundedSender<HttpProductionCancellationCommand>,
}

struct HttpProductionCancellationCommand {
    reason: String,
    acknowledgement: std_mpsc::SyncSender<Result<(), HttpRunDriverError>>,
}

struct HttpRunSupervisor {
    options: HttpProductionRunDriverOptions,
    services: ApplicationRunServices,
    preparer: Arc<dyn HttpApplicationRunPreparer>,
    event_bus: Arc<HttpLiveEventBus>,
    registry: Weak<HttpSessionRunRegistry>,
    broker: Arc<HttpApprovalBroker>,
    start: HttpRunDriverStart,
    cancel_receiver: mpsc::UnboundedReceiver<HttpProductionCancellationCommand>,
}

impl HttpRunSupervisor {
    async fn run(mut self) -> Result<(), HttpRunDriverError> {
        let registry = self.registry.upgrade().ok_or_else(|| {
            HttpRunDriverError::new("production registry closed before run preparation")
        })?;
        let interaction = match self.start.run.approval_mode {
            HttpRunApprovalMode::Ask => ApplicationRunInteraction::ExternallyInteractive,
            HttpRunApprovalMode::Deny | HttpRunApprovalMode::AllowReadonly => {
                ApplicationRunInteraction::AdapterManaged
            }
        };
        let request = ApplicationRunRequest {
            config_path: self.options.config_path.clone(),
            launch_cwd: self.options.launch_cwd.clone(),
            prompt: self.start.prompt.clone(),
            run_id: self.start.run.id.clone(),
            session_path: Some(PathBuf::from(&self.start.session.session_log_path)),
            interaction,
            constraints: None,
        };
        let services = self.services.clone();
        let preparer = Arc::clone(&self.preparer);
        let mut preparation = Box::pin(preparer.prepare(request, services));
        let preparation_outcome = tokio::select! {
            biased;
            result = &mut preparation => Ok(result),
            cancellation = self.cancel_receiver.recv() => Err(cancellation),
        };
        let preparation_result = match preparation_outcome {
            Ok(result) => {
                drop(preparation);
                result
            }
            Err(Some(cancellation)) => {
                let deadline = cancellation_deadline(self.options.cancellation_timeout);
                let joined =
                    tokio::time::timeout(remaining_until(deadline), &mut preparation).await;
                let preparation_result = match joined {
                    Ok(result) => result,
                    Err(_) => {
                        let error = HttpRunDriverError::new(
                            "production preparation did not quiesce before the cancellation deadline",
                        );
                        let error = quarantine_cancellation_failure(
                            &registry,
                            &self.start.run.id,
                            &cancellation.acknowledgement,
                            error,
                        );
                        let _ = preparation.await;
                        return Err(error);
                    }
                };
                drop(preparation);
                return match preparation_result {
                    Ok(prepared) => {
                        self.cancel_prepared_before_execution(
                            &registry,
                            cancellation,
                            prepared,
                            deadline,
                        )
                        .await
                    }
                    Err(_) => {
                        self.cancel_after_failed_preparation(&registry, cancellation, deadline)
                            .await
                    }
                };
            }
            Err(None) => {
                return Err(HttpRunDriverError::new(
                    "production cancellation owner closed during run preparation",
                ));
            }
        };
        let prepared = match preparation_result {
            Ok(prepared) => prepared,
            Err(error) => {
                let event = PublicRunEvent::new(
                    &self.start.session.durable_session_scope_id,
                    &self.start.run.id,
                    1,
                    PublicRunEventKind::RunFailed {
                        error: error.to_string(),
                    },
                );
                let event_bus = Arc::clone(&self.event_bus);
                tokio::task::spawn_blocking(move || event_bus.publish_run_event(event))
                    .await
                    .map_err(|_| {
                        HttpRunDriverError::new(
                            "production preparation terminal publication worker failed",
                        )
                    })?
                    .map_err(|error| HttpRunDriverError::new(error.to_string()))?;
                registry
                    .record_run_terminal(&self.start.run.id, HttpRunTerminalOutcome::Failed)
                    .map_err(registry_driver_error)?;
                return Ok(());
            }
        };
        if prepared.session_id() != self.start.session.durable_session_scope_id
            || prepared.session_log_path()
                != PathBuf::from(&self.start.session.session_log_path).as_path()
        {
            return Err(HttpRunDriverError::new(
                "prepared application run does not match its durable HTTP session binding",
            ));
        }
        let (execution, control) = prepared.into_parts();
        let control = Arc::new(control);
        let event_handler = HttpProductionEventHandler {
            durable_session_scope_id: self.start.session.durable_session_scope_id.clone(),
            run_id: self.start.run.id.clone(),
            approval_mode: self.start.run.approval_mode,
            approval_timeout: self.options.approval_timeout,
            registry: Arc::downgrade(&registry),
            broker: Arc::clone(&self.broker),
            event_bus: Arc::clone(&self.event_bus),
        };
        let approval_handler = HttpProductionApprovalHandler {
            mode: self.start.run.approval_mode,
            run_id: self.start.run.id.clone(),
            registry: Arc::downgrade(&registry),
            broker: Arc::clone(&self.broker),
        };
        let mut execution =
            Box::pin(execution.execute_on_owned_blocking(event_handler.clone(), approval_handler));
        tokio::select! {
            biased;
            result = &mut execution => {
                let terminal_was_delivered = control
                    .terminal_was_delivered()
                    .map_err(|error| HttpRunDriverError::new(error.to_string()))?;
                if !terminal_was_delivered {
                    return Err(HttpRunDriverError::new(
                        "production execution ended without a durable protocol terminal",
                    ));
                }
                let terminal = http_terminal_from_application_result(&result);
                registry
                    .record_run_terminal(&self.start.run.id, terminal)
                    .map_err(registry_driver_error)?;
            }
            cancellation = self.cancel_receiver.recv() => {
                let Some(cancellation) = cancellation else {
                    return Err(HttpRunDriverError::new(
                        "production cancellation owner closed before run terminal",
                    ));
                };
                let acknowledgement = cancellation.acknowledgement;
                let deadline = cancellation_deadline(self.options.cancellation_timeout);
                let mut acknowledgement_sent = false;
                let request_control = Arc::clone(&control);
                let request_broker = Arc::clone(&self.broker);
                let request_timeout = remaining_until(deadline);
                let mut request_worker = tokio::task::spawn_blocking(move || {
                    request_control.request_cancellation(
                        cancellation.reason,
                        Some(request_timeout),
                        || request_broker.cancel_all(),
                    )
                });
                let request = match tokio::time::timeout(
                    remaining_until(deadline),
                    &mut request_worker,
                )
                .await
                {
                    Ok(Ok(request)) => request,
                    Ok(Err(_)) => {
                        let error = quarantine_cancellation_failure(
                            &registry,
                            &self.start.run.id,
                            &acknowledgement,
                            HttpRunDriverError::new(
                                "production cancellation activation worker failed",
                            ),
                        );
                        let natural_result = (&mut execution).await;
                        if record_natural_terminal_if_delivered(
                            &control,
                            &registry,
                            &self.start.run.id,
                            &natural_result,
                        )? {
                            return Ok(());
                        }
                        return Err(error);
                    }
                    Err(_) => {
                        let error = quarantine_cancellation_failure(
                            &registry,
                            &self.start.run.id,
                            &acknowledgement,
                            HttpRunDriverError::new(
                                "production cancellation activation missed its shared deadline",
                            ),
                        );
                        acknowledgement_sent = true;
                        match request_worker.await {
                            Ok(request) => request,
                            Err(_) => {
                                let natural_result = (&mut execution).await;
                                if record_natural_terminal_if_delivered(
                                    &control,
                                    &registry,
                                    &self.start.run.id,
                                    &natural_result,
                                )? {
                                    return Ok(());
                                }
                                return Err(error);
                            }
                        }
                    }
                };
                let ticket = match request {
                    Ok(ticket) => ticket,
                    Err(error) => match error.into_ticket() {
                        Some(ticket) => ticket,
                        None => {
                            let natural_result = match tokio::time::timeout(
                                remaining_until(deadline),
                                &mut execution,
                            )
                            .await
                            {
                                Ok(result) => result,
                                Err(_) => {
                                    let error = HttpRunDriverError::new(
                                        "natural run terminal did not join before the cancellation deadline",
                                    );
                                    let error = if acknowledgement_sent {
                                        error
                                    } else {
                                        quarantine_cancellation_failure(
                                            &registry,
                                            &self.start.run.id,
                                            &acknowledgement,
                                            error,
                                        )
                                    };
                                    let natural_result = (&mut execution).await;
                                    if record_natural_terminal_if_delivered(
                                        &control,
                                        &registry,
                                        &self.start.run.id,
                                        &natural_result,
                                    )? {
                                        return Ok(());
                                    }
                                    return Err(error);
                                }
                            };
                            let terminal_was_delivered = match control.terminal_was_delivered() {
                                Ok(delivered) => delivered,
                                Err(error) => {
                                    let error = HttpRunDriverError::new(error.to_string());
                                    let error = if acknowledgement_sent {
                                        error
                                    } else {
                                        quarantine_cancellation_failure(
                                            &registry,
                                            &self.start.run.id,
                                            &acknowledgement,
                                            error,
                                        )
                                    };
                                    return Err(error);
                                }
                            };
                            if !terminal_was_delivered {
                                let error = HttpRunDriverError::new(
                                    "natural run completion won cancellation without a durable protocol terminal",
                                );
                                let error = if acknowledgement_sent {
                                    error
                                } else {
                                    quarantine_cancellation_failure(
                                        &registry,
                                        &self.start.run.id,
                                        &acknowledgement,
                                        error,
                                    )
                                };
                                return Err(error);
                            }
                            let terminal = http_terminal_from_application_result(&natural_result);
                            if let Err(error) = registry
                                .record_run_terminal(&self.start.run.id, terminal)
                            {
                                let error = registry_driver_error(error);
                                let error = if acknowledgement_sent {
                                    error
                                } else {
                                    quarantine_cancellation_failure(
                                        &registry,
                                        &self.start.run.id,
                                        &acknowledgement,
                                        error,
                                    )
                                };
                                return Err(error);
                            }
                            self.broker.cancel_all();
                            if !acknowledgement_sent {
                                let _ = acknowledgement.send(Ok(()));
                            }
                            return Ok(());
                        }
                    },
                };
                let execution_joined = tokio::time::timeout(
                    ticket.remaining_timeout(),
                    &mut execution,
                )
                .await
                .is_ok();
                if !execution_joined && !acknowledgement_sent {
                    let _ = quarantine_cancellation_failure(
                        &registry,
                        &self.start.run.id,
                        &acknowledgement,
                        HttpRunDriverError::new(
                            "production execution did not join before the cancellation deadline",
                        ),
                    );
                    acknowledgement_sent = true;
                }
                let finalize_control = Arc::clone(&control);
                let runtime = tokio::runtime::Handle::current();
                let mut cancellation_events = event_handler;
                let mut finalize_worker = tokio::task::spawn_blocking(move || {
                    runtime.block_on(finalize_control.finalize_cancellation(
                        ticket,
                        execution_joined,
                        &mut cancellation_events,
                    ))
                });
                let finalized = match tokio::time::timeout(
                    remaining_until(deadline),
                    &mut finalize_worker,
                )
                .await
                {
                    Ok(Ok(finalized)) => finalized,
                    Ok(Err(_)) => Err(anyhow!(
                        "production cancellation finalization worker failed"
                    )),
                    Err(_) => {
                        if !acknowledgement_sent {
                            let _ = quarantine_cancellation_failure(
                                &registry,
                                &self.start.run.id,
                                &acknowledgement,
                                HttpRunDriverError::new(
                                    "production cancellation finalization missed its shared deadline",
                                ),
                            );
                            acknowledgement_sent = true;
                        }
                        finalize_worker.await.map_err(|_| {
                            HttpRunDriverError::new(
                                "production cancellation finalization worker failed",
                            )
                        })?
                    }
                };
                let terminal = match finalized {
                    Ok(sigil_kernel::RunCancellationTerminalOutcome::Cancelled) => {
                        HttpRunTerminalOutcome::Cancelled
                    }
                    Ok(sigil_kernel::RunCancellationTerminalOutcome::Interrupted) => {
                        HttpRunTerminalOutcome::Interrupted
                    }
                    Err(error) => {
                        let error = HttpRunDriverError::new(format!(
                            "production cancellation terminal could not be durably proven: {error}"
                        ));
                        let error = if acknowledgement_sent {
                            error
                        } else {
                            quarantine_cancellation_failure(
                                &registry,
                                &self.start.run.id,
                                &acknowledgement,
                                error,
                            )
                        };
                        if !execution_joined {
                            let _ = (&mut execution).await;
                        }
                        return Err(error);
                    }
                };
                if !execution_joined {
                    let _ = (&mut execution).await;
                }
                let terminal_was_delivered = match control.terminal_was_delivered() {
                    Ok(delivered) => delivered,
                    Err(error) => {
                        let error = HttpRunDriverError::new(error.to_string());
                        let error = if acknowledgement_sent {
                            error
                        } else {
                            quarantine_cancellation_failure(
                                &registry,
                                &self.start.run.id,
                                &acknowledgement,
                                error,
                            )
                        };
                        return Err(error);
                    }
                };
                if !terminal_was_delivered {
                    let error = HttpRunDriverError::new(
                        "production cancellation ended without a durable protocol terminal",
                    );
                    let error = if acknowledgement_sent {
                        error
                    } else {
                        quarantine_cancellation_failure(
                            &registry,
                            &self.start.run.id,
                            &acknowledgement,
                            error,
                        )
                    };
                    return Err(error);
                }
                if let Err(error) = registry
                    .record_run_terminal(&self.start.run.id, terminal)
                {
                    let error = registry_driver_error(error);
                    let error = if acknowledgement_sent {
                        error
                    } else {
                        quarantine_cancellation_failure(
                            &registry,
                            &self.start.run.id,
                            &acknowledgement,
                            error,
                        )
                    };
                    return Err(error);
                }
                if !acknowledgement_sent {
                    let _ = acknowledgement.send(Ok(()));
                }
            }
        }
        self.broker.cancel_all();
        Ok(())
    }

    async fn cancel_prepared_before_execution(
        &self,
        registry: &Arc<HttpSessionRunRegistry>,
        cancellation: HttpProductionCancellationCommand,
        prepared: PreparedApplicationRun,
        deadline: Instant,
    ) -> Result<(), HttpRunDriverError> {
        let acknowledgement = cancellation.acknowledgement;
        if prepared.session_id() != self.start.session.durable_session_scope_id
            || prepared.session_log_path()
                != PathBuf::from(&self.start.session.session_log_path).as_path()
        {
            let error = HttpRunDriverError::new(
                "prepared cancellation does not match its durable HTTP session binding",
            );
            return Err(quarantine_cancellation_failure(
                registry,
                &self.start.run.id,
                &acknowledgement,
                error,
            ));
        }
        let (execution, control) = prepared.into_parts();
        let control = Arc::new(control);
        let request_control = Arc::clone(&control);
        let request_broker = Arc::clone(&self.broker);
        let request_timeout = remaining_until(deadline);
        let mut request_worker = tokio::task::spawn_blocking(move || {
            request_control.request_cancellation(cancellation.reason, Some(request_timeout), || {
                request_broker.cancel_all()
            })
        });
        let mut acknowledgement_sent = false;
        let request = match tokio::time::timeout(remaining_until(deadline), &mut request_worker)
            .await
        {
            Ok(Ok(request)) => request,
            Ok(Err(_)) => {
                let error =
                    HttpRunDriverError::new("pre-execution cancellation activation worker failed");
                return Err(quarantine_cancellation_failure(
                    registry,
                    &self.start.run.id,
                    &acknowledgement,
                    error,
                ));
            }
            Err(_) => {
                let _ = quarantine_cancellation_failure(
                    registry,
                    &self.start.run.id,
                    &acknowledgement,
                    HttpRunDriverError::new(
                        "pre-execution cancellation activation missed its shared deadline",
                    ),
                );
                acknowledgement_sent = true;
                request_worker.await.map_err(|_| {
                    HttpRunDriverError::new("pre-execution cancellation activation worker failed")
                })?
            }
        };
        let ticket = match request {
            Ok(ticket) => ticket,
            Err(error) => match error.into_ticket() {
                Some(ticket) => ticket,
                None => {
                    let error = HttpRunDriverError::new(
                        "pre-execution cancellation could not be durably activated",
                    );
                    return Err(if acknowledgement_sent {
                        error
                    } else {
                        quarantine_cancellation_failure(
                            registry,
                            &self.start.run.id,
                            &acknowledgement,
                            error,
                        )
                    });
                }
            },
        };
        drop(execution);
        let finalize_control = Arc::clone(&control);
        let runtime = tokio::runtime::Handle::current();
        let mut event_handler = HttpProductionEventHandler {
            durable_session_scope_id: self.start.session.durable_session_scope_id.clone(),
            run_id: self.start.run.id.clone(),
            approval_mode: self.start.run.approval_mode,
            approval_timeout: self.options.approval_timeout,
            registry: Arc::downgrade(registry),
            broker: Arc::clone(&self.broker),
            event_bus: Arc::clone(&self.event_bus),
        };
        let mut finalize_worker = tokio::task::spawn_blocking(move || {
            runtime.block_on(finalize_control.finalize_cancellation(
                ticket,
                true,
                &mut event_handler,
            ))
        });
        let finalized = match tokio::time::timeout(remaining_until(deadline), &mut finalize_worker)
            .await
        {
            Ok(Ok(finalized)) => finalized,
            Ok(Err(_)) => Err(anyhow!(
                "pre-execution cancellation finalization worker failed"
            )),
            Err(_) => {
                if !acknowledgement_sent {
                    let _ = quarantine_cancellation_failure(
                        registry,
                        &self.start.run.id,
                        &acknowledgement,
                        HttpRunDriverError::new(
                            "pre-execution cancellation finalization missed its shared deadline",
                        ),
                    );
                    acknowledgement_sent = true;
                }
                finalize_worker.await.map_err(|_| {
                    HttpRunDriverError::new("pre-execution cancellation finalization worker failed")
                })?
            }
        };
        let result = finalized
            .map_err(|error| {
                HttpRunDriverError::new(format!(
                    "pre-execution cancellation terminal could not be durably proven: {error}"
                ))
            })
            .and_then(|terminal| {
                if !control
                    .terminal_was_delivered()
                    .map_err(|error| HttpRunDriverError::new(error.to_string()))?
                {
                    return Err(HttpRunDriverError::new(
                        "pre-execution cancellation ended without a durable protocol terminal",
                    ));
                }
                let terminal = match terminal {
                    sigil_kernel::RunCancellationTerminalOutcome::Cancelled => {
                        HttpRunTerminalOutcome::Cancelled
                    }
                    sigil_kernel::RunCancellationTerminalOutcome::Interrupted => {
                        HttpRunTerminalOutcome::Interrupted
                    }
                };
                registry
                    .record_run_terminal(&self.start.run.id, terminal)
                    .map(|_| ())
                    .map_err(registry_driver_error)
            });
        match result {
            Ok(()) => {
                if !acknowledgement_sent {
                    let _ = acknowledgement.send(Ok(()));
                }
                Ok(())
            }
            Err(error) if acknowledgement_sent => Err(error),
            Err(error) => Err(quarantine_cancellation_failure(
                registry,
                &self.start.run.id,
                &acknowledgement,
                error,
            )),
        }
    }

    async fn cancel_after_failed_preparation(
        &self,
        registry: &Arc<HttpSessionRunRegistry>,
        cancellation: HttpProductionCancellationCommand,
        deadline: Instant,
    ) -> Result<(), HttpRunDriverError> {
        let acknowledgement = cancellation.acknowledgement;
        let config_path = self.options.config_path.clone();
        let session_path = PathBuf::from(&self.start.session.session_log_path);
        let run_id = self.start.run.id.clone();
        let reason = cancellation.reason;
        let mut binding_worker = tokio::task::spawn_blocking(move || {
            record_application_preparation_cancellation(
                &config_path,
                &session_path,
                &run_id,
                &reason,
            )
        });
        let mut acknowledgement_sent = false;
        let binding_result =
            match tokio::time::timeout(remaining_until(deadline), &mut binding_worker).await {
                Ok(joined) => match joined {
                    Ok(binding) => {
                        binding.map_err(|error| HttpRunDriverError::new(error.to_string()))
                    }
                    Err(_) => Err(HttpRunDriverError::new(
                        "production preparation cancellation worker failed",
                    )),
                },
                Err(_) => {
                    let error = HttpRunDriverError::new(
                        "preparation cancellation evidence missed its shared deadline",
                    );
                    let _ = quarantine_cancellation_failure(
                        registry,
                        &self.start.run.id,
                        &acknowledgement,
                        error,
                    );
                    acknowledgement_sent = true;
                    Ok(binding_worker
                        .await
                        .map_err(|_| {
                            HttpRunDriverError::new(
                                "production preparation cancellation worker failed",
                            )
                        })?
                        .map_err(|error| HttpRunDriverError::new(error.to_string()))?)
                }
            };
        let binding = match binding_result {
            Ok(binding) => binding,
            Err(error) if acknowledgement_sent => return Err(error),
            Err(error) => {
                return Err(quarantine_cancellation_failure(
                    registry,
                    &self.start.run.id,
                    &acknowledgement,
                    error,
                ));
            }
        };
        let result = async {
            if binding.session_scope_id != self.start.session.durable_session_scope_id
                || binding.session_log_path != Path::new(&self.start.session.session_log_path)
            {
                return Err(HttpRunDriverError::new(
                    "preparation cancellation does not match its durable HTTP session binding",
                ));
            }
            let event = PublicRunEvent::new(
                &self.start.session.durable_session_scope_id,
                &self.start.run.id,
                1,
                PublicRunEventKind::RunCancelled,
            );
            let event_bus = Arc::clone(&self.event_bus);
            let mut publication_worker =
                tokio::task::spawn_blocking(move || event_bus.publish_run_event(event));
            match tokio::time::timeout(remaining_until(deadline), &mut publication_worker).await {
                Ok(joined) => {
                    joined
                        .map_err(|_| {
                            HttpRunDriverError::new(
                                "production preparation cancellation publication worker failed",
                            )
                        })?
                        .map_err(|error| HttpRunDriverError::new(error.to_string()))?;
                }
                Err(_) => {
                    let error = HttpRunDriverError::new(
                        "preparation cancellation publication missed its shared deadline",
                    );
                    if !acknowledgement_sent {
                        let _ = quarantine_cancellation_failure(
                            registry,
                            &self.start.run.id,
                            &acknowledgement,
                            error,
                        );
                        acknowledgement_sent = true;
                    }
                    publication_worker
                        .await
                        .map_err(|_| {
                            HttpRunDriverError::new(
                                "production preparation cancellation publication worker failed",
                            )
                        })?
                        .map_err(|error| HttpRunDriverError::new(error.to_string()))?;
                }
            };
            registry
                .record_run_terminal(&self.start.run.id, HttpRunTerminalOutcome::Cancelled)
                .map(|_| ())
                .map_err(registry_driver_error)
        }
        .await;
        if acknowledgement_sent {
            return result;
        }
        match result {
            Ok(()) => {
                let _ = acknowledgement.send(Ok(()));
                Ok(())
            }
            Err(error) => Err(quarantine_cancellation_failure(
                registry,
                &self.start.run.id,
                &acknowledgement,
                error,
            )),
        }
    }
}

#[derive(Clone)]
struct HttpProductionEventHandler {
    durable_session_scope_id: String,
    run_id: String,
    approval_mode: HttpRunApprovalMode,
    approval_timeout: Duration,
    registry: Weak<HttpSessionRunRegistry>,
    broker: Arc<HttpApprovalBroker>,
    event_bus: Arc<HttpLiveEventBus>,
}

impl ApplicationRunEventHandler for HttpProductionEventHandler {
    fn handle_public_event(&mut self, event: PublicRunEvent) -> Result<()> {
        if event.run_id != self.run_id {
            return Err(anyhow!(
                "application event belongs to another production run"
            ));
        }
        if event.session_id != self.durable_session_scope_id {
            return Err(anyhow!(
                "application event belongs to another durable production session"
            ));
        }
        let approval_request = match &event.event {
            PublicRunEventKind::ApprovalRequested { call, spec, .. }
                if self.approval_mode == HttpRunApprovalMode::Ask =>
            {
                let registry = self
                    .registry
                    .upgrade()
                    .ok_or_else(|| anyhow!("production approval registry is closed"))?;
                let pending =
                    self.broker
                        .register(&self.run_id, call, spec, self.approval_timeout)?;
                if let Err(error) =
                    registry.register_approval_request(&self.run_id, pending.clone())
                {
                    self.broker.cancel(&call.id);
                    return Err(anyhow!(error));
                }
                Some(pending)
            }
            _ => None,
        };
        if let Err(error) = self
            .event_bus
            .publish_run_event_with_approval(event, approval_request.clone())
        {
            if let Some(approval) = approval_request {
                let call_id = approval.call_id;
                self.broker.cancel(&call_id);
                if let Some(registry) = self.registry.upgrade() {
                    let _ = registry.expire_approval_request(&self.run_id, &call_id);
                }
            }
            return Err(anyhow!(error));
        }
        Ok(())
    }
}

struct HttpProductionApprovalHandler {
    mode: HttpRunApprovalMode,
    run_id: String,
    registry: Weak<HttpSessionRunRegistry>,
    broker: Arc<HttpApprovalBroker>,
}

impl ApprovalHandler for HttpProductionApprovalHandler {
    fn approve_tool_call(&mut self, call: &ToolCall, spec: &ToolSpec) -> Result<ToolApproval> {
        match self.mode {
            HttpRunApprovalMode::Deny => Ok(ToolApproval::Deny {
                reason: "HTTP run approval mode denies gated tool calls".to_owned(),
            }),
            HttpRunApprovalMode::AllowReadonly
                if spec.access == ToolAccess::Read && spec.network_effect.is_none() =>
            {
                Ok(ToolApproval::Approve)
            }
            HttpRunApprovalMode::AllowReadonly => Ok(ToolApproval::Deny {
                reason: if matches!(spec.network_effect, Some(NetworkEffect::Read)) {
                    "network read approval requires an explicit HTTP user decision".to_owned()
                } else {
                    "HTTP allow_readonly mode denies non-read-only tool calls".to_owned()
                },
            }),
            HttpRunApprovalMode::Ask => {
                let outcome = self.broker.wait_for_decision(&call.id)?;
                if outcome.expired
                    && let Some(registry) = self.registry.upgrade()
                {
                    registry
                        .expire_approval_request(&self.run_id, &call.id)
                        .map_err(|error| anyhow!(error))?;
                }
                match outcome.decision {
                    Some(HttpApprovalDecisionRecord {
                        decision: ToolApprovalUserDecision::Approved,
                        ..
                    }) => Ok(ToolApproval::Approve),
                    Some(HttpApprovalDecisionRecord {
                        decision: ToolApprovalUserDecision::Denied,
                        reason,
                        ..
                    }) => Ok(ToolApproval::Deny {
                        reason: reason.unwrap_or_else(|| "HTTP user denied tool call".to_owned()),
                    }),
                    Some(HttpApprovalDecisionRecord {
                        decision: ToolApprovalUserDecision::ApprovedForSession,
                        ..
                    }) => Err(anyhow!(
                        "HTTP V1 does not support approve-for-session decisions"
                    )),
                    None => Ok(ToolApproval::Deny {
                        reason: "HTTP approval request expired before a decision arrived"
                            .to_owned(),
                    }),
                }
            }
        }
    }

    fn approval_is_explicit_user_action(&self) -> bool {
        self.mode == HttpRunApprovalMode::Ask
    }
}

#[derive(Default)]
struct HttpApprovalBroker {
    pending: Mutex<BTreeMap<String, Arc<HttpApprovalSlot>>>,
}

impl HttpApprovalBroker {
    fn register(
        &self,
        run_id: &str,
        call: &ToolCall,
        spec: &ToolSpec,
        timeout: Duration,
    ) -> Result<HttpPendingApproval> {
        let now_ms = current_unix_time_ms();
        let timeout_ms = timeout.as_millis().try_into().unwrap_or(u64::MAX);
        let expires_at_ms = now_ms.saturating_add(timeout_ms);
        let tool_call_hash = tool_call_hash(call)?;
        let approval_request_id =
            approval_request_id(run_id, &call.id, &tool_call_hash, expires_at_ms);
        let slot = Arc::new(HttpApprovalSlot {
            deadline: Instant::now()
                .checked_add(timeout)
                .unwrap_or_else(Instant::now),
            state: Mutex::new(HttpApprovalSlotState::Waiting),
            changed: Condvar::new(),
        });
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| anyhow!("production approval broker is unavailable"))?;
        if pending.insert(call.id.clone(), slot).is_some() {
            return Err(anyhow!("duplicate production approval call id"));
        }
        Ok(HttpPendingApproval {
            call_id: call.id.clone(),
            tool_name: spec.name.clone(),
            approval_request_id,
            tool_call_hash,
            policy_version: HTTP_APPROVAL_POLICY_VERSION.to_owned(),
            expires_at_ms,
        })
    }

    fn resolve(
        &self,
        call_id: &str,
        decision: HttpApprovalDecisionRecord,
    ) -> Result<(), HttpRunDriverError> {
        let slot = self
            .pending
            .lock()
            .map_err(|_| HttpRunDriverError::new("production approval broker is unavailable"))?
            .get(call_id)
            .cloned()
            .ok_or_else(|| {
                HttpRunDriverError::new(format!("production approval is not pending: {call_id}"))
            })?;
        let mut state = slot
            .state
            .lock()
            .map_err(|_| HttpRunDriverError::new("production approval slot is unavailable"))?;
        if !matches!(*state, HttpApprovalSlotState::Waiting) {
            return Err(HttpRunDriverError::new(format!(
                "production approval is no longer waiting: {call_id}"
            )));
        }
        *state = HttpApprovalSlotState::Resolved(decision);
        slot.changed.notify_all();
        Ok(())
    }

    fn wait_for_decision(&self, call_id: &str) -> Result<HttpApprovalWaitOutcome> {
        let slot = self
            .pending
            .lock()
            .map_err(|_| anyhow!("production approval broker is unavailable"))?
            .get(call_id)
            .cloned()
            .ok_or_else(|| anyhow!("production approval slot is missing"))?;
        let mut state = slot
            .state
            .lock()
            .map_err(|_| anyhow!("production approval slot is unavailable"))?;
        loop {
            match &*state {
                HttpApprovalSlotState::Resolved(decision) => {
                    let decision = decision.clone();
                    drop(state);
                    self.remove(call_id, &slot);
                    return Ok(HttpApprovalWaitOutcome {
                        decision: Some(decision),
                        expired: false,
                    });
                }
                HttpApprovalSlotState::Cancelled => {
                    drop(state);
                    self.remove(call_id, &slot);
                    return Err(anyhow!("production approval wait was cancelled"));
                }
                HttpApprovalSlotState::Waiting => {}
            }
            let remaining = slot.deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                *state = HttpApprovalSlotState::Cancelled;
                drop(state);
                self.remove(call_id, &slot);
                return Ok(HttpApprovalWaitOutcome {
                    decision: None,
                    expired: true,
                });
            }
            let waited = slot
                .changed
                .wait_timeout(state, remaining)
                .map_err(|_| anyhow!("production approval slot is unavailable"))?;
            state = waited.0;
        }
    }

    fn cancel(&self, call_id: &str) {
        let slot = self
            .pending
            .lock()
            .ok()
            .and_then(|pending| pending.get(call_id).cloned());
        if let Some(slot) = slot
            && let Ok(mut state) = slot.state.lock()
        {
            *state = HttpApprovalSlotState::Cancelled;
            slot.changed.notify_all();
        }
    }

    fn cancel_all(&self) {
        let slots = self
            .pending
            .lock()
            .map(|pending| pending.values().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        for slot in slots {
            if let Ok(mut state) = slot.state.lock() {
                *state = HttpApprovalSlotState::Cancelled;
                slot.changed.notify_all();
            }
        }
    }

    fn remove(&self, call_id: &str, expected: &Arc<HttpApprovalSlot>) {
        if let Ok(mut pending) = self.pending.lock()
            && pending
                .get(call_id)
                .is_some_and(|slot| Arc::ptr_eq(slot, expected))
        {
            pending.remove(call_id);
        }
    }
}

struct HttpApprovalSlot {
    deadline: Instant,
    state: Mutex<HttpApprovalSlotState>,
    changed: Condvar,
}

enum HttpApprovalSlotState {
    Waiting,
    Resolved(HttpApprovalDecisionRecord),
    Cancelled,
}

struct HttpApprovalWaitOutcome {
    decision: Option<HttpApprovalDecisionRecord>,
    expired: bool,
}

fn tool_call_hash(call: &ToolCall) -> Result<String> {
    let bytes = serde_json::to_vec(call)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn approval_request_id(
    run_id: &str,
    call_id: &str,
    tool_call_hash: &str,
    expires_at_ms: u64,
) -> String {
    let mut hasher = Sha256::new();
    for part in [
        run_id.as_bytes(),
        call_id.as_bytes(),
        tool_call_hash.as_bytes(),
        &expires_at_ms.to_be_bytes(),
    ] {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part);
    }
    format!("http-approval-v1:{:x}", hasher.finalize())
}

fn current_unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn cancellation_deadline(timeout: Duration) -> Instant {
    Instant::now()
        .checked_add(timeout)
        .unwrap_or_else(Instant::now)
}

fn remaining_until(deadline: Instant) -> Duration {
    deadline.saturating_duration_since(Instant::now())
}

fn quarantine_cancellation_failure(
    registry: &HttpSessionRunRegistry,
    run_id: &str,
    acknowledgement: &std_mpsc::SyncSender<Result<(), HttpRunDriverError>>,
    error: HttpRunDriverError,
) -> HttpRunDriverError {
    let error = match registry.record_run_execution_uncertain(run_id) {
        Ok(_) => error,
        Err(quarantine_error) => HttpRunDriverError::new(format!(
            "{error}; production run quarantine failed: {quarantine_error}"
        )),
    };
    let _ = acknowledgement.send(Err(error.clone()));
    error
}

fn record_natural_terminal_if_delivered(
    control: &ApplicationRunControl,
    registry: &HttpSessionRunRegistry,
    run_id: &str,
    result: &Result<ApplicationRunOutput>,
) -> Result<bool, HttpRunDriverError> {
    if !control
        .terminal_was_delivered()
        .map_err(|error| HttpRunDriverError::new(error.to_string()))?
    {
        return Ok(false);
    }
    registry
        .record_run_terminal(run_id, http_terminal_from_application_result(result))
        .map_err(registry_driver_error)?;
    Ok(true)
}

fn http_terminal_from_application_result(
    result: &Result<ApplicationRunOutput>,
) -> HttpRunTerminalOutcome {
    match result {
        Ok(output) => match output.terminal_status {
            ApplicationRunTerminalStatus::Succeeded => HttpRunTerminalOutcome::Finished,
            ApplicationRunTerminalStatus::Interrupted => HttpRunTerminalOutcome::Interrupted,
            ApplicationRunTerminalStatus::Blocked => HttpRunTerminalOutcome::Failed,
        },
        Err(_) => HttpRunTerminalOutcome::Failed,
    }
}

fn registry_driver_error(error: crate::HttpRegistryError) -> HttpRunDriverError {
    HttpRunDriverError::new(format!(
        "production registry terminal update failed: {error}"
    ))
}

#[cfg(test)]
#[path = "tests/production_driver_tests.rs"]
mod tests;
