use anyhow::Result;
use serde_json::json;
use sigil_kernel::{
    AgentInvocationPolicy, AgentProfile, AgentProfileId, AgentProfileKind, AgentProfileSnapshot,
    AgentProfileSnapshotId, AgentProfileSource, AgentResultPolicy, AgentRole, AgentTrustState,
    RootConfig, ToolAllowlistConfig, ToolRegistryScope,
};

use crate::{LOAD_SKILL_TOOL_NAME, provider_config_key};

use super::{ResolvedAgentProfile, hash_json};

pub(super) struct BuiltinProfileSpec<'a> {
    pub(super) id: &'a str,
    pub(super) kind: AgentProfileKind,
    pub(super) role: AgentRole,
    pub(super) description: &'a str,
    pub(super) instructions: &'a str,
    pub(super) enabled: bool,
    pub(super) invocation_policy: AgentInvocationPolicy,
    pub(super) result_policy: AgentResultPolicy,
    pub(super) tool_scope_override: Option<ToolRegistryScope>,
    pub(super) nickname_candidates: &'a [&'a str],
}

pub(super) fn builtin_profile(
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
    let tool_scope = spec
        .tool_scope_override
        .unwrap_or_else(|| role_tool_scope(root_config, spec.role));
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
        invocation_policy: spec.invocation_policy,
        result_policy: spec.result_policy,
        user_invocable: spec.invocation_policy.default_user_invocable(),
        model_invocable: spec.invocation_policy.default_model_invocable(),
        skills: Vec::new(),
        mcp_servers: Vec::new(),
        nickname_candidates: spec
            .nickname_candidates
            .iter()
            .map(|value| (*value).to_owned())
            .collect(),
        aliases: Vec::new(),
        slash_names: Vec::new(),
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
        execution_role: spec.role,
        enabled: spec.enabled,
        enabled_override: None,
        user_invocable_override: None,
        model_invocable_override: None,
        source: AgentProfileSource::System,
        trust_state: AgentTrustState::Trusted,
    })
}

pub(super) fn capture_profile_snapshot(
    profile: &ResolvedAgentProfile,
) -> Result<AgentProfileSnapshot> {
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

fn configured_allowlist_is_empty(config: &ToolAllowlistConfig) -> bool {
    !config.allow_all && config.names.is_empty() && config.prefixes.is_empty()
}

pub(super) fn read_only_role_tool_scope() -> ToolRegistryScope {
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

fn short_hash(hash: &str) -> &str {
    hash.get(..12).unwrap_or(hash)
}
