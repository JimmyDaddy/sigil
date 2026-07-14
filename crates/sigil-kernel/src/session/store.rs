#[cfg(test)]
use super::writer::SessionWriterFault;
use super::writer::{LinearSessionWriter, PendingStoredEvent, shared_session_writer};
use super::*;
use crate::EventId;
use thiserror::Error;

/// A session stream uses a durable format this pre-release build intentionally no longer reads.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[error(
    "session log uses unsupported {format_name} format on line {physical_line} in {path}; the file was not modified"
)]
pub struct SessionStreamCompatibilityError {
    /// Source session log that was left untouched.
    pub path: PathBuf,
    /// One-based physical line containing the unsupported record.
    pub physical_line: usize,
    /// Stable name of the unsupported pre-release payload shape.
    pub format_name: &'static str,
}

pub(super) fn unsupported_legacy_session_log_entry(
    path: &Path,
    physical_line: usize,
) -> anyhow::Error {
    SessionStreamCompatibilityError {
        path: path.to_path_buf(),
        physical_line,
        format_name: "legacy SessionLogEntry",
    }
    .into()
}

pub(super) fn unsupported_legacy_compaction_record(
    path: &Path,
    physical_line: usize,
) -> anyhow::Error {
    SessionStreamCompatibilityError {
        path: path.to_path_buf(),
        physical_line,
        format_name: "legacy CompactionRecord payload",
    }
    .into()
}

pub(super) fn is_unsupported_legacy_session_error(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<SessionStreamCompatibilityError>()
        .is_some()
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
    if serde_json::from_str::<SessionLogEntry>(line).is_ok() {
        return Err(unsupported_legacy_session_log_entry(path, physical_line));
    }

    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return Ok(None);
    };
    // The removed pre-release compaction payload was written as a raw JSON object rather than a
    // `SessionLogEntry` enum or V2 event envelope. Treat it as an explicitly unsupported format
    // before tail recovery gets a chance to interpret its final line as malformed bytes.
    if value
        .get("control")
        .and_then(|control| control.get("compaction_applied"))
        .is_some()
    {
        return Err(unsupported_legacy_compaction_record(path, physical_line));
    }
    let looks_like_stored_event = ["schema_version", "event_type"]
        .into_iter()
        .all(|field| value.get(field).is_some());
    if !looks_like_stored_event {
        return Ok(None);
    }

    let event = StoredEvent::from_json_str(line)
        .with_context(|| stream_line_context("stored event", physical_line, path))?;
    reject_legacy_compaction_record_payload(&event, path, physical_line)?;
    Ok(Some(event))
}

fn reject_legacy_compaction_record_payload(
    event: &StoredEvent,
    path: &Path,
    physical_line: usize,
) -> Result<()> {
    let Some(event_type) = event.event_kind() else {
        return Ok(());
    };
    if event_type.payload_metadata().storage != DurableEventPayloadStorage::SessionLogEntry {
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
        return Err(unsupported_legacy_compaction_record(path, physical_line));
    }
    Ok(())
}

/// Append-only JSONL store for session and control-plane history.
#[derive(Debug, Clone)]
pub struct JsonlSessionStore {
    path: PathBuf,
    writer: std::sync::Arc<Mutex<LinearSessionWriter>>,
}

impl JsonlSessionStore {
    /// Creates a store rooted at `path`, creating parent directories when needed.
    pub fn new(path: impl Into<PathBuf>) -> Result<Self> {
        let (path, writer) = shared_session_writer(path)?;
        Ok(Self { path, writer })
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
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| anyhow::anyhow!("session writer lock poisoned"))?;
        let (mut events, _) = writer.append_events(
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
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| anyhow::anyhow!("session writer lock poisoned"))?;
        let records = writer.read_records_writer()?;
        if !should_append(&records)? {
            return Ok(false);
        }
        writer.append_events(
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
        Ok(true)
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
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| anyhow::anyhow!("session writer lock poisoned"))?;
        let records = writer.read_records_writer()?;
        if !should_append(&records)? {
            return Ok(None);
        }
        let (mut events, _) = writer.append_events(
            vec![PendingStoredEvent {
                event_type,
                event_class,
                payload,
                event_id: Some(event_id),
                correlation_id,
                causation_id,
            }],
            true,
        )?;
        events
            .pop()
            .map(Some)
            .context("session writer returned no event for a conditional append")
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
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| anyhow::anyhow!("session writer lock poisoned"))?;
        let records = writer.read_records_writer()?;
        if !should_append(&records)? {
            return Ok(None);
        }
        let (events, _) = writer.append_events(pending, true)?;
        Ok(Some(events))
    }

    /// Appends a provider-visible or control session entry as a v2 stored event.
    pub fn append_session_entry_event(&self, entry: &SessionLogEntry) -> Result<StoredEvent> {
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

    /// Reads all durable records in writer mode, performing tail recovery when needed.
    pub fn read_event_records_writer(&self) -> Result<Vec<SessionStreamRecord>> {
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| anyhow::anyhow!("session writer lock poisoned"))?;
        writer.read_records_writer()
    }

    pub(super) fn load_entries_writer_reconciled(
        &self,
        fallback_provider_name: String,
        fallback_model_name: String,
    ) -> Result<(Vec<SessionLogEntry>, String, String)> {
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| anyhow::anyhow!("session writer lock poisoned"))?;
        let records = writer.read_records_writer()?;
        ConversationQueueDurableProjection::from_records(&records)?;
        let mut entries = session_entries_from_records(&records)?;
        let (provider_name, model_name) = session_identity_from_entries(&entries)
            .unwrap_or((fallback_provider_name, fallback_model_name));

        let mut reconciled_entries = Vec::new();

        if !has_session_identity(&entries) {
            let entry = SessionLogEntry::Control(ControlEntry::SessionIdentity {
                provider_name: provider_name.clone(),
                model_name: model_name.clone(),
            });
            entries.push(entry);
            reconciled_entries.push(entries.last().expect("identity entry was pushed").clone());
        }

        for execution in interrupted_tool_executions(&entries) {
            let entry = SessionLogEntry::Control(ControlEntry::ToolExecution(Box::new(execution)));
            entries.push(entry.clone());
            reconciled_entries.push(entry);
        }

        for interruption in interrupted_agent_attempts(&entries) {
            let entry = SessionLogEntry::Control(ControlEntry::AgentRunInterrupted(interruption));
            entries.push(entry.clone());
            reconciled_entries.push(entry);
        }

        for closed_route in closed_agent_routes(&entries) {
            let entry = SessionLogEntry::Control(ControlEntry::AgentRouteClosed(closed_route));
            entries.push(entry.clone());
            reconciled_entries.push(entry);
        }

        for interrupted_message in interrupted_agent_mailbox_messages(&entries) {
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
            writer.append_events(pending, false)?;
        }

        Ok((entries, provider_name, model_name))
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
    /// Returns an error when the line is a legacy entry, not a valid stored event, or when a
    /// stored event's embedded session entry payload is malformed.
    pub fn session_entry_from_json_line(line: &str) -> Result<Option<SessionLogEntry>> {
        Self::session_entry_from_json_line_at_path(line, Path::new("<session JSONL line>"), 1)
    }

    /// Decodes one session entry with its source location for an actionable compatibility error.
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
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| anyhow::anyhow!("session writer lock poisoned"))?;
        writer.append_audit_batch(batch)
    }

    pub(crate) fn append_audit_batch_if<F>(
        &self,
        batch: DurableAuditBatch,
        should_append: F,
    ) -> Result<Option<DurableAppendReceipt>>
    where
        F: FnOnce(&[SessionStreamRecord]) -> Result<bool>,
    {
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| anyhow::anyhow!("session writer lock poisoned"))?;
        let records = writer.read_records_writer()?;
        if !should_append(&records)? {
            return Ok(None);
        }
        writer.cache_event_links_for_audit_batch(&batch, &records);
        writer.append_audit_batch(batch).map(Some)
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
    pub(super) fn writer_parent_sync_count(&self) -> Result<u64> {
        let writer = self
            .writer
            .lock()
            .map_err(|_| anyhow::anyhow!("session writer lock poisoned"))?;
        Ok(writer.parent_sync_count())
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
        SessionLogEntry::ToolResult(_) => DurableEventType::ToolResultRecorded,
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
        ControlEntry::ToolApproval(approval)
            if approval.action == ToolApprovalAuditAction::Resolved =>
        {
            DurableEventType::ApprovalResolved
        }
        ControlEntry::ToolApproval(_) => DurableEventType::SessionEntryRecorded,
        ControlEntry::ToolExecution(execution) => tool_execution_event_type(execution.status),
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
        ControlEntry::TaskCreatedFromPlan(_) => DurableEventType::TaskCreatedFromPlan,
        ControlEntry::TaskRun(_) => DurableEventType::TaskStatusChanged,
        ControlEntry::TaskPlan(_) => DurableEventType::TaskStatusChanged,
        ControlEntry::TaskStep(_) => DurableEventType::TaskStatusChanged,
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
        ControlEntry::IsolatedWorkspaceCreated(_) => DurableEventType::IsolatedWorkspaceCreated,
        ControlEntry::IsolatedChangeSetProduced(_) => DurableEventType::IsolatedChangeSetProduced,
        ControlEntry::MergeReviewRequested(_) => DurableEventType::MergeReviewRequested,
        ControlEntry::MergeReviewResolved(_) => DurableEventType::MergeReviewResolved,
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
    let Some(value) = event.payload.get("session_log_entry") else {
        return Ok(None);
    };
    let entry = serde_json::from_value(value.clone())
        .context("failed to decode session entry from stored event payload")?;
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
    let payload = event
        .payload()
        .expect("v2 durable domain event must carry a payload");
    let Some(value) = payload.payload.get("session_log_entry") else {
        return Ok(None);
    };
    let entry = serde_json::from_value(value.clone())
        .context("failed to decode session entry from domain event payload")?;
    Ok(Some(entry))
}

pub(super) fn lock_shared_with_retry(file: &File, path: &Path) -> Result<()> {
    let mut last_error = None;
    for attempt in 0..=SESSION_LOG_SHARED_LOCK_RETRIES {
        match file.try_lock_shared() {
            Ok(()) => return Ok(()),
            Err(std::fs::TryLockError::WouldBlock) => {
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
    if let Some(error) = last_error {
        Err(error).with_context(|| format!("failed to lock {}", path.display()))
    } else {
        bail!("failed to lock {}", path.display())
    }
}

pub(super) fn lock_exclusive_with_retry(file: &File, path: &Path) -> Result<()> {
    let mut last_error = None;
    for attempt in 0..=SESSION_LOG_SHARED_LOCK_RETRIES {
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                last_error = Some(error);
                if attempt < SESSION_LOG_SHARED_LOCK_RETRIES {
                    thread::sleep(SESSION_LOG_SHARED_LOCK_RETRY_DELAY);
                    continue;
                }
            }
            Err(error) => {
                last_error = Some(error);
                break;
            }
        }
    }
    if let Some(error) = last_error {
        Err(error).with_context(|| format!("failed to lock {}", path.display()))
    } else {
        bail!("failed to lock {}", path.display())
    }
}
