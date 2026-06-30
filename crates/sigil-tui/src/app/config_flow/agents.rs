use super::*;

pub(super) fn render_section(lines: &mut Vec<String>, config_state: &ConfigState) {
    let (_skill_count, skill_agent_count) = skill_config_counts(config_state);
    let agent_count = config_state.agent_profiles.len();
    lines.push("[discovery]".to_owned());
    lines.push(render_config_readonly_row(
        "Enabled",
        bool_summary(config_state.draft.base_root_config.skills.enabled),
    ));
    lines.push(render_config_readonly_row(
        "Configured",
        &format!("{} {}", agent_count, pluralize("agent", agent_count)),
    ));
    lines.push(render_config_readonly_row(
        "Compatibility",
        &format!(
            "{} {}",
            skill_agent_count,
            pluralize("agent", skill_agent_count)
        ),
    ));
    lines.push(render_config_readonly_row(
        "Warnings",
        &format!("{} warnings", config_state.agent_warnings.len()),
    ));
    if agent_count == 0 {
        lines.push(render_config_hint_row("No agents discovered"));
        lines.push(render_config_hint_row(
            "Agents are discovered from built-ins, workspace profiles, plugins, and compatibility sources",
        ));
    } else {
        lines.push(render_config_readonly_row(
            "Selected",
            &selected_agent_summary(config_state),
        ));
        lines.push(String::new());
        lines.push("[agents]".to_owned());
        lines.extend(render_agent_index_lines(config_state));
        if let Some(agent) = config_state.selected_agent() {
            lines.push(String::new());
            lines.push("[agent]".to_owned());
            lines.push(render_config_readonly_row(
                "Agent",
                agent.profile.id.as_str(),
            ));
            lines.extend(render_agent_detail_lines(agent));
        }
    }
    if !config_state.agent_warnings.is_empty() {
        lines.push(String::new());
        lines.push("[warnings]".to_owned());
        for warning in config_state.agent_warnings.iter().take(4) {
            lines.push(render_config_hint_row(warning));
        }
        if config_state.agent_warnings.len() > 4 {
            lines.push(format!(
                "... {} more warnings",
                config_state.agent_warnings.len() - 4
            ));
        }
    }
    lines.push(String::new());
    lines.push("Up/Down agent  PgUp/PgDn wrap  footer trust/disable".to_owned());
    lines.extend(render_config_selection_details(config_state));
}
