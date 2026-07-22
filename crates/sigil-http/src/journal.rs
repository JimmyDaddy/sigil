use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    path::{Path, PathBuf},
    sync::Mutex,
};

use serde::{Deserialize, Serialize};
use sigil_kernel::MAX_EVENT_BYTES;
use thiserror::Error as ThisError;

use crate::durable_io::{
    acquire_exclusive_lease, atomic_replace, canonical_durable_path, read_bounded,
};
use crate::sse::{
    HTTP_PROTOCOL_EVENT_SCHEMA_VERSION, HttpProtocolCursor, HttpProtocolEvent,
    HttpProtocolReplayError,
};

const HTTP_PROTOCOL_JOURNAL_SCHEMA_VERSION: u32 = 3;
const HTTP_PROTOCOL_JOURNAL_PREVIOUS_SCHEMA_VERSION: u32 = 2;
const HTTP_PROTOCOL_EVENT_PREVIOUS_SCHEMA_VERSION: u32 = 1;
pub(crate) const MAX_HTTP_PROTOCOL_JOURNAL_EVENTS: usize = 4_096;
pub(crate) const MAX_HTTP_PROTOCOL_JOURNAL_BYTES: usize = 16 * 1024 * 1024;

/// Crash-safe bounded durable storage for replayable HTTP protocol events.
pub struct HttpDurableProtocolJournal {
    path: PathBuf,
    max_events: usize,
    state: Mutex<HttpProtocolJournalState>,
    _lease: File,
}

impl HttpDurableProtocolJournal {
    /// Opens or creates a durable protocol journal.
    ///
    /// # Errors
    ///
    /// Returns an error when the journal cannot be read, validated, or initialized atomically.
    pub fn open(
        path: impl Into<PathBuf>,
        max_events: usize,
    ) -> Result<Self, HttpProtocolJournalError> {
        if max_events == 0 || max_events > MAX_HTTP_PROTOCOL_JOURNAL_EVENTS {
            return Err(HttpProtocolJournalError::InvalidCapacity {
                requested: max_events,
                limit: MAX_HTTP_PROTOCOL_JOURNAL_EVENTS,
            });
        }
        let path = canonical_durable_path(path.into()).map_err(HttpProtocolJournalError::io)?;
        let lease = acquire_exclusive_lease(&path).map_err(HttpProtocolJournalError::io)?;
        let mut state = if path.exists() {
            let bytes = read_bounded(&path, MAX_HTTP_PROTOCOL_JOURNAL_BYTES)
                .map_err(HttpProtocolJournalError::io)?;
            serde_json::from_slice::<HttpProtocolJournalFile>(&bytes)
                .map_err(|error| HttpProtocolJournalError::Corrupt {
                    message: error.to_string(),
                })?
                .into_state()?
        } else {
            HttpProtocolJournalState::default()
        };
        state.seal_recovered_streams();
        state.trim(max_events)?;
        persist_state(&path, &state)?;
        Ok(Self {
            path,
            max_events,
            state: Mutex::new(state),
            _lease: lease,
        })
    }

    /// Returns the canonical journal path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Durably records one replayable event before it can be published to live subscribers.
    ///
    /// # Errors
    ///
    /// Returns an error for transient events, non-monotonic run sequences, or failed durable I/O.
    pub fn append(&self, event: HttpProtocolEvent) -> Result<(), HttpProtocolJournalError> {
        let event = canonical_durable_event(event)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| HttpProtocolJournalError::Unavailable)?;
        let mut candidate = state.clone();
        candidate.append(event)?;
        candidate.trim(self.max_events)?;
        persist_state(&self.path, &candidate)?;
        *state = candidate;
        Ok(())
    }

    /// Replays a retained durable suffix for one run.
    ///
    /// # Errors
    ///
    /// Returns an error when the cursor is invalid, wrong-scope, ahead of the durable stream, or
    /// older than the bounded retention window.
    pub fn replay_run_after(
        &self,
        session_id: &str,
        run_id: &str,
        last_event_id: Option<&str>,
    ) -> Result<Vec<HttpProtocolEvent>, HttpProtocolReplayError> {
        let cursor = parse_scoped_cursor(session_id, run_id, last_event_id)?;
        let after_sequence = cursor.as_ref().map_or(0, |cursor| cursor.sequence);
        let state = self
            .state
            .lock()
            .map_err(|_| HttpProtocolReplayError::JournalUnavailable)?;
        let key = HttpProtocolStreamKey::new(session_id, run_id);
        let Some(watermark) = state.high_watermarks.get(&key).copied() else {
            if after_sequence == 0 {
                return Ok(Vec::new());
            }
            return Err(HttpProtocolReplayError::CursorExpired);
        };
        if after_sequence > watermark.latest_sequence {
            return Err(HttpProtocolReplayError::CursorAhead);
        }
        if after_sequence < watermark.evicted_through_sequence {
            return Err(HttpProtocolReplayError::CursorExpired);
        }
        if after_sequence == watermark.latest_sequence {
            return Ok(Vec::new());
        }
        Ok(state
            .events
            .iter()
            .filter(|event| {
                event.run_event.session_id == session_id
                    && event.run_event.run_id == run_id
                    && event.run_event.sequence > after_sequence
            })
            .cloned()
            .collect())
    }

    /// Reads the latest durable protocol sequence for one run without applying retention cursors.
    ///
    /// # Errors
    ///
    /// Returns an error when durable journal state cannot be read safely.
    pub fn latest_run_sequence(
        &self,
        session_id: &str,
        run_id: &str,
    ) -> Result<Option<u64>, HttpProtocolJournalError> {
        let state = self
            .state
            .lock()
            .map_err(|_| HttpProtocolJournalError::Unavailable)?;
        Ok(state
            .high_watermarks
            .get(&HttpProtocolStreamKey::new(session_id, run_id))
            .map(|watermark| watermark.latest_sequence))
    }
}

/// Durable journal failures.
#[derive(Debug, Clone, PartialEq, Eq, ThisError)]
pub enum HttpProtocolJournalError {
    /// The configured journal is malformed or violates the replay contract.
    #[error("http protocol journal is corrupt: {message}")]
    Corrupt { message: String },
    /// A transient event was incorrectly offered to durable storage.
    #[error("transient http protocol events cannot be durably journaled")]
    TransientEvent,
    /// A run sequence did not advance monotonically.
    #[error(
        "http protocol sequence is not monotonic for {session_id}/{run_id}: latest {latest}, received {received}"
    )]
    NonMonotonicSequence {
        session_id: String,
        run_id: String,
        latest: u64,
        received: u64,
    },
    /// A terminal run stream cannot accept later events.
    #[error("http protocol stream is already terminal for {session_id}/{run_id}")]
    StreamAlreadyTerminal { session_id: String, run_id: String },
    /// One event exceeded the kernel's durable event-size boundary.
    #[error("http protocol event is too large: {bytes} bytes exceeds {limit}")]
    EventTooLarge { bytes: usize, limit: usize },
    /// Configured retention would exceed the hard allocation boundary.
    #[error("http protocol journal capacity {requested} is outside 1..={limit}")]
    InvalidCapacity { requested: usize, limit: usize },
    /// Serialized journal state exceeded the hard durable-file boundary.
    #[error("http protocol journal is too large: {bytes} bytes exceeds {limit}")]
    JournalTooLarge { bytes: usize, limit: usize },
    /// The bounded stream identity set cannot admit another concurrently retained stream.
    #[error("http protocol journal is at its bounded stream capacity")]
    StreamCapacity,
    /// Durable journal state could not be locked.
    #[error("http protocol journal is unavailable")]
    Unavailable,
    /// Durable filesystem work failed.
    #[error("http protocol journal I/O failed: {message}")]
    Io { message: String },
}

impl HttpProtocolJournalError {
    fn io(error: std::io::Error) -> Self {
        Self::Io {
            message: error.to_string(),
        }
    }
}

#[derive(Debug, Clone, Default)]
struct HttpProtocolJournalState {
    events: Vec<HttpProtocolEvent>,
    high_watermarks: BTreeMap<HttpProtocolStreamKey, HttpProtocolStreamWatermark>,
}

impl HttpProtocolJournalState {
    fn append(&mut self, event: HttpProtocolEvent) -> Result<(), HttpProtocolJournalError> {
        validate_event_size(&event)?;
        let key = HttpProtocolStreamKey::new(
            event.run_event.session_id.clone(),
            event.run_event.run_id.clone(),
        );
        let received = event.run_event.sequence;
        let existing = self.high_watermarks.get(&key).copied();
        if existing.is_some_and(|watermark| !watermark.accepts_events) {
            return Err(HttpProtocolJournalError::StreamAlreadyTerminal {
                session_id: key.session_id,
                run_id: key.run_id,
            });
        }
        let latest = existing.map_or(0, |watermark| watermark.latest_sequence);
        if received <= latest {
            return Err(HttpProtocolJournalError::NonMonotonicSequence {
                session_id: key.session_id,
                run_id: key.run_id,
                latest,
                received,
            });
        }
        let terminal = protocol_event_is_terminal(&event);
        self.high_watermarks.insert(
            key,
            HttpProtocolStreamWatermark {
                latest_sequence: received,
                evicted_through_sequence: existing
                    .map_or(0, |watermark| watermark.evicted_through_sequence),
                terminal,
                accepts_events: !terminal,
            },
        );
        self.events.push(event);
        Ok(())
    }

    fn trim(&mut self, max_events: usize) -> Result<(), HttpProtocolJournalError> {
        let remove = self.events.len().saturating_sub(max_events);
        if remove > 0 {
            for event in self.events.drain(..remove) {
                let key =
                    HttpProtocolStreamKey::new(event.run_event.session_id, event.run_event.run_id);
                if let Some(watermark) = self.high_watermarks.get_mut(&key) {
                    watermark.evicted_through_sequence = watermark
                        .evicted_through_sequence
                        .max(event.run_event.sequence);
                }
            }
        }
        let retained = self
            .events
            .iter()
            .map(|event| {
                HttpProtocolStreamKey::new(
                    event.run_event.session_id.clone(),
                    event.run_event.run_id.clone(),
                )
            })
            .collect::<BTreeSet<_>>();
        self.high_watermarks
            .retain(|key, watermark| watermark.accepts_events || retained.contains(key));
        self.ensure_stream_capacity(max_events)
    }

    fn seal_recovered_streams(&mut self) {
        for watermark in self.high_watermarks.values_mut() {
            watermark.accepts_events = false;
        }
    }

    fn ensure_stream_capacity(&self, max_events: usize) -> Result<(), HttpProtocolJournalError> {
        if self.high_watermarks.len() <= max_events {
            Ok(())
        } else {
            Err(HttpProtocolJournalError::StreamCapacity)
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct HttpProtocolStreamWatermark {
    latest_sequence: u64,
    evicted_through_sequence: u64,
    terminal: bool,
    accepts_events: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct HttpProtocolStreamKey {
    session_id: String,
    run_id: String,
}

impl HttpProtocolStreamKey {
    fn new(session_id: impl Into<String>, run_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            run_id: run_id.into(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
struct HttpProtocolJournalFile {
    schema_version: u32,
    events: Vec<HttpProtocolEvent>,
    high_watermarks: Vec<HttpProtocolJournalWatermark>,
}

impl HttpProtocolJournalFile {
    fn from_state(state: &HttpProtocolJournalState) -> Self {
        Self {
            schema_version: HTTP_PROTOCOL_JOURNAL_SCHEMA_VERSION,
            events: state.events.clone(),
            high_watermarks: state
                .high_watermarks
                .iter()
                .map(|(key, watermark)| HttpProtocolJournalWatermark {
                    session_id: key.session_id.clone(),
                    run_id: key.run_id.clone(),
                    latest_sequence: watermark.latest_sequence,
                    evicted_through_sequence: watermark.evicted_through_sequence,
                    terminal: watermark.terminal,
                    accepts_events: watermark.accepts_events,
                })
                .collect(),
        }
    }

    fn into_state(mut self) -> Result<HttpProtocolJournalState, HttpProtocolJournalError> {
        match self.schema_version {
            HTTP_PROTOCOL_JOURNAL_SCHEMA_VERSION => {}
            HTTP_PROTOCOL_JOURNAL_PREVIOUS_SCHEMA_VERSION => {
                self.events = self
                    .events
                    .into_iter()
                    .map(migrate_previous_schema_event)
                    .collect::<Result<Vec<_>, _>>()?;
                self.schema_version = HTTP_PROTOCOL_JOURNAL_SCHEMA_VERSION;
            }
            schema_version => {
                return Err(HttpProtocolJournalError::Corrupt {
                    message: format!("unsupported schema version {schema_version}"),
                });
            }
        }
        if self.events.len() > MAX_HTTP_PROTOCOL_JOURNAL_EVENTS
            || self.high_watermarks.len() > MAX_HTTP_PROTOCOL_JOURNAL_EVENTS
        {
            return Err(HttpProtocolJournalError::Corrupt {
                message: "protocol journal record count exceeds its hard boundary".to_owned(),
            });
        }
        let mut high_watermarks = BTreeMap::new();
        for watermark in self.high_watermarks {
            if watermark.latest_sequence == 0
                || watermark.evicted_through_sequence > watermark.latest_sequence
                || (watermark.terminal && watermark.accepts_events)
            {
                return Err(HttpProtocolJournalError::Corrupt {
                    message: "invalid protocol watermark".to_owned(),
                });
            }
            let key = HttpProtocolStreamKey::new(watermark.session_id, watermark.run_id);
            if high_watermarks
                .insert(
                    key,
                    HttpProtocolStreamWatermark {
                        latest_sequence: watermark.latest_sequence,
                        evicted_through_sequence: watermark.evicted_through_sequence,
                        terminal: watermark.terminal,
                        accepts_events: watermark.accepts_events,
                    },
                )
                .is_some()
            {
                return Err(HttpProtocolJournalError::Corrupt {
                    message: "duplicate protocol watermark".to_owned(),
                });
            }
        }
        let mut observed = BTreeMap::<HttpProtocolStreamKey, (u64, bool)>::new();
        for event in &self.events {
            validate_event_size(event)?;
            let canonical = canonical_durable_event(event.clone())?;
            if serde_json::to_value(&canonical).map_err(|error| {
                HttpProtocolJournalError::Corrupt {
                    message: error.to_string(),
                }
            })? != serde_json::to_value(event).map_err(|error| {
                HttpProtocolJournalError::Corrupt {
                    message: error.to_string(),
                }
            })? {
                return Err(HttpProtocolJournalError::Corrupt {
                    message: "journal contains a non-canonical durable event".to_owned(),
                });
            }
            let expected_cursor = HttpProtocolCursor::from_run_event(&event.run_event)
                .map_err(|error| HttpProtocolJournalError::Corrupt {
                    message: error.to_string(),
                })?
                .encode();
            if event.replay_id.as_deref() != Some(expected_cursor.as_str()) {
                return Err(HttpProtocolJournalError::Corrupt {
                    message: "journal event cursor does not match its payload".to_owned(),
                });
            }
            let key = HttpProtocolStreamKey::new(
                event.run_event.session_id.clone(),
                event.run_event.run_id.clone(),
            );
            let (previous, terminal) = observed.get(&key).copied().unwrap_or((0, false));
            if terminal || event.run_event.sequence <= previous {
                return Err(HttpProtocolJournalError::Corrupt {
                    message: "journal event sequences are not ordered".to_owned(),
                });
            }
            observed.insert(
                key,
                (event.run_event.sequence, protocol_event_is_terminal(event)),
            );
        }
        for (key, (sequence, terminal)) in observed {
            let Some(watermark) = high_watermarks.get(&key) else {
                return Err(HttpProtocolJournalError::Corrupt {
                    message: "journal watermark is missing for a retained event".to_owned(),
                });
            };
            if watermark.latest_sequence != sequence
                || watermark.evicted_through_sequence >= sequence
                || terminal != watermark.terminal
            {
                return Err(HttpProtocolJournalError::Corrupt {
                    message: "journal watermark disagrees with retained events".to_owned(),
                });
            }
        }
        Ok(HttpProtocolJournalState {
            events: self.events,
            high_watermarks,
        })
    }
}

fn migrate_previous_schema_event(
    event: HttpProtocolEvent,
) -> Result<HttpProtocolEvent, HttpProtocolJournalError> {
    if event.schema_version != HTTP_PROTOCOL_EVENT_PREVIOUS_SCHEMA_VERSION || !event.is_durable() {
        return Err(HttpProtocolJournalError::Corrupt {
            message: "previous protocol journal contains an incompatible event".to_owned(),
        });
    }
    let replay_id = event.replay_id;
    let approval_request = event.approval_request;
    let original_run_event = serde_json::to_value(&event.run_event).map_err(|error| {
        HttpProtocolJournalError::Corrupt {
            message: error.to_string(),
        }
    })?;
    let mut migrated = HttpProtocolEvent::from_run_event(event.run_event).map_err(|error| {
        HttpProtocolJournalError::Corrupt {
            message: error.to_string(),
        }
    })?;
    let migrated_run_event = serde_json::to_value(&migrated.run_event).map_err(|error| {
        HttpProtocolJournalError::Corrupt {
            message: error.to_string(),
        }
    })?;
    if !migrated.is_durable()
        || migrated.replay_id != replay_id
        || migrated_run_event != original_run_event
    {
        return Err(HttpProtocolJournalError::Corrupt {
            message: "previous protocol journal contains a non-canonical durable event".to_owned(),
        });
    }
    migrated.approval_request = approval_request;
    if !migrated.has_valid_approval_metadata() {
        return Err(HttpProtocolJournalError::Corrupt {
            message: "previous protocol journal approval metadata does not match its payload"
                .to_owned(),
        });
    }
    Ok(migrated)
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
struct HttpProtocolJournalWatermark {
    session_id: String,
    run_id: String,
    latest_sequence: u64,
    evicted_through_sequence: u64,
    terminal: bool,
    accepts_events: bool,
}

fn canonical_durable_event(
    event: HttpProtocolEvent,
) -> Result<HttpProtocolEvent, HttpProtocolJournalError> {
    if event.schema_version != HTTP_PROTOCOL_EVENT_SCHEMA_VERSION || !event.is_durable() {
        return Err(HttpProtocolJournalError::TransientEvent);
    }
    let replay_id = event.replay_id;
    let approval_request = event.approval_request;
    let provisional_id = event.provisional_id;
    let mut canonical = HttpProtocolEvent::from_run_event(event.run_event).map_err(|error| {
        HttpProtocolJournalError::Corrupt {
            message: error.to_string(),
        }
    })?;
    if !canonical.is_durable() {
        return Err(HttpProtocolJournalError::TransientEvent);
    }
    if canonical.replay_id != replay_id {
        return Err(HttpProtocolJournalError::Corrupt {
            message: "journal event cursor does not match its payload".to_owned(),
        });
    }
    if canonical.provisional_id != provisional_id {
        return Err(HttpProtocolJournalError::Corrupt {
            message: "journal event provisional identity does not match its payload".to_owned(),
        });
    }
    canonical.approval_request = approval_request;
    if !canonical.has_valid_approval_metadata() {
        return Err(HttpProtocolJournalError::Corrupt {
            message: "journal event approval metadata does not match its payload".to_owned(),
        });
    }
    Ok(canonical)
}

fn validate_event_size(event: &HttpProtocolEvent) -> Result<(), HttpProtocolJournalError> {
    let bytes = serde_json::to_vec(event)
        .map_err(|error| HttpProtocolJournalError::Corrupt {
            message: error.to_string(),
        })?
        .len();
    if bytes > MAX_EVENT_BYTES {
        Err(HttpProtocolJournalError::EventTooLarge {
            bytes,
            limit: MAX_EVENT_BYTES,
        })
    } else {
        Ok(())
    }
}

fn protocol_event_is_terminal(event: &HttpProtocolEvent) -> bool {
    matches!(
        &event.run_event.event,
        sigil_kernel::PublicRunEventKind::RunFinished { .. }
            | sigil_kernel::PublicRunEventKind::RunFailed { .. }
            | sigil_kernel::PublicRunEventKind::RunCancelled
    )
}

fn parse_scoped_cursor(
    session_id: &str,
    run_id: &str,
    last_event_id: Option<&str>,
) -> Result<Option<HttpProtocolCursor>, HttpProtocolReplayError> {
    let Some(value) = last_event_id else {
        return Ok(None);
    };
    let cursor = HttpProtocolCursor::parse(value).map_err(|error| {
        HttpProtocolReplayError::InvalidCursor {
            message: error.to_string(),
        }
    })?;
    if cursor.session_id != session_id || cursor.run_id != run_id {
        return Err(HttpProtocolReplayError::CursorScopeMismatch);
    }
    Ok(Some(cursor))
}

fn persist_state(
    path: &Path,
    state: &HttpProtocolJournalState,
) -> Result<(), HttpProtocolJournalError> {
    let bytes =
        serde_json::to_vec(&HttpProtocolJournalFile::from_state(state)).map_err(|error| {
            HttpProtocolJournalError::Corrupt {
                message: error.to_string(),
            }
        })?;
    if bytes.len() > MAX_HTTP_PROTOCOL_JOURNAL_BYTES {
        return Err(HttpProtocolJournalError::JournalTooLarge {
            bytes: bytes.len(),
            limit: MAX_HTTP_PROTOCOL_JOURNAL_BYTES,
        });
    }
    atomic_replace(path, &bytes).map_err(HttpProtocolJournalError::io)
}

#[cfg(test)]
#[path = "tests/journal_tests.rs"]
mod tests;
