//! Durable application command reservations backed by the R71 managed storage writer.

use std::{
    collections::BTreeMap,
    fmt,
    sync::{Arc, Mutex},
};

use futures::future::{BoxFuture, ready};
use serde::{Deserialize, Serialize};
use sigil_application::{
    ApplicationCommandReceipt, ApplicationCommandRequest, ApplicationError,
    ApplicationInFlightReceipt, CommandConflict, CommandReservationKey,
};

use crate::{
    RuntimeApplicationReservationAdmission, RuntimeApplicationReservationStore,
    managed_storage_writer::{
        ManagedStorageWriterAdapterV1, ManagedStorageWriterLeaseV1, StorageWriterChannelV1,
    },
};

const APPLICATION_RESERVATION_SCHEMA_VERSION: u16 = 2;
const LEGACY_APPLICATION_RESERVATION_SCHEMA_VERSION: u16 = 1;
const MAX_APPLICATION_RESERVATION_ENTRIES: usize = 4096;
const MAX_APPLICATION_RESERVATION_BYTES: usize = 16 * 1024 * 1024;

/// Legacy whole-file representation retained only so an existing R70.4 reservation namespace
/// can be migrated into the append-only journal on first reopen.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DurableReservationFile {
    schema_version: u16,
    entries: Vec<DurableReservationEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DurableReservationEntry {
    key: CommandReservationKey,
    fingerprint: String,
    state: DurableReservationState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum DurableReservationState {
    Reserved,
    DispatchStarted,
    Terminal(Box<ApplicationCommandReceipt>),
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
    DispatchStarted {
        key: CommandReservationKey,
        fingerprint: String,
    },
    Terminal {
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
        let (entries, legacy_snapshot) = decode_entries(&bytes)?;
        let store = Self {
            writer,
            lease: Mutex::new(Some(lease)),
            entries: Mutex::new(entries),
            durable_bytes: Mutex::new(bytes.len()),
        };
        if legacy_snapshot {
            store.rewrite_as_journal()?;
        }
        Ok(store)
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

    fn rewrite_as_journal(&self) -> Result<(), ApplicationError> {
        let entries = self
            .entries
            .lock()
            .map_err(|_| ApplicationError::Unavailable)?
            .clone();
        let mut bytes = Vec::new();
        for (key, record) in &entries {
            append_journal_bytes(
                &mut bytes,
                DurableReservationOperation::Reserve {
                    key: key.clone(),
                    fingerprint: record.fingerprint.clone(),
                },
            )?;
            if matches!(record.state, DurableReservationState::DispatchStarted) {
                append_journal_bytes(
                    &mut bytes,
                    DurableReservationOperation::DispatchStarted {
                        key: key.clone(),
                        fingerprint: record.fingerprint.clone(),
                    },
                )?;
            }
            if let DurableReservationState::Terminal(receipt) = &record.state {
                append_journal_bytes(
                    &mut bytes,
                    DurableReservationOperation::Terminal {
                        key: key.clone(),
                        fingerprint: record.fingerprint.clone(),
                        receipt: receipt.clone(),
                    },
                )?;
            }
        }
        if bytes.len() > MAX_APPLICATION_RESERVATION_BYTES {
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
            .replace_record_bytes(lease, &bytes)
            .map_err(|_| ApplicationError::Unavailable)?;
        *self
            .durable_bytes
            .lock()
            .map_err(|_| ApplicationError::Unavailable)? = bytes.len();
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
            Ok(match &record.state {
                DurableReservationState::Terminal(receipt) => {
                    RuntimeApplicationReservationAdmission::Existing(receipt.as_ref().clone())
                }
                DurableReservationState::Reserved | DurableReservationState::DispatchStarted => {
                    RuntimeApplicationReservationAdmission::InFlight(ApplicationInFlightReceipt {
                        command_id: request.envelope.command_id,
                        command_kind: request.envelope.command.kind().to_owned(),
                        reservation_fingerprint: fingerprint,
                    })
                }
            })
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
                DurableReservationState::Terminal(_) => Err(ApplicationError::InvalidRequest(
                    "terminal application reservation cannot dispatch".to_owned(),
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
            ) {
                return Err(ApplicationError::InvalidRequest(
                    "non-terminal replay response cannot be persisted".to_owned(),
                ));
            }
            self.append_operation(DurableReservationOperation::Terminal {
                key,
                fingerprint,
                receipt: Box::new(receipt.clone()),
            })?;
            record.state = DurableReservationState::Terminal(Box::new(receipt));
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
) -> Result<(BTreeMap<CommandReservationKey, ReservationRecord>, bool), ApplicationError> {
    if bytes.is_empty() {
        return Ok((BTreeMap::new(), false));
    }
    if let Ok(file) = serde_json::from_slice::<DurableReservationFile>(bytes)
        && file.schema_version == LEGACY_APPLICATION_RESERVATION_SCHEMA_VERSION
    {
        return Ok((decode_legacy_entries(file)?, true));
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
    Ok((entries, false))
}

fn decode_legacy_entries(
    file: DurableReservationFile,
) -> Result<BTreeMap<CommandReservationKey, ReservationRecord>, ApplicationError> {
    if file.entries.len() > MAX_APPLICATION_RESERVATION_ENTRIES {
        return Err(ApplicationError::CorruptProjection(
            "application reservation journal exceeds its entry bound".to_owned(),
        ));
    }
    let mut entries = BTreeMap::new();
    for entry in file.entries {
        validate_reservation_material(&entry.key, &entry.fingerprint)?;
        if entries
            .insert(
                entry.key,
                ReservationRecord {
                    fingerprint: entry.fingerprint,
                    state: entry.state,
                },
            )
            .is_some()
        {
            return Err(ApplicationError::CorruptProjection(
                "application reservation journal has duplicate keys".to_owned(),
            ));
        }
    }
    Ok(entries)
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
            let record = entries.get_mut(&key).ok_or_else(|| {
                ApplicationError::CorruptProjection(
                    "application reservation dispatch has no reservation".to_owned(),
                )
            })?;
            if record.fingerprint != fingerprint {
                return Err(ApplicationError::CorruptProjection(
                    "application reservation dispatch fingerprint conflict".to_owned(),
                ));
            }
            if matches!(record.state, DurableReservationState::Reserved) {
                record.state = DurableReservationState::DispatchStarted;
            }
        }
        DurableReservationOperation::Terminal {
            key,
            fingerprint,
            receipt,
        } => {
            let record = entries.get_mut(&key).ok_or_else(|| {
                ApplicationError::CorruptProjection(
                    "application reservation terminal has no reservation".to_owned(),
                )
            })?;
            if record.fingerprint != fingerprint {
                return Err(ApplicationError::CorruptProjection(
                    "application reservation terminal fingerprint conflict".to_owned(),
                ));
            }
            if let DurableReservationState::Terminal(previous) = &record.state {
                if previous != &receipt {
                    return Err(ApplicationError::CorruptProjection(
                        "application reservation terminal was rewritten".to_owned(),
                    ));
                }
            } else {
                record.state = DurableReservationState::Terminal(receipt);
            }
        }
    }
    Ok(())
}

fn validate_reservation_material(
    key: &CommandReservationKey,
    fingerprint: &str,
) -> Result<(), ApplicationError> {
    if key.command_id.as_str().is_empty() || fingerprint.is_empty() {
        return Err(ApplicationError::CorruptProjection(
            "application reservation entry is incomplete".to_owned(),
        ));
    }
    Ok(())
}

fn append_journal_bytes(
    bytes: &mut Vec<u8>,
    operation: DurableReservationOperation,
) -> Result<(), ApplicationError> {
    let entry = DurableReservationJournalEntry {
        schema_version: APPLICATION_RESERVATION_SCHEMA_VERSION,
        operation,
    };
    let encoded = serde_json::to_vec(&entry).map_err(|_| {
        ApplicationError::CorruptProjection(
            "application reservation journal entry could not be encoded".to_owned(),
        )
    })?;
    bytes.extend_from_slice(&encoded);
    bytes.push(b'\n');
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sigil_application::{ApplicationCommandId, CommandRejection};

    fn key() -> CommandReservationKey {
        CommandReservationKey {
            application_instance: sigil_application::ApplicationInstanceId::new("instance")
                .expect("instance"),
            principal: sigil_application::AuthenticatedSubject::new("subject").expect("subject"),
            client_epoch: 1,
            command_id: ApplicationCommandId::new("command").expect("command"),
        }
    }

    fn terminal() -> ApplicationCommandReceipt {
        ApplicationCommandReceipt::Rejected(CommandRejection {
            kind: "test".to_owned(),
            reason: "test rejection".to_owned(),
        })
    }

    #[test]
    fn journal_replay_preserves_dispatch_and_terminal_state() {
        let key = key();
        let mut bytes = Vec::new();
        append_journal_bytes(
            &mut bytes,
            DurableReservationOperation::Reserve {
                key: key.clone(),
                fingerprint: "fingerprint".to_owned(),
            },
        )
        .expect("reserve");
        append_journal_bytes(
            &mut bytes,
            DurableReservationOperation::Reserve {
                key: key.clone(),
                fingerprint: "fingerprint".to_owned(),
            },
        )
        .expect("idempotent duplicate reserve");
        append_journal_bytes(
            &mut bytes,
            DurableReservationOperation::DispatchStarted {
                key: key.clone(),
                fingerprint: "fingerprint".to_owned(),
            },
        )
        .expect("dispatch");
        let receipt = terminal();
        append_journal_bytes(
            &mut bytes,
            DurableReservationOperation::Terminal {
                key: key.clone(),
                fingerprint: "fingerprint".to_owned(),
                receipt: Box::new(receipt.clone()),
            },
        )
        .expect("terminal");
        append_journal_bytes(
            &mut bytes,
            DurableReservationOperation::Terminal {
                key: key.clone(),
                fingerprint: "fingerprint".to_owned(),
                receipt: Box::new(receipt.clone()),
            },
        )
        .expect("idempotent duplicate terminal");

        let (entries, legacy) = decode_entries(&bytes).expect("replay");
        assert!(!legacy);
        let record = entries.get(&key).expect("record");
        assert_eq!(record.fingerprint, "fingerprint");
        assert!(matches!(record.state, DurableReservationState::Terminal(_)));
    }

    #[test]
    fn journal_replay_rejects_terminal_rewrite_and_unknown_schema() {
        let key = key();
        let mut entries = BTreeMap::new();
        apply_journal_operation(
            &mut entries,
            DurableReservationOperation::Reserve {
                key: key.clone(),
                fingerprint: "fingerprint".to_owned(),
            },
        )
        .expect("reserve");
        apply_journal_operation(
            &mut entries,
            DurableReservationOperation::Terminal {
                key: key.clone(),
                fingerprint: "fingerprint".to_owned(),
                receipt: Box::new(terminal()),
            },
        )
        .expect("terminal");
        let error = apply_journal_operation(
            &mut entries,
            DurableReservationOperation::Terminal {
                key,
                fingerprint: "fingerprint".to_owned(),
                receipt: Box::new(ApplicationCommandReceipt::Rejected(CommandRejection {
                    kind: "other".to_owned(),
                    reason: "other rejection".to_owned(),
                })),
            },
        )
        .expect_err("terminal rewrite");
        assert!(matches!(error, ApplicationError::CorruptProjection(_)));

        let unknown = serde_json::json!({
            "schema_version": APPLICATION_RESERVATION_SCHEMA_VERSION + 1,
            "operation": {
                "type": "reserve",
                "value": { "key": {}, "fingerprint": "fingerprint" }
            }
        });
        let error = decode_entries(serde_json::to_string(&unknown).expect("json").as_bytes())
            .expect_err("unknown schema");
        assert!(matches!(error, ApplicationError::CorruptProjection(_)));
    }

    #[test]
    fn legacy_snapshot_is_marked_for_one_time_migration() {
        let legacy = DurableReservationFile {
            schema_version: LEGACY_APPLICATION_RESERVATION_SCHEMA_VERSION,
            entries: vec![DurableReservationEntry {
                key: key(),
                fingerprint: "fingerprint".to_owned(),
                state: DurableReservationState::Reserved,
            }],
        };
        let bytes = serde_json::to_vec(&legacy).expect("legacy json");
        let (entries, legacy_snapshot) = decode_entries(&bytes).expect("decode legacy");
        assert!(legacy_snapshot);
        assert_eq!(entries.len(), 1);
    }
}
