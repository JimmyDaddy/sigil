use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde_json::json;
use sha2::{Digest, Sha256};
use sigil_kernel::{
    AgentInvocationPolicy, AgentProfile, AgentProfileId, AgentProfileKind, AgentProfileSource,
    AgentResultPolicy, AgentTrustState, RootConfig, SkillDescriptor, SkillSource, SkillTrustState,
    ToolRegistryScope,
};

use super::{
    ResolvedAgentProfile, display_path, hash_json, normalize_profile_name_list,
    read_only_role_tool_scope,
    wire::{
        NativeAgentProfileWire, markdown_agent_profile_wire, markdown_body_without_frontmatter,
    },
    workspace_path,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NativeAgentProfileFormat {
    Toml,
    Markdown,
}

pub(super) fn native_agent_entrypoint(dir: &Path) -> Option<(PathBuf, NativeAgentProfileFormat)> {
    let toml = dir.join("agent.toml");
    if toml.is_file() {
        return Some((toml, NativeAgentProfileFormat::Toml));
    }
    let markdown = dir.join("AGENT.md");
    markdown
        .is_file()
        .then_some((markdown, NativeAgentProfileFormat::Markdown))
}

pub(super) fn plugin_agent_profile_format(entrypoint: &Path) -> Result<NativeAgentProfileFormat> {
    match entrypoint.file_name().and_then(|name| name.to_str()) {
        Some("agent.toml") => Ok(NativeAgentProfileFormat::Toml),
        Some("AGENT.md") => Ok(NativeAgentProfileFormat::Markdown),
        Some(name) if name.ends_with(".toml") => Ok(NativeAgentProfileFormat::Toml),
        Some(name) if name.ends_with(".md") => Ok(NativeAgentProfileFormat::Markdown),
        _ => bail!("plugin agent path must point to agent.toml, AGENT.md, .toml, or .md"),
    }
}

pub(super) fn fallback_plugin_agent_id(path: &Path) -> Result<String> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    let fallback = if matches!(file_name, "agent.toml" | "AGENT.md") {
        path.parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .unwrap_or("")
    } else {
        path.file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("")
    };
    AgentProfileId::new(fallback.to_owned())
        .with_context(|| format!("invalid plugin agent fallback id {fallback:?}"))?;
    Ok(fallback.to_owned())
}

pub(super) fn namespaced_plugin_agent_profile_id(
    plugin_id: &str,
    local_id: &str,
) -> Result<AgentProfileId> {
    let plugin_segment = plugin_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    let candidate = format!("{plugin_segment}-{local_id}");
    if candidate.len() <= 96 {
        return AgentProfileId::new(candidate);
    }

    let hash = hash_bytes(format!("{plugin_id}\0{local_id}").as_bytes());
    let prefix = format!("plugin-{}-", &hash[..12]);
    let max_local_len = 96usize.saturating_sub(prefix.len());
    let local_part = &local_id[..local_id.len().min(max_local_len)];
    AgentProfileId::new(format!("{prefix}{local_part}"))
}

pub(super) fn workspace_agent_profile_from_raw(
    root_config: &RootConfig,
    workspace_root: &Path,
    root: &Path,
    entrypoint: &Path,
    fallback_id: &str,
    raw: &str,
    format: NativeAgentProfileFormat,
) -> Result<ResolvedAgentProfile> {
    let (wire, markdown_body) = match format {
        NativeAgentProfileFormat::Toml => (toml::from_str::<NativeAgentProfileWire>(raw)?, None),
        NativeAgentProfileFormat::Markdown => markdown_agent_profile_wire(raw)?,
    };
    let id = wire.id.as_deref().unwrap_or(fallback_id);
    if id != fallback_id {
        bail!("agent profile id {id:?} must match directory name {fallback_id:?}");
    }
    let profile_id = AgentProfileId::new(id.to_owned())?;
    let invocation_policy = wire
        .invocation_policy
        .unwrap_or(AgentInvocationPolicy::ManualOnly);
    let user_invocable = wire
        .user_invocable
        .unwrap_or_else(|| invocation_policy.default_user_invocable());
    let model_invocable = wire
        .model_invocable
        .unwrap_or_else(|| invocation_policy.default_model_invocable());
    let tool_scope = wire
        .tool_scope
        .or_else(|| {
            wire.allowed_tools
                .clone()
                .or_else(|| wire.tools.clone())
                .map(|tools| {
                    ToolRegistryScope::from_names_and_prefixes(tools, Vec::<String>::new())
                })
        })
        .unwrap_or_else(read_only_role_tool_scope);
    let instructions = wire
        .instructions
        .or(markdown_body)
        .unwrap_or_default()
        .trim()
        .to_owned();
    let aliases = normalize_profile_name_list(wire.aliases.unwrap_or_default(), "agent alias")?;
    let slash_names =
        normalize_profile_name_list(wire.slash_names.unwrap_or_default(), "agent slash name")?;
    let profile = AgentProfile {
        id: profile_id,
        kind: wire.kind.unwrap_or(AgentProfileKind::Subagent),
        description: wire.description.unwrap_or_default(),
        instructions,
        model: wire.model.or_else(|| Some(root_config.agent.model.clone())),
        provider: wire
            .provider
            .or_else(|| Some(root_config.agent.provider.clone())),
        reasoning_effort: wire.reasoning_effort,
        tool_scope,
        permission_policy: root_config.permission.clone(),
        invocation_policy,
        result_policy: wire.result_policy.unwrap_or_default(),
        user_invocable,
        model_invocable,
        skills: wire.skills.unwrap_or_default(),
        mcp_servers: wire.mcp_servers.unwrap_or_default(),
        nickname_candidates: wire.nickname_candidates.unwrap_or_default(),
        aliases,
        slash_names,
    };
    Ok(ResolvedAgentProfile {
        source_hash: hash_json(&json!({
            "kind": "workspace_agent_profile",
            "root": display_path(workspace_root, root),
            "entrypoint": display_path(workspace_root, entrypoint),
            "sha256": hash_bytes(raw.as_bytes()),
        }))?,
        profile,
        enabled: wire.enabled.unwrap_or(true),
        enabled_override: None,
        user_invocable_override: None,
        model_invocable_override: None,
        source: AgentProfileSource::Workspace,
        trust_state: wire.trust.or(wire.trust_state).unwrap_or_default(),
    })
}

pub(super) fn plugin_agent_profile_from_raw(
    root_config: &RootConfig,
    workspace_root: &Path,
    plugin_id: &str,
    plugin_root: &Path,
    entrypoint: &Path,
    fallback_id: &str,
    raw: &str,
    format: NativeAgentProfileFormat,
) -> Result<ResolvedAgentProfile> {
    let (wire, markdown_body) = match format {
        NativeAgentProfileFormat::Toml => (toml::from_str::<NativeAgentProfileWire>(raw)?, None),
        NativeAgentProfileFormat::Markdown => markdown_agent_profile_wire(raw)?,
    };
    let local_id = wire.id.as_deref().unwrap_or(fallback_id);
    if local_id != fallback_id {
        bail!("agent profile id {local_id:?} must match file-derived id {fallback_id:?}");
    }
    let profile_id = namespaced_plugin_agent_profile_id(plugin_id, local_id)?;
    let invocation_policy = wire
        .invocation_policy
        .unwrap_or(AgentInvocationPolicy::ManualOnly);
    let user_invocable = wire
        .user_invocable
        .unwrap_or_else(|| invocation_policy.default_user_invocable());
    let model_invocable = wire
        .model_invocable
        .unwrap_or_else(|| invocation_policy.default_model_invocable());
    let tool_scope = wire
        .tool_scope
        .or_else(|| {
            wire.allowed_tools
                .clone()
                .or_else(|| wire.tools.clone())
                .map(|tools| {
                    ToolRegistryScope::from_names_and_prefixes(tools, Vec::<String>::new())
                })
        })
        .unwrap_or_else(read_only_role_tool_scope);
    let instructions = wire
        .instructions
        .or(markdown_body)
        .unwrap_or_default()
        .trim()
        .to_owned();
    let aliases = normalize_profile_name_list(wire.aliases.unwrap_or_default(), "agent alias")?;
    let slash_names =
        normalize_profile_name_list(wire.slash_names.unwrap_or_default(), "agent slash name")?;
    let profile = AgentProfile {
        id: profile_id,
        kind: wire.kind.unwrap_or(AgentProfileKind::Subagent),
        description: wire.description.unwrap_or_default(),
        instructions,
        model: wire.model.or_else(|| Some(root_config.agent.model.clone())),
        provider: wire
            .provider
            .or_else(|| Some(root_config.agent.provider.clone())),
        reasoning_effort: wire.reasoning_effort,
        tool_scope,
        permission_policy: root_config.permission.clone(),
        invocation_policy,
        result_policy: wire.result_policy.unwrap_or_default(),
        user_invocable,
        model_invocable,
        skills: wire.skills.unwrap_or_default(),
        mcp_servers: wire.mcp_servers.unwrap_or_default(),
        nickname_candidates: wire.nickname_candidates.unwrap_or_default(),
        aliases,
        slash_names,
    };
    Ok(ResolvedAgentProfile {
        source_hash: hash_json(&json!({
            "kind": "plugin_agent_profile",
            "plugin_id": plugin_id,
            "root": display_path(workspace_root, plugin_root),
            "entrypoint": display_path(workspace_root, entrypoint),
            "sha256": hash_bytes(raw.as_bytes()),
        }))?,
        profile,
        enabled: wire.enabled.unwrap_or(true),
        enabled_override: None,
        user_invocable_override: None,
        model_invocable_override: None,
        source: AgentProfileSource::Plugin {
            plugin_id: plugin_id.to_owned(),
        },
        trust_state: wire.trust.or(wire.trust_state).unwrap_or_default(),
    })
}

pub(super) fn child_session_skill_profile(
    root_config: &RootConfig,
    workspace_root: &Path,
    descriptor: &SkillDescriptor,
) -> Result<ResolvedAgentProfile> {
    let profile_id = AgentProfileId::new(descriptor.id.clone())?;
    let invocation_policy = AgentInvocationPolicy::from_invocability(
        descriptor.user_invocable,
        descriptor.model_invocable,
    );
    let tool_scope = if tool_scope_is_empty(&descriptor.allowed_tools) {
        read_only_role_tool_scope()
    } else {
        descriptor.allowed_tools.clone()
    };
    let entrypoint = workspace_path(workspace_root, &descriptor.entrypoint);
    let raw = fs::read_to_string(&entrypoint).with_context(|| {
        format!(
            "failed to read child-session skill {}",
            entrypoint.display()
        )
    })?;
    let instructions = compatibility_skill_instructions(descriptor, &raw);
    let profile = AgentProfile {
        id: profile_id,
        kind: AgentProfileKind::Subagent,
        description: descriptor.description.clone(),
        instructions,
        model: Some(root_config.agent.model.clone()),
        provider: Some(root_config.agent.provider.clone()),
        reasoning_effort: None,
        tool_scope,
        permission_policy: root_config.permission.clone(),
        invocation_policy,
        result_policy: AgentResultPolicy::SummaryWithPageRef,
        user_invocable: descriptor.user_invocable,
        model_invocable: descriptor.model_invocable,
        skills: vec![descriptor.id.clone()],
        mcp_servers: Vec::new(),
        nickname_candidates: if descriptor.name.trim().is_empty() {
            Vec::new()
        } else {
            vec![descriptor.name.clone()]
        },
        aliases: Vec::new(),
        slash_names: Vec::new(),
    };
    Ok(ResolvedAgentProfile {
        source_hash: hash_json(&json!({
            "kind": "child_session_skill_agent_profile",
            "skill_id": descriptor.id,
            "entrypoint": display_path(workspace_root, &entrypoint),
            "sha256": descriptor.sha256,
            "run_as": descriptor.run_as.as_str(),
            "model_invocable": descriptor.model_invocable,
            "user_invocable": descriptor.user_invocable,
        }))?,
        profile,
        enabled: descriptor.enabled,
        enabled_override: None,
        user_invocable_override: None,
        model_invocable_override: None,
        source: agent_profile_source_from_skill(descriptor),
        trust_state: agent_trust_from_skill(descriptor.trust),
    })
}

fn compatibility_skill_instructions(descriptor: &SkillDescriptor, raw: &str) -> String {
    let mut parts = Vec::new();
    if !descriptor.description.trim().is_empty() {
        parts.push(format!("Description: {}", descriptor.description.trim()));
    }
    if let Some(when_to_use) = descriptor.when_to_use.as_deref()
        && !when_to_use.trim().is_empty()
    {
        parts.push(format!("When to use: {}", when_to_use.trim()));
    }
    let body = markdown_body_without_frontmatter(raw).trim().to_owned();
    if !body.is_empty() {
        parts.push(body);
    }
    parts.join("\n\n")
}

fn agent_profile_source_from_skill(descriptor: &SkillDescriptor) -> AgentProfileSource {
    let entrypoint = descriptor.entrypoint.to_string_lossy();
    if entrypoint.starts_with(".claude/") || entrypoint.starts_with(".claude\\") {
        return AgentProfileSource::Compatibility {
            provider: "claude".to_owned(),
        };
    }
    if entrypoint.starts_with(".reasonix/") || entrypoint.starts_with(".reasonix\\") {
        return AgentProfileSource::Compatibility {
            provider: "reasonix".to_owned(),
        };
    }
    match &descriptor.source {
        SkillSource::Workspace => AgentProfileSource::Workspace,
        SkillSource::User => AgentProfileSource::User,
        SkillSource::Plugin { plugin_id } => AgentProfileSource::Plugin {
            plugin_id: plugin_id.clone(),
        },
    }
}

fn agent_trust_from_skill(trust: SkillTrustState) -> AgentTrustState {
    match trust {
        SkillTrustState::Trusted => AgentTrustState::Trusted,
        SkillTrustState::NeedsReview => AgentTrustState::NeedsReview,
        SkillTrustState::Disabled => AgentTrustState::Disabled,
    }
}

fn hash_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

pub(super) fn tool_scope_is_empty(scope: &ToolRegistryScope) -> bool {
    !scope.allow_all && scope.names.is_empty() && scope.prefixes.is_empty()
}
