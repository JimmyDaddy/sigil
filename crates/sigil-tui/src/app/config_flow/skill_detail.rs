use super::*;

pub(super) fn render_skill_detail_lines(skill: &SkillDescriptor) -> Vec<String> {
    let name = if skill.name.trim().is_empty() {
        skill.id.as_str()
    } else {
        skill.name.as_str()
    };
    let description = if skill.description.trim().is_empty() {
        "none"
    } else {
        skill.description.as_str()
    };
    let argument_hint = skill.argument_hint.as_deref().unwrap_or("none");

    vec![
        render_config_readonly_row("Type", skill_display_noun(skill)),
        render_config_readonly_row("Name", name),
        render_config_readonly_row("Description", description),
        render_config_readonly_row("Enabled", bool_summary(skill.enabled)),
        render_config_readonly_row("Model", bool_summary(skill.model_invocable)),
        render_config_readonly_row("User", bool_summary(skill.user_invocable)),
        render_config_readonly_row("Run mode", skill.run_as.as_str()),
        render_config_readonly_row("Trust", skill.trust.as_str()),
        render_config_readonly_row("Source", &skill_source_summary(&skill.source)),
        render_config_readonly_row("Hash", &short_hash(&skill.sha256)),
        render_config_readonly_row("Entrypoint", &skill.entrypoint.display().to_string()),
        render_config_readonly_row("Root", &skill.root.display().to_string()),
        render_config_readonly_row("Argument hint", argument_hint),
        render_config_readonly_row("Slash", &skill_slash_summary(skill)),
        render_config_readonly_row("Allowed tools", &tool_scope_summary(&skill.allowed_tools)),
        render_config_readonly_row(
            "Disallowed tools",
            &tool_scope_summary(&skill.disallowed_tools),
        ),
        render_config_readonly_row("Paths", &path_pattern_summary(&skill.path_patterns)),
        render_config_readonly_row(
            "Use",
            skill_action_label(skill_invoke_unavailable_reason(skill)),
        ),
    ]
}

pub(super) fn render_skill_index_lines(config_state: &ConfigState, agents: bool) -> Vec<String> {
    config_state
        .skill_descriptors
        .iter()
        .enumerate()
        .filter(|(_, skill)| skill_is_agent(skill) == agents)
        .map(|(index, skill)| {
            let marker = if index == config_state.selected_skill_index {
                ">"
            } else {
                " "
            };
            format!(
                "{marker} {}: {} · {} · {} · {}",
                skill.id,
                skill.trust.as_str(),
                skill.run_as.as_str(),
                skill_source_summary(&skill.source),
                skill_slash_summary(skill)
            )
        })
        .collect()
}

pub(super) fn skill_config_counts(config_state: &ConfigState) -> (usize, usize) {
    let agent_count = config_state
        .skill_descriptors
        .iter()
        .filter(|skill| skill_is_agent(skill))
        .count();
    let skill_count = config_state
        .skill_descriptors
        .len()
        .saturating_sub(agent_count);
    (skill_count, agent_count)
}

pub(super) fn selected_skill_summary(config_state: &ConfigState) -> String {
    let Some(skill) = config_state.selected_skill() else {
        return "none".to_owned();
    };
    let selected_is_agent = skill_is_agent(skill);
    let total = config_state
        .skill_descriptors
        .iter()
        .filter(|candidate| skill_is_agent(candidate) == selected_is_agent)
        .count();
    let position = config_state
        .skill_descriptors
        .iter()
        .take(config_state.selected_skill_index + 1)
        .filter(|candidate| skill_is_agent(candidate) == selected_is_agent)
        .count();
    format!("{} {position}/{total}", skill_display_noun(skill))
}

pub(super) fn skill_section_noun(section: ConfigSection) -> &'static str {
    match section {
        ConfigSection::Agents => "agent",
        ConfigSection::Skills => "skill",
        _ => "skill or agent",
    }
}

pub(super) fn skill_is_agent(skill: &SkillDescriptor) -> bool {
    matches!(skill.run_as, SkillRunMode::ChildSession)
}

pub(super) fn skill_is_command(skill: &SkillDescriptor) -> bool {
    !skill_is_agent(skill) && skill.entrypoint.starts_with(Path::new(".sigil/commands"))
}

pub(super) fn skill_display_noun(skill: &SkillDescriptor) -> &'static str {
    if skill_is_agent(skill) {
        "agent"
    } else if skill_is_command(skill) {
        "command"
    } else {
        "skill"
    }
}

pub(super) fn skill_display_title(skill: &SkillDescriptor) -> &'static str {
    if skill_is_agent(skill) {
        "Agent"
    } else if skill_is_command(skill) {
        "Command"
    } else {
        "Skill"
    }
}

pub(super) fn skill_slash_summary(skill: &SkillDescriptor) -> String {
    let command = format!("/{}", skill.id);
    if SLASH_COMMANDS
        .iter()
        .any(|spec| spec.canonical == command || spec.aliases.contains(&command.as_str()))
    {
        format!("shadowed by native {command}")
    } else if skill.user_invocable {
        command
    } else {
        "not user-invocable".to_owned()
    }
}

pub(super) fn skill_action_label(reason: Option<&'static str>) -> &'static str {
    match reason {
        Some(reason) => reason,
        None => "available",
    }
}

pub(super) fn skill_load_unavailable_reason(skill: &SkillDescriptor) -> Option<&'static str> {
    if !skill.enabled {
        return Some("is disabled");
    }
    if skill.trust != SkillTrustState::Trusted {
        return Some("is not trusted");
    }
    if !skill.model_invocable {
        return Some("is not model-invocable");
    }
    None
}

pub(super) fn skill_invoke_unavailable_reason(skill: &SkillDescriptor) -> Option<&'static str> {
    if let Some(reason) = skill_load_unavailable_reason(skill) {
        return Some(reason);
    }
    if !skill.user_invocable {
        return Some("is not user-invocable");
    }
    None
}

pub(super) fn skill_invoke_prompt(skill: &SkillDescriptor, arguments: &str) -> String {
    let trimmed = arguments.trim();
    if trimmed.is_empty() {
        return format!(
            "Use the `load_skill` tool to load skill `{}`, then apply that skill to the current task. No additional arguments were provided.",
            skill.id
        );
    }
    format!(
        "Use the `load_skill` tool to load skill `{}`, then apply that skill to the current task with these arguments:\n\n```text\n{}\n```",
        skill.id, trimmed
    )
}

pub(super) fn skill_source_summary(source: &SkillSource) -> String {
    match source {
        SkillSource::Workspace => "workspace".to_owned(),
        SkillSource::User => "user".to_owned(),
        SkillSource::Plugin { plugin_id } => format!("plugin:{plugin_id}"),
    }
}
