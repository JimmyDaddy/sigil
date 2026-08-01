use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::execution_backend::{
    ExecutionBackendCapabilities, ExecutionBackendKind, ExecutionCleanupReceipt,
    ExecutionCleanupStatus, ExecutionSandboxProfile,
};
use crate::session::{ControlEntry, SessionLogEntry};

/// Current append-only terminal lifecycle schema.
pub const TERMINAL_TASK_SCHEMA_VERSION: u32 = 2;

/// Stable identifier for one local terminal task.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct TerminalTaskId(String);

impl TerminalTaskId {
    /// Creates a path-safe terminal task identifier.
    ///
    /// # Errors
    ///
    /// Returns an error when `value` is empty or contains path separators or unstable characters.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        validate_stable_id("terminal task id", &value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for TerminalTaskId {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// Durable handle for one Sigil-owned terminal task.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct TerminalTaskHandle {
    pub task_id: TerminalTaskId,
    pub command_sha256: String,
    pub cwd_label: String,
    pub shell_label: String,
    pub shell_sha256: String,
    pub log_ref: String,
    pub created_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_backend: Option<TerminalExecutionBackendKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_backend_capabilities: Option<TerminalExecutionBackendCapabilities>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enforcement_backend: Option<ExecutionBackendKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enforcement_backend_capabilities: Option<ExecutionBackendCapabilities>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_profile: Option<ExecutionSandboxProfile>,
}

/// Terminal execution backend used for a persistent terminal task.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TerminalExecutionBackendKind {
    LocalProcess,
    LocalPty,
    SandboxedPty,
}

impl TerminalExecutionBackendKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LocalProcess => "local_process",
            Self::LocalPty => "local_pty",
            Self::SandboxedPty => "sandboxed_pty",
        }
    }
}

/// Capability summary for one terminal execution backend.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TerminalExecutionBackendCapabilities {
    pub persistent_pty: bool,
    pub input: bool,
    pub resize: bool,
    pub cancel: bool,
    pub output_log: bool,
}

impl TerminalExecutionBackendCapabilities {
    #[must_use]
    pub fn local_process() -> Self {
        Self {
            persistent_pty: false,
            input: false,
            resize: false,
            cancel: true,
            output_log: true,
        }
    }

    #[must_use]
    pub fn local_pty() -> Self {
        Self {
            persistent_pty: true,
            input: true,
            resize: true,
            cancel: true,
            output_log: true,
        }
    }

    #[must_use]
    pub fn sandboxed_pty() -> Self {
        Self::local_pty()
    }
}

/// Durable lifecycle status for one terminal task.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TerminalTaskStatus {
    Starting,
    Running,
    Exited {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
    },
    Failed {
        reason: String,
    },
    Cancelled,
    Interrupted,
}

/// Readiness probe family configured for one persistent terminal task.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TerminalReadinessKind {
    None,
    OutputContains,
    OutputRegex,
}

/// Latest bounded readiness fact published by the terminal task owner.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TerminalReadinessStatus {
    None,
    Waiting {
        kind: TerminalReadinessKind,
    },
    Ready {
        kind: TerminalReadinessKind,
        ready_at_ms: u64,
    },
    Failed {
        kind: TerminalReadinessKind,
        reason: String,
    },
    TimedOut {
        kind: TerminalReadinessKind,
    },
}

impl TerminalReadinessStatus {
    #[must_use]
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::None | Self::Ready { .. })
    }

    #[must_use]
    pub fn is_waiting(&self) -> bool {
        matches!(self, Self::Waiting { .. })
    }
}

/// Bounded lifecycle wake emitted by the live terminal task owner.
///
/// The event intentionally excludes command text, output bodies, and machine-local paths. The
/// owner's exact snapshot remains authoritative; consumers use `(task_id, generation)` only to
/// deduplicate wakes and detect gaps.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TerminalLifecycleEvent {
    pub task_id: TerminalTaskId,
    /// Execution transport selected for the exact owner generation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_backend: Option<TerminalExecutionBackendKind>,
    /// Effective sandbox profile bound before the process was spawned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_profile: Option<ExecutionSandboxProfile>,
    pub generation: u64,
    pub status: TerminalTaskStatus,
    pub readiness: TerminalReadinessStatus,
    pub total_output_bytes: u64,
    pub emitted_at_ms: u64,
}

/// Exact owner snapshot paired with the bounded wake that caused it to be read.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TerminalLifecycleUpdateV2 {
    pub event: TerminalLifecycleEvent,
    pub task: TerminalTaskEntry,
}

/// Run-scoped route for terminal lifecycle durability and application projection.
///
/// Implementations must persist the exact owner snapshot before publishing the bounded event.
/// The route is held by the task observer, so it may outlive the foreground model turn without
/// relying on a process-global registry.
#[async_trait]
pub trait TerminalLifecycleSink: Send + Sync + std::fmt::Debug {
    /// Persists and projects one generation update.
    async fn publish(&self, update: TerminalLifecycleUpdateV2) -> Result<()>;
}

/// Product-surface factory for an exact session/run terminal lifecycle route.
///
/// The terminal tool resolves this factory from the immutable [`crate::ToolContext`] at start
/// time. This prevents a process-global "current run" pointer from assigning a background task to
/// whichever concurrent run happened to bind most recently.
pub trait TerminalLifecycleSinkFactory: Send + Sync + std::fmt::Debug {
    /// Freezes one durable route for a terminal task started by the exact logical run.
    fn sink_for_run(
        &self,
        session_scope_id: &str,
        logical_run_id: &str,
        recorder: crate::MutationEventRecorder,
    ) -> Result<Arc<dyn TerminalLifecycleSink>>;
}

/// Stable reason why terminal output capture ended before a complete artifact was available.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TerminalOutputTerminationReason {
    OutputLimitExceeded,
    OutputCaptureFailed,
    OutputDrainTimeout,
}

impl TerminalOutputTerminationReason {
    /// Returns the stable wire and diagnostic code for this termination reason.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OutputLimitExceeded => "output_limit_exceeded",
            Self::OutputCaptureFailed => "output_capture_failed",
            Self::OutputDrainTimeout => "output_drain_timeout",
        }
    }
}

impl TerminalTaskStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Exited { .. } => "exited",
            Self::Failed { .. } => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }

    pub fn is_active(&self) -> bool {
        matches!(self, Self::Starting | Self::Running)
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Exited { .. } | Self::Failed { .. } | Self::Cancelled | Self::Interrupted
        )
    }
}

/// Append-only control entry for one terminal task lifecycle update.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct TerminalTaskEntry {
    /// Current durable terminal lifecycle schema.
    pub schema_version: u32,
    pub handle: TerminalTaskHandle,
    /// Monotone owner generation used to deduplicate wakes and detect gaps.
    pub generation: u64,
    pub status: TerminalTaskStatus,
    /// Latest readiness fact published by the exact task owner.
    pub readiness: TerminalReadinessStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_preview: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_hash: Option<String>,
    pub output_truncated: bool,
    /// Bytes observed by the streaming collectors before EOF or a terminal capture failure.
    ///
    /// This can exceed the retained artifact size when a hard limit terminates the process tree.
    pub output_total_bytes: u64,
    /// Hard artifact limit that caused termination, if output collection crossed one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_limit_bytes: Option<u64>,
    /// Stable reason why output collection ended before a complete artifact was available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_termination_reason: Option<TerminalOutputTerminationReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup: Option<ExecutionCleanupReceipt>,
    pub updated_at_ms: u64,
}

impl TerminalTaskEntry {
    /// Returns a bounded, path-safe projection suitable for the session JSONL and task metadata.
    ///
    /// Live owners may retain richer process state separately. Output bodies, raw commands and
    /// machine-local paths are deliberately absent from this value.
    pub fn durable_projection(&self) -> Result<Self> {
        let mut projected = self.clone();
        projected.output_preview = None;
        projected.status = bounded_terminal_status(&self.status);
        projected.readiness = bounded_terminal_readiness(&self.readiness);
        projected.cleanup = self
            .cleanup
            .as_ref()
            .map(|cleanup| ExecutionCleanupReceipt {
                status: cleanup.status,
                reason: cleanup
                    .reason
                    .as_deref()
                    .map(|reason| bounded_terminal_text(reason, MAX_TERMINAL_REASON_BYTES)),
            });
        projected.validate_durable()?;
        Ok(projected)
    }

    /// Validates the bounded, privacy-safe durable terminal-task contract.
    ///
    /// # Errors
    ///
    /// Returns an error when the entry contains a raw or unbounded field, an invalid
    /// digest, an unsafe path label, or exceeds the serialized durable-entry budget.
    pub fn validate_durable(&self) -> Result<()> {
        if self.schema_version != TERMINAL_TASK_SCHEMA_VERSION {
            bail!(
                "unsupported terminal task schema version {}",
                self.schema_version
            );
        }
        validate_terminal_handle(&self.handle)?;
        validate_terminal_status(&self.status)?;
        validate_terminal_readiness(&self.readiness)?;
        if self.output_preview.is_some() {
            bail!("durable terminal task must not contain an output preview");
        }
        if let Some(hash) = &self.output_hash {
            validate_terminal_sha256("terminal output hash", hash)?;
        }
        if let Some(cleanup) = &self.cleanup
            && let Some(reason) = &cleanup.reason
            && (reason.len() > MAX_TERMINAL_REASON_BYTES
                || crate::safe_persistence_text(reason) != *reason)
        {
            bail!("terminal cleanup reason is not bounded safe text");
        }
        let bytes = serde_json::to_vec(self).context("failed to size terminal task entry")?;
        if bytes.len() > MAX_DURABLE_TERMINAL_TASK_BYTES {
            bail!(
                "durable terminal task exceeds maximum of {} bytes",
                MAX_DURABLE_TERMINAL_TASK_BYTES
            );
        }
        Ok(())
    }

    /// Projects a terminal task control entry from terminal tool metadata.
    ///
    /// Returns `Ok(None)` for non-terminal tool metadata.
    ///
    /// # Errors
    ///
    /// Returns an error when terminal metadata is present but incomplete or malformed.
    pub fn from_tool_result_details(details: &Value) -> Result<Option<Self>> {
        let Some(object) = details.as_object() else {
            return Ok(None);
        };
        if !object.contains_key("status_detail") {
            return Ok(None);
        }

        let task_id = TerminalTaskId::new(required_string(details, "task_id")?.to_owned())?;
        let status = serde_json::from_value::<TerminalTaskStatus>(
            required_value(details, "status_detail")?.clone(),
        )
        .map_err(|error| anyhow!("invalid terminal task status_detail: {error}"))?;
        let entry = Self {
            schema_version: required_u64(details, "schema_version")?
                .try_into()
                .map_err(|_| anyhow!("terminal task schema_version exceeds u32"))?,
            handle: TerminalTaskHandle {
                task_id,
                command_sha256: required_string(details, "command_sha256")?.to_owned(),
                cwd_label: required_string(details, "cwd_label")?.to_owned(),
                shell_label: required_string(details, "shell_label")?.to_owned(),
                shell_sha256: required_string(details, "shell_sha256")?.to_owned(),
                log_ref: required_string(details, "log_ref")?.to_owned(),
                created_at_ms: required_u64(details, "created_at_ms")?,
                execution_backend: optional_value(details, "execution_backend")
                    .map(|value| serde_json::from_value(value.clone()))
                    .transpose()
                    .map_err(|error| anyhow!("invalid terminal task execution_backend: {error}"))?,
                execution_backend_capabilities: optional_value(
                    details,
                    "execution_backend_capabilities",
                )
                .map(|value| serde_json::from_value(value.clone()))
                .transpose()
                .map_err(|error| {
                    anyhow!("invalid terminal task execution_backend_capabilities: {error}")
                })?,
                enforcement_backend: optional_value(details, "enforcement_backend")
                    .map(|value| serde_json::from_value(value.clone()))
                    .transpose()
                    .map_err(|error| {
                        anyhow!("invalid terminal task enforcement_backend: {error}")
                    })?,
                enforcement_backend_capabilities: optional_value(
                    details,
                    "enforcement_backend_capabilities",
                )
                .map(|value| serde_json::from_value(value.clone()))
                .transpose()
                .map_err(|error| {
                    anyhow!("invalid terminal task enforcement_backend_capabilities: {error}")
                })?,
                sandbox_profile: optional_value(details, "sandbox_profile")
                    .map(|value| serde_json::from_value(value.clone()))
                    .transpose()
                    .map_err(|error| anyhow!("invalid terminal task sandbox_profile: {error}"))?,
            },
            generation: required_u64(details, "generation")?,
            status,
            readiness: serde_json::from_value(required_value(details, "readiness")?.clone())
                .map_err(|error| anyhow!("invalid terminal task readiness: {error}"))?,
            output_preview: optional_string(details, "output_preview").map(str::to_owned),
            output_hash: optional_string(details, "output_hash").map(str::to_owned),
            output_truncated: details
                .get("output_truncated")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            output_total_bytes: details
                .get("output_total_bytes")
                .and_then(Value::as_u64)
                .unwrap_or_default(),
            output_limit_bytes: details.get("output_limit_bytes").and_then(Value::as_u64),
            output_termination_reason: optional_value(details, "output_termination_reason")
                .map(|value| serde_json::from_value(value.clone()))
                .transpose()
                .map_err(|error| {
                    anyhow!("invalid terminal task output_termination_reason: {error}")
                })?,
            cleanup: optional_value(details, "cleanup")
                .map(|value| serde_json::from_value(value.clone()))
                .transpose()
                .map_err(|error| anyhow!("invalid terminal task cleanup: {error}"))?,
            updated_at_ms: required_u64(details, "updated_at_ms")?,
        };
        entry.durable_projection().map(Some)
    }
}

pub const MAX_DURABLE_TERMINAL_TASK_BYTES: usize = 16 * 1024;
pub const MAX_TERMINAL_CWD_LABEL_BYTES: usize = 256;
pub const MAX_TERMINAL_SHELL_LABEL_BYTES: usize = 64;
pub const MAX_TERMINAL_LOG_REF_BYTES: usize = 160;
pub const MAX_TERMINAL_REASON_BYTES: usize = 512;

fn validate_terminal_handle(handle: &TerminalTaskHandle) -> Result<()> {
    validate_terminal_sha256("terminal command hash", &handle.command_sha256)?;
    validate_terminal_relative_label("terminal cwd label", &handle.cwd_label)?;
    if handle.shell_label.is_empty()
        || handle.shell_label.len() > MAX_TERMINAL_SHELL_LABEL_BYTES
        || handle.shell_label.contains('/')
        || handle.shell_label.contains('\\')
        || crate::safe_persistence_text(&handle.shell_label) != handle.shell_label
    {
        bail!("terminal shell label is not bounded safe text");
    }
    validate_terminal_sha256("terminal shell hash", &handle.shell_sha256)?;
    if handle.log_ref.is_empty()
        || handle.log_ref.len() > MAX_TERMINAL_LOG_REF_BYTES
        || !handle
            .log_ref
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':'))
    {
        bail!("terminal log ref is not a bounded opaque identity");
    }
    Ok(())
}

fn validate_terminal_relative_label(label: &str, value: &str) -> Result<()> {
    let path = std::path::Path::new(value);
    if value.is_empty()
        || value.len() > MAX_TERMINAL_CWD_LABEL_BYTES
        || path.is_absolute()
        || value.contains('\\')
        || value.as_bytes().get(1).is_some_and(|byte| *byte == b':')
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
        || crate::safe_persistence_text(value) != value
    {
        bail!("{label} is invalid");
    }
    Ok(())
}

fn validate_terminal_sha256(label: &str, value: &str) -> Result<()> {
    let digest = value.strip_prefix("sha256:").unwrap_or(value);
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("{label} is not a sha256 digest");
    }
    Ok(())
}

fn bounded_terminal_status(status: &TerminalTaskStatus) -> TerminalTaskStatus {
    match status {
        TerminalTaskStatus::Failed { reason } => TerminalTaskStatus::Failed {
            reason: bounded_terminal_text(reason, MAX_TERMINAL_REASON_BYTES),
        },
        status => status.clone(),
    }
}

fn bounded_terminal_readiness(readiness: &TerminalReadinessStatus) -> TerminalReadinessStatus {
    match readiness {
        TerminalReadinessStatus::Failed { kind, reason } => TerminalReadinessStatus::Failed {
            kind: *kind,
            reason: bounded_terminal_text(reason, MAX_TERMINAL_REASON_BYTES),
        },
        readiness => readiness.clone(),
    }
}

fn validate_terminal_status(status: &TerminalTaskStatus) -> Result<()> {
    if let TerminalTaskStatus::Failed { reason } = status
        && (reason.len() > MAX_TERMINAL_REASON_BYTES
            || crate::safe_persistence_text(reason) != *reason)
    {
        bail!("terminal failure reason is not bounded safe text");
    }
    Ok(())
}

fn validate_terminal_readiness(readiness: &TerminalReadinessStatus) -> Result<()> {
    if let TerminalReadinessStatus::Failed { reason, .. } = readiness
        && (reason.len() > MAX_TERMINAL_REASON_BYTES
            || crate::safe_persistence_text(reason) != *reason)
    {
        bail!("terminal readiness reason is not bounded safe text");
    }
    Ok(())
}

fn bounded_terminal_text(value: &str, max_bytes: usize) -> String {
    let safe = crate::safe_persistence_text(value)
        .split_whitespace()
        .map(|token| {
            let lower = token.to_ascii_lowercase();
            if lower.contains("http://") || lower.contains("https://") {
                "[redacted-url]"
            } else {
                token
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    if safe.len() <= max_bytes {
        return safe;
    }
    let mut boundary = max_bytes;
    while boundary > 0 && !safe.is_char_boundary(boundary) {
        boundary -= 1;
    }
    safe[..boundary].to_owned()
}

/// Latest terminal task state reconstructed from append-only control entries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TerminalTaskProjection {
    pub tasks: BTreeMap<TerminalTaskId, TerminalTaskSummary>,
    pub latest_task_id: Option<TerminalTaskId>,
    pub active_task_ids: Vec<TerminalTaskId>,
    pub replay_order: Vec<TerminalTaskId>,
}

impl TerminalTaskProjection {
    /// Replays append-only session entries into the latest terminal task projection.
    pub fn from_entries(entries: &[SessionLogEntry]) -> Self {
        let mut projection = Self::default();
        for entry in entries {
            if let SessionLogEntry::Control(control) = entry {
                projection.apply_control_entry(control);
            }
        }
        projection.refresh_active_task_ids();
        projection
    }

    pub(crate) fn apply_control_entry(&mut self, control: &ControlEntry) {
        if let ControlEntry::TerminalTask(task_entry) = control {
            self.apply_entry(task_entry);
            self.refresh_active_task_ids();
        }
    }

    pub fn latest(&self) -> Option<&TerminalTaskSummary> {
        self.latest_task_id
            .as_ref()
            .and_then(|id| self.tasks.get(id))
    }

    /// Builds interrupted control entries for active tasks that no process manager can recover.
    ///
    /// Callers pass the task ids still known to a live process manager after restore. Running
    /// tasks missing from that set are interrupted immediately. Starting tasks are interrupted
    /// only after `starting_timeout_ms` has elapsed since their latest update.
    pub fn interrupted_entries_for_missing_processes(
        &self,
        live_task_ids: &BTreeSet<TerminalTaskId>,
        now_ms: u64,
        starting_timeout_ms: u64,
    ) -> Vec<TerminalTaskEntry> {
        self.tasks
            .values()
            .filter(|summary| {
                should_interrupt_missing_process(
                    summary,
                    live_task_ids,
                    now_ms,
                    starting_timeout_ms,
                )
            })
            .map(|summary| summary.interrupted_entry(now_ms))
            .collect()
    }

    fn apply_entry(&mut self, entry: &TerminalTaskEntry) {
        let id = entry.handle.task_id.clone();
        if self
            .tasks
            .get(&id)
            .is_some_and(|current| current.generation >= entry.generation)
        {
            return;
        }
        self.replay_order.push(id.clone());
        self.latest_task_id = Some(id.clone());
        self.tasks.insert(id, TerminalTaskSummary::from(entry));
    }

    fn refresh_active_task_ids(&mut self) {
        self.active_task_ids = self
            .tasks
            .iter()
            .filter_map(|(id, summary)| summary.status.is_active().then_some(id.clone()))
            .collect();
    }
}

/// Latest projected state for one terminal task id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalTaskSummary {
    pub schema_version: u32,
    pub handle: TerminalTaskHandle,
    pub generation: u64,
    pub status: TerminalTaskStatus,
    pub readiness: TerminalReadinessStatus,
    pub output_preview: Option<String>,
    pub output_hash: Option<String>,
    pub output_truncated: bool,
    /// Latest observed byte total reconstructed from the append-only terminal entry.
    pub output_total_bytes: u64,
    /// Hard artifact limit that caused termination, when applicable.
    pub output_limit_bytes: Option<u64>,
    /// Stable output termination reason, when collection did not complete normally.
    pub output_termination_reason: Option<TerminalOutputTerminationReason>,
    pub cleanup: Option<ExecutionCleanupReceipt>,
    pub updated_at_ms: u64,
}

impl TerminalTaskSummary {
    fn interrupted_entry(&self, updated_at_ms: u64) -> TerminalTaskEntry {
        TerminalTaskEntry {
            schema_version: self.schema_version,
            handle: self.handle.clone(),
            generation: self.generation.saturating_add(1),
            status: TerminalTaskStatus::Interrupted,
            readiness: interrupted_readiness(&self.readiness),
            output_preview: self.output_preview.clone(),
            output_hash: self.output_hash.clone(),
            output_truncated: self.output_truncated,
            output_total_bytes: self.output_total_bytes,
            output_limit_bytes: self.output_limit_bytes,
            output_termination_reason: self.output_termination_reason,
            cleanup: Some(ExecutionCleanupReceipt::unknown(
                "terminal task was interrupted before cleanup could be proven",
            )),
            updated_at_ms,
        }
    }
}

impl From<&TerminalTaskEntry> for TerminalTaskSummary {
    fn from(entry: &TerminalTaskEntry) -> Self {
        Self {
            schema_version: entry.schema_version,
            handle: entry.handle.clone(),
            generation: entry.generation,
            status: entry.status.clone(),
            readiness: entry.readiness.clone(),
            output_preview: entry.output_preview.clone(),
            output_hash: entry.output_hash.clone(),
            output_truncated: entry.output_truncated,
            output_total_bytes: entry.output_total_bytes,
            output_limit_bytes: entry.output_limit_bytes,
            output_termination_reason: entry.output_termination_reason,
            cleanup: entry.cleanup.clone(),
            updated_at_ms: entry.updated_at_ms,
        }
    }
}

fn interrupted_readiness(readiness: &TerminalReadinessStatus) -> TerminalReadinessStatus {
    match readiness {
        TerminalReadinessStatus::Waiting { kind } => TerminalReadinessStatus::Failed {
            kind: *kind,
            reason: "terminal owner was interrupted before readiness was resolved".to_owned(),
        },
        settled => settled.clone(),
    }
}

#[must_use]
pub fn terminal_cleanup_receipt_for_status(
    status: &TerminalTaskStatus,
) -> Option<ExecutionCleanupReceipt> {
    match status {
        TerminalTaskStatus::Starting | TerminalTaskStatus::Running => None,
        TerminalTaskStatus::Exited { .. } => Some(ExecutionCleanupReceipt::not_needed()),
        TerminalTaskStatus::Cancelled => Some(ExecutionCleanupReceipt::completed(
            "terminal process tree cancellation and reap were confirmed",
        )),
        TerminalTaskStatus::Interrupted => Some(ExecutionCleanupReceipt::unknown(
            "terminal process disappeared before cleanup could be proven",
        )),
        TerminalTaskStatus::Failed { reason } => {
            let status = if reason.contains("kill") || reason.contains("cancel") {
                ExecutionCleanupStatus::Failed
            } else {
                ExecutionCleanupStatus::Unknown
            };
            Some(ExecutionCleanupReceipt {
                status,
                reason: Some(reason.clone()),
            })
        }
    }
}

fn should_interrupt_missing_process(
    summary: &TerminalTaskSummary,
    live_task_ids: &BTreeSet<TerminalTaskId>,
    now_ms: u64,
    starting_timeout_ms: u64,
) -> bool {
    if live_task_ids.contains(&summary.handle.task_id) {
        return false;
    }

    match summary.status {
        TerminalTaskStatus::Running => true,
        TerminalTaskStatus::Starting => {
            now_ms.saturating_sub(summary.updated_at_ms) >= starting_timeout_ms
        }
        TerminalTaskStatus::Exited { .. }
        | TerminalTaskStatus::Failed { .. }
        | TerminalTaskStatus::Cancelled
        | TerminalTaskStatus::Interrupted => false,
    }
}

fn required_value<'a>(details: &'a Value, key: &str) -> Result<&'a Value> {
    details
        .get(key)
        .ok_or_else(|| anyhow!("missing terminal task field {key}"))
}

fn required_string<'a>(details: &'a Value, key: &str) -> Result<&'a str> {
    required_value(details, key)?
        .as_str()
        .ok_or_else(|| anyhow!("terminal task field {key} must be a string"))
}

fn optional_string<'a>(details: &'a Value, key: &str) -> Option<&'a str> {
    details.get(key).and_then(Value::as_str)
}

fn optional_value<'a>(details: &'a Value, key: &str) -> Option<&'a Value> {
    details.get(key).filter(|value| !value.is_null())
}

fn required_u64(details: &Value, key: &str) -> Result<u64> {
    required_value(details, key)?
        .as_u64()
        .ok_or_else(|| anyhow!("terminal task field {key} must be an unsigned integer"))
}

fn validate_stable_id(label: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        bail!("{label} cannot be empty");
    }
    if value == "." || value == ".." || value.contains('/') || value.contains('\\') {
        bail!("{label} must not contain path separators or traversal");
    }
    if !value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        bail!("{label} contains unsupported characters");
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/terminal_task_tests.rs"]
mod tests;
