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
const MAX_APPLICATION_RESERVATION_ENTRIES: usize = 4096;
const MAX_APPLICATION_RESERVATION_BYTES: usize = 16 * 1024 * 1024;

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
#[serde(deny_unknown_fields)]
struct DurableReservationJournalEntry {
    schema_version: u16,
    operation: DurableReservationOperation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "type",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum DurableReservationOperation {
    Reserve {
        key: CommandReservationKey,
        fingerprint: String,
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

#[derive(Debug, Clone)]
struct ReservationRecord {
    fingerprint: String,
    state: DurableReservationState,
}

/// Production application reservation authority for one managed application-control namespace.
pub struct ManagedApplicationReservationStore {
    writer: Arc<ManagedStorageWriterAdapterV1>,
    lease: Mutex<Option<ManagedStorageWriterLeaseV1>>,
    entries: Mutex<BTreeMap<CommandReservationKey, ReservationRecord>>,
    durable_bytes: Mutex<usize>,
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
        let lease = writer
            .acquire_named(StorageWriterChannelV1::ApplicationControlLog, key)
            .map_err(|_| ApplicationError::Unavailable)?;
        let bytes = writer
            .read_record_bytes(&lease, MAX_APPLICATION_RESERVATION_BYTES)
            .map_err(|_| ApplicationError::Unavailable)?;
        let entries = decode_entries(&bytes)?;
        Ok(Self {
            writer,
            lease: Mutex::new(Some(lease)),
            entries: Mutex::new(entries),
            durable_bytes: Mutex::new(bytes.len()),
        })
    }

    fn append_operation(
        &self,
        operation: DurableReservationOperation,
    ) -> Result<(), ApplicationError> {
        let entry = DurableReservationJournalEntry {
            schema_version: APPLICATION_RESERVATION_SCHEMA_VERSION,
            operation,
        };
        let bytes = serde_json::to_vec(&entry).map_err(|_| {
            ApplicationError::CorruptProjection(
                "application reservation journal entry could not be encoded".to_owned(),
            )
        })?;
        let record_bytes = bytes.len().checked_add(1).ok_or_else(|| {
            ApplicationError::InvalidRequest("reservation journal overflow".to_owned())
        })?;
        let mut durable_bytes = self
            .durable_bytes
            .lock()
            .map_err(|_| ApplicationError::Unavailable)?;
        let next_bytes = durable_bytes.checked_add(record_bytes).ok_or_else(|| {
            ApplicationError::InvalidRequest("reservation journal overflow".to_owned())
        })?;
        if next_bytes > MAX_APPLICATION_RESERVATION_BYTES {
            return Err(ApplicationError::InvalidRequest(
                "application reservation store exceeds its byte bound".to_owned(),
            ));
        }
        let lease = self
            .lease
            .lock()
            .map_err(|_| ApplicationError::Unavailable)?;
        let lease = lease.as_ref().ok_or(ApplicationError::Unavailable)?;
        self.writer
            .write_record(lease, &bytes)
            .map_err(|_| ApplicationError::Unavailable)?;
        *durable_bytes = next_bytes;
        Ok(())
    }
}

impl RuntimeApplicationReservationStore for ManagedApplicationReservationStore {
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
            let Some(record) = entries.get(&key) else {
                self.append_operation(DurableReservationOperation::Reserve {
                    key: key.clone(),
                    fingerprint: fingerprint.clone(),
                })?;
                entries.insert(
                    key,
                    ReservationRecord {
                        fingerprint,
                        state: DurableReservationState::Reserved,
                    },
                );
                return Ok(RuntimeApplicationReservationAdmission::Reserved);
            };
            if record.fingerprint != fingerprint {
                return Ok(RuntimeApplicationReservationAdmission::Conflict(
                    CommandConflict {
                        command_id: request.envelope.command_id,
                        original_fingerprint: record.fingerprint.clone(),
                        received_fingerprint: fingerprint,
                    },
                ));
            }
            Ok(existing_admission(
                record,
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
        let result = (|| {
            let mut entries = self
                .entries
                .lock()
                .map_err(|_| ApplicationError::Unavailable)?;
            let record = entries.get_mut(&key).ok_or(ApplicationError::Unavailable)?;
            if record.fingerprint != fingerprint {
                return Err(ApplicationError::ScopeMismatch);
            }
            match &record.state {
                DurableReservationState::Reserved => {
                    self.append_operation(DurableReservationOperation::DispatchStarted {
                        key,
                        fingerprint: fingerprint.clone(),
                    })?;
                    record.state = DurableReservationState::DispatchStarted;
                    Ok(())
                }
                DurableReservationState::DispatchStarted => Ok(()),
                _ => Err(ApplicationError::InvalidRequest(
                    "application reservation cannot dispatch after lifecycle progressed".to_owned(),
                )),
            }
        })();
        Box::pin(ready(result))
    }

    fn mark_effect_started(
        &self,
        key: CommandReservationKey,
        fingerprint: String,
        binding: CommandEffectBinding,
    ) -> BoxFuture<'static, Result<(), ApplicationError>> {
        let result = (|| {
            binding.validate()?;
            if binding.recovery.key != key || binding.reservation_fingerprint != fingerprint {
                return Err(ApplicationError::ScopeMismatch);
            }
            let mut entries = self
                .entries
                .lock()
                .map_err(|_| ApplicationError::Unavailable)?;
            let record = entries.get_mut(&key).ok_or(ApplicationError::Unavailable)?;
            if record.fingerprint != fingerprint {
                return Err(ApplicationError::ScopeMismatch);
            }
            match &record.state {
                DurableReservationState::DispatchStarted => {
                    self.append_operation(DurableReservationOperation::EffectStarted {
                        key,
                        fingerprint: fingerprint.clone(),
                        binding: Box::new(binding.clone()),
                    })?;
                    record.state = DurableReservationState::EffectStarted(Box::new(binding));
                    Ok(())
                }
                DurableReservationState::EffectStarted(previous) if **previous == binding => Ok(()),
                _ => Err(ApplicationError::InvalidRequest(
                    "application reservation cannot start an effect from its current lifecycle phase"
                        .to_owned(),
                )),
            }
        })();
        Box::pin(ready(result))
    }

    fn mark_domain_committed(
        &self,
        key: CommandReservationKey,
        fingerprint: String,
        receipt: ApplicationDomainReceipt,
    ) -> BoxFuture<'static, Result<(), ApplicationError>> {
        let result = (|| {
            receipt.validate_for(&key)?;
            let mut entries = self
                .entries
                .lock()
                .map_err(|_| ApplicationError::Unavailable)?;
            let record = entries.get_mut(&key).ok_or(ApplicationError::Unavailable)?;
            if record.fingerprint != fingerprint {
                return Err(ApplicationError::ScopeMismatch);
            }
            match &record.state {
                DurableReservationState::EffectStarted(_) => {
                    self.append_operation(DurableReservationOperation::DomainCommitted {
                        key,
                        fingerprint: fingerprint.clone(),
                        receipt: Box::new(receipt.clone()),
                    })?;
                    record.state = DurableReservationState::DomainCommitted(Box::new(receipt));
                    Ok(())
                }
                DurableReservationState::DomainCommitted(previous) if **previous == receipt => {
                    Ok(())
                }
                _ => Err(ApplicationError::InvalidRequest(
                    "application reservation domain commit is not monotonic".to_owned(),
                )),
            }
        })();
        Box::pin(ready(result))
    }

    fn mark_confirmed_no_effect(
        &self,
        key: CommandReservationKey,
        fingerprint: String,
        proof: CommandNoEffectProof,
    ) -> BoxFuture<'static, Result<(), ApplicationError>> {
        let result = (|| {
            proof.validate()?;
            if proof.source.key != key || proof.reservation_fingerprint != fingerprint {
                return Err(ApplicationError::ScopeMismatch);
            }
            let mut entries = self
                .entries
                .lock()
                .map_err(|_| ApplicationError::Unavailable)?;
            let record = entries.get_mut(&key).ok_or(ApplicationError::Unavailable)?;
            if record.fingerprint != fingerprint {
                return Err(ApplicationError::ScopeMismatch);
            }
            match &record.state {
                DurableReservationState::EffectStarted(_) => {
                    self.append_operation(DurableReservationOperation::ConfirmedNoEffect {
                        key,
                        fingerprint: fingerprint.clone(),
                        proof: Box::new(proof.clone()),
                    })?;
                    record.state = DurableReservationState::ConfirmedNoEffect(Box::new(proof));
                    Ok(())
                }
                DurableReservationState::ConfirmedNoEffect(previous) if **previous == proof => {
                    Ok(())
                }
                _ => Err(ApplicationError::InvalidRequest(
                    "application reservation no-effect proof is not monotonic".to_owned(),
                )),
            }
        })();
        Box::pin(ready(result))
    }

    fn mark_uncertain(
        &self,
        key: CommandReservationKey,
        fingerprint: String,
        receipt: UncertainCommandReceipt,
    ) -> BoxFuture<'static, Result<(), ApplicationError>> {
        let result = (|| {
            receipt.validate()?;
            if receipt.recovery.key != key || receipt.reservation_fingerprint != fingerprint {
                return Err(ApplicationError::ScopeMismatch);
            }
            let mut entries = self
                .entries
                .lock()
                .map_err(|_| ApplicationError::Unavailable)?;
            let record = entries.get_mut(&key).ok_or(ApplicationError::Unavailable)?;
            if record.fingerprint != fingerprint {
                return Err(ApplicationError::ScopeMismatch);
            }
            match &record.state {
                DurableReservationState::DispatchStarted
                | DurableReservationState::EffectStarted(_) => {
                    self.append_operation(DurableReservationOperation::Uncertain {
                        key,
                        fingerprint: fingerprint.clone(),
                        receipt: Box::new(receipt.clone()),
                    })?;
                    record.state = DurableReservationState::Uncertain(Box::new(receipt));
                    Ok(())
                }
                DurableReservationState::Uncertain(previous) if **previous == receipt => Ok(()),
                _ => Err(ApplicationError::InvalidRequest(
                    "application reservation uncertainty is not monotonic".to_owned(),
                )),
            }
        })();
        Box::pin(ready(result))
    }

    fn settle(
        &self,
        key: CommandReservationKey,
        fingerprint: String,
        receipt: ApplicationCommandReceipt,
    ) -> BoxFuture<'static, Result<(), ApplicationError>> {
        let result = (|| {
            let mut entries = self
                .entries
                .lock()
                .map_err(|_| ApplicationError::Unavailable)?;
            let record = entries.get_mut(&key).ok_or(ApplicationError::Unavailable)?;
            if record.fingerprint != fingerprint {
                return Err(ApplicationError::ScopeMismatch);
            }
            if matches!(
                &receipt,
                ApplicationCommandReceipt::PayloadConflict(_)
                    | ApplicationCommandReceipt::InFlight(_)
                    | ApplicationCommandReceipt::Replayed(_)
                    | ApplicationCommandReceipt::ReplayedUncertain(_)
                    | ApplicationCommandReceipt::ConfirmedNoEffect(_)
                    | ApplicationCommandReceipt::SafetyStopRequestedButUnrecorded(_)
            ) {
                return Err(ApplicationError::InvalidRequest(
                    "non-terminal replay response cannot be persisted".to_owned(),
                ));
            }
            match &record.state {
                DurableReservationState::DomainCommitted(domain) if matches!(&receipt, ApplicationCommandReceipt::Settled(settled) if settled == domain.as_ref()) =>
                    {}
                DurableReservationState::ConfirmedNoEffect(_)
                    if matches!(&receipt, ApplicationCommandReceipt::Rejected(_)) => {}
                DurableReservationState::Uncertain(uncertain) if matches!(&receipt, ApplicationCommandReceipt::Uncertain(stored) if stored == uncertain.as_ref()) =>
                    {}
                DurableReservationState::Settled(previous) if **previous == receipt => {
                    return Ok(());
                }
                _ => {
                    return Err(ApplicationError::InvalidRequest(
                        "application reservation settlement is not backed by its lifecycle state"
                            .to_owned(),
                    ));
                }
            }
            self.append_operation(DurableReservationOperation::Settled {
                key,
                fingerprint,
                receipt: Box::new(receipt.clone()),
            })?;
            record.state = DurableReservationState::Settled(Box::new(receipt));
            Ok(())
        })();
        Box::pin(ready(result))
    }
}

impl Drop for ManagedApplicationReservationStore {
    fn drop(&mut self) {
        let Ok(mut lease) = self.lease.lock() else {
            return;
        };
        if let Some(lease) = lease.take() {
            let _ = self.writer.finalize(lease);
        }
    }
}

fn decode_entries(
    bytes: &[u8],
) -> Result<BTreeMap<CommandReservationKey, ReservationRecord>, ApplicationError> {
    if bytes.is_empty() {
        return Ok(BTreeMap::new());
    }
    if bytes.len() > MAX_APPLICATION_RESERVATION_BYTES {
        return Err(ApplicationError::CorruptProjection(
            "application reservation journal exceeds its byte bound".to_owned(),
        ));
    }

    let mut entries = BTreeMap::new();
    let mut saw_record = false;
    for line in bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty() && !line.iter().all(u8::is_ascii_whitespace))
    {
        saw_record = true;
        let line = std::str::from_utf8(line).map_err(|_| {
            ApplicationError::CorruptProjection(
                "application reservation journal is corrupt".to_owned(),
            )
        })?;
        let entry = serde_json::from_str::<DurableReservationJournalEntry>(line).map_err(|_| {
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
        if entries.len() > MAX_APPLICATION_RESERVATION_ENTRIES {
            return Err(ApplicationError::CorruptProjection(
                "application reservation journal exceeds its entry bound".to_owned(),
            ));
        }
    }
    if !saw_record {
        return Err(ApplicationError::CorruptProjection(
            "application reservation journal is corrupt".to_owned(),
        ));
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
                    },
                );
            }
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
                |state| matches!(state, DurableReservationState::EffectStarted(_)),
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
                |state| matches!(state, DurableReservationState::EffectStarted(_)),
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
