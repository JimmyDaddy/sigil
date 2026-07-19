use std::{
    collections::BTreeMap,
    fs::File,
    path::{Path, PathBuf},
    sync::Mutex,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    HttpApprovalCommandReceipt, HttpRunCancelCommandReceipt, HttpRunStartCommandReceipt,
    HttpVerificationRerunCommandReceipt,
    durable_io::{acquire_exclusive_lease, atomic_replace, canonical_durable_path, read_bounded},
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
    server_epoch: u64,
    state: Mutex<HttpCommandStoreState>,
    _lease: File,
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
    /// sealed as aborted. They remain retained so a retry cannot silently execute the command a
    /// second time.
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
        persist_state(&path, &state)?;
        Ok(Self {
            path,
            max_identities,
            server_epoch: state.server_epoch,
            state: Mutex::new(state),
            _lease: lease,
        })
    }

    /// Returns the canonical durable store path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn server_epoch(&self) -> u64 {
        self.server_epoch
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
        persist_state(&self.path, &candidate)?;
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
        persist_state(&self.path, &candidate)?;
        *state = candidate;
        Ok(())
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
    Approval(HttpApprovalCommandReceipt),
    Verification(Box<HttpVerificationRerunCommandReceipt>),
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
    atomic_replace(path, &bytes).map_err(HttpCommandStoreError::io)
}

#[cfg(test)]
#[path = "tests/command_store_tests.rs"]
mod tests;
