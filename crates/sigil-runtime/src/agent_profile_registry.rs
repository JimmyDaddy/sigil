use std::collections::BTreeSet;

use anyhow::{Context, Result};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    AgentProfile, AgentProfileId, AgentProfileKind, AgentProfileSnapshot, AgentProfileSnapshotId,
    AgentProfileSource, AgentRole, AgentTrustState, RootConfig, ToolRegistryScope,
};

use crate::{LOAD_SKILL_TOOL_NAME, provider_config_key};

pub const BUILD_PROFILE_ID: &str = "build";
pub const PLAN_PROFILE_ID: &str = "plan";
pub const EXPLORE_PROFILE_ID: &str = "explore";
pub const WORKER_PROFILE_ID: &str = "worker";

/// Resolved runtime profile plus discovery metadata that is not part of the durable kernel type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAgentProfile {
    pub profile: AgentProfile,
    pub enabled: bool,
    pub source: AgentProfileSource,
    pub source_hash: String,
    pub trust_state: AgentTrustState,
}

impl ResolvedAgentProfile {
    #[must_use]
    pub fn id(&self) -> &AgentProfileId {
        &self.profile.id
    }
}

/// Deterministic model-visible projection of trusted invocable agent profiles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelVisibleAgentIndex {
    pub entries: Vec<ModelVisibleAgentIndexEntry>,
    pub hidden_count: usize,
    pub fingerprint: String,
}

/// One bounded profile row visible to model-facing spawn-agent descriptions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelVisibleAgentIndexEntry {
    pub profile_id: AgentProfileId,
    pub kind: AgentProfileKind,
    pub description: String,
    pub tool_scope_hash: String,
}

/// Current run constraints used to filter the model-visible agent index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentProfileIndexContext {
    pub tool_scope: ToolRegistryScope,
    pub allowed_profile_ids: Option<BTreeSet<AgentProfileId>>,
    pub max_entries: Option<usize>,
}

impl Default for AgentProfileIndexContext {
    fn default() -> Self {
        Self {
            tool_scope: ToolRegistryScope {
                allow_all: true,
                ..ToolRegistryScope::default()
            },
            allowed_profile_ids: None,
            max_entries: None,
        }
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
                    user_invocable: true,
                    model_invocable: false,
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
                    user_invocable: true,
                    model_invocable: false,
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
                    user_invocable: true,
                    model_invocable: true,
                    nickname_candidates: &["Atlas", "Delta", "Echo"],
                },
            )?,
            builtin_profile(
                root_config,
                BuiltinProfileSpec {
                    id: WORKER_PROFILE_ID,
                    kind: AgentProfileKind::Subagent,
                    role: AgentRole::SubagentWrite,
                    description: "Foreground implementation worker agent.",
                    instructions: "Perform implementation work only through the guarded foreground path.",
                    enabled: root_config.task.enabled,
                    user_invocable: true,
                    model_invocable: false,
                    nickname_candidates: &["Patch", "Forge", "Quill"],
                },
            )?,
        ];
        profiles.sort_by(|left, right| left.profile.id.cmp(&right.profile.id));
        Ok(Self {
            profiles,
            warnings: Vec::new(),
        })
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

    /// Builds a deterministic model-visible index for the current run constraints.
    ///
    /// # Errors
    ///
    /// Returns an error if stable profile hashing fails.
    pub fn model_visible_index(
        &self,
        context: &AgentProfileIndexContext,
    ) -> Result<ModelVisibleAgentIndex> {
        let mut candidates = self
            .profiles
            .iter()
            .filter(|profile| profile.enabled)
            .filter(|profile| profile.trust_state == AgentTrustState::Trusted)
            .filter(|profile| profile.profile.model_invocable)
            .filter(|profile| profile_allowed_by_context(profile, context))
            .map(|profile| {
                Ok(ModelVisibleAgentIndexEntry {
                    profile_id: profile.profile.id.clone(),
                    kind: profile.profile.kind,
                    description: profile.profile.description.clone(),
                    tool_scope_hash: hash_json(&serde_json::to_value(
                        &profile.profile.tool_scope,
                    )?)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        candidates.sort_by(|left, right| left.profile_id.cmp(&right.profile_id));
        let total = candidates.len();
        if let Some(max_entries) = context.max_entries {
            candidates.truncate(max_entries);
        }
        let hidden_count = total.saturating_sub(candidates.len());
        let fingerprint = model_visible_fingerprint(&candidates, hidden_count, context)?;
        Ok(ModelVisibleAgentIndex {
            entries: candidates,
            hidden_count,
            fingerprint,
        })
    }
}

struct BuiltinProfileSpec<'a> {
    id: &'a str,
    kind: AgentProfileKind,
    role: AgentRole,
    description: &'a str,
    instructions: &'a str,
    enabled: bool,
    user_invocable: bool,
    model_invocable: bool,
    nickname_candidates: &'a [&'a str],
}

fn builtin_profile(
    root_config: &RootConfig,
    spec: BuiltinProfileSpec<'_>,
) -> Result<ResolvedAgentProfile> {
    let role_config = root_config.task.role_config(spec.role);
    let provider = role_config
        .provider
        .clone()
        .unwrap_or_else(|| root_config.agent.provider.clone());
    let model = role_config
        .model
        .clone()
        .unwrap_or_else(|| root_config.agent.model.clone());
    let tool_scope = role_tool_scope(root_config, spec.role);
    let profile = AgentProfile {
        id: AgentProfileId::new(spec.id)?,
        kind: spec.kind,
        description: spec.description.to_owned(),
        instructions: spec.instructions.to_owned(),
        model: Some(model),
        provider: Some(provider),
        reasoning_effort: role_config.reasoning_effort.clone(),
        tool_scope,
        permission_policy: root_config.permission.clone(),
        user_invocable: spec.user_invocable,
        model_invocable: spec.model_invocable,
        skills: Vec::new(),
        mcp_servers: Vec::new(),
        nickname_candidates: spec
            .nickname_candidates
            .iter()
            .map(|value| (*value).to_owned())
            .collect(),
    };
    Ok(ResolvedAgentProfile {
        source_hash: hash_json(&json!({
            "kind": "builtin_task_role",
            "profile": spec.id,
            "role": spec.role.as_str(),
            "provider": profile.provider.as_deref(),
            "provider_key": provider_config_key(profile.provider.as_deref().unwrap_or_default()),
            "model": profile.model.as_deref(),
            "reasoning_effort": profile.reasoning_effort.as_ref(),
            "tools": &role_config.tools,
            "allow_write_subagents": root_config.task.allow_write_subagents,
        }))?,
        profile,
        enabled: spec.enabled,
        source: AgentProfileSource::System,
        trust_state: AgentTrustState::Trusted,
    })
}

fn capture_profile_snapshot(profile: &ResolvedAgentProfile) -> Result<AgentProfileSnapshot> {
    let profile_hash = hash_json(&serde_json::to_value(&profile.profile)?)?;
    let tool_hash = hash_json(&serde_json::to_value(&profile.profile.tool_scope)?)?;
    let permission_hash = hash_json(&serde_json::to_value(&profile.profile.permission_policy)?)?;
    let mcp_hash = hash_json(&json!({ "servers": profile.profile.mcp_servers }))?;
    let skill_hashes = profile
        .profile
        .skills
        .iter()
        .map(|skill| hash_json(&json!({ "skill": skill })))
        .collect::<Result<Vec<_>>>()?;
    let snapshot_id = AgentProfileSnapshotId::new(format!(
        "snapshot_{}_{}",
        profile.profile.id.as_str(),
        short_hash(&profile_hash)
    ))?;
    Ok(AgentProfileSnapshot {
        snapshot_id,
        profile_id: profile.profile.id.clone(),
        source: profile.source.clone(),
        source_hash: profile.source_hash.clone(),
        profile_hash,
        resolved_tool_scope_hash: tool_hash,
        resolved_permission_policy_hash: permission_hash,
        resolved_mcp_scope_hash: mcp_hash,
        resolved_skill_hashes: skill_hashes,
        trust_state: profile.trust_state,
    })
}

fn profile_allowed_by_context(
    profile: &ResolvedAgentProfile,
    context: &AgentProfileIndexContext,
) -> bool {
    if let Some(allowed_profile_ids) = &context.allowed_profile_ids
        && !allowed_profile_ids.contains(&profile.profile.id)
    {
        return false;
    }
    tool_scope_contains(&context.tool_scope, &profile.profile.tool_scope)
}

fn tool_scope_contains(parent: &ToolRegistryScope, child: &ToolRegistryScope) -> bool {
    if parent.allow_all {
        return true;
    }
    if child.allow_all {
        return false;
    }
    child.names.iter().all(|name| parent.allows(name))
        && child.prefixes.iter().all(|prefix| parent.allows(prefix))
}

fn model_visible_fingerprint(
    entries: &[ModelVisibleAgentIndexEntry],
    hidden_count: usize,
    context: &AgentProfileIndexContext,
) -> Result<String> {
    let entries_json = entries
        .iter()
        .map(|entry| {
            json!({
                "profile_id": entry.profile_id.as_str(),
                "kind": entry.kind,
                "description": entry.description,
                "tool_scope_hash": entry.tool_scope_hash,
            })
        })
        .collect::<Vec<_>>();
    hash_json(&json!({
        "entries": entries_json,
        "hidden_count": hidden_count,
        "tool_scope": context.tool_scope,
        "allowed_profile_ids": context.allowed_profile_ids.as_ref().map(|ids| {
            ids.iter().map(|id| id.as_str().to_owned()).collect::<Vec<_>>()
        }),
    }))
}

fn role_tool_scope(root_config: &RootConfig, role: AgentRole) -> ToolRegistryScope {
    let configured = &root_config.task.role_config(role).tools;
    if !configured_allowlist_is_empty(configured) {
        return ToolRegistryScope {
            allow_all: configured.allow_all,
            names: configured.names.iter().cloned().collect(),
            prefixes: configured.prefixes.clone(),
        };
    }
    match role {
        AgentRole::Planner | AgentRole::SubagentRead => read_only_role_tool_scope(),
        AgentRole::Executor => ToolRegistryScope {
            allow_all: true,
            ..ToolRegistryScope::default()
        },
        AgentRole::SubagentWrite if root_config.task.allow_write_subagents => ToolRegistryScope {
            allow_all: true,
            ..ToolRegistryScope::default()
        },
        AgentRole::SubagentWrite => read_only_role_tool_scope(),
    }
}

fn configured_allowlist_is_empty(config: &sigil_kernel::ToolAllowlistConfig) -> bool {
    !config.allow_all && config.names.is_empty() && config.prefixes.is_empty()
}

fn read_only_role_tool_scope() -> ToolRegistryScope {
    ToolRegistryScope::from_names_and_prefixes(
        [
            "read_file",
            "ls",
            "glob",
            "grep",
            "code_symbols",
            "code_workspace_symbols",
            "code_definition",
            "code_references",
            "code_diagnostics",
            LOAD_SKILL_TOOL_NAME,
        ],
        std::iter::empty::<&str>(),
    )
}

fn hash_json(value: &Value) -> Result<String> {
    let bytes = serde_json::to_vec(value)?;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

fn short_hash(hash: &str) -> &str {
    hash.get(..12).unwrap_or(hash)
}

#[cfg(test)]
#[path = "tests/agent_profile_registry_tests.rs"]
mod tests;
