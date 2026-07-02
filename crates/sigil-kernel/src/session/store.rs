use super::*;

/// Append-only JSONL store for session and control-plane history.
#[derive(Debug, Clone)]
pub struct JsonlSessionStore {
    path: PathBuf,
}

impl JsonlSessionStore {
    /// Creates a store rooted at `path`, creating parent directories when needed.
    pub fn new(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        Ok(Self { path })
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
        let _guard = SESSION_LOG_IO_LOCK
            .lock()
            .map_err(|_| anyhow::anyhow!("session log I/O lock poisoned"))?;
        let mut file = self.open_locked_file()?;
        let mut records = recover_tail_if_needed_locked(&mut file, &self.path)?;
        let event = append_event_locked(
            &self.path,
            &mut file,
            &mut records,
            event_type,
            event_class,
            payload,
        )?;
        Ok(event)
    }

    /// Appends a provider-visible or control session entry as a v2 stored event.
    pub fn append_session_entry_event(&self, entry: &SessionLogEntry) -> Result<StoredEvent> {
        let event_type = session_entry_event_type(entry);
        let event_class = session_entry_event_class(event_type);
        let payload = serde_json::json!({ "session_log_entry": entry });
        self.append_event(event_type, event_class, payload)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads all mixed-format records from `path`.
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

    /// Reads all mixed-format records in writer mode, performing tail recovery when needed.
    pub fn read_event_records_writer(&self) -> Result<Vec<SessionStreamRecord>> {
        let _guard = SESSION_LOG_IO_LOCK
            .lock()
            .map_err(|_| anyhow::anyhow!("session log I/O lock poisoned"))?;
        let mut file = self.open_locked_file()?;
        recover_tail_if_needed_locked(&mut file, &self.path)
    }

    pub(super) fn load_entries_writer_reconciled(
        &self,
        fallback_provider_name: String,
        fallback_model_name: String,
    ) -> Result<(Vec<SessionLogEntry>, String, String)> {
        let _guard = SESSION_LOG_IO_LOCK
            .lock()
            .map_err(|_| anyhow::anyhow!("session log I/O lock poisoned"))?;
        let mut file = self.open_locked_file()?;
        let mut records = recover_tail_if_needed_locked(&mut file, &self.path)?;
        let mut entries = session_entries_from_records(&records)?;
        let (provider_name, model_name) = session_identity_from_entries(&entries)
            .unwrap_or((fallback_provider_name, fallback_model_name));

        if !has_session_identity(&entries) {
            let entry = SessionLogEntry::Control(ControlEntry::SessionIdentity {
                provider_name: provider_name.clone(),
                model_name: model_name.clone(),
            });
            append_session_entry_event_locked(&self.path, &mut file, &mut records, &entry)?;
            entries.push(entry);
        }

        for execution in interrupted_tool_executions(&entries) {
            let entry = SessionLogEntry::Control(ControlEntry::ToolExecution(Box::new(execution)));
            append_session_entry_event_locked(&self.path, &mut file, &mut records, &entry)?;
            entries.push(entry);
        }

        for interruption in interrupted_agent_attempts(&entries) {
            let entry = SessionLogEntry::Control(ControlEntry::AgentRunInterrupted(interruption));
            append_session_entry_event_locked(&self.path, &mut file, &mut records, &entry)?;
            entries.push(entry);
        }

        for closed_route in closed_agent_routes(&entries) {
            let entry = SessionLogEntry::Control(ControlEntry::AgentRouteClosed(closed_route));
            append_session_entry_event_locked(&self.path, &mut file, &mut records, &entry)?;
            entries.push(entry);
        }

        for interrupted_message in interrupted_agent_mailbox_messages(&entries) {
            let entry =
                SessionLogEntry::Control(ControlEntry::AgentMailboxMessage(interrupted_message));
            append_session_entry_event_locked(&self.path, &mut file, &mut records, &entry)?;
            entries.push(entry);
        }

        Ok((entries, provider_name, model_name))
    }

    /// Reads all valid JSONL entries from `path`.
    pub fn read_entries(path: impl AsRef<Path>) -> Result<Vec<SessionLogEntry>> {
        let path = path.as_ref();
        let records = Self::read_event_records(path)?;
        session_entries_from_records(&records)
    }

    /// Decodes one JSONL record into a session entry when the record carries one.
    ///
    /// This accepts both legacy `SessionLogEntry` lines and v2 `StoredEvent` lines. Unknown
    /// non-critical v2 records are skipped so product surfaces can tail mixed session streams
    /// without learning each durable event payload shape.
    ///
    /// # Errors
    ///
    /// Returns an error when the line is neither a legacy session entry nor a valid stored event,
    /// or when a stored event's embedded session entry payload is malformed.
    pub fn session_entry_from_json_line(line: &str) -> Result<Option<SessionLogEntry>> {
        let line = line.trim();
        if line.is_empty() {
            return Ok(None);
        }
        if let Ok(entry) = serde_json::from_str::<SessionLogEntry>(line) {
            return Ok(Some(entry));
        }
        let event = StoredEvent::from_json_str(line)
            .context("failed to decode stored event from session JSONL line")?;
        session_entry_from_stored_event(&event)
    }

    pub(super) fn open_locked_file(&self) -> Result<File> {
        let existed = self.path.exists();
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("failed to open {}", self.path.display()))?;
        lock_exclusive_with_retry(&file, &self.path)?;
        if !existed {
            sync_parent_dir(&self.path)?;
        }
        Ok(file)
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

    let first_v2 = raw_records
        .iter()
        .position(|(_, line)| line_is_v2_stored_event(line).unwrap_or(false));
    if first_v2.is_some()
        && raw_records
            .iter()
            .skip(first_v2.unwrap_or_default())
            .any(|(_, line)| !line_is_v2_stored_event(line).unwrap_or(false))
    {
        let path = path.display();
        bail!("legacy session entry appears after v2 stored event in {path}");
    }

    let legacy_prefix_lines = match first_v2 {
        Some(index) => &raw_records[..index],
        None => raw_records.as_slice(),
    };
    let legacy_session_id = (!legacy_prefix_lines.is_empty()).then(|| {
        let mut prefix = String::new();
        for (_, line) in legacy_prefix_lines {
            prefix.push_str(line);
            prefix.push('\n');
        }
        stable_event_uuid(
            "sigil-legacy-session",
            &stable_event_hash(prefix.as_bytes()),
        )
    });

    let mut records = Vec::with_capacity(raw_records.len());
    let mut expected_session_id = None;
    for (record_ordinal, (physical_line, line)) in raw_records.iter().enumerate() {
        let stream_sequence = record_ordinal as u64 + 1;
        if line_is_v2_stored_event(line)? {
            let event = StoredEvent::from_json_str(line)
                .with_context(|| stream_line_context("stored event", *physical_line, path))?;
            validate_stream_record_identity(
                *physical_line,
                stream_sequence,
                &event.session_id,
                event.stream_sequence,
                &mut expected_session_id,
            )?;
            records.push(SessionStreamRecord::Stored(event));
            continue;
        }

        let session_id = legacy_session_id
            .as_ref()
            .expect("legacy session id is derived when legacy records are present");
        let entry: SessionLogEntry = serde_json::from_str(line)
            .with_context(|| stream_line_context("session entry", *physical_line, path))?;
        validate_stream_record_identity(
            *physical_line,
            stream_sequence,
            session_id,
            stream_sequence,
            &mut expected_session_id,
        )?;
        let raw_line_hash = stable_event_hash(line.as_bytes());
        let event_id = stable_event_uuid(session_id, &format!("{stream_sequence}:{raw_line_hash}"));
        let payload = serde_json::to_value(&entry).context("failed to serialize legacy entry")?;
        let event = LegacyEvent {
            event_id,
            session_id: session_id.clone(),
            stream_sequence,
            raw_line_hash,
            payload,
        };
        let entry = Box::new(entry);
        records.push(SessionStreamRecord::Legacy { event, entry });
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

pub(super) fn line_is_v2_stored_event(line: &str) -> Result<bool> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return Ok(false);
    };
    Ok(is_v2_stored_event_value(&value))
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

pub(super) fn append_event_locked(
    path: &Path,
    file: &mut File,
    records: &mut Vec<SessionStreamRecord>,
    event_type: DurableEventType,
    event_class: EventClass,
    payload: serde_json::Value,
) -> Result<StoredEvent> {
    if !event_type.appendable() {
        bail!("{} cannot be appended as a v2 event", event_type.as_str());
    }

    let session_id = stream_session_id(records).unwrap_or_else(|| session_id_for_path(path));
    let next_sequence = next_stream_sequence(records);
    let event_id_seed = event_id_seed(&session_id, next_sequence, event_type, &payload);
    let event_id = stable_event_uuid("sigil-event", &event_id_seed);
    let kind = event_type;
    let class = event_class;
    let sequence = next_sequence;
    let event = StoredEvent::new(kind, class, event_id, session_id, sequence, payload)?;
    append_stored_event_to_locked_file(file, &event)?;
    records.push(SessionStreamRecord::Stored(event.clone()));
    Ok(event)
}

pub(super) fn append_session_entry_event_locked(
    path: &Path,
    file: &mut File,
    records: &mut Vec<SessionStreamRecord>,
    entry: &SessionLogEntry,
) -> Result<StoredEvent> {
    let event_type = session_entry_event_type(entry);
    let payload = serde_json::json!({ "session_log_entry": entry });
    let class = session_entry_event_class(event_type);
    append_event_locked(path, file, records, event_type, class, payload)
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
    if matches!(event.event_kind(), None | Some(DurableEventType::Legacy)) {
        return Ok(None);
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
    if let DomainEvent::Legacy(event) = event {
        let entry = serde_json::from_value(event.payload.clone())
            .context("failed to decode session entry from legacy domain event payload")?;
        return Ok(Some(entry));
    }
    let payload = event
        .payload()
        .unwrap_or_else(|| unreachable!("non-legacy domain event must carry payload"));
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
