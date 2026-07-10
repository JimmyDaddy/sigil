use std::{
    collections::HashMap,
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
    pub(super) correlation_id: Option<String>,
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
            correlation_id,
            authorization_id: None,
        })
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
    stream_sequence: u64,
    record_checksum: String,
    record_id: String,
    correlation_id: Option<String>,
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
    correlation_id: Option<String>,
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
            correlation_id: correlation_id
                .map(|value| validate_identity("correlation_id", value))
                .transpose()?,
            authorization_id: None,
        })
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
    /// correlation, authorization, sequence, offset, event, or checksum binding differs.
    fn validate_and_consume(
        &self,
        receipt: DurableAppendReceipt,
        expectation: DurableAppendExpectation,
    ) -> std::result::Result<DurableAppendPermit, DurableAuditError>;
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
    requires_reload: bool,
    #[cfg(test)]
    full_scan_count: u64,
    #[cfg(test)]
    parent_sync_count: u64,
    #[cfg(test)]
    next_fault: Option<SessionWriterFault>,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SessionWriterFault {
    ParentDirectorySync,
    PartialFirstRecord,
    BeforeSync,
}

#[derive(Debug)]
pub(super) struct PendingStoredEvent {
    pub(super) event_type: DurableEventType,
    pub(super) event_class: EventClass,
    pub(super) payload: Value,
    pub(super) correlation_id: Option<String>,
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
            requires_reload: false,
            #[cfg(test)]
            full_scan_count: 0,
            #[cfg(test)]
            parent_sync_count: 0,
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
            .append(true)
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

    fn ensure_current_tail(&mut self, file: &mut File) -> Result<()> {
        let needs_reload = self.requires_reload
            || self
                .tail
                .as_ref()
                .is_none_or(|tail| !tail_matches_file(file, &self.path, tail).unwrap_or(false));
        if needs_reload {
            self.reload_from_file(file)?;
        }
        Ok(())
    }

    fn reload_from_file(&mut self, file: &mut File) -> Result<Vec<SessionStreamRecord>> {
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
        self.ensure_writer_lease()?;
        let mut file = self.open_locked_data_file()?;
        self.ensure_current_tail(&mut file)?;
        let tail = self
            .tail
            .as_ref()
            .context("session writer tail is unavailable after reload")?;
        let mut next_sequence = tail.next_sequence;
        let mut events = Vec::with_capacity(pending.len());
        let mut lines = Vec::with_capacity(pending.len());
        let mut prefix_hasher = tail.prefix_hasher.clone();
        let mut any_recovery_critical = force_sync;
        for pending in pending {
            if !pending.event_type.appendable() {
                bail!(
                    "{} cannot be appended as a v2 event",
                    pending.event_type.as_str()
                );
            }
            let event_id_seed = event_id_seed(
                &tail.session_id,
                next_sequence,
                pending.event_type,
                &pending.payload,
            );
            let event_id = stable_event_uuid("sigil-event", &event_id_seed);
            let mut event = StoredEvent::new(
                pending.event_type,
                pending.event_class,
                event_id,
                tail.session_id.clone(),
                next_sequence,
                pending.payload,
            )?;
            event.correlation_id = pending.correlation_id;
            event.record_checksum = event.compute_record_checksum()?;
            any_recovery_critical |= event.sync_class()? != EventSyncClass::NormalEvent;
            lines.push(event.to_json_line()?.into_bytes());
            events.push(event);
            next_sequence = next_sequence
                .checked_add(1)
                .context("session stream sequence overflow")?;
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

        let last = events
            .last()
            .expect("non-empty pending events produce at least one stored event");
        let last_line = lines
            .last()
            .expect("non-empty pending events produce at least one line");
        self.tail = Some(SessionWriterTail {
            session_id: tail.session_id.clone(),
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
                    record.correlation_id.clone(),
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
                correlation_id: record.correlation_id,
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
                    (_, record_id, correlation_id, authorization_id),
                )| {
                    DurableAppendRecordReceipt {
                        event_type: event
                            .event_kind()
                            .expect("durable audit append uses a known event type"),
                        event_id: event.event_id,
                        stream_sequence: event.stream_sequence,
                        record_checksum: event.record_checksum,
                        record_id,
                        correlation_id,
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
                        && actual.correlation_id == expected.correlation_id
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
    pub(super) fn inject_fault(&mut self, fault: SessionWriterFault) {
        self.next_fault = Some(fault);
    }
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
