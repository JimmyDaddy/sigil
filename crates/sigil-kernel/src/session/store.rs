use super::active_projection::ActiveSessionProjection;
#[cfg(test)]
use super::writer::SessionWriterFault;
use super::writer::{
    PendingStoredEvent, SharedSessionCoordinator, shared_existing_session_writer,
    shared_session_writer,
};
use super::*;
use crate::{EventId, interrupted_agent_threads_excluding_live_owner};
use std::collections::BTreeSet;
use thiserror::Error;

/// Maximum encoded JSONL record, including its newline. Checked before allocating or decoding.
pub const MAX_SESSION_RAW_RECORD_BYTES: usize = 2 * 1024 * 1024;

/// A cooperative observer stopped before admitting more I/O; this is not corrupt history.
#[derive(Debug, Clone, Copy, Error)]
#[error("session observation cancelled or deadline exceeded")]
pub struct SessionObservationCancelled;

/// Cooperative observer budget. This is cancellation state, never write or recovery authority.
#[derive(Debug, Clone, Default)]
pub struct SessionReadBudget {
    cancelled: Arc<std::sync::atomic::AtomicBool>,
    deadline: Option<std::time::Instant>,
}

impl SessionReadBudget {
    #[must_use]
    pub fn new(deadline: Option<std::time::Instant>) -> Self {
        Self {
            cancelled: Arc::default(),
            deadline,
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub fn check(&self) -> Result<()> {
        if self.cancelled.load(Ordering::Acquire)
            || self
                .deadline
                .is_some_and(|deadline| std::time::Instant::now() >= deadline)
        {
            return Err(SessionObservationCancelled.into());
        }
        Ok(())
    }
}

/// Operation class whose bounded session-file lock acquisition was exhausted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionIoBusyKind {
    Reader,
    Writer,
    Recovery,
}

/// Typed, provider-neutral indication that another process currently owns a session-file lock.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[error("session {kind:?} I/O is busy for {path}")]
pub struct SessionIoBusyError {
    /// The operation that could not acquire ownership.
    pub kind: SessionIoBusyKind,
    /// Exact session or lease path whose lock remained contended.
    pub path: PathBuf,
}

/// Privacy-safe process-local counters for operating-system session-file lock acquisition.
///
/// The counters contain no paths or session identifiers. Attempts include bounded retry attempts,
/// while contention and failure count only actual lock outcomes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SessionIoLockMetricsSnapshot {
    pub shared_lock_attempt_total: u64,
    pub exclusive_lock_attempt_total: u64,
    pub contention_total: u64,
    pub failure_total: u64,
}

impl SessionIoLockMetricsSnapshot {
    /// Computes a saturating delta suitable for scoped runtime evidence.
    #[must_use]
    pub fn saturating_delta(self, earlier: Self) -> Self {
        Self {
            shared_lock_attempt_total: self
                .shared_lock_attempt_total
                .saturating_sub(earlier.shared_lock_attempt_total),
            exclusive_lock_attempt_total: self
                .exclusive_lock_attempt_total
                .saturating_sub(earlier.exclusive_lock_attempt_total),
            contention_total: self
                .contention_total
                .saturating_sub(earlier.contention_total),
            failure_total: self.failure_total.saturating_sub(earlier.failure_total),
        }
    }
}

/// Returns privacy-safe process-local session-file lock counters.
#[must_use]
pub fn session_io_lock_metrics() -> SessionIoLockMetricsSnapshot {
    SessionIoLockMetricsSnapshot {
        shared_lock_attempt_total: SESSION_SHARED_LOCK_ATTEMPT_TOTAL.load(Ordering::Relaxed),
        exclusive_lock_attempt_total: SESSION_EXCLUSIVE_LOCK_ATTEMPT_TOTAL.load(Ordering::Relaxed),
        contention_total: SESSION_LOCK_CONTENTION_TOTAL.load(Ordering::Relaxed),
        failure_total: SESSION_LOCK_FAILURE_TOTAL.load(Ordering::Relaxed),
    }
}

pub(super) fn stored_event_from_stream_line(
    line: &str,
    path: &Path,
    physical_line: usize,
) -> Result<StoredEvent> {
    match classify_session_stream_line(line, path, physical_line)? {
        Some(event) => Ok(event),
        None => StoredEvent::from_json_str(line)
            .with_context(|| stream_line_context("stored event", physical_line, path)),
    }
}

/// Classifies a physical JSONL line without treating ordinary malformed tail bytes as a v2 event.
///
/// A line with the v2 envelope shape must deserialize and validate as a [`StoredEvent`]; a raw
/// `SessionLogEntry` is the explicitly unsupported pre-release format. Other bytes can be
/// considered recoverable tail corruption by the writer recovery path.
pub(super) fn classify_session_stream_line(
    line: &str,
    path: &Path,
    physical_line: usize,
) -> Result<Option<StoredEvent>> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return Ok(None);
    };
    let looks_like_stored_event = ["schema_version", "event_type"]
        .into_iter()
        .all(|field| value.get(field).is_some());
    if !looks_like_stored_event {
        bail!(
            "session stream line {physical_line} in {} is not a stored event",
            path.display()
        );
    }

    StoredEvent::from_json_str(line)
        .map(Some)
        .with_context(|| stream_line_context("stored event", physical_line, path))
}

/// Append-only JSONL store for session and control-plane history.
#[derive(Debug, Clone)]
pub struct JsonlSessionStore {
    path: PathBuf,
    writer: std::sync::Arc<SharedSessionCoordinator>,
    live_background_agent_thread_ids: std::sync::Arc<BTreeSet<crate::AgentThreadId>>,
    live_run_cancellation_scope_ids: std::sync::Arc<BTreeSet<String>>,
}

/// Read-only access to one existing store's coordinated durable record snapshots.
///
/// Only a store owner can derive this handle. It retains that owner's coordinator without
/// exposing a path, append operation, recovery operation, or another way to acquire a writer.
#[derive(Debug, Clone)]
pub struct SessionRecordReadHandle {
    coordinator: std::sync::Arc<SharedSessionCoordinator>,
}

impl SessionRecordReadHandle {
    /// Opens an existing, host-authorized stream for observation without acquiring write or
    /// recovery authority. No filesystem state is created or repaired.
    pub fn open_existing_observer(path: impl Into<PathBuf>) -> Result<Self> {
        Ok(Self {
            coordinator: super::writer::shared_session_observer(path)?,
        })
    }
    /// Captures a physical cut between committed writer batches. This is read evidence only.
    pub fn source_snapshot(
        &self,
        budget: &SessionReadBudget,
    ) -> Result<SessionRecordSourceSnapshot> {
        self.coordinator.read_source_snapshot(budget)
    }
    /// Reads one strictly validated durable snapshot after waiting for the existing writer.
    ///
    /// All file locks and coordinator guards are released before returning the records. The
    /// caller can derive several views from that same snapshot without retaining a file lock.
    /// This does not create a stream, repair its tail, or publish a cached projection. Writer
    /// callbacks must use their supplied records instead of re-entering this handle.
    ///
    /// # Errors
    ///
    /// Returns the strict reader's coordination, external-lock, I/O, or validation error.
    pub fn read_event_records(&self) -> Result<Vec<SessionStreamRecord>> {
        self.coordinator.read_records_coordinated()
    }

    /// Reads the current durable session entries through the existing owner's coordinator.
    ///
    /// This is intentionally different from [`JsonlSessionStore::read_entries`]: detached
    /// observers must retain the coordinator that belongs to the live session writer so a
    /// concurrent append waits in-process instead of racing a second path-based reader.
    pub fn read_entries(&self) -> Result<Vec<SessionLogEntry>> {
        let records = self.read_event_records()?;
        ConversationQueueDurableProjection::from_records(&records)?;
        session_entries_from_records(&records)
    }

    /// Reads the complete strictly validated stream once and retains the byte offsets needed by
    /// later bounded tail reads. The coordinator and shared file lock are released before the
    /// range is returned; offsets are metadata only and never grant write or recovery authority.
    pub fn read_event_records_with_offsets(&self) -> Result<SessionRecordRange> {
        self.coordinator.read_records_coordinated_with_offsets()
    }

    /// Reads a bounded JSONL range from the already-owned session stream.
    ///
    /// `start_offset` must point at the beginning of a durable line. The range is validated with
    /// the supplied sequence/session predecessor facts and never repairs or truncates the file.
    pub fn read_event_record_range(
        &self,
        start_offset: u64,
        expected_sequence: u64,
        expected_session_id: Option<&str>,
        max_records: usize,
        max_bytes: usize,
    ) -> Result<SessionRecordRange> {
        if max_records == 0 || max_bytes == 0 {
            bail!("session record range bounds must be non-zero");
        }
        self.read_event_record_range_with_budget(
            start_offset,
            expected_sequence,
            expected_session_id,
            max_records,
            max_bytes,
            &SessionReadBudget::default(),
        )
    }

    /// Reads a chunk while checking cancellation during coordinator waits and raw reads.
    pub fn read_event_record_range_with_budget(
        &self,
        start_offset: u64,
        expected_sequence: u64,
        expected_session_id: Option<&str>,
        max_records: usize,
        max_bytes: usize,
        budget: &SessionReadBudget,
    ) -> Result<SessionRecordRange> {
        if max_records == 0 || max_bytes == 0 {
            bail!("session record range bounds must be non-zero");
        }
        self.coordinator.read_record_range(
            start_offset,
            expected_sequence,
            expected_session_id,
            max_records,
            max_bytes,
            budget,
        )
    }
}

/// A strictly validated, bounded read from a session JSONL stream.
#[derive(Debug, Clone)]
pub struct SessionRecordSourceSnapshot {
    pub(super) metadata: std::fs::Metadata,
}

impl SessionRecordSourceSnapshot {
    #[must_use]
    pub fn byte_len(&self) -> u64 {
        self.metadata.len()
    }
    #[must_use]
    pub fn modified_at(&self) -> Option<std::time::SystemTime> {
        self.metadata.modified().ok()
    }
    #[must_use]
    pub fn same_source(&self, other: &Self) -> bool {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            self.metadata.dev() == other.metadata.dev()
                && self.metadata.ino() == other.metadata.ino()
        }
        #[cfg(not(unix))]
        {
            self.metadata.created().ok() == other.metadata.created().ok()
        }
    }
}

/// A strictly validated, bounded read from a session JSONL stream.
#[derive(Debug, Clone)]
pub struct SessionRecordRange {
    records: Vec<SessionStreamRecord>,
    record_offsets: Vec<u64>,
    record_end_offsets: Vec<u64>,
    start_offset: u64,
    end_offset: u64,
    has_more: bool,
    source: SessionRecordSourceSnapshot,
}

impl SessionRecordRange {
    #[must_use]
    pub fn source_snapshot(&self) -> &SessionRecordSourceSnapshot {
        &self.source
    }
    #[must_use]
    pub fn records(&self) -> &[SessionStreamRecord] {
        &self.records
    }

    #[must_use]
    pub fn into_records(self) -> Vec<SessionStreamRecord> {
        self.records
    }

    /// Returns the byte offset for each record in [`Self::records`]. The vectors always have the
    /// same length; offsets are relative to the beginning of the session stream.
    #[must_use]
    pub fn record_offsets(&self) -> &[u64] {
        &self.record_offsets
    }

    #[must_use]
    pub fn record_end_offsets(&self) -> &[u64] {
        &self.record_end_offsets
    }

    #[must_use]
    pub fn start_offset(&self) -> u64 {
        self.start_offset
    }

    #[must_use]
    pub fn end_offset(&self) -> u64 {
        self.end_offset
    }

    #[must_use]
    pub fn has_more(&self) -> bool {
        self.has_more
    }
}

impl JsonlSessionStore {
    /// Derives read-only observation from this store's already-established coordinator.
    #[must_use]
    pub fn read_handle(&self) -> SessionRecordReadHandle {
        SessionRecordReadHandle {
            coordinator: std::sync::Arc::clone(&self.writer),
        }
    }

    /// Creates a store rooted at `path`, creating parent directories when needed.
    pub fn new(path: impl Into<PathBuf>) -> Result<Self> {
        let (path, writer) = shared_session_writer(path)?;
        Ok(Self {
            path,
            writer,
            live_background_agent_thread_ids: std::sync::Arc::default(),
            live_run_cancellation_scope_ids: std::sync::Arc::default(),
        })
    }

    /// Preserves background threads whose live process-local owner is held by the exact session
    /// attachment while this store restores its durable projection.
    #[must_use]
    pub fn with_live_background_agent_threads(
        mut self,
        thread_ids: BTreeSet<crate::AgentThreadId>,
    ) -> Self {
        self.live_background_agent_thread_ids = std::sync::Arc::new(thread_ids);
        self
    }

    /// Keeps cancellation requests for these still-owned run scopes open during session reload.
    #[must_use]
    pub fn with_live_run_cancellation_scopes(mut self, scope_ids: BTreeSet<String>) -> Self {
        self.live_run_cancellation_scope_ids = std::sync::Arc::new(scope_ids);
        self
    }

    pub(super) fn live_run_cancellation_scope_ids(&self) -> &BTreeSet<String> {
        &self.live_run_cancellation_scope_ids
    }

    /// Reopens one already-published durable stream without creating, permission-hardening, or
    /// reseeding either its data file or writer sidecar.
    ///
    /// The shared coordinator becomes permanently require-existing for this canonical path. A
    /// valid crash tail may still be recovered by normal writer operations, but an absent or
    /// empty recovered stream is rejected rather than treated as a fresh session.
    pub fn open_existing(path: impl Into<PathBuf>) -> Result<Self> {
        let (path, writer) = shared_existing_session_writer(path)?;
        Ok(Self {
            path,
            writer,
            live_background_agent_thread_ids: std::sync::Arc::default(),
            live_run_cancellation_scope_ids: std::sync::Arc::default(),
        })
    }

    /// Structurally parses bytes previously obtained from a caller-validated session-log object.
    ///
    /// This reuses the kernel's stored-event, checksum, sequence, and same-session validation.
    /// It deliberately does not prove that the bytes came from a particular storage authority;
    /// callers must retain their own exact-scope admission and no-follow provenance checks.
    pub fn read_event_records_from_validated_bytes(
        bytes: &[u8],
    ) -> Result<Vec<SessionStreamRecord>> {
        let content = std::str::from_utf8(bytes)
            .context("validated session-log bytes are not valid UTF-8")?;
        read_stream_records_from_str(Path::new("<validated-session-log-bytes>"), content)
    }

    /// Returns a consistent scheduler-facing projection and its exact durable frontier.
    ///
    /// The first call may rebuild from the durable stream. Subsequent ordinary appends advance
    /// the shared projection incrementally without rescanning the JSONL prefix.
    ///
    /// # Errors
    ///
    /// Returns an error when the durable stream cannot be recovered or fails projection
    /// validation.
    pub fn active_projection_snapshot(&self) -> Result<ActiveSessionProjectionSnapshot> {
        self.writer.snapshot()
    }

    pub(super) fn with_locked_projection<T>(
        &self,
        adopt: impl FnOnce(ActiveSessionProjectionSnapshot) -> Result<T>,
    ) -> Result<T> {
        self.writer.with_locked_projection(adopt)
    }

    /// Returns privacy-safe process-local active projection counters.
    #[must_use]
    pub fn active_projection_metrics(&self) -> ActiveProjectionMetricsSnapshot {
        self.writer.metrics()
    }

    /// Registers an observer shared by all store handles for this canonical session path.
    pub fn register_active_projection_observer(
        &self,
        observer: std::sync::Arc<dyn ActiveProjectionObserver>,
    ) -> ActiveProjectionSubscription {
        self.writer.register_observer(observer)
    }

    /// Appends a single serialized session entry to the durable JSONL file.
    pub fn append(&self, entry: &SessionLogEntry) -> Result<()> {
        self.append_session_entry_event(entry).map(|_| ())
    }

    /// Appends one v2 stored event to the durable JSONL file.
    pub fn append_event(
        &self,
        event_type: DurableEventType,
        event_class: EventClass,
        payload: serde_json::Value,
    ) -> Result<StoredEvent> {
        let mut events = self.writer.append_events(
            vec![PendingStoredEvent {
                event_type,
                event_class,
                payload,
                event_id: None,
                correlation_id: None,
                causation_id: None,
            }],
            false,
        )?;
        events
            .pop()
            .context("session writer returned no event for a single append")
    }

    pub(crate) fn append_events_and_session_entries(
        &self,
        durable_events: Vec<(DurableEventType, EventClass, serde_json::Value)>,
        entries: &[SessionLogEntry],
    ) -> Result<Vec<StoredEvent>> {
        if durable_events.is_empty() && entries.is_empty() {
            bail!("durable event append batch must not be empty");
        }
        if entries.iter().any(|entry| {
            matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::ConversationInputPromoted(_))
            )
        }) {
            bail!("conversation input promotion must use the critical direct promotion append API");
        }
        let mut pending = durable_events
            .into_iter()
            .map(|(event_type, event_class, payload)| PendingStoredEvent {
                event_type,
                event_class,
                payload,
                event_id: None,
                correlation_id: None,
                causation_id: None,
            })
            .collect::<Vec<_>>();
        pending.extend(entries.iter().map(|entry| {
            let event_type = session_entry_event_type(entry);
            PendingStoredEvent {
                event_type,
                event_class: session_entry_event_class(event_type),
                payload: serde_json::json!({ "session_log_entry": entry }),
                event_id: None,
                correlation_id: None,
                causation_id: None,
            }
        }));
        self.writer.append_events(pending, false)
    }

    /// Conditionally appends one mixed direct-event/session-entry writer batch.
    ///
    /// The predicate and append run under the same process writer and its persistent cross-process
    /// single-writer ownership, so admission code can compare the current durable projection
    /// without a competing writer. Direct events are always ordered before session entries.
    pub(crate) fn append_events_and_session_entries_if<F>(
        &self,
        durable_events: Vec<(DurableEventType, EventClass, serde_json::Value)>,
        entries: &[SessionLogEntry],
        should_append: F,
    ) -> Result<Option<Vec<StoredEvent>>>
    where
        F: FnOnce(&[SessionStreamRecord]) -> Result<bool>,
    {
        if durable_events.is_empty() && entries.is_empty() {
            bail!("conditional mixed event append batch must not be empty");
        }
        if entries.iter().any(|entry| {
            matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::ConversationInputPromoted(_))
            )
        }) {
            bail!("conversation input promotion must use the critical direct promotion append API");
        }
        let mut pending = durable_events
            .into_iter()
            .map(|(event_type, event_class, payload)| PendingStoredEvent {
                event_type,
                event_class,
                payload,
                event_id: None,
                correlation_id: None,
                causation_id: None,
            })
            .collect::<Vec<_>>();
        pending.extend(entries.iter().map(|entry| {
            let event_type = session_entry_event_type(entry);
            PendingStoredEvent {
                event_type,
                event_class: session_entry_event_class(event_type),
                payload: serde_json::json!({ "session_log_entry": entry }),
                event_id: None,
                correlation_id: None,
                causation_id: None,
            }
        }));
        if pending.len() > 1 {
            self.writer
                .append_crash_safe_bundle_if_records(pending, should_append)
        } else {
            self.writer
                .append_events_if_records(pending, false, should_append)
        }
    }

    /// Commits preallocated event identities through the existing crash-safe bundle protocol.
    /// The predicate and durable intent publication share the session's single writer.
    pub(crate) fn append_crash_safe_events_if<F>(
        &self,
        events: Vec<(EventId, DurableEventType, EventClass, serde_json::Value)>,
        should_append: F,
    ) -> Result<Option<Vec<StoredEvent>>>
    where
        F: FnOnce(&[SessionStreamRecord]) -> Result<bool>,
    {
        let pending = events
            .into_iter()
            .map(
                |(event_id, event_type, event_class, payload)| PendingStoredEvent {
                    event_type,
                    event_class,
                    payload,
                    event_id: Some(event_id),
                    correlation_id: None,
                    causation_id: None,
                },
            )
            .collect();
        self.writer
            .append_crash_safe_bundle_if_records(pending, should_append)
    }

    pub(crate) fn append_event_if<F>(
        &self,
        event_type: DurableEventType,
        event_class: EventClass,
        payload: serde_json::Value,
        should_append: F,
    ) -> Result<bool>
    where
        F: FnOnce(&[SessionStreamRecord]) -> Result<bool>,
    {
        Ok(self
            .writer
            .append_events_if_records(
                vec![PendingStoredEvent {
                    event_type,
                    event_class,
                    payload,
                    event_id: None,
                    correlation_id: None,
                    causation_id: None,
                }],
                false,
                should_append,
            )?
            .is_some())
    }

    pub(crate) fn append_public_event_outbox(
        &self,
        entry: &PublicEventOutboxEntryV1,
    ) -> Result<bool> {
        self.writer.append_public_event_outbox(entry)
    }

    pub(crate) fn append_public_event_delivery(
        &self,
        receipt: &PublicEventDeliveryReceiptV1,
    ) -> Result<bool> {
        self.writer.append_public_event_delivery(receipt)
    }

    pub(crate) fn public_event_outbox_durable_sequence(&self, run_id: &str) -> Result<u64> {
        self.writer.public_event_outbox_durable_sequence(run_id)
    }

    pub(super) fn append_event_if_with_identity<F>(
        &self,
        event_type: DurableEventType,
        payload: serde_json::Value,
        event_id: EventId,
        correlation_id: Option<EventId>,
        causation_id: Option<EventId>,
        should_append: F,
    ) -> Result<Option<StoredEvent>>
    where
        F: FnOnce(&[SessionStreamRecord]) -> Result<bool>,
    {
        let event_class = event_type
            .expected_event_class()
            .context("identified durable event type has no event class")?;
        let events = self.writer.append_events_if_records(
            vec![PendingStoredEvent {
                event_type,
                event_class,
                payload,
                event_id: Some(event_id),
                correlation_id,
                causation_id,
            }],
            true,
            should_append,
        )?;
        match events {
            Some(mut events) => events
                .pop()
                .map(Some)
                .context("session writer returned no event for a conditional append"),
            None => Ok(None),
        }
    }

    pub(super) fn append_event_if_active_with_identity<F>(
        &self,
        event_type: DurableEventType,
        payload: serde_json::Value,
        event_id: EventId,
        correlation_id: Option<EventId>,
        causation_id: Option<EventId>,
        expected_frontier: &ActiveProjectionFrontier,
        should_append: F,
    ) -> Result<Option<StoredEvent>>
    where
        F: FnOnce(&ActiveSessionProjection) -> Result<bool>,
    {
        let event_class = event_type
            .expected_event_class()
            .context("identified durable event type has no event class")?;
        let events = self.writer.append_events_if_active(
            vec![PendingStoredEvent {
                event_type,
                event_class,
                payload,
                event_id: Some(event_id),
                correlation_id,
                causation_id,
            }],
            true,
            Some(expected_frontier),
            should_append,
        )?;
        match events {
            Some(mut events) => events
                .pop()
                .map(Some)
                .context("session writer returned no event for an active conditional append"),
            None => Ok(None),
        }
    }

    pub(super) fn append_event_if_records_at_active_frontier<F>(
        &self,
        event_type: DurableEventType,
        payload: serde_json::Value,
        event_id: EventId,
        correlation_id: Option<EventId>,
        causation_id: Option<EventId>,
        expected_frontier: &ActiveProjectionFrontier,
        should_append: F,
    ) -> Result<Option<StoredEvent>>
    where
        F: FnOnce(&[SessionStreamRecord]) -> Result<bool>,
    {
        let event_class = event_type
            .expected_event_class()
            .context("identified durable event type has no event class")?;
        let events = self.writer.append_events_if_records_at_frontier(
            vec![PendingStoredEvent {
                event_type,
                event_class,
                payload,
                event_id: Some(event_id),
                correlation_id,
                causation_id,
            }],
            true,
            expected_frontier,
            should_append,
        )?;
        match events {
            Some(mut events) => events
                .pop()
                .map(Some)
                .context("session writer returned no event for a frontier-bound records append"),
            None => Ok(None),
        }
    }

    pub(super) fn append_event_if_active_projection<F>(
        &self,
        event_type: DurableEventType,
        payload: serde_json::Value,
        event_id: EventId,
        correlation_id: Option<EventId>,
        causation_id: Option<EventId>,
        expected_frontier: Option<&ActiveProjectionFrontier>,
        should_append: F,
    ) -> Result<Option<StoredEvent>>
    where
        F: FnOnce(&super::active_projection::ActiveSessionProjection) -> Result<bool>,
    {
        let event_class = event_type
            .expected_event_class()
            .context("identified durable event type has no event class")?;
        let events = self.writer.append_events_if_active(
            vec![PendingStoredEvent {
                event_type,
                event_class,
                payload,
                event_id: Some(event_id),
                correlation_id,
                causation_id,
            }],
            true,
            expected_frontier,
            should_append,
        )?;
        match events {
            Some(mut events) => events
                .pop()
                .map(Some)
                .context("session writer returned no event for an active projection append"),
            None => Ok(None),
        }
    }

    pub(super) fn append_application_queue_events(
        &self,
        pending: Vec<PendingStoredEvent>,
        command: &crate::ConversationQueueMutationCommand,
    ) -> Result<Option<StoredEvent>> {
        self.writer
            .append_events_if_active(pending, true, None, |projection| {
                projection.queue().validate_mutation(command)?;
                Ok(true)
            })?
            .map(|events| {
                events
                    .into_iter()
                    .next()
                    .context("application queue batch has no domain event")
            })
            .transpose()
    }

    /// Appends one ordered set of preallocated durable events while holding the single-writer
    /// lease across both the compare predicate and the append. This is intentionally lower-level
    /// than the strict audit receipt API because typed payload validation can depend on the real
    /// stream sequence assigned to each member of the batch.
    pub(super) fn append_events_if_with_identities<F>(
        &self,
        pending: Vec<PendingStoredEvent>,
        should_append: F,
    ) -> Result<Option<Vec<StoredEvent>>>
    where
        F: FnOnce(&[SessionStreamRecord]) -> Result<bool>,
    {
        if pending.is_empty() {
            bail!("conditional durable append batch must not be empty");
        }
        self.writer
            .append_events_if_records(pending, true, should_append)
    }

    /// Appends a provider-visible or control session entry as a v2 stored event.
    pub(super) fn append_control_publication(
        &self,
        pending: Vec<PendingStoredEvent>,
        run_id: &str,
    ) -> Result<Vec<StoredEvent>> {
        self.writer.append_control_publication(pending, run_id)
    }

    pub(super) fn append_control_publication_if_records<F>(
        &self,
        pending: Vec<PendingStoredEvent>,
        run_id: &str,
        should_append: F,
    ) -> Result<Option<Vec<StoredEvent>>>
    where
        F: FnOnce(&[SessionStreamRecord]) -> Result<bool>,
    {
        self.writer
            .append_control_publication_if_records(pending, run_id, should_append)
    }

    /// Appends a provider-visible or control session entry as a v2 stored event.
    pub fn append_session_entry_event(&self, entry: &SessionLogEntry) -> Result<StoredEvent> {
        validate_session_entry_durable_contract(entry)?;
        if matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ConversationInputPromoted(_))
        ) {
            bail!("conversation input promotion must use the critical direct promotion append API");
        }
        let event_type = session_entry_event_type(entry);
        let event_class = session_entry_event_class(event_type);
        let payload = serde_json::json!({ "session_log_entry": entry });
        self.append_event(event_type, event_class, payload)
    }

    /// Appends one ordered group of provider-visible or control session entries while holding the
    /// session writer and data-file lock for the whole group.
    pub(super) fn append_session_entry_events(
        &self,
        entries: &[SessionLogEntry],
    ) -> Result<Vec<StoredEvent>> {
        if entries.is_empty() {
            bail!("session entry append batch must not be empty");
        }
        entries
            .iter()
            .try_for_each(validate_session_entry_durable_contract)?;
        if entries.iter().any(|entry| {
            matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::ConversationInputPromoted(_))
            )
        }) {
            bail!("conversation input promotion must use the critical direct promotion append API");
        }
        let pending = entries
            .iter()
            .map(|entry| {
                let event_type = session_entry_event_type(entry);
                PendingStoredEvent {
                    event_type,
                    event_class: session_entry_event_class(event_type),
                    payload: serde_json::json!({ "session_log_entry": entry }),
                    event_id: None,
                    correlation_id: None,
                    causation_id: None,
                }
            })
            .collect::<Vec<_>>();
        if pending.len() > 1 {
            self.writer.append_crash_safe_bundle(pending)
        } else {
            self.writer.append_events(pending, false)
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads all current V2 durable records from `path`.
    pub fn read_event_records(path: impl AsRef<Path>) -> Result<Vec<SessionStreamRecord>> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(Vec::new());
        }

        let _guard = SESSION_LOG_IO_LOCK
            .lock()
            .map_err(|_| anyhow::anyhow!("session log I/O lock poisoned"))?;
        let mut file =
            fs::File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
        lock_shared_with_retry(&file, path)?;
        read_stream_records_from_file(&mut file, path)
    }

    /// Reads and validates the exact durable prefix named by a provider-request source frontier.
    ///
    /// Unlike a file-length check, this proves that `durable_end_offset` lands on the recorded
    /// JSONL event boundary and that the terminal sequence, event id, checksum, and session id all
    /// match. Records appended after the frontier are deliberately ignored.
    pub fn read_event_records_through_provider_frontier(
        path: impl AsRef<Path>,
        frontier: &crate::ProviderRequestSourceFrontierV1,
    ) -> Result<Vec<SessionStreamRecord>> {
        let path = path.as_ref();
        let _guard = SESSION_LOG_IO_LOCK
            .lock()
            .map_err(|_| anyhow::anyhow!("session log I/O lock poisoned"))?;
        let mut file =
            fs::File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
        lock_shared_with_retry(&file, path)?;
        let file_len = file
            .metadata()
            .with_context(|| format!("failed to stat {}", path.display()))?
            .len();
        if frontier.durable_end_offset > file_len {
            bail!("provider request source frontier exceeds the durable session length");
        }
        let prefix_len = usize::try_from(frontier.durable_end_offset)
            .context("provider request source frontier exceeds platform limits")?;
        let mut prefix = vec![0_u8; prefix_len];
        file.seek(SeekFrom::Start(0))
            .with_context(|| format!("failed to seek {}", path.display()))?;
        file.read_exact(&mut prefix)
            .with_context(|| format!("failed to read {}", path.display()))?;
        if !prefix.is_empty() && !prefix.ends_with(b"\n") {
            bail!("provider request source frontier is not a durable record boundary");
        }
        let content = std::str::from_utf8(&prefix)
            .context("provider request source frontier is not valid UTF-8")?;
        let records = read_stream_records_from_str(path, content)?;
        match (records.last(), frontier.stream_sequence) {
            (None, None) if frontier.session_id == session_id_for_path(path) => {}
            (Some(record), Some(sequence))
                if record.session_id() == frontier.session_id
                    && record.stream_sequence() == sequence
                    && frontier.event_id.as_deref() == Some(record.event_id())
                    && frontier.record_checksum.as_deref() == Some(record.record_checksum()) => {}
            _ => bail!("provider request source frontier does not match its durable record"),
        }
        if records
            .iter()
            .any(|record| record.session_id() != frontier.session_id)
        {
            bail!("provider request source frontier belongs to another durable session");
        }
        Ok(records)
    }

    /// Reads durable records after waiting for this store's in-process writer, without recovery.
    ///
    /// This retains the static reader's checksum, tail, and external-lock validation. It never
    /// creates a stream, repairs a tail, or publishes a projection. A caller already inside a
    /// writer callback must use the callback's records instead of re-entering this method.
    ///
    /// # Errors
    ///
    /// Returns an error when coordination fails or the strict reader cannot lock, read, or
    /// validate the durable stream.
    pub fn read_event_records_coordinated(&self) -> Result<Vec<SessionStreamRecord>> {
        self.writer.read_records_coordinated()
    }

    /// Reads all durable records in writer mode, performing tail recovery when needed.
    pub fn read_event_records_writer(&self) -> Result<Vec<SessionStreamRecord>> {
        self.writer.read_reconciled_records()
    }

    /// Appends a bounded batch of public-event delivery receipts through the existing session
    /// writer and outbox authority.
    pub fn append_public_event_delivery_batch(
        &self,
        receipts: &[PublicEventDeliveryReceiptV1],
    ) -> Result<usize> {
        self.writer.append_public_event_delivery_batch(
            receipts,
            &SessionReadBudget::default(),
            true,
        )
    }

    /// Commits one receipt bundle after cancellable coordinator acquisition. An admitted bundle
    /// completes atomically even if cancellation is requested during its physical commit. The
    /// writer must already be initialized by its session owner; this observer path never replays
    /// history or performs recovery on behalf of that owner.
    pub fn append_public_event_delivery_batch_with_budget(
        &self,
        receipts: &[PublicEventDeliveryReceiptV1],
        budget: &SessionReadBudget,
    ) -> Result<usize> {
        self.writer
            .append_public_event_delivery_batch(receipts, budget, false)
    }

    pub(super) fn load_entries_writer_reconciled(
        &self,
        initial_provider_name: String,
        initial_model_name: String,
        initial_route: Option<crate::ResolvedModelRoute>,
        initial_route_trust: Option<crate::RouteEgressTrustBinding>,
    ) -> Result<(
        Vec<SessionLogEntry>,
        Vec<SessionStreamRecord>,
        String,
        String,
    )> {
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| anyhow::anyhow!("session writer lock poisoned"))?;
        // Loading/recovery is a reconciliation boundary: rebuild the writer cache from the
        // durable stream so startup sees records appended outside this process and derived
        // indexes cannot reuse a stale admission snapshot. Hot append paths keep using the
        // in-memory cache; this path intentionally performs the explicit disk replay.
        let mut records = writer.reload_records_writer()?;
        ConversationQueueDurableProjection::from_records(&records)?;
        let mut entries = session_entries_from_records(&records)?;

        let mut reconciled_entries = Vec::new();
        let recovered_at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        if !has_session_identity(&entries) {
            anyhow::ensure!(
                entries.iter().all(|entry| matches!(
                    entry,
                    SessionLogEntry::Control(ControlEntry::WorkspaceTrustDecision(_))
                )),
                "current session stream is missing its required session identity"
            );
            let route_semantic_fingerprint = initial_route
                .as_ref()
                .map(|route| route.semantic_fingerprint.clone());
            let entry = SessionLogEntry::Control(ControlEntry::SessionIdentity {
                provider_name: initial_provider_name,
                model_name: initial_model_name,
                resolved_model_route: initial_route,
            });
            entries.push(entry);
            reconciled_entries.push(entries.last().expect("identity entry was pushed").clone());
            if let (Some(route_semantic_fingerprint), Some(egress_trust_binding)) =
                (route_semantic_fingerprint, initial_route_trust)
            {
                let entry = SessionLogEntry::Control(ControlEntry::SessionRouteTrustBound {
                    route_semantic_fingerprint,
                    egress_trust_binding,
                });
                entries.push(entry);
                reconciled_entries.push(
                    entries
                        .last()
                        .expect("route trust entry was pushed")
                        .clone(),
                );
            }
        }
        let (provider_name, model_name) = session_identity_from_entries(&entries)
            .ok_or_else(|| anyhow::anyhow!("current session identity could not be decoded"))?;

        for approval in unresolved_tool_approvals(&entries, recovered_at_ms) {
            let entry = SessionLogEntry::Control(ControlEntry::ToolApproval(approval));
            entries.push(entry.clone());
            reconciled_entries.push(entry);
        }

        for execution in interrupted_tool_executions(&entries) {
            let entry = SessionLogEntry::Control(ControlEntry::ToolExecution(Box::new(execution)));
            entries.push(entry.clone());
            reconciled_entries.push(entry);
        }

        for interruption in crate::interrupted_agent_attempts_excluding_live_owner(
            &entries,
            &self.live_background_agent_thread_ids,
        ) {
            let entry = SessionLogEntry::Control(ControlEntry::AgentRunInterrupted(interruption));
            entries.push(entry.clone());
            reconciled_entries.push(entry);
        }

        for interruption in interrupted_agent_threads_excluding_live_owner(
            &entries,
            &self.live_background_agent_thread_ids,
        ) {
            let entry =
                SessionLogEntry::Control(ControlEntry::AgentThreadStatusChanged(interruption));
            entries.push(entry.clone());
            reconciled_entries.push(entry);
        }

        let tasks = crate::TaskStateProjection::from_entries(&entries);
        let agent_threads = crate::AgentThreadStateProjection::from_entries(&entries);
        let interrupted_direct_tasks = tasks
            .tasks
            .values()
            .filter(|task| {
                task.status == crate::TaskRunStatus::Running
                    && task.latest_plan_version.is_none()
                    && task.direct_execution_admission.is_some()
            })
            .filter_map(|task| {
                let children = tasks.direct_task_background_agents(&task.task_id);
                let has_unrecoverable_child = children.iter().any(|thread_id| {
                    tasks.agent_thread_status(thread_id)
                        == Some(crate::AgentThreadStatus::Interrupted)
                        && agent_threads
                            .threads
                            .get(thread_id)
                            .is_none_or(|thread| thread.result.is_none())
                });
                let all_children_terminal = !children.is_empty()
                    && children.iter().all(|thread_id| {
                        tasks
                            .agent_thread_status(thread_id)
                            .is_some_and(|status| status.is_terminal())
                    });
                (has_unrecoverable_child && all_children_terminal).then(|| {
                    SessionLogEntry::Control(ControlEntry::TaskRun(crate::TaskRunEntry {
                        task_id: task.task_id.clone(),
                        parent_session_ref: task.parent_session_ref.clone(),
                        objective: task.objective.clone(),
                        title: task.title.clone(),
                        status: crate::TaskRunStatus::Interrupted,
                        reason: Some(
                            "direct Task background child lost its live owner before a durable result was recorded"
                                .to_owned(),
                        ),
                    }))
                })
            })
            .collect::<Vec<_>>();
        for entry in interrupted_direct_tasks {
            entries.push(entry.clone());
            reconciled_entries.push(entry);
        }

        for interruption in crate::interrupted_agent_result_continuations_excluding_live_owner(
            &entries,
            &self.live_background_agent_thread_ids,
        ) {
            let entry =
                SessionLogEntry::Control(ControlEntry::AgentResultContinuation(interruption));
            entries.push(entry.clone());
            reconciled_entries.push(entry);
        }

        for stale_route in stale_expired_agent_approval_routes(&entries, recovered_at_ms) {
            let entry = SessionLogEntry::Control(ControlEntry::AgentApprovalRoute(stale_route));
            entries.push(entry.clone());
            reconciled_entries.push(entry);
        }

        for closed_route in crate::closed_agent_routes_excluding_live_owner(
            &entries,
            &self.live_background_agent_thread_ids,
        ) {
            let entry = SessionLogEntry::Control(ControlEntry::AgentRouteClosed(closed_route));
            entries.push(entry.clone());
            reconciled_entries.push(entry);
        }

        for stale_route in stale_task_approval_routes_for_restore(&entries) {
            let entry =
                SessionLogEntry::Control(ControlEntry::TaskSubagentApprovalRoute(stale_route));
            entries.push(entry.clone());
            reconciled_entries.push(entry);
        }

        for interrupted_message in crate::interrupted_agent_mailbox_messages_excluding_live_owner(
            &entries,
            &self.live_background_agent_thread_ids,
        ) {
            let entry =
                SessionLogEntry::Control(ControlEntry::AgentMailboxMessage(interrupted_message));
            entries.push(entry.clone());
            reconciled_entries.push(entry);
        }

        if !reconciled_entries.is_empty() {
            let pending = reconciled_entries
                .iter()
                .map(|entry| {
                    let event_type = session_entry_event_type(entry);
                    PendingStoredEvent {
                        event_type,
                        event_class: session_entry_event_class(event_type),
                        payload: serde_json::json!({ "session_log_entry": entry }),
                        event_id: None,
                        correlation_id: None,
                        causation_id: None,
                    }
                })
                .collect();
            let (events, _) = writer.append_events(pending, false)?;
            records.extend(events.into_iter().map(SessionStreamRecord::Stored));
        }
        drop(writer);
        self.writer.seed_records(&records)?;

        Ok((entries, records, provider_name, model_name))
    }

    /// Reads all valid JSONL entries from `path`.
    pub fn read_entries(path: impl AsRef<Path>) -> Result<Vec<SessionLogEntry>> {
        let path = path.as_ref();
        let records = Self::read_event_records(path)?;
        ConversationQueueDurableProjection::from_records(&records)?;
        session_entries_from_records(&records)
    }

    /// Decodes one JSONL record into a session entry when the record carries one.
    ///
    /// Unknown non-critical v2 records are skipped so product surfaces can tail session streams
    /// without learning each durable event payload shape.
    ///
    /// # Errors
    ///
    /// Returns an error when the line is not a valid stored event or when a
    /// stored event's embedded session entry payload is malformed.
    pub fn session_entry_from_json_line(line: &str) -> Result<Option<SessionLogEntry>> {
        Self::session_entry_from_json_line_at_path(line, Path::new("<session JSONL line>"), 1)
    }

    /// Decodes one session entry with its source location for an actionable format error.
    pub fn session_entry_from_json_line_at_path(
        line: &str,
        path: &Path,
        physical_line: usize,
    ) -> Result<Option<SessionLogEntry>> {
        let line = line.trim();
        if line.is_empty() {
            return Ok(None);
        }
        let event = stored_event_from_stream_line(line, path, physical_line)
            .context("failed to decode stored event from session JSONL line")?;
        session_entry_from_stored_event(&event)
    }

    pub(super) fn append_audit_batch(
        &self,
        batch: DurableAuditBatch,
    ) -> Result<DurableAppendReceipt> {
        self.writer.append_audit_batch(batch)
    }

    pub(crate) fn append_audit_batch_if<F>(
        &self,
        batch: DurableAuditBatch,
        should_append: F,
    ) -> Result<Option<DurableAppendReceipt>>
    where
        F: FnOnce(&[SessionStreamRecord]) -> Result<bool>,
    {
        self.writer.append_audit_batch_if(batch, should_append)
    }

    pub(super) fn validate_audit_receipt(
        &self,
        receipt: DurableAppendReceipt,
        expectation: DurableAppendExpectation,
    ) -> Result<DurableAppendPermit> {
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| anyhow::anyhow!("session writer lock poisoned"))?;
        writer.validate_audit_receipt(receipt, expectation)
    }

    /// Re-reads and synchronizes the stream under the single-writer lease after an append
    /// acknowledgement error.
    ///
    /// This operation is intentionally explicit and may perform a full scan or tail recovery. It
    /// is not part of ordinary session loading or the hot append path.
    pub fn reconcile_durable_event(
        &self,
        expectation: &DurableEventReconciliationExpectation,
    ) -> DurableEventReconciliation {
        let mut writer = match self.writer.lock() {
            Ok(writer) => writer,
            Err(_) => {
                return DurableEventReconciliation::Indeterminate {
                    reason: "session writer lock poisoned".to_owned(),
                };
            }
        };
        writer.reconcile_event(expectation)
    }

    pub fn next_stream_sequence(&self) -> Result<u64> {
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| anyhow::anyhow!("session writer lock poisoned"))?;
        writer.next_sequence()
    }

    #[cfg(test)]
    pub(super) fn writer_full_scan_count(&self) -> Result<u64> {
        let writer = self
            .writer
            .lock()
            .map_err(|_| anyhow::anyhow!("session writer lock poisoned"))?;
        Ok(writer.full_scan_count())
    }

    #[cfg(test)]
    pub(crate) fn inject_writer_fault(&self, fault: SessionWriterFault) -> Result<()> {
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| anyhow::anyhow!("session writer lock poisoned"))?;
        writer.inject_fault(fault);
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn inject_active_projection_schema_mismatch(&self) -> Result<()> {
        self.writer.inject_active_projection_schema_mismatch()
    }

    #[cfg(test)]
    pub(super) fn writer_parent_sync_count(&self) -> Result<u64> {
        let writer = self
            .writer
            .lock()
            .map_err(|_| anyhow::anyhow!("session writer lock poisoned"))?;
        Ok(writer.parent_sync_count())
    }

    #[cfg(test)]
    pub(crate) fn writer_data_sync_count(&self) -> Result<u64> {
        let writer = self
            .writer
            .lock()
            .map_err(|_| anyhow::anyhow!("session writer lock poisoned"))?;
        Ok(writer.data_sync_count())
    }
}

fn validate_session_entry_durable_contract(entry: &SessionLogEntry) -> Result<()> {
    match entry {
        SessionLogEntry::Control(control) => control.validate_durable_contract(),
        SessionLogEntry::ToolResultV3(result) => result.validate(),
        SessionLogEntry::RuntimeContextSnapshotV2(snapshot) => snapshot.validate(),
        _ => Ok(()),
    }
}

pub(super) fn session_entries_from_records(
    records: &[SessionStreamRecord],
) -> Result<Vec<SessionLogEntry>> {
    let mut projection = SessionEntryProjection::default();
    for record in records {
        projection.apply_record(record)?;
    }
    Ok(projection.entries)
}

#[derive(Default)]
pub(super) struct SessionEntryProjection {
    pub(super) entries: Vec<SessionLogEntry>,
    pub(super) cursor: Option<ProjectionCursor>,
}

impl SessionEntryProjection {
    pub(super) fn apply_record(&mut self, record: &SessionStreamRecord) -> Result<()> {
        let cursor = record.projection_cursor(SESSION_ENTRY_PROJECTION_SCHEMA_VERSION);
        let event = record.domain_event_record()?.map(|record| record.event);
        self.apply_cursor_and_event(cursor, event.as_ref())
    }

    pub(super) fn apply_cursor_and_event(
        &mut self,
        cursor: ProjectionCursor,
        event: Option<&DomainEvent>,
    ) -> Result<()> {
        let last_applied_record_checksum = &cursor.last_applied_record_checksum;
        match projection_apply_decision_for_record(
            self.cursor.as_ref(),
            &cursor.session_id,
            cursor.last_applied_stream_sequence,
            &cursor.last_applied_event_id,
            last_applied_record_checksum,
        )? {
            ProjectionApplyDecision::IgnoreAlreadyApplied => return Ok(()),
            ProjectionApplyDecision::Apply => {}
        }
        if let Some(event) = event
            && let Some(entry) = session_entry_from_domain_event(event)?
        {
            self.entries.push(entry);
        }
        self.cursor = Some(cursor);
        Ok(())
    }
}

pub(super) fn has_session_identity(entries: &[SessionLogEntry]) -> bool {
    entries.iter().any(is_session_identity_entry)
}

pub(super) fn is_session_identity_entry(entry: &SessionLogEntry) -> bool {
    matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::SessionIdentity { .. })
    )
}

pub(super) fn read_stream_records_from_file(
    file: &mut File,
    path: &Path,
) -> Result<Vec<SessionStreamRecord>> {
    file.seek(SeekFrom::Start(0))
        .with_context(|| format!("failed to seek {}", path.display()))?;
    let mut content = String::new();
    file.read_to_string(&mut content)
        .with_context(|| format!("failed to read {}", path.display()))?;
    read_stream_records_from_str(path, &content)
}

/// Streaming reader for a validated session stream.
///
/// Unlike [`JsonlSessionStore::read_event_records`], this reader retains only the current line
/// and validation cursor. It is intended for bounded transcript/page projections that must not
/// materialize an entire long-lived session in memory.
pub struct SessionStreamRecordReader {
    reader: BufReader<File>,
    path: PathBuf,
    physical_line: usize,
    record_ordinal: u64,
    expected_session_id: Option<String>,
}

impl Iterator for SessionStreamRecordReader {
    type Item = Result<SessionStreamRecord>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let mut line = String::new();
            match self.reader.read_line(&mut line) {
                Ok(0) => return None,
                Ok(_) => {}
                Err(error) => return Some(Err(anyhow::Error::new(error))),
            }
            self.physical_line = self.physical_line.saturating_add(1);
            if line.trim().is_empty() {
                continue;
            }
            self.record_ordinal = self.record_ordinal.saturating_add(1);
            let event = match stored_event_from_stream_line(
                line.trim_end_matches(['\r', '\n']),
                &self.path,
                self.physical_line,
            ) {
                Ok(event) => event,
                Err(error) => return Some(Err(error)),
            };
            if let Err(error) = validate_stream_record_identity(
                self.physical_line,
                self.record_ordinal,
                &event.session_id,
                event.stream_sequence,
                &mut self.expected_session_id,
            ) {
                return Some(Err(error));
            }
            return Some(Ok(SessionStreamRecord::Stored(event)));
        }
    }
}

impl JsonlSessionStore {
    /// Opens a shared-locked streaming reader without retaining the complete session in memory.
    pub fn read_event_record_stream(path: impl AsRef<Path>) -> Result<SessionStreamRecordReader> {
        let path = path.as_ref();
        let file =
            fs::File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
        lock_shared_with_retry(&file, path)?;
        Ok(SessionStreamRecordReader {
            reader: BufReader::new(file),
            path: path.to_path_buf(),
            physical_line: 0,
            record_ordinal: 0,
            expected_session_id: None,
        })
    }
}

pub(super) fn read_event_record_range_locked(
    file: &mut File,
    path: &Path,
    start_offset: u64,
    expected_sequence: u64,
    expected_session_id: Option<&str>,
    max_records: usize,
    max_bytes: usize,
    budget: &SessionReadBudget,
) -> Result<SessionRecordRange> {
    let source = SessionRecordSourceSnapshot {
        metadata: file
            .metadata()
            .with_context(|| format!("failed to stat {}", path.display()))?,
    };
    let file_len = source.byte_len();
    if start_offset > file_len {
        bail!("session record range starts beyond the durable stream");
    }
    if start_offset > 0 {
        file.seek(SeekFrom::Start(start_offset.saturating_sub(1)))
            .with_context(|| format!("failed to seek {}", path.display()))?;
        let mut previous = [0_u8; 1];
        file.read_exact(&mut previous)
            .with_context(|| format!("failed to read {}", path.display()))?;
        if previous[0] != b'\n' {
            bail!("session record range does not begin at a line boundary");
        }
    }
    file.seek(SeekFrom::Start(start_offset))
        .with_context(|| format!("failed to seek {}", path.display()))?;
    let mut reader = BufReader::new(file.try_clone()?);
    reader
        .seek(SeekFrom::Start(start_offset))
        .with_context(|| format!("failed to seek {}", path.display()))?;
    let mut records = Vec::with_capacity(max_records.min(256));
    let mut record_offsets = Vec::with_capacity(max_records.min(256));
    let mut record_end_offsets = Vec::with_capacity(max_records.min(256));
    let mut consumed = 0usize;
    let mut expected_session = expected_session_id.map(str::to_owned);
    let mut next_sequence = expected_sequence;
    while records.len() < max_records && consumed < max_bytes {
        budget.check()?;
        let mut bytes = Vec::new();
        loop {
            budget.check()?;
            let available = reader.fill_buf()?;
            if available.is_empty() {
                break;
            }
            let length = available
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(available.len(), |index| index + 1);
            if bytes.len().saturating_add(length) > MAX_SESSION_RAW_RECORD_BYTES {
                bail!("session raw record exceeds its byte bound");
            }
            if consumed.saturating_add(bytes.len()).saturating_add(length) > max_bytes {
                if records.is_empty() {
                    bail!("session record range exceeds its byte bound");
                }
                return Ok(SessionRecordRange {
                    records,
                    record_offsets,
                    record_end_offsets,
                    start_offset,
                    source,
                    end_offset: start_offset + consumed as u64,
                    has_more: true,
                });
            }
            let complete = available[length - 1] == b'\n';
            bytes.extend_from_slice(&available[..length]);
            reader.consume(length);
            if complete {
                break;
            }
        }
        let read = bytes.len();
        if read == 0 {
            break;
        }
        let line = std::str::from_utf8(&bytes).context("session record is not UTF-8")?;
        let record_offset = start_offset.saturating_add(consumed as u64);
        consumed = consumed.saturating_add(read);
        if line.trim().is_empty() {
            continue;
        }
        let sequence = next_sequence
            .checked_add(1)
            .context("session record range sequence exhausted")?;
        let event = stored_event_from_stream_line(
            line.trim_end_matches(['\r', '\n']),
            path,
            sequence as usize,
        )?;
        if event.stream_sequence != sequence {
            bail!("session record range has a sequence gap");
        }
        if let Some(expected) = expected_session.as_deref() {
            if expected != event.session_id {
                bail!("session record range belongs to another session");
            }
        } else {
            expected_session = Some(event.session_id.clone());
        }
        record_offsets.push(record_offset);
        record_end_offsets.push(start_offset + consumed as u64);
        records.push(SessionStreamRecord::Stored(event));
        next_sequence = sequence;
    }
    let end_offset = start_offset.saturating_add(consumed as u64);
    Ok(SessionRecordRange {
        records,
        record_offsets,
        record_end_offsets,
        start_offset,
        end_offset,
        has_more: end_offset < file_len,
        source,
    })
}

pub(super) fn read_stream_records_from_str(
    path: &Path,
    content: &str,
) -> Result<Vec<SessionStreamRecord>> {
    let raw_records = content
        .lines()
        .enumerate()
        .filter_map(|(line_index, line)| {
            (!line.trim().is_empty()).then_some((line_index + 1, line.to_owned()))
        })
        .collect::<Vec<_>>();
    if raw_records.is_empty() {
        return Ok(Vec::new());
    }

    let mut records = Vec::with_capacity(raw_records.len());
    let mut expected_session_id = None;
    for (record_ordinal, (physical_line, line)) in raw_records.iter().enumerate() {
        let stream_sequence = record_ordinal as u64 + 1;
        let event = stored_event_from_stream_line(line, path, *physical_line)?;
        validate_stream_record_identity(
            *physical_line,
            stream_sequence,
            &event.session_id,
            event.stream_sequence,
            &mut expected_session_id,
        )?;
        records.push(SessionStreamRecord::Stored(event));
    }
    Ok(records)
}

pub(super) fn validate_stream_record_identity(
    physical_line: usize,
    expected_sequence: u64,
    session_id: &str,
    stream_sequence: u64,
    expected_session_id: &mut Option<String>,
) -> Result<()> {
    if stream_sequence != expected_sequence {
        let message =
            stream_sequence_mismatch_message(physical_line, stream_sequence, expected_sequence);
        return Err(anyhow::anyhow!(message));
    }
    match expected_session_id {
        Some(expected) if expected != session_id => {
            let message = stream_session_mismatch_message(physical_line, session_id, expected);
            return Err(anyhow::anyhow!(message));
        }
        Some(_) => {}
        None => *expected_session_id = Some(session_id.to_owned()),
    }
    Ok(())
}

pub(super) fn stream_sequence_mismatch_message(
    physical_line: usize,
    stream_sequence: u64,
    expected_sequence: u64,
) -> String {
    const PREFIX: &str = "stream_sequence does not match expected sequence";
    format!("{PREFIX} on line {physical_line}: {stream_sequence} vs {expected_sequence}")
}

pub(super) fn stream_session_mismatch_message(
    physical_line: usize,
    session_id: &str,
    expected: &str,
) -> String {
    const PREFIX: &str = "session_id does not match stream session_id";
    format!("{PREFIX} on line {physical_line}: {session_id} vs {expected}")
}

pub(super) fn stream_line_context(kind: &str, physical_line: usize, path: &Path) -> String {
    let path = path.display();
    format!("failed to parse {kind} on line {physical_line} from {path}")
}

pub(super) fn append_stored_event_to_locked_file(
    file: &mut File,
    event: &StoredEvent,
) -> Result<()> {
    file.seek(SeekFrom::End(0))
        .context("failed to seek session log before append")?;
    let line = event.to_json_line()?;
    file.write_all(line.as_bytes())
        .context("failed to append stored event")?;
    file.flush().context("failed to flush stored event")?;
    if event.sync_class()? != EventSyncClass::NormalEvent {
        file.sync_all().context("failed to sync stored event")?;
    }
    Ok(())
}

pub(super) fn event_id_seed(
    session_id: &str,
    stream_sequence: u64,
    event_type: DurableEventType,
    payload: &serde_json::Value,
) -> String {
    let event_type = event_type.as_str();
    let payload_hash = stable_json_hash(payload);
    format!("{session_id}:{stream_sequence}:{event_type}:{payload_hash}")
}

pub(super) fn stream_session_id(records: &[SessionStreamRecord]) -> Option<String> {
    records.last().map(|record| record.session_id().to_owned())
}

pub(super) fn session_id_for_path(path: &Path) -> String {
    let path_key = path.as_os_str().to_string_lossy();
    stable_event_uuid("sigil-session-path", &path_key)
}

pub(super) fn next_stream_sequence(records: &[SessionStreamRecord]) -> u64 {
    records
        .iter()
        .map(SessionStreamRecord::stream_sequence)
        .max()
        .map_or(1, |max_sequence| max_sequence + 1)
}

pub(super) fn session_entry_event_type(entry: &SessionLogEntry) -> DurableEventType {
    match entry {
        SessionLogEntry::User(_) => DurableEventType::UserMessageRecorded,
        SessionLogEntry::Assistant(_) => DurableEventType::AssistantMessageRecorded,
        SessionLogEntry::RuntimeContextSnapshotV2(_) => {
            DurableEventType::RuntimeContextSnapshotRecordedV2
        }
        SessionLogEntry::ToolResultV3(_) => DurableEventType::ToolResultRecordedV3,
        SessionLogEntry::Control(control) => control_entry_event_type(control),
    }
}

pub(super) fn session_entry_event_class(event_type: DurableEventType) -> EventClass {
    if event_type == DurableEventType::ContextSourceCaptured {
        return EventClass::NonCritical;
    }
    if event_type == DurableEventType::SessionEntryRecorded {
        return EventClass::NonCritical;
    }
    EventClass::Critical
}

pub(super) fn control_entry_event_type(entry: &ControlEntry) -> DurableEventType {
    match entry {
        ControlEntry::ProviderDiagnostic(_) => DurableEventType::DiagnosticRecorded,
        ControlEntry::SessionCompositionBound(_) => DurableEventType::SessionCompositionBound,
        ControlEntry::SessionRuntimeTransitionV1(_) => DurableEventType::SessionRuntimeTransitionV1,
        ControlEntry::ApplicationOperationPreparedV1(_) => {
            DurableEventType::ApplicationOperationPreparedV1
        }
        ControlEntry::ApplicationOperationCommittedV1(_) => {
            DurableEventType::ApplicationOperationCommittedV1
        }
        ControlEntry::ToolApproval(approval)
            if approval.action == ToolApprovalAuditAction::Resolved =>
        {
            DurableEventType::ApprovalResolved
        }
        ControlEntry::ToolApproval(_) => DurableEventType::SessionEntryRecorded,
        ControlEntry::ToolExecution(execution) => tool_execution_event_type(execution.status),
        ControlEntry::ToolArtifactRead(_) => DurableEventType::ToolArtifactReadRecorded,
        ControlEntry::ToolEgress(_) => DurableEventType::EgressDecisionRecorded,
        ControlEntry::PluginTrustDecision(_) => DurableEventType::ExtensionTrustDecision,
        ControlEntry::PluginHookExecutionStarted(_) => DurableEventType::PluginHookExecutionStarted,
        ControlEntry::PluginHookExecutionFinished(_) => {
            DurableEventType::PluginHookExecutionFinished
        }
        ControlEntry::AgentProfileTrustDecision(_) => DurableEventType::ExtensionTrustDecision,
        ControlEntry::PlanDraftCreated(_) => DurableEventType::PlanDraftCreated,
        ControlEntry::PlanDecisionRecorded(_) => DurableEventType::PlanDecisionRecorded,
        ControlEntry::PlanPermissionGranted(_) => DurableEventType::PlanPermissionGranted,
        ControlEntry::ConversationRouteDecisionRecorded(_) => {
            DurableEventType::ConversationRouteDecisionRecorded
        }
        ControlEntry::PlanReviewAttempt(_) => DurableEventType::PlanReviewAttempt,
        ControlEntry::PlanReviewResolutionRecordedV1(_) => {
            DurableEventType::PlanReviewResolutionRecorded
        }
        ControlEntry::UserInputRequested(_)
        | ControlEntry::UserInputDecisionAccepted(_)
        | ControlEntry::UserInputContinuationClaimed(_)
        | ControlEntry::UserInputContinuationStarted(_)
        | ControlEntry::UserInputContinuationReleased(_)
        | ControlEntry::UserInputResolved(_) => DurableEventType::UserInputLifecycleChanged,
        ControlEntry::TaskCreatedFromPlan(_) => DurableEventType::TaskCreatedFromPlan,
        ControlEntry::TaskDirectExecutionAdmittedV1(_)
        | ControlEntry::TaskDirectRequirementsBoundV1(_)
        | ControlEntry::TaskDirectExecutionAttemptV1(_)
        | ControlEntry::TaskChecklistUpdatedV1(_) => DurableEventType::TaskStatusChanged,
        ControlEntry::TaskHandoffRequested(_) => DurableEventType::TaskHandoffRequested,
        ControlEntry::TaskHandoffResolved(_) => DurableEventType::TaskHandoffResolved,
        ControlEntry::TaskContinuationSelected(_) => DurableEventType::TaskStatusChanged,
        ControlEntry::TaskRun(_) => DurableEventType::TaskStatusChanged,
        ControlEntry::TaskRunCancellationScopeBound(_) => DurableEventType::TaskStatusChanged,
        ControlEntry::TaskRunTargetSelected(_) => DurableEventType::TaskStatusChanged,
        ControlEntry::TaskPlan(_) => DurableEventType::TaskStatusChanged,
        ControlEntry::TaskStepContractBoundV2(_) => DurableEventType::TaskStatusChanged,
        ControlEntry::TaskPlanContractSetCommittedV2(_) => DurableEventType::TaskStatusChanged,
        ControlEntry::TaskStep(_) => DurableEventType::TaskStatusChanged,
        ControlEntry::TaskParticipantAttempt(_) => DurableEventType::TaskStatusChanged,
        ControlEntry::TaskParticipantResult(_) => DurableEventType::TaskStatusChanged,
        ControlEntry::JobIntentRecorded(_) => DurableEventType::JobIntentRecorded,
        ControlEntry::StepLeaseRecorded(_) => DurableEventType::StepLeaseRecorded,
        ControlEntry::StepLeaseHeartbeatRecorded(_) => DurableEventType::StepLeaseHeartbeatRecorded,
        ControlEntry::CheckSpecRecorded(_) => DurableEventType::CheckSpecRecorded,
        ControlEntry::VerificationPolicyChanged(_) => DurableEventType::VerificationPolicyChanged,
        ControlEntry::VerificationCheckRun(_) => DurableEventType::VerificationCheckRun,
        ControlEntry::VerificationRecorded(_) => DurableEventType::VerificationRecorded,
        ControlEntry::VerificationReceiptLinkRecorded(_) => {
            DurableEventType::VerificationReceiptLinkRecorded
        }
        ControlEntry::VerificationFailureLocatorRecorded(_) => {
            DurableEventType::VerificationFailureLocatorRecorded
        }
        ControlEntry::ReadinessEvaluated(_) => DurableEventType::ReadinessEvaluated,
        ControlEntry::ChildVerificationReceiptLinked(_) => {
            DurableEventType::ChildVerificationReceiptLinked
        }
        ControlEntry::WorkspaceTrustDecision(_) => DurableEventType::WorkspaceTrustDecision,
        ControlEntry::WriteLeaseAcquired(_) => DurableEventType::WriteLeaseAcquired,
        ControlEntry::WriteLeaseReleased(_) => DurableEventType::WriteLeaseReleased,
        ControlEntry::IsolatedWorkspacePrepared(_) => DurableEventType::IsolatedWorkspacePrepared,
        ControlEntry::IsolatedWorkspaceCreated(_) => DurableEventType::IsolatedWorkspaceCreated,
        ControlEntry::IsolatedWorkspaceCleanupRecorded(_) => {
            DurableEventType::IsolatedWorkspaceCleanupRecorded
        }
        ControlEntry::IsolatedChangeSetProduced(_) => DurableEventType::IsolatedChangeSetProduced,
        ControlEntry::MergeReviewRequested(_) => DurableEventType::MergeReviewRequested,
        ControlEntry::MergeReviewResolved(_) => DurableEventType::MergeReviewResolved,
        ControlEntry::IntegrationPlanRecorded(_) => DurableEventType::IntegrationPlanRecorded,
        ControlEntry::IntegrationLaneChanged(_) => DurableEventType::IntegrationLaneChanged,
        ControlEntry::IntegrationLanePrepared(_) => DurableEventType::IntegrationLanePrepared,
        ControlEntry::IntegrationLaneMemberApplied(_) => {
            DurableEventType::IntegrationLaneMemberApplied
        }
        ControlEntry::IntegrationLaneVerificationLinked(_) => {
            DurableEventType::IntegrationLaneVerificationLinked
        }
        ControlEntry::IntegrationLaneTerminal(_) => DurableEventType::IntegrationLaneTerminal,
        ControlEntry::IntegrationLaneCleanupRecorded(_) => {
            DurableEventType::IntegrationLaneCleanupRecorded
        }
        ControlEntry::TaskPromotionPreviewRecorded(_) => {
            DurableEventType::TaskPromotionPreviewRecorded
        }
        ControlEntry::TaskPromotionAuthorityConsumed(_) => {
            DurableEventType::TaskPromotionAuthorityConsumed
        }
        ControlEntry::IntegrationPromotionRecorded(_) => {
            DurableEventType::IntegrationPromotionRecorded
        }
        ControlEntry::TaskParentVerificationRecorded(_) => {
            DurableEventType::TaskParentVerificationRecorded
        }
        ControlEntry::TaskGuidancePromoted(_) => DurableEventType::TaskGuidancePromoted,
        ControlEntry::OrchestrationRouteDisabled(_) => DurableEventType::OrchestrationRouteDisabled,
        ControlEntry::PrefixSnapshotCaptured(_) => DurableEventType::ContextSourceCaptured,
        ControlEntry::MemorySnapshotCaptured(_) => DurableEventType::ContextSourceCaptured,
        ControlEntry::ContextAssemblySkipped(_) => DurableEventType::ContextSourceCaptured,
        ControlEntry::SkillIndexCaptured(_) => DurableEventType::ContextSourceCaptured,
        ControlEntry::SkillLoaded(_) => DurableEventType::ContextSourceCaptured,
        ControlEntry::PluginManifestCaptured(_) => DurableEventType::ContextSourceCaptured,
        ControlEntry::AgentProfileCaptured(_) => DurableEventType::ContextSourceCaptured,
        _ => DurableEventType::SessionEntryRecorded,
    }
}

pub(super) fn tool_execution_event_type(status: ToolExecutionStatus) -> DurableEventType {
    if status == ToolExecutionStatus::Started {
        DurableEventType::ToolExecutionStarted
    } else {
        DurableEventType::ToolExecutionFinished
    }
}

pub(super) fn session_entry_from_stored_event(
    event: &StoredEvent,
) -> Result<Option<SessionLogEntry>> {
    if event.event_kind().is_none() {
        return Ok(None);
    }
    if event.event_kind() == Some(DurableEventType::ConversationInputPromoted) {
        let entry: ConversationInputPromotedEntry =
            serde_json::from_value(event.payload.clone())
                .context("failed to decode conversation input promoted event payload")?;
        entry.validate_for_session(&event.session_id)?;
        return Ok(Some(SessionLogEntry::Control(
            ControlEntry::ConversationInputPromoted(entry),
        )));
    }
    if event.event_kind() == Some(DurableEventType::ToolResultRecordedV2) {
        bail!(
            "unsupported session schema: found legacy tool_result_recorded_v2 (expected tool-result-v3); old sessions are not migratable"
        );
    }
    if event.event_kind() == Some(DurableEventType::SessionCompositionBound) {
        return session_composition_entry_from_payload(&event.payload).map(Some);
    }
    let Some(value) = event.payload.get("session_log_entry") else {
        return Ok(None);
    };
    let entry: SessionLogEntry = serde_json::from_value(value.clone())
        .context("failed to decode session entry from stored event payload")?;
    validate_session_entry_durable_contract(&entry)?;
    if matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::SessionCompositionBound(_))
    ) && event.event_kind() != Some(DurableEventType::SessionCompositionBound)
    {
        bail!("session composition used the wrong durable event type");
    }
    if matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::ProviderDiagnostic(_))
    ) && event.event_kind() != Some(DurableEventType::DiagnosticRecorded)
    {
        bail!("provider diagnostic used the wrong durable event type");
    }
    if let SessionLogEntry::ToolResultV3(result) = &entry {
        if event.event_kind() != Some(DurableEventType::ToolResultRecordedV3) {
            bail!("tool result payload used the wrong durable event type");
        }
        result.validate()?;
    }
    if let SessionLogEntry::Control(ControlEntry::ToolArtifactRead(receipt)) = &entry {
        if event.event_kind() != Some(DurableEventType::ToolArtifactReadRecorded) {
            bail!("tool artifact read receipt used the wrong durable event type");
        }
        receipt.validate()?;
    }
    Ok(Some(entry))
}

pub(crate) fn session_entry_from_domain_event(
    event: &DomainEvent,
) -> Result<Option<SessionLogEntry>> {
    if let DomainEvent::ConversationInputPromoted(payload) = event {
        let entry: ConversationInputPromotedEntry = serde_json::from_value(payload.payload.clone())
            .context("failed to decode conversation input promoted domain payload")?;
        entry.validate_shape()?;
        return Ok(Some(SessionLogEntry::Control(
            ControlEntry::ConversationInputPromoted(entry),
        )));
    }
    if event.event_type() == DurableEventType::ToolResultRecordedV2 {
        bail!(
            "unsupported session schema: found legacy tool_result_recorded_v2 (expected tool-result-v3); old sessions are not migratable"
        );
    }
    let payload = event
        .payload()
        .expect("v2 durable domain event must carry a payload");
    if event.event_type() == DurableEventType::SessionCompositionBound {
        return session_composition_entry_from_payload(&payload.payload).map(Some);
    }
    let Some(value) = payload.payload.get("session_log_entry") else {
        return Ok(None);
    };
    let entry: SessionLogEntry = serde_json::from_value(value.clone())
        .context("failed to decode session entry from domain event payload")?;
    validate_session_entry_durable_contract(&entry)?;
    if matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::SessionCompositionBound(_))
    ) && event.event_type() != DurableEventType::SessionCompositionBound
    {
        bail!("session composition used the wrong durable event type");
    }
    if let SessionLogEntry::ToolResultV3(result) = &entry {
        if event.event_type() != DurableEventType::ToolResultRecordedV3 {
            bail!("tool result payload used the wrong durable event type");
        }
        result.validate()?;
    }
    if let SessionLogEntry::Control(ControlEntry::ToolArtifactRead(receipt)) = &entry {
        if event.event_type() != DurableEventType::ToolArtifactReadRecorded {
            bail!("tool artifact read receipt used the wrong durable event type");
        }
        receipt.validate()?;
    }
    Ok(Some(entry))
}

fn session_composition_entry_from_payload(payload: &serde_json::Value) -> Result<SessionLogEntry> {
    let value = payload
        .get("session_log_entry")
        .context("session composition event is missing its session_log_entry payload")?;
    let entry: SessionLogEntry = serde_json::from_value(value.clone())
        .context("failed to decode session composition event payload")?;
    let SessionLogEntry::Control(ControlEntry::SessionCompositionBound(snapshot)) = &entry else {
        bail!("session composition event carried a different session entry");
    };
    snapshot.validate()?;
    Ok(entry)
}

pub(super) fn lock_shared_with_retry(file: &File, path: &Path) -> Result<()> {
    lock_shared_with_budget(file, path, &SessionReadBudget::default())
}

pub(super) fn lock_shared_with_budget(
    file: &File,
    path: &Path,
    budget: &SessionReadBudget,
) -> Result<()> {
    let mut last_error = None;
    for attempt in 0..=SESSION_LOG_SHARED_LOCK_RETRIES {
        budget.check()?;
        SESSION_SHARED_LOCK_ATTEMPT_TOTAL.fetch_add(1, Ordering::Relaxed);
        match file.try_lock_shared() {
            Ok(()) => return Ok(()),
            Err(std::fs::TryLockError::WouldBlock) => {
                SESSION_LOCK_CONTENTION_TOTAL.fetch_add(1, Ordering::Relaxed);
                if attempt < SESSION_LOG_SHARED_LOCK_RETRIES {
                    thread::sleep(SESSION_LOG_SHARED_LOCK_RETRY_DELAY);
                    continue;
                }
            }
            Err(std::fs::TryLockError::Error(error)) => {
                last_error = Some(error);
                break;
            }
        }
    }
    SESSION_LOCK_FAILURE_TOTAL.fetch_add(1, Ordering::Relaxed);
    if let Some(error) = last_error {
        Err(error).with_context(|| format!("failed to lock {}", path.display()))
    } else {
        Err(SessionIoBusyError {
            kind: SessionIoBusyKind::Reader,
            path: path.to_path_buf(),
        }
        .into())
    }
}

pub(super) fn lock_exclusive_with_retry(file: &File, path: &Path) -> Result<()> {
    let mut last_error = None;
    let mut exhausted_contention = false;
    for attempt in 0..=SESSION_LOG_SHARED_LOCK_RETRIES {
        SESSION_EXCLUSIVE_LOCK_ATTEMPT_TOTAL.fetch_add(1, Ordering::Relaxed);
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(()),
            Err(error) if lock_is_contended(&error) => {
                SESSION_LOCK_CONTENTION_TOTAL.fetch_add(1, Ordering::Relaxed);
                last_error = Some(error);
                if attempt < SESSION_LOG_SHARED_LOCK_RETRIES {
                    thread::sleep(SESSION_LOG_SHARED_LOCK_RETRY_DELAY);
                    continue;
                }
                exhausted_contention = true;
            }
            Err(error) => {
                last_error = Some(error);
                break;
            }
        }
    }
    SESSION_LOCK_FAILURE_TOTAL.fetch_add(1, Ordering::Relaxed);
    if exhausted_contention {
        Err(SessionIoBusyError {
            kind: SessionIoBusyKind::Writer,
            path: path.to_path_buf(),
        }
        .into())
    } else if let Some(error) = last_error {
        Err(error).with_context(|| format!("failed to lock {}", path.display()))
    } else {
        bail!("failed to lock {}", path.display())
    }
}

fn lock_is_contended(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::WouldBlock
        || error.raw_os_error() == fs2::lock_contended_error().raw_os_error()
}

#[cfg(test)]
#[path = "tests/session_record_read_handle_tests.rs"]
mod read_handle_tests;
