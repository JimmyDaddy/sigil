use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Mutex,
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    CompactionConfig, MemoryConfig, MemoryLoadReport,
    agent_thread::{
        AgentApprovalRouteEntry, AgentElicitationRouteEntry, AgentMergeSafePointEntry,
        AgentProfileCapturedEntry, AgentProfilePolicyEntry, AgentProfilePolicyProjection,
        AgentProfileTrustEntry, AgentProfileTrustProjection, AgentResultContinuationEntry,
        AgentResultContinuationProjection, AgentRouteClosedEntry, AgentRunAttemptStartedEntry,
        AgentRunHeartbeatEntry, AgentRunInterruptedEntry, AgentThreadClosedEntry,
        AgentThreadDisplayNameEntry, AgentThreadMessageRoutedEntry, AgentThreadResultRecordedEntry,
        AgentThreadStartedEntry, AgentThreadStateProjection, AgentThreadStatusChangedEntry,
        closed_agent_routes, interrupted_agent_attempts,
    },
    changeset::{ChangeSet, ChangeSetProjection, ChangeSetResult},
    conversation_queue::{
        ConversationInputEditedEntry, ConversationInputQueueControlEntry,
        ConversationInputQueuedEntry, ConversationInputReorderedEntry,
        ConversationInputStatusEntry, ConversationQueueProjection,
    },
    event::{
        DomainEvent, DurableEventType, EventClass, EventSyncClass, LegacyEvent,
        ProjectionApplyDecision, ProjectionCursor, StoredEvent, StoredEventDecode,
        decode_stored_event, is_v2_stored_event_value, projection_apply_decision_for_record,
        stable_event_hash, stable_event_uuid,
    },
    memory::{apply_memory_report, materialize_memory},
    mutation::{ExecutionMutationProfile, MutationEventRecorder},
    permission::{
        ApprovalMode, PathTrustZone, PermissionConfirmation, PermissionRisk, ToolOperation,
    },
    plan::{PlanApprovalProjection, PlanApprovedEntry},
    plugin::{PluginManifestSnapshot, PluginStateProjection, PluginTrustEntry},
    provider::{
        CompletionRequest, ModelMessage, PrefixSnapshot, ProviderContinuationState, ResponseHandle,
        SessionStats, UsageStats,
    },
    skill::{SkillIndexSnapshot, SkillLoadEntry, SkillStateProjection},
    task::{
        TaskChildSessionDisplayNameEntry, TaskChildSessionEntry, TaskPlanEntry, TaskRunEntry,
        TaskStateProjection, TaskStepEntry, TaskSubagentApprovalRouteEntry,
        TaskSubagentElicitationRouteEntry,
    },
    terminal_task::{TerminalTaskEntry, TerminalTaskProjection},
    tool::{
        ToolAccess, ToolError, ToolErrorKind, ToolPreviewSnapshot, ToolResult, ToolResultMeta,
        ToolSpec, ToolSubject, ToolSubjectKind, ToolSubjectScope,
    },
    verification::{
        CheckSpecRecordedEntry, ChildVerificationReceiptLinked, ReadinessEvaluatedEntry,
        VerificationPolicyChangedEntry, VerificationRecordedEntry, VerificationStateProjection,
        WorkspaceTrustDecisionEntry,
    },
};

static SESSION_LOG_IO_LOCK: Mutex<()> = Mutex::new(());
const SESSION_LOG_SHARED_LOCK_RETRIES: usize = 50;
const SESSION_LOG_SHARED_LOCK_RETRY_DELAY: Duration = Duration::from_millis(10);

/// Append-only session log entry stored in the durable JSONL session file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::large_enum_variant)] // Keep the durable JSONL shape unboxed across control variants.
#[serde(rename_all = "snake_case")]
pub enum SessionLogEntry {
    #[serde(alias = "User")]
    User(ModelMessage),
    #[serde(alias = "Assistant")]
    Assistant(ModelMessage),
    #[serde(alias = "ToolResult")]
    ToolResult(ModelMessage),
    #[serde(alias = "Control")]
    Control(ControlEntry),
}

/// One physical record in a mixed legacy/v2 session stream.
#[derive(Debug, Clone)]
pub enum SessionStreamRecord {
    Legacy {
        event: LegacyEvent,
        entry: Box<SessionLogEntry>,
    },
    Stored(StoredEvent),
}

impl SessionStreamRecord {
    pub fn stream_sequence(&self) -> u64 {
        match self {
            Self::Legacy { event, .. } => event.stream_sequence,
            Self::Stored(event) => event.stream_sequence,
        }
    }

    pub fn session_id(&self) -> &str {
        match self {
            Self::Legacy { event, .. } => &event.session_id,
            Self::Stored(event) => &event.session_id,
        }
    }

    pub fn event_id(&self) -> &str {
        match self {
            Self::Legacy { event, .. } => &event.event_id,
            Self::Stored(event) => &event.event_id,
        }
    }

    pub fn record_checksum(&self) -> &str {
        match self {
            Self::Legacy { event, .. } => &event.raw_line_hash,
            Self::Stored(event) => &event.record_checksum,
        }
    }

    pub fn projection_cursor(&self, projection_schema_version: u16) -> ProjectionCursor {
        ProjectionCursor {
            session_id: self.session_id().to_owned(),
            projection_schema_version,
            last_applied_stream_sequence: self.stream_sequence(),
            last_applied_event_id: self.event_id().to_owned(),
            last_applied_record_checksum: self.record_checksum().to_owned(),
        }
    }

    pub fn domain_event_record(&self) -> Result<Option<DomainEventRecord>> {
        let domain_event = match self {
            Self::Legacy { event, .. } => Some(DomainEvent::Legacy(event.clone())),
            Self::Stored(event) => match decode_stored_event(event.clone())? {
                StoredEventDecode::Known(event) => Some(event),
                StoredEventDecode::UnknownNonCritical(_) => None,
            },
        };
        Ok(domain_event.map(|event| DomainEventRecord {
            event,
            cursor: self.projection_cursor(SESSION_ENTRY_PROJECTION_SCHEMA_VERSION),
        }))
    }
}

pub const SESSION_ENTRY_PROJECTION_SCHEMA_VERSION: u16 = 1;
pub const VERIFICATION_STATE_PROJECTION_SCHEMA_VERSION: u16 = 1;

/// One reducer-facing domain event plus the cursor position proving where it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct DomainEventRecord {
    pub event: DomainEvent,
    pub cursor: ProjectionCursor,
}

/// Stable compaction metadata persisted in the append-only control plane.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct CompactionRecord {
    pub summary: String,
    pub compacted_message_count: usize,
    pub retained_tail_message_count: usize,
}

/// Deterministic preview of what one manual compaction would fold and project.
#[derive(Debug, Clone)]
pub struct CompactionPreview {
    pub record: CompactionRecord,
    pub folded_messages: Vec<ModelMessage>,
    pub projected_messages: Vec<ModelMessage>,
}

/// Stable memory payload captured for a specific memory fingerprint.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MemorySnapshot {
    pub messages: Vec<ModelMessage>,
    pub report: MemoryLoadReport,
}

/// Control-plane state that must survive resume and remain outside model-facing chat history.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::large_enum_variant)] // Boxing variants would churn append-only control projection matches.
#[serde(rename_all = "snake_case")]
pub enum ControlEntry {
    #[serde(alias = "SessionIdentity")]
    SessionIdentity {
        provider_name: String,
        model_name: String,
    },
    #[serde(alias = "ContinuationStateSaved")]
    ContinuationStateSaved(ProviderContinuationState),
    #[serde(alias = "ResponseHandleTracked")]
    ResponseHandleTracked(crate::provider::ResponseHandle),
    #[serde(alias = "BackgroundTaskTracked")]
    BackgroundTaskTracked(crate::provider::BackgroundTaskHandle),
    #[serde(alias = "PrefixSnapshotCaptured")]
    PrefixSnapshotCaptured(PrefixSnapshot),
    #[serde(alias = "MemorySnapshotCaptured")]
    MemorySnapshotCaptured(MemorySnapshot),
    #[serde(alias = "UsageSnapshot")]
    UsageSnapshot(UsageStats),
    #[serde(alias = "ToolApproval")]
    ToolApproval(ToolApprovalEntry),
    #[serde(alias = "ToolExecution")]
    ToolExecution(Box<ToolExecutionEntry>),
    #[serde(alias = "ToolEgress")]
    ToolEgress(Box<ToolEgressEntry>),
    #[serde(alias = "McpElicitation")]
    McpElicitation(Box<McpElicitationEntry>),
    #[serde(alias = "ToolPreviewCaptured")]
    ToolPreviewCaptured(ToolPreviewSnapshot),
    #[serde(alias = "SkillIndexCaptured")]
    SkillIndexCaptured(SkillIndexSnapshot),
    #[serde(alias = "SkillLoaded")]
    SkillLoaded(SkillLoadEntry),
    #[serde(alias = "PluginManifestCaptured")]
    PluginManifestCaptured(PluginManifestSnapshot),
    #[serde(alias = "PluginTrustDecision")]
    PluginTrustDecision(PluginTrustEntry),
    #[serde(alias = "ChangeSetProposed")]
    ChangeSetProposed(ChangeSet),
    #[serde(alias = "ChangeSetApplied")]
    ChangeSetApplied(ChangeSetResult),
    #[serde(alias = "TerminalTask")]
    TerminalTask(TerminalTaskEntry),
    #[serde(alias = "CompactionApplied")]
    CompactionApplied(CompactionRecord),
    #[serde(alias = "PlanApproved")]
    PlanApproved(PlanApprovedEntry),
    #[serde(alias = "TaskRun")]
    TaskRun(TaskRunEntry),
    #[serde(alias = "TaskPlan")]
    TaskPlan(TaskPlanEntry),
    #[serde(alias = "TaskStep")]
    TaskStep(TaskStepEntry),
    #[serde(alias = "TaskChildSession")]
    TaskChildSession(TaskChildSessionEntry),
    #[serde(alias = "TaskChildSessionDisplayName")]
    TaskChildSessionDisplayName(TaskChildSessionDisplayNameEntry),
    #[serde(alias = "TaskSubagentApprovalRoute")]
    TaskSubagentApprovalRoute(TaskSubagentApprovalRouteEntry),
    #[serde(alias = "TaskSubagentElicitationRoute")]
    TaskSubagentElicitationRoute(TaskSubagentElicitationRouteEntry),
    #[serde(alias = "CheckSpecRecorded")]
    CheckSpecRecorded(CheckSpecRecordedEntry),
    #[serde(alias = "VerificationPolicyChanged")]
    VerificationPolicyChanged(VerificationPolicyChangedEntry),
    #[serde(alias = "VerificationRecorded")]
    VerificationRecorded(VerificationRecordedEntry),
    #[serde(alias = "ReadinessEvaluated")]
    ReadinessEvaluated(ReadinessEvaluatedEntry),
    #[serde(alias = "ChildVerificationReceiptLinked")]
    ChildVerificationReceiptLinked(ChildVerificationReceiptLinked),
    #[serde(alias = "WorkspaceTrustDecision")]
    WorkspaceTrustDecision(WorkspaceTrustDecisionEntry),
    #[serde(alias = "AgentProfileCaptured")]
    AgentProfileCaptured(AgentProfileCapturedEntry),
    #[serde(alias = "AgentProfileTrustDecision")]
    AgentProfileTrustDecision(AgentProfileTrustEntry),
    #[serde(alias = "AgentProfilePolicyDecision")]
    AgentProfilePolicyDecision(AgentProfilePolicyEntry),
    #[serde(alias = "AgentThreadStarted")]
    AgentThreadStarted(AgentThreadStartedEntry),
    #[serde(alias = "AgentThreadStatusChanged")]
    AgentThreadStatusChanged(AgentThreadStatusChangedEntry),
    #[serde(alias = "AgentThreadMessageRouted")]
    AgentThreadMessageRouted(AgentThreadMessageRoutedEntry),
    #[serde(alias = "AgentThreadResultRecorded")]
    AgentThreadResultRecorded(AgentThreadResultRecordedEntry),
    #[serde(alias = "AgentResultContinuation")]
    AgentResultContinuation(AgentResultContinuationEntry),
    #[serde(alias = "AgentThreadDisplayName")]
    AgentThreadDisplayName(AgentThreadDisplayNameEntry),
    #[serde(alias = "AgentApprovalRoute")]
    AgentApprovalRoute(AgentApprovalRouteEntry),
    #[serde(alias = "AgentElicitationRoute")]
    AgentElicitationRoute(AgentElicitationRouteEntry),
    #[serde(alias = "AgentRunAttemptStarted")]
    AgentRunAttemptStarted(AgentRunAttemptStartedEntry),
    #[serde(alias = "AgentRunHeartbeat")]
    AgentRunHeartbeat(AgentRunHeartbeatEntry),
    #[serde(alias = "AgentRunInterrupted")]
    AgentRunInterrupted(AgentRunInterruptedEntry),
    #[serde(alias = "AgentRouteClosed")]
    AgentRouteClosed(AgentRouteClosedEntry),
    #[serde(alias = "AgentMergeSafePoint")]
    AgentMergeSafePoint(AgentMergeSafePointEntry),
    #[serde(alias = "AgentThreadClosed")]
    AgentThreadClosed(AgentThreadClosedEntry),
    #[serde(alias = "ConversationInputQueued")]
    ConversationInputQueued(ConversationInputQueuedEntry),
    #[serde(alias = "ConversationInputQueueControl")]
    ConversationInputQueueControl(ConversationInputQueueControlEntry),
    #[serde(alias = "ConversationInputEdited")]
    ConversationInputEdited(ConversationInputEditedEntry),
    #[serde(alias = "ConversationInputReordered")]
    ConversationInputReordered(ConversationInputReorderedEntry),
    #[serde(alias = "ConversationInputStatusChanged")]
    ConversationInputStatusChanged(ConversationInputStatusEntry),
    #[serde(alias = "Note")]
    Note {
        kind: String,
        data: serde_json::Value,
    },
}

/// Append-only audit entry for permission policy evaluation and interactive approval decisions.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ToolApprovalEntry {
    pub action: ToolApprovalAuditAction,
    pub call_id: String,
    pub tool_name: String,
    pub access: ToolAccess,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<ToolOperation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk: Option<PermissionRisk>,
    pub subjects: Vec<ToolSubjectAudit>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subject_zones: Vec<PathTrustZone>,
    pub policy_decision: ApprovalMode,
    pub external_directory_required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmation: Option<PermissionConfirmation>,
    #[serde(default)]
    pub snapshot_required: bool,
    pub user_decision: Option<ToolApprovalUserDecision>,
    pub reason: Option<String>,
    pub preview_hash: Option<String>,
}

/// Stable phase marker for one approval audit entry.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolApprovalAuditAction {
    PolicyEvaluated,
    Requested,
    Resolved,
    PreviewFailed,
}

/// Stable user approval decision persisted in the control log.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolApprovalUserDecision {
    Approved,
    Denied,
}

/// Append-only audit entry for one tool execution lifecycle step.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ToolExecutionEntry {
    pub call_id: String,
    pub tool_name: String,
    pub status: ToolExecutionStatus,
    pub duration_ms: Option<u64>,
    pub subjects: Vec<ToolSubjectAudit>,
    pub changed_files: Vec<String>,
    pub metadata: ToolResultMeta,
    pub error: Option<ToolError>,
    pub model_content_hash: Option<String>,
}

/// Append-only audit entry for one outbound tool call summary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct ToolEgressEntry {
    pub call_id: String,
    pub tool_name: String,
    pub destination: String,
    pub operation: String,
    pub subjects: Vec<ToolSubjectAudit>,
    pub payload: serde_json::Value,
    pub redacted: bool,
}

/// Append-only audit entry for one MCP elicitation decision.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct McpElicitationEntry {
    pub server_name: String,
    pub message_preview: String,
    pub message_hash: String,
    pub requested_schema_hash: String,
    pub requested_field_names: Vec<String>,
    pub required_field_names: Vec<String>,
    pub action: McpElicitationDecision,
    pub content_field_names: Vec<String>,
    pub content_redacted: bool,
}

impl McpElicitationEntry {
    pub fn new(
        server_name: impl Into<String>,
        message: &str,
        requested_schema: &serde_json::Value,
        action: McpElicitationDecision,
        content: Option<&serde_json::Value>,
    ) -> Self {
        Self {
            server_name: server_name.into(),
            message_preview: truncate_stable(message, 160),
            message_hash: stable_text_hash(message),
            requested_schema_hash: stable_json_hash(requested_schema),
            requested_field_names: json_object_keys(requested_schema.get("properties")),
            required_field_names: json_string_array(requested_schema.get("required")),
            action,
            content_field_names: content.map(json_top_level_keys).unwrap_or_default(),
            content_redacted: content.is_some(),
        }
    }
}

/// Stable MCP elicitation user decision persisted in the control log.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum McpElicitationDecision {
    Accepted,
    Declined,
    Cancelled,
}

/// Stable execution status for session audit records.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolExecutionStatus {
    Started,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

/// Durable subject snapshot for one permission or execution audit record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ToolSubjectAudit {
    pub kind: ToolSubjectKind,
    pub original: String,
    pub normalized: String,
    pub canonical_path: Option<String>,
    pub scope: ToolSubjectScope,
}

impl From<&ToolSubject> for ToolSubjectAudit {
    fn from(subject: &ToolSubject) -> Self {
        Self {
            kind: subject.kind,
            original: subject.original.clone(),
            normalized: subject.normalized.clone(),
            canonical_path: subject
                .canonical_path
                .as_ref()
                .map(|path| path.display().to_string()),
            scope: subject.scope,
        }
    }
}

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

    fn load_entries_writer_reconciled(
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

    fn open_locked_file(&self) -> Result<File> {
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("failed to open {}", self.path.display()))?;
        lock_exclusive_with_retry(&file, &self.path)?;
        Ok(file)
    }
}

fn session_entries_from_records(records: &[SessionStreamRecord]) -> Result<Vec<SessionLogEntry>> {
    let mut projection = SessionEntryProjection::default();
    for record in records {
        projection.apply_record(record)?;
    }
    Ok(projection.entries)
}

#[derive(Default)]
struct SessionEntryProjection {
    entries: Vec<SessionLogEntry>,
    cursor: Option<ProjectionCursor>,
}

impl SessionEntryProjection {
    fn apply_record(&mut self, record: &SessionStreamRecord) -> Result<()> {
        let cursor = record.projection_cursor(SESSION_ENTRY_PROJECTION_SCHEMA_VERSION);
        let event = record.domain_event_record()?.map(|record| record.event);
        self.apply_cursor_and_event(cursor, event.as_ref())
    }

    fn apply_cursor_and_event(
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

fn has_session_identity(entries: &[SessionLogEntry]) -> bool {
    entries.iter().any(is_session_identity_entry)
}

fn is_session_identity_entry(entry: &SessionLogEntry) -> bool {
    matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::SessionIdentity { .. })
    )
}

fn read_stream_records_from_file(file: &mut File, path: &Path) -> Result<Vec<SessionStreamRecord>> {
    file.seek(SeekFrom::Start(0))
        .with_context(|| format!("failed to seek {}", path.display()))?;
    let mut content = String::new();
    file.read_to_string(&mut content)
        .with_context(|| format!("failed to read {}", path.display()))?;
    read_stream_records_from_str(path, &content)
}

fn read_stream_records_from_str(path: &Path, content: &str) -> Result<Vec<SessionStreamRecord>> {
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

fn validate_stream_record_identity(
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

fn stream_sequence_mismatch_message(
    physical_line: usize,
    stream_sequence: u64,
    expected_sequence: u64,
) -> String {
    const PREFIX: &str = "stream_sequence does not match expected sequence";
    format!("{PREFIX} on line {physical_line}: {stream_sequence} vs {expected_sequence}")
}

fn stream_session_mismatch_message(
    physical_line: usize,
    session_id: &str,
    expected: &str,
) -> String {
    const PREFIX: &str = "session_id does not match stream session_id";
    format!("{PREFIX} on line {physical_line}: {session_id} vs {expected}")
}

fn stream_line_context(kind: &str, physical_line: usize, path: &Path) -> String {
    let path = path.display();
    format!("failed to parse {kind} on line {physical_line} from {path}")
}

fn line_is_v2_stored_event(line: &str) -> Result<bool> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return Ok(false);
    };
    Ok(is_v2_stored_event_value(&value))
}

fn append_stored_event_to_locked_file(file: &mut File, event: &StoredEvent) -> Result<()> {
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

fn event_id_seed(
    session_id: &str,
    stream_sequence: u64,
    event_type: DurableEventType,
    payload: &serde_json::Value,
) -> String {
    let event_type = event_type.as_str();
    let payload_hash = stable_json_hash(payload);
    format!("{session_id}:{stream_sequence}:{event_type}:{payload_hash}")
}

fn append_event_locked(
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

fn append_session_entry_event_locked(
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

fn stream_session_id(records: &[SessionStreamRecord]) -> Option<String> {
    records.last().map(|record| record.session_id().to_owned())
}

fn session_id_for_path(path: &Path) -> String {
    let path_key = path.as_os_str().to_string_lossy();
    stable_event_uuid("sigil-session-path", &path_key)
}

fn next_stream_sequence(records: &[SessionStreamRecord]) -> u64 {
    records
        .iter()
        .map(SessionStreamRecord::stream_sequence)
        .max()
        .map_or(1, |max_sequence| max_sequence + 1)
}

fn session_entry_event_type(entry: &SessionLogEntry) -> DurableEventType {
    match entry {
        SessionLogEntry::User(_) => DurableEventType::UserMessageRecorded,
        SessionLogEntry::Assistant(_) => DurableEventType::AssistantMessageRecorded,
        SessionLogEntry::ToolResult(_) => DurableEventType::ToolResultRecorded,
        SessionLogEntry::Control(control) => control_entry_event_type(control),
    }
}

fn session_entry_event_class(event_type: DurableEventType) -> EventClass {
    if event_type == DurableEventType::ContextSourceCaptured {
        return EventClass::NonCritical;
    }
    if event_type == DurableEventType::SessionEntryRecorded {
        return EventClass::NonCritical;
    }
    EventClass::Critical
}

fn control_entry_event_type(entry: &ControlEntry) -> DurableEventType {
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
        ControlEntry::AgentProfileTrustDecision(_) => DurableEventType::ExtensionTrustDecision,
        ControlEntry::TaskRun(_) => DurableEventType::TaskStatusChanged,
        ControlEntry::TaskPlan(_) => DurableEventType::TaskStatusChanged,
        ControlEntry::TaskStep(_) => DurableEventType::TaskStatusChanged,
        ControlEntry::CheckSpecRecorded(_) => DurableEventType::CheckSpecRecorded,
        ControlEntry::VerificationPolicyChanged(_) => DurableEventType::VerificationPolicyChanged,
        ControlEntry::VerificationRecorded(_) => DurableEventType::VerificationRecorded,
        ControlEntry::ReadinessEvaluated(_) => DurableEventType::ReadinessEvaluated,
        ControlEntry::ChildVerificationReceiptLinked(_) => {
            DurableEventType::ChildVerificationReceiptLinked
        }
        ControlEntry::WorkspaceTrustDecision(_) => DurableEventType::WorkspaceTrustDecision,
        ControlEntry::PrefixSnapshotCaptured(_) => DurableEventType::ContextSourceCaptured,
        ControlEntry::MemorySnapshotCaptured(_) => DurableEventType::ContextSourceCaptured,
        ControlEntry::SkillIndexCaptured(_) => DurableEventType::ContextSourceCaptured,
        ControlEntry::SkillLoaded(_) => DurableEventType::ContextSourceCaptured,
        ControlEntry::PluginManifestCaptured(_) => DurableEventType::ContextSourceCaptured,
        ControlEntry::AgentProfileCaptured(_) => DurableEventType::ContextSourceCaptured,
        _ => DurableEventType::SessionEntryRecorded,
    }
}

fn tool_execution_event_type(status: ToolExecutionStatus) -> DurableEventType {
    if status == ToolExecutionStatus::Started {
        DurableEventType::ToolExecutionStarted
    } else {
        DurableEventType::ToolExecutionFinished
    }
}

fn session_entry_from_stored_event(event: &StoredEvent) -> Result<Option<SessionLogEntry>> {
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

fn session_entry_from_domain_event(event: &DomainEvent) -> Result<Option<SessionLogEntry>> {
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

fn lock_shared_with_retry(file: &File, path: &Path) -> Result<()> {
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

fn lock_exclusive_with_retry(file: &File, path: &Path) -> Result<()> {
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
struct TailRecoveryIntent {
    original_size: u64,
    recovered_size: u64,
    discarded_bytes: u64,
    quarantine_path: PathBuf,
    original_hash: String,
    event_id: String,
    session_id: String,
}

fn recover_tail_if_needed_locked(file: &mut File, path: &Path) -> Result<Vec<SessionStreamRecord>> {
    if let Some(intent) = read_tail_recovery_intent(path)? {
        match read_stream_records_from_file(file, path) {
            Ok(records) => {
                if records.iter().any(|record| {
                    matches!(
                        record,
                        SessionStreamRecord::Stored(event)
                            if event.event_type == DurableEventType::LogTailRecovered.as_str()
                                && event.event_id == intent.event_id
                    )
                }) {
                    clear_tail_recovery_intent(path)?;
                    return Ok(records);
                }
            }
            Err(read_error) => {
                recover_from_pending_tail_intent(file, path, &intent)
                    .with_context(|| read_error.to_string())?;
            }
        }
        append_tail_recovery_event_locked(file, path, &intent)?;
        clear_tail_recovery_intent(path)?;
        return read_stream_records_from_file(file, path);
    }

    file.seek(SeekFrom::Start(0))
        .with_context(|| format!("failed to seek {}", path.display()))?;
    let mut content = String::new();
    file.read_to_string(&mut content)
        .with_context(|| format!("failed to read {}", path.display()))?;

    let Some(corruption) = tail_corruption(path, &content)? else {
        return read_stream_records_from_str(path, &content);
    };

    let original_hash = stable_event_hash(content.as_bytes());
    let recovered_content = &content[..corruption.recovered_size as usize];
    let recovered_records = read_stream_records_from_str(path, recovered_content)?;
    let session_id = stream_session_id(&recovered_records).unwrap_or_else(|| {
        stable_event_uuid("sigil-session-path", &path.as_os_str().to_string_lossy())
    });
    let event_id = stable_event_uuid(
        "sigil-tail-recovery",
        &format!(
            "{original_hash}:{}:{}",
            corruption.recovered_size, corruption.discarded_bytes
        ),
    );
    let quarantine_path = quarantine_tail_copy(path, &content, &original_hash)?;
    let intent = TailRecoveryIntent {
        original_size: content.len() as u64,
        recovered_size: corruption.recovered_size,
        discarded_bytes: corruption.discarded_bytes,
        quarantine_path,
        original_hash,
        event_id,
        session_id,
    };
    write_tail_recovery_intent(path, &intent)?;
    file.set_len(intent.recovered_size)
        .with_context(|| format!("failed to truncate {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("failed to sync truncated {}", path.display()))?;
    append_tail_recovery_event_locked(file, path, &intent)?;
    clear_tail_recovery_intent(path)?;
    read_stream_records_from_file(file, path)
}

fn recover_from_pending_tail_intent(
    file: &mut File,
    path: &Path,
    intent: &TailRecoveryIntent,
) -> Result<()> {
    file.seek(SeekFrom::Start(0))
        .with_context(|| format!("failed to seek {}", path.display()))?;
    let mut content = String::new();
    file.read_to_string(&mut content)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let current_hash = stable_event_hash(content.as_bytes());
    if current_hash != intent.original_hash {
        bail!(
            "tail recovery intent exists but current log hash does not match recorded original hash"
        );
    }
    if content.len() < intent.recovered_size as usize {
        bail!("tail recovery intent recovered_size is past current log length");
    }
    read_stream_records_from_str(path, &content[..intent.recovered_size as usize])
        .context("tail recovery intent points to invalid recovered prefix")?;
    file.set_len(intent.recovered_size)
        .with_context(|| format!("failed to truncate {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("failed to sync truncated {}", path.display()))
}

#[derive(Debug, Clone, Copy)]
struct TailCorruption {
    recovered_size: u64,
    discarded_bytes: u64,
}

fn tail_corruption(path: &Path, content: &str) -> Result<Option<TailCorruption>> {
    let mut line_start = 0usize;
    let mut physical_line = 1usize;
    let mut non_empty_lines = Vec::new();
    for segment in content.split_inclusive('\n') {
        let line_end = line_start + segment.len();
        let line = segment.trim_end_matches(['\n', '\r']);
        if !line.trim().is_empty() {
            non_empty_lines.push((physical_line, line_start, line_end, line.to_owned()));
        }
        line_start = line_end;
        physical_line += 1;
    }
    for (index, (physical_line, start, _end, line)) in non_empty_lines.iter().enumerate() {
        if record_line_is_valid_or_fail_closed(*physical_line, line, path)? {
            continue;
        }
        if index + 1 == non_empty_lines.len() {
            return Ok(Some(TailCorruption {
                recovered_size: *start as u64,
                discarded_bytes: (content.len() - *start) as u64,
            }));
        }
        bail!("middle corruption in session log {}", path.display());
    }
    Ok(None)
}

fn record_line_is_valid_or_fail_closed(
    physical_line: usize,
    line: &str,
    path: &Path,
) -> Result<bool> {
    if line_is_v2_stored_event(line)? {
        StoredEvent::from_json_str(line)
            .with_context(|| stream_line_context("stored event", physical_line, path))?;
        return Ok(true);
    }
    Ok(serde_json::from_str::<SessionLogEntry>(line).is_ok())
}

fn append_tail_recovery_event_locked(
    file: &mut File,
    _path: &Path,
    intent: &TailRecoveryIntent,
) -> Result<()> {
    let records = read_stream_records_from_file(file, _path)?;
    let next_sequence = records
        .iter()
        .map(SessionStreamRecord::stream_sequence)
        .max()
        .unwrap_or(0)
        + 1;
    let event = StoredEvent::new(
        DurableEventType::LogTailRecovered,
        EventClass::Critical,
        intent.event_id.clone(),
        intent.session_id.clone(),
        next_sequence,
        serde_json::json!({
            "original_size": intent.original_size,
            "recovered_size": intent.recovered_size,
            "discarded_bytes": intent.discarded_bytes,
            "quarantine_path": intent.quarantine_path,
            "original_hash": intent.original_hash,
        }),
    )?;
    append_stored_event_to_locked_file(file, &event)
}

fn quarantine_tail_copy(path: &Path, content: &str, original_hash: &str) -> Result<PathBuf> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let dir = parent.join(".sigil-recovery");
    fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let short_hash = original_hash
        .trim_start_matches("sha256:")
        .chars()
        .take(16)
        .collect::<String>();
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("session.jsonl");
    let quarantine_path = dir.join(format!("{file_name}.corrupt.{short_hash}"));
    fs::write(&quarantine_path, content)
        .with_context(|| format!("failed to write {}", quarantine_path.display()))?;
    let quarantine_file = File::open(&quarantine_path)
        .with_context(|| format!("failed to open {}", quarantine_path.display()))?;
    quarantine_file
        .sync_all()
        .with_context(|| format!("failed to sync {}", quarantine_path.display()))?;
    Ok(quarantine_path)
}

fn tail_recovery_intent_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("session.jsonl");
    path.with_file_name(format!("{file_name}.tail-recovery-intent"))
}

fn read_tail_recovery_intent(path: &Path) -> Result<Option<TailRecoveryIntent>> {
    let intent_path = tail_recovery_intent_path(path);
    if !intent_path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(&intent_path)
        .with_context(|| format!("failed to read {}", intent_path.display()))?;
    let intent = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse {}", intent_path.display()))?;
    Ok(Some(intent))
}

fn write_tail_recovery_intent(path: &Path, intent: &TailRecoveryIntent) -> Result<()> {
    let intent_path = tail_recovery_intent_path(path);
    let content = serde_json::to_vec(intent).context("failed to serialize tail recovery intent")?;
    fs::write(&intent_path, content)
        .with_context(|| format!("failed to write {}", intent_path.display()))?;
    let file = File::open(&intent_path)
        .with_context(|| format!("failed to open {}", intent_path.display()))?;
    file.sync_all()
        .with_context(|| format!("failed to sync {}", intent_path.display()))
}

fn clear_tail_recovery_intent(path: &Path) -> Result<()> {
    let intent_path = tail_recovery_intent_path(path);
    if intent_path.exists() {
        fs::remove_file(&intent_path)
            .with_context(|| format!("failed to remove {}", intent_path.display()))?;
    }
    Ok(())
}

/// In-memory session state backed by an optional append-only JSONL store.
#[derive(Debug)]
pub struct Session {
    provider_name: String,
    model_name: String,
    entries: Vec<SessionLogEntry>,
    store: Option<JsonlSessionStore>,
    stats: SessionStats,
}

impl Session {
    /// Creates a new in-memory session with the given provider and model identity.
    pub fn new(provider_name: impl Into<String>, model_name: impl Into<String>) -> Self {
        Self {
            provider_name: provider_name.into(),
            model_name: model_name.into(),
            entries: Vec::new(),
            store: None,
            stats: SessionStats::default(),
        }
    }

    /// Attaches a durable JSONL store to the session.
    pub fn with_store(mut self, store: JsonlSessionStore) -> Self {
        self.store = Some(store);
        self
    }

    /// Rehydrates a session from a preloaded list of entries.
    pub fn from_entries(
        provider_name: impl Into<String>,
        model_name: impl Into<String>,
        entries: Vec<SessionLogEntry>,
    ) -> Self {
        let stats = session_stats_from_entries(&entries);
        Self {
            provider_name: provider_name.into(),
            model_name: model_name.into(),
            entries,
            store: None,
            stats,
        }
    }

    /// Loads a session from the durable store and recovers its persisted identity when possible.
    pub fn load_from_store(
        provider_name: impl Into<String>,
        model_name: impl Into<String>,
        store: JsonlSessionStore,
    ) -> Result<Self> {
        let fallback_provider_name = provider_name.into();
        let fallback_model_name = model_name.into();
        let (entries, provider_name, model_name) =
            store.load_entries_writer_reconciled(fallback_provider_name, fallback_model_name)?;
        Ok(Self::from_entries(provider_name, model_name, entries).with_store(store))
    }

    /// Appends a single entry to the in-memory log and durable store when present.
    pub fn append(&mut self, entry: SessionLogEntry) -> Result<()> {
        if let Some(store) = &self.store {
            store.append(&entry)?;
        }
        self.entries.push(entry);
        Ok(())
    }

    pub fn append_user_message(&mut self, message: ModelMessage) -> Result<()> {
        self.append(SessionLogEntry::User(message))
    }

    pub fn append_assistant_message(&mut self, message: ModelMessage) -> Result<()> {
        self.append(SessionLogEntry::Assistant(message))
    }

    pub fn append_tool_message(&mut self, message: ModelMessage) -> Result<()> {
        self.append(SessionLogEntry::ToolResult(message))
    }

    pub fn append_control(&mut self, control: ControlEntry) -> Result<()> {
        self.append(SessionLogEntry::Control(control))
    }

    /// Appends a durable domain event that does not project into provider-visible chat history.
    ///
    /// In-memory sessions without a backing store cannot persist durable-only events, so they return
    /// `Ok(None)` instead of fabricating an in-memory fact that would disappear on resume.
    pub fn append_durable_event(
        &mut self,
        event_type: DurableEventType,
        event_class: EventClass,
        payload: serde_json::Value,
    ) -> Result<Option<StoredEvent>> {
        self.store
            .as_ref()
            .map(|store| store.append_event(event_type, event_class, payload))
            .transpose()
    }

    /// Returns a store-backed mutation recorder for tool contexts when this session is durable.
    pub fn mutation_event_recorder(&self) -> Option<MutationEventRecorder> {
        self.store.as_ref().cloned().map(MutationEventRecorder::new)
    }

    /// Reconciles prepared controlled mutations that were left without terminal commit events.
    ///
    /// This requires a workspace root and is therefore run by the agent before a new turn, rather
    /// than during store-only session loading.
    pub fn reconcile_prepared_mutations(
        &mut self,
        workspace_root: impl AsRef<Path>,
    ) -> Result<Vec<StoredEvent>> {
        let Some(recorder) = self.mutation_event_recorder() else {
            return Ok(Vec::new());
        };
        recorder.reconcile_prepared_mutations(workspace_root)
    }

    /// Reconciles interrupted write-capable tool executions with persisted mutation profiles.
    ///
    /// `Session::load_from_store` can mark unfinished tool executions as interrupted without a
    /// workspace root. This method runs at the next agent turn, when the workspace root is known,
    /// and records workspace mutation evidence without replaying the tool.
    pub fn reconcile_unfinished_write_tool_executions(
        &mut self,
        workspace_root: impl AsRef<Path>,
    ) -> Result<Vec<StoredEvent>> {
        let Some(recorder) = self.mutation_event_recorder() else {
            return Ok(Vec::new());
        };
        let workspace_root = workspace_root.as_ref();
        let mut events = Vec::new();
        for execution in interrupted_tool_execution_profiles(&self.entries) {
            if let Some(event) =
                recorder.reconcile_execution_mutation_profile(workspace_root, &execution)?
            {
                events.push(event);
            }
        }
        Ok(events)
    }

    pub fn entries(&self) -> &[SessionLogEntry] {
        &self.entries
    }

    pub fn provider_name(&self) -> &str {
        &self.provider_name
    }

    pub fn model_name(&self) -> &str {
        &self.model_name
    }

    /// Returns the provider-visible message projection, including the latest compaction summary.
    pub fn messages(&self) -> Vec<ModelMessage> {
        self.projected_messages()
    }

    pub fn continuation_states(&self, provider_name: &str) -> Vec<ProviderContinuationState> {
        let mut latest_by_key: HashMap<(String, Option<String>), ProviderContinuationState> =
            HashMap::new();
        for entry in &self.entries {
            if let SessionLogEntry::Control(ControlEntry::ContinuationStateSaved(state)) = entry
                && state.provider_name == provider_name
            {
                latest_by_key.insert(
                    (state.state_kind.clone(), state.message_id.clone()),
                    state.clone(),
                );
            }
        }
        latest_by_key.into_values().collect()
    }

    pub fn latest_response_handle(&self, provider_name: &str) -> Option<ResponseHandle> {
        self.entries.iter().rev().find_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::ResponseHandleTracked(handle))
                if handle.provider_name == provider_name =>
            {
                Some(handle.clone())
            }
            _ => None,
        })
    }

    pub fn latest_prefix_snapshot(&self) -> Option<PrefixSnapshot> {
        self.entries.iter().rev().find_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::PrefixSnapshotCaptured(snapshot)) => {
                Some(snapshot.clone())
            }
            _ => None,
        })
    }

    pub fn latest_memory_snapshot(&self) -> Option<MemorySnapshot> {
        self.entries.iter().rev().find_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::MemorySnapshotCaptured(snapshot)) => {
                Some(snapshot.clone())
            }
            _ => None,
        })
    }

    pub fn latest_compaction_record(&self) -> Option<CompactionRecord> {
        latest_compaction_record(&self.entries)
    }

    /// Returns durable plan approvals reconstructed from append-only control entries.
    pub fn plan_approval_projection(&self) -> PlanApprovalProjection {
        PlanApprovalProjection::from_entries(&self.entries)
    }

    /// Returns a durable task projection reconstructed from append-only control entries.
    pub fn task_state_projection(&self) -> TaskStateProjection {
        TaskStateProjection::from_entries(&self.entries)
    }

    /// Returns a durable agent thread projection reconstructed from append-only control entries.
    pub fn agent_thread_state_projection(&self) -> AgentThreadStateProjection {
        AgentThreadStateProjection::from_entries(&self.entries)
    }

    /// Returns durable agent profile trust decisions reconstructed from append-only control entries.
    pub fn agent_profile_trust_projection(&self) -> AgentProfileTrustProjection {
        AgentProfileTrustProjection::from_entries(&self.entries)
    }

    /// Returns durable agent profile policy decisions reconstructed from append-only control entries.
    pub fn agent_profile_policy_projection(&self) -> AgentProfilePolicyProjection {
        AgentProfilePolicyProjection::from_entries(&self.entries)
    }

    /// Returns a durable skill projection reconstructed from append-only control entries.
    pub fn skill_state_projection(&self) -> SkillStateProjection {
        SkillStateProjection::from_entries(&self.entries)
    }

    /// Returns a durable plugin projection reconstructed from append-only control entries.
    pub fn plugin_state_projection(&self) -> PluginStateProjection {
        PluginStateProjection::from_entries(&self.entries)
    }

    /// Returns a durable change set projection reconstructed from append-only control entries.
    pub fn changeset_projection(&self) -> ChangeSetProjection {
        ChangeSetProjection::from_entries(&self.entries)
    }

    /// Returns durable verification evidence reconstructed from append-only control entries.
    pub fn verification_state_projection(&self) -> VerificationStateProjection {
        VerificationStateProjection::from_entries(&self.entries)
    }

    /// Rebuilds verification state directly from the durable mixed-format event stream.
    ///
    /// This is the RFC-0001 replay path for verification projection. It preserves the existing
    /// infallible `verification_state_projection` API while giving callers and tests a fail-closed
    /// durable replay option.
    pub fn try_verification_state_projection_from_durable(
        &self,
    ) -> Result<Option<VerificationStateProjection>> {
        let Some(store) = &self.store else {
            return Ok(None);
        };
        let records = JsonlSessionStore::read_event_records(store.path())?;
        let mut projection = VerificationStateProjection::default();
        let mut cursor: Option<ProjectionCursor> = None;
        for record in records {
            apply_verification_projection_record(&mut projection, &mut cursor, &record)?;
        }
        Ok(Some(projection))
    }

    /// Returns a durable terminal task projection reconstructed from append-only control entries.
    pub fn terminal_task_projection(&self) -> TerminalTaskProjection {
        TerminalTaskProjection::from_entries(&self.entries)
    }

    pub fn conversation_queue_projection(&self) -> ConversationQueueProjection {
        ConversationQueueProjection::from_entries(&self.entries)
    }

    pub fn agent_result_continuation_projection(&self) -> AgentResultContinuationProjection {
        AgentResultContinuationProjection::from_entries(&self.entries)
    }

    /// Builds one provider request from stable system memory, projected session history, and tools.
    ///
    /// # Errors
    ///
    /// Returns an error when memory loading, prefix materialization, or durable control writes fail.
    pub fn build_request(
        &mut self,
        workspace_root: &Path,
        memory_config: &MemoryConfig,
        tools: Vec<ToolSpec>,
        reasoning_effort: Option<crate::provider::ReasoningEffort>,
        previous_response_handle: Option<crate::provider::ResponseHandle>,
        traffic_partition_key: Option<String>,
    ) -> Result<CompletionRequest> {
        self.build_request_with_transient_messages(
            workspace_root,
            memory_config,
            tools,
            reasoning_effort,
            previous_response_handle,
            traffic_partition_key,
            &[],
        )
    }

    /// Builds one provider request with extra transient messages that are not appended as
    /// provider-visible session history.
    ///
    /// # Errors
    ///
    /// Returns an error when memory loading, prefix materialization, or durable control writes fail.
    #[allow(clippy::too_many_arguments)]
    pub fn build_request_with_transient_messages(
        &mut self,
        workspace_root: &Path,
        memory_config: &MemoryConfig,
        tools: Vec<ToolSpec>,
        reasoning_effort: Option<crate::provider::ReasoningEffort>,
        previous_response_handle: Option<crate::provider::ResponseHandle>,
        traffic_partition_key: Option<String>,
        transient_messages: &[ModelMessage],
    ) -> Result<CompletionRequest> {
        let memory = self.memory_snapshot_for_request(workspace_root, memory_config)?;
        let projected_messages = self.projected_messages();
        let mut request_messages = memory.messages.clone();
        request_messages.extend(projected_messages);
        request_messages.extend(transient_messages.iter().cloned());

        let materialized_messages =
            serde_json::to_string(&request_messages).context("failed to serialize messages")?;
        let materialized_tools =
            serde_json::to_string(&tools).context("failed to serialize tool specs")?;
        let prefix_materialized = format!("{materialized_messages}\n{materialized_tools}");
        let digest = Sha256::digest(prefix_materialized.as_bytes());
        let mut snapshot = PrefixSnapshot {
            materialized_text: prefix_materialized,
            sha256: format!("{digest:x}"),
            provider_name: self.provider_name.clone(),
            model_name: self.model_name.clone(),
            memory_fingerprint: "none".to_owned(),
            tool_schema_fingerprint: format!("{:x}", Sha256::digest(materialized_tools.as_bytes())),
            skill_index_fingerprint: "none".to_owned(),
        };
        apply_memory_report(&mut snapshot, &memory.report);
        self.append_control(ControlEntry::PrefixSnapshotCaptured(snapshot))?;
        Ok(CompletionRequest {
            provider_name: self.provider_name.clone(),
            model_name: self.model_name.clone(),
            messages: request_messages,
            tools,
            temperature: None,
            max_tokens: None,
            reasoning_effort,
            previous_response_handle,
            continuation_states: self.continuation_states(&self.provider_name),
            traffic_partition_key,
            background: false,
            store: false,
            deterministic_materialization: true,
        })
    }

    fn memory_snapshot_for_request(
        &mut self,
        workspace_root: &Path,
        memory_config: &MemoryConfig,
    ) -> Result<MemorySnapshot> {
        let memory = materialize_memory(workspace_root, memory_config)?;
        if let Some(snapshot) = self.latest_memory_snapshot()
            && snapshot.report.fingerprint == memory.report.fingerprint
        {
            return Ok(snapshot);
        }

        let snapshot = MemorySnapshot {
            messages: memory.messages,
            report: memory.report,
        };
        self.append_control(ControlEntry::MemorySnapshotCaptured(snapshot.clone()))?;
        Ok(snapshot)
    }

    /// Applies one stable compaction record and persists it in the append-only control log.
    ///
    /// # Errors
    ///
    /// Returns an error when compaction is disabled or the session does not yet have enough
    /// history to fold safely.
    pub fn compact_now(&mut self, config: &CompactionConfig) -> Result<CompactionRecord> {
        if !config.enabled {
            bail!("compaction is disabled");
        }

        let raw_messages = self.raw_messages();
        if raw_messages.len() < 2 {
            bail!("session does not have enough history to compact");
        }

        let compacted_message_count = compaction_boundary(&raw_messages, config.tail_messages);
        if compacted_message_count == 0 {
            bail!("session does not have enough stable history to compact");
        }

        let summary = summarize_messages(&raw_messages[..compacted_message_count]);
        let record = CompactionRecord {
            summary,
            compacted_message_count,
            retained_tail_message_count: raw_messages.len().saturating_sub(compacted_message_count),
        };
        self.append_control(ControlEntry::CompactionApplied(record.clone()))?;
        self.stats.last_prompt_tokens = 0;
        Ok(record)
    }

    /// Returns whether the current session has enough stable history to compact safely.
    pub fn can_compact(&self, config: &CompactionConfig) -> bool {
        if !config.enabled {
            return false;
        }

        let raw_messages = self.raw_messages();
        raw_messages.len() >= 2 && compaction_boundary(&raw_messages, config.tail_messages) > 0
    }

    /// Computes a deterministic manual compaction preview without mutating durable state.
    ///
    /// # Errors
    ///
    /// Returns an error when compaction is disabled. Returns `Ok(None)` when the current session
    /// does not yet have enough stable history to fold safely.
    pub fn compaction_preview(
        &self,
        config: &CompactionConfig,
    ) -> Result<Option<CompactionPreview>> {
        if !config.enabled {
            bail!("compaction is disabled");
        }

        let raw_messages = self.raw_messages();
        if raw_messages.len() < 2 {
            return Ok(None);
        }

        let compacted_message_count = compaction_boundary(&raw_messages, config.tail_messages);
        if compacted_message_count == 0 {
            return Ok(None);
        }

        let record = CompactionRecord {
            summary: summarize_messages(&raw_messages[..compacted_message_count]),
            compacted_message_count,
            retained_tail_message_count: raw_messages.len().saturating_sub(compacted_message_count),
        };
        Ok(Some(CompactionPreview {
            folded_messages: raw_messages[..compacted_message_count].to_vec(),
            projected_messages: projected_messages_with_record(&raw_messages, &record),
            record,
        }))
    }

    pub fn store_path(&self) -> Option<&Path> {
        self.store.as_ref().map(JsonlSessionStore::path)
    }

    pub fn stats(&self) -> &SessionStats {
        &self.stats
    }

    pub fn stats_mut(&mut self) -> &mut SessionStats {
        &mut self.stats
    }

    pub fn ensure_identity_entry(&mut self) -> Result<()> {
        if self.entries.iter().any(|entry| {
            matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::SessionIdentity { .. })
            )
        }) {
            return Ok(());
        }

        self.append_control(ControlEntry::SessionIdentity {
            provider_name: self.provider_name.clone(),
            model_name: self.model_name.clone(),
        })
    }

    fn raw_messages(&self) -> Vec<ModelMessage> {
        self.entries
            .iter()
            .filter_map(|entry| match entry {
                SessionLogEntry::User(message)
                | SessionLogEntry::Assistant(message)
                | SessionLogEntry::ToolResult(message) => Some(message.clone()),
                SessionLogEntry::Control(_) => None,
            })
            .collect()
    }

    fn projected_messages(&self) -> Vec<ModelMessage> {
        let raw_messages = self.raw_messages();
        let Some(record) = latest_compaction_record(&self.entries) else {
            return repair_orphan_tool_results(&raw_messages);
        };
        if record.compacted_message_count == 0 || record.summary.trim().is_empty() {
            return repair_orphan_tool_results(&raw_messages);
        }
        repair_orphan_tool_results(&projected_messages_with_record(&raw_messages, &record))
    }
}

fn apply_verification_projection_record(
    projection: &mut VerificationStateProjection,
    cursor: &mut Option<ProjectionCursor>,
    record: &SessionStreamRecord,
) -> Result<()> {
    let next_cursor = record.projection_cursor(VERIFICATION_STATE_PROJECTION_SCHEMA_VERSION);
    match projection_apply_decision_for_record(
        cursor.as_ref(),
        &next_cursor.session_id,
        next_cursor.last_applied_stream_sequence,
        &next_cursor.last_applied_event_id,
        &next_cursor.last_applied_record_checksum,
    )? {
        ProjectionApplyDecision::IgnoreAlreadyApplied => return Ok(()),
        ProjectionApplyDecision::Apply => {}
    }
    if let Some(domain_record) = record.domain_event_record()?
        && let Some(SessionLogEntry::Control(control)) =
            session_entry_from_domain_event(&domain_record.event)?
    {
        projection.apply_control_entry(&control);
    }
    *cursor = Some(next_cursor);
    Ok(())
}

fn compaction_summary_message(record: &CompactionRecord) -> ModelMessage {
    let digest = Sha256::digest(
        format!(
            "{}\n{}\n{}",
            record.summary, record.compacted_message_count, record.retained_tail_message_count
        )
        .as_bytes(),
    );
    ModelMessage {
        id: format!("compaction:{digest:x}"),
        role: crate::MessageRole::Assistant,
        content: Some(record.summary.clone()),
        tool_calls: Vec::new(),
        tool_call_id: None,
    }
}

fn projected_messages_with_record(
    raw_messages: &[ModelMessage],
    record: &CompactionRecord,
) -> Vec<ModelMessage> {
    let mut projected = vec![compaction_summary_message(record)];
    if record.compacted_message_count < raw_messages.len() {
        projected.extend(
            raw_messages[record.compacted_message_count..]
                .iter()
                .cloned(),
        );
    }
    projected
}

fn repair_orphan_tool_results(messages: &[ModelMessage]) -> Vec<ModelMessage> {
    let mut repaired = Vec::with_capacity(messages.len());
    let mut index = 0usize;

    while index < messages.len() {
        let message = &messages[index];
        repaired.push(message.clone());

        if !matches!(message.role, crate::MessageRole::Assistant) || message.tool_calls.is_empty() {
            index += 1;
            continue;
        }

        index += 1;
        let mut satisfied_call_ids = Vec::new();
        while index < messages.len() && matches!(messages[index].role, crate::MessageRole::Tool) {
            if let Some(tool_call_id) = &messages[index].tool_call_id
                && message
                    .tool_calls
                    .iter()
                    .any(|call| call.id == *tool_call_id)
            {
                satisfied_call_ids.push(tool_call_id.clone());
            }
            repaired.push(messages[index].clone());
            index += 1;
        }

        for call in &message.tool_calls {
            if !satisfied_call_ids.iter().any(|call_id| call_id == &call.id) {
                repaired.push(synthetic_orphan_tool_result(call));
            }
        }
    }

    repaired
}

fn synthetic_orphan_tool_result(call: &crate::ToolCall) -> ModelMessage {
    let result = ToolResult::error(
        call.id.clone(),
        call.name.clone(),
        ToolErrorKind::Interrupted,
        format!(
            "tool call {} did not return a result before the previous run stopped; retry the tool call with valid arguments if it is still needed",
            call.name
        ),
    );
    let mut message = result.to_model_message();
    message.id = format!("local_repair:missing_tool_result:{}", call.id);
    message
}

fn interrupted_tool_executions(entries: &[SessionLogEntry]) -> Vec<ToolExecutionEntry> {
    let mut open_executions = HashMap::<String, ToolExecutionEntry>::new();
    for entry in entries {
        let SessionLogEntry::Control(ControlEntry::ToolExecution(execution)) = entry else {
            continue;
        };
        match execution.status {
            ToolExecutionStatus::Started => {
                open_executions.insert(execution.call_id.clone(), execution.as_ref().clone());
            }
            ToolExecutionStatus::Completed
            | ToolExecutionStatus::Failed
            | ToolExecutionStatus::Cancelled
            | ToolExecutionStatus::Interrupted => {
                open_executions.remove(&execution.call_id);
            }
        }
    }

    open_executions
        .into_values()
        .map(|mut execution| {
            execution.status = ToolExecutionStatus::Interrupted;
            execution.duration_ms = None;
            execution.changed_files = Vec::new();
            execution.metadata.changed_files = Vec::new();
            execution.error = Some(ToolError {
                kind: ToolErrorKind::Interrupted,
                message: "tool execution was interrupted before a completion record was written"
                    .to_owned(),
                retryable: true,
                details: serde_json::Value::Null,
            });
            execution.model_content_hash = None;
            execution
        })
        .collect()
}

fn interrupted_tool_execution_profiles(
    entries: &[SessionLogEntry],
) -> Vec<ExecutionMutationProfile> {
    entries
        .iter()
        .filter_map(|entry| {
            let SessionLogEntry::Control(ControlEntry::ToolExecution(execution)) = entry else {
                return None;
            };
            if execution.status != ToolExecutionStatus::Interrupted {
                return None;
            }
            execution
                .metadata
                .details
                .get("execution_mutation_profile")
                .cloned()
                .and_then(|value| serde_json::from_value(value).ok())
        })
        .collect()
}

pub fn latest_compaction_record(entries: &[SessionLogEntry]) -> Option<CompactionRecord> {
    entries.iter().rev().find_map(|entry| match entry {
        SessionLogEntry::Control(ControlEntry::CompactionApplied(record)) => Some(record.clone()),
        _ => None,
    })
}

pub fn session_stats_from_entries(entries: &[SessionLogEntry]) -> SessionStats {
    let mut stats = SessionStats::default();
    for entry in entries {
        match entry {
            SessionLogEntry::Control(ControlEntry::UsageSnapshot(usage)) => {
                stats.apply_usage(usage)
            }
            SessionLogEntry::Control(ControlEntry::CompactionApplied(_)) => {
                stats.last_prompt_tokens = 0;
            }
            SessionLogEntry::User(_)
            | SessionLogEntry::Assistant(_)
            | SessionLogEntry::ToolResult(_)
            | SessionLogEntry::Control(_) => {}
        }
    }
    stats
}

fn compaction_boundary(messages: &[ModelMessage], requested_tail_messages: usize) -> usize {
    if messages.is_empty() {
        return 0;
    }

    let tail_messages = requested_tail_messages.max(1);
    let mut boundary = messages.len().saturating_sub(tail_messages);
    while boundary > 0
        && (matches!(messages[boundary].role, crate::MessageRole::Tool)
            || !messages[boundary - 1].tool_calls.is_empty()
            || matches!(messages[boundary - 1].role, crate::MessageRole::Tool))
    {
        if !messages[boundary - 1].tool_calls.is_empty() {
            boundary -= 1;
            break;
        }
        boundary -= 1;
    }
    boundary
}

fn summarize_messages(messages: &[ModelMessage]) -> String {
    let mut lines = vec![format!(
        "Compacted {} earlier messages into a stable local summary.",
        messages.len()
    )];

    for (index, message) in messages.iter().enumerate() {
        let label = match message.role {
            crate::MessageRole::System => "system",
            crate::MessageRole::User => "user",
            crate::MessageRole::Assistant => "assistant",
            crate::MessageRole::Tool => "tool",
        };
        if !message.tool_calls.is_empty() {
            let names = message
                .tool_calls
                .iter()
                .map(|call| call.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let content = message.content.as_deref().unwrap_or_default();
            let truncated = truncate_stable(content, 160);
            if !truncated.is_empty() {
                lines.push(format!(
                    "{:02}. {} {} tool_calls [{}]",
                    index + 1,
                    label,
                    truncated,
                    names
                ));
                continue;
            }
            lines.push(format!(
                "{:02}. {} tool_calls [{}]",
                index + 1,
                label,
                names
            ));
            continue;
        }

        let content = message.content.clone().unwrap_or_default();
        let truncated = truncate_stable(&content, 160);
        if matches!(message.role, crate::MessageRole::Tool) {
            let tool_call_id = message.tool_call_id.as_deref().unwrap_or("unknown");
            lines.push(format!(
                "{:02}. {} {} => {}",
                index + 1,
                label,
                tool_call_id,
                truncated
            ));
        } else {
            lines.push(format!("{:02}. {} {}", index + 1, label, truncated));
        }
    }

    lines.join("\n")
}

fn truncate_stable(content: &str, max_chars: usize) -> String {
    let normalized = content.split_whitespace().collect::<Vec<_>>().join(" ");
    let char_count = normalized.chars().count();
    if char_count <= max_chars {
        return normalized;
    }
    let truncated = normalized.chars().take(max_chars).collect::<String>();
    format!("{truncated}...")
}

fn stable_json_hash(value: &serde_json::Value) -> String {
    let serialized =
        serde_json::to_string(value).unwrap_or_else(|_| "<unserializable-json>".to_owned());
    stable_text_hash(&serialized)
}

fn stable_text_hash(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    format!("{digest:x}")
}

fn json_object_keys(value: Option<&serde_json::Value>) -> Vec<String> {
    let Some(object) = value.and_then(serde_json::Value::as_object) else {
        return Vec::new();
    };
    let mut keys = object.keys().cloned().collect::<Vec<_>>();
    keys.sort();
    keys
}

fn json_string_array(value: Option<&serde_json::Value>) -> Vec<String> {
    let Some(values) = value.and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    let mut strings = values
        .iter()
        .filter_map(|value| value.as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    strings.sort();
    strings
}

fn json_top_level_keys(value: &serde_json::Value) -> Vec<String> {
    let Some(object) = value.as_object() else {
        return Vec::new();
    };
    let mut keys = object.keys().cloned().collect::<Vec<_>>();
    keys.sort();
    keys
}

fn session_identity_from_entries(entries: &[SessionLogEntry]) -> Option<(String, String)> {
    entries.iter().find_map(|entry| match entry {
        SessionLogEntry::Control(ControlEntry::SessionIdentity {
            provider_name,
            model_name,
        }) => Some((provider_name.clone(), model_name.clone())),
        SessionLogEntry::Control(ControlEntry::PrefixSnapshotCaptured(snapshot)) => {
            Some((snapshot.provider_name.clone(), snapshot.model_name.clone()))
        }
        _ => None,
    })
}

#[cfg(test)]
#[path = "tests/session_tests.rs"]
mod tests;
