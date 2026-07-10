use std::{
    any::Any,
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{Arc, RwLock},
};

use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

use crate::{
    mutation::{ExecutionMutationProfile, MutationEventRecorder, WorkspaceMutationScan},
    permission::{ApprovalMode, ToolOperation, infer_tool_operation},
    provider::ModelMessage,
    session::ControlEntry,
    verification::{DEFAULT_TASK_VERIFICATION_SCOPE_HASH, ToolEffect, VerificationScope},
};

const MODEL_TOOL_CONTENT_MAX_BYTES: usize = 32 * 1024;
const MODEL_TOOL_CONTENT_HEAD_BYTES: usize = 24 * 1024;
const MODEL_TOOL_CONTENT_TAIL_BYTES: usize = 8 * 1024;

/// JSON-schema-backed tool contract exposed to model providers and UI approvals.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub category: ToolCategory,
    pub access: ToolAccess,
    pub preview: ToolPreviewCapability,
}

/// Role-specific tool visibility and execution scope.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ToolRegistryScope {
    #[serde(default)]
    pub allow_all: bool,
    #[serde(default)]
    pub names: BTreeSet<String>,
    #[serde(default)]
    pub prefixes: Vec<String>,
}

impl ToolRegistryScope {
    pub fn from_names_and_prefixes(
        names: impl IntoIterator<Item = impl Into<String>>,
        prefixes: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            allow_all: false,
            names: names.into_iter().map(Into::into).collect(),
            prefixes: prefixes.into_iter().map(Into::into).collect(),
        }
    }

    pub fn allows(&self, name: &str) -> bool {
        self.allow_all
            || self.names.contains(name)
            || self.prefixes.iter().any(|prefix| name.starts_with(prefix))
    }

    pub fn is_empty(&self) -> bool {
        !self.allow_all && self.names.is_empty() && self.prefixes.is_empty()
    }

    pub fn intersection(&self, other: &Self) -> Self {
        if self.is_empty() || other.is_empty() {
            return Self::default();
        }
        if self.allow_all {
            return other.clone();
        }
        if other.allow_all {
            return self.clone();
        }

        let mut names = self
            .names
            .iter()
            .filter(|name| other.allows(name))
            .cloned()
            .collect::<BTreeSet<_>>();
        names.extend(other.names.iter().filter(|name| self.allows(name)).cloned());

        let mut prefixes = Vec::new();
        for left in &self.prefixes {
            for right in &other.prefixes {
                if left.starts_with(right) {
                    push_unique_prefix(&mut prefixes, left.clone());
                } else if right.starts_with(left) {
                    push_unique_prefix(&mut prefixes, right.clone());
                }
            }
        }

        Self {
            allow_all: false,
            names,
            prefixes,
        }
    }

    pub fn union(&self, other: &Self) -> Self {
        if self.allow_all || other.allow_all {
            return Self {
                allow_all: true,
                ..Self::default()
            };
        }

        let mut names = self.names.clone();
        names.extend(other.names.iter().cloned());

        let mut prefixes = self.prefixes.clone();
        for prefix in &other.prefixes {
            push_unique_prefix(&mut prefixes, prefix.clone());
        }

        Self {
            allow_all: false,
            names,
            prefixes,
        }
    }
}

fn push_unique_prefix(prefixes: &mut Vec<String>, prefix: String) {
    if !prefixes.iter().any(|existing| existing == &prefix) {
        prefixes.push(prefix);
    }
}

/// Coarse product category for one provider-neutral tool.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolCategory {
    File,
    Search,
    Shell,
    Mcp,
    Agent,
    Custom,
}

impl ToolCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Search => "search",
            Self::Shell => "shell",
            Self::Mcp => "mcp",
            Self::Agent => "agent",
            Self::Custom => "custom",
        }
    }
}

/// Provider-neutral access class used by permission policy and UI risk labels.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolAccess {
    Read,
    Write,
    Execute,
    Network,
}

impl ToolAccess {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Execute => "execute",
            Self::Network => "network",
        }
    }
}

/// Declares whether a tool can or must provide an approval preview.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolPreviewCapability {
    None,
    Optional,
    Required,
}

/// Mutation evidence strategy owned by one tool implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolMutationTracking {
    None,
    /// Every workspace write uses the RFC-0002 coordinator and its exact per-file evidence.
    Controlled,
    /// Effects are not fully mediated, so the registry must scan the workspace around execution.
    Unknown,
}

/// One resource or capability subject touched by a tool call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ToolSubject {
    pub kind: ToolSubjectKind,
    pub original: String,
    pub normalized: String,
    #[serde(default)]
    pub canonical_path: Option<PathBuf>,
    pub scope: ToolSubjectScope,
}

impl ToolSubject {
    pub fn path(original: impl Into<String>, normalized: impl Into<String>) -> Self {
        Self::path_with_scope(original, normalized, None, ToolSubjectScope::Workspace)
    }

    pub fn path_with_scope(
        original: impl Into<String>,
        normalized: impl Into<String>,
        canonical_path: Option<PathBuf>,
        scope: ToolSubjectScope,
    ) -> Self {
        Self {
            kind: ToolSubjectKind::Path,
            original: original.into(),
            normalized: normalized.into(),
            canonical_path,
            scope,
        }
    }

    pub fn command(command: impl Into<String>, normalized: impl Into<String>) -> Self {
        Self {
            kind: ToolSubjectKind::Command,
            original: command.into(),
            normalized: normalized.into(),
            canonical_path: None,
            scope: ToolSubjectScope::Unknown,
        }
    }

    pub fn mcp_tool(name: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            kind: ToolSubjectKind::McpTool,
            original: name.clone(),
            normalized: name,
            canonical_path: None,
            scope: ToolSubjectScope::Unknown,
        }
    }

    pub fn mcp_trust_class(server_name: impl Into<String>, trust_class: impl Into<String>) -> Self {
        let server_name = server_name.into();
        let trust_class = trust_class.into();
        Self {
            kind: ToolSubjectKind::McpTrustClass,
            original: format!("{server_name}:{trust_class}"),
            normalized: format!("mcp_trust_class:{trust_class}"),
            canonical_path: None,
            scope: ToolSubjectScope::Unknown,
        }
    }

    /// Creates an MCP trust subject whose durable identity binds one concrete process environment.
    ///
    /// The stable normalized value remains suitable for permission-rule matching, while the
    /// original value retains the static and live fingerprints for approval/audit consumers.
    #[must_use]
    pub fn mcp_trust_class_with_process_binding(
        server_name: impl Into<String>,
        trust_class: impl Into<String>,
        static_fingerprint: impl AsRef<str>,
        live_fingerprint: impl AsRef<str>,
    ) -> Self {
        let server_name = server_name.into();
        let trust_class = trust_class.into();
        Self {
            kind: ToolSubjectKind::McpTrustClass,
            original: format!(
                "{server_name}:{trust_class}:{}:{}",
                static_fingerprint.as_ref(),
                live_fingerprint.as_ref()
            ),
            normalized: format!("mcp_trust_class:{trust_class}"),
            canonical_path: None,
            scope: ToolSubjectScope::Unknown,
        }
    }

    pub fn agent(profile_id: impl Into<String>) -> Self {
        let profile_id = profile_id.into();
        Self {
            kind: ToolSubjectKind::Agent,
            original: profile_id.clone(),
            normalized: format!("agent:{profile_id}"),
            canonical_path: None,
            scope: ToolSubjectScope::Unknown,
        }
    }
}

/// Safe summary of data a tool is about to send outside the local agent boundary.
///
/// The payload must be pre-redacted and bounded by the tool implementation; it is persisted
/// in the control plane for audit and must not contain raw file contents or secrets.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct ToolEgressAudit {
    pub destination: String,
    pub operation: String,
    pub payload: Value,
    pub redacted: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolSubjectKind {
    Path,
    Command,
    NetworkEndpoint,
    McpTool,
    McpTrustClass,
    Agent,
    Other,
}

impl ToolSubjectKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Path => "path",
            Self::Command => "command",
            Self::NetworkEndpoint => "network_endpoint",
            Self::McpTool => "mcp_tool",
            Self::McpTrustClass => "mcp_trust_class",
            Self::Agent => "agent",
            Self::Other => "other",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolSubjectScope {
    Workspace,
    External,
    Unknown,
}

impl ToolSubjectScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::External => "external",
            Self::Unknown => "unknown",
        }
    }
}

/// Execution context shared with tools at runtime.
#[derive(Clone)]
pub struct ToolContext {
    pub workspace_root: PathBuf,
    pub timeout_secs: u64,
    pub mutation_recorder: Option<MutationEventRecorder>,
    approved_subjects: Vec<ToolSubject>,
    progress_sink: Option<Arc<dyn ToolProgressSink>>,
    execution_mutation_profile_recorded_call_ids: BTreeSet<String>,
    cancellation: Option<crate::RunCancellationHandle>,
}

impl std::fmt::Debug for ToolContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolContext")
            .field("workspace_root", &self.workspace_root)
            .field("timeout_secs", &self.timeout_secs)
            .field("mutation_recorder", &self.mutation_recorder.is_some())
            .field("approved_subjects", &self.approved_subjects.len())
            .field("progress_sink", &self.progress_sink.is_some())
            .field("cancellation", &self.cancellation.is_some())
            .field(
                "execution_mutation_profile_recorded_call_ids",
                &self.execution_mutation_profile_recorded_call_ids.len(),
            )
            .finish()
    }
}

impl ToolContext {
    #[must_use]
    pub fn new(workspace_root: impl Into<PathBuf>, timeout_secs: u64) -> Self {
        Self {
            workspace_root: workspace_root.into(),
            timeout_secs,
            mutation_recorder: None,
            approved_subjects: Vec::new(),
            progress_sink: None,
            execution_mutation_profile_recorded_call_ids: BTreeSet::new(),
            cancellation: None,
        }
    }

    #[must_use]
    pub fn with_mutation_recorder(mut self, recorder: MutationEventRecorder) -> Self {
        self.mutation_recorder = Some(recorder);
        self
    }

    #[must_use]
    pub fn with_cancellation(mut self, cancellation: crate::RunCancellationHandle) -> Self {
        self.cancellation = Some(cancellation);
        self
    }

    /// Admits one nested forward effect at the last responsible execution boundary.
    pub fn begin_forward_effect(
        &self,
        kind: crate::RunEffectKind,
    ) -> Result<Option<crate::RunEffectGuard>> {
        self.cancellation
            .as_ref()
            .map(|handle| handle.begin_effect(crate::RunEffectClass::Forward, kind))
            .transpose()
            .map_err(Into::into)
    }

    #[must_use]
    pub fn cancellation_handle(&self) -> Option<crate::RunCancellationHandle> {
        self.cancellation.clone()
    }

    /// Carries the exact subjects authorized by the agent into the execution boundary.
    ///
    /// This does not grant permission by itself. Tools may use it only to fail closed when a
    /// dynamic subject changes after approval and before an external effect starts.
    #[must_use]
    pub fn with_approved_subjects(mut self, subjects: Vec<ToolSubject>) -> Self {
        self.approved_subjects = subjects;
        self
    }

    /// Returns the exact subjects authorized for this execution, if it was dispatched by an agent.
    #[must_use]
    pub fn approved_subjects(&self) -> &[ToolSubject] {
        &self.approved_subjects
    }

    #[must_use]
    pub fn with_progress_sink(mut self, sink: Arc<dyn ToolProgressSink>) -> Self {
        self.progress_sink = Some(sink);
        self
    }

    /// Emits a transient tool progress update to the runtime event stream.
    ///
    /// # Errors
    ///
    /// Returns an error when the downstream progress channel has been closed.
    pub fn emit_progress(&self, event: ToolProgressEvent) -> Result<()> {
        if let Some(sink) = &self.progress_sink {
            sink.emit(event)?;
        }
        Ok(())
    }

    #[must_use]
    pub(crate) fn with_execution_mutation_profile_recorded(
        mut self,
        call_id: impl Into<String>,
    ) -> Self {
        self.execution_mutation_profile_recorded_call_ids
            .insert(call_id.into());
        self
    }

    #[must_use]
    fn execution_mutation_profile_recorded_for(&self, call_id: &str) -> bool {
        self.execution_mutation_profile_recorded_call_ids
            .contains(call_id)
    }
}

/// Transient progress sink installed by the agent loop while a tool is executing.
///
/// Progress events are for live UI surfaces and must not be treated as provider-visible final
/// tool results. Durable audit state should continue to use started/completed tool execution
/// entries and final [`ToolResult`] metadata.
pub trait ToolProgressSink: Send + Sync {
    /// Emits one progress event.
    ///
    /// # Errors
    ///
    /// Returns an error when the receiver cannot accept the event.
    fn emit(&self, event: ToolProgressEvent) -> Result<()>;
}

/// Stable identifier for one logical tool execution lifecycle.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct ToolExecutionId(String);

impl ToolExecutionId {
    /// Creates an identifier safe to use in progress coalescing keys and durable execution records.
    ///
    /// # Errors
    ///
    /// Returns an error when `value` is empty or contains path separators or unstable characters.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        validate_tool_execution_id(&value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ToolExecutionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ToolExecutionId {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

fn validate_tool_execution_id(value: &str) -> Result<()> {
    if value.is_empty() {
        bail!("tool execution id cannot be empty");
    }
    if value == "." || value == ".." || value.contains('/') || value.contains('\\') {
        bail!("tool execution id must not contain path separators or traversal");
    }
    if !value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        bail!("tool execution id contains unsupported characters");
    }
    Ok(())
}

/// Provider-neutral live progress update for a running tool execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct ToolProgressEvent {
    pub execution_id: ToolExecutionId,
    pub call_id: String,
    pub tool_name: String,
    pub sequence: u64,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_preview: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_log_ref: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at_ms: Option<u64>,
    #[serde(default)]
    pub details: Value,
}

/// Normalized tool execution result returned to the agent loop and UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ToolResult {
    pub call_id: String,
    pub tool_name: String,
    pub content: String,
    pub status: ToolResultStatus,
    pub metadata: ToolResultMeta,
    #[serde(skip)]
    pub transient_context: Vec<ModelMessage>,
    #[serde(skip)]
    pub control_entries: Vec<ControlEntry>,
}

impl ToolResult {
    pub fn ok(
        call_id: impl Into<String>,
        tool_name: impl Into<String>,
        content: impl Into<String>,
        metadata: ToolResultMeta,
    ) -> Self {
        Self {
            call_id: call_id.into(),
            tool_name: tool_name.into(),
            content: content.into(),
            status: ToolResultStatus::Ok,
            metadata,
            transient_context: Vec::new(),
            control_entries: Vec::new(),
        }
    }

    pub fn error(
        call_id: impl Into<String>,
        tool_name: impl Into<String>,
        kind: ToolErrorKind,
        message: impl Into<String>,
    ) -> Self {
        let message = message.into();
        Self {
            call_id: call_id.into(),
            tool_name: tool_name.into(),
            content: message.clone(),
            status: ToolResultStatus::Error(ToolError {
                kind,
                message,
                retryable: false,
                details: Value::Null,
            }),
            metadata: ToolResultMeta::default(),
            transient_context: Vec::new(),
            control_entries: Vec::new(),
        }
    }

    pub fn with_error_details(mut self, retryable: bool, details: Value) -> Self {
        if let ToolResultStatus::Error(error) = &mut self.status {
            error.retryable = retryable;
            error.details = details;
        }
        self
    }

    pub fn with_transient_context(mut self, context: Vec<ModelMessage>) -> Self {
        self.transient_context = context;
        self
    }

    pub fn with_control_entry(mut self, entry: ControlEntry) -> Self {
        self.control_entries.push(entry);
        self
    }

    pub fn is_error(&self) -> bool {
        matches!(self.status, ToolResultStatus::Error(_))
    }

    pub fn to_model_content(&self) -> String {
        let mut envelope = Map::new();
        let model_content = model_visible_tool_content(&self.content);
        match &self.status {
            ToolResultStatus::Ok => {
                envelope.insert("status".to_owned(), Value::String("ok".to_owned()));
                envelope.insert(
                    "content".to_owned(),
                    Value::String(model_content.content.into_owned()),
                );
            }
            ToolResultStatus::Error(error) => {
                envelope.insert("status".to_owned(), Value::String("error".to_owned()));
                envelope.insert(
                    "content".to_owned(),
                    Value::String(model_content.content.into_owned()),
                );
                envelope.insert("error".to_owned(), error.to_model_value());
            }
        }
        if let Some(truncation) = model_content.truncation {
            envelope.insert("content_truncation".to_owned(), truncation.to_model_value());
        }
        if let Some(meta) = self.metadata.to_model_value() {
            envelope.insert("meta".to_owned(), meta);
        }
        serde_json::to_string(&Value::Object(envelope)).unwrap_or_else(|error| {
            format!(
                r#"{{"status":"error","error":{{"kind":"internal","message":"failed to serialize tool result: {error}","retryable":false}}}}"#
            )
        })
    }

    pub fn to_model_message(&self) -> crate::provider::ModelMessage {
        crate::provider::ModelMessage::tool(self.call_id.clone(), self.to_model_content())
    }

    pub fn summary(&self) -> ToolResultSummary {
        let (error_kind, error_message) = match &self.status {
            ToolResultStatus::Ok => (None, None),
            ToolResultStatus::Error(error) => (Some(error.kind), Some(error.message.clone())),
        };
        ToolResultSummary {
            call_id: self.call_id.clone(),
            tool_name: self.tool_name.clone(),
            is_error: self.is_error(),
            status_label: if self.is_error() {
                "error".to_owned()
            } else {
                "ok".to_owned()
            },
            content_preview: self.content.clone(),
            changed_files: self.metadata.changed_files.clone(),
            exit_code: self.metadata.exit_code,
            truncated: self.metadata.truncated,
            bytes: self.metadata.bytes.or(self.metadata.returned_bytes),
            error_kind,
            error_message,
        }
    }
}

struct ModelVisibleToolContent<'a> {
    content: Cow<'a, str>,
    truncation: Option<ModelToolContentTruncation>,
}

struct ModelToolContentTruncation {
    original_bytes: usize,
    omitted_bytes: usize,
    head_bytes: usize,
    tail_bytes: usize,
}

impl ModelToolContentTruncation {
    fn to_model_value(&self) -> Value {
        let mut object = Map::new();
        object.insert("truncated".to_owned(), Value::Bool(true));
        object.insert(
            "reason".to_owned(),
            Value::String("model_context_limit".to_owned()),
        );
        object.insert(
            "original_bytes".to_owned(),
            Value::Number((self.original_bytes as u64).into()),
        );
        object.insert(
            "omitted_bytes".to_owned(),
            Value::Number((self.omitted_bytes as u64).into()),
        );
        object.insert(
            "head_bytes".to_owned(),
            Value::Number((self.head_bytes as u64).into()),
        );
        object.insert(
            "tail_bytes".to_owned(),
            Value::Number((self.tail_bytes as u64).into()),
        );
        Value::Object(object)
    }
}

fn model_visible_tool_content(content: &str) -> ModelVisibleToolContent<'_> {
    if content.len() <= MODEL_TOOL_CONTENT_MAX_BYTES {
        return ModelVisibleToolContent {
            content: Cow::Borrowed(content),
            truncation: None,
        };
    }

    let head_end = previous_char_boundary(content, MODEL_TOOL_CONTENT_HEAD_BYTES);
    let tail_start = next_char_boundary(
        content,
        content.len().saturating_sub(MODEL_TOOL_CONTENT_TAIL_BYTES),
    )
    .max(head_end);
    let tail_bytes = content.len().saturating_sub(tail_start);
    let omitted_bytes = tail_start.saturating_sub(head_end);
    let marker = format!(
        "\n[model content truncated: original_bytes={} omitted_bytes={} head_bytes={} tail_bytes={}]\n",
        content.len(),
        omitted_bytes,
        head_end,
        tail_bytes
    );
    let mut visible = String::with_capacity(head_end + marker.len() + tail_bytes);
    visible.push_str(&content[..head_end]);
    visible.push_str(&marker);
    visible.push_str(&content[tail_start..]);

    ModelVisibleToolContent {
        content: Cow::Owned(visible),
        truncation: Some(ModelToolContentTruncation {
            original_bytes: content.len(),
            omitted_bytes,
            head_bytes: head_end,
            tail_bytes,
        }),
    }
}

fn previous_char_boundary(value: &str, max_index: usize) -> usize {
    let mut index = max_index.min(value.len());
    while index > 0 && !value.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn next_char_boundary(value: &str, min_index: usize) -> usize {
    let mut index = min_index.min(value.len());
    while index < value.len() && !value.is_char_boundary(index) {
        index += 1;
    }
    index
}

/// Structured success/error status for one tool result.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolResultStatus {
    Ok,
    Error(ToolError),
}

/// Stable structured tool error returned to provider-visible history and UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ToolError {
    pub kind: ToolErrorKind,
    pub message: String,
    pub retryable: bool,
    #[serde(default)]
    pub details: Value,
}

impl ToolError {
    fn to_model_value(&self) -> Value {
        let mut object = Map::new();
        object.insert(
            "kind".to_owned(),
            Value::String(self.kind.as_str().to_owned()),
        );
        object.insert("message".to_owned(), Value::String(self.message.clone()));
        object.insert("retryable".to_owned(), Value::Bool(self.retryable));
        if !value_is_empty(&self.details) {
            object.insert("details".to_owned(), model_visible_details(&self.details));
        }
        Value::Object(object)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolErrorKind {
    InvalidInput,
    PermissionDenied,
    ApprovalRequired,
    ApprovalDenied,
    PathOutsideWorkspace,
    ExternalDirectoryRequired,
    NotFound,
    Timeout,
    /// Execution exceeded a bounded runtime resource such as captured output.
    ResourceLimit,
    Interrupted,
    ExitStatus,
    Io,
    Utf8,
    Network,
    Protocol,
    Unsupported,
    /// A recovery-critical effect cannot proceed without its configured durable audit sink.
    DurabilityRequired,
    /// An approval-bound immutable mutation no longer matches its call or workspace revision.
    StalePreparedMutation,
    Internal,
}

impl ToolErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid_input",
            Self::PermissionDenied => "permission_denied",
            Self::ApprovalRequired => "approval_required",
            Self::ApprovalDenied => "approval_denied",
            Self::PathOutsideWorkspace => "path_outside_workspace",
            Self::ExternalDirectoryRequired => "external_directory_required",
            Self::NotFound => "not_found",
            Self::Timeout => "timeout",
            Self::ResourceLimit => "resource_limit",
            Self::Interrupted => "interrupted",
            Self::ExitStatus => "exit_status",
            Self::Io => "io",
            Self::Utf8 => "utf8",
            Self::Network => "network",
            Self::Protocol => "protocol",
            Self::Unsupported => "unsupported",
            Self::DurabilityRequired => "durability_required",
            Self::StalePreparedMutation => "stale_prepared_mutation",
            Self::Internal => "internal",
        }
    }
}

/// Shared summary used by TUI, CLI, and future audit surfaces.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ToolResultSummary {
    pub call_id: String,
    pub tool_name: String,
    pub is_error: bool,
    pub status_label: String,
    pub content_preview: String,
    pub changed_files: Vec<String>,
    pub exit_code: Option<i32>,
    pub truncated: bool,
    pub bytes: Option<u64>,
    pub error_kind: Option<ToolErrorKind>,
    pub error_message: Option<String>,
}

/// Human-readable preview shown before a mutating tool is approved.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ToolPreview {
    pub title: String,
    pub summary: String,
    pub body: String,
    pub changed_files: Vec<String>,
    #[serde(default)]
    pub file_diffs: Vec<ToolPreviewFile>,
}

/// Per-file diff section within a tool preview.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ToolPreviewFile {
    pub path: String,
    pub diff: String,
}

/// Tool-owned immutable artifact materialized before permission approval.
///
/// The kernel keeps the artifact opaque while binding its content digest and exact subjects to
/// the permission decision. A prepared artifact is moved into execution exactly once; tools must
/// not place a second-query token or mutable cache handle in this value.
pub struct ToolPreparation {
    preview: ToolPreview,
    subjects: Vec<ToolSubject>,
    content_digest: String,
    artifact: Box<dyn Any + Send + Sync>,
}

impl std::fmt::Debug for ToolPreparation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolPreparation")
            .field("preview", &self.preview)
            .field("subjects", &self.subjects)
            .field("content_digest", &self.content_digest)
            .finish_non_exhaustive()
    }
}

impl ToolPreparation {
    /// Creates a tool preparation from one immutable artifact and its content-bound digest.
    pub fn new<T>(
        preview: ToolPreview,
        subjects: Vec<ToolSubject>,
        content_digest: impl Into<String>,
        artifact: T,
    ) -> Result<Self>
    where
        T: Any + Send + Sync,
    {
        let content_digest = content_digest.into();
        if !content_digest.starts_with("sha256:") {
            bail!("tool preparation content digest must use sha256");
        }
        if subjects.is_empty() {
            bail!("tool preparation must bind at least one permission subject");
        }
        Ok(Self {
            preview,
            subjects,
            content_digest,
            artifact: Box::new(artifact),
        })
    }
}

/// Permission binding attached to one prepared tool artifact.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ToolPreparationBinding {
    pub call_id: String,
    pub tool_name: String,
    pub args_digest: String,
    pub approval_identity: String,
    pub policy_fingerprint: String,
    pub subjects: Vec<ToolSubject>,
}

/// Safe durable projection linking approval, execution, and mutation batch audit records.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct PreparedToolAuditBinding {
    pub schema_version: u32,
    pub approval_identity: String,
    pub prepared_digest: String,
    pub content_digest: String,
    pub args_digest: String,
    pub policy_fingerprint: String,
}

/// Registry-owned draft whose exact subjects must participate in permission evaluation.
pub struct ToolPreparationDraft {
    tool: Arc<dyn Tool>,
    args: Value,
    preparation: ToolPreparation,
    call_id: String,
    tool_name: String,
    args_digest: String,
}

impl std::fmt::Debug for ToolPreparationDraft {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolPreparationDraft")
            .field("call_id", &self.call_id)
            .field("tool_name", &self.tool_name)
            .field("args_digest", &self.args_digest)
            .field("preparation", &self.preparation)
            .finish()
    }
}

impl ToolPreparationDraft {
    #[must_use]
    pub fn preview(&self) -> &ToolPreview {
        &self.preparation.preview
    }

    #[must_use]
    pub fn subjects(&self) -> &[ToolSubject] {
        &self.preparation.subjects
    }

    /// Binds this one-shot draft to the exact permission and approval authority.
    pub(crate) fn bind_with_approval_identity(
        self,
        policy_fingerprint: impl Into<String>,
        approval_identity: impl Into<String>,
    ) -> Result<PreparedToolCall> {
        let policy_fingerprint = policy_fingerprint.into();
        if !policy_fingerprint.starts_with("sha256:") {
            bail!("prepared tool policy fingerprint must use sha256");
        }
        let approval_identity = approval_identity.into();
        if approval_identity.trim().is_empty() {
            bail!("prepared tool approval identity must not be empty");
        }
        let binding = ToolPreparationBinding {
            call_id: self.call_id.clone(),
            tool_name: self.tool_name.clone(),
            args_digest: self.args_digest,
            approval_identity,
            policy_fingerprint,
            subjects: self.preparation.subjects.clone(),
        };
        let digest_material = serde_json::json!({
            "schema_version": 2,
            "binding": {
                "call_id": &binding.call_id,
                "tool_name": &binding.tool_name,
                "args_digest": &binding.args_digest,
                "policy_fingerprint": &binding.policy_fingerprint,
                "subjects": &binding.subjects,
            },
            "content_digest": self.preparation.content_digest,
        });
        let encoded = serde_json::to_vec(&digest_material)
            .map_err(|error| anyhow!("failed to encode prepared tool binding: {error}"))?;
        let prepared_digest = crate::stable_event_hash(encoded);
        Ok(PreparedToolCall {
            tool: self.tool,
            args: self.args,
            artifact: self.preparation.artifact,
            preview: self.preparation.preview,
            binding,
            content_digest: self.preparation.content_digest,
            prepared_digest,
        })
    }
}

/// One approval-bound prepared tool call consumed by execution exactly once.
pub struct PreparedToolCall {
    tool: Arc<dyn Tool>,
    args: Value,
    artifact: Box<dyn Any + Send + Sync>,
    preview: ToolPreview,
    binding: ToolPreparationBinding,
    content_digest: String,
    prepared_digest: String,
}

impl std::fmt::Debug for PreparedToolCall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedToolCall")
            .field("binding", &self.binding)
            .field("content_digest", &self.content_digest)
            .field("prepared_digest", &self.prepared_digest)
            .finish_non_exhaustive()
    }
}

impl PreparedToolCall {
    pub(crate) fn authorize(mut self, approval_identity: impl Into<String>) -> Result<Self> {
        let approval_identity = approval_identity.into();
        if approval_identity.trim().is_empty() {
            bail!("prepared tool approval identity must not be empty");
        }
        self.binding.approval_identity = approval_identity;
        Ok(self)
    }

    #[must_use]
    pub fn preview(&self) -> &ToolPreview {
        &self.preview
    }

    #[must_use]
    pub fn prepared_digest(&self) -> &str {
        &self.prepared_digest
    }

    #[must_use]
    pub fn binding(&self) -> &ToolPreparationBinding {
        &self.binding
    }

    #[must_use]
    pub fn audit_binding(&self) -> PreparedToolAuditBinding {
        PreparedToolAuditBinding {
            schema_version: 1,
            approval_identity: self.binding.approval_identity.clone(),
            prepared_digest: self.prepared_digest.clone(),
            content_digest: self.content_digest.clone(),
            args_digest: self.binding.args_digest.clone(),
            policy_fingerprint: self.binding.policy_fingerprint.clone(),
        }
    }

    fn into_execution(self) -> PreparedToolExecution {
        PreparedToolExecution {
            artifact: self.artifact,
            binding: self.binding,
            content_digest: self.content_digest,
            prepared_digest: self.prepared_digest,
        }
    }
}

/// Tool-facing approval-bound artifact passed only through the prepared execution path.
pub struct PreparedToolExecution {
    artifact: Box<dyn Any + Send + Sync>,
    binding: ToolPreparationBinding,
    content_digest: String,
    prepared_digest: String,
}

impl std::fmt::Debug for PreparedToolExecution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedToolExecution")
            .field("binding", &self.binding)
            .field("content_digest", &self.content_digest)
            .field("prepared_digest", &self.prepared_digest)
            .finish_non_exhaustive()
    }
}

impl PreparedToolExecution {
    #[must_use]
    pub fn prepared_digest(&self) -> &str {
        &self.prepared_digest
    }

    #[must_use]
    pub fn binding(&self) -> &ToolPreparationBinding {
        &self.binding
    }

    #[must_use]
    pub fn content_digest(&self) -> &str {
        &self.content_digest
    }

    #[must_use]
    pub fn audit_binding(&self) -> PreparedToolAuditBinding {
        PreparedToolAuditBinding {
            schema_version: 1,
            approval_identity: self.binding.approval_identity.clone(),
            prepared_digest: self.prepared_digest.clone(),
            content_digest: self.content_digest.clone(),
            args_digest: self.binding.args_digest.clone(),
            policy_fingerprint: self.binding.policy_fingerprint.clone(),
        }
    }

    /// Consumes and downcasts the opaque tool-owned artifact.
    pub fn into_artifact<T>(self) -> Result<T>
    where
        T: Any + Send + Sync,
    {
        self.artifact
            .downcast::<T>()
            .map(|artifact| *artifact)
            .map_err(|_| anyhow!("prepared tool artifact type does not match the registered tool"))
    }
}

/// Bounded, persisted projection of one tool preview for user-facing UI replay.
///
/// This snapshot is control-plane data only. It is designed for TUI/session restore surfaces and
/// must not be injected into provider-visible tool result content.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ToolPreviewSnapshot {
    pub call_id: String,
    pub tool_name: String,
    pub title: String,
    pub summary: String,
    #[serde(default)]
    pub changed_files: Vec<String>,
    #[serde(default)]
    pub file_diffs: Vec<ToolPreviewFileSnapshot>,
    pub original_stats: ToolDiffStats,
    pub rendered_stats: ToolDiffStats,
    pub original_line_count: usize,
    pub rendered_line_count: usize,
    pub original_byte_count: usize,
    pub rendered_byte_count: usize,
    pub truncated: bool,
    #[serde(default)]
    pub original_preview_hash: Option<String>,
    pub budget: ToolDiffBudget,
}

impl ToolPreviewSnapshot {
    /// Builds a bounded snapshot from an approval preview.
    ///
    /// The resulting snapshot keeps enough unified diff context for UI rendering while recording
    /// the original stats needed to show truncation honestly.
    pub fn from_preview(
        call_id: impl Into<String>,
        tool_name: impl Into<String>,
        preview: &ToolPreview,
        budget: ToolDiffBudget,
        original_preview_hash: Option<String>,
    ) -> Self {
        let mut rendered_files = Vec::new();
        let mut original_stats = ToolDiffStats::default();
        let mut rendered_stats = ToolDiffStats::default();
        let mut original_line_count = 0usize;
        let mut rendered_line_count = 0usize;
        let mut original_byte_count = 0usize;
        let mut rendered_byte_count = 0usize;
        let mut truncated = preview.file_diffs.len() > budget.max_files;

        for file in preview.file_diffs.iter().take(budget.max_files) {
            let file_original_line_count = diff_line_count(&file.diff);
            let file_original_byte_count = file.diff.len();
            let file_original_stats = ToolDiffStats::from_unified_diff(&file.diff);
            original_stats += file_original_stats;
            original_line_count += file_original_line_count;
            original_byte_count += file_original_byte_count;

            let remaining_lines = budget.max_lines_total.saturating_sub(rendered_line_count);
            let remaining_bytes = budget.max_bytes_total.saturating_sub(rendered_byte_count);
            let line_budget = budget.max_lines_per_file.min(remaining_lines);
            let byte_budget = budget.max_bytes_per_file.min(remaining_bytes);
            let bounded = bounded_diff_text(&file.diff, line_budget, byte_budget);
            let file_rendered_stats = ToolDiffStats::from_unified_diff(&bounded.diff);
            let file_rendered_line_count = diff_line_count(&bounded.diff);
            let file_rendered_byte_count = bounded.diff.len();

            truncated |= bounded.truncated;
            rendered_stats += file_rendered_stats;
            rendered_line_count += file_rendered_line_count;
            rendered_byte_count += file_rendered_byte_count;

            rendered_files.push(ToolPreviewFileSnapshot {
                path: file.path.clone(),
                diff: bounded.diff,
                original_stats: file_original_stats,
                rendered_stats: file_rendered_stats,
                original_line_count: file_original_line_count,
                rendered_line_count: file_rendered_line_count,
                original_byte_count: file_original_byte_count,
                rendered_byte_count: file_rendered_byte_count,
                truncated: bounded.truncated,
            });
        }

        for file in preview.file_diffs.iter().skip(budget.max_files) {
            original_stats += ToolDiffStats::from_unified_diff(&file.diff);
            original_line_count += diff_line_count(&file.diff);
            original_byte_count += file.diff.len();
        }

        Self {
            call_id: call_id.into(),
            tool_name: tool_name.into(),
            title: preview.title.clone(),
            summary: preview.summary.clone(),
            changed_files: preview.changed_files.clone(),
            file_diffs: rendered_files,
            original_stats,
            rendered_stats,
            original_line_count,
            rendered_line_count,
            original_byte_count,
            rendered_byte_count,
            truncated,
            original_preview_hash,
            budget,
        }
    }
}

/// Per-file bounded diff captured for one tool preview.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ToolPreviewFileSnapshot {
    pub path: String,
    pub diff: String,
    pub original_stats: ToolDiffStats,
    pub rendered_stats: ToolDiffStats,
    pub original_line_count: usize,
    pub rendered_line_count: usize,
    pub original_byte_count: usize,
    pub rendered_byte_count: usize,
    pub truncated: bool,
}

/// Unified diff statistics used by approval and historical tool-card surfaces.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ToolDiffStats {
    pub added: usize,
    pub removed: usize,
    pub hunks: usize,
}

impl ToolDiffStats {
    /// Counts added, removed, and hunk header lines in unified diff text.
    pub fn from_unified_diff(diff: &str) -> Self {
        let mut stats = Self::default();
        for line in diff.lines() {
            if line.starts_with("@@") {
                stats.hunks += 1;
            } else if line.starts_with('+') && !line.starts_with("+++") {
                stats.added += 1;
            } else if line.starts_with('-') && !line.starts_with("---") {
                stats.removed += 1;
            }
        }
        stats
    }
}

impl std::ops::AddAssign for ToolDiffStats {
    fn add_assign(&mut self, rhs: Self) {
        self.added += rhs.added;
        self.removed += rhs.removed;
        self.hunks += rhs.hunks;
    }
}

/// Budget used when persisting tool preview diffs into the append-only control log.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ToolDiffBudget {
    pub max_files: usize,
    pub max_lines_total: usize,
    pub max_lines_per_file: usize,
    pub max_bytes_total: usize,
    pub max_bytes_per_file: usize,
}

impl Default for ToolDiffBudget {
    fn default() -> Self {
        Self {
            max_files: 12,
            max_lines_total: 320,
            max_lines_per_file: 160,
            max_bytes_total: 96 * 1024,
            max_bytes_per_file: 48 * 1024,
        }
    }
}

struct BoundedDiffText {
    diff: String,
    truncated: bool,
}

fn bounded_diff_text(diff: &str, max_lines: usize, max_bytes: usize) -> BoundedDiffText {
    let original_line_count = diff_line_count(diff);
    if max_lines == 0 || max_bytes == 0 {
        return BoundedDiffText {
            diff: String::new(),
            truncated: !diff.is_empty(),
        };
    }

    let mut rendered = String::new();
    let mut rendered_lines = 0usize;
    for line in diff.lines().take(max_lines) {
        let separator_bytes = usize::from(!rendered.is_empty());
        if rendered.len() + separator_bytes + line.len() > max_bytes {
            break;
        }
        if !rendered.is_empty() {
            rendered.push('\n');
        }
        rendered.push_str(line);
        rendered_lines += 1;
    }

    BoundedDiffText {
        truncated: rendered_lines < original_line_count || rendered.len() < diff.len(),
        diff: rendered,
    }
}

fn diff_line_count(diff: &str) -> usize {
    if diff.is_empty() {
        0
    } else {
        diff.lines().count()
    }
}

/// Additional structured metadata emitted by a tool execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ToolResultMeta {
    pub duration_ms: Option<u64>,
    pub exit_code: Option<i32>,
    pub stdout_bytes: Option<u64>,
    pub stderr_bytes: Option<u64>,
    pub bytes: Option<u64>,
    pub truncated: bool,
    pub omitted_bytes: Option<u64>,
    pub limit_bytes: Option<u64>,
    pub limit_lines: Option<u64>,
    pub returned_bytes: Option<u64>,
    pub returned_lines: Option<u64>,
    pub total_bytes: Option<u64>,
    pub total_lines: Option<u64>,
    pub returned_matches: Option<u64>,
    pub total_matches: Option<u64>,
    pub returned_entries: Option<u64>,
    pub total_entries: Option<u64>,
    pub changed_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<ToolReceiptMetadata>,
    #[serde(default)]
    pub details: Value,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolReceiptStatus {
    Pending,
    Completed,
    Failed,
    Interrupted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ToolReceiptMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    #[serde(default)]
    pub idempotent: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mutation_operation_ids: Vec<String>,
    pub status: ToolReceiptStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolReceiptReplayDecision {
    ReplayAllowed,
    ReplayDenied,
}

impl ToolReceiptMetadata {
    #[must_use]
    pub fn replay_decision(&self) -> ToolReceiptReplayDecision {
        if self.idempotent
            && self.idempotency_key.is_some()
            && self.status == ToolReceiptStatus::Interrupted
        {
            ToolReceiptReplayDecision::ReplayAllowed
        } else {
            ToolReceiptReplayDecision::ReplayDenied
        }
    }
}

impl Default for ToolResultMeta {
    fn default() -> Self {
        Self {
            duration_ms: None,
            exit_code: None,
            stdout_bytes: None,
            stderr_bytes: None,
            bytes: None,
            truncated: false,
            omitted_bytes: None,
            limit_bytes: None,
            limit_lines: None,
            returned_bytes: None,
            returned_lines: None,
            total_bytes: None,
            total_lines: None,
            returned_matches: None,
            total_matches: None,
            returned_entries: None,
            total_entries: None,
            changed_files: Vec::new(),
            receipt: None,
            details: Value::Null,
        }
    }
}

impl ToolResultMeta {
    fn to_model_value(&self) -> Option<Value> {
        let mut object = Map::new();
        insert_u64(&mut object, "duration_ms", self.duration_ms);
        insert_i32(&mut object, "exit_code", self.exit_code);
        insert_u64(&mut object, "stdout_bytes", self.stdout_bytes);
        insert_u64(&mut object, "stderr_bytes", self.stderr_bytes);
        insert_u64(&mut object, "bytes", self.bytes);
        if self.truncated {
            object.insert("truncated".to_owned(), Value::Bool(true));
        }
        insert_u64(&mut object, "omitted_bytes", self.omitted_bytes);
        insert_u64(&mut object, "limit_bytes", self.limit_bytes);
        insert_u64(&mut object, "limit_lines", self.limit_lines);
        insert_u64(&mut object, "returned_bytes", self.returned_bytes);
        insert_u64(&mut object, "returned_lines", self.returned_lines);
        insert_u64(&mut object, "total_bytes", self.total_bytes);
        insert_u64(&mut object, "total_lines", self.total_lines);
        insert_u64(&mut object, "returned_matches", self.returned_matches);
        insert_u64(&mut object, "total_matches", self.total_matches);
        insert_u64(&mut object, "returned_entries", self.returned_entries);
        insert_u64(&mut object, "total_entries", self.total_entries);
        if !self.changed_files.is_empty() {
            object.insert(
                "changed_files".to_owned(),
                Value::Array(
                    self.changed_files
                        .iter()
                        .cloned()
                        .map(Value::String)
                        .collect(),
                ),
            );
        }
        if !value_is_empty(&self.details) {
            object.insert("details".to_owned(), model_visible_details(&self.details));
        }
        (!object.is_empty()).then_some(Value::Object(object))
    }
}

fn model_visible_details(value: &Value) -> Value {
    const MODEL_DETAIL_STRING_LIMIT: usize = 4096;
    const MODEL_DETAIL_STRING_PREVIEW: usize = 240;

    fn omitted_string_metadata(text: &str, reason: &str, include_preview: bool) -> Value {
        let mut object = Map::new();
        object.insert("omitted".to_owned(), Value::Bool(true));
        object.insert("reason".to_owned(), Value::String(reason.to_owned()));
        object.insert(
            "bytes".to_owned(),
            Value::Number((text.len() as u64).into()),
        );
        object.insert(
            "chars".to_owned(),
            Value::Number((text.chars().count() as u64).into()),
        );
        object.insert(
            "lines".to_owned(),
            Value::Number((text.lines().count() as u64).into()),
        );
        if include_preview {
            object.insert(
                "preview".to_owned(),
                Value::String(text.chars().take(MODEL_DETAIL_STRING_PREVIEW).collect()),
            );
        }
        Value::Object(object)
    }

    fn sanitize(key: Option<&str>, value: &Value) -> Value {
        match value {
            Value::String(text) if key == Some("output_preview") => {
                omitted_string_metadata(text, "ui_artifact_only", false)
            }
            Value::String(text) if text.len() > MODEL_DETAIL_STRING_LIMIT => {
                omitted_string_metadata(text, "model_context_limit", true)
            }
            Value::Array(values) => {
                Value::Array(values.iter().map(|value| sanitize(None, value)).collect())
            }
            Value::Object(values) => Value::Object(
                values
                    .iter()
                    .map(|(key, value)| (key.clone(), sanitize(Some(key.as_str()), value)))
                    .collect(),
            ),
            _ => value.clone(),
        }
    }

    sanitize(None, value)
}

fn insert_i32(object: &mut Map<String, Value>, key: &str, value: Option<i32>) {
    if let Some(value) = value {
        object.insert(key.to_owned(), Value::Number(value.into()));
    }
}

fn insert_u64(object: &mut Map<String, Value>, key: &str, value: Option<u64>) {
    if let Some(value) = value {
        object.insert(key.to_owned(), Value::Number(value.into()));
    }
}

fn value_is_empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Bool(false) => true,
        Value::Array(values) => values.is_empty(),
        Value::Object(values) => values.is_empty(),
        _ => false,
    }
}

/// Exact provider-neutral identity for resources owned by one registered tool lifecycle.
///
/// The namespace identifies the owning subsystem, `scope` retains its exact resource identity, and
/// `generation` distinguishes concurrent or replacement lifecycles inside that scope.
/// Provider-visible tool names must not be used as lifecycle identities because they may be
/// sanitized, truncated, or hashed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ToolLifecycleOwner {
    namespace: String,
    scope: String,
    generation: String,
}

impl ToolLifecycleOwner {
    #[must_use]
    pub fn new(
        namespace: impl Into<String>,
        scope: impl Into<String>,
        generation: impl Into<String>,
    ) -> Self {
        Self {
            namespace: namespace.into(),
            scope: scope.into(),
            generation: generation.into(),
        }
    }

    #[must_use]
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    #[must_use]
    pub fn scope(&self) -> &str {
        &self.scope
    }

    #[must_use]
    pub fn generation(&self) -> &str {
        &self.generation
    }

    #[must_use]
    pub fn belongs_to(&self, namespace: &str, scope: &str) -> bool {
        self.namespace == namespace && self.scope == scope
    }
}

#[async_trait]
pub trait Tool: Send + Sync {
    /// Returns the tool's stable contract and JSON Schema surface.
    fn spec(&self) -> ToolSpec;

    /// Declares how this tool records workspace mutation evidence.
    fn mutation_tracking(&self) -> ToolMutationTracking {
        default_tool_mutation_tracking(&self.spec())
    }

    /// Shuts down lifecycle resources owned by this registered tool generation.
    ///
    /// Stateless tools use the default no-op. Long-lived process or transport tools override this
    /// hook so registry replacement can prove the retired generation has stopped before reporting
    /// success.
    ///
    /// # Errors
    ///
    /// Returns an error when owned lifecycle resources cannot be shut down completely.
    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    /// Returns the exact lifecycle owner for long-lived resources held by this tool.
    ///
    /// Stateless tools return `None`. All tools backed by the same process or transport generation
    /// return the same lossless owner so registry replacement can retire them atomically.
    fn lifecycle_owner(&self) -> Option<ToolLifecycleOwner> {
        None
    }

    /// Returns stable permission subjects for one tool call.
    ///
    /// # Errors
    ///
    /// Returns an error when the arguments are invalid and no reliable subjects can be derived.
    fn permission_subjects(&self, _ctx: &ToolContext, _args: &Value) -> Result<Vec<ToolSubject>> {
        Ok(Vec::new())
    }

    /// Returns the access class used for permission policy on this concrete call.
    ///
    /// Most tools use their static [`ToolSpec::access`]. Shell-like tools may conservatively
    /// downgrade a simple read-only command to `Read` while keeping unknown syntax as `Execute`.
    ///
    /// # Errors
    ///
    /// Returns an error when the arguments are invalid and no reliable access class can be derived.
    fn permission_access(&self, _ctx: &ToolContext, _args: &Value) -> Result<ToolAccess> {
        Ok(self.spec().access)
    }

    /// Returns the fine-grained operation used by permission policy on this concrete call.
    ///
    /// Tools with argument-dependent behavior can override this. The default keeps existing tools
    /// compatible by deriving the operation from the stable tool name and dynamic access class.
    ///
    /// # Errors
    ///
    /// Returns an error when the arguments are invalid and no reliable operation can be derived.
    fn permission_operation(&self, ctx: &ToolContext, args: &Value) -> Result<ToolOperation> {
        let spec = self.spec();
        let access = self.permission_access(ctx, args)?;
        Ok(infer_tool_operation(&spec.name, access))
    }

    /// Returns an optional tool-provided default approval mode for this concrete call.
    ///
    /// This is used for configuration domains that are more specific than the global
    /// access default, such as one MCP server's trust policy, while still allowing
    /// explicit permission tool and subject rules to override it.
    ///
    /// # Errors
    ///
    /// Returns an error when the arguments are invalid and no reliable default can be derived.
    fn permission_default_mode(
        &self,
        _ctx: &ToolContext,
        _args: &Value,
    ) -> Result<Option<ApprovalMode>> {
        Ok(None)
    }

    /// Returns a safe, bounded audit summary for one outbound tool call.
    ///
    /// This hook is evaluated after permission approval and before execution. The returned
    /// payload is written to durable control state, so implementations must not include raw
    /// secrets, large user content, or unbounded remote payloads.
    ///
    /// # Errors
    ///
    /// Returns an error when the arguments are invalid and no reliable egress summary can be
    /// derived.
    fn egress_audit(&self, _ctx: &ToolContext, _args: &Value) -> Result<Option<ToolEgressAudit>> {
        Ok(None)
    }

    /// Produces an optional approval preview for the given tool call.
    ///
    /// # Errors
    ///
    /// Returns an error when preview materialization fails and the caller should surface
    /// that failure instead of silently fabricating a preview.
    async fn preview(&self, _ctx: ToolContext, _args: Value) -> Result<Option<ToolPreview>> {
        Ok(None)
    }

    /// Materializes an immutable, one-shot artifact before permission evaluation.
    ///
    /// Tools whose exact mutation subjects are known only after an external planner response use
    /// this hook. The returned subjects replace the coarse pre-plan permission subjects. The
    /// default keeps existing tools on the ordinary preview/execute path.
    async fn prepare(
        &self,
        _ctx: ToolContext,
        _call_id: String,
        _args: Value,
    ) -> Result<Option<ToolPreparation>> {
        Ok(None)
    }

    /// Executes one approval-bound artifact without replanning or re-querying its source.
    ///
    /// # Errors
    ///
    /// The default fails closed because a tool that returns [`ToolPreparation`] must explicitly
    /// implement the matching one-shot execution path.
    async fn execute_prepared(
        &self,
        _ctx: ToolContext,
        _args: Value,
        _prepared: PreparedToolExecution,
    ) -> Result<ToolResult> {
        bail!("tool does not implement prepared execution")
    }

    /// Executes the tool call within the provided workspace context.
    ///
    /// # Errors
    ///
    /// Returns an error when arguments are invalid or the underlying tool action fails before
    /// it can be expressed as a structured [`ToolResult`].
    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult>;
}

/// Runtime registry for built-in and remote tools.
#[derive(Clone)]
pub struct ToolRegistry {
    tools: Arc<RwLock<BTreeMap<String, Arc<dyn Tool>>>>,
    scope: Option<Arc<ToolRegistryScope>>,
    deny_scope: Option<Arc<ToolRegistryScope>>,
}

/// Strong role-specific view over a shared tool registry.
#[derive(Clone)]
pub struct ScopedToolRegistry {
    inner: ToolRegistry,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self {
            tools: Arc::new(RwLock::new(BTreeMap::new())),
            scope: None,
            deny_scope: None,
        }
    }
}

impl ToolRegistry {
    /// Creates an empty tool registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers one tool by its stable spec name, replacing any prior entry with the same name.
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        let name = tool.spec().name.clone();
        let mut tools = match self.tools.write() {
            Ok(tools) => tools,
            Err(poisoned) => poisoned.into_inner(),
        };
        tools.insert(name, tool);
    }

    /// Returns a role-scoped registry sharing the same underlying tool map.
    pub fn scoped(&self, scope: ToolRegistryScope) -> ScopedToolRegistry {
        ScopedToolRegistry {
            inner: Self {
                tools: Arc::clone(&self.tools),
                scope: Some(Arc::new(self.effective_scope(scope))),
                deny_scope: self.deny_scope.clone(),
            },
        }
    }

    /// Returns a scoped registry that also denies matching tool names across all tool paths.
    pub fn scoped_with_denies(
        &self,
        scope: ToolRegistryScope,
        deny_scope: ToolRegistryScope,
    ) -> ScopedToolRegistry {
        ScopedToolRegistry {
            inner: Self {
                tools: Arc::clone(&self.tools),
                scope: Some(Arc::new(self.effective_scope(scope))),
                deny_scope: self.effective_deny_scope(deny_scope).map(Arc::new),
            },
        }
    }

    fn effective_scope(&self, scope: ToolRegistryScope) -> ToolRegistryScope {
        match self.scope.as_deref() {
            Some(existing) => existing.intersection(&scope),
            None => scope,
        }
    }

    fn effective_deny_scope(&self, deny_scope: ToolRegistryScope) -> Option<ToolRegistryScope> {
        let effective = match self.deny_scope.as_deref() {
            Some(existing) => existing.union(&deny_scope),
            None => deny_scope,
        };
        (!effective.is_empty()).then_some(effective)
    }

    /// Removes registered tools whose names start with the provided prefix.
    ///
    /// Returns the number of removed tools.
    pub fn unregister_by_name_prefix(&mut self, prefix: &str) -> usize {
        self.drain_by_name_prefix(prefix).len()
    }

    /// Removes and returns registered tools whose names start with the provided prefix.
    pub fn drain_by_name_prefix(&mut self, prefix: &str) -> Vec<Arc<dyn Tool>> {
        let mut tools = match self.tools.write() {
            Ok(tools) => tools,
            Err(poisoned) => poisoned.into_inner(),
        };
        let names = tools
            .keys()
            .filter(|name| name.starts_with(prefix))
            .cloned()
            .collect::<Vec<_>>();
        names
            .into_iter()
            .filter_map(|name| tools.remove(&name))
            .collect()
    }

    /// Removes and returns tools belonging to one exact, opaque lifecycle owner.
    pub fn drain_by_lifecycle_owner(&mut self, owner: &ToolLifecycleOwner) -> Vec<Arc<dyn Tool>> {
        let mut tools = match self.tools.write() {
            Ok(tools) => tools,
            Err(poisoned) => poisoned.into_inner(),
        };
        let names = tools
            .iter()
            .filter(|(_, tool)| tool.lifecycle_owner().as_ref() == Some(owner))
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        names
            .into_iter()
            .filter_map(|name| tools.remove(&name))
            .collect()
    }

    /// Returns distinct lifecycle generations registered for one exact opaque scope.
    pub fn lifecycle_owners_by_scope(
        &self,
        namespace: &str,
        scope: &str,
    ) -> Vec<ToolLifecycleOwner> {
        let tools = match self.tools.read() {
            Ok(tools) => tools,
            Err(poisoned) => poisoned.into_inner(),
        };
        tools
            .values()
            .filter_map(|tool| tool.lifecycle_owner())
            .filter(|owner| owner.belongs_to(namespace, scope))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    /// Returns the full list of registered tool specifications.
    pub fn specs(&self) -> Vec<ToolSpec> {
        let tools = match self.tools.read() {
            Ok(tools) => tools,
            Err(poisoned) => poisoned.into_inner(),
        };
        tools
            .values()
            .filter_map(|tool| {
                let spec = tool.spec();
                self.allows(&spec.name).then_some(spec)
            })
            .collect()
    }

    /// Returns one registered spec by name.
    pub fn spec_for(&self, name: &str) -> Option<ToolSpec> {
        if !self.allows(name) {
            return None;
        }
        let tools = match self.tools.read() {
            Ok(tools) => tools,
            Err(poisoned) => poisoned.into_inner(),
        };
        tools.get(name).map(|tool| tool.spec())
    }

    /// Executes a tool call by name.
    ///
    /// # Errors
    ///
    /// Returns an error when the tool is unknown, the JSON args are invalid, or the tool fails.
    pub async fn execute(
        &self,
        ctx: ToolContext,
        call: crate::provider::ToolCall,
    ) -> Result<ToolResult> {
        let (tool, spec, mutation_tracking) = {
            let tools = match self.tools.read() {
                Ok(tools) => tools,
                Err(poisoned) => poisoned.into_inner(),
            };
            let tool = self.allowed_tool(&tools, &call.name)?;
            let spec = tool.spec();
            let mutation_tracking = tool.mutation_tracking();
            (tool, spec, mutation_tracking)
        };
        ensure_execution_mutation_profile_recorded(&ctx, &spec, mutation_tracking, &call.id)?;
        let args: Value = serde_json::from_str(&call.args_json)
            .map_err(|error| anyhow!("invalid tool args for {}: {error}", call.name))?;
        let mutation_scan =
            begin_unknown_mutation_scan(&ctx, mutation_tracking).map_err(|error| {
                anyhow!(
                    "failed to start workspace mutation detection for {}: {error:#}",
                    spec.name
                )
            })?;
        let call_id = call.id;
        let result = tool.execute(ctx.clone(), call_id.clone(), args).await;
        finish_unknown_mutation_scan(&ctx, &spec, &call_id, mutation_scan)
            .map_err(|error| unknown_mutation_scan_finish_error(&spec, error))?;
        result
    }

    /// Executes a tool call after the caller has persisted the corresponding started audit.
    ///
    /// Callers that execute unknown-side-effect tools with a mutation recorder must first append
    /// `ToolExecutionStarted` carrying `ExecutionMutationProfile`; this wrapper marks that
    /// precondition for the low-level registry guard.
    ///
    /// # Errors
    ///
    /// Returns an error when the tool is unknown, the JSON args are invalid, or the tool fails.
    pub async fn execute_after_started_audit(
        &self,
        ctx: ToolContext,
        call: crate::provider::ToolCall,
    ) -> Result<ToolResult> {
        let ctx = ctx.with_execution_mutation_profile_recorded(call.id.clone());
        self.execute(ctx, call).await
    }

    /// Executes an approval-bound prepared artifact after the caller persists started audit.
    ///
    /// The prepared call is consumed by value. Any registry generation, call identity, argument,
    /// subject, or approval binding mismatch fails with `stale_prepared_mutation` before mutation.
    pub(crate) async fn execute_prepared_after_started_audit(
        &self,
        ctx: ToolContext,
        call: crate::provider::ToolCall,
        prepared: PreparedToolCall,
        current_policy_fingerprint: &str,
        current_approval_identity: &str,
    ) -> Result<ToolResult> {
        let ctx = ctx.with_execution_mutation_profile_recorded(call.id.clone());
        let mismatch = if prepared.binding.policy_fingerprint != current_policy_fingerprint {
            Some("policy_changed_after_approval")
        } else if prepared.binding.approval_identity != current_approval_identity {
            Some("approval_authority_changed")
        } else {
            None
        };
        if let Some(reason) = mismatch {
            return Ok(stale_prepared_tool_result(
                &call,
                prepared.prepared_digest(),
                reason,
            ));
        }
        self.execute_prepared(ctx, call, prepared).await
    }

    /// Executes one prepared tool call by consuming its immutable approval-bound artifact.
    async fn execute_prepared(
        &self,
        ctx: ToolContext,
        call: crate::provider::ToolCall,
        prepared: PreparedToolCall,
    ) -> Result<ToolResult> {
        let current_tool = {
            let tools = match self.tools.read() {
                Ok(tools) => tools,
                Err(poisoned) => poisoned.into_inner(),
            };
            self.allowed_tool(&tools, &call.name)?
        };
        let spec = current_tool.spec();
        let mutation_tracking = current_tool.mutation_tracking();
        ensure_execution_mutation_profile_recorded(&ctx, &spec, mutation_tracking, &call.id)?;
        let args: Value = serde_json::from_str(&call.args_json)
            .map_err(|error| anyhow!("invalid tool args for {}: {error}", call.name))?;
        let observed_args_digest = prepared_args_digest(&args)?;
        let mismatch = if call.id != prepared.binding.call_id {
            Some("call_id_changed")
        } else if call.name != prepared.binding.tool_name {
            Some("tool_name_changed")
        } else if observed_args_digest != prepared.binding.args_digest || args != prepared.args {
            Some("args_changed_after_preview")
        } else if ctx.approved_subjects() != prepared.binding.subjects.as_slice() {
            Some("approved_subjects_changed")
        } else if !Arc::ptr_eq(&current_tool, &prepared.tool) {
            Some("registered_tool_generation_changed")
        } else {
            None
        };
        if let Some(reason) = mismatch {
            return Ok(stale_prepared_tool_result(
                &call,
                prepared.prepared_digest(),
                reason,
            ));
        }

        let mutation_scan =
            begin_unknown_mutation_scan(&ctx, mutation_tracking).map_err(|error| {
                anyhow!(
                    "failed to start workspace mutation detection for {}: {error:#}",
                    spec.name
                )
            })?;
        let call_id = call.id;
        let result = current_tool
            .execute_prepared(ctx.clone(), args, prepared.into_execution())
            .await;
        finish_unknown_mutation_scan(&ctx, &spec, &call_id, mutation_scan)
            .map_err(|error| unknown_mutation_scan_finish_error(&spec, error))?;
        result
    }

    /// Returns the mutation profile that must be persisted before executing this tool call.
    ///
    /// # Errors
    ///
    /// Returns an error when the tool is unknown or the workspace snapshot cannot be captured.
    pub fn execution_mutation_profile(
        &self,
        ctx: &ToolContext,
        call: &crate::provider::ToolCall,
    ) -> Result<Option<ExecutionMutationProfile>> {
        let (spec, mutation_tracking) = {
            let tools = match self.tools.read() {
                Ok(tools) => tools,
                Err(poisoned) => poisoned.into_inner(),
            };
            let tool = self.allowed_tool(&tools, &call.name)?;
            (tool.spec(), tool.mutation_tracking())
        };
        execution_mutation_profile_for_tool(ctx, &spec, mutation_tracking, &call.id)
    }

    /// Builds a preview for a tool call by name.
    ///
    /// # Errors
    ///
    /// Returns an error when the tool is unknown, the JSON args are invalid, or preview
    /// generation itself fails.
    pub async fn preview(
        &self,
        ctx: ToolContext,
        call: crate::provider::ToolCall,
    ) -> Result<Option<ToolPreview>> {
        let tool = {
            let tools = match self.tools.read() {
                Ok(tools) => tools,
                Err(poisoned) => poisoned.into_inner(),
            };
            self.allowed_tool(&tools, &call.name)?
        };
        let args: Value = serde_json::from_str(&call.args_json)
            .map_err(|error| anyhow!("invalid tool args for {}: {error}", call.name))?;
        tool.preview(ctx, args).await
    }

    /// Materializes a one-shot tool artifact before permission evaluation.
    ///
    /// The draft's exact subjects must replace any coarse pre-plan subjects when constructing the
    /// permission decision. Returning `None` leaves the tool on the ordinary preview path.
    pub async fn prepare(
        &self,
        ctx: ToolContext,
        call: crate::provider::ToolCall,
    ) -> Result<Option<ToolPreparationDraft>> {
        let tool = {
            let tools = match self.tools.read() {
                Ok(tools) => tools,
                Err(poisoned) => poisoned.into_inner(),
            };
            self.allowed_tool(&tools, &call.name)?
        };
        let args: Value = serde_json::from_str(&call.args_json)
            .map_err(|error| anyhow!("invalid tool args for {}: {error}", call.name))?;
        let args_digest = prepared_args_digest(&args)?;
        let Some(preparation) = tool.prepare(ctx, call.id.clone(), args.clone()).await? else {
            return Ok(None);
        };
        Ok(Some(ToolPreparationDraft {
            tool,
            args,
            preparation,
            call_id: call.id,
            tool_name: call.name,
            args_digest,
        }))
    }

    /// Returns stable permission subjects for a tool call by name.
    ///
    /// # Errors
    ///
    /// Returns an error when the tool is unknown or the JSON arguments are invalid.
    pub fn permission_subjects(
        &self,
        ctx: &ToolContext,
        call: &crate::provider::ToolCall,
    ) -> Result<Vec<ToolSubject>> {
        let tool = {
            let tools = match self.tools.read() {
                Ok(tools) => tools,
                Err(poisoned) => poisoned.into_inner(),
            };
            self.allowed_tool(&tools, &call.name)?
        };
        let args: Value = serde_json::from_str(&call.args_json)
            .map_err(|error| anyhow!("invalid tool args for {}: {error}", call.name))?;
        tool.permission_subjects(ctx, &args)
    }

    /// Returns the dynamic permission access class for one concrete tool call.
    ///
    /// # Errors
    ///
    /// Returns an error when the tool is unknown, the JSON args are invalid, or the tool cannot
    /// derive a reliable access class for the call.
    pub fn permission_access(
        &self,
        ctx: &ToolContext,
        call: &crate::provider::ToolCall,
    ) -> Result<ToolAccess> {
        let tool = {
            let tools = match self.tools.read() {
                Ok(tools) => tools,
                Err(poisoned) => poisoned.into_inner(),
            };
            self.allowed_tool(&tools, &call.name)?
        };
        let args: Value = serde_json::from_str(&call.args_json)
            .map_err(|error| anyhow!("invalid tool args for {}: {error}", call.name))?;
        tool.permission_access(ctx, &args)
    }

    /// Returns the fine-grained permission operation for a tool call by name.
    ///
    /// # Errors
    ///
    /// Returns an error when the tool is unknown, the JSON args are invalid, or the tool cannot
    /// derive a reliable operation for the call.
    pub fn permission_operation(
        &self,
        ctx: &ToolContext,
        call: &crate::provider::ToolCall,
    ) -> Result<ToolOperation> {
        let tool = {
            let tools = match self.tools.read() {
                Ok(tools) => tools,
                Err(poisoned) => poisoned.into_inner(),
            };
            self.allowed_tool(&tools, &call.name)?
        };
        let args: Value = serde_json::from_str(&call.args_json)
            .map_err(|error| anyhow!("invalid tool args for {}: {error}", call.name))?;
        tool.permission_operation(ctx, &args)
    }

    /// Returns an optional tool-provided default approval mode for a tool call by name.
    ///
    /// # Errors
    ///
    /// Returns an error when the tool is unknown or the JSON arguments are invalid.
    pub fn permission_default_mode(
        &self,
        ctx: &ToolContext,
        call: &crate::provider::ToolCall,
    ) -> Result<Option<ApprovalMode>> {
        let tool = {
            let tools = match self.tools.read() {
                Ok(tools) => tools,
                Err(poisoned) => poisoned.into_inner(),
            };
            self.allowed_tool(&tools, &call.name)?
        };
        let args: Value = serde_json::from_str(&call.args_json)
            .map_err(|error| anyhow!("invalid tool args for {}: {error}", call.name))?;
        tool.permission_default_mode(ctx, &args)
    }

    /// Returns a safe outbound audit summary for a tool call by name.
    ///
    /// # Errors
    ///
    /// Returns an error when the tool is unknown or the JSON arguments are invalid.
    pub fn egress_audit(
        &self,
        ctx: &ToolContext,
        call: &crate::provider::ToolCall,
    ) -> Result<Option<ToolEgressAudit>> {
        let tool = {
            let tools = match self.tools.read() {
                Ok(tools) => tools,
                Err(poisoned) => poisoned.into_inner(),
            };
            self.allowed_tool(&tools, &call.name)?
        };
        let args: Value = serde_json::from_str(&call.args_json)
            .map_err(|error| anyhow!("invalid tool args for {}: {error}", call.name))?;
        tool.egress_audit(ctx, &args)
    }

    fn allows(&self, name: &str) -> bool {
        self.scope.as_ref().is_none_or(|scope| scope.allows(name))
            && self
                .deny_scope
                .as_ref()
                .is_none_or(|scope| !scope.allows(name))
    }

    fn allowed_tool(
        &self,
        tools: &BTreeMap<String, Arc<dyn Tool>>,
        name: &str,
    ) -> Result<Arc<dyn Tool>> {
        if !self.allows(name) {
            return Err(anyhow!("tool {name} is not available in this role scope"));
        }
        tools
            .get(name)
            .cloned()
            .ok_or_else(|| anyhow!("unknown tool {name}"))
    }
}

impl ScopedToolRegistry {
    /// Returns this scoped registry as the standard registry type used by the agent loop.
    pub fn into_registry(self) -> ToolRegistry {
        self.inner
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        self.inner.specs()
    }

    pub fn spec_for(&self, name: &str) -> Option<ToolSpec> {
        self.inner.spec_for(name)
    }

    /// Executes a scoped tool call.
    ///
    /// # Errors
    ///
    /// Returns an error when the tool is outside the role scope, unknown, or fails.
    pub async fn execute(
        &self,
        ctx: ToolContext,
        call: crate::provider::ToolCall,
    ) -> Result<ToolResult> {
        self.inner.execute(ctx, call).await
    }

    /// Builds a scoped approval preview.
    ///
    /// # Errors
    ///
    /// Returns an error when the tool is outside the role scope, unknown, or preview fails.
    pub async fn preview(
        &self,
        ctx: ToolContext,
        call: crate::provider::ToolCall,
    ) -> Result<Option<ToolPreview>> {
        self.inner.preview(ctx, call).await
    }

    pub async fn prepare(
        &self,
        ctx: ToolContext,
        call: crate::provider::ToolCall,
    ) -> Result<Option<ToolPreparationDraft>> {
        self.inner.prepare(ctx, call).await
    }

    pub fn permission_subjects(
        &self,
        ctx: &ToolContext,
        call: &crate::provider::ToolCall,
    ) -> Result<Vec<ToolSubject>> {
        self.inner.permission_subjects(ctx, call)
    }

    pub fn permission_access(
        &self,
        ctx: &ToolContext,
        call: &crate::provider::ToolCall,
    ) -> Result<ToolAccess> {
        self.inner.permission_access(ctx, call)
    }

    pub fn permission_operation(
        &self,
        ctx: &ToolContext,
        call: &crate::provider::ToolCall,
    ) -> Result<ToolOperation> {
        self.inner.permission_operation(ctx, call)
    }

    pub fn permission_default_mode(
        &self,
        ctx: &ToolContext,
        call: &crate::provider::ToolCall,
    ) -> Result<Option<ApprovalMode>> {
        self.inner.permission_default_mode(ctx, call)
    }

    pub fn egress_audit(
        &self,
        ctx: &ToolContext,
        call: &crate::provider::ToolCall,
    ) -> Result<Option<ToolEgressAudit>> {
        self.inner.egress_audit(ctx, call)
    }
}

fn prepared_args_digest(args: &Value) -> Result<String> {
    let encoded = serde_json::to_vec(args)
        .map_err(|error| anyhow!("failed to encode prepared tool arguments: {error}"))?;
    Ok(crate::stable_event_hash(encoded))
}

fn stale_prepared_tool_result(
    call: &crate::provider::ToolCall,
    prepared_digest: &str,
    reason: &str,
) -> ToolResult {
    let mut result = ToolResult::error(
        call.id.clone(),
        call.name.clone(),
        ToolErrorKind::StalePreparedMutation,
        format!("prepared mutation is stale: {reason}"),
    )
    .with_error_details(
        false,
        serde_json::json!({
            "reason": reason,
            "prepared_mutation_digest": prepared_digest,
        }),
    );
    result.metadata.details = serde_json::json!({
        "prepared_mutation_digest": prepared_digest,
        "stale_reason": reason,
    });
    result
}

fn begin_unknown_mutation_scan(
    ctx: &ToolContext,
    mutation_tracking: ToolMutationTracking,
) -> Result<Option<WorkspaceMutationScan>> {
    if mutation_tracking != ToolMutationTracking::Unknown {
        return Ok(None);
    }
    let Some(recorder) = &ctx.mutation_recorder else {
        return Ok(None);
    };
    let scope = VerificationScope::all_tracked(DEFAULT_TASK_VERIFICATION_SCOPE_HASH);
    recorder
        .capture_workspace_scan(&ctx.workspace_root, &scope)
        .map(Some)
}

fn ensure_execution_mutation_profile_recorded(
    ctx: &ToolContext,
    spec: &ToolSpec,
    mutation_tracking: ToolMutationTracking,
    call_id: &str,
) -> Result<()> {
    if mutation_tracking != ToolMutationTracking::Unknown || ctx.mutation_recorder.is_none() {
        return Ok(());
    }
    if ctx.execution_mutation_profile_recorded_for(call_id) {
        return Ok(());
    }
    Err(anyhow!(
        "tool {} requires persisted ToolExecutionStarted execution mutation profile before execution",
        spec.name
    ))
}

pub(crate) fn execution_mutation_profile_for_tool(
    ctx: &ToolContext,
    spec: &ToolSpec,
    mutation_tracking: ToolMutationTracking,
    call_id: &str,
) -> Result<Option<ExecutionMutationProfile>> {
    if mutation_tracking != ToolMutationTracking::Unknown {
        return Ok(None);
    }
    let Some(recorder) = &ctx.mutation_recorder else {
        return Ok(None);
    };
    let scope = VerificationScope::all_tracked(DEFAULT_TASK_VERIFICATION_SCOPE_HASH);
    recorder
        .execution_mutation_profile(
            &ctx.workspace_root,
            &scope,
            call_id.to_owned(),
            spec.name.clone(),
            unknown_mutation_tool_effect(spec),
        )
        .map(Some)
}

fn finish_unknown_mutation_scan(
    ctx: &ToolContext,
    spec: &ToolSpec,
    call_id: &str,
    scan: Option<WorkspaceMutationScan>,
) -> Result<()> {
    let Some(scan) = scan else {
        return Ok(());
    };
    let Some(recorder) = &ctx.mutation_recorder else {
        return Ok(());
    };
    match recorder.record_workspace_mutation_if_changed(
        &scan,
        &ctx.workspace_root,
        call_id.to_owned(),
        spec.name.clone(),
        unknown_mutation_tool_effect(spec),
    ) {
        Ok(_) => {}
        Err(_) => {
            recorder.record_workspace_scan_unavailable_after(
                &scan,
                call_id.to_owned(),
                spec.name.clone(),
                unknown_mutation_tool_effect(spec),
            )?;
        }
    }
    Ok(())
}

fn unknown_mutation_scan_finish_error(spec: &ToolSpec, error: anyhow::Error) -> anyhow::Error {
    let message = format!(
        "failed to finish workspace mutation detection for {}: {error:#}",
        spec.name
    );
    anyhow!(message)
}

fn default_tool_mutation_tracking(spec: &ToolSpec) -> ToolMutationTracking {
    if spec.access == ToolAccess::Read {
        ToolMutationTracking::None
    } else if matches!(
        spec.category,
        ToolCategory::Shell | ToolCategory::Mcp | ToolCategory::Custom
    ) {
        ToolMutationTracking::Unknown
    } else {
        ToolMutationTracking::None
    }
}

fn unknown_mutation_tool_effect(_spec: &ToolSpec) -> ToolEffect {
    ToolEffect::Unknown
}

#[cfg(test)]
#[path = "tests/tool_tests.rs"]
mod tests;
