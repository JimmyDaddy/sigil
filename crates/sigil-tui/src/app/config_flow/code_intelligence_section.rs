use super::*;

pub(super) fn render_section(app: &AppState, lines: &mut Vec<String>, config_state: &ConfigState) {
    lines.push("[controls]".to_owned());
    lines.push(render_config_value_row(
        config_state,
        ConfigField::CodeIntelEnabled,
    ));
    lines.push(render_config_value_row(
        config_state,
        ConfigField::CodeIntelServerStartup,
    ));
    lines.push(render_config_readonly_row(
        "Auto discover",
        bool_summary(config_state.draft.code_intelligence_auto_discover),
    ));
    lines.push(render_config_readonly_row(
        "Missing reports",
        bool_summary(config_state.draft.code_intelligence_report_missing),
    ));
    lines.push(String::new());
    lines.push("[trust]".to_owned());
    lines.extend(render_code_intelligence_trust_summary());
    lines.push(String::new());
    lines.push("[readiness]".to_owned());
    lines.extend(app.render_code_intelligence_readiness_summary(config_state));
    lines.push(render_config_hint_row(
        "LSP discovery details are configured in sigil.toml or surfaced by doctor",
    ));
    lines.extend(render_config_selection_details(config_state));
}
