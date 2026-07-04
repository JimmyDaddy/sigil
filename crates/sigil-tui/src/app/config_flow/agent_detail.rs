use super::*;

pub(super) fn render_agent_detail_lines(agent: &ResolvedAgentProfile) -> Vec<String> {
    let description = if agent.profile.description.trim().is_empty() {
        "none"
    } else {
        agent.profile.description.as_str()
    };
    let provider = agent.profile.provider.as_deref().unwrap_or("session");
    let model = agent.profile.model.as_deref().unwrap_or("session");
    let reasoning = agent
        .profile
        .reasoning_effort
        .as_ref()
        .map(|effort| effort.as_str())
        .unwrap_or("session");
    vec![
        render_config_readonly_row("Kind", agent_profile_kind_label(agent.profile.kind)),
        render_config_readonly_row("Description", description),
        render_config_readonly_row("Enabled", &agent_enabled_summary(agent)),
        render_config_readonly_row("User", &agent_user_invocable_summary(agent)),
        render_config_readonly_row("Model", &agent_model_invocable_summary(agent)),
        render_config_readonly_row("Trust", agent_trust_state_label(agent.trust_state)),
        render_config_readonly_row("Source", &agent_profile_source_summary(&agent.source)),
        render_config_readonly_row("Source hash", &short_hash(&agent.source_hash)),
        render_config_readonly_row("Provider", provider),
        render_config_readonly_row("Model name", model),
        render_config_readonly_row("Reasoning", reasoning),
        render_config_readonly_row("Invocation", agent.profile.invocation_policy.as_str()),
        render_config_readonly_row("Result", agent.profile.result_policy.as_str()),
        render_config_readonly_row("Tools", &tool_scope_summary(&agent.profile.tool_scope)),
        render_config_readonly_row(
            "Permission",
            agent.profile.permission_policy.default_mode.as_str(),
        ),
        render_config_readonly_row("Skills", &list_summary(&agent.profile.skills)),
        render_config_readonly_row("MCP", &list_summary(&agent.profile.mcp_servers)),
        render_config_readonly_row(
            "Nicknames",
            &list_summary(&agent.profile.nickname_candidates),
        ),
        render_config_readonly_row("Aliases", &list_summary(&agent.profile.aliases)),
        render_config_readonly_row("Slash", &agent_slash_name_summary(agent)),
    ]
}

pub(super) fn render_agent_index_lines(config_state: &ConfigState) -> Vec<String> {
    config_state
        .agent_profiles
        .iter()
        .enumerate()
        .map(|(index, agent)| {
            let marker = if index == config_state.selected_agent_index {
                ">"
            } else {
                " "
            };
            format!(
                "{marker} {}: {} · {} · {} · {}",
                agent.profile.id.as_str(),
                agent_trust_state_label(agent.trust_state),
                agent_profile_kind_label(agent.profile.kind),
                agent_profile_source_summary(&agent.source),
                agent_policy_flags(agent)
            )
        })
        .collect()
}

pub(super) fn selected_agent_summary(config_state: &ConfigState) -> String {
    let Some(agent) = config_state.selected_agent() else {
        return "none".to_owned();
    };
    format!(
        "agent {}/{} · {}",
        config_state.selected_agent_index + 1,
        config_state.agent_profiles.len(),
        agent.profile.id.as_str()
    )
}

pub(super) fn agent_policy_flags(agent: &ResolvedAgentProfile) -> String {
    format!(
        "enabled={} user={} model={}",
        bool_summary(agent.effective_enabled()),
        bool_summary(agent.effective_user_invocation_allowed()),
        bool_summary(agent.effective_model_invocation_allowed())
    )
}

pub(super) fn agent_enabled_summary(agent: &ResolvedAgentProfile) -> String {
    bool_override_summary(
        agent.effective_enabled(),
        agent.enabled,
        agent.enabled_override,
    )
}

pub(super) fn agent_user_invocable_summary(agent: &ResolvedAgentProfile) -> String {
    bool_override_summary(
        agent.effective_user_invocation_allowed(),
        agent.profile.user_invocation_allowed(),
        agent.user_invocable_override,
    )
}

pub(super) fn agent_model_invocable_summary(agent: &ResolvedAgentProfile) -> String {
    bool_override_summary(
        agent.effective_model_invocation_allowed(),
        agent.profile.model_invocation_allowed(),
        agent.model_invocable_override,
    )
}

pub(super) fn agent_slash_name_summary(agent: &ResolvedAgentProfile) -> String {
    if agent.profile.slash_names.is_empty() {
        return "none".to_owned();
    }
    agent
        .profile
        .slash_names
        .iter()
        .map(|name| format!("/{name}"))
        .collect::<Vec<_>>()
        .join(",")
}

pub(super) fn bool_override_summary(
    effective: bool,
    source: bool,
    override_value: Option<bool>,
) -> String {
    match override_value {
        Some(_) => format!(
            "{} (source {})",
            bool_summary(effective),
            bool_summary(source)
        ),
        None => bool_summary(effective).to_owned(),
    }
}

pub(super) fn agent_profile_kind_label(kind: AgentProfileKind) -> &'static str {
    match kind {
        AgentProfileKind::Primary => "primary",
        AgentProfileKind::Subagent => "subagent",
        AgentProfileKind::System => "system",
        AgentProfileKind::Unknown => "unknown",
    }
}

pub(super) fn agent_trust_state_label(state: AgentTrustState) -> &'static str {
    match state {
        AgentTrustState::Trusted => "trusted",
        AgentTrustState::NeedsReview => "needs_review",
        AgentTrustState::Disabled => "disabled",
        AgentTrustState::Unknown => "unknown",
    }
}

pub(super) fn agent_profile_source_summary(source: &AgentProfileSource) -> String {
    match source {
        AgentProfileSource::Workspace => "workspace".to_owned(),
        AgentProfileSource::User => "user".to_owned(),
        AgentProfileSource::Plugin { plugin_id } => format!("plugin:{plugin_id}"),
        AgentProfileSource::Compatibility { provider } => format!("compat:{provider}"),
        AgentProfileSource::System => "system".to_owned(),
        AgentProfileSource::Unknown => "unknown".to_owned(),
    }
}

pub(super) fn tool_scope_summary(scope: &ToolRegistryScope) -> String {
    if scope.allow_all {
        return "all".to_owned();
    }
    let mut parts = Vec::new();
    if !scope.names.is_empty() {
        parts.push(format!(
            "names={}",
            scope.names.iter().cloned().collect::<Vec<_>>().join(",")
        ));
    }
    if !scope.prefixes.is_empty() {
        parts.push(format!("prefixes={}", scope.prefixes.join(",")));
    }
    if parts.is_empty() {
        "none".to_owned()
    } else {
        parts.join(" ")
    }
}
