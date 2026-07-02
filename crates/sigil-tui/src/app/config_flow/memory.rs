use super::*;

pub(super) fn render_section(app: &AppState, lines: &mut Vec<String>, config_state: &ConfigState) {
    lines.push("[workspace memory]".to_owned());
    lines.push(render_config_value_row(
        config_state,
        ConfigField::MemoryEnabled,
    ));
    lines.push(String::new());
    lines.push("[loaded context]".to_owned());
    lines.push(render_config_readonly_row(
        "Documents",
        &format!("{} loaded", app.runtime.memory_document_count),
    ));
    lines.push(render_config_readonly_row(
        "Last scan",
        &app.runtime.memory_last_status,
    ));
    lines.push(render_config_readonly_row(
        "Root files",
        "SIGIL.md, AGENTS.md, CLAUDE.md, local override",
    ));
    lines.extend(render_config_selection_details(config_state));
}
