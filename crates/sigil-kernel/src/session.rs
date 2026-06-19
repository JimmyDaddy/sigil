use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    CompactionConfig, MemoryConfig, MemoryLoadReport,
    changeset::{ChangeSet, ChangeSetProjection, ChangeSetResult},
    memory::{apply_memory_report, materialize_memory},
    permission::ApprovalMode,
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
};

/// Append-only session log entry stored in the durable JSONL session file.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    pub subjects: Vec<ToolSubjectAudit>,
    pub policy_decision: ApprovalMode,
    pub external_directory_required: bool,
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
        let line = serde_json::to_string(entry).context("failed to serialize session entry")?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("failed to open {}", self.path.display()))?;
        writeln!(file, "{line}").context("failed to append session entry")
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads all valid JSONL entries from `path`.
    pub fn read_entries(path: impl AsRef<Path>) -> Result<Vec<SessionLogEntry>> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(Vec::new());
        }

        let file =
            fs::File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
        let reader = BufReader::new(file);
        let mut entries = Vec::new();
        for (index, line) in reader.lines().enumerate() {
            let line = line.with_context(|| {
                format!("failed to read line {} from {}", index + 1, path.display())
            })?;
            if line.trim().is_empty() {
                continue;
            }
            let entry: SessionLogEntry = serde_json::from_str(&line).with_context(|| {
                format!(
                    "failed to parse session entry on line {} from {}",
                    index + 1,
                    path.display()
                )
            })?;
            entries.push(entry);
        }
        Ok(entries)
    }
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
        let entries = JsonlSessionStore::read_entries(store.path())?;
        let fallback_provider_name = provider_name.into();
        let fallback_model_name = model_name.into();
        let (provider_name, model_name) = session_identity_from_entries(&entries)
            .unwrap_or((fallback_provider_name, fallback_model_name));
        let mut session = Self::from_entries(provider_name, model_name, entries).with_store(store);
        session.ensure_identity_entry()?;
        session.mark_interrupted_tool_executions()?;
        Ok(session)
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

    /// Returns a durable task projection reconstructed from append-only control entries.
    pub fn task_state_projection(&self) -> TaskStateProjection {
        TaskStateProjection::from_entries(&self.entries)
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

    /// Returns a durable terminal task projection reconstructed from append-only control entries.
    pub fn terminal_task_projection(&self) -> TerminalTaskProjection {
        TerminalTaskProjection::from_entries(&self.entries)
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

    fn mark_interrupted_tool_executions(&mut self) -> Result<()> {
        for execution in interrupted_tool_executions(&self.entries) {
            self.append_control(ControlEntry::ToolExecution(Box::new(execution)))?;
        }
        Ok(())
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
