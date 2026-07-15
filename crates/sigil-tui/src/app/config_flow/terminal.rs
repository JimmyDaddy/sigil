use super::*;

pub(super) fn render_section(lines: &mut Vec<String>, config_state: &ConfigState) {
    lines.push("[attention signals]".to_owned());
    lines.push(render_config_value_row(
        config_state,
        ConfigField::TerminalNotificationsEnabled,
    ));
    lines.push(render_config_value_row(
        config_state,
        ConfigField::TerminalNotificationMethod,
    ));
    lines.push(render_config_value_row(
        config_state,
        ConfigField::TerminalNotificationMinimumRunDurationMs,
    ));
    lines.push(render_config_hint_row(
        "Fixed privacy-safe messages only; terminal support and focus handling are automatic",
    ));
    lines.push(String::new());
    lines.push("[interaction]".to_owned());
    lines.push(render_config_readonly_row(
        "Keyboard enhancement",
        config_state.draft.terminal_keyboard_enhancement.as_str(),
    ));
    lines.push(render_config_readonly_row(
        "Mouse capture",
        bool_summary(config_state.draft.terminal_mouse_capture),
    ));
    lines.push(render_config_readonly_row(
        "OSC52 clipboard",
        bool_summary(config_state.draft.terminal_osc52_clipboard),
    ));
    lines.push(render_config_readonly_row(
        "Scroll sensitivity",
        &format!("{} rows", config_state.draft.terminal_scroll_sensitivity),
    ));
    lines.push(String::new());
    lines.push("[compatibility]".to_owned());
    lines.push(render_config_hint_row(
        "Terminal compatibility settings are edited in sigil.toml or guided by doctor",
    ));
    lines.push(render_config_hint_row(
        "Use defaults unless your terminal or multiplexer mishandles mouse/clipboard",
    ));
    lines.extend(render_config_selection_details(config_state));
}
