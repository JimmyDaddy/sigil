use super::*;

pub(super) fn render_section(app: &AppState, lines: &mut Vec<String>, config_state: &ConfigState) {
    lines.push("[permissions]".to_owned());
    lines.push(render_config_value_row(
        config_state,
        ConfigField::PermissionsDefaultMode,
    ));
    lines.push(String::new());
    lines.push("[workspace]".to_owned());
    lines.extend(verification::render_trust_summary(app, config_state));
    lines.push(String::new());
    lines.push("[advanced]".to_owned());
    lines.extend(render_permission_rule_summary(config_state));
    lines.extend(verification::render_scope_summary(config_state));
    lines.extend(render_config_selection_details(config_state));
}
