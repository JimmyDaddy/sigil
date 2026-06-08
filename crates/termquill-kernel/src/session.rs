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
    CompactionConfig, MemoryConfig,
    memory::{apply_memory_report, materialize_memory},
    provider::{
        CompletionRequest, ModelMessage, PrefixSnapshot, ProviderContinuationState, ResponseHandle,
        SessionStats, UsageStats,
    },
    tool::ToolSpec,
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
    #[serde(alias = "UsageSnapshot")]
    UsageSnapshot(UsageStats),
    #[serde(alias = "CompactionApplied")]
    CompactionApplied(CompactionRecord),
    #[serde(alias = "Note")]
    Note {
        kind: String,
        data: serde_json::Value,
    },
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

    pub fn latest_compaction_record(&self) -> Option<CompactionRecord> {
        latest_compaction_record(&self.entries)
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
        let memory = materialize_memory(workspace_root, memory_config)?;
        let projected_messages = self.projected_messages();
        let mut request_messages = memory.messages.clone();
        request_messages.extend(projected_messages);

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
    ModelMessage {
        id: format!("local_repair:missing_tool_result:{}", call.id),
        role: crate::MessageRole::Tool,
        content: Some(format!(
            "tool call {} did not return a result before the previous run stopped; retry the tool call with valid arguments if it is still needed",
            call.name
        )),
        tool_calls: Vec::new(),
        tool_call_id: Some(call.id.clone()),
    }
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
            || messages[boundary - 1].tool_calls.is_empty().not()
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

trait BoolExt {
    fn not(self) -> bool;
}

impl BoolExt for bool {
    fn not(self) -> bool {
        !self
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use anyhow::Result;

    use crate::{
        CompactionRecord, MemoryConfig, ProviderContinuationState, ResponseHandle, UsageStats,
        provider::ModelMessage,
    };

    use super::{
        CompactionConfig, ControlEntry, JsonlSessionStore, PrefixSnapshot, Session,
        SessionLogEntry, session_stats_from_entries,
    };

    #[test]
    fn load_from_store_recovers_identity_from_prefix_snapshot() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("session.jsonl");
        let store = JsonlSessionStore::new(&path)?;
        store.append(&SessionLogEntry::Control(
            ControlEntry::PrefixSnapshotCaptured(PrefixSnapshot {
                materialized_text: "prefix".to_owned(),
                sha256: "abc".to_owned(),
                provider_name: "deepseek".to_owned(),
                model_name: "deepseek-v4-flash".to_owned(),
                memory_fingerprint: "none".to_owned(),
                tool_schema_fingerprint: "tools".to_owned(),
                skill_index_fingerprint: "skills".to_owned(),
            }),
        ))?;

        let session = Session::load_from_store("other-provider", "other-model", store)?;

        assert_eq!(session.provider_name(), "deepseek");
        assert_eq!(session.model_name(), "deepseek-v4-flash");
        assert!(session.entries().iter().any(|entry| {
            matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::SessionIdentity {
                    provider_name,
                    model_name,
                }) if provider_name == "deepseek" && model_name == "deepseek-v4-flash"
            )
        }));
        Ok(())
    }

    #[test]
    fn load_from_store_persists_identity_for_empty_log() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("session.jsonl");
        let store = JsonlSessionStore::new(&path)?;

        let session = Session::load_from_store("deepseek", "deepseek-v4-flash", store)?;

        assert_eq!(session.provider_name(), "deepseek");
        assert_eq!(session.model_name(), "deepseek-v4-flash");
        assert_eq!(session.entries().len(), 1);
        assert!(matches!(
            session.entries()[0],
            SessionLogEntry::Control(ControlEntry::SessionIdentity { .. })
        ));
        Ok(())
    }

    #[test]
    fn build_request_persists_prefix_snapshot_in_memory_and_store() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("session.jsonl");
        let store = JsonlSessionStore::new(&path)?;
        fs::write(temp.path().join("AGENTS.md"), "repo rules\n")?;
        let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(store.clone());
        session.append_user_message(ModelMessage::user("hello"))?;

        let request = session.build_request(
            temp.path(),
            &MemoryConfig { enabled: true },
            Vec::new(),
            None,
            None,
            None,
        )?;

        assert_eq!(request.provider_name, "deepseek");
        assert!(
            request
                .messages
                .iter()
                .any(|message| matches!(message.role, crate::MessageRole::System))
        );
        assert!(session.entries().iter().any(|entry| {
            matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::PrefixSnapshotCaptured(_))
            )
        }));

        let reloaded = JsonlSessionStore::read_entries(store.path())?;
        assert!(reloaded.iter().any(|entry| {
            matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::PrefixSnapshotCaptured(_))
            )
        }));
        Ok(())
    }

    #[test]
    fn messages_repair_orphan_tool_call_projection() -> Result<()> {
        let mut session = Session::new("deepseek", "deepseek-v4-flash");
        session.append_assistant_message(ModelMessage::assistant(
            None,
            vec![crate::ToolCall {
                id: "call-1".to_owned(),
                name: "read_file".to_owned(),
                args_json: "{}".to_owned(),
            }],
        ))?;
        session.append_user_message(ModelMessage::user("continue"))?;

        let projected = session.messages();

        assert_eq!(projected.len(), 3);
        assert!(matches!(projected[0].role, crate::MessageRole::Assistant));
        assert!(matches!(projected[1].role, crate::MessageRole::Tool));
        assert_eq!(projected[1].id, "local_repair:missing_tool_result:call-1");
        assert_eq!(projected[1].tool_call_id.as_deref(), Some("call-1"));
        assert!(projected[1].content.as_deref().is_some_and(|content| {
            content.contains("did not return a result before the previous run stopped")
        }));
        assert!(matches!(projected[2].role, crate::MessageRole::User));
        Ok(())
    }

    #[test]
    fn latest_control_state_queries_return_latest_matching_records() -> Result<()> {
        let mut session = Session::new("deepseek", "deepseek-v4-flash");
        session.append_control(ControlEntry::ResponseHandleTracked(ResponseHandle {
            provider_name: "deepseek".to_owned(),
            response_id: "response-old".to_owned(),
            continuation_cursor: Some("cursor-old".to_owned()),
        }))?;
        session.append_control(ControlEntry::ResponseHandleTracked(ResponseHandle {
            provider_name: "other".to_owned(),
            response_id: "response-other".to_owned(),
            continuation_cursor: None,
        }))?;
        session.append_control(ControlEntry::PrefixSnapshotCaptured(PrefixSnapshot {
            materialized_text: "prefix-old".to_owned(),
            sha256: "old".to_owned(),
            provider_name: "deepseek".to_owned(),
            model_name: "deepseek-v4-flash".to_owned(),
            memory_fingerprint: "memory-old".to_owned(),
            tool_schema_fingerprint: "tools-old".to_owned(),
            skill_index_fingerprint: "skills-old".to_owned(),
        }))?;
        session.append_control(ControlEntry::ContinuationStateSaved(
            ProviderContinuationState {
                provider_name: "deepseek".to_owned(),
                state_kind: "reasoning".to_owned(),
                message_id: Some("message-1".to_owned()),
                opaque_blob: serde_json::json!({"cursor":"old"}),
            },
        ))?;
        session.append_control(ControlEntry::ContinuationStateSaved(
            ProviderContinuationState {
                provider_name: "deepseek".to_owned(),
                state_kind: "reasoning".to_owned(),
                message_id: Some("message-1".to_owned()),
                opaque_blob: serde_json::json!({"cursor":"new"}),
            },
        ))?;
        session.append_control(ControlEntry::CompactionApplied(CompactionRecord {
            summary: "summary-old".to_owned(),
            compacted_message_count: 1,
            retained_tail_message_count: 2,
        }))?;
        session.append_control(ControlEntry::ResponseHandleTracked(ResponseHandle {
            provider_name: "deepseek".to_owned(),
            response_id: "response-new".to_owned(),
            continuation_cursor: Some("cursor-new".to_owned()),
        }))?;
        session.append_control(ControlEntry::PrefixSnapshotCaptured(PrefixSnapshot {
            materialized_text: "prefix-new".to_owned(),
            sha256: "new".to_owned(),
            provider_name: "deepseek".to_owned(),
            model_name: "deepseek-v4-flash".to_owned(),
            memory_fingerprint: "memory-new".to_owned(),
            tool_schema_fingerprint: "tools-new".to_owned(),
            skill_index_fingerprint: "skills-new".to_owned(),
        }))?;
        session.append_control(ControlEntry::CompactionApplied(CompactionRecord {
            summary: "summary-new".to_owned(),
            compacted_message_count: 3,
            retained_tail_message_count: 2,
        }))?;

        assert!(matches!(
            session.latest_response_handle("deepseek"),
            Some(handle) if handle.response_id == "response-new"
                && handle.continuation_cursor.as_deref() == Some("cursor-new")
        ));
        assert!(matches!(
            session.latest_response_handle("other"),
            Some(handle) if handle.response_id == "response-other"
        ));
        assert!(matches!(
            session.latest_prefix_snapshot(),
            Some(snapshot) if snapshot.sha256 == "new"
        ));
        assert!(matches!(
            session.latest_compaction_record(),
            Some(record) if record.summary == "summary-new"
        ));
        let states = session.continuation_states("deepseek");
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].opaque_blob, serde_json::json!({"cursor":"new"}));
        Ok(())
    }

    #[test]
    fn compaction_persists_record_and_projects_summary_plus_tail() -> Result<()> {
        let mut session = Session::new("deepseek", "deepseek-v4-flash");
        session.append_user_message(ModelMessage::user("step one"))?;
        session.append_assistant_message(ModelMessage::assistant(
            Some("step two".to_owned()),
            Vec::new(),
        ))?;
        session.append_user_message(ModelMessage::user("step three"))?;
        session.append_assistant_message(ModelMessage::assistant(
            Some("step four".to_owned()),
            Vec::new(),
        ))?;

        let record = session.compact_now(&CompactionConfig {
            enabled: true,
            soft_threshold_ratio: 0.5,
            hard_threshold_ratio: 0.8,
            context_window_tokens: Some(1000),
            tail_messages: 2,
        })?;

        assert_eq!(record.compacted_message_count, 2);
        assert_eq!(record.retained_tail_message_count, 2);
        assert!(session.entries().iter().any(|entry| {
            matches!(entry, SessionLogEntry::Control(ControlEntry::CompactionApplied(saved)) if saved == &record)
        }));

        let projected = session.messages();
        assert_eq!(projected.len(), 3);
        assert!(
            projected[0]
                .content
                .as_deref()
                .is_some_and(|content| content.contains("Compacted 2 earlier messages"))
        );
        assert_eq!(projected[1].content.as_deref(), Some("step three"));
        assert_eq!(projected[2].content.as_deref(), Some("step four"));
        Ok(())
    }

    #[test]
    fn can_compact_requires_a_safe_boundary() -> Result<()> {
        let mut session = Session::new("deepseek", "deepseek-v4-flash");
        session.append_assistant_message(ModelMessage::assistant(
            None,
            vec![crate::ToolCall {
                id: "tool-1".to_owned(),
                name: "read_file".to_owned(),
                args_json: "{\"path\":\"README.md\"}".to_owned(),
            }],
        ))?;
        session.append_tool_message(ModelMessage::tool("tool-1", "ok"))?;

        assert!(!session.can_compact(&CompactionConfig {
            enabled: true,
            soft_threshold_ratio: 0.5,
            hard_threshold_ratio: 0.8,
            context_window_tokens: Some(1000),
            tail_messages: 1,
        }));
        Ok(())
    }

    #[test]
    fn compaction_preview_reports_folded_messages_and_projected_after_state() -> Result<()> {
        let mut session = Session::new("deepseek", "deepseek-v4-flash");
        session.append_user_message(ModelMessage::user("alpha"))?;
        session.append_assistant_message(ModelMessage::assistant(
            Some("beta".to_owned()),
            Vec::new(),
        ))?;
        session.append_user_message(ModelMessage::user("gamma"))?;
        session.append_assistant_message(ModelMessage::assistant(
            Some("delta".to_owned()),
            Vec::new(),
        ))?;

        let preview = session
            .compaction_preview(&CompactionConfig {
                enabled: true,
                soft_threshold_ratio: 0.5,
                hard_threshold_ratio: 0.8,
                context_window_tokens: Some(1000),
                tail_messages: 2,
            })?
            .expect("preview should exist");

        assert_eq!(preview.record.compacted_message_count, 2);
        assert_eq!(preview.folded_messages.len(), 2);
        assert_eq!(preview.projected_messages.len(), 3);
        assert!(
            preview.projected_messages[0]
                .content
                .as_deref()
                .is_some_and(|content| content.contains("Compacted 2 earlier messages"))
        );
        assert_eq!(
            preview.projected_messages[1].content.as_deref(),
            Some("gamma")
        );
        assert_eq!(
            preview.projected_messages[2].content.as_deref(),
            Some("delta")
        );
        Ok(())
    }

    #[test]
    fn load_from_store_accepts_legacy_pascal_case_control_entries() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("legacy-session.jsonl");
        fs::write(
            &path,
            "{\"Control\":{\"SessionIdentity\":{\"provider_name\":\"deepseek\",\"model_name\":\"deepseek-v4-flash\"}}}\n",
        )?;

        let store = JsonlSessionStore::new(&path)?;
        let session = Session::load_from_store("fallback-provider", "fallback-model", store)?;

        assert_eq!(session.provider_name(), "deepseek");
        assert_eq!(session.model_name(), "deepseek-v4-flash");
        Ok(())
    }

    #[test]
    fn session_stats_are_restored_from_usage_snapshots() -> Result<()> {
        let entries = vec![
            SessionLogEntry::Control(ControlEntry::UsageSnapshot(UsageStats {
                prompt_tokens: 120,
                completion_tokens: 10,
                cache_hit_tokens: 90,
                cache_miss_tokens: 30,
                input_cost: 0.0,
                output_cost: 0.0,
                cache_savings: 0.0,
                system_fingerprint: None,
            })),
            SessionLogEntry::Control(ControlEntry::UsageSnapshot(UsageStats {
                prompt_tokens: 48,
                completion_tokens: 6,
                cache_hit_tokens: 28,
                cache_miss_tokens: 20,
                input_cost: 0.0,
                output_cost: 0.0,
                cache_savings: 0.0,
                system_fingerprint: None,
            })),
            SessionLogEntry::Control(ControlEntry::CompactionApplied(CompactionRecord {
                summary: "summary".to_owned(),
                compacted_message_count: 2,
                retained_tail_message_count: 2,
            })),
        ];

        let stats = session_stats_from_entries(&entries);
        let session = Session::from_entries("deepseek", "deepseek-v4-flash", entries);

        assert_eq!(stats.prompt_tokens, 168);
        assert_eq!(stats.last_prompt_tokens, 0);
        assert_eq!(session.stats().prompt_tokens, 168);
        assert_eq!(session.stats().last_prompt_tokens, 0);
        Ok(())
    }
}
