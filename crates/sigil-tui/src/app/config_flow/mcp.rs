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
        lines.push(render_config_readonly_row(
            "Selected",
            &format!(
                "{} of {}",
                config_state.selected_mcp_server_index + 1,
                config_state.draft.mcp_servers.len()
            ),
        ));
        if config_state.selected_mcp_server().is_some() {
            lines.push(String::new());
            lines.push("[server]".to_owned());
            if let Some(server) = config_state.selected_mcp_server() {
                lines.push(render_config_readonly_row("Name", &server.name));
            }
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
    lines.push("PgUp/PgDn server  footer activate/refresh".to_owned());
    lines.push(render_config_hint_row(
        "MCP command, args, and timeout are edited in the config file",
    ));
    lines.extend(render_config_selection_details(config_state));
}
