use super::*;

pub(super) fn render_permission_rule_summary(config_state: &ConfigState) -> Vec<String> {
    let rules = &config_state.draft.base_root_config.permission.rules;
    let rule_count = if rules.is_empty() {
        "none".to_owned()
    } else {
        format!("{} configured", rules.len())
    };
    let mut lines = vec![render_config_readonly_row("Rule overrides", &rule_count)];

    if rules.is_empty() {
        lines.push(render_config_hint_row(
            "All unmatched tools use the default mode above",
        ));
        return lines;
    }

    for rule in rules.iter().take(4) {
        let tool = rule.tool_name.as_deref().unwrap_or("any tool");
        let subject = rule.subject_glob.as_deref().unwrap_or("any subject");
        lines.push(format!("- {tool} · {} · {subject}", rule.mode.as_str()));
    }
    if rules.len() > 4 {
        lines.push(format!("... {} more rules in config file", rules.len() - 4));
    }

    lines
}
