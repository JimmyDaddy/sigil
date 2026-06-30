use super::*;

pub(super) fn render_section(lines: &mut Vec<String>, config_state: &ConfigState) {
    let (skill_count, agent_count) = skill_config_counts(config_state);
    lines.push("[discovery]".to_owned());
    lines.push(render_config_readonly_row(
        "Enabled",
        bool_summary(config_state.draft.base_root_config.skills.enabled),
    ));
    lines.push(render_config_readonly_row(
        "Configured",
        &format!("{} {}", skill_count, pluralize("skill", skill_count)),
    ));
    lines.push(render_config_readonly_row(
        "Agents",
        &format!("{} {}", agent_count, pluralize("agent", agent_count)),
    ));
    lines.push(render_config_readonly_row(
        "Warnings",
        &format!("{} warnings", config_state.skill_warnings.len()),
    ));
    if skill_count == 0 {
        lines.push(render_config_hint_row("No skills discovered"));
        lines.push(render_config_hint_row(
            "Reusable inline skills are discovered from configured skills directories",
        ));
    } else {
        lines.push(render_config_readonly_row(
            "Selected",
            &selected_skill_summary(config_state),
        ));
        lines.push(String::new());
        lines.push("[skills]".to_owned());
        lines.extend(render_skill_index_lines(config_state, false));
        if let Some(skill) = config_state.selected_skill() {
            lines.push(String::new());
            lines.push("[skill]".to_owned());
            lines.push(render_config_readonly_row("Skill", &skill.id));
            lines.extend(render_skill_detail_lines(skill));
        }
    }
    if !config_state.skill_warnings.is_empty() {
        lines.push(String::new());
        lines.push("[warnings]".to_owned());
        for warning in config_state.skill_warnings.iter().take(4) {
            lines.push(render_config_hint_row(warning));
        }
        if config_state.skill_warnings.len() > 4 {
            lines.push(format!(
                "... {} more warnings",
                config_state.skill_warnings.len() - 4
            ));
        }
    }
    lines.push(String::new());
    lines.push("Up/Down skill  PgUp/PgDn wrap  footer use".to_owned());
    lines.extend(render_config_selection_details(config_state));
}
