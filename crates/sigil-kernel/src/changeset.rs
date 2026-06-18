use std::collections::BTreeMap;

use anyhow::{Result, bail};
use serde::{Deserialize, Deserializer, Serialize};

use crate::session::{ControlEntry, SessionLogEntry};

/// Stable identifier for one proposed change set.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct ChangeSetId(String);

impl ChangeSetId {
    /// Creates a path-safe change set identifier.
    ///
    /// # Errors
    ///
    /// Returns an error when `value` is empty or contains path separators or unstable characters.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        validate_stable_id("change set id", &value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for ChangeSetId {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// Proposed multi-file edit bundle shown to users before application.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ChangeSet {
    pub id: ChangeSetId,
    pub title: String,
    pub summary: String,
    pub risk: ChangeSetRisk,
    #[serde(default)]
    pub files: Vec<ChangeSetFile>,
    #[serde(default)]
    pub validations: Vec<ChangeSetValidation>,
}

/// One file touched by a proposed change set.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ChangeSetFile {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_path: Option<String>,
    pub action: ChangeSetFileAction,
    pub risk: ChangeSetRisk,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_hash: Option<String>,
    #[serde(default)]
    pub additions: u32,
    #[serde(default)]
    pub deletions: u32,
    #[serde(default)]
    pub validations: Vec<ChangeSetValidation>,
}

/// File operation represented by a change set file entry.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChangeSetFileAction {
    Create,
    Update,
    Delete,
    Rename,
}

impl ChangeSetFileAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Update => "update",
            Self::Delete => "delete",
            Self::Rename => "rename",
        }
    }
}

/// User-facing risk level for a proposed change set or file.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChangeSetRisk {
    Low,
    Medium,
    High,
}

impl ChangeSetRisk {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// Validation result captured before applying one change set.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ChangeSetValidation {
    pub kind: ChangeSetValidationKind,
    pub status: ChangeSetValidationStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Validation category used by changeset apply tooling.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChangeSetValidationKind {
    Path,
    Hash,
    Mtime,
    Snippet,
    Symlink,
    Binary,
    Permission,
    Custom,
}

impl ChangeSetValidationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Path => "path",
            Self::Hash => "hash",
            Self::Mtime => "mtime",
            Self::Snippet => "snippet",
            Self::Symlink => "symlink",
            Self::Binary => "binary",
            Self::Permission => "permission",
            Self::Custom => "custom",
        }
    }
}

/// Stable validation status for a proposed change set.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChangeSetValidationStatus {
    Pending,
    Passed,
    Failed,
    Skipped,
}

impl ChangeSetValidationStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
        }
    }
}

/// Durable result recorded after attempting to apply a change set.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ChangeSetResult {
    pub id: ChangeSetId,
    pub status: ChangeSetResultStatus,
    #[serde(default)]
    pub file_results: Vec<ChangeSetFileResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Per-file result recorded after a change set apply attempt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ChangeSetFileResult {
    pub path: String,
    pub action: ChangeSetFileAction,
    pub status: ChangeSetFileResultStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default)]
    pub validations: Vec<ChangeSetValidation>,
}

/// Overall result of a change set apply attempt.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChangeSetResultStatus {
    Applied,
    PartiallyApplied,
    Failed,
    Cancelled,
}

impl ChangeSetResultStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::PartiallyApplied => "partially_applied",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Per-file result status for a change set apply attempt.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChangeSetFileResultStatus {
    Applied,
    Skipped,
    Failed,
}

impl ChangeSetFileResultStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::Skipped => "skipped",
            Self::Failed => "failed",
        }
    }
}

/// Latest change set state reconstructed from append-only control entries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChangeSetProjection {
    pub changesets: BTreeMap<ChangeSetId, ChangeSetState>,
    pub latest_change_set_id: Option<ChangeSetId>,
    pub replay_order: Vec<ChangeSetId>,
}

impl ChangeSetProjection {
    /// Replays append-only session entries into the latest change set projection.
    pub fn from_entries(entries: &[SessionLogEntry]) -> Self {
        let mut projection = Self::default();
        for entry in entries {
            match entry {
                SessionLogEntry::Control(ControlEntry::ChangeSetProposed(change_set)) => {
                    projection.apply_proposed(change_set);
                }
                SessionLogEntry::Control(ControlEntry::ChangeSetApplied(result)) => {
                    projection.apply_result(result);
                }
                SessionLogEntry::User(_)
                | SessionLogEntry::Assistant(_)
                | SessionLogEntry::ToolResult(_)
                | SessionLogEntry::Control(_) => {}
            }
        }
        projection
    }

    pub fn latest(&self) -> Option<&ChangeSetState> {
        self.latest_change_set_id
            .as_ref()
            .and_then(|id| self.changesets.get(id))
    }

    fn apply_proposed(&mut self, change_set: &ChangeSet) {
        self.record_replay(&change_set.id);
        self.latest_change_set_id = Some(change_set.id.clone());
        let state = self
            .changesets
            .entry(change_set.id.clone())
            .or_insert_with(|| ChangeSetState::new(change_set.id.clone()));
        state.proposal = Some(change_set.clone());
        state.result = None;
    }

    fn apply_result(&mut self, result: &ChangeSetResult) {
        self.record_replay(&result.id);
        self.latest_change_set_id = Some(result.id.clone());
        let state = self
            .changesets
            .entry(result.id.clone())
            .or_insert_with(|| ChangeSetState::new(result.id.clone()));
        state.result = Some(result.clone());
    }

    fn record_replay(&mut self, id: &ChangeSetId) {
        self.replay_order.push(id.clone());
    }
}

/// Latest projected state for one change set id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeSetState {
    pub id: ChangeSetId,
    pub proposal: Option<ChangeSet>,
    pub result: Option<ChangeSetResult>,
}

impl ChangeSetState {
    fn new(id: ChangeSetId) -> Self {
        Self {
            id,
            proposal: None,
            result: None,
        }
    }
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
#[path = "tests/changeset_tests.rs"]
mod tests;
