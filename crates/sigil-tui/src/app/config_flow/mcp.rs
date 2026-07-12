use super::*;

pub(super) fn render_section(app: &AppState, lines: &mut Vec<String>, config_state: &ConfigState) {
    lines.push("[servers]".to_owned());
    lines.push(render_config_readonly_row(
        "Configured",
        &format!("{} servers", config_state.draft.mcp_servers.len()),
    ));
    if config_state.draft.mcp_servers.is_empty() {
        lines.push(render_config_hint_row("No MCP servers configured"));
        lines.push(render_config_hint_row(
            "Add MCP servers in ~/.sigil/sigil.toml or your explicit config file",
        ));
    } else {
        lines.push(render_config_value_row(config_state, ConfigField::McpName));
        if config_state.selected_mcp_server().is_some() {
            lines.push(String::new());
            lines.push("[lifecycle]".to_owned());
            lines.extend(render_mcp_lifecycle_summary(
                config_state,
                &app.selected_mcp_runtime_status_label(config_state),
            ));
            if let Some(boundary) = app.selected_mcp_boundary_label(config_state) {
                lines.push(render_config_readonly_row("Boundary", &boundary));
            }
        }
    }
    lines.push(String::new());
    lines.push("Enter next server · Down actions · footer activate/refresh".to_owned());
    lines.push(render_config_hint_row(
        "Transport-specific fields are edited in the config file; this view never shows resolved secret values",
    ));
    lines.extend(render_config_selection_details(config_state));
}
