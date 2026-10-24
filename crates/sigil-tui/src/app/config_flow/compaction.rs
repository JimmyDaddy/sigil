use super::*;

pub(super) fn render_section(
    app: &AppState,
    lines: &mut ConfigDetailPresentation,
    config_state: &ConfigState,
) {
    lines.push("[configuration preference]".to_owned());
    lines.field(config_state, ConfigField::CompactionEnabled);
    lines.field(config_state, ConfigField::CompactionNativeCarrierEnabled);
    lines.push(render_config_readonly_row(
        "Effective window",
        &render_effective_context_window(config_state),
    ));
    lines.field(config_state, ConfigField::CompactionContextWindowTokens);
    lines.push(String::new());
    lines.push("[current session]".to_owned());
    lines.push(render_config_readonly_row(
        "Compaction status",
        &app.runtime.compaction_status,
    ));
    lines.extend(render_config_selection_details(config_state));
}
