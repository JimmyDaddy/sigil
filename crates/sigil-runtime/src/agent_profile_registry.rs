use std::path::Path;

use anyhow::{Context, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sigil_kernel::{
    AgentInvocationPolicy, AgentProfile, AgentProfileId, AgentProfileKind,
    AgentProfilePolicyProjection, AgentProfileSnapshot, AgentProfileSource,
    AgentProfileTrustProjection, AgentResultPolicy, AgentRole, AgentTrustState, RootConfig,
    SessionLogEntry, changeset_only_child_tool_scope,
};

mod builtin;
mod discovery;
mod index;
mod names;
mod paths;
mod profiles;
mod wire;
use builtin::{
    BuiltinProfileSpec, builtin_profile, capture_profile_snapshot, read_only_role_tool_scope,
};
use discovery::{
    discover_child_session_skill_profiles, discover_plugin_agent_profiles,
    discover_workspace_agent_profiles,
};
use index::build_model_visible_index;
pub use index::{AgentProfileIndexContext, ModelVisibleAgentIndex, ModelVisibleAgentIndexEntry};
use names::{disable_conflicting_profile_names, normalize_profile_name_list};
#[cfg(test)]
use paths::configured_dir;
use paths::{
    agent_profile_source_label, display_path, path_stays_in_workspace, sorted_dir_entries,
    workspace_path,
};
#[cfg(test)]
use profiles::{NativeAgentProfileFormat, namespaced_plugin_agent_profile_id};
use profiles::{
    child_session_skill_profile, fallback_plugin_agent_id, native_agent_entrypoint,
    plugin_agent_profile_format, plugin_agent_profile_from_raw, tool_scope_is_empty,
    workspace_agent_profile_from_raw,
};
#[cfg(test)]
use wire::{
    markdown_agent_profile_wire, markdown_body_without_frontmatter, parse_agent_kind, parse_bool,
    parse_invocation_policy, parse_reasoning_effort, parse_result_policy, parse_trust_state,
};

pub const BUILD_PROFILE_ID: &str = "build";
pub const PLAN_PROFILE_ID: &str = "plan";
pub const EXPLORE_PROFILE_ID: &str = "explore";
pub const WORKER_PROFILE_ID: &str = "worker";

/// Resolved runtime profile plus discovery metadata that is not part of the durable kernel type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAgentProfile {
    pub profile: AgentProfile,
    pub execution_role: AgentRole,
    pub enabled: bool,
    pub enabled_override: Option<bool>,
    pub user_invocable_override: Option<bool>,
    pub model_invocable_override: Option<bool>,
    pub source: AgentProfileSource,
    pub source_hash: String,
    pub trust_state: AgentTrustState,
}

impl ResolvedAgentProfile {
    #[must_use]
    pub fn id(&self) -> &AgentProfileId {
        &self.profile.id
    }

    #[must_use]
    pub fn effective_enabled(&self) -> bool {
        self.enabled_override.unwrap_or(self.enabled)
    }

    #[must_use]
    pub fn effective_user_invocation_allowed(&self) -> bool {
        self.user_invocable_override
            .unwrap_or_else(|| self.profile.user_invocation_allowed())
    }

    #[must_use]
    pub fn effective_model_invocation_allowed(&self) -> bool {
        self.model_invocable_override
            .unwrap_or_else(|| self.profile.model_invocation_allowed())
    }
}

/// Deterministic runtime registry for built-in and discovered agent profiles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentProfileRegistry {
    profiles: Vec<ResolvedAgentProfile>,
    warnings: Vec<String>,
}

impl AgentProfileRegistry {
    /// Builds the first runtime profile registry from existing task role configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if a built-in profile id or captured profile hash cannot be produced.
    pub fn from_root_config(root_config: &RootConfig) -> Result<Self> {
        Self::from_root_config_inner(root_config, None, None)
    }

    /// Builds the registry and overlays durable profile trust decisions from session entries.
    ///
    /// # Errors
    ///
    /// Returns an error if profile discovery, stable hashing, or trust projection application fails.
    pub fn from_root_config_with_entries(
        root_config: &RootConfig,
        entries: &[SessionLogEntry],
    ) -> Result<Self> {
        let mut registry = Self::from_root_config_inner(root_config, None, Some(entries))?;
        registry.apply_session_entry_projections(entries)?;
        Ok(registry)
    }

    /// Builds the runtime profile registry from config plus native workspace agent profiles.
    ///
    /// Native workspace profiles are read from the configured `.sigil/agents` directory using the
    /// caller-resolved workspace root. This keeps profile discovery out of config parsing and makes
    /// path trust checks explicit at the runtime boundary.
    ///
    /// # Errors
    ///
    /// Returns an error if built-in profile construction or stable hashing fails.
    pub fn from_root_config_with_workspace(
        root_config: &RootConfig,
        workspace_root: &Path,
    ) -> Result<Self> {
        Self::from_root_config_inner(root_config, Some(workspace_root), None)
    }

    /// Builds the workspace-aware registry and overlays durable profile trust decisions.
    ///
    /// # Errors
    ///
    /// Returns an error if profile discovery, stable hashing, or trust projection application fails.
    pub fn from_root_config_with_workspace_and_entries(
        root_config: &RootConfig,
        workspace_root: &Path,
        entries: &[SessionLogEntry],
    ) -> Result<Self> {
        let mut registry =
            Self::from_root_config_inner(root_config, Some(workspace_root), Some(entries))?;
        registry.apply_session_entry_projections(entries)?;
        Ok(registry)
    }

    fn from_root_config_inner(
        root_config: &RootConfig,
        workspace_root: Option<&Path>,
        entries: Option<&[SessionLogEntry]>,
    ) -> Result<Self> {
        let mut profiles = vec![
            builtin_profile(
                root_config,
                BuiltinProfileSpec {
                    id: BUILD_PROFILE_ID,
                    kind: AgentProfileKind::Primary,
                    role: AgentRole::Executor,
                    description: "Default coding agent.",
                    instructions: "Handle ordinary coding tasks in the current session.",
                    enabled: true,
                    invocation_policy: AgentInvocationPolicy::ManualOnly,
                    result_policy: AgentResultPolicy::SummaryWithPageRef,
                    tool_scope_override: None,
                    nickname_candidates: &[],
                },
            )?,
            builtin_profile(
                root_config,
                BuiltinProfileSpec {
                    id: PLAN_PROFILE_ID,
                    kind: AgentProfileKind::Primary,
                    role: AgentRole::Planner,
                    description: "Analysis-first planning agent.",
                    instructions: "Research and plan before execution; do not edit files directly.",
                    enabled: root_config.task.enabled,
                    invocation_policy: AgentInvocationPolicy::ManualOnly,
                    result_policy: AgentResultPolicy::SummaryOnly,
                    tool_scope_override: None,
                    nickname_candidates: &[],
                },
            )?,
            builtin_profile(
                root_config,
                BuiltinProfileSpec {
                    id: EXPLORE_PROFILE_ID,
                    kind: AgentProfileKind::Subagent,
                    role: AgentRole::SubagentRead,
                    description: "Read-only codebase exploration and verification agent.",
                    instructions: "Inspect the repository with read-only tools and return a bounded result.",
                    enabled: root_config.task.enabled,
                    invocation_policy: AgentInvocationPolicy::ModelAllowed,
                    result_policy: AgentResultPolicy::SummaryWithPageRef,
                    tool_scope_override: None,
                    nickname_candidates: &["Atlas", "Delta", "Echo"],
                },
            )?,
            builtin_profile(
                root_config,
                BuiltinProfileSpec {
                    id: WORKER_PROFILE_ID,
                    kind: AgentProfileKind::Subagent,
                    role: AgentRole::SubagentWrite,
                    description: "Changeset-only foreground implementation worker agent.",
                    instructions: "Propose implementation changes through the guarded changeset-only foreground path. Do not claim changes were applied until the parent accepts and applies the merge review.",
                    enabled: root_config.task.enabled,
                    invocation_policy: AgentInvocationPolicy::ModelAllowed,
                    result_policy: AgentResultPolicy::ForegroundMergeRequired,
                    tool_scope_override: Some(changeset_only_child_tool_scope()),
                    nickname_candidates: &["Patch", "Forge", "Quill"],
                },
            )?,
        ];
        let mut warnings = Vec::new();
        if let Some(workspace_root) = workspace_root {
            discover_workspace_agent_profiles(
                root_config,
                workspace_root,
                &mut profiles,
                &mut warnings,
            )?;
            discover_plugin_agent_profiles(
                root_config,
                workspace_root,
                entries.unwrap_or(&[]),
                &mut profiles,
                &mut warnings,
            )?;
            discover_child_session_skill_profiles(
                root_config,
                workspace_root,
                &mut profiles,
                &mut warnings,
            )?;
        }
        profiles.sort_by(|left, right| left.profile.id.cmp(&right.profile.id));
        disable_conflicting_profile_names(&mut profiles, &mut warnings);
        Ok(Self { profiles, warnings })
    }

    #[must_use]
    pub fn profiles(&self) -> &[ResolvedAgentProfile] {
        &self.profiles
    }

    #[must_use]
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    #[must_use]
    pub fn get(&self, profile_id: &AgentProfileId) -> Option<&ResolvedAgentProfile> {
        self.profiles
            .iter()
            .find(|profile| &profile.profile.id == profile_id)
    }

    /// Captures an immutable snapshot for a resolved profile.
    ///
    /// # Errors
    ///
    /// Returns an error if stable JSON hashing or id validation fails.
    pub fn capture_snapshot(&self, profile_id: &AgentProfileId) -> Result<AgentProfileSnapshot> {
        let resolved = self
            .get(profile_id)
            .with_context(|| format!("agent profile {} is not registered", profile_id.as_str()))?;
        capture_profile_snapshot(resolved)
    }

    /// Applies durable trust decisions to non-system profiles.
    ///
    /// A matching source/profile hash replays the reviewed decision. If an older decision exists for
    /// the same profile id but the current snapshot no longer matches, the profile returns to review
    /// state so source edits cannot silently retain stale trust.
    ///
    /// # Errors
    ///
    /// Returns an error if a profile snapshot cannot be captured for stable hash comparison.
    pub fn apply_profile_trust_projection(
        &mut self,
        projection: &AgentProfileTrustProjection,
    ) -> Result<()> {
        for profile in &mut self.profiles {
            if profile.source == AgentProfileSource::System {
                continue;
            }
            let snapshot = capture_profile_snapshot(profile)?;
            if let Some(decision) = projection.decision_for_snapshot(&snapshot) {
                profile.trust_state = decision;
            } else if projection.has_decision_for_profile(&profile.profile.id) {
                profile.trust_state = AgentTrustState::NeedsReview;
            }
        }
        Ok(())
    }

    /// Applies durable user policy overrides to non-system profiles.
    ///
    /// Matching source/profile hashes replay the user's enabled/user/model invocability decisions
    /// without mutating the source [`AgentProfile`]. Stale decisions are cleared so profile edits do
    /// not silently retain old policy.
    ///
    /// # Errors
    ///
    /// Returns an error if a profile snapshot cannot be captured for stable hash comparison.
    pub fn apply_profile_policy_projection(
        &mut self,
        projection: &AgentProfilePolicyProjection,
    ) -> Result<()> {
        for profile in &mut self.profiles {
            if profile.source == AgentProfileSource::System {
                continue;
            }
            let snapshot = capture_profile_snapshot(profile)?;
            if let Some(policy) = projection.policy_for_snapshot(&snapshot) {
                profile.enabled_override = policy.enabled;
                profile.user_invocable_override = policy.user_invocable;
                profile.model_invocable_override = policy.model_invocable;
            } else if projection.has_policy_for_profile(&profile.profile.id) {
                profile.enabled_override = None;
                profile.user_invocable_override = None;
                profile.model_invocable_override = None;
            }
        }
        Ok(())
    }

    fn apply_session_entry_projections(&mut self, entries: &[SessionLogEntry]) -> Result<()> {
        let trust_projection = AgentProfileTrustProjection::from_entries(entries);
        self.apply_profile_trust_projection(&trust_projection)?;
        let policy_projection = AgentProfilePolicyProjection::from_entries(entries);
        self.apply_profile_policy_projection(&policy_projection)
    }

    /// Builds a deterministic model-visible index for the current run constraints.
    ///
    /// # Errors
    ///
    /// Returns an error if stable profile hashing fails.
    pub fn model_visible_index(
        &self,
        context: &AgentProfileIndexContext,
    ) -> Result<ModelVisibleAgentIndex> {
        build_model_visible_index(&self.profiles, context)
    }
}

fn hash_json(value: &Value) -> Result<String> {
    let bytes = serde_json::to_vec(value)?;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
#[path = "tests/agent_profile_registry_tests.rs"]
mod tests;
