use std::{
    collections::BTreeMap,
    fs::File,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    HttpApprovalCommandReceipt, HttpConversationQueueCommandReceipt,
    HttpConversationRecoveryCommandReceipt, HttpIntentDropCommandReceipt,
    HttpPlanDecisionCommandReceipt, HttpRunCancelCommandReceipt, HttpRunStartCommandReceipt,
    HttpTaskIntegrationAcceptanceCommandReceipt, HttpTaskPauseCommandReceipt,
    HttpTerminalTaskCancelCommandReceipt, HttpUserInputDecisionCommandReceipt,
    HttpVerificationRerunCommandReceipt,
    durable_io::{acquire_exclusive_lease, atomic_replace, canonical_durable_path, read_bounded},
};
use sigil_runtime::managed_storage_writer::{
    ManagedStorageWriterAdapterV1, ManagedStorageWriterLeaseV1, StorageWriterChannelV1,
};

const HTTP_COMMAND_STORE_SCHEMA_VERSION: u32 = 1;
const MAX_HTTP_COMMAND_IDENTITIES: usize = 4_096;
const MAX_HTTP_COMMAND_STORE_BYTES: usize = 16 * 1024 * 1024;
const MAX_HTTP_COMMAND_IDENTITY_PART_BYTES: usize = 512;
pub(crate) const HTTP_DURABLE_COMMAND_PROMPT_OMISSION: &str =
    "[omitted from durable command receipt]";

/// Crash-safe command identity storage used by the production HTTP registry.
pub struct HttpDurableCommandStore {
    path: PathBuf,
    max_identities: usize,
    server_epoch: Mutex<u64>,
    state: Mutex<HttpCommandStoreState>,
    managed_writer: Mutex<Option<ManagedCommandWriter>>,
    _lease: File,
}

struct ManagedCommandWriter {
    writer: Arc<ManagedStorageWriterAdapterV1>,
    lease: Mutex<Option<ManagedStorageWriterLeaseV1>>,
    channel: StorageWriterChannelV1,
}

impl ManagedCommandWriter {
    fn new(
        writer: Arc<ManagedStorageWriterAdapterV1>,
        key: &str,
        channel: StorageWriterChannelV1,
    ) -> Result<Self, HttpCommandStoreError> {
        let lease = writer
            .acquire_named(channel, key)
            .map_err(|error| HttpCommandStoreError::io(std::io::Error::other(error.to_string())))?;
        Ok(Self {
            writer,
            lease: Mutex::new(Some(lease)),
            channel,
        })
    }

    fn read_snapshot(&self) -> Result<Vec<u8>, HttpCommandStoreError> {
        let lease = self
            .lease
            .lock()
            .map_err(|_| HttpCommandStoreError::Unavailable)?;
        let Some(lease) = lease.as_ref() else {
            return Err(HttpCommandStoreError::Unavailable);
        };
        self.writer
            .read_record_bytes(lease, MAX_HTTP_COMMAND_STORE_BYTES)
            .map_err(|error| HttpCommandStoreError::io(std::io::Error::other(error.to_string())))
    }

    fn replace_snapshot(&self, bytes: &[u8]) -> Result<(), HttpCommandStoreError> {
        let lease = self
            .lease
            .lock()
            .map_err(|_| HttpCommandStoreError::Unavailable)?;
        let Some(lease) = lease.as_ref() else {
            return Err(HttpCommandStoreError::Unavailable);
        };
        // ApplicationControlLog is an append-log channel even though this adapter stores one
        // bounded canonical snapshot in its namespace. Keep the physical record a complete
        // JSONL line so finalization observes the same channel contract as other application
        // control writers. AdapterIdempotencyLedger retains its historical JSON snapshot shape.
        let record = if self.channel == StorageWriterChannelV1::ApplicationControlLog
            && !bytes.is_empty()
            && !bytes.ends_with(b"\n")
        {
            let mut record = bytes.to_vec();
            record.push(b'\n');
            record
        } else {
            bytes.to_vec()
        };
        self.writer
            .replace_record_bytes(lease, &record)
            .map_err(|error| HttpCommandStoreError::io(std::io::Error::other(error.to_string())))
    }

    fn finalize(&self) {
        let Ok(mut lease) = self.lease.lock() else {
            return;
        };
        if let Some(lease) = lease.take() {
            let _ = self.writer.finalize(lease);
        }
    }
}

impl Drop for ManagedCommandWriter {
    fn drop(&mut self) {
        self.finalize();
    }
}

impl std::fmt::Debug for HttpDurableCommandStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HttpDurableCommandStore")
            .field("path", &self.path)
            .field("max_identities", &self.max_identities)
            .field("server_epoch", &self.server_epoch)
            .finish_non_exhaustive()
    }
}

impl HttpDurableCommandStore {
    /// Opens or creates a bounded command identity store.
    ///
    /// Reservations that did not receive a durable completion before a prior process stopped are
    /// sealed as aborted. They remain retained so ordinary commands cannot silently execute a
    /// second time. An exact `user_input_decision` identity is the narrow exception: its kernel
    /// command is itself append-only and idempotent, so an aborted adapter attempt may be reserved
    /// again to recover a durably accepted answer whose continuation was never registered. A
    /// `Reserved` user-input identity is also retryable after its process-local reservation was
    /// released because completion persistence failed; the registry remains the exclusive
    /// in-process concurrency owner.
    ///
    /// # Errors
    ///
    /// Returns an error when the file is oversized, malformed, already leased, or cannot be
    /// durably initialized.
    pub fn open(
        path: impl Into<PathBuf>,
        max_identities: usize,
    ) -> Result<Self, HttpCommandStoreError> {
        if max_identities == 0 || max_identities > MAX_HTTP_COMMAND_IDENTITIES {
            return Err(HttpCommandStoreError::InvalidCapacity {
                requested: max_identities,
                limit: MAX_HTTP_COMMAND_IDENTITIES,
            });
        }
        let path = canonical_durable_path(path.into()).map_err(HttpCommandStoreError::io)?;
        let lease = acquire_exclusive_lease(&path).map_err(HttpCommandStoreError::io)?;
        let mut state = if path.exists() {
            let bytes = read_bounded(&path, MAX_HTTP_COMMAND_STORE_BYTES)
                .map_err(HttpCommandStoreError::io)?;
            serde_json::from_slice::<HttpCommandStoreFile>(&bytes)
                .map_err(|error| HttpCommandStoreError::Corrupt {
                    message: error.to_string(),
                })?
                .into_state()?
        } else {
            HttpCommandStoreState::default()
        };
        if state.entries.len() > max_identities {
            return Err(HttpCommandStoreError::CapacityExceeded {
                retained: state.entries.len(),
                capacity: max_identities,
            });
        }
        state.seal_incomplete();
        state.server_epoch =
            state
                .server_epoch
                .checked_add(1)
                .ok_or_else(|| HttpCommandStoreError::Corrupt {
                    message: "server epoch exhausted".to_owned(),
                })?;
        // Preserve the legacy store's epoch-on-open behavior until a current-schema boot has
        // successfully attached its managed ledger. The attachment then retires this
        // compatibility file after importing it into the authority owner.
        persist_state(&path, &state)?;
        Ok(Self {
            path,
            max_identities,
            server_epoch: Mutex::new(state.server_epoch),
            state: Mutex::new(state),
            managed_writer: Mutex::new(None),
            _lease: lease,
        })
    }

    /// Attaches command identity persistence to the composed managed idempotency ledger.
    /// Existing legacy state is imported once and the server epoch advances when a managed
    /// ledger from a previous process is reopened.
    pub(crate) fn attach_managed_writer(
        &self,
        writer: Arc<ManagedStorageWriterAdapterV1>,
        key: &str,
    ) -> Result<(), HttpCommandStoreError> {
        self.attach_writer(
            writer,
            key,
            StorageWriterChannelV1::AdapterIdempotencyLedger,
        )
    }

    /// Performs the one-time HTTP compatibility cutover into the application reservation
    /// authority. The legacy file remains exclusively leased until the managed snapshot has
    /// been durably replaced; if the process stops before the legacy file is retired, the next
    /// boot prefers the managed snapshot and retries retirement under the same leases.
    pub(crate) fn attach_application_writer(
        &self,
        writer: Arc<ManagedStorageWriterAdapterV1>,
        key: &str,
    ) -> Result<(), HttpCommandStoreError> {
        self.attach_writer(writer, key, StorageWriterChannelV1::ApplicationControlLog)
    }

    fn attach_writer(
        &self,
        writer: Arc<ManagedStorageWriterAdapterV1>,
        key: &str,
        channel: StorageWriterChannelV1,
    ) -> Result<(), HttpCommandStoreError> {
        let managed = ManagedCommandWriter::new(writer, key, channel)?;
        let managed_bytes = managed.read_snapshot()?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| HttpCommandStoreError::Unavailable)?;
        let had_managed_state = !managed_bytes.is_empty();
        let mut candidate = if had_managed_state {
            decode_state(&managed_bytes)?
        } else {
            state.clone()
        };
        if candidate.entries.len() > self.max_identities {
            return Err(HttpCommandStoreError::CapacityExceeded {
                retained: candidate.entries.len(),
                capacity: self.max_identities,
            });
        }
        candidate.seal_incomplete();
        if had_managed_state {
            candidate.server_epoch = candidate.server_epoch.checked_add(1).ok_or_else(|| {
                HttpCommandStoreError::Corrupt {
                    message: "server epoch exhausted".to_owned(),
                }
            })?;
        }
        let bytes = encode_state(&candidate)?;
        managed.replace_snapshot(&bytes)?;
        *state = candidate.clone();
        let mut epoch = self
            .server_epoch
            .lock()
            .map_err(|_| HttpCommandStoreError::Unavailable)?;
        *epoch = candidate.server_epoch;
        drop(epoch);
        let mut attached = self
            .managed_writer
            .lock()
            .map_err(|_| HttpCommandStoreError::Unavailable)?;
        if attached.is_some() {
            return Err(HttpCommandStoreError::Unavailable);
        }
        *attached = Some(managed);
        // A successful import makes the managed record the only writable authority. Retire the
        // compatibility pathname even when it pre-dated this process; keeping it would leave a
        // second durable-looking source that future code could accidentally reopen.
        if self.path.exists() {
            std::fs::remove_file(&self.path).map_err(HttpCommandStoreError::io)?;
            if let Some(parent) = self.path.parent() {
                File::open(parent)
                    .and_then(|file| file.sync_all())
                    .map_err(HttpCommandStoreError::io)?;
            }
        }
        Ok(())
    }

    /// Returns the canonical durable store path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn server_epoch(&self) -> u64 {
        *self
            .server_epoch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn reserve(
        &self,
        identity: HttpStoredCommandIdentity,
    ) -> Result<HttpStoredCommandClaim, HttpCommandStoreError> {
        identity.validate()?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| HttpCommandStoreError::Unavailable)?;
        if let Some(existing) = state.entries.get(&identity.key) {
            if existing.identity != identity {
                return Ok(HttpStoredCommandClaim::Conflict);
            }
            if matches!(
                existing.completion,
                HttpStoredCommandCompletion::Reserved | HttpStoredCommandCompletion::Aborted
            ) && identity.kind == "user_input_decision"
            {
                if existing.completion == HttpStoredCommandCompletion::Aborted {
                    let mut candidate = state.clone();
                    candidate
                        .entries
                        .get_mut(&identity.key)
                        .ok_or(HttpCommandStoreError::ReservationMissing)?
                        .completion = HttpStoredCommandCompletion::Reserved;
                    self.persist_state(&candidate)?;
                    *state = candidate;
                }
                return Ok(HttpStoredCommandClaim::Execute);
            }
            return Ok(HttpStoredCommandClaim::Existing(Box::new(
                existing.completion.clone(),
            )));
        }
        if state.entries.len() >= self.max_identities {
            return Err(HttpCommandStoreError::Saturated);
        }
        let mut candidate = state.clone();
        candidate.entries.insert(
            identity.key.clone(),
            HttpStoredCommandEntry {
                identity,
                completion: HttpStoredCommandCompletion::Reserved,
            },
        );
        self.persist_state(&candidate)?;
        *state = candidate;
        Ok(HttpStoredCommandClaim::Execute)
    }

    pub(crate) fn complete(
        &self,
        identity: &HttpStoredCommandIdentity,
        completion: HttpStoredCommandCompletion,
    ) -> Result<(), HttpCommandStoreError> {
        if completion == HttpStoredCommandCompletion::Reserved {
            return Err(HttpCommandStoreError::InvalidCompletion);
        }
        validate_completion(identity, &completion)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| HttpCommandStoreError::Unavailable)?;
        let existing = state
            .entries
            .get(&identity.key)
            .ok_or(HttpCommandStoreError::ReservationMissing)?;
        if existing.identity != *identity {
            return Err(HttpCommandStoreError::IdentityConflict);
        }
        if existing.completion == completion {
            return Ok(());
        }
        if existing.completion != HttpStoredCommandCompletion::Reserved {
            return Err(HttpCommandStoreError::CompletionConflict);
        }
        let mut candidate = state.clone();
        candidate
            .entries
            .get_mut(&identity.key)
            .ok_or(HttpCommandStoreError::ReservationMissing)?
            .completion = completion;
        self.persist_state(&candidate)?;
        *state = candidate;
        Ok(())
    }

    fn persist_state(&self, state: &HttpCommandStoreState) -> Result<(), HttpCommandStoreError> {
        let bytes = encode_state(state)?;
        let managed = self
            .managed_writer
            .lock()
            .map_err(|_| HttpCommandStoreError::Unavailable)?;
        if let Some(managed) = managed.as_ref() {
            managed.replace_snapshot(&bytes)
        } else {
            atomic_replace(&self.path, &bytes).map_err(HttpCommandStoreError::io)
        }
    }
}

/// Durable command identity store failures.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HttpCommandStoreError {
    /// Configured capacity exceeds the audited hard boundary.
    #[error("http command store capacity {requested} is outside 1..={limit}")]
    InvalidCapacity { requested: usize, limit: usize },
    /// Existing durable state cannot fit within the requested capacity without unsafe eviction.
    #[error("http command store retains {retained} identities, exceeding capacity {capacity}")]
    CapacityExceeded { retained: usize, capacity: usize },
    /// The configured bounded identity store is full.
    #[error("http command store is at its bounded identity capacity")]
    Saturated,
    /// Durable state is malformed or violates its identity contract.
    #[error("http command store is corrupt: {message}")]
    Corrupt { message: String },
    /// A command identity component is empty or exceeds its safe bound.
    #[error("http command identity is invalid")]
    InvalidIdentity,
    /// A completion attempted to use the internal reserved marker.
    #[error("http command completion is invalid")]
    InvalidCompletion,
    /// No durable reservation exists for the completion.
    #[error("http command reservation is missing")]
    ReservationMissing,
    /// The same durable key was associated with different request material.
    #[error("http command identity conflicts with its durable reservation")]
    IdentityConflict,
    /// An already completed identity was offered a different terminal completion.
    #[error("http command completion conflicts with its durable terminal")]
    CompletionConflict,
    /// In-process durable state is unavailable.
    #[error("http command store is unavailable")]
    Unavailable,
    /// Filesystem persistence failed.
    #[error("http command store I/O failed: {message}")]
    Io { message: String },
    /// Serialized state exceeded the hard file boundary.
    #[error("http command store is too large: {bytes} bytes exceeds {limit}")]
    StoreTooLarge { bytes: usize, limit: usize },
}

impl HttpCommandStoreError {
    fn io(error: std::io::Error) -> Self {
        Self::Io {
            message: error.to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(crate) struct HttpStoredCommandKey {
    pub(crate) session_id: String,
    pub(crate) client_id: String,
    pub(crate) command_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(crate) struct HttpStoredCommandIdentity {
    pub(crate) key: HttpStoredCommandKey,
    pub(crate) kind: String,
    pub(crate) fingerprint_sha256: String,
}

impl HttpStoredCommandIdentity {
    fn validate(&self) -> Result<(), HttpCommandStoreError> {
        for value in [
            &self.key.session_id,
            &self.key.client_id,
            &self.key.command_id,
            &self.kind,
        ] {
            if value.trim().is_empty()
                || value.len() > MAX_HTTP_COMMAND_IDENTITY_PART_BYTES
                || value.chars().any(char::is_control)
            {
                return Err(HttpCommandStoreError::InvalidIdentity);
            }
        }
        if self.fingerprint_sha256.len() != 64
            || !self
                .fingerprint_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(HttpCommandStoreError::InvalidIdentity);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "receipt", rename_all = "snake_case")]
pub(crate) enum HttpStoredCommandCompletion {
    Reserved,
    Start(HttpRunStartCommandReceipt),
    Cancel(HttpRunCancelCommandReceipt),
    Pause(HttpTaskPauseCommandReceipt),
    TerminalCancel(HttpTerminalTaskCancelCommandReceipt),
    Approval(HttpApprovalCommandReceipt),
    Verification(Box<HttpVerificationRerunCommandReceipt>),
    Integration(Box<HttpTaskIntegrationAcceptanceCommandReceipt>),
    PlanDecision(HttpPlanDecisionCommandReceipt),
    UserInputDecision(HttpUserInputDecisionCommandReceipt),
    IntentDrop(Box<HttpIntentDropCommandReceipt>),
    Queue(Box<HttpConversationQueueCommandReceipt>),
    Recovery(Box<HttpConversationRecoveryCommandReceipt>),
    Aborted,
}

pub(crate) enum HttpStoredCommandClaim {
    Execute,
    Existing(Box<HttpStoredCommandCompletion>),
    Conflict,
}

#[derive(Debug, Clone, Default)]
struct HttpCommandStoreState {
    server_epoch: u64,
    entries: BTreeMap<HttpStoredCommandKey, HttpStoredCommandEntry>,
}

impl HttpCommandStoreState {
    fn seal_incomplete(&mut self) {
        for entry in self.entries.values_mut() {
            if entry.completion == HttpStoredCommandCompletion::Reserved {
                entry.completion = HttpStoredCommandCompletion::Aborted;
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HttpStoredCommandEntry {
    identity: HttpStoredCommandIdentity,
    completion: HttpStoredCommandCompletion,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
struct HttpCommandStoreFile {
    schema_version: u32,
    server_epoch: u64,
    entries: Vec<HttpCommandStoreFileEntry>,
}

impl HttpCommandStoreFile {
    fn from_state(state: &HttpCommandStoreState) -> Self {
        Self {
            schema_version: HTTP_COMMAND_STORE_SCHEMA_VERSION,
            server_epoch: state.server_epoch,
            entries: state
                .entries
                .values()
                .map(|entry| HttpCommandStoreFileEntry {
                    identity: entry.identity.clone(),
                    completion: entry.completion.clone(),
                })
                .collect(),
        }
    }

    fn into_state(self) -> Result<HttpCommandStoreState, HttpCommandStoreError> {
        if self.schema_version != HTTP_COMMAND_STORE_SCHEMA_VERSION {
            return Err(HttpCommandStoreError::Corrupt {
                message: format!("unsupported schema version {}", self.schema_version),
            });
        }
        if self.server_epoch == 0 {
            return Err(HttpCommandStoreError::Corrupt {
                message: "server epoch must be positive".to_owned(),
            });
        }
        if self.entries.len() > MAX_HTTP_COMMAND_IDENTITIES {
            return Err(HttpCommandStoreError::Corrupt {
                message: "command identity count exceeds hard limit".to_owned(),
            });
        }
        let mut state = HttpCommandStoreState {
            server_epoch: self.server_epoch,
            entries: BTreeMap::new(),
        };
        for entry in self.entries {
            entry.identity.validate()?;
            validate_completion(&entry.identity, &entry.completion)?;
            let key = entry.identity.key.clone();
            if state
                .entries
                .insert(
                    key,
                    HttpStoredCommandEntry {
                        identity: entry.identity,
                        completion: entry.completion,
                    },
                )
                .is_some()
            {
                return Err(HttpCommandStoreError::Corrupt {
                    message: "duplicate command identity key".to_owned(),
                });
            }
        }
        Ok(state)
    }
}

fn validate_completion(
    identity: &HttpStoredCommandIdentity,
    completion: &HttpStoredCommandCompletion,
) -> Result<(), HttpCommandStoreError> {
    let valid = match completion {
        HttpStoredCommandCompletion::Reserved | HttpStoredCommandCompletion::Aborted => true,
        HttpStoredCommandCompletion::Start(receipt) => {
            identity.kind == "start"
                && receipt.command_id == identity.key.command_id
                && receipt.client_id == identity.key.client_id
                && receipt.session_id == identity.key.session_id
                && receipt.run.session_id == identity.key.session_id
                && receipt.run.prompt_preview == HTTP_DURABLE_COMMAND_PROMPT_OMISSION
                && !receipt.replayed
        }
        HttpStoredCommandCompletion::Cancel(receipt) => {
            identity.kind == "cancel"
                && receipt.command_id == identity.key.command_id
                && receipt.client_id == identity.key.client_id
                && receipt.session_id == identity.key.session_id
                && receipt.run.session_id == identity.key.session_id
                && receipt.run.prompt_preview == HTTP_DURABLE_COMMAND_PROMPT_OMISSION
                && !receipt.replayed
        }
        HttpStoredCommandCompletion::Pause(receipt) => {
            identity.kind == "pause"
                && receipt.command_id == identity.key.command_id
                && receipt.client_id == identity.key.client_id
                && receipt.session_id == identity.key.session_id
                && receipt.run.session_id == identity.key.session_id
                && receipt.run.prompt_preview == HTTP_DURABLE_COMMAND_PROMPT_OMISSION
                && !receipt.replayed
        }
        HttpStoredCommandCompletion::TerminalCancel(receipt) => {
            identity.kind == "terminal_cancel"
                && receipt.command_id == identity.key.command_id
                && receipt.client_id == identity.key.client_id
                && receipt.session_id == identity.key.session_id
                && !receipt.run_id.trim().is_empty()
                && !receipt.terminal_task.task_id.trim().is_empty()
                && !receipt.replayed
        }
        HttpStoredCommandCompletion::Approval(receipt) => {
            identity.kind == "approval"
                && receipt.command_id == identity.key.command_id
                && receipt.client_id == identity.key.client_id
                && receipt.session_id == identity.key.session_id
                && !receipt.replayed
        }
        HttpStoredCommandCompletion::Verification(receipt) => {
            identity.kind == "verification"
                && receipt.command_id == identity.key.command_id
                && receipt.client_id == identity.key.client_id
                && receipt.session_id == identity.key.session_id
                && !receipt.replayed
        }
        HttpStoredCommandCompletion::Integration(receipt) => {
            identity.kind == "integration"
                && receipt.command_id == identity.key.command_id
                && receipt.client_id == identity.key.client_id
                && receipt.session_id == identity.key.session_id
                && !receipt.replayed
        }
        HttpStoredCommandCompletion::PlanDecision(receipt) => {
            identity.kind == "plan_decision"
                && receipt.command_id == identity.key.command_id
                && receipt.client_id == identity.key.client_id
                && receipt.session_id == identity.key.session_id
                && !receipt.replayed
        }
        HttpStoredCommandCompletion::UserInputDecision(receipt) => {
            identity.kind == "user_input_decision"
                && receipt.command_id == identity.key.command_id
                && receipt.client_id == identity.key.client_id
                && receipt.session_id == identity.key.session_id
                && !receipt
                    .request
                    .identity
                    .session_scope_id
                    .as_str()
                    .trim()
                    .is_empty()
                && !receipt.replayed
        }
        HttpStoredCommandCompletion::IntentDrop(receipt) => {
            identity.kind == "intent_drop"
                && receipt.command_id == identity.key.command_id
                && receipt.client_id == identity.key.client_id
                && receipt.session_id == identity.key.session_id
                && !receipt.replayed
        }
        HttpStoredCommandCompletion::Queue(receipt) => {
            identity.kind == "queue"
                && receipt.command_id == identity.key.command_id
                && receipt.client_id == identity.key.client_id
                && receipt.session_id == identity.key.session_id
                && receipt.queue.session_id == identity.key.session_id
                && !receipt.replayed
        }
        HttpStoredCommandCompletion::Recovery(receipt) => {
            identity.kind == "recovery"
                && receipt.command_id == identity.key.command_id
                && receipt.client_id == identity.key.client_id
                && receipt.session_id == identity.key.session_id
                && !receipt.replayed
        }
    };
    if valid {
        Ok(())
    } else {
        Err(HttpCommandStoreError::Corrupt {
            message: "command completion does not match its durable identity".to_owned(),
        })
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
struct HttpCommandStoreFileEntry {
    identity: HttpStoredCommandIdentity,
    completion: HttpStoredCommandCompletion,
}

fn persist_state(path: &Path, state: &HttpCommandStoreState) -> Result<(), HttpCommandStoreError> {
    let bytes = encode_state(state)?;
    atomic_replace(path, &bytes).map_err(HttpCommandStoreError::io)
}

fn encode_state(state: &HttpCommandStoreState) -> Result<Vec<u8>, HttpCommandStoreError> {
    let bytes = serde_json::to_vec(&HttpCommandStoreFile::from_state(state)).map_err(|error| {
        HttpCommandStoreError::Corrupt {
            message: error.to_string(),
        }
    })?;
    if bytes.len() > MAX_HTTP_COMMAND_STORE_BYTES {
        return Err(HttpCommandStoreError::StoreTooLarge {
            bytes: bytes.len(),
            limit: MAX_HTTP_COMMAND_STORE_BYTES,
        });
    }
    Ok(bytes)
}

fn decode_state(bytes: &[u8]) -> Result<HttpCommandStoreState, HttpCommandStoreError> {
    serde_json::from_slice::<HttpCommandStoreFile>(bytes)
        .map_err(|error| HttpCommandStoreError::Corrupt {
            message: error.to_string(),
        })?
        .into_state()
}

#[cfg(test)]
#[path = "tests/command_store_tests.rs"]
mod tests;
