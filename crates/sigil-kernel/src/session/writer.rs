use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
    time::SystemTime,
};

use anyhow::{Context, Result, bail};
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

use super::*;

const DURABLE_IDENTITY_MAX_BYTES: usize = 512;

static SESSION_WRITER_REGISTRY: OnceLock<
    Mutex<HashMap<PathBuf, Weak<Mutex<LinearSessionWriter>>>>,
> = OnceLock::new();

/// One recovery-critical durable event to append before a protected effect.
#[derive(Debug)]
pub struct DurableAuditRecord {
    pub(super) event_type: DurableEventType,
    pub(super) payload: Value,
    pub(super) record_id: String,
    pub(super) event_id: Option<String>,
    pub(super) correlation_id: Option<String>,
    pub(super) causation_id: Option<String>,
    pub(super) authorization_id: Option<String>,
}

impl DurableAuditRecord {
    /// Creates a recovery-critical audit record with stable live identities.
    ///
    /// # Errors
    ///
    /// Returns [`DurableAuditError::InvalidRequest`] when the event is not recovery-critical, an
    /// identity is invalid, `record_id` is absent from the checksum-covered payload, or the event
    /// payload does not pass its registered session-entry/typed-domain schema. A `None` correlation
    /// is valid for transport-level audit records.
    pub fn new(
        event_type: DurableEventType,
        payload: Value,
        record_id: impl Into<String>,
        correlation_id: Option<String>,
    ) -> std::result::Result<Self, DurableAuditError> {
        if event_type.sync_class() != Some(EventSyncClass::RecoveryCritical) {
            return Err(DurableAuditError::InvalidRequest {
                reason: format!(
                    "{} is not a recovery-critical durable event",
                    event_type.as_str()
                ),
            });
        }
        let record_id = validate_identity("record_id", record_id.into())?;
        if !json_contains_identity(&payload, &record_id) {
            return Err(DurableAuditError::InvalidRequest {
                reason: "record_id must be present in the checksum-covered payload".to_owned(),
            });
        }
        let correlation_id = correlation_id
            .map(|value| validate_identity("correlation_id", value))
            .transpose()?;
        validate_strict_audit_payload(event_type, &payload)?;
        Ok(Self {
            event_type,
            payload,
            record_id,
            event_id: None,
            correlation_id,
            causation_id: None,
            authorization_id: None,
        })
    }

    /// Uses a caller-preallocated durable event identity.
    ///
    /// Preallocation lets a recovery coordinator use the event as a correlation root before the
    /// append begins and later reconcile an ambiguous append acknowledgement by exact identity.
    ///
    /// # Errors
    ///
    /// Returns [`DurableAuditError::InvalidRequest`] when the identity is malformed or would make
    /// the event causally depend on itself.
    pub fn with_event_id(
        mut self,
        event_id: impl Into<String>,
    ) -> std::result::Result<Self, DurableAuditError> {
        let event_id = validate_identity("event_id", event_id.into())?;
        if self.causation_id.as_deref() == Some(event_id.as_str()) {
            return Err(DurableAuditError::InvalidRequest {
                reason: "causation_id must not equal event_id".to_owned(),
            });
        }
        self.event_id = Some(event_id);
        Ok(self)
    }

    /// Binds the direct durable predecessor of this event.
    ///
    /// # Errors
    ///
    /// Returns [`DurableAuditError::InvalidRequest`] when the identity is malformed, no
    /// correlation root is present, or the event would causally depend on itself.
    pub fn with_causation_id(
        mut self,
        causation_id: impl Into<String>,
    ) -> std::result::Result<Self, DurableAuditError> {
        if self.event_id.is_none() {
            return Err(DurableAuditError::InvalidRequest {
                reason: "causation_id requires a preallocated event_id".to_owned(),
            });
        }
        if self.correlation_id.is_none() {
            return Err(DurableAuditError::InvalidRequest {
                reason: "causation_id requires correlation_id".to_owned(),
            });
        }
        let causation_id = validate_identity("causation_id", causation_id.into())?;
        if self.event_id.as_deref() == Some(causation_id.as_str()) {
            return Err(DurableAuditError::InvalidRequest {
                reason: "causation_id must not equal event_id".to_owned(),
            });
        }
        self.causation_id = Some(causation_id);
        Ok(self)
    }

    /// Builds the exact lookup used after an append acknowledgement becomes ambiguous.
    ///
    /// # Errors
    ///
    /// Returns [`DurableAuditError::InvalidRequest`] unless this record has a preallocated event
    /// identity and still satisfies the strict durable payload contract.
    pub fn reconciliation_expectation(
        &self,
        session_id: impl Into<String>,
    ) -> std::result::Result<DurableEventReconciliationExpectation, DurableAuditError> {
        DurableEventReconciliationExpectation::new(
            session_id,
            self.event_type,
            self.event_id
                .clone()
                .ok_or_else(|| DurableAuditError::InvalidRequest {
                    reason: "reconciliation requires a preallocated event_id".to_owned(),
                })?,
            self.payload.clone(),
            self.correlation_id.clone(),
            self.causation_id.clone(),
        )
    }

    /// Binds an optional authorization identity to the live append receipt.
    ///
    /// # Errors
    ///
    /// Returns [`DurableAuditError::InvalidRequest`] for an invalid identity or when the identity
    /// is absent from the checksum-covered payload.
    pub fn with_authorization_id(
        mut self,
        authorization_id: impl Into<String>,
    ) -> std::result::Result<Self, DurableAuditError> {
        let authorization_id = validate_identity("authorization_id", authorization_id.into())?;
        if !json_contains_identity(&self.payload, &authorization_id) {
            return Err(DurableAuditError::InvalidRequest {
                reason: "authorization_id must be present in the checksum-covered payload"
                    .to_owned(),
            });
        }
        self.authorization_id = Some(authorization_id);
        Ok(self)
    }
}

/// An ordered recovery-critical append batch.
#[derive(Debug)]
pub struct DurableAuditBatch {
    pub(super) batch_id: String,
    pub(super) records: Vec<DurableAuditRecord>,
}

impl DurableAuditBatch {
    /// Creates a non-empty batch with unique record identities.
    ///
    /// # Errors
    ///
    /// Returns [`DurableAuditError::InvalidRequest`] for an invalid batch identity, an empty
    /// batch, or duplicate record identities.
    pub fn new(
        batch_id: impl Into<String>,
        records: Vec<DurableAuditRecord>,
    ) -> std::result::Result<Self, DurableAuditError> {
        let batch_id = validate_identity("batch_id", batch_id.into())?;
        if records.is_empty() {
            return Err(DurableAuditError::InvalidRequest {
                reason: "durable audit batch must contain at least one record".to_owned(),
            });
        }
        let mut record_ids = std::collections::BTreeSet::new();
        if records
            .iter()
            .any(|record| !record_ids.insert(record.record_id.as_str()))
        {
            return Err(DurableAuditError::InvalidRequest {
                reason: "durable audit batch contains duplicate record_id values".to_owned(),
            });
        }
        let mut event_ids = std::collections::BTreeSet::new();
        if records.iter().any(|record| {
            record
                .event_id
                .as_ref()
                .is_some_and(|event_id| !event_ids.insert(event_id.as_str()))
        }) {
            return Err(DurableAuditError::InvalidRequest {
                reason: "durable audit batch contains duplicate event_id values".to_owned(),
            });
        }
        Ok(Self { batch_id, records })
    }
}

/// Typed failure returned by the strict durable-audit path.
#[derive(Debug, Error)]
pub enum DurableAuditError {
    #[error("durable audit writer is unavailable because the session has no durable store")]
    MissingDurableStore,
    #[error("invalid durable audit request: {reason}")]
    InvalidRequest { reason: String },
    #[error("durable audit append failed")]
    AppendFailed(#[source] anyhow::Error),
    #[error("durable append receipt does not match the expected writer or record identities")]
    ReceiptMismatch,
}

/// Durable position for one record in an append receipt.
#[derive(Debug)]
pub struct DurableAppendRecordReceipt {
    event_type: DurableEventType,
    event_id: String,
    event_id_preallocated: bool,
    stream_sequence: u64,
    record_checksum: String,
    record_id: String,
    correlation_id: Option<String>,
    causation_id: Option<String>,
    authorization_id: Option<String>,
    start_offset: u64,
    end_offset: u64,
}

impl DurableAppendRecordReceipt {
    pub fn event_type(&self) -> DurableEventType {
        self.event_type
    }

    pub fn event_id(&self) -> &str {
        &self.event_id
    }

    pub fn stream_sequence(&self) -> u64 {
        self.stream_sequence
    }

    pub fn record_checksum(&self) -> &str {
        &self.record_checksum
    }

    pub fn record_id(&self) -> &str {
        &self.record_id
    }

    pub fn correlation_id(&self) -> Option<&str> {
        self.correlation_id.as_deref()
    }

    pub fn causation_id(&self) -> Option<&str> {
        self.causation_id.as_deref()
    }

    pub fn authorization_id(&self) -> Option<&str> {
        self.authorization_id.as_deref()
    }

    pub fn start_offset(&self) -> u64 {
        self.start_offset
    }

    pub fn end_offset(&self) -> u64 {
        self.end_offset
    }
}

/// Non-serializable proof that one record or batch reached a synced session-file position.
///
/// Receipts intentionally cannot be cloned or constructed by callers. The strict validator
/// consumes them by value before a protected effect can use the resulting permit.
#[derive(Debug)]
pub struct DurableAppendReceipt {
    writer_generation: String,
    session_id: String,
    batch_id: String,
    records: Vec<DurableAppendRecordReceipt>,
    durable_end_offset: u64,
}

impl DurableAppendReceipt {
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn batch_id(&self) -> &str {
        &self.batch_id
    }

    pub fn records(&self) -> &[DurableAppendRecordReceipt] {
        &self.records
    }

    pub fn durable_end_offset(&self) -> u64 {
        self.durable_end_offset
    }
}

/// Expected identities for consuming one strict durable append receipt.
#[derive(Debug)]
pub struct DurableAppendExpectation {
    session_id: String,
    batch_id: String,
    records: Vec<DurableAppendRecordExpectation>,
}

impl DurableAppendExpectation {
    /// Creates an exact receipt expectation.
    ///
    /// # Errors
    ///
    /// Returns [`DurableAuditError::InvalidRequest`] when an identity is invalid or no records are
    /// supplied.
    pub fn new(
        session_id: impl Into<String>,
        batch_id: impl Into<String>,
        records: Vec<DurableAppendRecordExpectation>,
    ) -> std::result::Result<Self, DurableAuditError> {
        if records.is_empty() {
            return Err(DurableAuditError::InvalidRequest {
                reason: "durable append expectation must contain at least one record".to_owned(),
            });
        }
        let mut record_ids = std::collections::BTreeSet::new();
        if records
            .iter()
            .any(|record| !record_ids.insert(record.record_id.as_str()))
        {
            return Err(DurableAuditError::InvalidRequest {
                reason: "durable append expectation contains duplicate record_id values".to_owned(),
            });
        }
        Ok(Self {
            session_id: validate_identity("session_id", session_id.into())?,
            batch_id: validate_identity("batch_id", batch_id.into())?,
            records,
        })
    }
}

/// Expected stable identities for one record in a strict receipt.
#[derive(Debug)]
pub struct DurableAppendRecordExpectation {
    event_type: DurableEventType,
    record_id: String,
    event_id: Option<String>,
    correlation_id: Option<String>,
    causation_id: Option<String>,
    authorization_id: Option<String>,
}

impl DurableAppendRecordExpectation {
    /// Creates a record expectation without an authorization identity.
    ///
    /// # Errors
    ///
    /// Returns [`DurableAuditError::InvalidRequest`] for an invalid identity. A `None` correlation
    /// is matched exactly and is distinct from every query-level correlation.
    pub fn new(
        event_type: DurableEventType,
        record_id: impl Into<String>,
        correlation_id: Option<String>,
    ) -> std::result::Result<Self, DurableAuditError> {
        if event_type.sync_class() != Some(EventSyncClass::RecoveryCritical) {
            return Err(DurableAuditError::InvalidRequest {
                reason: format!(
                    "{} is not a recovery-critical durable event",
                    event_type.as_str()
                ),
            });
        }
        Ok(Self {
            event_type,
            record_id: validate_identity("record_id", record_id.into())?,
            event_id: None,
            correlation_id: correlation_id
                .map(|value| validate_identity("correlation_id", value))
                .transpose()?,
            causation_id: None,
            authorization_id: None,
        })
    }

    /// Requires the receipt to carry a specific preallocated event identity.
    ///
    /// # Errors
    ///
    /// Returns [`DurableAuditError::InvalidRequest`] when the identity is malformed or conflicts
    /// with the expected causal predecessor.
    pub fn with_event_id(
        mut self,
        event_id: impl Into<String>,
    ) -> std::result::Result<Self, DurableAuditError> {
        let event_id = validate_identity("event_id", event_id.into())?;
        if self.causation_id.as_deref() == Some(event_id.as_str()) {
            return Err(DurableAuditError::InvalidRequest {
                reason: "causation_id must not equal event_id".to_owned(),
            });
        }
        self.event_id = Some(event_id);
        Ok(self)
    }

    /// Requires the receipt to carry the exact direct predecessor identity.
    ///
    /// # Errors
    ///
    /// Returns [`DurableAuditError::InvalidRequest`] when the identity is malformed, no
    /// correlation root is expected, or the event would causally depend on itself.
    pub fn with_causation_id(
        mut self,
        causation_id: impl Into<String>,
    ) -> std::result::Result<Self, DurableAuditError> {
        if self.event_id.is_none() {
            return Err(DurableAuditError::InvalidRequest {
                reason: "causation_id requires an expected event_id".to_owned(),
            });
        }
        if self.correlation_id.is_none() {
            return Err(DurableAuditError::InvalidRequest {
                reason: "causation_id requires correlation_id".to_owned(),
            });
        }
        let causation_id = validate_identity("causation_id", causation_id.into())?;
        if self.event_id.as_deref() == Some(causation_id.as_str()) {
            return Err(DurableAuditError::InvalidRequest {
                reason: "causation_id must not equal event_id".to_owned(),
            });
        }
        self.causation_id = Some(causation_id);
        Ok(self)
    }

    /// Adds the expected authorization identity.
    ///
    /// # Errors
    ///
    /// Returns [`DurableAuditError::InvalidRequest`] for an invalid identity.
    pub fn with_authorization_id(
        mut self,
        authorization_id: impl Into<String>,
    ) -> std::result::Result<Self, DurableAuditError> {
        self.authorization_id = Some(validate_identity(
            "authorization_id",
            authorization_id.into(),
        )?);
        Ok(self)
    }
}

/// Exact event identity and payload used to reconcile an ambiguous append acknowledgement.
#[derive(Debug, Clone)]
pub struct DurableEventReconciliationExpectation {
    session_id: String,
    event_type: DurableEventType,
    event_id: String,
    payload: Value,
    correlation_id: Option<String>,
    causation_id: Option<String>,
}

impl DurableEventReconciliationExpectation {
    /// Creates one strict recovery-critical lookup.
    ///
    /// # Errors
    ///
    /// Returns [`DurableAuditError::InvalidRequest`] when identities or payload schema are invalid.
    pub fn new(
        session_id: impl Into<String>,
        event_type: DurableEventType,
        event_id: impl Into<String>,
        payload: Value,
        correlation_id: Option<String>,
        causation_id: Option<String>,
    ) -> std::result::Result<Self, DurableAuditError> {
        if event_type.sync_class() != Some(EventSyncClass::RecoveryCritical) {
            return Err(DurableAuditError::InvalidRequest {
                reason: format!(
                    "{} is not a recovery-critical durable event",
                    event_type.as_str()
                ),
            });
        }
        let event_id = validate_identity("event_id", event_id.into())?;
        let correlation_id = correlation_id
            .map(|value| validate_identity("correlation_id", value))
            .transpose()?;
        let causation_id = causation_id
            .map(|value| validate_identity("causation_id", value))
            .transpose()?;
        if causation_id.is_some() && correlation_id.is_none() {
            return Err(DurableAuditError::InvalidRequest {
                reason: "causation_id requires correlation_id".to_owned(),
            });
        }
        if causation_id.as_deref() == Some(event_id.as_str()) {
            return Err(DurableAuditError::InvalidRequest {
                reason: "causation_id must not equal event_id".to_owned(),
            });
        }
        validate_strict_audit_payload(event_type, &payload)?;
        Ok(Self {
            session_id: validate_identity("session_id", session_id.into())?,
            event_type,
            event_id,
            payload,
            correlation_id,
            causation_id,
        })
    }

    /// Creates one strict reconciliation lookup for a direct payload whose validation is bound
    /// to the actual durable session id.
    ///
    /// Some direct payload contracts derive identities from the owning session. Validating those
    /// payloads through the ordinary schema probe would substitute a synthetic session id and
    /// incorrectly reject valid records. This constructor validates the exact same payload with
    /// the caller's real session scope before it can be appended or reconciled.
    pub(super) fn new_session_bound_direct(
        session_id: impl Into<String>,
        event_type: DurableEventType,
        event_id: impl Into<String>,
        payload: Value,
        correlation_id: Option<String>,
        causation_id: Option<String>,
    ) -> std::result::Result<Self, DurableAuditError> {
        let session_id = validate_identity("session_id", session_id.into())?;
        let event_id = validate_identity("event_id", event_id.into())?;
        if event_type.sync_class() != Some(EventSyncClass::RecoveryCritical) {
            return Err(DurableAuditError::InvalidRequest {
                reason: format!(
                    "{} is not a recovery-critical durable event",
                    event_type.as_str()
                ),
            });
        }
        let correlation_id = correlation_id
            .map(|value| validate_identity("correlation_id", value))
            .transpose()?;
        let causation_id = causation_id
            .map(|value| validate_identity("causation_id", value))
            .transpose()?;
        if causation_id.is_some() && correlation_id.is_none() {
            return Err(DurableAuditError::InvalidRequest {
                reason: "causation_id requires correlation_id".to_owned(),
            });
        }
        if causation_id.as_deref() == Some(event_id.as_str()) {
            return Err(DurableAuditError::InvalidRequest {
                reason: "causation_id must not equal event_id".to_owned(),
            });
        }
        let mut probe = StoredEvent::new(
            event_type,
            event_type
                .expected_event_class()
                .ok_or_else(|| DurableAuditError::InvalidRequest {
                    reason: format!("{} is not appendable", event_type.as_str()),
                })?,
            event_id.clone(),
            session_id.clone(),
            1,
            payload.clone(),
        )
        .map_err(|error| DurableAuditError::InvalidRequest {
            reason: format!("{} payload is invalid: {error}", event_type.as_str()),
        })?;
        probe.correlation_id = correlation_id.clone();
        probe.causation_id = causation_id.clone();
        probe.record_checksum =
            probe
                .compute_record_checksum()
                .map_err(|error| DurableAuditError::InvalidRequest {
                    reason: format!("{} payload is invalid: {error}", event_type.as_str()),
                })?;
        match decode_typed_stored_event(probe).map_err(|error| {
            DurableAuditError::InvalidRequest {
                reason: format!("{} payload is invalid: {error}", event_type.as_str()),
            }
        })? {
            TypedStoredEventDecode::Known(event)
                if !matches!(*event, TypedDomainEvent::Other(_)) => {}
            TypedStoredEventDecode::Known(_) | TypedStoredEventDecode::UnknownNonCritical(_) => {
                return Err(DurableAuditError::InvalidRequest {
                    reason: format!(
                        "{} has no strict typed audit payload schema",
                        event_type.as_str()
                    ),
                });
            }
        }
        Ok(Self {
            session_id,
            event_type,
            event_id,
            payload,
            correlation_id,
            causation_id,
        })
    }

    pub fn event_id(&self) -> &str {
        &self.event_id
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }
}

/// Result of explicitly re-reading the durable stream after an append acknowledgement failure.
#[derive(Debug)]
pub enum DurableEventReconciliation {
    /// Exactly one matching domain event identity, typed JSON payload and link set is durable.
    ExactPresent(Box<StoredEvent>),
    /// The stream was read under the writer lease and the event identity is absent.
    ConfirmedAbsent,
    /// The identity exists but its type, payload, links, or multiplicity conflicts.
    Conflict { reason: String },
    /// Durable presence could not be established because the stream was not safely readable.
    Indeterminate { reason: String },
}

/// One-shot proof returned after exact receipt validation.
#[derive(Debug)]
pub struct DurableAppendPermit {
    session_id: String,
    batch_id: String,
    durable_end_offset: u64,
}

impl DurableAppendPermit {
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn batch_id(&self) -> &str {
        &self.batch_id
    }

    pub fn durable_end_offset(&self) -> u64 {
        self.durable_end_offset
    }
}

mod sealed {
    pub trait Sealed {}
}

/// Strict durable audit writer used by later pre-egress ordering.
///
/// All methods perform blocking file and synchronization I/O. Async orchestration must call them
/// from its blocking-I/O bridge (for example, `spawn_blocking`) rather than on an async worker.
pub trait DurableAuditWriter: sealed::Sealed + Send + Sync {
    /// Appends and synchronizes one recovery-critical record.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the append or synchronization fails. No receipt is returned
    /// unless the complete record is durable.
    fn append_and_sync(
        &self,
        record: DurableAuditRecord,
    ) -> std::result::Result<DurableAppendReceipt, DurableAuditError>;

    /// Appends and synchronizes one ordered recovery-critical batch.
    ///
    /// # Errors
    ///
    /// Returns a typed error when any write, flush, or synchronization step fails. A partial write
    /// never produces a receipt and poisons the cached tail until reload/recovery.
    fn append_batch_and_sync(
        &self,
        batch: DurableAuditBatch,
    ) -> std::result::Result<DurableAppendReceipt, DurableAuditError>;

    /// Consumes a receipt after matching it to exact expected and persisted identities.
    ///
    /// # Errors
    ///
    /// Returns [`DurableAuditError::ReceiptMismatch`] when any writer, session, batch, record,
    /// correlation, causation, authorization, sequence, offset, event, or checksum binding differs.
    fn validate_and_consume(
        &self,
        receipt: DurableAppendReceipt,
        expectation: DurableAppendExpectation,
    ) -> std::result::Result<DurableAppendPermit, DurableAuditError>;

    /// Re-reads the durable stream after an append acknowledgement becomes ambiguous.
    fn reconcile_event(
        &self,
        expectation: &DurableEventReconciliationExpectation,
    ) -> DurableEventReconciliation;
}

impl sealed::Sealed for JsonlSessionStore {}

impl DurableAuditWriter for JsonlSessionStore {
    fn append_and_sync(
        &self,
        record: DurableAuditRecord,
    ) -> std::result::Result<DurableAppendReceipt, DurableAuditError> {
        let batch_id = record.record_id.clone();
        self.append_audit_batch(DurableAuditBatch {
            batch_id,
            records: vec![record],
        })
        .map_err(DurableAuditError::AppendFailed)
    }

    fn append_batch_and_sync(
        &self,
        batch: DurableAuditBatch,
    ) -> std::result::Result<DurableAppendReceipt, DurableAuditError> {
        self.append_audit_batch(batch)
            .map_err(DurableAuditError::AppendFailed)
    }

    fn validate_and_consume(
        &self,
        receipt: DurableAppendReceipt,
        expectation: DurableAppendExpectation,
    ) -> std::result::Result<DurableAppendPermit, DurableAuditError> {
        self.validate_audit_receipt(receipt, expectation)
            .map_err(|_| DurableAuditError::ReceiptMismatch)
    }

    fn reconcile_event(
        &self,
        expectation: &DurableEventReconciliationExpectation,
    ) -> DurableEventReconciliation {
        self.reconcile_durable_event(expectation)
    }
}

#[derive(Debug, Clone)]
struct SessionWriterTail {
    session_id: String,
    next_sequence: u64,
    durable_offset: u64,
    last_sequence: Option<u64>,
    last_event_id: Option<String>,
    last_record_checksum: Option<String>,
    durable_prefix_hash: String,
    prefix_hasher: Sha256,
    tail_suffix_len: usize,
    tail_suffix_hash: String,
    file_fingerprint: SessionFileFingerprint,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionFileFingerprint {
    len: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    changed_seconds: i64,
    #[cfg(unix)]
    changed_nanoseconds: i64,
}

#[derive(Debug)]
pub(super) struct LinearSessionWriter {
    path: PathBuf,
    generation: String,
    lease_file: Option<File>,
    parent_dir_synced: bool,
    tail: Option<SessionWriterTail>,
    event_links: Option<DurableEventLinkIndex>,
    requires_reload: bool,
    #[cfg(test)]
    full_scan_count: u64,
    #[cfg(test)]
    parent_sync_count: u64,
    #[cfg(test)]
    data_sync_count: u64,
    #[cfg(test)]
    next_fault: Option<SessionWriterFault>,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionWriterFault {
    ParentDirectorySync,
    BeforeWrite,
    PartialFirstRecord,
    BeforeSync,
}

#[derive(Debug)]
pub(super) struct PendingStoredEvent {
    pub(super) event_type: DurableEventType,
    pub(super) event_class: EventClass,
    pub(super) payload: Value,
    pub(super) event_id: Option<String>,
    pub(super) correlation_id: Option<String>,
    pub(super) causation_id: Option<String>,
}

#[derive(Debug, Default)]
struct DurableEventLinkIndex {
    correlations: HashMap<String, Option<String>>,
    duplicate_event_ids: HashSet<String>,
}

impl DurableEventLinkIndex {
    fn from_records(records: &[SessionStreamRecord]) -> Self {
        let mut index = Self::default();
        for record in records {
            let SessionStreamRecord::Stored(event) = record;
            index.insert(record.event_id().to_owned(), event.correlation_id.clone());
        }
        index
    }

    fn insert(&mut self, event_id: String, correlation_id: Option<String>) {
        if self
            .correlations
            .insert(event_id.clone(), correlation_id)
            .is_some()
        {
            self.duplicate_event_ids.insert(event_id);
        }
    }

    fn validate_pending(&self, pending: &[PendingStoredEvent]) -> Result<()> {
        let mut pending_correlations = HashMap::<&str, Option<&str>>::new();
        for event in pending {
            if event.causation_id.is_some() && event.event_id.is_none() {
                bail!("causation_id requires a preallocated event_id");
            }
            if event.causation_id.is_some() && event.correlation_id.is_none() {
                bail!("causation_id requires correlation_id");
            }
            if let Some(event_id) = event.event_id.as_deref()
                && (self.correlations.contains_key(event_id)
                    || pending_correlations.contains_key(event_id))
            {
                bail!("preallocated event_id {event_id} already exists in the durable stream");
            }

            if let Some(correlation_id) = event.correlation_id.as_deref() {
                let self_root = event.event_id.as_deref() == Some(correlation_id);
                let known_root = self.correlations.contains_key(correlation_id)
                    || pending_correlations.contains_key(correlation_id);
                if !self_root && !known_root {
                    bail!(
                        "correlation_id {correlation_id} must be this event_id or an earlier durable event"
                    );
                }
                if !self_root && self.duplicate_event_ids.contains(correlation_id) {
                    bail!("correlation_id {correlation_id} resolves to multiple durable events");
                }
            }

            if let Some(causation_id) = event.causation_id.as_deref() {
                if self.duplicate_event_ids.contains(causation_id) {
                    bail!("causation_id {causation_id} resolves to multiple durable events");
                }
                let predecessor_correlation = pending_correlations
                    .get(causation_id)
                    .copied()
                    .or_else(|| {
                        self.correlations
                            .get(causation_id)
                            .map(Option::as_deref)
                    })
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "causation_id {causation_id} does not reference an earlier durable event"
                        )
                    })?;
                if predecessor_correlation != event.correlation_id.as_deref() {
                    bail!("causation_id {causation_id} belongs to a different correlation chain");
                }
            }

            if let Some(event_id) = event.event_id.as_deref() {
                pending_correlations.insert(event_id, event.correlation_id.as_deref());
            }
        }
        Ok(())
    }

    fn validate_generated_events(&self, events: &[StoredEvent]) -> Result<()> {
        let mut event_ids = HashSet::with_capacity(events.len());
        for event in events {
            if self.correlations.contains_key(&event.event_id)
                || !event_ids.insert(event.event_id.as_str())
            {
                bail!(
                    "generated event_id {} already exists in the durable stream",
                    event.event_id
                );
            }
        }
        Ok(())
    }

    fn extend(&mut self, events: &[StoredEvent]) {
        for event in events {
            self.insert(event.event_id.clone(), event.correlation_id.clone());
        }
    }
}

type StoredEventAppendResult = (Vec<StoredEvent>, Vec<(u64, u64)>);

impl LinearSessionWriter {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            generation: Uuid::new_v4().to_string(),
            lease_file: None,
            parent_dir_synced: false,
            tail: None,
            event_links: None,
            requires_reload: false,
            #[cfg(test)]
            full_scan_count: 0,
            #[cfg(test)]
            parent_sync_count: 0,
            #[cfg(test)]
            data_sync_count: 0,
            #[cfg(test)]
            next_fault: None,
        }
    }

    fn ensure_writer_lease(&mut self) -> Result<()> {
        if self.lease_file.is_some() {
            return Ok(());
        }
        let lease_path = writer_lease_path(&self.path);
        let existed = lease_path.exists();
        let lease = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lease_path)
            .with_context(|| format!("failed to open {}", lease_path.display()))?;
        lock_exclusive_with_retry(&lease, &lease_path)?;
        if !existed {
            sync_parent_dir(&lease_path)?;
        }
        self.lease_file = Some(lease);
        Ok(())
    }

    fn open_locked_data_file(&mut self) -> Result<File> {
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&self.path)
            .with_context(|| format!("failed to open {}", self.path.display()))?;
        lock_exclusive_with_retry(&file, &self.path)?;
        if !self.parent_dir_synced {
            #[cfg(test)]
            if self.next_fault == Some(SessionWriterFault::ParentDirectorySync) {
                self.next_fault = None;
                bail!("injected session parent directory sync failure");
            }
            sync_parent_dir(&self.path)?;
            self.parent_dir_synced = true;
            #[cfg(test)]
            {
                self.parent_sync_count = self.parent_sync_count.saturating_add(1);
            }
        }
        Ok(file)
    }

    fn tail_needs_reload(&self, file: &mut File) -> bool {
        self.requires_reload
            || self
                .tail
                .as_ref()
                .is_none_or(|tail| !tail_matches_file(file, &self.path, tail).unwrap_or(false))
    }

    fn ensure_current_tail(&mut self, file: &mut File) -> Result<()> {
        if self.tail_needs_reload(file) {
            self.reload_from_file(file)?;
        }
        Ok(())
    }

    fn ensure_event_link_index(&mut self, file: &mut File) -> Result<()> {
        if self.event_links.is_none() || self.tail_needs_reload(file) {
            let records = self.reload_from_file(file)?;
            self.event_links = Some(DurableEventLinkIndex::from_records(&records));
        }
        Ok(())
    }

    fn reload_from_file(&mut self, file: &mut File) -> Result<Vec<SessionStreamRecord>> {
        self.event_links = None;
        let previous_tail = self.tail.clone();
        let recovered = recover_tail_if_needed_locked(file, &self.path)?;
        #[cfg(test)]
        {
            self.full_scan_count = self.full_scan_count.saturating_add(1);
        }
        if let Some(previous) = previous_tail.as_ref() {
            validate_reloaded_stream_extends_tail(previous, &recovered.records, file, &self.path)?;
        }
        self.tail = Some(tail_state_from_content(
            file,
            &self.path,
            &recovered.records,
            &recovered.content,
        )?);
        self.requires_reload = false;
        Ok(recovered.records)
    }

    pub(super) fn append_events(
        &mut self,
        pending: Vec<PendingStoredEvent>,
        force_sync: bool,
    ) -> Result<StoredEventAppendResult> {
        if pending.is_empty() {
            bail!("session writer append batch must not be empty");
        }
        for event in &pending {
            validate_pending_session_entry_payload(event)?;
        }
        self.ensure_writer_lease()?;
        let mut file = self.open_locked_data_file()?;
        let validates_event_links = pending
            .iter()
            .any(|event| event.event_id.is_some() || event.causation_id.is_some());
        if validates_event_links {
            self.ensure_event_link_index(&mut file)?;
            self.event_links
                .as_ref()
                .expect("event link index is present after initialization")
                .validate_pending(&pending)?;
        } else {
            self.ensure_current_tail(&mut file)?;
        }
        let (session_id, mut next_sequence, mut prefix_hasher) = {
            let tail = self
                .tail
                .as_ref()
                .context("session writer tail is unavailable after reload")?;
            (
                tail.session_id.clone(),
                tail.next_sequence,
                tail.prefix_hasher.clone(),
            )
        };
        let mut events = Vec::with_capacity(pending.len());
        let mut lines = Vec::with_capacity(pending.len());
        let mut any_recovery_critical = force_sync;
        for pending in pending {
            let event_id = pending.event_id.unwrap_or_else(|| {
                let event_id_seed = event_id_seed(
                    &session_id,
                    next_sequence,
                    pending.event_type,
                    &pending.payload,
                );
                stable_event_uuid("sigil-event", &event_id_seed)
            });
            let mut event = StoredEvent::new(
                pending.event_type,
                pending.event_class,
                event_id,
                session_id.clone(),
                next_sequence,
                pending.payload,
            )?;
            event.correlation_id = pending.correlation_id;
            event.causation_id = pending.causation_id;
            event.record_checksum = event.compute_record_checksum()?;
            any_recovery_critical |= event.sync_class()? != EventSyncClass::NormalEvent;
            lines.push(event.to_json_line()?.into_bytes());
            events.push(event);
            next_sequence = next_sequence
                .checked_add(1)
                .context("session stream sequence overflow")?;
        }
        if let Some(event_links) = self.event_links.as_ref() {
            event_links.validate_generated_events(&events)?;
        }

        file.seek(SeekFrom::End(0))
            .context("failed to seek session log before append batch")?;
        let start_offset = file.stream_position()?;
        let mut offsets = Vec::with_capacity(lines.len());
        let mut cursor = start_offset;
        #[cfg(test)]
        let injected_fault = self.next_fault.take();
        let write_result = (|| -> Result<()> {
            #[cfg(test)]
            if injected_fault == Some(SessionWriterFault::BeforeWrite) {
                bail!("injected stored event pre-write failure");
            }
            #[cfg(test)]
            if injected_fault == Some(SessionWriterFault::PartialFirstRecord) {
                let line = &lines[0];
                let partial_len = (line.len() / 2).max(1);
                file.write_all(&line[..partial_len])
                    .context("failed to inject partial stored event write")?;
                file.flush()
                    .context("failed to flush injected partial stored event write")?;
                bail!("injected partial stored event write failure");
            }
            for line in &lines {
                file.write_all(line)
                    .context("failed to append stored event batch")?;
                prefix_hasher.update(line);
                let end = cursor
                    .checked_add(line.len() as u64)
                    .context("session durable offset overflow")?;
                offsets.push((cursor, end));
                cursor = end;
            }
            file.flush().context("failed to flush stored event batch")?;
            #[cfg(test)]
            if injected_fault == Some(SessionWriterFault::BeforeSync) {
                bail!("injected stored event sync failure");
            }
            if any_recovery_critical {
                file.sync_all()
                    .context("failed to sync stored event batch")?;
            }
            let observed_len = file
                .metadata()
                .context("failed to inspect stored event batch length")?
                .len();
            if observed_len != cursor {
                bail!(
                    "stored event batch durable offset mismatch: expected {cursor}, got {observed_len}"
                );
            }
            Ok(())
        })();
        if let Err(error) = write_result {
            self.requires_reload = true;
            return Err(error);
        }
        #[cfg(test)]
        if any_recovery_critical {
            self.data_sync_count = self.data_sync_count.saturating_add(1);
        }

        let last = events
            .last()
            .expect("non-empty pending events produce at least one stored event");
        let last_line = lines
            .last()
            .expect("non-empty pending events produce at least one line");
        self.tail = Some(SessionWriterTail {
            session_id,
            next_sequence,
            durable_offset: cursor,
            last_sequence: Some(last.stream_sequence),
            last_event_id: Some(last.event_id.clone()),
            last_record_checksum: Some(last.record_checksum.clone()),
            durable_prefix_hash: format!("sha256:{:x}", prefix_hasher.clone().finalize()),
            prefix_hasher,
            tail_suffix_len: last_line.len(),
            tail_suffix_hash: stable_event_hash(last_line),
            file_fingerprint: file_fingerprint(&file, &self.path)?,
        });
        if let Some(event_links) = self.event_links.as_mut() {
            event_links.extend(&events);
        }
        self.requires_reload = false;
        Ok((events, offsets))
    }

    pub(super) fn append_audit_batch(
        &mut self,
        batch: DurableAuditBatch,
    ) -> Result<DurableAppendReceipt> {
        let DurableAuditBatch { batch_id, records } = batch;
        let identities = records
            .iter()
            .map(|record| {
                (
                    record.event_type,
                    record.record_id.clone(),
                    record.event_id.clone(),
                    record.correlation_id.clone(),
                    record.causation_id.clone(),
                    record.authorization_id.clone(),
                )
            })
            .collect::<Vec<_>>();
        let pending = records
            .into_iter()
            .map(|record| PendingStoredEvent {
                event_type: record.event_type,
                event_class: record
                    .event_type
                    .expected_event_class()
                    .expect("appendable durable audit event has an expected class"),
                payload: record.payload,
                event_id: record.event_id,
                correlation_id: record.correlation_id,
                causation_id: record.causation_id,
            })
            .collect::<Vec<_>>();
        let (events, offsets) = self.append_events(pending, true)?;
        let tail = self
            .tail
            .as_ref()
            .context("session writer tail is unavailable after durable audit append")?;
        let records = events
            .into_iter()
            .zip(offsets)
            .zip(identities)
            .map(
                |(
                    (event, (start_offset, end_offset)),
                    (
                        _,
                        record_id,
                        expected_event_id,
                        correlation_id,
                        causation_id,
                        authorization_id,
                    ),
                )| {
                    debug_assert!(
                        expected_event_id
                            .as_deref()
                            .is_none_or(|expected| expected == event.event_id)
                    );
                    DurableAppendRecordReceipt {
                        event_type: event
                            .event_kind()
                            .expect("durable audit append uses a known event type"),
                        event_id: event.event_id,
                        event_id_preallocated: expected_event_id.is_some(),
                        stream_sequence: event.stream_sequence,
                        record_checksum: event.record_checksum,
                        record_id,
                        correlation_id,
                        causation_id,
                        authorization_id,
                        start_offset,
                        end_offset,
                    }
                },
            )
            .collect::<Vec<_>>();
        Ok(DurableAppendReceipt {
            writer_generation: self.generation.clone(),
            session_id: tail.session_id.clone(),
            batch_id,
            records,
            durable_end_offset: tail.durable_offset,
        })
    }

    pub(super) fn validate_audit_receipt(
        &mut self,
        receipt: DurableAppendReceipt,
        expectation: DurableAppendExpectation,
    ) -> Result<DurableAppendPermit> {
        self.ensure_writer_lease()?;
        let mut file = self.open_locked_data_file()?;
        self.ensure_current_tail(&mut file)?;
        let tail = self
            .tail
            .as_ref()
            .context("session writer tail is unavailable during receipt validation")?;
        let durable_bytes_match = receipt_records_match_file(&mut file, &receipt.records)?;
        let records_match = receipt.records.len() == expectation.records.len()
            && receipt
                .records
                .iter()
                .zip(&expectation.records)
                .all(|(actual, expected)| {
                    actual.event_type == expected.event_type
                        && actual.record_id == expected.record_id
                        && if actual.event_id_preallocated {
                            expected.event_id.as_deref() == Some(actual.event_id.as_str())
                        } else {
                            expected
                                .event_id
                                .as_deref()
                                .is_none_or(|event_id| actual.event_id == event_id)
                        }
                        && actual.correlation_id == expected.correlation_id
                        && actual.causation_id == expected.causation_id
                        && actual.authorization_id == expected.authorization_id
                });
        if receipt.writer_generation != self.generation
            || receipt.session_id != expectation.session_id
            || receipt.session_id != tail.session_id
            || receipt.batch_id != expectation.batch_id
            || !durable_bytes_match
            || !records_match
            || receipt.records.is_empty()
            || receipt.durable_end_offset > tail.durable_offset
            || receipt
                .records
                .last()
                .is_none_or(|record| record.end_offset != receipt.durable_end_offset)
            || receipt
                .records
                .iter()
                .any(|record| record.start_offset >= record.end_offset)
            || receipt.records.windows(2).any(|pair| {
                pair[0].end_offset != pair[1].start_offset
                    || pair[0].stream_sequence.checked_add(1) != Some(pair[1].stream_sequence)
            })
        {
            bail!("durable append receipt identity mismatch");
        }
        Ok(DurableAppendPermit {
            session_id: receipt.session_id,
            batch_id: receipt.batch_id,
            durable_end_offset: receipt.durable_end_offset,
        })
    }

    pub(super) fn read_records_writer(&mut self) -> Result<Vec<SessionStreamRecord>> {
        self.ensure_writer_lease()?;
        let mut file = self.open_locked_data_file()?;
        self.reload_from_file(&mut file)
    }

    pub(super) fn cache_event_links_for_audit_batch(
        &mut self,
        batch: &DurableAuditBatch,
        records: &[SessionStreamRecord],
    ) {
        if batch
            .records
            .iter()
            .any(|record| record.event_id.is_some() || record.causation_id.is_some())
        {
            self.event_links = Some(DurableEventLinkIndex::from_records(records));
        }
    }

    pub(super) fn reconcile_event(
        &mut self,
        expectation: &DurableEventReconciliationExpectation,
    ) -> DurableEventReconciliation {
        let (session_id, records) = match (|| -> Result<(String, Vec<SessionStreamRecord>)> {
            self.ensure_writer_lease()?;
            let mut file = self.open_locked_data_file()?;
            let records = self.reload_from_file(&mut file)?;
            file.sync_all()
                .context("failed to sync session stream during reconciliation")?;
            let session_id = self
                .tail
                .as_ref()
                .context("session writer tail is unavailable during reconciliation")?
                .session_id
                .clone();
            Ok((session_id, records))
        })() {
            Ok(result) => result,
            Err(error) => {
                return DurableEventReconciliation::Indeterminate {
                    reason: format!("failed to read durable stream: {error:#}"),
                };
            }
        };
        if session_id != expectation.session_id {
            return DurableEventReconciliation::Conflict {
                reason: format!(
                    "reconciliation expected session {}, found {session_id}",
                    expectation.session_id
                ),
            };
        }
        let matching_identity = records
            .iter()
            .filter(|record| record.event_id() == expectation.event_id)
            .collect::<Vec<_>>();
        if matching_identity.is_empty() {
            return DurableEventReconciliation::ConfirmedAbsent;
        }
        if matching_identity.len() != 1 {
            return DurableEventReconciliation::Conflict {
                reason: format!(
                    "event_id {} appears {} times",
                    expectation.event_id,
                    matching_identity.len()
                ),
            };
        }
        let SessionStreamRecord::Stored(event) = matching_identity[0];
        if event.event_kind() != Some(expectation.event_type)
            || event.event_class
                != expectation
                    .event_type
                    .expected_event_class()
                    .expect("reconciliation only accepts appendable event types")
            || event.payload != expectation.payload
            || event.correlation_id != expectation.correlation_id
            || event.causation_id != expectation.causation_id
        {
            return DurableEventReconciliation::Conflict {
                reason: format!(
                    "event_id {} exists with conflicting durable content",
                    expectation.event_id
                ),
            };
        }
        DurableEventReconciliation::ExactPresent(Box::new(event.clone()))
    }

    pub(super) fn next_sequence(&mut self) -> Result<u64> {
        self.ensure_writer_lease()?;
        let mut file = self.open_locked_data_file()?;
        self.ensure_current_tail(&mut file)?;
        Ok(self
            .tail
            .as_ref()
            .context("session writer tail is unavailable")?
            .next_sequence)
    }

    #[cfg(test)]
    pub(super) fn full_scan_count(&self) -> u64 {
        self.full_scan_count
    }

    #[cfg(test)]
    pub(super) fn parent_sync_count(&self) -> u64 {
        self.parent_sync_count
    }

    #[cfg(test)]
    pub(super) fn data_sync_count(&self) -> u64 {
        self.data_sync_count
    }

    #[cfg(test)]
    pub(super) fn inject_fault(&mut self, fault: SessionWriterFault) {
        self.next_fault = Some(fault);
    }
}

fn validate_pending_session_entry_payload(event: &PendingStoredEvent) -> Result<()> {
    if event.event_type.payload_metadata().storage != DurableEventPayloadStorage::SessionLogEntry {
        return Ok(());
    }
    let Some(entry) = event.payload.get("session_log_entry").cloned() else {
        return Ok(());
    };
    if entry
        .get("control")
        .and_then(|control| control.get("compaction_applied"))
        .is_some()
    {
        bail!("legacy CompactionRecord payload is unsupported in this pre-release build");
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/session_writer_tests.rs"]
mod tests;

pub(super) fn shared_session_writer(
    path: impl Into<PathBuf>,
) -> Result<(PathBuf, Arc<Mutex<LinearSessionWriter>>)> {
    let path = canonical_session_path(path.into())?;
    let registry = SESSION_WRITER_REGISTRY.get_or_init(|| Mutex::new(HashMap::new()));
    let mut registry = registry
        .lock()
        .map_err(|_| anyhow::anyhow!("session writer registry lock poisoned"))?;
    registry.retain(|_, writer| writer.strong_count() > 0);
    if let Some(writer) = registry.get(&path).and_then(Weak::upgrade) {
        return Ok((path, writer));
    }
    let writer = Arc::new(Mutex::new(LinearSessionWriter::new(path.clone())));
    registry.insert(path.clone(), Arc::downgrade(&writer));
    Ok((path, writer))
}

fn canonical_session_path(path: PathBuf) -> Result<PathBuf> {
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .context("failed to resolve current directory for session path")?
            .join(path)
    };
    if let Ok(metadata) = fs::symlink_metadata(&path) {
        if metadata.file_type().is_symlink() {
            return fs::canonicalize(&path)
                .with_context(|| format!("failed to canonicalize {}", path.display()));
        }
        return fs::canonicalize(&path)
            .with_context(|| format!("failed to canonicalize {}", path.display()));
    }
    let file_name = path
        .file_name()
        .context("session path must include a file name")?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    let canonical_parent = fs::canonicalize(parent)
        .with_context(|| format!("failed to canonicalize {}", parent.display()))?;
    Ok(canonical_parent.join(file_name))
}

fn writer_lease_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("session.jsonl");
    path.with_file_name(format!("{file_name}.writer-lock"))
}

fn validate_identity(
    field: &'static str,
    value: String,
) -> std::result::Result<String, DurableAuditError> {
    if value.is_empty()
        || value.len() > DURABLE_IDENTITY_MAX_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(DurableAuditError::InvalidRequest {
            reason: format!("{field} must be non-empty, bounded, and control-free"),
        });
    }
    Ok(value)
}

fn json_contains_identity(value: &Value, identity: &str) -> bool {
    match value {
        Value::String(value) => value == identity,
        Value::Array(values) => values
            .iter()
            .any(|value| json_contains_identity(value, identity)),
        Value::Object(values) => values
            .values()
            .any(|value| json_contains_identity(value, identity)),
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
}

fn validate_strict_audit_payload(
    event_type: DurableEventType,
    payload: &Value,
) -> std::result::Result<(), DurableAuditError> {
    match event_type.payload_metadata().storage {
        DurableEventPayloadStorage::SessionLogEntry => {
            let entry = payload.get("session_log_entry").cloned().ok_or_else(|| {
                DurableAuditError::InvalidRequest {
                    reason: format!(
                        "{} requires a session_log_entry payload",
                        event_type.as_str()
                    ),
                }
            })?;
            let entry = serde_json::from_value::<SessionLogEntry>(entry).map_err(|error| {
                DurableAuditError::InvalidRequest {
                    reason: format!(
                        "{} session_log_entry payload is invalid: {error}",
                        event_type.as_str()
                    ),
                }
            })?;
            if session_entry_event_type(&entry) != event_type {
                return Err(DurableAuditError::InvalidRequest {
                    reason: format!("session_log_entry does not map to {}", event_type.as_str()),
                });
            }
            Ok(())
        }
        DurableEventPayloadStorage::DirectJson => {
            let probe = StoredEvent::new(
                event_type,
                event_type.expected_event_class().ok_or_else(|| {
                    DurableAuditError::InvalidRequest {
                        reason: format!("{} is not appendable", event_type.as_str()),
                    }
                })?,
                "strict-audit-schema-probe".to_owned(),
                "strict-audit-schema-probe".to_owned(),
                1,
                payload.clone(),
            )
            .map_err(|error| DurableAuditError::InvalidRequest {
                reason: format!("{} payload is invalid: {error}", event_type.as_str()),
            })?;
            match decode_typed_stored_event(probe).map_err(|error| {
                DurableAuditError::InvalidRequest {
                    reason: format!("{} payload is invalid: {error}", event_type.as_str()),
                }
            })? {
                TypedStoredEventDecode::Known(event)
                    if !matches!(*event, TypedDomainEvent::Other(_)) =>
                {
                    Ok(())
                }
                TypedStoredEventDecode::Known(_)
                | TypedStoredEventDecode::UnknownNonCritical(_) => {
                    Err(DurableAuditError::InvalidRequest {
                        reason: format!(
                            "{} has no strict typed audit payload schema",
                            event_type.as_str()
                        ),
                    })
                }
            }
        }
    }
}

fn receipt_records_match_file(
    file: &mut File,
    receipts: &[DurableAppendRecordReceipt],
) -> Result<bool> {
    for receipt in receipts {
        let byte_len = receipt
            .end_offset
            .checked_sub(receipt.start_offset)
            .context("durable append receipt offset order is invalid")?;
        let byte_len = usize::try_from(byte_len)
            .context("durable append receipt byte length does not fit usize")?;
        let mut bytes = vec![0_u8; byte_len];
        file.seek(SeekFrom::Start(receipt.start_offset))
            .context("failed to seek durable append receipt record")?;
        file.read_exact(&mut bytes)
            .context("failed to read durable append receipt record")?;
        let line = std::str::from_utf8(&bytes)
            .context("durable append receipt record is not valid UTF-8")?;
        let event = StoredEvent::from_json_str(line)
            .context("durable append receipt record is not a valid stored event")?;
        if event.event_kind() != Some(receipt.event_type)
            || event.event_id != receipt.event_id
            || event.stream_sequence != receipt.stream_sequence
            || event.record_checksum != receipt.record_checksum
            || event.correlation_id != receipt.correlation_id
            || event.causation_id != receipt.causation_id
            || !json_contains_identity(&event.payload, &receipt.record_id)
            || receipt
                .authorization_id
                .as_deref()
                .is_some_and(|identity| !json_contains_identity(&event.payload, identity))
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn tail_state_from_content(
    file: &File,
    path: &Path,
    records: &[SessionStreamRecord],
    content: &[u8],
) -> Result<SessionWriterTail> {
    let tail_start = last_non_empty_record_start(content).unwrap_or(0);
    let tail_suffix = &content[tail_start..];
    let mut prefix_hasher = Sha256::new();
    prefix_hasher.update(content);
    let last = records.last();
    Ok(SessionWriterTail {
        session_id: stream_session_id(records).unwrap_or_else(|| session_id_for_path(path)),
        next_sequence: next_stream_sequence(records),
        durable_offset: content.len() as u64,
        last_sequence: last.map(SessionStreamRecord::stream_sequence),
        last_event_id: last.map(|record| record.event_id().to_owned()),
        last_record_checksum: last.map(|record| record.record_checksum().to_owned()),
        durable_prefix_hash: format!("sha256:{:x}", prefix_hasher.clone().finalize()),
        prefix_hasher,
        tail_suffix_len: tail_suffix.len(),
        tail_suffix_hash: stable_event_hash(tail_suffix),
        file_fingerprint: file_fingerprint(file, path)?,
    })
}

fn last_non_empty_record_start(content: &[u8]) -> Option<usize> {
    let mut start = 0usize;
    let mut last = None;
    for (index, byte) in content.iter().enumerate() {
        if *byte == b'\n' {
            if content[start..index]
                .iter()
                .any(|byte| !byte.is_ascii_whitespace())
            {
                last = Some(start);
            }
            start = index + 1;
        }
    }
    if content[start..]
        .iter()
        .any(|byte| !byte.is_ascii_whitespace())
    {
        last = Some(start);
    }
    last
}

fn tail_matches_file(file: &mut File, path: &Path, tail: &SessionWriterTail) -> Result<bool> {
    if file_fingerprint(file, path)? != tail.file_fingerprint {
        return Ok(false);
    }
    if tail.tail_suffix_len > tail.durable_offset as usize {
        return Ok(false);
    }
    let start = tail
        .durable_offset
        .saturating_sub(tail.tail_suffix_len as u64);
    file.seek(SeekFrom::Start(start))
        .with_context(|| format!("failed to seek {} tail", path.display()))?;
    let mut suffix = vec![0_u8; tail.tail_suffix_len];
    file.read_exact(&mut suffix)
        .with_context(|| format!("failed to read {} tail", path.display()))?;
    Ok(stable_event_hash(&suffix) == tail.tail_suffix_hash)
}

fn validate_reloaded_stream_extends_tail(
    previous: &SessionWriterTail,
    records: &[SessionStreamRecord],
    file: &mut File,
    path: &Path,
) -> Result<()> {
    let observed_len = file
        .metadata()
        .with_context(|| format!("failed to inspect {}", path.display()))?
        .len();
    if observed_len < previous.durable_offset {
        bail!("session stream was truncated while writer owner was active");
    }
    file.seek(SeekFrom::Start(0))
        .with_context(|| format!("failed to seek {} prefix", path.display()))?;
    let mut remaining = previous.durable_offset;
    let mut prefix_hasher = Sha256::new();
    let mut buffer = [0_u8; 16 * 1024];
    while remaining > 0 {
        let read_len = usize::try_from(remaining.min(buffer.len() as u64))
            .context("session durable offset does not fit usize")?;
        file.read_exact(&mut buffer[..read_len])
            .with_context(|| format!("failed to read {} durable prefix", path.display()))?;
        prefix_hasher.update(&buffer[..read_len]);
        remaining -= read_len as u64;
    }
    let observed_prefix_hash = format!("sha256:{:x}", prefix_hasher.finalize());
    if observed_prefix_hash != previous.durable_prefix_hash {
        bail!("session stream prefix changed while writer owner was active");
    }
    let Some(last_sequence) = previous.last_sequence else {
        return Ok(());
    };
    let record = records
        .iter()
        .find(|record| record.stream_sequence() == last_sequence)
        .context("session stream was truncated or replaced while writer owner was active")?;
    if record.session_id() != previous.session_id
        || Some(record.event_id()) != previous.last_event_id.as_deref()
        || Some(record.record_checksum()) != previous.last_record_checksum.as_deref()
    {
        bail!("session stream prefix changed while writer owner was active");
    }
    Ok(())
}

fn file_fingerprint(file: &File, path: &Path) -> Result<SessionFileFingerprint> {
    let file_metadata = file
        .metadata()
        .with_context(|| format!("failed to inspect {}", path.display()))?;
    let path_metadata =
        fs::metadata(path).with_context(|| format!("failed to inspect {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if file_metadata.dev() != path_metadata.dev() || file_metadata.ino() != path_metadata.ino()
        {
            bail!("session file identity changed while writer owner was active");
        }
        Ok(SessionFileFingerprint {
            len: file_metadata.len(),
            modified: file_metadata.modified().ok(),
            device: file_metadata.dev(),
            inode: file_metadata.ino(),
            changed_seconds: file_metadata.ctime(),
            changed_nanoseconds: file_metadata.ctime_nsec(),
        })
    }
    #[cfg(not(unix))]
    {
        if file_metadata.len() != path_metadata.len()
            || file_metadata.modified().ok() != path_metadata.modified().ok()
        {
            bail!("session file identity changed while writer owner was active");
        }
        Ok(SessionFileFingerprint {
            len: file_metadata.len(),
            modified: file_metadata.modified().ok(),
        })
    }
}
