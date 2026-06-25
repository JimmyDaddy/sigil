use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{Arc, RwLock},
};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{
    permission::{ApprovalMode, ToolOperation, infer_tool_operation},
    provider::ModelMessage,
    session::ControlEntry,
};

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
#[derive(Debug, Clone)]
pub struct ToolContext {
    pub workspace_root: PathBuf,
    pub timeout_secs: u64,
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
        match &self.status {
            ToolResultStatus::Ok => {
                envelope.insert("status".to_owned(), Value::String("ok".to_owned()));
                envelope.insert("content".to_owned(), Value::String(self.content.clone()));
            }
            ToolResultStatus::Error(error) => {
                envelope.insert("status".to_owned(), Value::String("error".to_owned()));
                envelope.insert("content".to_owned(), Value::String(self.content.clone()));
                envelope.insert("error".to_owned(), error.to_model_value());
            }
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
            object.insert("details".to_owned(), self.details.clone());
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
    Interrupted,
    ExitStatus,
    Io,
    Utf8,
    Network,
    Protocol,
    Unsupported,
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
            Self::Interrupted => "interrupted",
            Self::ExitStatus => "exit_status",
            Self::Io => "io",
            Self::Utf8 => "utf8",
            Self::Network => "network",
            Self::Protocol => "protocol",
            Self::Unsupported => "unsupported",
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
    #[serde(default)]
    pub details: Value,
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
            object.insert("details".to_owned(), self.details.clone());
        }
        (!object.is_empty()).then_some(Value::Object(object))
    }
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

#[async_trait]
pub trait Tool: Send + Sync {
    /// Returns the tool's stable contract and JSON Schema surface.
    fn spec(&self) -> ToolSpec;

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
        let tool = {
            let tools = match self.tools.read() {
                Ok(tools) => tools,
                Err(poisoned) => poisoned.into_inner(),
            };
            self.allowed_tool(&tools, &call.name)?
        };
        let args: Value = serde_json::from_str(&call.args_json)
            .map_err(|error| anyhow!("invalid tool args for {}: {error}", call.name))?;
        tool.execute(ctx, call.id, args).await
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

#[cfg(test)]
#[path = "tests/tool_tests.rs"]
mod tests;
