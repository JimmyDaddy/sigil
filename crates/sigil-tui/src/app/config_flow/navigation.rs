use super::*;

pub(super) fn render_config_nav_lines(config_state: Option<&ConfigState>) -> Vec<String> {
    let Some(state) = config_state else {
        return Vec::new();
    };

    let mut lines = vec![
        if state.show_advanced {
            "Config · advanced"
        } else {
            "Config · simple"
        }
        .to_owned(),
        String::new(),
    ];
    for section in state.visible_sections() {
        lines.push(format!(
            "{} {}",
            if *section == state.selected_section {
                ">"
            } else {
                " "
            },
            section.title()
        ));
    }
    lines.push(String::new());
    lines.push(CONFIG_SECTION_NAV_HINT.to_owned());
    lines.push(CONFIG_FIELD_NAV_HINT.to_owned());
    lines.push(CONFIG_EDIT_OR_TOGGLE_HINT.to_owned());
    lines.push(format!("{CONFIG_SAVE_HINT}  Ctrl-A advanced  Esc close"));
    if state.selected_section == ConfigSection::Storage {
        lines.push("Storage: footer clean artifacts".to_owned());
    } else if state.selected_section == ConfigSection::Mcp {
        lines.push("MCP: PgUp/PgDn switch".to_owned());
        lines.push("MCP: footer activate/refresh".to_owned());
        lines.push("MCP: edit servers in sigil.toml".to_owned());
    } else if state.selected_section == ConfigSection::Agents {
        lines.push("Agents: Up/Down select".to_owned());
        lines.push("Agents: PgUp/PgDn wrap".to_owned());
        lines.push("Agents: footer trust/disable".to_owned());
    } else if state.selected_section == ConfigSection::Skills {
        lines.push("Skills: Up/Down select".to_owned());
        lines.push("Skills: PgUp/PgDn wrap".to_owned());
        lines.push("Skills: footer use".to_owned());
    } else if state.selected_section == ConfigSection::Plugins {
        lines.push("Plugins: Up/Down select".to_owned());
        lines.push("Plugins: PgUp/PgDn wrap".to_owned());
        lines.push("Plugins: footer approve/deny".to_owned());
    } else if state.selected_section == ConfigSection::Permissions {
        lines.push("Permissions: Enter cycle mode".to_owned());
        lines.push("Permissions: task checks run from task status".to_owned());
    } else if state.selected_section == ConfigSection::Appearance {
        lines.push("Appearance: Enter cycle".to_owned());
        lines.push("Appearance: color overrides in sigil.toml".to_owned());
    } else if state.selected_section == ConfigSection::Terminal {
        lines.push("Terminal: compatibility lives in sigil.toml".to_owned());
    } else if state.selected_section == ConfigSection::CodeIntelligence {
        lines.push("Code Intel: Enter cycle mode/startup".to_owned());
    }
    lines
}

pub(super) fn move_config_collection_selection(
    config_state: &mut ConfigState,
    forward: bool,
    storage_artifact_count: usize,
) -> Option<ConfigFieldMove> {
    match config_state.selected_section {
        ConfigSection::Agents => Some(config_state.move_agent(forward)),
        ConfigSection::Skills => Some(config_state.move_skill(forward)),
        ConfigSection::Plugins => Some(config_state.move_plugin(forward)),
        ConfigSection::Storage => Some(move_storage_artifact_selection(
            config_state,
            forward,
            storage_artifact_count,
        )),
        _ => None,
    }
}

pub(super) fn move_storage_artifact_selection(
    config_state: &mut ConfigState,
    forward: bool,
    artifact_count: usize,
) -> ConfigFieldMove {
    if artifact_count == 0 {
        return ConfigFieldMove::Unavailable;
    }
    let current = config_state
        .selected_storage_artifact_index
        .min(artifact_count.saturating_sub(1));
    let next = if forward {
        if current + 1 >= artifact_count {
            return ConfigFieldMove::Boundary;
        }
        current + 1
    } else {
        if current == 0 {
            return ConfigFieldMove::Boundary;
        }
        current - 1
    };
    config_state.selected_storage_artifact_index = next;
    ConfigFieldMove::Moved
}

pub(super) fn config_collection_selection_notice(
    config_state: &ConfigState,
    storage_artifact_count: usize,
) -> Option<String> {
    match config_state.selected_section {
        ConfigSection::Agents if config_state.selected_agent().is_some() => {
            Some(selected_agent_summary(config_state))
        }
        ConfigSection::Skills if config_state.selected_skill().is_some() => {
            Some(selected_skill_summary(config_state))
        }
        ConfigSection::Plugins if !config_state.plugin_manifests.is_empty() => Some(format!(
            "plugin {}/{}",
            config_state.selected_plugin_index + 1,
            config_state.plugin_manifests.len()
        )),
        ConfigSection::Storage if storage_artifact_count > 0 => Some(format!(
            "artifact {}/{}",
            config_state.selected_storage_artifact_index + 1,
            storage_artifact_count
        )),
        _ => None,
    }
}
