//! Durable application command reservations backed by the R71 managed storage writer.

use std::{
    collections::BTreeMap,
    fmt,
    sync::{Arc, Mutex},
};

use futures::future::{BoxFuture, ready};
use serde::{Deserialize, Serialize};
use sigil_application::{
    ApplicationCommandReceipt, ApplicationCommandRequest, ApplicationDomainReceipt,
    ApplicationError, ApplicationInFlightReceipt, CommandConflict, CommandEffectBinding,
    CommandLifecyclePhase, CommandNoEffectProof, CommandRecoveryBinding, CommandReservationKey,
    UncertainCommandReceipt,
};

use crate::{
    RuntimeApplicationReservationAdmission, RuntimeApplicationReservationStore,
    managed_storage_writer::{
        ManagedStorageWriterAdapterV1, ManagedStorageWriterLeaseV1, StorageWriterChannelV1,
    },
};

const APPLICATION_RESERVATION_SCHEMA_VERSION: u16 = 3;

mod index;
mod recovery;
use index::ReservationIndex;

/// Admission acquired during a fallible operation. Dropping it releases the live holder,
/// retaining the physical storage charge even when the journal tail cannot be finalized.
struct PendingNamespace {
    writer: Arc<ManagedStorageWriterAdapterV1>,
    lease: Option<ManagedStorageWriterLeaseV1>,
}

impl PendingNamespace {
    fn acquire(
        writer: &Arc<ManagedStorageWriterAdapterV1>,
        channel: StorageWriterChannelV1,
        key: &str,
    ) -> Result<Self, ApplicationError> {
        let lease = writer
            .acquire_named(channel, key)
            .map_err(|_| ApplicationError::Unavailable)?;
        Ok(Self {
            writer: Arc::clone(writer),
            lease: Some(lease),
        })
    }
    fn get(&self) -> Result<&ManagedStorageWriterLeaseV1, ApplicationError> {
        self.lease.as_ref().ok_or(ApplicationError::Unavailable)
    }
    fn take(mut self) -> Result<ManagedStorageWriterLeaseV1, ApplicationError> {
        self.lease.take().ok_or(ApplicationError::Unavailable)
    }
}
impl Drop for PendingNamespace {
    fn drop(&mut self) {
        if let Some(lease) = self.lease.take() {
            let _ = self.writer.detach(lease);
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum DurableReservationState {
    Reserved,
    DispatchStarted,
    EffectStarted(Box<CommandEffectBinding>),
    DomainCommitted(Box<ApplicationDomainReceipt>),
    ConfirmedNoEffect(Box<CommandNoEffectProof>),
    Uncertain(Box<UncertainCommandReceipt>),
    Settled(Box<ApplicationCommandReceipt>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DurableReservationJournalEntry {
    schema_version: u16,
    operation: DurableReservationOperation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
enum DurableReservationOperation {
    Reserve {
        key: CommandReservationKey,
        fingerprint: String,
    },
    ReserveWithContextV1 {
        key: CommandReservationKey,
        fingerprint: String,
        request: Box<ApplicationCommandRequest>,
    },
    DispatchStarted {
        key: CommandReservationKey,
        fingerprint: String,
    },
    EffectStarted {
        key: CommandReservationKey,
        fingerprint: String,
        binding: Box<CommandEffectBinding>,
    },
    SessionRuntimeEffectResumedV1 {
        key: CommandReservationKey,
        fingerprint: String,
        binding: Box<CommandEffectBinding>,
    },
    DomainCommitted {
        key: CommandReservationKey,
        fingerprint: String,
        receipt: Box<ApplicationDomainReceipt>,
    },
    ConfirmedNoEffect {
        key: CommandReservationKey,
        fingerprint: String,
        proof: Box<CommandNoEffectProof>,
    },
    Uncertain {
        key: CommandReservationKey,
        fingerprint: String,
        receipt: Box<UncertainCommandReceipt>,
    },
    Settled {
        key: CommandReservationKey,
        fingerprint: String,
        receipt: Box<ApplicationCommandReceipt>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ReservationRecord {
    fingerprint: String,
    state: DurableReservationState,
    #[serde(default)]
    request: Option<Box<ApplicationCommandRequest>>,
    #[serde(default)]
    effect_binding: Option<Box<CommandEffectBinding>>,
}

/// Production application reservation authority for one managed application-control namespace.
pub struct ManagedApplicationReservationStore {
    writer: Arc<ManagedStorageWriterAdapterV1>,
    lease: Mutex<Option<ManagedStorageWriterLeaseV1>>,
    index_lease: Mutex<Option<ManagedStorageWriterLeaseV1>>,
    entries: Mutex<ReservationIndex>,
    recovery_lease: Mutex<Option<ManagedStorageWriterLeaseV1>>,
    logical_key: String,
    logical_journal_id: sigil_kernel::resource::CanonicalHash,
    generation: std::sync::atomic::AtomicU64,
}

impl fmt::Debug for ManagedApplicationReservationStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ManagedApplicationReservationStore")
            .field("writer", &"<managed storage writer>")
            .field("lease", &"<redacted>")
            .field("entries", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl ManagedApplicationReservationStore {
    pub fn open(
        writer: Arc<ManagedStorageWriterAdapterV1>,
        key: &str,
    ) -> Result<Self, ApplicationError> {
        let logical_journal_id = ManagedStorageWriterAdapterV1::named_namespace_digest(
            StorageWriterChannelV1::ApplicationControlLog,
            key,
        );
        let recovery_lease = PendingNamespace::acquire(
            &writer,
            StorageWriterChannelV1::ApplicationControlRecovery,
            key,
        )?;
        let recovery_state = writer
            .control_log_recovery_state(recovery_lease.get()?)
            .map_err(|_| ApplicationError::Unavailable)?;
        let generation = recovery_state.as_ref().map_or(0, |state| {
            if state.phase == sigil_kernel::managed_storage::ControlLogRecoveryPhaseV1::Activated {
                state.preview.request.successor_generation
            } else {
                state.preview.request.from_generation
            }
        });
        let epoch_key = recovery::generation_key(key, generation);
        let lease = PendingNamespace::acquire(
            &writer,
            StorageWriterChannelV1::ApplicationControlLog,
            &epoch_key,
        )?;
        let index_lease = PendingNamespace::acquire(
            &writer,
            StorageWriterChannelV1::ApplicationCommandIndex,
            &epoch_key,
        )?;
        let binding = sigil_application::CommandJournalBinding {
            logical_journal_id: logical_journal_id.to_hex(),
            command_generation: generation,
        };
        let recovering = recovery_state.is_some_and(|state| {
            state.phase != sigil_kernel::managed_storage::ControlLogRecoveryPhaseV1::Activated
        });
        let entries = ReservationIndex::open(
            &writer,
            lease.get()?,
            index_lease.get()?,
            binding,
            recovering,
        )?;
        Ok(Self {
            writer,
            lease: Mutex::new(Some(lease.take()?)),
            index_lease: Mutex::new(Some(index_lease.take()?)),
            entries: Mutex::new(entries),
            recovery_lease: Mutex::new(Some(recovery_lease.take()?)),
            logical_key: key.to_owned(),
            logical_journal_id,
            generation: std::sync::atomic::AtomicU64::new(generation),
        })
    }

    fn append_operation(
        &self,
        entries: &mut ReservationIndex,
        operation: DurableReservationOperation,
    ) -> Result<(), ApplicationError> {
        let lease = self
            .lease
            .lock()
            .map_err(|_| ApplicationError::Unavailable)?;
        let index_lease = self
            .index_lease
            .lock()
            .map_err(|_| ApplicationError::Unavailable)?;
        entries.append(
            &self.writer,
            lease.as_ref().ok_or(ApplicationError::Unavailable)?,
            index_lease.as_ref().ok_or(ApplicationError::Unavailable)?,
            operation,
        )
    }

    fn transition(&self, operation: DurableReservationOperation) -> Result<(), ApplicationError> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| ApplicationError::Unavailable)?;
        self.append_operation(&mut entries, operation)
    }
}

impl RuntimeApplicationReservationStore for ManagedApplicationReservationStore {
    fn recover_control_log(
        &self,
        action: sigil_application::ControlLogRecoveryAction,
    ) -> BoxFuture<'static, Result<sigil_application::ControlLogRecoveryOutcome, ApplicationError>>
    {
        use sigil_application::{
            ControlLogRecoveryAction as Action, ControlLogRecoveryOutcome as Outcome,
        };
        let result = match action {
            Action::Preview => self
                .preview_control_log_recovery()
                .map(Box::new)
                .map(Outcome::Preview),
            Action::SealAndRotate { preview } => self
                .seal_and_rotate_control_log(&preview)
                .map(Outcome::Activated),
        };
        Box::pin(ready(result))
    }
    fn forward_guard(
        &self,
        request: &ApplicationCommandRequest,
    ) -> Result<
        Option<Box<dyn sigil_kernel::managed_storage::ManagedStorageForwardGuardV1>>,
        ApplicationError,
    > {
        let entries = self
            .entries
            .lock()
            .map_err(|_| ApplicationError::Unavailable)?;
        if !entries.is_writable() {
            return Err(ApplicationError::Unavailable);
        }
        let generation = request
            .admission
            .command_journal
            .as_ref()
            .map_or(0, |binding| binding.command_generation);
        if generation != self.binding().command_generation {
            return Err(ApplicationError::ScopeMismatch);
        }
        let lease = self
            .lease
            .lock()
            .map_err(|_| ApplicationError::Unavailable)?;
        self.writer
            .command_forward_guard(lease.as_ref().ok_or(ApplicationError::Unavailable)?)
            .map(Some)
            .map_err(|_| ApplicationError::Unavailable)
    }
    fn command_journal_binding(
        &self,
    ) -> Result<Option<sigil_application::CommandJournalBinding>, ApplicationError> {
        Ok(Some(self.binding()))
    }

    fn original_command_context(
        &self,
        key: CommandReservationKey,
    ) -> BoxFuture<
        'static,
        Result<Option<sigil_application::OriginalCommandContext>, ApplicationError>,
    > {
        let result = (|| {
            key.validate()?;
            let entries = self
                .entries
                .lock()
                .map_err(|_| ApplicationError::Unavailable)?;
            match self.find_record(&entries, &key)? {
                Some((generation, record)) => record
                    .request
                    .map(|request| {
                        Some(sigil_application::OriginalCommandContext {
                            expected_frontier: request.envelope.expected_frontier,
                            command_journal: Some(sigil_application::CommandJournalBinding {
                                logical_journal_id: self.logical_journal_id.to_hex(),
                                command_generation: generation,
                            }),
                        })
                    })
                    .ok_or_else(|| {
                        ApplicationError::InvalidRequest(
                            "original command context is unknown; supply the original request"
                                .to_owned(),
                        )
                    }),
                None => Ok(None),
            }
        })();
        Box::pin(ready(result))
    }

    fn reserve(
        &self,
        key: CommandReservationKey,
        fingerprint: String,
        request: ApplicationCommandRequest,
    ) -> BoxFuture<'static, Result<RuntimeApplicationReservationAdmission, ApplicationError>> {
        let result = (|| {
            let mut entries = self
                .entries
                .lock()
                .map_err(|_| ApplicationError::Unavailable)?;
            let requested_generation = match &request.admission.command_journal {
                Some(binding) if binding.logical_journal_id == self.logical_journal_id.to_hex() => {
                    binding.command_generation
                }
                Some(_) => return Err(ApplicationError::ScopeMismatch),
                None => 0,
            };
            let current_generation = self.generation.load(std::sync::atomic::Ordering::Acquire);
            if requested_generation > current_generation {
                return Err(ApplicationError::ScopeMismatch);
            }
            let found = if requested_generation == current_generation {
                self.find_record(&entries, &key)?
            } else {
                self.historical_record(requested_generation, &key)?
                    .map(|record| (requested_generation, record))
            };
            let Some((_generation, record)) = found else {
                if requested_generation != current_generation {
                    return Err(ApplicationError::Unavailable);
                }
                self.append_operation(
                    &mut entries,
                    DurableReservationOperation::ReserveWithContextV1 {
                        key,
                        fingerprint,
                        request: Box::new(request),
                    },
                )?;
                return Ok(RuntimeApplicationReservationAdmission::Reserved);
            };
            if record.fingerprint != fingerprint {
                return Ok(RuntimeApplicationReservationAdmission::Conflict(
                    CommandConflict {
                        command_id: request.envelope.command_id,
                        original_fingerprint: record.fingerprint,
                        received_fingerprint: fingerprint,
                    },
                ));
            }
            Ok(existing_admission(
                &record,
                &key,
                request.envelope.command_id,
                request.envelope.command.kind().to_owned(),
                fingerprint,
            ))
        })();
        Box::pin(ready(result))
    }

    fn mark_dispatch_started(
        &self,
        key: CommandReservationKey,
        fingerprint: String,
    ) -> BoxFuture<'static, Result<(), ApplicationError>> {
        Box::pin(ready(self.transition(
            DurableReservationOperation::DispatchStarted { key, fingerprint },
        )))
    }

    fn resume_session_runtime_effect(
        &self,
        key: CommandReservationKey,
        fingerprint: String,
        binding: CommandEffectBinding,
    ) -> BoxFuture<'static, Result<(), ApplicationError>> {
        Box::pin(ready(self.transition(
            DurableReservationOperation::SessionRuntimeEffectResumedV1 {
                key,
                fingerprint,
                binding: Box::new(binding),
            },
        )))
    }

    fn mark_effect_started(
        &self,
        key: CommandReservationKey,
        fingerprint: String,
        binding: CommandEffectBinding,
    ) -> BoxFuture<'static, Result<(), ApplicationError>> {
        Box::pin(ready(self.transition(
            DurableReservationOperation::EffectStarted {
                key,
                fingerprint,
                binding: Box::new(binding),
            },
        )))
    }

    fn mark_domain_committed(
        &self,
        key: CommandReservationKey,
        fingerprint: String,
        receipt: ApplicationDomainReceipt,
    ) -> BoxFuture<'static, Result<(), ApplicationError>> {
        Box::pin(ready(self.transition(
            DurableReservationOperation::DomainCommitted {
                key,
                fingerprint,
                receipt: Box::new(receipt),
            },
        )))
    }

    fn mark_confirmed_no_effect(
        &self,
        key: CommandReservationKey,
        fingerprint: String,
        proof: CommandNoEffectProof,
    ) -> BoxFuture<'static, Result<(), ApplicationError>> {
        Box::pin(ready(self.transition(
            DurableReservationOperation::ConfirmedNoEffect {
                key,
                fingerprint,
                proof: Box::new(proof),
            },
        )))
    }

    fn mark_uncertain(
        &self,
        key: CommandReservationKey,
        fingerprint: String,
        receipt: UncertainCommandReceipt,
    ) -> BoxFuture<'static, Result<(), ApplicationError>> {
        Box::pin(ready(self.transition(
            DurableReservationOperation::Uncertain {
                key,
                fingerprint,
                receipt: Box::new(receipt),
            },
        )))
    }

    fn settle(
        &self,
        key: CommandReservationKey,
        fingerprint: String,
        receipt: ApplicationCommandReceipt,
    ) -> BoxFuture<'static, Result<(), ApplicationError>> {
        Box::pin(ready(self.transition(
            DurableReservationOperation::Settled {
                key,
                fingerprint,
                receipt: Box::new(receipt),
            },
        )))
    }
}

impl ManagedApplicationReservationStore {
    fn find_record(
        &self,
        current: &ReservationIndex,
        key: &CommandReservationKey,
    ) -> Result<Option<(u64, ReservationRecord)>, ApplicationError> {
        let generation = self.generation.load(std::sync::atomic::Ordering::Acquire);
        if let Some(record) = current.get(key)? {
            return Ok(Some((generation, record)));
        }
        for historical in (0..generation).rev() {
            if let Some(record) = self.historical_record(historical, key)? {
                return Ok(Some((historical, record)));
            }
        }
        Ok(None)
    }
}

impl Drop for ManagedApplicationReservationStore {
    fn drop(&mut self) {
        // The index connection closes before RA measures its physical object.
        if let Ok(index) = self.entries.get_mut() {
            index.close();
        }
        for slot in [&self.lease, &self.index_lease, &self.recovery_lease] {
            if let Ok(mut slot) = slot.lock()
                && let Some(lease) = slot.take()
            {
                let _ = self.writer.detach(lease);
            }
        }
    }
}

#[cfg(test)]
fn decode_entries(
    bytes: &[u8],
) -> Result<BTreeMap<CommandReservationKey, ReservationRecord>, ApplicationError> {
    let mut entries = BTreeMap::new();
    for line in bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let entry: DurableReservationJournalEntry = serde_json::from_slice(line).map_err(|_| {
            ApplicationError::CorruptProjection(
                "application reservation journal is corrupt".to_owned(),
            )
        })?;
        if entry.schema_version != APPLICATION_RESERVATION_SCHEMA_VERSION {
            return Err(ApplicationError::CorruptProjection(
                "unsupported application reservation journal".to_owned(),
            ));
        }
        apply_journal_operation(&mut entries, entry.operation)?;
    }
    Ok(entries)
}

fn existing_admission(
    record: &ReservationRecord,
    key: &CommandReservationKey,
    command_id: sigil_application::ApplicationCommandId,
    command_kind: String,
    fingerprint: String,
) -> RuntimeApplicationReservationAdmission {
    match &record.state {
        // The domain commit is the durable terminal fact. `Settled` is merely its replay index,
        // so an index append failure must not regress a verified commit into `InFlight`.
        DurableReservationState::DomainCommitted(receipt) => {
            RuntimeApplicationReservationAdmission::Existing(Box::new(
                ApplicationCommandReceipt::Settled(receipt.as_ref().clone()),
            ))
        }
        DurableReservationState::Settled(receipt) => {
            RuntimeApplicationReservationAdmission::Existing(Box::new(receipt.as_ref().clone()))
        }
        DurableReservationState::ConfirmedNoEffect(proof) => {
            RuntimeApplicationReservationAdmission::Existing(Box::new(
                ApplicationCommandReceipt::ConfirmedNoEffect(proof.as_ref().clone()),
            ))
        }
        DurableReservationState::Uncertain(receipt) => {
            RuntimeApplicationReservationAdmission::Existing(Box::new(
                ApplicationCommandReceipt::Uncertain(receipt.as_ref().clone()),
            ))
        }
        // A fresh caller owns the `Reserved` admission. Once this state is observed again, the
        // reservation log alone cannot elect a unique executor: a prior marker append may have
        // reached storage while its acknowledgement was lost. Require owner reconciliation
        // rather than allowing a second dispatch attempt.
        DurableReservationState::Reserved | DurableReservationState::DispatchStarted => {
            RuntimeApplicationReservationAdmission::Existing(Box::new(
                ApplicationCommandReceipt::Uncertain(UncertainCommandReceipt {
                    command_id,
                    command_kind,
                    reservation_fingerprint: fingerprint,
                    recovery: CommandRecoveryBinding {
                        key: key.clone(),
                        phase: phase_for_state(&record.state),
                    },
                    owner_recovery_binding: None,
                }),
            ))
        }
        DurableReservationState::EffectStarted(_) => {
            RuntimeApplicationReservationAdmission::InFlight(ApplicationInFlightReceipt {
                command_id,
                command_kind,
                reservation_fingerprint: fingerprint,
                phase: CommandLifecyclePhase::EffectStarted,
            })
        }
    }
}

fn apply_journal_operation(
    entries: &mut BTreeMap<CommandReservationKey, ReservationRecord>,
    operation: DurableReservationOperation,
) -> Result<(), ApplicationError> {
    match operation {
        DurableReservationOperation::Reserve { key, fingerprint } => {
            validate_reservation_material(&key, &fingerprint)?;
            if let Some(record) = entries.get(&key) {
                if record.fingerprint != fingerprint {
                    return Err(ApplicationError::CorruptProjection(
                        "application reservation journal fingerprint conflict".to_owned(),
                    ));
                }
                if !matches!(&record.state, DurableReservationState::Reserved) {
                    return Err(ApplicationError::CorruptProjection(
                        "application reservation reserve is not monotonic".to_owned(),
                    ));
                }
            } else {
                entries.insert(
                    key,
                    ReservationRecord {
                        fingerprint,
                        state: DurableReservationState::Reserved,
                        request: None,
                        effect_binding: None,
                    },
                );
            }
        }
        DurableReservationOperation::ReserveWithContextV1 {
            key,
            fingerprint,
            request,
        } => {
            if request
                .admission
                .reservation_key(&request.envelope.command_id)
                != key
                || sigil_application::command_fingerprint(&request)? != fingerprint
            {
                return Err(corrupt_reservation(ApplicationError::ScopeMismatch));
            }
            let existing = entries.get(&key);
            if let Some(previous) = existing.and_then(|entry| entry.request.as_ref()) {
                if previous.envelope != request.envelope
                    || previous
                        .admission
                        .reservation_key(&previous.envelope.command_id)
                        != key
                {
                    return Err(corrupt_reservation(ApplicationError::ScopeMismatch));
                }
                return Ok(());
            }
            if existing.is_none() {
                apply_journal_operation(
                    entries,
                    DurableReservationOperation::Reserve {
                        key: key.clone(),
                        fingerprint: fingerprint.clone(),
                    },
                )?;
            }
            let record = checked_record(entries, &key, &fingerprint, "restore original context")?;
            record.request = Some(request);
        }
        DurableReservationOperation::DispatchStarted { key, fingerprint } => {
            transition_record(
                entries,
                key,
                fingerprint,
                "dispatch",
                DurableReservationState::Reserved,
                DurableReservationState::DispatchStarted,
            )?;
        }
        DurableReservationOperation::EffectStarted {
            key,
            fingerprint,
            binding,
        } => {
            binding.validate().map_err(corrupt_reservation)?;
            if binding.recovery.key != key || binding.reservation_fingerprint != fingerprint {
                return Err(corrupt_reservation(ApplicationError::ScopeMismatch));
            }
            let previous_binding = binding.clone();
            let stored_binding = binding.clone();
            let stored_key = key.clone();
            let stored_fingerprint = fingerprint.clone();
            transition_with_payload(
                entries,
                key,
                fingerprint,
                "effect",
                |state| matches!(state, DurableReservationState::DispatchStarted),
                |state| match state {
                    DurableReservationState::EffectStarted(previous) => {
                        previous == &previous_binding
                    }
                    _ => false,
                },
                DurableReservationState::EffectStarted(binding),
            )?;
            checked_record(
                entries,
                &stored_key,
                &stored_fingerprint,
                "bind owner effect",
            )?
            .effect_binding = Some(stored_binding);
        }
        DurableReservationOperation::SessionRuntimeEffectResumedV1 {
            key,
            fingerprint,
            binding,
        } => {
            binding.validate().map_err(corrupt_reservation)?;
            let record = checked_record(entries, &key, &fingerprint, "resume runtime effect")?;
            let request = record
                .request
                .as_ref()
                .ok_or(ApplicationError::Unavailable)?;
            if !matches!(
                &request.envelope.command,
                sigil_application::ApplicationCommand::Provider(
                    sigil_application::ProviderCommand::SelectRoute { .. }
                )
            ) || binding.recovery.key != key
                || binding.reservation_fingerprint != fingerprint
                || binding.command_id != request.envelope.command_id
                || binding.command_kind != request.envelope.command.kind()
                || binding.recovery.phase != CommandLifecyclePhase::EffectStarted
                || record
                    .effect_binding
                    .as_ref()
                    .is_some_and(|original| original != &binding)
            {
                return Err(corrupt_reservation(ApplicationError::ScopeMismatch));
            }
            let may_resume = match &record.state {
                DurableReservationState::DispatchStarted => true,
                DurableReservationState::EffectStarted(original) => original == &binding,
                DurableReservationState::Uncertain(receipt) => matches!(
                    receipt.recovery.phase,
                    CommandLifecyclePhase::DispatchStarted | CommandLifecyclePhase::EffectStarted
                ),
                DurableReservationState::DomainCommitted(_) => record.effect_binding.is_some(),
                DurableReservationState::Settled(receipt) => match receipt.as_ref() {
                    ApplicationCommandReceipt::Settled(_)
                    | ApplicationCommandReceipt::Replayed(_) => record.effect_binding.is_some(),
                    ApplicationCommandReceipt::Uncertain(uncertain)
                    | ApplicationCommandReceipt::ReplayedUncertain(uncertain) => matches!(
                        uncertain.recovery.phase,
                        CommandLifecyclePhase::DispatchStarted
                            | CommandLifecyclePhase::EffectStarted
                    ),
                    _ => false,
                },
                _ => false,
            };
            if !may_resume {
                return Err(corrupt_reservation(ApplicationError::Unavailable));
            }
            record.effect_binding = Some(binding.clone());
            record.state = DurableReservationState::EffectStarted(binding);
        }
        DurableReservationOperation::DomainCommitted {
            key,
            fingerprint,
            receipt,
        } => {
            receipt.validate_for(&key).map_err(corrupt_reservation)?;
            let previous_receipt = receipt.clone();
            transition_with_payload(
                entries,
                key,
                fingerprint,
                "domain commit",
                |state| {
                    matches!(
                        state,
                        DurableReservationState::EffectStarted(_)
                            | DurableReservationState::Uncertain(_)
                    ) || matches!(state, DurableReservationState::Settled(receipt) if matches!(receipt.as_ref(), ApplicationCommandReceipt::Uncertain(_)))
                },
                |state| match state {
                    DurableReservationState::DomainCommitted(previous) => {
                        previous == &previous_receipt
                    }
                    _ => false,
                },
                DurableReservationState::DomainCommitted(receipt),
            )?;
        }
        DurableReservationOperation::ConfirmedNoEffect {
            key,
            fingerprint,
            proof,
        } => {
            proof.validate().map_err(corrupt_reservation)?;
            if proof.source.key != key || proof.reservation_fingerprint != fingerprint {
                return Err(corrupt_reservation(ApplicationError::ScopeMismatch));
            }
            let previous_proof = proof.clone();
            transition_with_payload(
                entries,
                key,
                fingerprint,
                "no-effect proof",
                |state| {
                    matches!(
                        state,
                        DurableReservationState::EffectStarted(_)
                            | DurableReservationState::Uncertain(_)
                    ) || matches!(state, DurableReservationState::Settled(receipt) if matches!(receipt.as_ref(), ApplicationCommandReceipt::Uncertain(_)))
                },
                |state| match state {
                    DurableReservationState::ConfirmedNoEffect(previous) => {
                        previous == &previous_proof
                    }
                    _ => false,
                },
                DurableReservationState::ConfirmedNoEffect(proof),
            )?;
        }
        DurableReservationOperation::Uncertain {
            key,
            fingerprint,
            receipt,
        } => {
            receipt.validate().map_err(corrupt_reservation)?;
            if receipt.recovery.key != key || receipt.reservation_fingerprint != fingerprint {
                return Err(corrupt_reservation(ApplicationError::ScopeMismatch));
            }
            let previous_receipt = receipt.clone();
            transition_with_payload(
                entries,
                key,
                fingerprint,
                "uncertain outcome",
                |state| {
                    matches!(
                        state,
                        DurableReservationState::DispatchStarted
                            | DurableReservationState::EffectStarted(_)
                    )
                },
                |state| match state {
                    DurableReservationState::Uncertain(previous) => previous == &previous_receipt,
                    _ => false,
                },
                DurableReservationState::Uncertain(receipt),
            )?;
        }
        DurableReservationOperation::Settled {
            key,
            fingerprint,
            receipt,
        } => {
            let record = checked_record(entries, &key, &fingerprint, "settlement")?;
            let valid = match &record.state {
                DurableReservationState::DomainCommitted(domain) => matches!(
                    receipt.as_ref(),
                    ApplicationCommandReceipt::Settled(settled) if settled == domain.as_ref()
                ),
                DurableReservationState::ConfirmedNoEffect(_) => {
                    matches!(receipt.as_ref(), ApplicationCommandReceipt::Rejected(_))
                }
                DurableReservationState::Uncertain(uncertain) => matches!(
                    receipt.as_ref(),
                    ApplicationCommandReceipt::Uncertain(stored) if stored == uncertain.as_ref()
                ),
                DurableReservationState::Settled(previous) => {
                    if previous == &receipt {
                        return Ok(());
                    }
                    false
                }
                _ => false,
            };
            if !valid {
                return Err(ApplicationError::CorruptProjection(
                    "application reservation settlement is not backed by its lifecycle state"
                        .to_owned(),
                ));
            }
            record.state = DurableReservationState::Settled(receipt);
        }
    }
    Ok(())
}

fn transition_record(
    entries: &mut BTreeMap<CommandReservationKey, ReservationRecord>,
    key: CommandReservationKey,
    fingerprint: String,
    operation: &str,
    expected: DurableReservationState,
    next: DurableReservationState,
) -> Result<(), ApplicationError> {
    let record = checked_record(entries, &key, &fingerprint, operation)?;
    if std::mem::discriminant(&record.state) == std::mem::discriminant(&next) {
        return Ok(());
    }
    if std::mem::discriminant(&record.state) != std::mem::discriminant(&expected) {
        return Err(ApplicationError::CorruptProjection(format!(
            "application reservation {operation} is not monotonic"
        )));
    }
    record.state = next;
    Ok(())
}

fn transition_with_payload(
    entries: &mut BTreeMap<CommandReservationKey, ReservationRecord>,
    key: CommandReservationKey,
    fingerprint: String,
    operation: &str,
    accepts_previous: impl FnOnce(&DurableReservationState) -> bool,
    is_duplicate: impl FnOnce(&DurableReservationState) -> bool,
    next: DurableReservationState,
) -> Result<(), ApplicationError> {
    let record = checked_record(entries, &key, &fingerprint, operation)?;
    if is_duplicate(&record.state) {
        return Ok(());
    }
    if !accepts_previous(&record.state) {
        return Err(ApplicationError::CorruptProjection(format!(
            "application reservation {operation} is not monotonic"
        )));
    }
    record.state = next;
    Ok(())
}

fn checked_record<'a>(
    entries: &'a mut BTreeMap<CommandReservationKey, ReservationRecord>,
    key: &CommandReservationKey,
    fingerprint: &str,
    operation: &str,
) -> Result<&'a mut ReservationRecord, ApplicationError> {
    let record = entries.get_mut(key).ok_or_else(|| {
        ApplicationError::CorruptProjection(format!(
            "application reservation {operation} has no reservation"
        ))
    })?;
    if record.fingerprint != fingerprint {
        return Err(ApplicationError::CorruptProjection(format!(
            "application reservation {operation} fingerprint conflict"
        )));
    }
    Ok(record)
}

fn corrupt_reservation(_: ApplicationError) -> ApplicationError {
    ApplicationError::CorruptProjection("application reservation journal is corrupt".to_owned())
}

fn phase_for_state(state: &DurableReservationState) -> CommandLifecyclePhase {
    match state {
        DurableReservationState::Reserved => CommandLifecyclePhase::Reserved,
        DurableReservationState::DispatchStarted => CommandLifecyclePhase::DispatchStarted,
        DurableReservationState::EffectStarted(_) => CommandLifecyclePhase::EffectStarted,
        DurableReservationState::DomainCommitted(_) => CommandLifecyclePhase::DomainCommitted,
        DurableReservationState::ConfirmedNoEffect(_) => CommandLifecyclePhase::ConfirmedNoEffect,
        DurableReservationState::Uncertain(_) => CommandLifecyclePhase::Uncertain,
        DurableReservationState::Settled(_) => CommandLifecyclePhase::Settled,
    }
}

fn validate_reservation_material(
    key: &CommandReservationKey,
    fingerprint: &str,
) -> Result<(), ApplicationError> {
    if key.validate().is_err()
        || fingerprint.len() != 64
        || !fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(ApplicationError::CorruptProjection(
            "application reservation entry is incomplete".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/application_reservation_store_tests.rs"]
mod tests;
