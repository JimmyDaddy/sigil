use super::*;

pub(super) fn render_permission_rule_summary(config_state: &ConfigState) -> Vec<String> {
    let commands = &config_state.draft.base_root_config.permission.commands;
    let rules = &config_state.draft.base_root_config.permission.rules;
    let command_count = commands.pattern_count();
    let mut lines = vec![render_config_readonly_row(
        "Command permissions",
        &command_permission_count_summary(command_count),
    )];

    if command_count == 0 {
        lines.push(render_config_hint_row(
            "No advanced command patterns configured",
        ));
    } else {
        let mut shown = 0usize;
        for (group, patterns) in [
            ("allow", commands.allow.as_slice()),
            ("ask", commands.ask.as_slice()),
            ("deny", commands.deny.as_slice()),
        ] {
            for pattern in patterns {
                if shown >= 4 {
                    break;
                }
                lines.push(format!("- {group} · {pattern}"));
                shown += 1;
            }
            if shown >= 4 {
                break;
            }
        }
        if command_count > 4 {
            lines.push(format!(
                "... {} more command patterns in config file",
                command_count - 4
            ));
        }
    }

    let rule_count = if rules.is_empty() {
        "none".to_owned()
    } else {
        format!("{} configured", rules.len())
    };
    lines.push(render_config_readonly_row("Rule overrides", &rule_count));

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

fn command_permission_count_summary(count: usize) -> String {
    if count == 0 {
        "none".to_owned()
    } else {
        format!("{count} patterns configured in config file")
    }
}
