use super::*;

pub(super) fn render_config_detail_header(config_state: &ConfigState) -> Vec<String> {
    let section = config_state.selected_section;
    let step_label = config_state
        .visible_sections()
        .iter()
        .map(|candidate| {
            if *candidate == section {
                format!("[{}]", candidate.step_token())
            } else {
                candidate.step_token().to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    vec![
        match section.flow_index() {
            Some(index) => format!(
                "{} {}/{} · {}",
                section.title(),
                index + 1,
                ConfigSection::FLOW.len(),
                section.summary()
            ),
            None => section.title().to_owned(),
        },
        step_label,
        String::new(),
    ]
}

pub(super) fn render_config_selection_details(config_state: &ConfigState) -> Vec<String> {
    let Some(field) = config_state.selected_field else {
        let mut lines = vec![
            String::new(),
            "[details]".to_owned(),
            CONFIG_CONTROLS_HINT.to_owned(),
            CONFIG_ACTIONS_HINT.to_owned(),
        ];
        if config_state.selected_section == ConfigSection::Mcp {
            lines.push("mcp: PgUp/PgDn server · footer activate/refresh".to_owned());
        } else if config_state.selected_section == ConfigSection::Agents {
            lines.push("agents: Up/Down agent · PgUp/PgDn wrap · footer trust/disable".to_owned());
        } else if config_state.selected_section == ConfigSection::Skills {
            lines.push("skills: Up/Down skill · PgUp/PgDn wrap · footer use".to_owned());
        } else if config_state.selected_section == ConfigSection::Plugins {
            lines.push("plugins: Up/Down plugin · PgUp/PgDn wrap · footer approve/deny".to_owned());
        }
        return lines;
    };
    let mut lines = vec![
        String::new(),
        "[details]".to_owned(),
        format!(
            "selected: {}",
            config_field_display_label(config_state, field)
        ),
        format!("key: {}", config_field_key_label(config_state, field)),
        config_field_help_text(config_state, field).to_owned(),
        String::new(),
        CONFIG_CONTROLS_HINT.to_owned(),
        CONFIG_ACTIONS_HINT.to_owned(),
    ];

    if matches!(field, ConfigField::ProviderApiKey) {
        let env_name = provider_api_key_env_name(&config_state.draft.provider_name);
        lines.push(format!("override: {env_name}"));
        lines.push("storage: saved api_key is plaintext in sigil.toml".to_owned());
    }
    if matches!(field, ConfigField::ProviderFimModel) {
        lines.push("advanced: provider-specific fields remain in config file or env".to_owned());
    }
    if matches!(field, ConfigField::AppearanceSyntaxTheme) {
        lines.push("appearance: auto follows the selected TUI theme for code blocks".to_owned());
    }
    if matches!(field, ConfigField::AppearanceColorGroup) {
        lines.push("advanced: color token groups are edited in sigil.toml".to_owned());
    }
    if matches!(field, ConfigField::AppearanceColorToken) {
        lines.push("advanced: color token selection is edited in sigil.toml".to_owned());
    }
    if matches!(field, ConfigField::AppearanceColorOverride) {
        lines.push("advanced: color overrides are edited in sigil.toml".to_owned());
    }
    if config_state.selected_section == ConfigSection::Mcp {
        lines.push("mcp: PgUp/PgDn server · footer activate/refresh".to_owned());
    } else if config_state.selected_section == ConfigSection::Agents {
        lines.push("agents: Up/Down agent · PgUp/PgDn wrap · footer trust/disable".to_owned());
    } else if config_state.selected_section == ConfigSection::Skills {
        lines.push("skills: Up/Down skill · PgUp/PgDn wrap · footer use".to_owned());
    } else if config_state.selected_section == ConfigSection::Plugins {
        lines.push("plugins: Up/Down plugin · PgUp/PgDn wrap · footer approve/deny".to_owned());
    }

    lines
}
