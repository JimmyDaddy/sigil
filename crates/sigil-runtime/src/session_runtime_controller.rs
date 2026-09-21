//! Session attachment controller for durable route transitions. Platform hosts retain all
//! worker handles; this controller alone orders intent, configuration, Ready and activation.

use crate::{
    RuntimeApplicationDispatch,
    interactive_session_attachment::InteractiveSessionAttachmentLease,
    provider_connections::{
        ResolvedRouteConfigSnapshot, SessionRouteMutationPermit, apply_session_runtime_transition,
    },
};
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use sigil_application::{
    ApplicationCommandRequest, ApplicationDomainCommitRef, ApplicationDomainReceipt,
    ApplicationFrontier, CommandEffectBinding, CommandLifecyclePhase, CommandRecoveryBinding,
    CommandReservationKey, command_fingerprint,
};
use sigil_kernel::{
    ControlEntry, JsonlSessionStore, ResolvedModelRoute, RootConfig, Session, SessionLogEntry,
    SessionRuntimeCommandCauseV1, SessionRuntimeReadyV1, SessionRuntimeTransitionIntentV1,
    SessionRuntimeTransitionPhaseV1, SessionRuntimeTransitionV1, StoredEvent,
    session_runtime_transition::{
        SessionRuntimeTransitionReplayV1, session_runtime_route_revision,
    },
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

/// Platform actions must keep every unfinished worker owner after failure. `ready` returns only
/// the identity received from the actual worker's private startup channel.
pub trait RuntimeSessionWorkerHost: Send + Sync {
    fn close_gate(&self) -> Result<()>;
    fn stop(&self) -> Result<()>;
    fn ready(
        &self,
        config: RootConfig,
        expected: &SessionRuntimeReadyV1,
    ) -> Result<SessionRuntimeReadyV1>;
    fn open_gate(&self, ready: &SessionRuntimeReadyV1) -> Result<()>;
    fn is_live(&self, ready: &SessionRuntimeReadyV1) -> Result<bool>;
}

/// Expected interruption when the attachment owner has requested application exit.
/// Hosts may distinguish this from a failed durable transition without parsing error text.
#[derive(Debug, thiserror::Error)]
#[error("runtime controller is exiting")]
pub struct RuntimeSessionControllerExiting;

/// One attachment-scoped transition owner. It retains the original request binding and
/// worker generation across platform stop/start actions; it never issues physical resources.
pub struct RuntimeSessionController {
    store: JsonlSessionStore,
    attachment: Arc<InteractiveSessionAttachmentLease>,
    config: RootConfig,
    host: Arc<dyn RuntimeSessionWorkerHost>,
    generation: AtomicU64,
    exit: Arc<AtomicBool>,
    permit: Mutex<Option<SessionRouteMutationPermit>>,
    serial: Mutex<()>,
}

impl RuntimeSessionController {
    /// Installs a trusted platform host for the already admitted session attachment. The supplied
    /// generation is the next unused live worker generation, including failed startup attempts.
    pub fn new(
        store: JsonlSessionStore,
        attachment: Arc<InteractiveSessionAttachmentLease>,
        config: RootConfig,
        host: Arc<dyn RuntimeSessionWorkerHost>,
        generation: u64,
        exit: Arc<AtomicBool>,
    ) -> Result<Self> {
        anyhow::ensure!(
            attachment.session_path() == store.path(),
            "runtime controller attachment mismatch"
        );
        Ok(Self {
            store,
            attachment,
            config,
            host,
            generation: AtomicU64::new(generation),
            exit,
            permit: Mutex::new(None),
            serial: Mutex::new(()),
        })
    }

    /// Stable operation identity derived from K; F is separately verified against its Intent.
    pub fn operation_id(request: &ApplicationCommandRequest) -> Result<String> {
        let cause = Self::cause(request)?;
        Ok(format!(
            "route-{:x}",
            Sha256::digest(serde_json::to_vec(&(
                cause.application_instance,
                cause.workspace_scope,
                cause.session_scope,
                cause.principal,
                cause.client_epoch,
                cause.command_id
            ))?)
        ))
    }

    /// Next unused process-local worker generation, including failed startup attempts.
    pub fn next_worker_generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    fn cause(request: &ApplicationCommandRequest) -> Result<SessionRuntimeCommandCauseV1> {
        let scope = &request.admission.scope;
        Ok(SessionRuntimeCommandCauseV1 {
            application_instance: scope.application_instance.to_string(),
            workspace_scope: scope.workspace.as_ref().map(ToString::to_string),
            session_scope: scope
                .session
                .as_ref()
                .context("route command requires session")?
                .to_string(),
            principal: request.admission.principal.to_string(),
            client_epoch: request.admission.client_epoch,
            command_id: request.envelope.command_id.to_string(),
            reservation_fingerprint: command_fingerprint(request)?,
        })
    }

    fn replay(
        &self,
        request: &ApplicationCommandRequest,
    ) -> Result<SessionRuntimeTransitionReplayV1> {
        let replay = SessionRuntimeTransitionReplayV1::from_records(
            &self.store.read_event_records_writer()?,
            &Self::operation_id(request)?,
        )?;
        if let Some((intent, _)) = &replay.intent {
            anyhow::ensure!(
                intent.cause == Self::cause(request)?,
                "runtime command cause mismatch"
            );
        }
        Ok(replay)
    }

    /// Durable intent is the owner claim, before any old worker is stopped. Acquiring the
    /// attachment's original permit fences real execution owners, including background work.
    pub fn bind_effect(
        &self,
        request: &ApplicationCommandRequest,
        route: &ResolvedModelRoute,
        key: CommandReservationKey,
        fingerprint: String,
    ) -> Result<CommandEffectBinding> {
        let _serial = self
            .serial
            .lock()
            .map_err(|_| anyhow::anyhow!("runtime controller poisoned"))?;
        self.check_exit()?;
        let cause = Self::cause(request)?;
        anyhow::ensure!(
            cause.reservation_fingerprint == fingerprint,
            "route fingerprint mismatch"
        );
        let operation_id = Self::operation_id(request)?;
        let replay = self.replay(request)?;
        let intent_record = if let Some((intent, record)) = replay.intent {
            anyhow::ensure!(&intent.target_route == route, "route retry target mismatch");
            record
        } else {
            let permit = self
                .attachment
                .route_mutation_authority(&cause.session_scope)?
                .issue_quiescence_permit()?;
            let records = self.store.read_event_records_writer()?;
            let current = Session::load_from_store_for_control(self.store.clone())?;
            anyhow::ensure!(
                current.session_scope_id() == cause.session_scope,
                "route source session mismatch"
            );
            let snapshot = ResolvedRouteConfigSnapshot::from_root_config(&self.config);
            let (provider, target, trust) = snapshot
                .resolved_route(&route.model_ref)
                .context("selected route unavailable")?;
            anyhow::ensure!(&target == route, "selected route configuration changed");
            let source = current
                .resolved_model_route()
                .context("source route missing")?
                .clone();
            let source_trust = current.route_egress_trust_binding();
            let reset_private_context = source != target;
            let binding = SessionRuntimeTransitionIntentV1 {
                cause,
                source_frontier_sequence: records
                    .last()
                    .map_or(0, sigil_kernel::SessionStreamRecord::stream_sequence),
                source_route_revision: session_runtime_route_revision(&records)?,
                source_route: source,
                source_trust,
                target_provider: provider,
                target_route: target.clone(),
                target_trust: trust.clone(),
                configuration_fingerprint: format!(
                    "{:x}",
                    Sha256::digest(serde_json::to_vec(&(&target, &trust))?)
                ),
                reset_private_context,
            };
            let record = self.append(
                operation_id,
                SessionRuntimeTransitionPhaseV1::Intent {
                    binding: Box::new(binding),
                },
            )?;
            *self
                .permit
                .lock()
                .map_err(|_| anyhow::anyhow!("runtime permit poisoned"))? = Some(permit);
            record
        };
        let binding = CommandEffectBinding {
            command_id: request.envelope.command_id.clone(),
            command_kind: request.envelope.command.kind().to_owned(),
            reservation_fingerprint: fingerprint,
            recovery: CommandRecoveryBinding {
                key,
                phase: CommandLifecyclePhase::EffectStarted,
            },
            owner_effect_id: intent_record.event_id,
        };
        binding.validate()?;
        Ok(binding)
    }

    /// Returns the original historical Activated commit, independently of current live readiness.
    /// This read-only operation never opens a gate, starts a worker or infers a commit from values.
    pub fn reconcile(
        &self,
        request: &ApplicationCommandRequest,
    ) -> Result<Option<RuntimeApplicationDispatch>> {
        Ok(self
            .replay(request)?
            .activated
            .map(|(_, event)| Self::receipt(request, event)))
    }

    /// Reads the existing durable Intent for an explicit same-operation retry. This method
    /// never issues a permit or creates a replacement Intent when the original is absent.
    pub fn resume_effect_binding(
        &self,
        request: &ApplicationCommandRequest,
    ) -> Result<CommandEffectBinding> {
        self.check_exit()?;
        let (_, record) = self
            .replay(request)?
            .intent
            .context("runtime transition has no original Intent to resume")?;
        let binding = CommandEffectBinding {
            command_id: request.envelope.command_id.clone(),
            command_kind: request.envelope.command.kind().to_owned(),
            reservation_fingerprint: sigil_application::command_fingerprint(request)?,
            recovery: CommandRecoveryBinding {
                key: request
                    .admission
                    .reservation_key(&request.envelope.command_id),
                phase: CommandLifecyclePhase::EffectStarted,
            },
            owner_effect_id: record.event_id,
        };
        binding.validate()?;
        Ok(binding)
    }

    /// Reads whether this exact operation has an Intent; errors remain unknown, not absence.
    pub fn has_bound_intent(&self, request: &ApplicationCommandRequest) -> Result<bool> {
        Ok(self.replay(request)?.intent.is_some())
    }

    /// Advances this owner's original operation and settles only after matching Ready, durable
    /// Activated and an open live gate. Retrying a Configured operation keeps its original source.
    pub fn dispatch(
        &self,
        request: &ApplicationCommandRequest,
    ) -> Result<RuntimeApplicationDispatch> {
        let _serial = self
            .serial
            .lock()
            .map_err(|_| anyhow::anyhow!("runtime controller poisoned"))?;
        self.check_exit()?;
        let replay = self.replay(request)?;
        if let Some((ready, event)) = replay.activated {
            if !self.host.is_live(&ready)? {
                self.host.open_gate(&ready)?;
            }
            return Ok(Self::receipt(request, event));
        }
        let (intent, _) = replay.intent.context("route intent missing")?;
        let operation_id = Self::operation_id(request)?;
        let permit = self
            .permit
            .lock()
            .map_err(|_| anyhow::anyhow!("runtime permit poisoned"))?
            .take()
            .map(Ok)
            .unwrap_or_else(|| {
                self.attachment
                    .route_mutation_authority(&intent.cause.session_scope)?
                    .issue_quiescence_permit()
                    .map_err(anyhow::Error::new)
            })?;
        self.host.close_gate()?;
        self.host.stop()?;
        self.check_exit()?;
        apply_session_runtime_transition(
            &self.store,
            &operation_id,
            &intent.cause.session_scope,
            permit,
        )?;
        let replay = self.replay(request)?;
        let (revision, _) = replay
            .configured
            .context("route configured marker missing")?;
        let mut config = self.config.clone();
        config.agent.runtime_provider.clear();
        config.agent.connection = Some(intent.target_route.model_ref.connection_id.clone());
        config.agent.model = intent.target_route.model_ref.model_id.clone();
        let config = config.with_effective_composition()?;
        let (provider, route, trust) = ResolvedRouteConfigSnapshot::from_root_config(&config)
            .resolved_route(&intent.target_route.model_ref)
            .context("configured runtime route is unavailable")?;
        anyhow::ensure!(
            provider == intent.target_provider
                && route == intent.target_route
                && trust == intent.target_trust,
            "runtime configuration changed after the original Intent"
        );
        let expected = SessionRuntimeReadyV1 {
            operation_id: operation_id.clone(),
            route_revision: revision,
            worker_generation: self
                .generation
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |generation| {
                    generation.checked_add(1)
                })
                .map_err(|_| anyhow::anyhow!("runtime worker generation exhausted"))?,
            boot_identity: uuid::Uuid::new_v4().to_string(),
        };
        self.check_exit()?;
        let ready = match self.host.ready(config, &expected) {
            Ok(ready) if ready == expected => ready,
            Ok(_) => {
                self.host.stop()?;
                anyhow::bail!("runtime Ready identity mismatch");
            }
            Err(error) => {
                return Err(match self.host.stop() {
                    Ok(()) => error,
                    Err(cleanup) => error.context(cleanup),
                });
            }
        };
        if let Err(error) = self.check_exit() {
            self.host.stop()?;
            return Err(error);
        }
        let activated =
            match sigil_kernel::session_runtime_transition::commit_session_runtime_activation(
                &self.store,
                &operation_id,
                ready.clone(),
            ) {
                Ok(event) => event,
                Err(error) => {
                    self.host.stop()?;
                    return Err(error);
                }
            };
        if let Err(error) = self.check_exit() {
            self.host.stop()?;
            return Err(error);
        }
        self.host.open_gate(&ready)?;
        Ok(Self::receipt(request, activated))
    }

    fn append(
        &self,
        operation_id: String,
        transition: SessionRuntimeTransitionPhaseV1,
    ) -> Result<StoredEvent> {
        self.store
            .append_session_entry_event(&SessionLogEntry::Control(
                ControlEntry::SessionRuntimeTransitionV1(SessionRuntimeTransitionV1 {
                    schema_version: 1,
                    operation_id,
                    transition,
                }),
            ))
    }

    fn check_exit(&self) -> Result<()> {
        if self.exit.load(Ordering::Acquire) {
            return Err(RuntimeSessionControllerExiting.into());
        }
        Ok(())
    }

    fn receipt(
        request: &ApplicationCommandRequest,
        event: StoredEvent,
    ) -> RuntimeApplicationDispatch {
        RuntimeApplicationDispatch::Settled(ApplicationDomainReceipt {
            command_id: request.envelope.command_id.clone(),
            command_kind: request.envelope.command.kind().to_owned(),
            frontier: ApplicationFrontier {
                schema_version: sigil_application::APPLICATION_CONTRACT_SCHEMA_VERSION,
                scope: request.admission.scope.clone(),
                writer_generation: request.envelope.expected_frontier.writer_generation,
                stream_generation: 1,
                through_sequence: event.stream_sequence,
                durable_cursor: format!("session-stream:{}", event.stream_sequence),
            },
            settlement: request.envelope.command.policy().settlement,
            summary: "session runtime activated".to_owned(),
            domain_commit: ApplicationDomainCommitRef {
                source_session_scope_id: request
                    .admission
                    .scope
                    .session
                    .as_ref()
                    .map(ToString::to_string),
                source_event_id: event.event_id,
                source_sequence: event.stream_sequence,
                source_digest: sigil_kernel::sha256_hex(event.record_checksum.as_bytes()),
            },
            outcome: None,
        })
    }
}

#[cfg(test)]
#[path = "tests/session_runtime_controller_tests.rs"]
mod tests;
