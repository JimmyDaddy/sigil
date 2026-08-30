//! Durable application projection-delivery acknowledgements.
//!
//! A projection ACK is a committed observer fact, not a presentation hint.  It therefore uses
//! its own managed application-control namespace and is replayed before a surface reconnects.
//! The store is deliberately separate from command reservations so a corrupt or full ACK log
//! cannot be mistaken for a command terminal receipt.

use std::{
    collections::BTreeMap,
    fmt,
    sync::{Arc, Mutex},
};

use futures::future::{BoxFuture, ready};
use serde::{Deserialize, Serialize};
use sigil_application::{ApplicationError, ApplicationScope, ProjectionDeliveryAck};

use crate::managed_storage_writer::{
    ManagedStorageWriterAdapterV1, ManagedStorageWriterLeaseV1, StorageWriterChannelV1,
};

const APPLICATION_DELIVERY_ACK_SCHEMA_VERSION: u16 = 1;
const MAX_APPLICATION_DELIVERY_ACK_ENTRIES: usize = 1_024;
const MAX_APPLICATION_DELIVERY_ACK_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DurableDeliveryAckJournalEntry {
    schema_version: u16,
    acknowledgement: ProjectionDeliveryAck,
}

/// Runtime-owned durable ACK journal for one authenticated application observer.
pub struct RuntimeApplicationDeliveryAckStore {
    writer: Arc<ManagedStorageWriterAdapterV1>,
    lease: Mutex<Option<ManagedStorageWriterLeaseV1>>,
    expected_scope: ApplicationScope,
    expected_observer_generation: u64,
    acknowledgements: Mutex<BTreeMap<String, ProjectionDeliveryAck>>,
    durable_bytes: Mutex<usize>,
}

impl fmt::Debug for RuntimeApplicationDeliveryAckStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeApplicationDeliveryAckStore")
            .field("writer", &"<managed storage writer>")
            .field("lease", &"<redacted>")
            .field("expected_scope", &self.expected_scope)
            .field(
                "expected_observer_generation",
                &self.expected_observer_generation,
            )
            .field("acknowledgements", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl RuntimeApplicationDeliveryAckStore {
    /// Opens a private managed namespace and replays its durable ACK journal.
    pub fn open(
        writer: Arc<ManagedStorageWriterAdapterV1>,
        key: &str,
        expected_scope: ApplicationScope,
        expected_observer_generation: u64,
    ) -> Result<Self, ApplicationError> {
        if expected_observer_generation == 0 {
            return Err(ApplicationError::InvalidRequest(
                "application delivery ACK observer generation must be non-zero".to_owned(),
            ));
        }
        let lease = writer
            .acquire_named(StorageWriterChannelV1::ApplicationControlLog, key)
            .map_err(|_| ApplicationError::Unavailable)?;
        let bytes = match writer.read_record_bytes(&lease, MAX_APPLICATION_DELIVERY_ACK_BYTES) {
            Ok(bytes) => bytes,
            Err(error) => {
                let _ = writer.finalize(lease);
                tracing::debug!(error = %error, "application delivery ACK journal could not be read");
                return Err(ApplicationError::Unavailable);
            }
        };
        let acknowledgements =
            match decode_entries(&bytes, &expected_scope, expected_observer_generation) {
                Ok(acknowledgements) => acknowledgements,
                Err(error) => {
                    let _ = writer.finalize(lease);
                    return Err(error);
                }
            };
        Ok(Self {
            writer,
            lease: Mutex::new(Some(lease)),
            expected_scope,
            expected_observer_generation,
            acknowledgements: Mutex::new(acknowledgements),
            durable_bytes: Mutex::new(bytes.len()),
        })
    }

    fn append(&self, acknowledgement: &ProjectionDeliveryAck) -> Result<(), ApplicationError> {
        let entry = DurableDeliveryAckJournalEntry {
            schema_version: APPLICATION_DELIVERY_ACK_SCHEMA_VERSION,
            acknowledgement: acknowledgement.clone(),
        };
        let encoded = serde_json::to_vec(&entry).map_err(|_| {
            ApplicationError::CorruptProjection(
                "application delivery ACK could not be encoded".to_owned(),
            )
        })?;
        let record_bytes = encoded.len().checked_add(1).ok_or_else(|| {
            ApplicationError::InvalidRequest("application delivery ACK journal overflow".to_owned())
        })?;
        let mut durable_bytes = self
            .durable_bytes
            .lock()
            .map_err(|_| ApplicationError::Unavailable)?;
        let next_bytes = durable_bytes.checked_add(record_bytes).ok_or_else(|| {
            ApplicationError::InvalidRequest("application delivery ACK journal overflow".to_owned())
        })?;
        if next_bytes > MAX_APPLICATION_DELIVERY_ACK_BYTES {
            return Err(ApplicationError::InvalidRequest(
                "application delivery ACK store exceeds its byte bound".to_owned(),
            ));
        }
        let lease = self
            .lease
            .lock()
            .map_err(|_| ApplicationError::Unavailable)?;
        let lease = lease.as_ref().ok_or(ApplicationError::Unavailable)?;
        self.writer
            .write_record(lease, &encoded)
            .map_err(|_| ApplicationError::Unavailable)?;
        *durable_bytes = next_bytes;
        Ok(())
    }

    /// Returns whether this observer has durably accepted the exact ACK.
    pub fn contains(&self, acknowledgement: &ProjectionDeliveryAck) -> bool {
        self.acknowledgements
            .lock()
            .ok()
            .and_then(|entries| entries.get(&acknowledgement.event_id).cloned())
            .is_some_and(|stored| stored == *acknowledgement)
    }

    fn validate_expected(
        &self,
        acknowledgement: &ProjectionDeliveryAck,
    ) -> Result<(), ApplicationError> {
        acknowledgement.validate()?;
        if acknowledgement.scope != self.expected_scope
            || acknowledgement.frontier.scope != self.expected_scope
            || acknowledgement.observer_generation != self.expected_observer_generation
        {
            return Err(ApplicationError::ScopeMismatch);
        }
        Ok(())
    }
}

impl crate::RuntimeApplicationDeliveryAcker for RuntimeApplicationDeliveryAckStore {
    fn acknowledge(
        &self,
        acknowledgement: ProjectionDeliveryAck,
    ) -> BoxFuture<'static, Result<(), ApplicationError>> {
        Box::pin(ready(self.acknowledge_sync(acknowledgement)))
    }
}

impl RuntimeApplicationDeliveryAckStore {
    fn acknowledge_sync(
        &self,
        acknowledgement: ProjectionDeliveryAck,
    ) -> Result<(), ApplicationError> {
        self.validate_expected(&acknowledgement)?;
        let mut entries = self
            .acknowledgements
            .lock()
            .map_err(|_| ApplicationError::Unavailable)?;
        if let Some(previous) = entries.get(&acknowledgement.event_id) {
            if previous == &acknowledgement {
                return Ok(());
            }
            return Err(ApplicationError::CorruptProjection(
                "application delivery ACK event identity was rewritten".to_owned(),
            ));
        }
        if entries.len() >= MAX_APPLICATION_DELIVERY_ACK_ENTRIES {
            return Err(ApplicationError::InvalidRequest(
                "application delivery ACK store exceeds its entry bound".to_owned(),
            ));
        }
        self.append(&acknowledgement)?;
        entries.insert(acknowledgement.event_id.clone(), acknowledgement);
        Ok(())
    }
}

impl Drop for RuntimeApplicationDeliveryAckStore {
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
    expected_scope: &ApplicationScope,
    expected_observer_generation: u64,
) -> Result<BTreeMap<String, ProjectionDeliveryAck>, ApplicationError> {
    if bytes.is_empty() {
        return Ok(BTreeMap::new());
    }
    if !bytes.ends_with(b"\n") {
        return Err(ApplicationError::CorruptProjection(
            "application delivery ACK journal ends with a partial record".to_owned(),
        ));
    }
    let mut entries = BTreeMap::new();
    for line in bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty() && !line.iter().all(u8::is_ascii_whitespace))
    {
        let entry =
            serde_json::from_slice::<DurableDeliveryAckJournalEntry>(line).map_err(|_| {
                ApplicationError::CorruptProjection(
                    "application delivery ACK journal is corrupt".to_owned(),
                )
            })?;
        if entry.schema_version != APPLICATION_DELIVERY_ACK_SCHEMA_VERSION {
            return Err(ApplicationError::CorruptProjection(
                "unsupported application delivery ACK journal".to_owned(),
            ));
        }
        entry.acknowledgement.validate()?;
        if entry.acknowledgement.scope != *expected_scope
            || entry.acknowledgement.frontier.scope != *expected_scope
            || entry.acknowledgement.observer_generation != expected_observer_generation
        {
            return Err(ApplicationError::ScopeMismatch);
        }
        if let Some(previous) = entries.get(&entry.acknowledgement.event_id)
            && previous != &entry.acknowledgement
        {
            return Err(ApplicationError::CorruptProjection(
                "application delivery ACK event identity was rewritten".to_owned(),
            ));
        }
        entries.insert(
            entry.acknowledgement.event_id.clone(),
            entry.acknowledgement,
        );
        if entries.len() > MAX_APPLICATION_DELIVERY_ACK_ENTRIES {
            return Err(ApplicationError::CorruptProjection(
                "application delivery ACK journal exceeds its entry bound".to_owned(),
            ));
        }
    }
    Ok(entries)
}

#[cfg(test)]
#[path = "tests/application_delivery_ack_store_tests.rs"]
mod tests;
