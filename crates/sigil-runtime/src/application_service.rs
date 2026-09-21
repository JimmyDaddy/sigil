//! Runtime implementation boundary for the transport-neutral application contract.
//!
//! This module owns the one in-process application service used by product surfaces.  Physical
//! resource allocation remains below the injected runtime executor/source; this layer owns
//! command identity, replay/conflict handling, page request lifecycle, and the application-port
//! boundary.

use std::{
    collections::BTreeMap,
    fmt,
    sync::{Arc, Mutex},
};

use futures::future::BoxFuture;
use sigil_application::{
    ApplicationCommandReceipt, ApplicationCommandRequest, ApplicationDomainReceipt,
    ApplicationError, ApplicationInFlightReceipt, ApplicationPort, CommandConflict,
    CommandEffectBinding, CommandLifecyclePhase, CommandNoEffectProof, CommandRecoveryBinding,
    CommandRejection, CommandReservationKey, OpenProjectionRequest, PageCancellationReceipt,
    PageRequestId, ProjectionDeliveryAck, ProjectionPage, ProjectionPageRequest,
    ProjectionSnapshot, SafetyStopDisposition, SafetyStopRequestedButUnrecorded,
    UncertainCommandReceipt, command_fingerprint,
};

/// Runtime query source for the bounded application projection.
///
/// Implementations adapt durable session/application truth into renderer-safe application
/// snapshots and pages.  They must not expose paths, provider payloads, or physical authority
/// objects through the application contract.
pub trait RuntimeApplicationProjectionSource: Send + Sync {
    /// Checks the host-owned scope without requiring the projection or command journal to
    /// be readable. Recovery remains available when either projection cannot be opened.
    fn validate_recovery_scope(
        &self,
        _scope: &sigil_application::ApplicationScope,
    ) -> Result<(), ApplicationError> {
        Err(ApplicationError::Unavailable)
    }
    fn delivery_batch(
        &self,
        _request: sigil_application::DurableDeliveryRequest,
    ) -> BoxFuture<'static, Result<sigil_application::DurableDeliveryBatch, ApplicationError>> {
        Box::pin(async { Err(ApplicationError::NotFound) })
    }
    fn open_projection(
        &self,
        request: OpenProjectionRequest,
    ) -> BoxFuture<'static, Result<ProjectionSnapshot, ApplicationError>>;

    fn page(
        &self,
        request: ProjectionPageRequest,
    ) -> BoxFuture<'static, Result<ProjectionPage, ApplicationError>>;
}

/// Runtime-owned command effect result after durable application admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeApplicationDispatch {
    Settled(ApplicationDomainReceipt),
    /// The domain owner has proven that no physical effect crossed its first boundary.
    ConfirmedNoEffect {
        rejection: CommandRejection,
        proof: CommandNoEffectProof,
    },
    /// Legacy executors may still report a rejection, but it is not a no-effect proof. The
    /// application service records it as uncertain unless the owner uses `ConfirmedNoEffect`.
    Rejected(CommandRejection),
    /// The host crossed a boundary whose domain outcome cannot be proved from this dispatch.
    /// The application service supplies the only typed recovery binding from the reservation key.
    Uncertain(UncertainCommandReceipt),
}

/// Runtime command executor supplied by the composition root.
///
/// The service reserves the command before calling this trait.  An executor error is therefore
/// converted to an `Uncertain` receipt instead of being treated as proof that no effect happened.
pub trait RuntimeApplicationCommandExecutor: Send + Sync {
    /// Reads the original Intent binding for an existing runtime transition. Implementations
    /// must not create an Intent, change a gate, or accept another command kind here.
    fn session_runtime_resume_binding(
        &self,
        _request: ApplicationCommandRequest,
    ) -> BoxFuture<'static, Result<CommandEffectBinding, ApplicationError>> {
        Box::pin(async { Err(ApplicationError::Unavailable) })
    }
    /// Queries the original domain owner's causal commit. This must never dispatch a command
    /// or infer success from a matching current value, channel send, or elapsed time.
    fn reconcile(
        &self,
        _request: ApplicationCommandRequest,
    ) -> BoxFuture<'static, Result<Option<RuntimeApplicationDispatch>, ApplicationError>> {
        Box::pin(async { Ok(None) })
    }
    /// Asks the concrete domain owner to durably bind the command before any physical effect.
    /// Implementations must return an error when they cannot prove such a binding; there is no
    /// default or process-local fallback.
    fn bind_effect(
        &self,
        request: ApplicationCommandRequest,
        key: CommandReservationKey,
        fingerprint: String,
    ) -> BoxFuture<'static, Result<CommandEffectBinding, ApplicationError>>;

    fn dispatch(
        &self,
        request: ApplicationCommandRequest,
    ) -> BoxFuture<'static, Result<RuntimeApplicationDispatch, ApplicationError>>;

    /// Requests an emergency close of the exact owner's forward gate when the reservation
    /// ledger is unavailable. This is deliberately separate from normal dispatch: a successful
    /// enqueue or a generic dispatch result does not prove that the gate is closed.
    fn request_safety_stop(
        &self,
        _request: ApplicationCommandRequest,
    ) -> BoxFuture<'static, Result<SafetyStopDisposition, ApplicationError>> {
        Box::pin(async { Ok(SafetyStopDisposition::Uncertain) })
    }
}

/// Runtime owner for application projection delivery acknowledgements.
pub trait RuntimeApplicationDeliveryAcker: Send + Sync {
    fn acknowledge(
        &self,
        acknowledgement: ProjectionDeliveryAck,
    ) -> BoxFuture<'static, Result<(), ApplicationError>>;
}

/// Result of atomically reserving a command identity in the application journal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeApplicationReservationAdmission {
    Reserved,
    InFlight(ApplicationInFlightReceipt),
    Existing(Box<ApplicationCommandReceipt>),
    Conflict(CommandConflict),
}

/// Durable application reservation store supplied by the runtime composition root.
pub trait RuntimeApplicationReservationStore: Send + Sync {
    /// Publishes an explicit retry gate for an existing runtime transition with unchanged K/F
    /// and owner Intent. Ordinary uncertain commands cannot use this transition.
    fn resume_session_runtime_effect(
        &self,
        _key: CommandReservationKey,
        _fingerprint: String,
        _binding: CommandEffectBinding,
    ) -> BoxFuture<'static, Result<(), ApplicationError>> {
        Box::pin(async { Err(ApplicationError::Unavailable) })
    }
    fn recover_control_log(
        &self,
        _action: sigil_application::ControlLogRecoveryAction,
    ) -> BoxFuture<'static, Result<sigil_application::ControlLogRecoveryOutcome, ApplicationError>>
    {
        Box::pin(async { Err(ApplicationError::Unavailable) })
    }
    fn forward_guard(
        &self,
        _request: &ApplicationCommandRequest,
    ) -> Result<
        Option<Box<dyn sigil_kernel::managed_storage::ManagedStorageForwardGuardV1>>,
        ApplicationError,
    > {
        Ok(None)
    }
    fn command_journal_binding(
        &self,
    ) -> Result<Option<sigil_application::CommandJournalBinding>, ApplicationError> {
        Ok(None)
    }
    fn original_command_context(
        &self,
        _key: CommandReservationKey,
    ) -> BoxFuture<
        'static,
        Result<Option<sigil_application::OriginalCommandContext>, ApplicationError>,
    > {
        Box::pin(async { Ok(None) })
    }
    fn reserve(
        &self,
        key: CommandReservationKey,
        fingerprint: String,
        request: ApplicationCommandRequest,
    ) -> BoxFuture<'static, Result<RuntimeApplicationReservationAdmission, ApplicationError>>;

    fn mark_dispatch_started(
        &self,
        key: CommandReservationKey,
        fingerprint: String,
    ) -> BoxFuture<'static, Result<(), ApplicationError>>;

    /// Persists an owner-issued effect binding before a physical owner crosses its first effect
    /// boundary. T03/T05 own the concrete binding/receipt producer; this application service
    /// only carries the typed lifecycle transition.
    fn mark_effect_started(
        &self,
        key: CommandReservationKey,
        fingerprint: String,
        binding: CommandEffectBinding,
    ) -> BoxFuture<'static, Result<(), ApplicationError>>;

    /// Records that the executor supplied an exact domain commit reference. The reservation
    /// journal remains a replay index and must not become a second domain commit authority.
    fn mark_domain_committed(
        &self,
        key: CommandReservationKey,
        fingerprint: String,
        receipt: ApplicationDomainReceipt,
    ) -> BoxFuture<'static, Result<(), ApplicationError>>;

    /// Makes an explicit no-effect proof durable without changing it into a successful command.
    fn mark_confirmed_no_effect(
        &self,
        key: CommandReservationKey,
        fingerprint: String,
        proof: CommandNoEffectProof,
    ) -> BoxFuture<'static, Result<(), ApplicationError>>;

    /// Records an outcome that must be reconciled. Recovery never re-dispatches this state.
    fn mark_uncertain(
        &self,
        key: CommandReservationKey,
        fingerprint: String,
        receipt: UncertainCommandReceipt,
    ) -> BoxFuture<'static, Result<(), ApplicationError>>;

    fn settle(
        &self,
        key: CommandReservationKey,
        fingerprint: String,
        receipt: ApplicationCommandReceipt,
    ) -> BoxFuture<'static, Result<(), ApplicationError>>;
}

#[derive(Debug, Clone)]
struct PageRecord {
    request: ProjectionPageRequest,
    result: Option<ProjectionPage>,
    cancelled: bool,
    abort: futures::future::AbortHandle,
}

struct PageLoadGuard {
    state: Arc<Mutex<RuntimeApplicationState>>,
    request_id: PageRequestId,
    complete: bool,
}

impl Drop for PageLoadGuard {
    fn drop(&mut self) {
        if !self.complete
            && let Ok(mut state) = self.state.lock()
            && let Some(record) = state.pages.get_mut(&self.request_id)
        {
            record.cancelled = true;
            record.abort.abort();
        }
    }
}

#[derive(Default)]
struct RuntimeApplicationState {
    pages: BTreeMap<PageRequestId, PageRecord>,
}

/// The single runtime implementation of [`ApplicationPort`].
pub struct RuntimeApplicationService {
    projection: Arc<dyn RuntimeApplicationProjectionSource>,
    executor: Arc<dyn RuntimeApplicationCommandExecutor>,
    reservations: Arc<dyn RuntimeApplicationReservationStore>,
    delivery: Arc<dyn RuntimeApplicationDeliveryAcker>,
    state: Arc<Mutex<RuntimeApplicationState>>,
}

impl fmt::Debug for RuntimeApplicationService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeApplicationService")
            .field("projection", &"<runtime projection source>")
            .field("executor", &"<runtime command executor>")
            .field("reservations", &"<runtime reservation store>")
            .field("delivery", &"<runtime delivery acker>")
            .finish_non_exhaustive()
    }
}

impl RuntimeApplicationService {
    pub fn new(
        projection: Arc<dyn RuntimeApplicationProjectionSource>,
        executor: Arc<dyn RuntimeApplicationCommandExecutor>,
        reservations: Arc<dyn RuntimeApplicationReservationStore>,
        delivery: Arc<dyn RuntimeApplicationDeliveryAcker>,
    ) -> Self {
        Self {
            projection,
            executor,
            reservations,
            delivery,
            state: Arc::new(Mutex::new(RuntimeApplicationState::default())),
        }
    }

    fn uncertain_receipt(
        request: &ApplicationCommandRequest,
        key: CommandReservationKey,
        fingerprint: String,
        phase: CommandLifecyclePhase,
    ) -> ApplicationCommandReceipt {
        ApplicationCommandReceipt::Uncertain(UncertainCommandReceipt {
            command_id: request.envelope.command_id.clone(),
            command_kind: request.envelope.command.kind().to_owned(),
            reservation_fingerprint: fingerprint,
            recovery: CommandRecoveryBinding { key, phase },
            owner_recovery_binding: None,
        })
    }

    fn validate_settled_receipt(
        request: &ApplicationCommandRequest,
        receipt: ApplicationDomainReceipt,
    ) -> Result<ApplicationCommandReceipt, ApplicationError> {
        let key = request
            .admission
            .reservation_key(&request.envelope.command_id);
        if receipt.command_id != request.envelope.command_id
            || receipt.command_kind != request.envelope.command.kind()
            || receipt.frontier.scope != request.admission.scope
            || receipt.settlement != request.envelope.command.policy().settlement
        {
            return Err(ApplicationError::CorruptProjection(
                "runtime command receipt does not match its admitted request".to_owned(),
            ));
        }
        receipt.validate_for(&key)?;
        Ok(ApplicationCommandReceipt::Settled(receipt))
    }

    fn exact_fail_safe_stop(request: &ApplicationCommandRequest) -> bool {
        matches!(
            &request.envelope.command,
            sigil_application::ApplicationCommand::Run(
                sigil_application::RunCommand::CancelTerminalTask { .. }
                    | sigil_application::RunCommand::Cancel { .. }
            )
        )
    }

    async fn stop_after_journal_failure(
        executor: &Arc<dyn RuntimeApplicationCommandExecutor>,
        request: &ApplicationCommandRequest,
        error: ApplicationError,
    ) -> Result<ApplicationCommandReceipt, ApplicationError> {
        if !Self::exact_fail_safe_stop(request)
            || !matches!(
                error,
                ApplicationError::Unavailable | ApplicationError::CorruptProjection(_)
            )
        {
            return Err(error);
        }
        // Only the exact authenticated stop owner can confirm this fail-safe lane. A failed
        // ledger write cannot manufacture a durable receipt, nor authorize a forward effect.
        match executor.request_safety_stop(request.clone()).await? {
            SafetyStopDisposition::ForwardGateClosed => {
                Ok(ApplicationCommandReceipt::SafetyStopRequestedButUnrecorded(
                    SafetyStopRequestedButUnrecorded {
                        command_id: request.envelope.command_id.clone(),
                        command_kind: request.envelope.command.kind().to_owned(),
                        reason: error.to_string(),
                    },
                ))
            }
            SafetyStopDisposition::Uncertain => Err(ApplicationError::Unavailable),
        }
    }

    async fn persist_uncertain_after_dispatch(
        reservations: &Arc<dyn RuntimeApplicationReservationStore>,
        request: &ApplicationCommandRequest,
        key: CommandReservationKey,
        fingerprint: String,
        phase: CommandLifecyclePhase,
    ) -> Result<ApplicationCommandReceipt, ApplicationError> {
        let receipt =
            match Self::uncertain_receipt(request, key.clone(), fingerprint.clone(), phase) {
                ApplicationCommandReceipt::Uncertain(receipt) => receipt,
                _ => unreachable!("uncertain_receipt always returns an uncertain receipt"),
            };
        reservations
            .mark_uncertain(key.clone(), fingerprint.clone(), receipt.clone())
            .await?;
        // The uncertainty record is the durable owner fact. Settlement is only an idempotent
        // replay index; a failure here must not turn the already-recorded uncertainty into a
        // successful command.
        let _ = reservations
            .settle(
                key,
                fingerprint,
                ApplicationCommandReceipt::Uncertain(receipt.clone()),
            )
            .await;
        Ok(ApplicationCommandReceipt::Uncertain(receipt))
    }
}

impl ApplicationPort for RuntimeApplicationService {
    fn recover_control_log(
        &self,
        scope: sigil_application::ApplicationScope,
        action: sigil_application::ControlLogRecoveryAction,
    ) -> BoxFuture<'static, Result<sigil_application::ControlLogRecoveryOutcome, ApplicationError>>
    {
        if let Err(error) = self.projection.validate_recovery_scope(&scope) {
            return Box::pin(async move { Err(error) });
        }
        self.reservations.recover_control_log(action)
    }
    fn command_journal_binding(
        &self,
    ) -> Result<Option<sigil_application::CommandJournalBinding>, ApplicationError> {
        self.reservations.command_journal_binding()
    }
    fn original_command_context(
        &self,
        key: CommandReservationKey,
    ) -> BoxFuture<
        'static,
        Result<Option<sigil_application::OriginalCommandContext>, ApplicationError>,
    > {
        self.reservations.original_command_context(key)
    }

    fn delivery_batch(
        &self,
        request: sigil_application::DurableDeliveryRequest,
    ) -> BoxFuture<'static, Result<sigil_application::DurableDeliveryBatch, ApplicationError>> {
        self.projection.delivery_batch(request)
    }
    fn open_projection(
        &self,
        request: OpenProjectionRequest,
    ) -> BoxFuture<'static, Result<ProjectionSnapshot, ApplicationError>> {
        self.projection.open_projection(request)
    }

    fn page(
        &self,
        request: ProjectionPageRequest,
    ) -> BoxFuture<'static, Result<ProjectionPage, ApplicationError>> {
        let projection = Arc::clone(&self.projection);
        let state = Arc::clone(&self.state);
        Box::pin(async move {
            if request.limit.get() > sigil_application::MAX_PAGE_ITEMS {
                return Err(ApplicationError::InvalidRequest(
                    "page limit exceeds application bound".to_owned(),
                ));
            }
            let (abort, registration) = futures::future::AbortHandle::new_pair();
            {
                let mut state = state.lock().map_err(|_| ApplicationError::Unavailable)?;
                if let Some(record) = state.pages.get(&request.request_id) {
                    if record.request != request {
                        return Err(ApplicationError::ScopeMismatch);
                    }
                    if record.cancelled {
                        return Err(ApplicationError::ResetRequired);
                    }
                    if let Some(result) = &record.result {
                        return Ok(result.clone());
                    }
                    return Err(ApplicationError::Unavailable);
                }
                // Page results are a disposable presentation cache. Keep active requests and a
                // bounded set of completed/cancelled requests instead of retaining every body.
                if state.pages.len() >= 32 {
                    let retired = state
                        .pages
                        .iter()
                        .find(|(_, record)| record.cancelled || record.result.is_some())
                        .map(|(id, _)| id.clone())
                        .ok_or(ApplicationError::Unavailable)?;
                    state.pages.remove(&retired);
                }
                state.pages.insert(
                    request.request_id.clone(),
                    PageRecord {
                        request: request.clone(),
                        result: None,
                        cancelled: false,
                        abort,
                    },
                );
            }

            let mut guard = PageLoadGuard {
                state: Arc::clone(&state),
                request_id: request.request_id.clone(),
                complete: false,
            };
            let result =
                futures::future::Abortable::new(projection.page(request.clone()), registration)
                    .await
                    .map_err(|_| ApplicationError::ResetRequired)?;
            let mut state = state.lock().map_err(|_| ApplicationError::Unavailable)?;
            let record = state
                .pages
                .get_mut(&request.request_id)
                .ok_or(ApplicationError::Unavailable)?;
            if record.cancelled {
                return Err(ApplicationError::ResetRequired);
            }
            let page = result?;
            if page.request_id != request.request_id
                || page.scope != request.scope
                || page.source_generation != request.source_generation
                || page.at_frontier != request.at_frontier
                || page.query != request.query
            {
                return Err(ApplicationError::CorruptProjection(
                    "runtime page response does not match its request".to_owned(),
                ));
            }
            record.result = Some(page.clone());
            guard.complete = true;
            Ok(page)
        })
    }

    fn cancel_page(&self, request: PageRequestId) -> BoxFuture<'static, PageCancellationReceipt> {
        let state = Arc::clone(&self.state);
        Box::pin(async move {
            let Ok(mut state) = state.lock() else {
                return PageCancellationReceipt::UnknownRequest;
            };
            let Some(record) = state.pages.get_mut(&request) else {
                return PageCancellationReceipt::UnknownRequest;
            };
            if record.result.is_some() {
                return PageCancellationReceipt::Completed;
            }
            record.cancelled = true;
            record.abort.abort();
            PageCancellationReceipt::CancelledBeforeLoad
        })
    }

    fn acknowledge(
        &self,
        acknowledgement: ProjectionDeliveryAck,
    ) -> BoxFuture<'static, Result<(), ApplicationError>> {
        if let Err(error) = acknowledgement.validate() {
            return Box::pin(async move { Err(error) });
        }
        self.delivery.acknowledge(acknowledgement)
    }

    fn execute(
        &self,
        request: ApplicationCommandRequest,
    ) -> BoxFuture<'static, Result<ApplicationCommandReceipt, ApplicationError>> {
        self.execute_admitted(request, false)
    }

    fn resume_session_runtime_transition(
        &self,
        request: ApplicationCommandRequest,
    ) -> BoxFuture<'static, Result<ApplicationCommandReceipt, ApplicationError>> {
        self.execute_admitted(request, true)
    }
}

impl RuntimeApplicationService {
    fn execute_admitted(
        &self,
        request: ApplicationCommandRequest,
        resume_runtime_transition: bool,
    ) -> BoxFuture<'static, Result<ApplicationCommandReceipt, ApplicationError>> {
        let executor = Arc::clone(&self.executor);
        let reservations = Arc::clone(&self.reservations);
        Box::pin(async move {
            request.validate()?;
            let resume_binding = if resume_runtime_transition {
                if !matches!(
                    &request.envelope.command,
                    sigil_application::ApplicationCommand::Provider(
                        sigil_application::ProviderCommand::SelectRoute { .. }
                    )
                ) {
                    return Err(ApplicationError::ScopeMismatch);
                }
                Some(
                    executor
                        .session_runtime_resume_binding(request.clone())
                        .await?,
                )
            } else {
                None
            };
            // Fail early for a sealed generation, then release the namespace guard before a
            // journal append takes that same lock. Acquire it again immediately before dispatch.
            if resume_runtime_transition {
                drop(reservations.forward_guard(&request)?);
            }
            let fingerprint = command_fingerprint(&request)?;
            let key = request
                .admission
                .reservation_key(&request.envelope.command_id);
            let admission = match reservations
                .reserve(key.clone(), fingerprint.clone(), request.clone())
                .await
            {
                Ok(admission) => admission,
                Err(error) => {
                    return Self::stop_after_journal_failure(&executor, &request, error).await;
                }
            };
            let unresolved = matches!(
                &admission,
                RuntimeApplicationReservationAdmission::InFlight(_)
            ) || matches!(&admission, RuntimeApplicationReservationAdmission::Existing(receipt)
                    if matches!(receipt.as_ref(), ApplicationCommandReceipt::Uncertain(_)
                        | ApplicationCommandReceipt::ReplayedUncertain(_)));
            // Explicit owner resume also restores the live gate. A read-only historical
            // Activated receipt is not evidence that this physical worker accepts commands.
            if unresolved && !resume_runtime_transition {
                match executor.reconcile(request.clone()).await? {
                    Some(RuntimeApplicationDispatch::Settled(domain)) => {
                        let ApplicationCommandReceipt::Settled(domain) =
                            Self::validate_settled_receipt(&request, domain)?
                        else {
                            return Err(ApplicationError::Unavailable);
                        };
                        // The domain source remains authoritative even if the old command
                        // generation has already been sealed and cannot accept another record.
                        if reservations
                            .mark_domain_committed(key.clone(), fingerprint.clone(), domain.clone())
                            .await
                            .is_ok()
                        {
                            let _ = reservations
                                .settle(
                                    key,
                                    fingerprint,
                                    ApplicationCommandReceipt::Settled(domain.clone()),
                                )
                                .await;
                        }
                        return Ok(ApplicationCommandReceipt::Replayed(domain));
                    }
                    Some(RuntimeApplicationDispatch::ConfirmedNoEffect { proof, .. }) => {
                        proof.validate()?;
                        if proof.source.key != key || proof.reservation_fingerprint != fingerprint {
                            return Err(ApplicationError::ScopeMismatch);
                        }
                        let _ = reservations
                            .mark_confirmed_no_effect(key, fingerprint, proof.clone())
                            .await;
                        return Ok(ApplicationCommandReceipt::ConfirmedNoEffect(proof));
                    }
                    _ => (),
                }
            }
            match admission {
                RuntimeApplicationReservationAdmission::Reserved if resume_runtime_transition => {
                    return Err(ApplicationError::Unavailable);
                }
                RuntimeApplicationReservationAdmission::Reserved => {}
                RuntimeApplicationReservationAdmission::InFlight(_)
                    if resume_runtime_transition => {}
                RuntimeApplicationReservationAdmission::InFlight(receipt) => {
                    return Ok(ApplicationCommandReceipt::InFlight(receipt));
                }
                RuntimeApplicationReservationAdmission::Existing(ref receipt)
                    if resume_runtime_transition
                        && matches!(
                            receipt.as_ref(),
                            ApplicationCommandReceipt::Uncertain(_)
                                | ApplicationCommandReceipt::ReplayedUncertain(_)
                                | ApplicationCommandReceipt::Settled(_)
                                | ApplicationCommandReceipt::Replayed(_)
                        ) => {}
                RuntimeApplicationReservationAdmission::Existing(receipt) => {
                    return Ok(match *receipt {
                        ApplicationCommandReceipt::Settled(domain) => {
                            ApplicationCommandReceipt::Replayed(domain)
                        }
                        ApplicationCommandReceipt::Uncertain(receipt) => {
                            ApplicationCommandReceipt::ReplayedUncertain(receipt)
                        }
                        receipt => receipt,
                    });
                }
                RuntimeApplicationReservationAdmission::Conflict(conflict) => {
                    return Ok(ApplicationCommandReceipt::PayloadConflict(conflict));
                }
            }

            if let Some(binding) = resume_binding {
                binding.validate()?;
                if binding.recovery.key != key
                    || binding.reservation_fingerprint != fingerprint
                    || binding.command_id != request.envelope.command_id
                    || binding.command_kind != request.envelope.command.kind()
                    || binding.recovery.phase != CommandLifecyclePhase::EffectStarted
                {
                    return Err(ApplicationError::ScopeMismatch);
                }
                reservations
                    .resume_session_runtime_effect(key.clone(), fingerprint.clone(), binding)
                    .await?;
            } else {
                if let Err(error) = reservations
                    .mark_dispatch_started(key.clone(), fingerprint.clone())
                    .await
                {
                    return Self::stop_after_journal_failure(&executor, &request, error).await;
                }
                let effect_binding = match executor
                    .bind_effect(request.clone(), key.clone(), fingerprint.clone())
                    .await
                {
                    Ok(binding) => binding,
                    Err(_) => {
                        return Self::persist_uncertain_after_dispatch(
                            &reservations,
                            &request,
                            key,
                            fingerprint,
                            CommandLifecyclePhase::DispatchStarted,
                        )
                        .await;
                    }
                };
                if effect_binding.validate().is_err() {
                    return Self::persist_uncertain_after_dispatch(
                        &reservations,
                        &request,
                        key,
                        fingerprint,
                        CommandLifecyclePhase::DispatchStarted,
                    )
                    .await;
                }
                if effect_binding.recovery.key != key
                    || effect_binding.reservation_fingerprint != fingerprint
                {
                    return Self::persist_uncertain_after_dispatch(
                        &reservations,
                        &request,
                        key,
                        fingerprint,
                        CommandLifecyclePhase::DispatchStarted,
                    )
                    .await;
                }
                if let Err(error) = reservations
                    .mark_effect_started(key.clone(), fingerprint.clone(), effect_binding)
                    .await
                {
                    return Self::stop_after_journal_failure(&executor, &request, error).await;
                }
            }

            let forward_guard = match reservations.forward_guard(&request) {
                Ok(guard) => guard,
                Err(error) => {
                    return Self::stop_after_journal_failure(&executor, &request, error).await;
                }
            };
            let dispatch = executor.dispatch(request.clone()).await;
            drop(forward_guard);
            let outcome = match dispatch {
                Ok(RuntimeApplicationDispatch::Settled(receipt)) => {
                    match Self::validate_settled_receipt(&request, receipt) {
                        Ok(receipt) => receipt,
                        Err(_) => Self::uncertain_receipt(
                            &request,
                            key.clone(),
                            fingerprint.clone(),
                            CommandLifecyclePhase::EffectStarted,
                        ),
                    }
                }
                Ok(RuntimeApplicationDispatch::ConfirmedNoEffect { rejection, proof }) => {
                    proof.validate()?;
                    if proof.source.key != key || proof.reservation_fingerprint != fingerprint {
                        return Err(ApplicationError::ScopeMismatch);
                    }
                    reservations
                        .mark_confirmed_no_effect(key.clone(), fingerprint.clone(), proof.clone())
                        .await?;
                    let rejection = ApplicationCommandReceipt::Rejected(rejection);
                    if reservations
                        .settle(key, fingerprint, rejection.clone())
                        .await
                        .is_err()
                    {
                        return Ok(ApplicationCommandReceipt::ConfirmedNoEffect(proof));
                    }
                    return Ok(rejection);
                }
                // A bare rejection has no owner-issued proof that it remained pre-effect. Keep
                // it in the same recovery lane as an executor transport failure.
                Ok(RuntimeApplicationDispatch::Rejected(_)) | Err(_) => Self::uncertain_receipt(
                    &request,
                    key.clone(),
                    fingerprint.clone(),
                    CommandLifecyclePhase::EffectStarted,
                ),
                Ok(RuntimeApplicationDispatch::Uncertain(receipt)) => {
                    if receipt.validate().is_err()
                        || receipt.recovery.key != key
                        || receipt.reservation_fingerprint != fingerprint
                        || receipt.recovery.phase != CommandLifecyclePhase::EffectStarted
                    {
                        Self::uncertain_receipt(
                            &request,
                            key.clone(),
                            fingerprint.clone(),
                            CommandLifecyclePhase::EffectStarted,
                        )
                    } else {
                        ApplicationCommandReceipt::Uncertain(receipt)
                    }
                }
            };

            match &outcome {
                ApplicationCommandReceipt::Settled(receipt) => {
                    reservations
                        .mark_domain_committed(key.clone(), fingerprint.clone(), receipt.clone())
                        .await?;
                    if reservations
                        .settle(key.clone(), fingerprint.clone(), outcome.clone())
                        .await
                        .is_err()
                    {
                        // The domain receipt is already verified and durable. The application
                        // settlement append is only a replay index, so surface the true terminal
                        // and let the next same-key request replay it without another dispatch.
                        return Ok(outcome.clone());
                    }
                }
                ApplicationCommandReceipt::Uncertain(receipt) => {
                    reservations
                        .mark_uncertain(key.clone(), fingerprint.clone(), receipt.clone())
                        .await?;
                    if reservations
                        .settle(
                            key,
                            fingerprint,
                            ApplicationCommandReceipt::Uncertain(receipt.clone()),
                        )
                        .await
                        .is_err()
                    {
                        // `mark_uncertain` is already durable, so returning its typed receipt is
                        // truthful and fail-closed even if the replay index finalization races a
                        // later storage fault.
                        return Ok(ApplicationCommandReceipt::Uncertain(receipt.clone()));
                    }
                }
                _ => {
                    return Err(ApplicationError::CorruptProjection(
                        "runtime dispatch produced an unsupported command receipt".to_owned(),
                    ));
                }
            }
            Ok(outcome)
        })
    }
}

#[cfg(test)]
#[path = "tests/application_service_tests.rs"]
mod tests;
