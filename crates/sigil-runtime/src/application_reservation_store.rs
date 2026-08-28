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

const APPLICATION_RESERVATION_SCHEMA_VERSION: u16 = 1;
const MAX_APPLICATION_RESERVATION_ENTRIES: usize = 4096;
const MAX_APPLICATION_RESERVATION_BYTES: usize = 16 * 1024 * 1024;

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
        })
    }

    fn persist(
        &self,
        entries: &BTreeMap<CommandReservationKey, ReservationRecord>,
    ) -> Result<(), ApplicationError> {
        if entries.len() > MAX_APPLICATION_RESERVATION_ENTRIES {
            return Err(ApplicationError::InvalidRequest(
                "application reservation store is saturated".to_owned(),
            ));
        }
        let file = DurableReservationFile {
            schema_version: APPLICATION_RESERVATION_SCHEMA_VERSION,
            entries: entries
                .iter()
                .map(|(key, record)| DurableReservationEntry {
                    key: key.clone(),
                    fingerprint: record.fingerprint.clone(),
                    state: record.state.clone(),
                })
                .collect(),
        };
        let bytes = serde_json::to_vec(&file).map_err(|_| {
            ApplicationError::CorruptProjection(
                "application reservation state could not be encoded".to_owned(),
            )
        })?;
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
            .map_err(|_| ApplicationError::Unavailable)
    }

    fn mutate<F, T>(&self, mutation: F) -> Result<T, ApplicationError>
    where
        F: FnOnce(
            &mut BTreeMap<CommandReservationKey, ReservationRecord>,
        ) -> Result<T, ApplicationError>,
    {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| ApplicationError::Unavailable)?;
        let previous = entries.clone();
        let value = mutation(&mut entries)?;
        if let Err(error) = self.persist(&entries) {
            *entries = previous;
            return Err(error);
        }
        Ok(value)
    }
}

impl RuntimeApplicationReservationStore for ManagedApplicationReservationStore {
    fn reserve(
        &self,
        key: CommandReservationKey,
        fingerprint: String,
        request: ApplicationCommandRequest,
    ) -> BoxFuture<'static, Result<RuntimeApplicationReservationAdmission, ApplicationError>> {
        let result = self.mutate(|entries| {
            let Some(record) = entries.get(&key) else {
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
        });
        Box::pin(ready(result))
    }

    fn mark_dispatch_started(
        &self,
        key: CommandReservationKey,
        fingerprint: String,
    ) -> BoxFuture<'static, Result<(), ApplicationError>> {
        let result = self.mutate(|entries| {
            let record = entries.get_mut(&key).ok_or(ApplicationError::Unavailable)?;
            if record.fingerprint != fingerprint {
                return Err(ApplicationError::ScopeMismatch);
            }
            match &record.state {
                DurableReservationState::Reserved => {
                    record.state = DurableReservationState::DispatchStarted;
                    Ok(())
                }
                DurableReservationState::DispatchStarted => Ok(()),
                DurableReservationState::Terminal(_) => Err(ApplicationError::InvalidRequest(
                    "terminal application reservation cannot dispatch".to_owned(),
                )),
            }
        });
        Box::pin(ready(result))
    }

    fn settle(
        &self,
        key: CommandReservationKey,
        fingerprint: String,
        receipt: ApplicationCommandReceipt,
    ) -> BoxFuture<'static, Result<(), ApplicationError>> {
        let result = self.mutate(|entries| {
            let record = entries.get_mut(&key).ok_or(ApplicationError::Unavailable)?;
            if record.fingerprint != fingerprint {
                return Err(ApplicationError::ScopeMismatch);
            }
            if matches!(
                &receipt,
                ApplicationCommandReceipt::PayloadConflict(_)
                    | ApplicationCommandReceipt::InFlight(_)
                    | ApplicationCommandReceipt::Replayed(_)
            ) {
                return Err(ApplicationError::InvalidRequest(
                    "non-terminal replay response cannot be persisted".to_owned(),
                ));
            }
            record.state = DurableReservationState::Terminal(Box::new(receipt));
            Ok(())
        });
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
    let file: DurableReservationFile = serde_json::from_slice(bytes).map_err(|_| {
        ApplicationError::CorruptProjection("application reservation journal is corrupt".to_owned())
    })?;
    if file.schema_version != APPLICATION_RESERVATION_SCHEMA_VERSION
        || file.entries.len() > MAX_APPLICATION_RESERVATION_ENTRIES
    {
        return Err(ApplicationError::CorruptProjection(
            "unsupported application reservation journal".to_owned(),
        ));
    }
    let mut entries = BTreeMap::new();
    for entry in file.entries {
        if entry.key.command_id.as_str().is_empty() || entry.fingerprint.is_empty() {
            return Err(ApplicationError::CorruptProjection(
                "application reservation entry is incomplete".to_owned(),
            ));
        }
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
