use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::session::{ControlEntry, SessionLogEntry};

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
#[serde(rename_all = "snake_case")]
pub struct TerminalTaskHandle {
    pub task_id: TerminalTaskId,
    pub command: String,
    pub cwd: PathBuf,
    pub shell: String,
    pub log_path: PathBuf,
    pub created_at_ms: u64,
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
#[serde(rename_all = "snake_case")]
pub struct TerminalTaskEntry {
    pub handle: TerminalTaskHandle,
    pub status: TerminalTaskStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_preview: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_hash: Option<String>,
    #[serde(default)]
    pub output_truncated: bool,
    pub updated_at_ms: u64,
}

impl TerminalTaskEntry {
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
        let cwd = serde_json::from_value::<PathBuf>(required_value(details, "cwd")?.clone())
            .map_err(|error| anyhow!("invalid terminal task cwd: {error}"))?;
        let log_path =
            serde_json::from_value::<PathBuf>(required_value(details, "log_path")?.clone())
                .map_err(|error| anyhow!("invalid terminal task log_path: {error}"))?;

        Ok(Some(Self {
            handle: TerminalTaskHandle {
                task_id,
                command: required_string(details, "command")?.to_owned(),
                cwd,
                shell: required_string(details, "shell")?.to_owned(),
                log_path,
                created_at_ms: required_u64(details, "created_at_ms")?,
            },
            status,
            output_preview: optional_string(details, "output_preview").map(str::to_owned),
            output_hash: optional_string(details, "output_hash").map(str::to_owned),
            output_truncated: details
                .get("output_truncated")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            updated_at_ms: required_u64(details, "updated_at_ms")?,
        }))
    }
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
            match entry {
                SessionLogEntry::Control(ControlEntry::TerminalTask(task_entry)) => {
                    projection.apply_entry(task_entry);
                }
                SessionLogEntry::User(_)
                | SessionLogEntry::Assistant(_)
                | SessionLogEntry::ToolResult(_)
                | SessionLogEntry::Control(_) => {}
            }
        }
        projection.refresh_active_task_ids();
        projection
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
    pub handle: TerminalTaskHandle,
    pub status: TerminalTaskStatus,
    pub output_preview: Option<String>,
    pub output_hash: Option<String>,
    pub output_truncated: bool,
    pub updated_at_ms: u64,
}

impl TerminalTaskSummary {
    fn interrupted_entry(&self, updated_at_ms: u64) -> TerminalTaskEntry {
        TerminalTaskEntry {
            handle: self.handle.clone(),
            status: TerminalTaskStatus::Interrupted,
            output_preview: self.output_preview.clone(),
            output_hash: self.output_hash.clone(),
            output_truncated: self.output_truncated,
            updated_at_ms,
        }
    }
}

impl From<&TerminalTaskEntry> for TerminalTaskSummary {
    fn from(entry: &TerminalTaskEntry) -> Self {
        Self {
            handle: entry.handle.clone(),
            status: entry.status.clone(),
            output_preview: entry.output_preview.clone(),
            output_hash: entry.output_hash.clone(),
            output_truncated: entry.output_truncated,
            updated_at_ms: entry.updated_at_ms,
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
