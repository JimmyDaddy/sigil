use super::*;

pub(super) fn render_section(lines: &mut Vec<String>, config_state: &ConfigState) {
    lines.push("[discovery]".to_owned());
    lines.push(render_config_readonly_row(
        "Configured",
        &format!("{} plugins", config_state.plugin_manifests.len()),
    ));
    lines.push(render_config_readonly_row(
        "Warnings",
        &format!("{} warnings", config_state.plugin_warnings.len()),
    ));
    if config_state.plugin_manifests.is_empty() {
        lines.push(render_config_hint_row("No plugin manifests discovered"));
        lines.push(render_config_hint_row(
            "Workspace plugins live under .sigil/plugins/<id>/plugin.toml",
        ));
    } else {
        lines.push(render_config_readonly_row(
            "Selected",
            &format!(
                "{} of {}",
                config_state.selected_plugin_index + 1,
                config_state.plugin_manifests.len()
            ),
        ));
        lines.push(String::new());
        lines.push("[plugins]".to_owned());
        lines.extend(render_plugin_index_lines(config_state));
        if let Some(plugin) = config_state.selected_plugin() {
            lines.push(String::new());
            lines.push("[plugin]".to_owned());
            lines.push(render_config_readonly_row("Plugin", &plugin.plugin_id));
            lines.extend(render_plugin_detail_lines(plugin));
        }
    }
    if !config_state.plugin_warnings.is_empty() {
        lines.push(String::new());
        lines.push("[warnings]".to_owned());
        for warning in config_state.plugin_warnings.iter().take(4) {
            lines.push(render_config_hint_row(warning));
        }
        if config_state.plugin_warnings.len() > 4 {
            lines.push(format!(
                "... {} more warnings",
                config_state.plugin_warnings.len() - 4
            ));
        }
    }
    lines.push(String::new());
    lines.push("Up/Down plugin  PgUp/PgDn wrap  footer approve/deny".to_owned());
    lines.extend(render_config_selection_details(config_state));
}
