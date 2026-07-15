use std::{
    fs::File,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    DisclosurePresentationError, DisclosurePresentationReceipt, EgressDisclosurePresenter,
    PreEgressDisclosure,
};
use thiserror::Error;

use crate::durable_io::{
    acquire_exclusive_lease, atomic_replace, canonical_durable_path, read_bounded,
};

/// Schema version for synthetic HTTP disclosure replay events.
pub const HTTP_EGRESS_DISCLOSURE_SCHEMA_VERSION: u32 = 1;
const HTTP_EGRESS_DISCLOSURE_CURSOR_PREFIX: &str = "sigil-http-disclosure-v1";
pub(crate) const MAX_HTTP_DISCLOSURE_JOURNAL_RECORDS: usize = 4_096;
pub(crate) const MAX_HTTP_DISCLOSURE_JOURNAL_BYTES: usize = 8 * 1024 * 1024;

/// A dedicated structured disclosure event retained by the synthetic HTTP replay adapter.
///
/// This event only proves that the safe disclosure entered a server-side replay buffer. It neither
/// starts a listener nor proves that a remote subscriber or person observed it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct HttpEgressDisclosureEvent {
    /// Schema version for this dedicated replay record.
    pub schema_version: u32,
    /// Stable event discriminator kept separate from public run events.
    pub event_type: String,
    /// Safe disclosure fields needed by a future replay surface.
    pub disclosure: PreEgressDisclosure,
}

impl HttpEgressDisclosureEvent {
    fn from_disclosure(disclosure: PreEgressDisclosure) -> Self {
        Self {
            schema_version: HTTP_EGRESS_DISCLOSURE_SCHEMA_VERSION,
            event_type: "egress_disclosure".to_owned(),
            disclosure,
        }
    }
}

/// Replayable production disclosure record acknowledged only after durable publication.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct HttpDurableEgressDisclosureRecord {
    /// Schema version for this dedicated replay record.
    pub schema_version: u32,
    /// Stable event discriminator kept separate from run events.
    pub event_type: String,
    /// Monotonic disclosure stream sequence.
    pub sequence: u64,
    /// Cursor accepted by the authenticated disclosure replay route.
    pub replay_id: String,
    /// Safe disclosure payload; raw query/userinfo/credentials are excluded by the kernel type.
    pub disclosure: PreEgressDisclosure,
}

/// Crash-safe bounded production disclosure replay journal.
pub struct HttpDurableEgressDisclosureJournal {
    path: PathBuf,
    max_records: usize,
    sink_fingerprint: String,
    state: Mutex<HttpDurableDisclosureState>,
    _lease: File,
}

impl std::fmt::Debug for HttpDurableEgressDisclosureJournal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HttpDurableEgressDisclosureJournal")
            .field("path", &self.path)
            .field("max_records", &self.max_records)
            .field("sink_fingerprint", &self.sink_fingerprint)
            .finish_non_exhaustive()
    }
}

impl HttpDurableEgressDisclosureJournal {
    /// Opens or initializes a production disclosure journal.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be read, validated, or durably initialized.
    pub fn open(
        path: impl Into<PathBuf>,
        max_records: usize,
    ) -> Result<Self, HttpDurableDisclosureError> {
        if max_records == 0 || max_records > MAX_HTTP_DISCLOSURE_JOURNAL_RECORDS {
            return Err(HttpDurableDisclosureError::InvalidCapacity {
                requested: max_records,
                limit: MAX_HTTP_DISCLOSURE_JOURNAL_RECORDS,
            });
        }
        let path = canonical_durable_path(path.into()).map_err(HttpDurableDisclosureError::io)?;
        let lease = acquire_exclusive_lease(&path).map_err(HttpDurableDisclosureError::io)?;
        let mut state = if path.exists() {
            let bytes = read_bounded(&path, MAX_HTTP_DISCLOSURE_JOURNAL_BYTES)
                .map_err(HttpDurableDisclosureError::io)?;
            serde_json::from_slice::<HttpDurableDisclosureFile>(&bytes)
                .map_err(|error| HttpDurableDisclosureError::Corrupt {
                    message: error.to_string(),
                })?
                .into_state()?
        } else {
            HttpDurableDisclosureState::default()
        };
        state.trim(max_records);
        persist_disclosure_state(&path, &state)?;
        Ok(Self {
            sink_fingerprint: disclosure_sink_fingerprint(&path),
            path,
            max_records,
            state: Mutex::new(state),
            _lease: lease,
        })
    }

    /// Returns the canonical production journal path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the receipt-bound fingerprint of this concrete durable sink.
    #[must_use]
    pub fn sink_fingerprint(&self) -> &str {
        &self.sink_fingerprint
    }

    /// Durably publishes one safe disclosure and returns its replay record.
    ///
    /// # Errors
    ///
    /// Returns an error without mutating acknowledged state when durable publication fails.
    pub fn publish(
        &self,
        disclosure: PreEgressDisclosure,
    ) -> Result<HttpDurableEgressDisclosureRecord, HttpDurableDisclosureError> {
        disclosure
            .validate()
            .map_err(|error| HttpDurableDisclosureError::InvalidDisclosure {
                message: error.to_string(),
            })?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| HttpDurableDisclosureError::Unavailable)?;
        let mut candidate = state.clone();
        let sequence =
            candidate
                .next_sequence
                .checked_add(1)
                .ok_or(HttpDurableDisclosureError::Corrupt {
                    message: "disclosure sequence overflow".to_owned(),
                })?;
        let record = HttpDurableEgressDisclosureRecord {
            schema_version: HTTP_EGRESS_DISCLOSURE_SCHEMA_VERSION,
            event_type: "egress_disclosure".to_owned(),
            sequence,
            replay_id: disclosure_cursor(sequence),
            disclosure,
        };
        candidate.next_sequence = sequence;
        candidate.records.push(record.clone());
        candidate.trim(self.max_records);
        persist_disclosure_state(&self.path, &candidate)?;
        *state = candidate;
        Ok(record)
    }

    /// Replays the retained disclosure suffix after an optional cursor.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed, ahead, or expired cursors.
    pub fn replay_after(
        &self,
        last_event_id: Option<&str>,
    ) -> Result<Vec<HttpDurableEgressDisclosureRecord>, HttpDisclosureReplayError> {
        let after_sequence = last_event_id
            .map(parse_disclosure_cursor)
            .transpose()?
            .unwrap_or(0);
        let state = self
            .state
            .lock()
            .map_err(|_| HttpDisclosureReplayError::Unavailable)?;
        if after_sequence > state.next_sequence {
            return Err(HttpDisclosureReplayError::CursorAhead);
        }
        if after_sequence == state.next_sequence {
            return Ok(Vec::new());
        }
        let oldest_retained = state.records.first().map(|record| record.sequence);
        if after_sequence > 0
            && oldest_retained.is_none_or(|oldest| after_sequence.saturating_add(1) < oldest)
        {
            return Err(HttpDisclosureReplayError::CursorExpired);
        }
        Ok(state
            .records
            .iter()
            .filter(|record| record.sequence > after_sequence)
            .cloned()
            .collect())
    }
}

/// Durable production disclosure storage failures.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HttpDurableDisclosureError {
    /// Stored state is malformed or internally inconsistent.
    #[error("http disclosure journal is corrupt: {message}")]
    Corrupt { message: String },
    /// Configured retention would exceed the hard allocation boundary.
    #[error("http disclosure journal capacity {requested} is outside 1..={limit}")]
    InvalidCapacity { requested: usize, limit: usize },
    /// A deserialized disclosure violated its kernel integrity contract.
    #[error("http disclosure is invalid: {message}")]
    InvalidDisclosure { message: String },
    /// Serialized journal state exceeded the hard durable-file boundary.
    #[error("http disclosure journal is too large: {bytes} bytes exceeds {limit}")]
    JournalTooLarge { bytes: usize, limit: usize },
    /// In-process durable state is unavailable.
    #[error("http disclosure journal is unavailable")]
    Unavailable,
    /// Durable filesystem work failed.
    #[error("http disclosure journal I/O failed: {message}")]
    Io { message: String },
}

impl HttpDurableDisclosureError {
    fn io(error: std::io::Error) -> Self {
        Self::Io {
            message: error.to_string(),
        }
    }
}

/// Production disclosure cursor failures.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HttpDisclosureReplayError {
    /// Cursor syntax or version is invalid.
    #[error("http disclosure replay cursor is invalid")]
    InvalidCursor,
    /// Cursor is newer than the durable disclosure stream.
    #[error("http disclosure replay cursor is ahead of durable events")]
    CursorAhead,
    /// Cursor fell outside bounded retention.
    #[error("http disclosure replay cursor has expired from retained history")]
    CursorExpired,
    /// Durable state cannot be read safely.
    #[error("http disclosure replay is unavailable")]
    Unavailable,
}

#[derive(Debug, Clone, Default)]
struct HttpDurableDisclosureState {
    next_sequence: u64,
    records: Vec<HttpDurableEgressDisclosureRecord>,
}

impl HttpDurableDisclosureState {
    fn trim(&mut self, max_records: usize) {
        let remove = self.records.len().saturating_sub(max_records);
        if remove > 0 {
            self.records.drain(..remove);
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
struct HttpDurableDisclosureFile {
    schema_version: u32,
    next_sequence: u64,
    records: Vec<HttpDurableEgressDisclosureRecord>,
}

impl HttpDurableDisclosureFile {
    fn from_state(state: &HttpDurableDisclosureState) -> Self {
        Self {
            schema_version: HTTP_EGRESS_DISCLOSURE_SCHEMA_VERSION,
            next_sequence: state.next_sequence,
            records: state.records.clone(),
        }
    }

    fn into_state(self) -> Result<HttpDurableDisclosureState, HttpDurableDisclosureError> {
        if self.schema_version != HTTP_EGRESS_DISCLOSURE_SCHEMA_VERSION {
            return Err(HttpDurableDisclosureError::Corrupt {
                message: format!("unsupported schema version {}", self.schema_version),
            });
        }
        if self.records.len() > MAX_HTTP_DISCLOSURE_JOURNAL_RECORDS {
            return Err(HttpDurableDisclosureError::Corrupt {
                message: "disclosure journal record count exceeds its hard boundary".to_owned(),
            });
        }
        let mut previous = 0;
        for record in &self.records {
            if record.schema_version != HTTP_EGRESS_DISCLOSURE_SCHEMA_VERSION
                || record.event_type != "egress_disclosure"
                || record.sequence <= previous
                || (previous > 0 && record.sequence != previous.saturating_add(1))
                || record.replay_id != disclosure_cursor(record.sequence)
            {
                return Err(HttpDurableDisclosureError::Corrupt {
                    message: "invalid retained disclosure record".to_owned(),
                });
            }
            record
                .disclosure
                .validate()
                .map_err(|error| HttpDurableDisclosureError::Corrupt {
                    message: format!("invalid retained disclosure payload: {error}"),
                })?;
            previous = record.sequence;
        }
        if (self.records.is_empty() && self.next_sequence != 0)
            || (!self.records.is_empty() && previous != self.next_sequence)
        {
            return Err(HttpDurableDisclosureError::Corrupt {
                message: "disclosure high watermark does not match its retained suffix".to_owned(),
            });
        }
        Ok(HttpDurableDisclosureState {
            next_sequence: self.next_sequence,
            records: self.records,
        })
    }
}

/// Production presenter backed by the authenticated server's durable disclosure journal.
#[derive(Clone, Debug)]
pub struct HttpDurableEgressDisclosurePresenter {
    journal: Arc<HttpDurableEgressDisclosureJournal>,
}

impl HttpDurableEgressDisclosurePresenter {
    /// Creates a presenter that acknowledges only after atomic durable publication.
    #[must_use]
    pub fn new(journal: Arc<HttpDurableEgressDisclosureJournal>) -> Self {
        Self { journal }
    }
}

#[async_trait]
impl EgressDisclosurePresenter for HttpDurableEgressDisclosurePresenter {
    async fn present(
        &self,
        disclosure: PreEgressDisclosure,
    ) -> Result<DisclosurePresentationReceipt, DisclosurePresentationError> {
        let journal = Arc::clone(&self.journal);
        let pending = disclosure.clone();
        tokio::task::spawn_blocking(move || journal.publish(pending))
            .await
            .map_err(|_| DisclosurePresentationError::WriteFailed)?
            .map_err(|_| DisclosurePresentationError::WriteFailed)?;
        disclosure.presentation_receipt(self.journal.sink_fingerprint())
    }
}

fn persist_disclosure_state(
    path: &Path,
    state: &HttpDurableDisclosureState,
) -> Result<(), HttpDurableDisclosureError> {
    let bytes =
        serde_json::to_vec(&HttpDurableDisclosureFile::from_state(state)).map_err(|error| {
            HttpDurableDisclosureError::Corrupt {
                message: error.to_string(),
            }
        })?;
    if bytes.len() > MAX_HTTP_DISCLOSURE_JOURNAL_BYTES {
        return Err(HttpDurableDisclosureError::JournalTooLarge {
            bytes: bytes.len(),
            limit: MAX_HTTP_DISCLOSURE_JOURNAL_BYTES,
        });
    }
    atomic_replace(path, &bytes).map_err(HttpDurableDisclosureError::io)
}

fn disclosure_cursor(sequence: u64) -> String {
    format!("{HTTP_EGRESS_DISCLOSURE_CURSOR_PREFIX}:{sequence}")
}

fn parse_disclosure_cursor(value: &str) -> Result<u64, HttpDisclosureReplayError> {
    let Some(sequence) = value.strip_prefix(&format!("{HTTP_EGRESS_DISCLOSURE_CURSOR_PREFIX}:"))
    else {
        return Err(HttpDisclosureReplayError::InvalidCursor);
    };
    let sequence = sequence
        .parse::<u64>()
        .map_err(|_| HttpDisclosureReplayError::InvalidCursor)?;
    if sequence == 0 {
        return Err(HttpDisclosureReplayError::InvalidCursor);
    }
    Ok(sequence)
}

fn disclosure_sink_fingerprint(path: &Path) -> String {
    let digest = Sha256::digest(path.to_string_lossy().as_bytes());
    format!("http-durable-disclosure-journal-v1:{digest:x}")
}

#[derive(Debug, Default)]
struct ReplayState {
    events: Vec<HttpEgressDisclosureEvent>,
    closed: bool,
    fail_next_publish: bool,
}

/// In-memory server-side replay buffer for the HTTP synthetic presenter contract.
#[derive(Debug, Default)]
pub struct HttpEgressDisclosureReplayBuffer {
    state: Mutex<ReplayState>,
}

impl HttpEgressDisclosureReplayBuffer {
    /// Creates an empty synthetic disclosure replay buffer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Publishes one structured disclosure event before the presenter acknowledges it.
    pub fn publish(
        &self,
        disclosure: PreEgressDisclosure,
    ) -> Result<(), HttpEgressDisclosureReplayError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| HttpEgressDisclosureReplayError::Unavailable)?;
        if state.closed {
            return Err(HttpEgressDisclosureReplayError::Closed);
        }
        if std::mem::take(&mut state.fail_next_publish) {
            return Err(HttpEgressDisclosureReplayError::PublishFailed);
        }
        state
            .events
            .push(HttpEgressDisclosureEvent::from_disclosure(disclosure));
        Ok(())
    }

    /// Returns a bounded snapshot for synthetic replay assertions and future adapter wiring.
    #[must_use]
    pub fn events(&self) -> Vec<HttpEgressDisclosureEvent> {
        self.state
            .lock()
            .map(|state| state.events.clone())
            .unwrap_or_default()
    }

    /// Closes the synthetic sink so subsequent presentation fails closed.
    pub fn close(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.closed = true;
        }
    }

    #[cfg(test)]
    pub(crate) fn fail_next_publish(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.fail_next_publish = true;
        }
    }
}

/// Errors from the synthetic replay buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum HttpEgressDisclosureReplayError {
    /// The replay sink was closed before publication.
    #[error("http disclosure replay buffer is closed")]
    Closed,
    /// The replay sink rejected this event.
    #[error("http disclosure replay publish failed")]
    PublishFailed,
    /// The replay buffer state is unavailable.
    #[error("http disclosure replay buffer is unavailable")]
    Unavailable,
}

/// Concrete synthetic HTTP presenter used until a real HTTP product surface is separately wired.
#[derive(Clone, Debug)]
pub struct HttpReplayEgressDisclosurePresenter {
    replay: Arc<HttpEgressDisclosureReplayBuffer>,
    sink_fingerprint: &'static str,
}

impl HttpReplayEgressDisclosurePresenter {
    /// Creates a presenter that acknowledges only after replay-buffer publication succeeds.
    #[must_use]
    pub fn new(replay: Arc<HttpEgressDisclosureReplayBuffer>) -> Self {
        Self {
            replay,
            sink_fingerprint: "http-synthetic-replay-buffer-v1",
        }
    }
}

#[async_trait]
impl EgressDisclosurePresenter for HttpReplayEgressDisclosurePresenter {
    async fn present(
        &self,
        disclosure: PreEgressDisclosure,
    ) -> Result<DisclosurePresentationReceipt, DisclosurePresentationError> {
        self.replay
            .publish(disclosure.clone())
            .map_err(|error| match error {
                HttpEgressDisclosureReplayError::Closed => DisclosurePresentationError::SinkClosed,
                HttpEgressDisclosureReplayError::PublishFailed
                | HttpEgressDisclosureReplayError::Unavailable => {
                    DisclosurePresentationError::WriteFailed
                }
            })?;
        disclosure.presentation_receipt(self.sink_fingerprint)
    }
}

#[cfg(test)]
#[path = "tests/disclosure_tests.rs"]
mod tests;
