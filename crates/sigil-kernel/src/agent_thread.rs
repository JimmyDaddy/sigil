use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, bail};
use serde::{Deserialize, Deserializer, Serialize};

use crate::{
    permission::PermissionConfig,
    provider::ReasoningEffort,
    session::{ControlEntry, SessionLogEntry},
    task::{
        AgentRole, SessionRef, TaskChildSessionDisplayNameEntry, TaskChildSessionEntry,
        TaskChildSessionStatus, TaskId, TaskStepId,
    },
    tool::ToolRegistryScope,
};

/// Stable identifier for an agent profile.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct AgentProfileId(String);

impl AgentProfileId {
    /// Creates a path-safe and control-log-safe profile identifier.
    ///
    /// # Errors
    ///
    /// Returns an error when `value` is empty, too long, or contains unstable characters.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        validate_stable_id("agent profile id", &value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Stable identifier for one concrete agent thread.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct AgentThreadId(String);

impl AgentThreadId {
    /// Creates a path-safe and control-log-safe thread identifier.
    ///
    /// # Errors
    ///
    /// Returns an error when `value` is empty, too long, or contains unstable characters.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        validate_stable_id("agent thread id", &value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Stable identifier for an immutable profile snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct AgentProfileSnapshotId(String);

impl AgentProfileSnapshotId {
    /// Creates a path-safe profile snapshot identifier.
    ///
    /// # Errors
    ///
    /// Returns an error when `value` is empty, too long, or contains unstable characters.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        validate_stable_id("agent profile snapshot id", &value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Stable identifier for an agent run attempt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct AgentRunAttemptId(String);

impl AgentRunAttemptId {
    /// Creates a path-safe run attempt identifier.
    ///
    /// # Errors
    ///
    /// Returns an error when `value` is empty, too long, or contains unstable characters.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        validate_stable_id("agent run attempt id", &value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Stable identifier for an approval, elicitation, or message route between agent threads.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct AgentRouteId(String);

impl AgentRouteId {
    /// Creates a path-safe route identifier.
    ///
    /// # Errors
    ///
    /// Returns an error when `value` is empty, too long, or contains unstable characters.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        validate_stable_id("agent route id", &value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Captured workspace root for one agent run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct WorkspaceRootSnapshot(String);

impl WorkspaceRootSnapshot {
    /// Creates a workspace-root snapshot suitable for durable audit records.
    ///
    /// # Errors
    ///
    /// Returns an error when `value` is empty or contains control characters.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.trim().is_empty() {
            bail!("workspace root snapshot cannot be empty");
        }
        if value.chars().any(char::is_control) {
            bail!("workspace root snapshot contains control characters");
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Provider-neutral kind for a runnable profile.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum AgentProfileKind {
    Primary,
    Subagent,
    System,
    #[serde(other)]
    Unknown,
}

/// Where an agent profile definition came from.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum AgentProfileSource {
    #[default]
    Workspace,
    User,
    Plugin {
        plugin_id: String,
    },
    Compatibility {
        provider: String,
    },
    System,
    LegacyTask,
    #[serde(other)]
    Unknown,
}

/// Trust state for one agent profile source and content hash.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTrustState {
    Trusted,
    #[default]
    NeedsReview,
    Disabled,
    #[serde(other)]
    Unknown,
}

/// Permission policy attached to a profile after runtime resolution.
pub type AgentPermissionPolicy = PermissionConfig;

/// Policy describing who may invoke an agent profile.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum AgentInvocationPolicy {
    /// Users may invoke the profile explicitly, but it is hidden from model-facing auto selection.
    ManualOnly,
    /// The profile may be shown to the model-facing agent index when trust and scope allow it.
    ModelAllowed,
    /// Internal profile that should not be exposed as a user or model invocation target.
    SystemOnly,
    #[serde(other)]
    Unknown,
}

impl AgentInvocationPolicy {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ManualOnly => "manual_only",
            Self::ModelAllowed => "model_allowed",
            Self::SystemOnly => "system_only",
            Self::Unknown => "unknown",
        }
    }

    #[must_use]
    pub fn from_invocability(user_invocable: bool, model_invocable: bool) -> Self {
        if model_invocable {
            Self::ModelAllowed
        } else if user_invocable {
            Self::ManualOnly
        } else {
            Self::SystemOnly
        }
    }

    #[must_use]
    pub fn default_user_invocable(self) -> bool {
        matches!(self, Self::ManualOnly | Self::ModelAllowed)
    }

    #[must_use]
    pub fn default_model_invocable(self) -> bool {
        matches!(self, Self::ModelAllowed)
    }
}

/// Policy describing how an agent profile returns or merges results.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum AgentResultPolicy {
    SummaryOnly,
    #[default]
    SummaryWithPageRef,
    ArtifactOnly,
    ForegroundMergeRequired,
    #[serde(other)]
    Unknown,
}

impl AgentResultPolicy {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SummaryOnly => "summary_only",
            Self::SummaryWithPageRef => "summary_with_page_ref",
            Self::ArtifactOnly => "artifact_only",
            Self::ForegroundMergeRequired => "foreground_merge_required",
            Self::Unknown => "unknown",
        }
    }
}

/// Runnable provider-neutral agent profile.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct AgentProfile {
    pub id: AgentProfileId,
    pub kind: AgentProfileKind,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub instructions: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    #[serde(default)]
    pub tool_scope: ToolRegistryScope,
    #[serde(default)]
    pub permission_policy: AgentPermissionPolicy,
    #[serde(default)]
    pub invocation_policy: AgentInvocationPolicy,
    #[serde(default)]
    pub result_policy: AgentResultPolicy,
    pub user_invocable: bool,
    pub model_invocable: bool,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub mcp_servers: Vec<String>,
    #[serde(default)]
    pub nickname_candidates: Vec<String>,
}

impl Default for AgentInvocationPolicy {
    fn default() -> Self {
        Self::ManualOnly
    }
}

impl AgentProfile {
    #[must_use]
    pub fn user_invocation_allowed(&self) -> bool {
        self.user_invocable
            && matches!(
                self.invocation_policy,
                AgentInvocationPolicy::ManualOnly | AgentInvocationPolicy::ModelAllowed
            )
    }

    #[must_use]
    pub fn model_invocation_allowed(&self) -> bool {
        self.model_invocable
            && matches!(self.invocation_policy, AgentInvocationPolicy::ModelAllowed)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
struct AgentProfileWire {
    id: AgentProfileId,
    kind: AgentProfileKind,
    #[serde(default)]
    description: String,
    #[serde(default)]
    instructions: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    reasoning_effort: Option<ReasoningEffort>,
    #[serde(default)]
    tool_scope: ToolRegistryScope,
    #[serde(default)]
    permission_policy: AgentPermissionPolicy,
    #[serde(default)]
    invocation_policy: Option<AgentInvocationPolicy>,
    #[serde(default)]
    result_policy: AgentResultPolicy,
    #[serde(default)]
    user_invocable: Option<bool>,
    #[serde(default)]
    model_invocable: Option<bool>,
    #[serde(default)]
    skills: Vec<String>,
    #[serde(default)]
    mcp_servers: Vec<String>,
    #[serde(default)]
    nickname_candidates: Vec<String>,
}

impl<'de> Deserialize<'de> for AgentProfile {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = AgentProfileWire::deserialize(deserializer)?;
        let invocation_policy = wire.invocation_policy.unwrap_or_else(|| {
            AgentInvocationPolicy::from_invocability(
                wire.user_invocable.unwrap_or(true),
                wire.model_invocable.unwrap_or(false),
            )
        });
        let user_invocable = wire
            .user_invocable
            .unwrap_or_else(|| invocation_policy.default_user_invocable());
        let model_invocable = wire
            .model_invocable
            .unwrap_or_else(|| invocation_policy.default_model_invocable());
        Ok(Self {
            id: wire.id,
            kind: wire.kind,
            description: wire.description,
            instructions: wire.instructions,
            model: wire.model,
            provider: wire.provider,
            reasoning_effort: wire.reasoning_effort,
            tool_scope: wire.tool_scope,
            permission_policy: wire.permission_policy,
            invocation_policy,
            result_policy: wire.result_policy,
            user_invocable,
            model_invocable,
            skills: wire.skills,
            mcp_servers: wire.mcp_servers,
            nickname_candidates: wire.nickname_candidates,
        })
    }
}

/// Immutable snapshot of profile source, trust, and resolved scopes for one run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct AgentProfileSnapshot {
    pub snapshot_id: AgentProfileSnapshotId,
    pub profile_id: AgentProfileId,
    #[serde(default)]
    pub source: AgentProfileSource,
    #[serde(default)]
    pub source_hash: String,
    #[serde(default)]
    pub profile_hash: String,
    #[serde(default)]
    pub resolved_tool_scope_hash: String,
    #[serde(default)]
    pub resolved_permission_policy_hash: String,
    #[serde(default)]
    pub resolved_mcp_scope_hash: String,
    #[serde(default)]
    pub resolved_skill_hashes: Vec<String>,
    #[serde(default)]
    pub trust_state: AgentTrustState,
}

/// Append-only trust review decision for one agent profile source and content hash.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct AgentProfileTrustEntry {
    pub profile_id: AgentProfileId,
    #[serde(default)]
    pub source: AgentProfileSource,
    #[serde(default)]
    pub source_hash: String,
    #[serde(default)]
    pub profile_hash: String,
    pub decision: AgentTrustState,
    pub reviewed_at_ms: u64,
}

impl AgentProfileTrustEntry {
    #[must_use]
    pub fn matches_snapshot(&self, snapshot: &AgentProfileSnapshot) -> bool {
        self.profile_id == snapshot.profile_id
            && self.source == snapshot.source
            && self.source_hash == snapshot.source_hash
            && self.profile_hash == snapshot.profile_hash
    }
}

/// Latest agent profile trust state reconstructed from append-only control entries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentProfileTrustProjection {
    pub trust_entries: BTreeMap<AgentProfileId, AgentProfileTrustEntry>,
    pub trust_replay_order: Vec<AgentProfileId>,
}

impl AgentProfileTrustProjection {
    #[must_use]
    pub fn from_entries(entries: &[SessionLogEntry]) -> Self {
        let mut projection = Self::default();
        for entry in entries {
            let SessionLogEntry::Control(ControlEntry::AgentProfileTrustDecision(entry)) = entry
            else {
                continue;
            };
            projection.apply_trust(entry);
        }
        projection
    }

    #[must_use]
    pub fn decision_for_snapshot(
        &self,
        snapshot: &AgentProfileSnapshot,
    ) -> Option<AgentTrustState> {
        self.trust_entries
            .get(&snapshot.profile_id)
            .and_then(|entry| entry.matches_snapshot(snapshot).then_some(entry.decision))
    }

    #[must_use]
    pub fn has_decision_for_profile(&self, profile_id: &AgentProfileId) -> bool {
        self.trust_entries.contains_key(profile_id)
    }

    fn apply_trust(&mut self, entry: &AgentProfileTrustEntry) {
        self.trust_replay_order.push(entry.profile_id.clone());
        self.trust_entries
            .insert(entry.profile_id.clone(), entry.clone());
    }
}

/// Append-only user policy override for one agent profile source and content hash.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct AgentProfilePolicyEntry {
    pub profile_id: AgentProfileId,
    #[serde(default)]
    pub source: AgentProfileSource,
    #[serde(default)]
    pub source_hash: String,
    #[serde(default)]
    pub profile_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_invocable: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_invocable: Option<bool>,
    pub reviewed_at_ms: u64,
}

impl AgentProfilePolicyEntry {
    #[must_use]
    pub fn matches_snapshot(&self, snapshot: &AgentProfileSnapshot) -> bool {
        self.profile_id == snapshot.profile_id
            && self.source == snapshot.source
            && self.source_hash == snapshot.source_hash
            && self.profile_hash == snapshot.profile_hash
    }
}

/// Latest agent profile policy overrides reconstructed from append-only control entries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentProfilePolicyProjection {
    pub policy_entries: BTreeMap<AgentProfileId, AgentProfilePolicyEntry>,
    pub policy_replay_order: Vec<AgentProfileId>,
}

impl AgentProfilePolicyProjection {
    #[must_use]
    pub fn from_entries(entries: &[SessionLogEntry]) -> Self {
        let mut projection = Self::default();
        for entry in entries {
            let SessionLogEntry::Control(ControlEntry::AgentProfilePolicyDecision(entry)) = entry
            else {
                continue;
            };
            projection.apply_policy(entry);
        }
        projection
    }

    #[must_use]
    pub fn policy_for_snapshot(
        &self,
        snapshot: &AgentProfileSnapshot,
    ) -> Option<&AgentProfilePolicyEntry> {
        self.policy_entries
            .get(&snapshot.profile_id)
            .filter(|entry| entry.matches_snapshot(snapshot))
    }

    #[must_use]
    pub fn has_policy_for_profile(&self, profile_id: &AgentProfileId) -> bool {
        self.policy_entries.contains_key(profile_id)
    }

    fn apply_policy(&mut self, entry: &AgentProfilePolicyEntry) {
        self.policy_replay_order.push(entry.profile_id.clone());
        self.policy_entries
            .insert(entry.profile_id.clone(), entry.clone());
    }
}

/// Immutable runtime context used to restore or audit one agent run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct AgentRunContextSnapshot {
    pub profile_snapshot_id: AgentProfileSnapshotId,
    pub provider: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    pub workspace_root: WorkspaceRootSnapshot,
    #[serde(default)]
    pub effective_tool_scope_hash: String,
    #[serde(default)]
    pub effective_permission_policy_hash: String,
    #[serde(default)]
    pub effective_mcp_scope_hash: String,
    #[serde(default)]
    pub provider_capability_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_visible_agent_index_hash: Option<String>,
    #[serde(default)]
    pub budget_policy_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_background_handle_ref: Option<String>,
}

/// Request to start one child or primary agent thread.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct AgentInvocationRequest {
    pub parent_session_ref: SessionRef,
    pub profile_id: AgentProfileId,
    pub objective: String,
    pub prompt: String,
    pub mode: AgentInvocationMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name_hint: Option<String>,
    pub created_from: AgentInvocationSource,
}

/// Runtime mode requested for a new agent thread.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentInvocationMode {
    Foreground,
    Background,
    JoinBeforeFinal,
    #[serde(other)]
    Unknown,
}

/// User or system surface that created an agent invocation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentInvocationSource {
    Chat,
    Mention,
    Skill,
    Task,
    Plugin,
    System,
    #[serde(other)]
    Unknown,
}

/// Durable lifecycle status for one agent thread.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentThreadStatus {
    Started,
    Running,
    Blocked,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
    Closed,
    Unavailable,
    #[serde(other)]
    Unknown,
}

impl AgentThreadStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed
                | Self::Failed
                | Self::Cancelled
                | Self::Interrupted
                | Self::Closed
                | Self::Unavailable
        )
    }
}

/// Terminal status stored in a bounded child result.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentThreadTerminalStatus {
    Completed,
    Failed,
    Cancelled,
    Interrupted,
    #[serde(other)]
    Unknown,
}

/// Durable route status for cross-thread messages, approvals, and elicitations.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentRouteStatus {
    Registered,
    Requested,
    Resolved,
    Rejected,
    Cancelled,
    Stale,
    Closed,
    #[serde(other)]
    Unknown,
}

impl AgentRouteStatus {
    fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Resolved | Self::Rejected | Self::Cancelled | Self::Stale | Self::Closed
        )
    }
}

/// Bounded usage summary for a completed child thread.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct AgentUsageSummary {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_tokens: Option<u64>,
}

/// Bounded artifact reference emitted by a child thread.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct AgentArtifactRef {
    pub kind: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
}

/// Structured result payload recorded when a child thread reaches a terminal state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct AgentThreadResult {
    pub thread_id: AgentThreadId,
    pub session_ref: SessionRef,
    pub status: AgentThreadTerminalStatus,
    pub summary: String,
    #[serde(default)]
    pub summary_truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_summary_chars: Option<usize>,
    #[serde(default)]
    pub artifacts: Vec<AgentArtifactRef>,
    #[serde(default)]
    pub changed_paths: Vec<String>,
    #[serde(default)]
    pub risks: Vec<String>,
    #[serde(default)]
    pub followups: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<AgentUsageSummary>,
    pub output_hash: String,
}

/// Append-only profile snapshot capture.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct AgentProfileCapturedEntry {
    pub snapshot: AgentProfileSnapshot,
}

/// Append-only start of one agent thread.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct AgentThreadStartedEntry {
    pub thread_id: AgentThreadId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_thread_id: Option<AgentThreadId>,
    pub parent_session_ref: SessionRef,
    pub thread_session_ref: SessionRef,
    pub profile_id: AgentProfileId,
    pub profile_snapshot_id: AgentProfileSnapshotId,
    pub run_context: AgentRunContextSnapshot,
    pub objective: String,
    #[serde(default)]
    pub prompt_hash: String,
    pub invocation_mode: AgentInvocationMode,
    pub invocation_source: AgentInvocationSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at_ms: Option<u64>,
}

/// Append-only status change for one agent thread.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct AgentThreadStatusChangedEntry {
    pub thread_id: AgentThreadId,
    pub status: AgentThreadStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at_ms: Option<u64>,
}

/// Append-only route for an explicit message or steering prompt between threads.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct AgentThreadMessageRoutedEntry {
    pub route_id: AgentRouteId,
    pub source_thread_id: AgentThreadId,
    pub target_thread_id: AgentThreadId,
    pub prompt_hash: String,
    pub status: AgentRouteStatus,
}

/// Append-only structured result for a terminal agent thread.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct AgentThreadResultRecordedEntry {
    pub result: AgentThreadResult,
}

/// Append-only presentation-only display name override.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct AgentThreadDisplayNameEntry {
    pub thread_id: AgentThreadId,
    pub display_name: String,
}

/// Append-only approval route from a source agent to a pending decision.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct AgentApprovalRouteEntry {
    pub route_id: AgentRouteId,
    pub source_thread_id: AgentThreadId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_thread_id: Option<AgentThreadId>,
    pub call_id: String,
    pub tool_name: String,
    pub status: AgentRouteStatus,
}

/// Append-only elicitation route from a source agent to a pending user input.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct AgentElicitationRouteEntry {
    pub route_id: AgentRouteId,
    pub source_thread_id: AgentThreadId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_thread_id: Option<AgentThreadId>,
    pub server_name: String,
    pub status: AgentRouteStatus,
}

/// Append-only start marker for one concrete provider attempt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct AgentRunAttemptStartedEntry {
    pub thread_id: AgentThreadId,
    pub attempt_id: AgentRunAttemptId,
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub background: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_background_handle_ref: Option<String>,
}

/// Append-only liveness marker for one provider attempt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct AgentRunHeartbeatEntry {
    pub thread_id: AgentThreadId,
    pub attempt_id: AgentRunAttemptId,
    pub updated_at_ms: u64,
}

/// Append-only terminal recovery marker for an unfinished attempt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct AgentRunInterruptedEntry {
    pub thread_id: AgentThreadId,
    pub attempt_id: AgentRunAttemptId,
    pub reason: String,
}

/// Append-only terminal recovery marker for a pending route.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct AgentRouteClosedEntry {
    pub route_id: AgentRouteId,
    pub reason: String,
}

/// Append-only marker that parent-visible result merging happened at a deterministic safe point.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct AgentMergeSafePointEntry {
    pub thread_id: AgentThreadId,
    pub parent_thread_id: AgentThreadId,
    pub result_hash: String,
}

/// Append-only close marker for hiding or archiving a terminal thread.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct AgentThreadClosedEntry {
    pub thread_id: AgentThreadId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Materialized agent-thread state reconstructed from append-only session entries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentThreadStateProjection {
    pub profiles: BTreeMap<AgentProfileSnapshotId, AgentProfileSnapshot>,
    pub threads: BTreeMap<AgentThreadId, AgentThreadProjection>,
    pub latest_thread_id: Option<AgentThreadId>,
    pub thread_replay_order: Vec<AgentThreadId>,
    pub approval_routes: BTreeMap<AgentRouteId, AgentApprovalRouteEntry>,
    pub elicitation_routes: BTreeMap<AgentRouteId, AgentElicitationRouteEntry>,
    pub message_routes: BTreeMap<AgentRouteId, AgentThreadMessageRoutedEntry>,
    pub closed_routes: BTreeMap<AgentRouteId, AgentRouteClosedEntry>,
    pub legacy_task_thread_ids: BTreeSet<AgentThreadId>,
}

impl AgentThreadStateProjection {
    /// Replays session entries into an agent thread projection.
    pub fn from_entries(entries: &[SessionLogEntry]) -> Self {
        let mut projection = Self::default();
        for entry in entries {
            let SessionLogEntry::Control(control) = entry else {
                continue;
            };
            projection.apply_control(control);
        }
        projection.finalize_replay();
        projection
    }

    pub fn latest_thread(&self) -> Option<&AgentThreadProjection> {
        self.latest_thread_id
            .as_ref()
            .and_then(|thread_id| self.threads.get(thread_id))
    }

    fn apply_control(&mut self, control: &ControlEntry) {
        match control {
            ControlEntry::AgentProfileCaptured(entry) => {
                self.profiles
                    .insert(entry.snapshot.snapshot_id.clone(), entry.snapshot.clone());
            }
            ControlEntry::AgentThreadStarted(entry) => self.apply_thread_started(entry),
            ControlEntry::AgentThreadStatusChanged(entry) => self.apply_status_changed(entry),
            ControlEntry::AgentThreadMessageRouted(entry) => {
                self.message_routes
                    .insert(entry.route_id.clone(), entry.clone());
            }
            ControlEntry::AgentThreadResultRecorded(entry) => {
                self.apply_result_recorded(&entry.result);
            }
            ControlEntry::AgentThreadDisplayName(entry) => self.apply_display_name(entry),
            ControlEntry::AgentApprovalRoute(entry) => {
                self.approval_routes
                    .insert(entry.route_id.clone(), entry.clone());
            }
            ControlEntry::AgentElicitationRoute(entry) => {
                self.elicitation_routes
                    .insert(entry.route_id.clone(), entry.clone());
            }
            ControlEntry::AgentRunAttemptStarted(entry) => {
                let thread = self.ensure_thread(&entry.thread_id);
                thread.attempts.insert(
                    entry.attempt_id.clone(),
                    AgentRunAttemptProjection {
                        attempt_id: entry.attempt_id.clone(),
                        provider: entry.provider.clone(),
                        model: entry.model.clone(),
                        background: entry.background,
                        provider_background_handle_ref: entry
                            .provider_background_handle_ref
                            .clone(),
                        interrupted: None,
                        last_heartbeat_ms: None,
                    },
                );
            }
            ControlEntry::AgentRunHeartbeat(entry) => {
                let thread = self.ensure_thread(&entry.thread_id);
                let attempt = thread.attempts.entry(entry.attempt_id.clone()).or_insert(
                    AgentRunAttemptProjection {
                        attempt_id: entry.attempt_id.clone(),
                        provider: String::new(),
                        model: String::new(),
                        background: false,
                        provider_background_handle_ref: None,
                        interrupted: None,
                        last_heartbeat_ms: None,
                    },
                );
                attempt.last_heartbeat_ms = Some(entry.updated_at_ms);
            }
            ControlEntry::AgentRunInterrupted(entry) => {
                let thread = self.ensure_thread(&entry.thread_id);
                let attempt = thread.attempts.entry(entry.attempt_id.clone()).or_insert(
                    AgentRunAttemptProjection {
                        attempt_id: entry.attempt_id.clone(),
                        provider: String::new(),
                        model: String::new(),
                        background: false,
                        provider_background_handle_ref: None,
                        interrupted: None,
                        last_heartbeat_ms: None,
                    },
                );
                attempt.interrupted = Some(entry.reason.clone());
                if !thread.status.is_terminal() {
                    thread.status = AgentThreadStatus::Interrupted;
                    thread.reason = Some(entry.reason.clone());
                }
            }
            ControlEntry::AgentRouteClosed(entry) => {
                self.closed_routes
                    .insert(entry.route_id.clone(), entry.clone());
            }
            ControlEntry::AgentMergeSafePoint(entry) => {
                let thread = self.ensure_thread(&entry.thread_id);
                thread.merge_safe_points.push(entry.clone());
            }
            ControlEntry::AgentThreadClosed(entry) => {
                let thread = self.ensure_thread(&entry.thread_id);
                thread.status = AgentThreadStatus::Closed;
                thread.reason = entry.reason.clone();
                thread.closed = true;
            }
            ControlEntry::TaskChildSession(entry) => self.apply_legacy_child_session(entry),
            ControlEntry::TaskChildSessionDisplayName(entry) => {
                self.apply_legacy_child_display_name(entry);
            }
            _ => {}
        }
    }

    fn apply_thread_started(&mut self, entry: &AgentThreadStartedEntry) {
        self.record_thread_replay(&entry.thread_id);
        let thread = self
            .threads
            .entry(entry.thread_id.clone())
            .or_insert_with(|| AgentThreadProjection::from_started(entry));
        let was_unresolved = thread.unresolved;
        thread.parent_thread_id = entry.parent_thread_id.clone();
        thread.parent_session_ref = Some(entry.parent_session_ref.clone());
        thread.thread_session_ref = Some(entry.thread_session_ref.clone());
        thread.profile_id = Some(entry.profile_id.clone());
        thread.profile_snapshot_id = Some(entry.profile_snapshot_id.clone());
        thread.run_context = Some(entry.run_context.clone());
        thread.objective = entry.objective.clone();
        thread.prompt_hash = entry.prompt_hash.clone();
        thread.invocation_mode = Some(entry.invocation_mode);
        thread.invocation_source = Some(entry.invocation_source);
        if let Some(display_name) = &entry.display_name {
            thread.display_name = Some(display_name.clone());
        }
        thread.unresolved = false;
        thread.profile_snapshot_missing = false;
        thread.profile_snapshot_mismatch = false;
        thread.reason = None;
        if was_unresolved || !thread.status.is_terminal() {
            thread.status = AgentThreadStatus::Started;
        }
    }

    fn apply_status_changed(&mut self, entry: &AgentThreadStatusChangedEntry) {
        self.record_thread_replay(&entry.thread_id);
        let thread = self.ensure_thread(&entry.thread_id);
        if thread.unresolved {
            thread.reason = entry
                .reason
                .clone()
                .or_else(|| Some("agent thread start entry missing".to_owned()));
            return;
        }
        if thread.status.is_terminal() && entry.status != thread.status {
            thread.duplicate_terminal_entries += usize::from(entry.status.is_terminal());
            return;
        }
        thread.status = entry.status;
        thread.reason = entry.reason.clone();
    }

    fn apply_result_recorded(&mut self, result: &AgentThreadResult) {
        self.record_thread_replay(&result.thread_id);
        let thread = self.ensure_thread(&result.thread_id);
        thread.thread_session_ref = Some(result.session_ref.clone());
        thread.result = Some(result.clone());
        if thread.unresolved {
            thread.reason = Some("agent thread start entry missing".to_owned());
            return;
        }
        thread.status = match result.status {
            AgentThreadTerminalStatus::Completed => AgentThreadStatus::Completed,
            AgentThreadTerminalStatus::Failed => AgentThreadStatus::Failed,
            AgentThreadTerminalStatus::Cancelled => AgentThreadStatus::Cancelled,
            AgentThreadTerminalStatus::Interrupted => AgentThreadStatus::Interrupted,
            AgentThreadTerminalStatus::Unknown => AgentThreadStatus::Unavailable,
        };
    }

    fn apply_display_name(&mut self, entry: &AgentThreadDisplayNameEntry) {
        self.record_thread_replay(&entry.thread_id);
        let thread = self.ensure_thread(&entry.thread_id);
        thread.display_name = Some(entry.display_name.clone());
    }

    fn apply_legacy_child_session(&mut self, entry: &TaskChildSessionEntry) {
        let thread_id = legacy_task_agent_thread_id(
            &entry.task_id,
            entry.plan_version,
            &entry.step_id,
            &entry.child_task_id,
        );
        self.legacy_task_thread_ids.insert(thread_id.clone());
        self.record_thread_replay(&thread_id);
        let thread = self
            .threads
            .entry(thread_id.clone())
            .or_insert_with(|| AgentThreadProjection::legacy_from_task_child(&thread_id, entry));
        thread.legacy_task = true;
        thread.invocation_source = Some(AgentInvocationSource::Task);
        thread.thread_session_ref = Some(entry.child_session_ref.clone());
        thread.profile_id = Some(legacy_profile_id_for_role(entry.role));
        thread.objective = format!(
            "legacy task {} step {}",
            entry.task_id.as_str(),
            entry.step_id.as_str()
        );
        thread.unresolved = false;
        thread.status = match entry.status {
            TaskChildSessionStatus::Started => AgentThreadStatus::Started,
            TaskChildSessionStatus::Completed => AgentThreadStatus::Completed,
            TaskChildSessionStatus::Failed => AgentThreadStatus::Failed,
            TaskChildSessionStatus::Cancelled => AgentThreadStatus::Cancelled,
            TaskChildSessionStatus::Interrupted => AgentThreadStatus::Interrupted,
            TaskChildSessionStatus::Unavailable => AgentThreadStatus::Unavailable,
        };
    }

    fn apply_legacy_child_display_name(&mut self, entry: &TaskChildSessionDisplayNameEntry) {
        let thread_id = legacy_task_agent_thread_id(
            &entry.task_id,
            entry.plan_version,
            &entry.step_id,
            &entry.child_task_id,
        );
        self.legacy_task_thread_ids.insert(thread_id.clone());
        let thread = self.ensure_thread(&thread_id);
        thread.legacy_task = true;
        thread.invocation_source = Some(AgentInvocationSource::Task);
        thread.display_name = Some(entry.display_name.clone());
    }

    fn ensure_thread(&mut self, thread_id: &AgentThreadId) -> &mut AgentThreadProjection {
        self.threads
            .entry(thread_id.clone())
            .or_insert_with(|| AgentThreadProjection::placeholder(thread_id.clone()))
    }

    fn record_thread_replay(&mut self, thread_id: &AgentThreadId) {
        self.latest_thread_id = Some(thread_id.clone());
        self.thread_replay_order.push(thread_id.clone());
    }

    fn finalize_replay(&mut self) {
        for thread in self.threads.values_mut() {
            if thread.legacy_task || thread.unresolved {
                continue;
            }
            let Some(profile_snapshot_id) = &thread.profile_snapshot_id else {
                thread.profile_snapshot_missing = true;
                thread.status = AgentThreadStatus::Unavailable;
                thread.reason = Some("agent profile snapshot reference missing".to_owned());
                continue;
            };
            let run_context_matches_snapshot = match &thread.run_context {
                Some(context) => &context.profile_snapshot_id == profile_snapshot_id,
                None => false,
            };
            if !run_context_matches_snapshot {
                thread.profile_snapshot_mismatch = true;
                thread.status = AgentThreadStatus::Unavailable;
                thread.reason = Some("agent profile snapshot mismatch".to_owned());
                continue;
            }
            if !self.profiles.contains_key(profile_snapshot_id) {
                thread.profile_snapshot_missing = true;
                thread.status = AgentThreadStatus::Unavailable;
                thread.reason = Some("agent profile snapshot missing".to_owned());
            }
        }
        self.apply_closed_route_states();
    }

    fn apply_closed_route_states(&mut self) {
        for route_id in self.closed_routes.keys() {
            if let Some(route) = self.approval_routes.get_mut(route_id) {
                route.status = AgentRouteStatus::Closed;
            }
            if let Some(route) = self.elicitation_routes.get_mut(route_id) {
                route.status = AgentRouteStatus::Closed;
            }
            if let Some(route) = self.message_routes.get_mut(route_id) {
                route.status = AgentRouteStatus::Closed;
            }
        }
    }
}

/// Projection for one agent thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentThreadProjection {
    pub thread_id: AgentThreadId,
    pub parent_thread_id: Option<AgentThreadId>,
    pub parent_session_ref: Option<SessionRef>,
    pub thread_session_ref: Option<SessionRef>,
    pub profile_id: Option<AgentProfileId>,
    pub profile_snapshot_id: Option<AgentProfileSnapshotId>,
    pub run_context: Option<AgentRunContextSnapshot>,
    pub objective: String,
    pub prompt_hash: String,
    pub invocation_mode: Option<AgentInvocationMode>,
    pub invocation_source: Option<AgentInvocationSource>,
    pub display_name: Option<String>,
    pub status: AgentThreadStatus,
    pub reason: Option<String>,
    pub result: Option<AgentThreadResult>,
    pub attempts: BTreeMap<AgentRunAttemptId, AgentRunAttemptProjection>,
    pub merge_safe_points: Vec<AgentMergeSafePointEntry>,
    pub duplicate_terminal_entries: usize,
    pub legacy_task: bool,
    pub closed: bool,
    pub unresolved: bool,
    pub profile_snapshot_missing: bool,
    pub profile_snapshot_mismatch: bool,
}

impl AgentThreadProjection {
    fn from_started(entry: &AgentThreadStartedEntry) -> Self {
        Self {
            thread_id: entry.thread_id.clone(),
            parent_thread_id: entry.parent_thread_id.clone(),
            parent_session_ref: Some(entry.parent_session_ref.clone()),
            thread_session_ref: Some(entry.thread_session_ref.clone()),
            profile_id: Some(entry.profile_id.clone()),
            profile_snapshot_id: Some(entry.profile_snapshot_id.clone()),
            run_context: Some(entry.run_context.clone()),
            objective: entry.objective.clone(),
            prompt_hash: entry.prompt_hash.clone(),
            invocation_mode: Some(entry.invocation_mode),
            invocation_source: Some(entry.invocation_source),
            display_name: entry.display_name.clone(),
            status: AgentThreadStatus::Started,
            reason: None,
            result: None,
            attempts: BTreeMap::new(),
            merge_safe_points: Vec::new(),
            duplicate_terminal_entries: 0,
            legacy_task: false,
            closed: false,
            unresolved: false,
            profile_snapshot_missing: false,
            profile_snapshot_mismatch: false,
        }
    }

    fn legacy_from_task_child(thread_id: &AgentThreadId, entry: &TaskChildSessionEntry) -> Self {
        Self {
            thread_id: thread_id.clone(),
            parent_thread_id: None,
            parent_session_ref: None,
            thread_session_ref: Some(entry.child_session_ref.clone()),
            profile_id: Some(legacy_profile_id_for_role(entry.role)),
            profile_snapshot_id: None,
            run_context: None,
            objective: format!(
                "legacy task {} step {}",
                entry.task_id.as_str(),
                entry.step_id.as_str()
            ),
            prompt_hash: String::new(),
            invocation_mode: Some(AgentInvocationMode::Foreground),
            invocation_source: Some(AgentInvocationSource::Task),
            display_name: None,
            status: AgentThreadStatus::Started,
            reason: None,
            result: None,
            attempts: BTreeMap::new(),
            merge_safe_points: Vec::new(),
            duplicate_terminal_entries: 0,
            legacy_task: true,
            closed: false,
            unresolved: false,
            profile_snapshot_missing: false,
            profile_snapshot_mismatch: false,
        }
    }

    fn placeholder(thread_id: AgentThreadId) -> Self {
        Self {
            thread_id,
            parent_thread_id: None,
            parent_session_ref: None,
            thread_session_ref: None,
            profile_id: None,
            profile_snapshot_id: None,
            run_context: None,
            objective: String::new(),
            prompt_hash: String::new(),
            invocation_mode: None,
            invocation_source: None,
            display_name: None,
            status: AgentThreadStatus::Unavailable,
            reason: Some("agent thread start entry missing".to_owned()),
            result: None,
            attempts: BTreeMap::new(),
            merge_safe_points: Vec::new(),
            duplicate_terminal_entries: 0,
            legacy_task: false,
            closed: false,
            unresolved: true,
            profile_snapshot_missing: false,
            profile_snapshot_mismatch: false,
        }
    }
}

/// Projection for one provider run attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRunAttemptProjection {
    pub attempt_id: AgentRunAttemptId,
    pub provider: String,
    pub model: String,
    pub background: bool,
    pub provider_background_handle_ref: Option<String>,
    pub interrupted: Option<String>,
    pub last_heartbeat_ms: Option<u64>,
}

/// Returns recovery entries for agent attempts that were started but never reached a terminal
/// attempt/thread state.
pub fn interrupted_agent_attempts(entries: &[SessionLogEntry]) -> Vec<AgentRunInterruptedEntry> {
    let mut started =
        BTreeMap::<(AgentThreadId, AgentRunAttemptId), AgentRunAttemptStartedEntry>::new();
    let mut terminal = BTreeSet::<(AgentThreadId, AgentRunAttemptId)>::new();
    let mut terminal_threads = BTreeSet::<AgentThreadId>::new();

    for entry in entries {
        let SessionLogEntry::Control(control) = entry else {
            continue;
        };
        match control {
            ControlEntry::AgentRunAttemptStarted(entry) => {
                started.insert(
                    (entry.thread_id.clone(), entry.attempt_id.clone()),
                    entry.clone(),
                );
            }
            ControlEntry::AgentRunInterrupted(entry) => {
                terminal.insert((entry.thread_id.clone(), entry.attempt_id.clone()));
            }
            ControlEntry::AgentThreadResultRecorded(entry) => {
                terminal_threads.insert(entry.result.thread_id.clone());
            }
            ControlEntry::AgentThreadStatusChanged(entry) if entry.status.is_terminal() => {
                terminal_threads.insert(entry.thread_id.clone());
            }
            ControlEntry::AgentThreadClosed(entry) => {
                terminal_threads.insert(entry.thread_id.clone());
            }
            _ => {}
        }
    }

    started
        .into_iter()
        .filter_map(|((thread_id, attempt_id), _)| {
            (!terminal.contains(&(thread_id.clone(), attempt_id.clone()))
                && !terminal_threads.contains(&thread_id))
            .then_some(AgentRunInterruptedEntry {
                thread_id,
                attempt_id,
                reason: "agent run interrupted during session restore".to_owned(),
            })
        })
        .collect()
}

/// Returns recovery entries for routes that were left non-terminal across process restart.
pub fn closed_agent_routes(entries: &[SessionLogEntry]) -> Vec<AgentRouteClosedEntry> {
    let mut statuses = BTreeMap::<AgentRouteId, AgentRouteStatus>::new();
    let mut already_closed = BTreeSet::<AgentRouteId>::new();
    for entry in entries {
        let SessionLogEntry::Control(control) = entry else {
            continue;
        };
        match control {
            ControlEntry::AgentApprovalRoute(entry) => {
                statuses.insert(entry.route_id.clone(), entry.status);
            }
            ControlEntry::AgentElicitationRoute(entry) => {
                statuses.insert(entry.route_id.clone(), entry.status);
            }
            ControlEntry::AgentThreadMessageRouted(entry) => {
                statuses.insert(entry.route_id.clone(), entry.status);
            }
            ControlEntry::AgentRouteClosed(entry) => {
                already_closed.insert(entry.route_id.clone());
            }
            _ => {}
        }
    }
    statuses
        .into_iter()
        .filter_map(|(route_id, status)| {
            (!status.is_terminal() && !already_closed.contains(&route_id)).then_some(
                AgentRouteClosedEntry {
                    route_id,
                    reason: "agent route closed during session restore".to_owned(),
                },
            )
        })
        .collect()
}

fn legacy_task_agent_thread_id(
    task_id: &TaskId,
    plan_version: u32,
    step_id: &TaskStepId,
    child_task_id: &TaskId,
) -> AgentThreadId {
    AgentThreadId::new(format!(
        "legacy_{}_v{}_{}_{}",
        task_id.as_str(),
        plan_version,
        step_id.as_str(),
        child_task_id.as_str()
    ))
    .expect("task ids are stable ids")
}

fn legacy_profile_id_for_role(role: AgentRole) -> AgentProfileId {
    let suffix = match role {
        AgentRole::Planner => "planner",
        AgentRole::Executor => "executor",
        AgentRole::SubagentRead => "subagent_read",
        AgentRole::SubagentWrite => "subagent_write",
    };
    AgentProfileId::new(format!("legacy_{suffix}")).expect("static profile id is valid")
}

fn validate_stable_id(label: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        bail!("{label} cannot be empty");
    }
    if value.len() > 96 {
        bail!("{label} is too long");
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        bail!("{label} contains unsupported characters");
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/agent_thread_tests.rs"]
mod tests;
