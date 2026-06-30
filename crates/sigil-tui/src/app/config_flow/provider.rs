use super::*;

pub(super) fn render_section(lines: &mut Vec<String>, config_state: &ConfigState) {
    lines.push("[runtime]".to_owned());
    lines.push(render_config_value_row(
        config_state,
        ConfigField::ProviderName,
    ));
    lines.push(String::new());
    lines.push("[model]".to_owned());
    lines.push(render_config_value_row(
        config_state,
        ConfigField::ProviderModel,
    ));
    lines.push(String::new());
    lines.push("[authentication]".to_owned());
    lines.push(render_config_value_row(
        config_state,
        ConfigField::ProviderApiKey,
    ));
    lines.push(String::new());
    lines.push("[endpoint]".to_owned());
    lines.push(render_config_hint_row(
        "custom endpoint is an advanced config-file setting",
    ));
    lines.push(String::new());
    lines.push("[advanced]".to_owned());
    lines.push(render_config_hint_row(
        "provider-specific FIM and strict-tool switches stay out of the default flow",
    ));
    lines.extend(render_config_selection_details(config_state));
    lines.push(String::new());
    lines.push("[capabilities]".to_owned());
    lines.extend(render_capability_summary(config_state));
}

fn render_capability_summary(config_state: &ConfigState) -> Vec<String> {
    let provider_name = config_state.draft.provider_name.as_str();
    let Some(capabilities) = provider_capabilities_for_name(provider_name) else {
        return vec![render_config_hint_row("Unknown provider capabilities")];
    };
    let view = provider_capability_view(provider_name, &capabilities);
    let supported = view
        .rows
        .iter()
        .filter(|row| row.status.as_str() == "supported")
        .count();
    let advanced = view
        .rows
        .iter()
        .filter(|row| row.status.as_str() == "advanced")
        .count();
    vec![
        render_config_readonly_row(
            "Provider matrix",
            &format!(
                "{} supported · {} advanced · {} total",
                supported,
                advanced,
                view.rows.len()
            ),
        ),
        render_config_hint_row("Full capability summary is available in /doctor"),
    ]
}
