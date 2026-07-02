use super::*;

pub(super) fn render_section(app: &AppState, lines: &mut Vec<String>, config_state: &ConfigState) {
    lines.push("[context]".to_owned());
    lines.push(render_config_value_row(
        config_state,
        ConfigField::CompactionEnabled,
    ));
    lines.push(render_config_readonly_row(
        "Effective window",
        &render_effective_context_window(config_state),
    ));
    lines.push(render_config_value_row(
        config_state,
        ConfigField::CompactionContextWindowTokens,
    ));
    lines.push(String::new());
    lines.push("[thresholds]".to_owned());
    lines.push(render_config_value_row(
        config_state,
        ConfigField::CompactionSoftThresholdRatio,
    ));
    lines.push(render_config_value_row(
        config_state,
        ConfigField::CompactionHardThresholdRatio,
    ));
    lines.push(render_config_value_row(
        config_state,
        ConfigField::CompactionTailMessages,
    ));
    lines.push(format!("status: {}", app.runtime.compaction_status));
    lines.extend(render_config_selection_details(config_state));
}
