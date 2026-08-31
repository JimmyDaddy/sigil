use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
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
use sigil_runtime::managed_storage_writer::{
    ManagedStorageWriterAdapterV1, ManagedStorageWriterLeaseV1, StorageWriterChannelV1,
};

const HTTP_PROTOCOL_JOURNAL_SCHEMA_VERSION: u32 = 3;
pub(crate) const MAX_HTTP_PROTOCOL_JOURNAL_EVENTS: usize = 4_096;
pub(crate) const MAX_HTTP_PROTOCOL_JOURNAL_BYTES: usize = 16 * 1024 * 1024;

/// Crash-safe bounded durable storage for replayable HTTP protocol events.
pub struct HttpDurableProtocolJournal {
    path: PathBuf,
    max_events: usize,
    state: Mutex<HttpProtocolJournalState>,
    managed_writer: Mutex<Option<ManagedProtocolReplayWriter>>,
    _lease: File,
}

struct ManagedProtocolReplayWriter {
    writer: std::sync::Arc<ManagedStorageWriterAdapterV1>,
    lease: Mutex<Option<ManagedStorageWriterLeaseV1>>,
}

impl ManagedProtocolReplayWriter {
    fn new(
        writer: std::sync::Arc<ManagedStorageWriterAdapterV1>,
        key: &str,
    ) -> Result<Self, HttpProtocolJournalError> {
        let lease = writer
            .acquire_named(StorageWriterChannelV1::AdapterDurableState, key)
            .map_err(|error| {
                HttpProtocolJournalError::io(std::io::Error::other(error.to_string()))
            })?;
        Ok(Self {
            writer,
            lease: Mutex::new(Some(lease)),
        })
    }

    fn read_snapshot(&self) -> Result<Vec<u8>, HttpProtocolJournalError> {
        let lease = self
            .lease
            .lock()
            .map_err(|_| HttpProtocolJournalError::Unavailable)?;
        let Some(lease) = lease.as_ref() else {
            return Err(HttpProtocolJournalError::Unavailable);
        };
        self.writer
            .read_record_bytes(lease, MAX_HTTP_PROTOCOL_JOURNAL_BYTES)
            .map_err(|error| HttpProtocolJournalError::io(std::io::Error::other(error.to_string())))
    }

    fn replace_snapshot(&self, bytes: &[u8]) -> Result<(), HttpProtocolJournalError> {
        let lease = self
            .lease
            .lock()
            .map_err(|_| HttpProtocolJournalError::Unavailable)?;
        let Some(lease) = lease.as_ref() else {
            return Err(HttpProtocolJournalError::Unavailable);
        };
        self.writer
            .replace_record_bytes(lease, bytes)
            .map_err(|error| HttpProtocolJournalError::io(std::io::Error::other(error.to_string())))
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

impl Drop for ManagedProtocolReplayWriter {
    fn drop(&mut self) {
        self.finalize();
    }
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
        let path_existed = fs::symlink_metadata(&path).is_ok();
        let lease = acquire_exclusive_lease(&path).map_err(HttpProtocolJournalError::io)?;
        let mut state = if path.exists() {
            let bytes_on_disk = path.metadata().map_err(HttpProtocolJournalError::io)?.len();
            if bytes_on_disk > MAX_HTTP_PROTOCOL_JOURNAL_BYTES as u64 {
                return Err(HttpProtocolJournalError::JournalTooLarge {
                    bytes: usize::try_from(bytes_on_disk).unwrap_or(usize::MAX),
                    limit: MAX_HTTP_PROTOCOL_JOURNAL_BYTES,
                });
            }
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
        if path_existed {
            persist_state(&path, &state)?;
        }
        Ok(Self {
            path,
            max_events,
            state: Mutex::new(state),
            managed_writer: Mutex::new(None),
            _lease: lease,
        })
    }

    /// Opens the replay journal and, for rebuildable legacy content, quarantines that source
    /// through the HTTP replay owner before retrying with an empty journal. Current-schema boot
    /// uses this only as a one-time legacy import boundary; all subsequent writes are redirected
    /// by `attach_managed_writer` to the authority-admitted adapter namespace.
    ///
    /// # Errors
    ///
    /// Returns an error when the journal cannot be opened, the invalid source cannot be safely
    /// isolated, or the rebuilt journal cannot be initialized.
    pub fn open_with_replay_rebuild(
        path: impl Into<PathBuf>,
        max_events: usize,
    ) -> Result<Self, HttpProtocolJournalError> {
        let path = path.into();
        match Self::open(path.clone(), max_events) {
            Ok(journal) => Ok(journal),
            Err(error) if error.permits_replay_rebuild() => {
                quarantine_invalid_replay_source(&path)?;
                Self::open(path, max_events)
            }
            Err(error) => Err(error),
        }
    }

    /// Switches the current-schema replay owner to the composed managed adapter state writer.
    /// Existing legacy state is imported once; all later snapshots are written only through the
    /// admitted adapter durable-state namespace.
    pub(crate) fn attach_managed_writer(
        &self,
        writer: std::sync::Arc<ManagedStorageWriterAdapterV1>,
        key: &str,
    ) -> Result<(), HttpProtocolJournalError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| HttpProtocolJournalError::Unavailable)?;
        // Keep the existing state -> writer lock order used by ordinary persistence. Reject a
        // second owner before acquiring another lease or replacing either snapshot.
        let mut attached = self
            .managed_writer
            .lock()
            .map_err(|_| HttpProtocolJournalError::Unavailable)?;
        if attached.is_some() {
            return Err(HttpProtocolJournalError::Unavailable);
        }
        let managed = ManagedProtocolReplayWriter::new(writer, key)?;
        let managed_bytes = managed.read_snapshot()?;
        let mut candidate = if managed_bytes.is_empty() {
            state.clone()
        } else {
            decode_state(&managed_bytes)?
        };
        candidate.seal_recovered_streams();
        candidate.trim(self.max_events)?;
        candidate.revision = state.next_revision()?;
        let bytes = encode_state(&candidate)?;
        managed.replace_snapshot(&bytes)?;
        *state = candidate;
        *attached = Some(managed);
        Ok(())
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
        self.append_with_stream_continuation(event, false)
    }

    /// Durably records one event while optionally keeping a foreground-terminal stream open for
    /// terminal tasks that still have a live process owner.
    pub fn append_with_stream_continuation(
        &self,
        event: HttpProtocolEvent,
        keep_stream_open: bool,
    ) -> Result<(), HttpProtocolJournalError> {
        self.append_with_stream_policy(event, keep_stream_open, false)
    }

    /// Durably appends the final terminal-task lifecycle and closes the retained stream in the
    /// same filesystem commit. This keeps lifecycle publication retry-safe: a failed persist
    /// consumes neither the event sequence nor the stream-close transition.
    pub fn append_and_close_stream(
        &self,
        event: HttpProtocolEvent,
    ) -> Result<(), HttpProtocolJournalError> {
        self.append_with_stream_policy(event, false, true)
    }

    fn append_with_stream_policy(
        &self,
        event: HttpProtocolEvent,
        keep_stream_open: bool,
        close_after_append: bool,
    ) -> Result<(), HttpProtocolJournalError> {
        let event = canonical_durable_event(event)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| HttpProtocolJournalError::Unavailable)?;
        let mut candidate = state.clone();
        let key = HttpProtocolStreamKey::new(
            event.run_event.session_id.clone(),
            event.run_event.run_id.clone(),
        );
        candidate.append(event, keep_stream_open)?;
        if close_after_append {
            candidate.close_stream(&key)?;
        }
        candidate.trim(self.max_events)?;
        candidate.revision = state.next_revision()?;
        self.persist_state(&candidate)?;
        *state = candidate;
        Ok(())
    }

    /// Closes one stream after every persistent terminal task reached an owner-confirmed terminal.
    pub fn close_stream(
        &self,
        session_id: &str,
        run_id: &str,
    ) -> Result<(), HttpProtocolJournalError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| HttpProtocolJournalError::Unavailable)?;
        let mut candidate = state.clone();
        let key = HttpProtocolStreamKey::new(session_id, run_id);
        candidate.close_stream(&key)?;
        candidate.revision = state.next_revision()?;
        self.persist_state(&candidate)?;
        *state = candidate;
        Ok(())
    }

    /// Returns an in-process cache revision to capture before reading a rebuild source.
    ///
    /// This is not a public-event sequence or persisted authority. It fences concurrent cache
    /// changes even when retention has removed a run's last event and watermark.
    pub(crate) fn replay_projection_revision(&self) -> Result<u64, HttpProtocolJournalError> {
        self.state
            .lock()
            .map(|state| state.revision)
            .map_err(|_| HttpProtocolJournalError::Unavailable)
    }

    /// Atomically replaces selected derived run windows with verified durable public events.
    ///
    /// The candidate removes only the selected runs directly, then applies the complete source
    /// and the normal global retention trim before one persist-and-swap. Retention can still
    /// evict another run's oldest suffix; it never clears another run as part of this reset.
    pub(crate) fn replace_run_replay_projections(
        &self,
        runs: &BTreeSet<(String, String)>,
        source: &[(HttpProtocolEvent, bool, bool)],
        expected_revision: u64,
    ) -> Result<(), HttpProtocolJournalError> {
        let canonical_source = source
            .iter()
            .map(|(event, keep_stream_open, close_stream_after_event)| {
                let key = (
                    event.run_event.session_id.clone(),
                    event.run_event.run_id.clone(),
                );
                if !runs.contains(&key) {
                    return Err(HttpProtocolJournalError::Corrupt {
                        message:
                            "rebuild source event belongs to a run outside the replacement set"
                                .to_owned(),
                    });
                }
                Ok((
                    canonical_durable_event(event.clone())?,
                    *keep_stream_open,
                    *close_stream_after_event,
                ))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let source_frontiers = canonical_source.iter().fold(
            BTreeMap::<HttpProtocolStreamKey, u64>::new(),
            |mut frontiers, (event, _, _)| {
                let key = HttpProtocolStreamKey::new(
                    event.run_event.session_id.clone(),
                    event.run_event.run_id.clone(),
                );
                frontiers
                    .entry(key)
                    .and_modify(|sequence| *sequence = (*sequence).max(event.run_event.sequence))
                    .or_insert(event.run_event.sequence);
                frontiers
            },
        );
        if source_frontiers.len() != runs.len() {
            return Err(HttpProtocolJournalError::Corrupt {
                message: "rebuild source is missing a requested run".to_owned(),
            });
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| HttpProtocolJournalError::Unavailable)?;
        if state.revision != expected_revision {
            return Err(HttpProtocolJournalError::StaleReplayProjection);
        }
        let source_by_identity = canonical_source
            .iter()
            .map(|(event, _, _)| {
                (
                    (
                        event.run_event.session_id.as_str(),
                        event.run_event.run_id.as_str(),
                        event.run_event.sequence,
                    ),
                    &event.run_event,
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut preserve_closed = Vec::new();
        for (session_id, run_id) in runs {
            let key = HttpProtocolStreamKey::new(session_id.clone(), run_id.clone());
            let source_frontier = source_frontiers.get(&key).copied().ok_or_else(|| {
                HttpProtocolJournalError::Corrupt {
                    message: "rebuild source is missing a requested run frontier".to_owned(),
                }
            })?;
            if let Some(current) = state.high_watermarks.get(&key)
                && current.latest_sequence > source_frontier
            {
                // A concurrent publisher can advance this derived window after the source was
                // read. Reject the stale snapshot without classifying valid state as corruption.
                return Err(HttpProtocolJournalError::NonMonotonicSequence {
                    session_id: session_id.clone(),
                    run_id: run_id.clone(),
                    latest: current.latest_sequence,
                    received: source_frontier,
                });
            }
            if let Some(current) = state.high_watermarks.get(&key)
                && !current.accepts_events
                && current.latest_sequence == source_frontier
            {
                preserve_closed.push((key, current.terminal));
            }
        }
        // The source remains authoritative, but an already published identity cannot silently
        // acquire different bytes through the rebuild path. Match the exact-publication guard.
        for current in &state.events {
            if !runs.contains(&(
                current.run_event.session_id.clone(),
                current.run_event.run_id.clone(),
            )) {
                continue;
            }
            let source = source_by_identity
                .get(&(
                    current.run_event.session_id.as_str(),
                    current.run_event.run_id.as_str(),
                    current.run_event.sequence,
                ))
                .ok_or_else(|| HttpProtocolJournalError::Corrupt {
                    message: "rebuild source omits a retained durable event identity".to_owned(),
                })?;
            let encode = |event: &sigil_kernel::PublicRunEvent| {
                serde_json::to_value(event).map_err(|error| HttpProtocolJournalError::Corrupt {
                    message: error.to_string(),
                })
            };
            if encode(&current.run_event)? != encode(source)? {
                return Err(HttpProtocolJournalError::Corrupt {
                    message: "rebuild source conflicts with a retained public event identity"
                        .to_owned(),
                });
            }
        }
        let mut candidate = state.clone();
        candidate.events.retain(|event| {
            !runs.contains(&(
                event.run_event.session_id.clone(),
                event.run_event.run_id.clone(),
            ))
        });
        candidate
            .high_watermarks
            .retain(|key, _| !runs.contains(&(key.session_id.clone(), key.run_id.clone())));
        for (event, keep_stream_open, close_stream_after_event) in canonical_source {
            let key = HttpProtocolStreamKey::new(
                event.run_event.session_id.clone(),
                event.run_event.run_id.clone(),
            );
            candidate.append(event, keep_stream_open)?;
            if close_stream_after_event {
                candidate.close_stream(&key)?;
            }
        }
        for (key, was_terminal) in preserve_closed {
            if let Some(watermark) = candidate.high_watermarks.get_mut(&key) {
                // An equal source frontier supplies no new fact that could reopen this stream.
                // Preserve recovery sealing without inventing a foreground domain terminal.
                watermark.accepts_events = false;
                watermark.terminal |= was_terminal;
            }
        }
        candidate.trim(self.max_events)?;
        candidate.revision = state.next_revision()?;
        self.persist_state(&candidate)?;
        *state = candidate;
        Ok(())
    }

    /// Returns one retained durable protocol event without cloning this run's replay suffix.
    ///
    /// An absent event is deliberately distinct from cursor expiry: exact delivery can only use
    /// this to recognize bytes already retained. Recovery of an evicted pending predecessor is
    /// owned by the verified-public-outbox rebuild path.
    pub(crate) fn retained_run_event_at(
        &self,
        session_id: &str,
        run_id: &str,
        sequence: u64,
    ) -> Result<Option<HttpProtocolEvent>, HttpProtocolReplayError> {
        let state = self
            .state
            .lock()
            .map_err(|_| HttpProtocolReplayError::JournalUnavailable)?;
        Ok(state
            .events
            .iter()
            .rev()
            .find(|event| {
                event.run_event.session_id == session_id
                    && event.run_event.run_id == run_id
                    && event.run_event.sequence == sequence
            })
            .cloned())
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

    /// Returns whether one durable stream still accepts events from its exact process owner.
    pub fn stream_accepts_events(
        &self,
        session_id: &str,
        run_id: &str,
    ) -> Result<Option<bool>, HttpProtocolJournalError> {
        let state = self
            .state
            .lock()
            .map_err(|_| HttpProtocolJournalError::Unavailable)?;
        Ok(state
            .high_watermarks
            .get(&HttpProtocolStreamKey::new(session_id, run_id))
            .map(|watermark| watermark.accepts_events))
    }

    fn persist_state(
        &self,
        state: &HttpProtocolJournalState,
    ) -> Result<(), HttpProtocolJournalError> {
        let bytes = encode_state(state)?;
        let managed = self
            .managed_writer
            .lock()
            .map_err(|_| HttpProtocolJournalError::Unavailable)?;
        if let Some(managed) = managed.as_ref() {
            managed.replace_snapshot(&bytes)
        } else {
            atomic_replace(&self.path, &bytes).map_err(HttpProtocolJournalError::io)
        }
    }
}

fn quarantine_invalid_replay_source(path: &Path) -> Result<(), HttpProtocolJournalError> {
    let canonical_path =
        canonical_durable_path(path.to_path_buf()).map_err(HttpProtocolJournalError::io)?;
    let metadata = fs::symlink_metadata(&canonical_path).map_err(HttpProtocolJournalError::io)?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(HttpProtocolJournalError::io(std::io::Error::other(
            "invalid HTTP replay state is not a regular owned file",
        )));
    }
    let parent = canonical_path.parent().ok_or_else(|| {
        HttpProtocolJournalError::io(std::io::Error::other(
            "invalid HTTP replay state has no parent directory",
        ))
    })?;
    let file_name = canonical_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            HttpProtocolJournalError::io(std::io::Error::other(
                "invalid HTTP replay state has no valid file name",
            ))
        })?;
    let quarantine_name = format!("{file_name}.invalid-{}", uuid::Uuid::new_v4().simple());
    fs::rename(&canonical_path, parent.join(quarantine_name)).map_err(HttpProtocolJournalError::io)
}

impl Drop for HttpDurableProtocolJournal {
    fn drop(&mut self) {
        if let Ok(mut managed) = self.managed_writer.lock()
            && let Some(managed) = managed.take()
        {
            managed.finalize();
        }
    }
}

/// Durable journal failures.
#[derive(Debug, Clone, PartialEq, Eq, ThisError)]
pub enum HttpProtocolJournalError {
    /// A rebuild source was read across a concurrent change to this derived cache.
    #[error(
        "http replay projection changed while its source was read; retry with a fresh snapshot"
    )]
    StaleReplayProjection,
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

    /// Returns whether an existing replay journal may be isolated and rebuilt at server startup.
    ///
    /// These failures describe invalid content in the HTTP replay projection itself. The journal
    /// is not the canonical conversation or command-idempotency store, so a new server process can
    /// safely start with an empty replay window after preserving the invalid file for diagnostics.
    #[must_use]
    pub const fn permits_replay_rebuild(&self) -> bool {
        matches!(
            self,
            Self::Corrupt { .. }
                | Self::EventTooLarge { .. }
                | Self::JournalTooLarge { .. }
                | Self::StreamCapacity
        )
    }
}

#[derive(Debug, Clone, Default)]
struct HttpProtocolJournalState {
    events: Vec<HttpProtocolEvent>,
    high_watermarks: BTreeMap<HttpProtocolStreamKey, HttpProtocolStreamWatermark>,
    // Process-local only: HttpProtocolJournalFile deliberately does not encode this fence.
    revision: u64,
}

impl HttpProtocolJournalState {
    fn next_revision(&self) -> Result<u64, HttpProtocolJournalError> {
        self.revision
            .checked_add(1)
            .ok_or(HttpProtocolJournalError::Unavailable)
    }

    fn append(
        &mut self,
        event: HttpProtocolEvent,
        keep_stream_open: bool,
    ) -> Result<(), HttpProtocolJournalError> {
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
        let terminal = protocol_event_is_terminal(&event) && !keep_stream_open;
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

    fn close_stream(
        &mut self,
        key: &HttpProtocolStreamKey,
    ) -> Result<(), HttpProtocolJournalError> {
        let watermark =
            self.high_watermarks
                .get_mut(key)
                .ok_or_else(|| HttpProtocolJournalError::Corrupt {
                    message: "terminal stream watermark is missing".to_owned(),
                })?;
        watermark.terminal = true;
        watermark.accepts_events = false;
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

    fn into_state(self) -> Result<HttpProtocolJournalState, HttpProtocolJournalError> {
        if self.schema_version != HTTP_PROTOCOL_JOURNAL_SCHEMA_VERSION {
            return Err(HttpProtocolJournalError::Corrupt {
                message: format!("unsupported schema version {}", self.schema_version),
            });
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
            let (previous, foreground_terminal_seen) =
                observed.get(&key).copied().unwrap_or((0, false));
            if event.run_event.sequence <= previous
                || (foreground_terminal_seen
                    && !matches!(
                        event.run_event.event,
                        sigil_kernel::PublicRunEventKind::TerminalLifecycle { .. }
                    ))
            {
                return Err(HttpProtocolJournalError::Corrupt {
                    message: "journal event sequences are not ordered".to_owned(),
                });
            }
            observed.insert(
                key,
                (
                    event.run_event.sequence,
                    foreground_terminal_seen || protocol_event_is_terminal(event),
                ),
            );
        }
        for (key, (sequence, _foreground_terminal_seen)) in observed {
            let Some(watermark) = high_watermarks.get(&key) else {
                return Err(HttpProtocolJournalError::Corrupt {
                    message: "journal watermark is missing for a retained event".to_owned(),
                });
            };
            if watermark.latest_sequence != sequence
                || watermark.evicted_through_sequence >= sequence
            {
                return Err(HttpProtocolJournalError::Corrupt {
                    message: "journal watermark disagrees with retained events".to_owned(),
                });
            }
        }
        Ok(HttpProtocolJournalState {
            events: self.events,
            high_watermarks,
            revision: 0,
        })
    }
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
            | sigil_kernel::PublicRunEventKind::RunBlocked { .. }
            | sigil_kernel::PublicRunEventKind::RunPaused { .. }
            | sigil_kernel::PublicRunEventKind::RunInterrupted { .. }
            | sigil_kernel::PublicRunEventKind::RouteRecoveryRequired { .. }
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
    let bytes = encode_state(state)?;
    atomic_replace(path, &bytes).map_err(HttpProtocolJournalError::io)
}

fn encode_state(state: &HttpProtocolJournalState) -> Result<Vec<u8>, HttpProtocolJournalError> {
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
    Ok(bytes)
}

fn decode_state(bytes: &[u8]) -> Result<HttpProtocolJournalState, HttpProtocolJournalError> {
    serde_json::from_slice::<HttpProtocolJournalFile>(bytes)
        .map_err(|error| HttpProtocolJournalError::Corrupt {
            message: error.to_string(),
        })?
        .into_state()
}

#[cfg(test)]
#[path = "tests/journal_tests.rs"]
mod tests;
