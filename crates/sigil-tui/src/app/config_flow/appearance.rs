use super::*;

pub(super) fn render_section(lines: &mut Vec<String>, config_state: &ConfigState) {
    lines.push("[layout]".to_owned());
    lines.push(render_config_value_row(
        config_state,
        ConfigField::AppearanceInfoRail,
    ));
    lines.push(render_config_hint_row(
        "F2 toggles visibility for the current run; narrow terminals still collapse the rail",
    ));
    lines.push(String::new());
    lines.push("[theme]".to_owned());
    lines.push(render_config_value_row(
        config_state,
        ConfigField::AppearanceTheme,
    ));
    lines.push(render_config_readonly_row(
        "Name",
        config_state.draft.appearance_theme.display_label(),
    ));
    lines.push(render_config_value_row(
        config_state,
        ConfigField::AppearanceSyntaxTheme,
    ));
    lines.push(render_config_readonly_row(
        "Syntax source",
        &render_syntax_theme_source(config_state),
    ));
    lines.push(render_config_value_row(
        config_state,
        ConfigField::AppearanceUsageCostCurrency,
    ));
    lines.push(render_config_readonly_row(
        "Cost source",
        &render_usage_cost_currency_source(config_state),
    ));
    let available = ThemeId::all()
        .iter()
        .map(|theme| theme.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    lines.push(render_config_readonly_row("Built-ins", &available));
    lines.push(render_config_readonly_row(
        "Overrides",
        &format!(
            "{} colors",
            config_state.draft.base_root_config.appearance.colors.len()
        ),
    ));
    lines.push(render_config_hint_row(
        "Fine-grained color token overrides are edited in sigil.toml",
    ));
    lines.push(String::new());
    lines.push("[diagnostics]".to_owned());
    lines.extend(render_appearance_diagnostic_lines(config_state));
    lines.push(String::new());
    lines.push("[preview]".to_owned());
    lines.extend(render_appearance_preview_lines(config_state));
    lines.push(String::new());
    lines.push("[scope]".to_owned());
    lines.push(render_config_hint_row(
        "Theme choices affect only the TUI and are not written to session history",
    ));
    lines.push(render_config_hint_row(
        "Theme draft previews immediately; Ctrl-S persists it",
    ));
    lines.extend(render_config_selection_details(config_state));
}

pub(super) fn draft_appearance_config(config_state: &ConfigState) -> AppearanceConfig {
    let mut appearance = config_state.draft.base_root_config.appearance.clone();
    appearance.theme = config_state.draft.appearance_theme;
    appearance.syntax_theme = config_state.draft.appearance_syntax_theme;
    appearance.usage_cost_currency = config_state.draft.appearance_usage_cost_currency;
    appearance.info_rail = config_state.draft.appearance_info_rail;
    appearance
}

pub(super) fn render_syntax_theme_source(config_state: &ConfigState) -> String {
    let configured = config_state.draft.appearance_syntax_theme;
    let resolved = config_state.draft.resolved_appearance_syntax_theme();
    if configured == SyntaxThemeId::Auto {
        format!("auto -> {}", resolved.display_label())
    } else {
        format!("manual -> {}", resolved.display_label())
    }
}

pub(super) fn render_usage_cost_currency_source(config_state: &ConfigState) -> String {
    match config_state.draft.appearance_usage_cost_currency {
        sigil_kernel::UsageCostCurrency::Auto => "auto -> provider balance currency".to_owned(),
        currency => format!("manual -> {}", currency.display_label()),
    }
}

pub(super) fn render_appearance_diagnostic_lines(config_state: &ConfigState) -> Vec<String> {
    let appearance = draft_appearance_config(config_state);
    let checks = appearance_doctor_checks(&appearance);
    let warnings = checks
        .iter()
        .filter(|check| check.status != DoctorStatus::Ok)
        .collect::<Vec<_>>();
    if warnings.is_empty() {
        return vec![render_config_readonly_row("Status", "ok")];
    }

    let mut lines = vec![render_config_readonly_row(
        "Status",
        &format!("{} warnings", warnings.len()),
    )];
    for check in warnings.iter().take(3) {
        lines.push(render_config_readonly_row(
            check.name.trim_start_matches("appearance:"),
            &format!("{}: {}", check.status.as_str(), check.message),
        ));
        if let Some(remediation) = &check.remediation {
            lines.push(render_config_hint_row(remediation));
        }
    }
    if warnings.len() > 3 {
        lines.push(format!("... {} more warnings", warnings.len() - 3));
    }
    lines
}

pub(super) fn render_appearance_preview_lines(config_state: &ConfigState) -> Vec<String> {
    let saved = config_state.draft.base_root_config.appearance.theme;
    let draft = config_state.draft.appearance_theme;
    let state = if saved == draft {
        "saved"
    } else {
        "unsaved draft"
    };
    vec![
        format!(
            "preview compare: current {} -> draft {} ({state})",
            saved.as_str(),
            draft.as_str()
        ),
        format!(
            "preview syntax: {} -> {}",
            config_state.draft.appearance_syntax_theme.as_str(),
            config_state
                .draft
                .resolved_appearance_syntax_theme()
                .display_label()
        ),
        "preview page: rail timeline composer tool modal".to_owned(),
        "preview shell: rail live composer footer".to_owned(),
        "preview composer: Build · agent: main · deepseek-v4-flash".to_owned(),
        "preview tool: read_file ✓ ok · doc excerpt · 2 hidden".to_owned(),
        "preview modal: Review Tool Call allow deny selected".to_owned(),
        format!(
            "preview token: {} {}",
            config_state.draft.selected_appearance_color_token(),
            config_state
                .draft
                .selected_appearance_color_override()
                .unwrap_or("inherited")
        ),
        "preview text: primary secondary muted".to_owned(),
        "preview status: success warning error pending".to_owned(),
        "preview diff: +added -removed @@ hunk".to_owned(),
        "preview markdown: heading link code".to_owned(),
    ]
}
