use crate::config_panel::{CONFIG_CONTROLS_HINT, CONFIG_HEADER_NOTICE};

use super::*;

#[test]
fn config_context_commands_truncate_to_content_width() {
    let line = render_config_context_line(CONFIG_CONTROLS_HINT, 30);
    let text = line_text(&line);
    let highlighted_width = line
        .spans
        .iter()
        .filter(|span| span.style.bg == Some(theme::config_tab_bg()))
        .map(|span| span.content.chars().count())
        .sum::<usize>();

    assert!(text.chars().count() <= 30);
    assert!(text.contains("keys Tab section"));
    assert!(!text.contains("controls:"));
    assert!(text.contains("..."));
    assert!(!text.contains("Enter edit"));
    assert!((12..20).contains(&highlighted_width));
}

#[test]
fn config_context_metadata_uses_chip_and_truncates_value() {
    let line = render_config_context_line(
        "key: a-very-long-config-key-that-needs-to-fit-the-details-panel",
        32,
    );
    let text = line_text(&line);
    let highlighted_width = line
        .spans
        .iter()
        .filter(|span| span.style.bg == Some(theme::config_tab_bg()))
        .map(|span| span.content.chars().count())
        .sum::<usize>();

    assert!(text.chars().count() <= 32);
    assert!(text.contains("key a-very"));
    assert!(!text.contains("meta key:"));
    assert!(text.contains("..."));
    assert_eq!(highlighted_width, "key ".chars().count());
}

#[test]
fn config_context_status_uses_state_chip() {
    let line = render_config_context_line("status: confirm close - Esc discards", 28);
    let text = line_text(&line);
    let highlighted_width = line
        .spans
        .iter()
        .filter(|span| span.style.bg == Some(theme::config_tab_bg()))
        .map(|span| span.content.chars().count())
        .sum::<usize>();

    assert!(text.chars().count() <= 28);
    assert!(text.contains("state status:"));
    assert_eq!(highlighted_width, "state ".chars().count());
}

#[test]
fn config_context_selected_field_uses_focus_chip() {
    let line = render_config_context_line("selected: Model", 24);
    let text = line_text(&line);
    let highlighted_width = highlighted_width(&line);

    assert!(text.contains("focus Model"));
    assert!(!text.contains("selected:"));
    assert_eq!(highlighted_width, "focus ".chars().count());
}

#[test]
fn config_header_notice_uses_hint_and_note_chips() {
    let hint_line = Line::from(render_config_header_notice(CONFIG_HEADER_NOTICE, 32));
    let note_line = Line::from(render_config_header_notice("opened config", 32));
    let hint_text = line_text(&hint_line);
    let note_text = line_text(&note_line);
    let hint_chip_width = highlighted_width(&hint_line);
    let note_chip_width = highlighted_width(&note_line);

    assert!(hint_text.contains("hint Tab section"));
    assert!(note_text.contains("note opened config"));
    assert_eq!(hint_chip_width, "hint ".chars().count());
    assert_eq!(note_chip_width, "note ".chars().count());
}

#[test]
fn config_step_line_only_highlights_selected_step() {
    let line = render_config_step_line("[provider] permissions memory", theme::config_primary());
    let text = line_text(&line);
    let selected_width = background_width(&line, theme::config_primary());
    let inactive_width = background_width(&line, theme::config_tab_bg());

    assert!(text.contains(" provider "));
    assert!(text.contains("permissions"));
    assert!(text.contains("memory"));
    assert_eq!(selected_width, " provider ".chars().count());
    assert_eq!(inactive_width, 0);
}

#[test]
fn centered_config_area_respects_min_max_and_side_margins() {
    let narrow = centered_config_area(Rect::new(0, 0, 60, 4));
    let wide = centered_config_area(Rect::new(0, 0, 220, 4));

    assert_eq!(narrow.width, 60);
    assert_eq!(narrow.x, 0);
    assert_eq!(wide.width, 152);
    assert_eq!(wide.x, 34);
}

#[test]
fn split_config_context_lines_trims_trailing_blanks_before_details() {
    let (main, context) = split_config_context_lines(vec![
        "Config".to_owned(),
        String::new(),
        String::new(),
        "[details]".to_owned(),
        "selected: Model".to_owned(),
    ]);

    assert_eq!(main, vec!["Config".to_owned()]);
    assert_eq!(context, vec!["selected: Model".to_owned()]);
}

#[test]
fn config_scroll_offset_keeps_focus_visible_with_padding() {
    assert_eq!(config_scroll_offset(20, 0, &[10]), 0);
    assert_eq!(config_scroll_offset(5, 8, &[4]), 0);
    assert_eq!(config_scroll_offset(20, 8, &[5]), 0);
    assert_eq!(config_scroll_offset(20, 8, &[6]), 4);
    assert_eq!(config_scroll_offset(20, 8, &[9]), 7);
    assert_eq!(config_scroll_offset(20, 8, &[]), 0);
}

#[test]
fn footer_status_spans_strip_status_prefix_and_handle_tight_widths() {
    let marker_only = Line::from(footer_status_spans(
        "status: dirty",
        3,
        Style::default().fg(theme::config_warning()),
    ));
    let full = Line::from(footer_status_spans(
        "status: dirty",
        16,
        Style::default().fg(theme::config_warning()),
    ));

    assert_eq!(line_text(&marker_only), "sta");
    assert_eq!(line_text(&full), "state dirty");
    assert_eq!(highlighted_width(&full), "state ".chars().count());
}

fn line_text(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

fn highlighted_width(line: &Line<'_>) -> usize {
    line.spans
        .iter()
        .filter(|span| span.style.bg == Some(theme::config_tab_bg()))
        .map(|span| span.content.chars().count())
        .sum()
}

fn background_width(line: &Line<'_>, background: Color) -> usize {
    line.spans
        .iter()
        .filter(|span| span.style.bg == Some(background))
        .map(|span| span.content.chars().count())
        .sum()
}

#[test]
fn centered_config_area_respects_narrow_and_wide_bounds() {
    let wide = centered_config_area(Rect::new(0, 0, 220, 10));
    let narrow = centered_config_area(Rect::new(3, 0, 60, 10));

    assert_eq!(wide.width, CONFIG_CONTENT_MAX_WIDTH);
    assert_eq!(wide.x, 34);
    assert_eq!(narrow.width, 60);
    assert_eq!(narrow.x, 3);
}

#[test]
fn render_config_header_notice_handles_zero_and_marker_only_widths() {
    assert!(render_config_header_notice(CONFIG_HEADER_NOTICE, 0).is_empty());

    let marker_only = Line::from(render_config_header_notice(CONFIG_HEADER_NOTICE, 4));

    assert_eq!(marker_only.spans.len(), 1);
    assert!(line_text(&marker_only).chars().count() <= 4);
    assert_eq!(marker_only.spans[0].style.bg, Some(theme::config_tab_bg()));
}

#[test]
fn config_header_span_helpers_stop_when_remaining_width_is_exhausted() {
    let mut spans = Vec::new();
    let mut remaining = 0;

    assert!(!push_config_header_span(
        &mut spans,
        &mut remaining,
        "Sigil config",
        Style::default(),
    ));

    remaining = 3;
    push_config_header_pair(&mut spans, &mut remaining, "field", "Model");
    let text = spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();

    assert!(!text.contains("Model"));
    assert!(text.chars().count() <= 3);
}

#[test]
fn config_panel_height_and_top_alignment_clamp_to_available_space() {
    let lines = vec![String::from("one"); 3];

    assert_eq!(config_panel_height(&lines, &[], 0), 0);
    assert_eq!(config_panel_height(&lines, &[], 5), 5);
    assert_eq!(
        top_aligned_config_area(Rect::new(1, 2, 80, 4), 10),
        Rect::new(1, 2, 80, 4)
    );
}

#[test]
fn config_scroll_offset_handles_padding_empty_focus_and_short_viewports() {
    assert_eq!(config_scroll_offset(20, 0, &[10]), 0);
    assert_eq!(config_scroll_offset(4, 8, &[3]), 0);
    assert_eq!(config_scroll_offset(20, 8, &[]), 0);
    assert_eq!(config_scroll_offset(20, 8, &[5]), 0);
    assert_eq!(config_scroll_offset(20, 8, &[12]), 10);
    assert_eq!(config_scroll_offset(20, 4, &[6]), 6);
}

#[test]
fn split_config_context_lines_trims_trailing_blank_lines_before_details() {
    let (main, context) = split_config_context_lines(vec![
        "Provider 1/5".to_owned(),
        String::new(),
        "[details]".to_owned(),
        "selected: Model".to_owned(),
    ]);
    let (plain, no_context) = split_config_context_lines(vec!["Provider 1/5".to_owned()]);

    assert_eq!(main, vec!["Provider 1/5"]);
    assert_eq!(context, vec!["selected: Model"]);
    assert_eq!(plain, vec!["Provider 1/5"]);
    assert!(no_context.is_empty());
}

#[test]
fn footer_helpers_cover_compact_toolbar_and_status_prefix_stripping() {
    assert_eq!(footer_action_accent("activate"), theme::config_detail());
    assert_eq!(footer_action_accent("close"), theme::config_danger());
    assert_eq!(footer_action_accent("other"), theme::config_primary());
    assert_eq!(footer_action_text("save", false, true), "[save]");
    assert_eq!(
        footer_action_text("save", true, false).chars().count(),
        CONFIG_FOOTER_BUTTON_WIDTH
    );
    assert_eq!(
        footer_action_width("save", true, false),
        CONFIG_FOOTER_BUTTON_WIDTH
    );
    assert_eq!(footer_status_value("status: saved"), "saved");
    assert_eq!(footer_status_value("saved"), "saved");
}

#[test]
fn footer_status_spans_handle_zero_and_marker_only_widths() {
    assert!(footer_status_spans("status: saved", 0, Style::default()).is_empty());

    let marker_only = Line::from(footer_status_spans("status: saved", 3, Style::default()));
    let full = Line::from(footer_status_spans(
        "status: unsaved - save before close",
        18,
        Style::default().fg(theme::config_warning()),
    ));

    assert_eq!(line_text(&marker_only), "sta");
    assert!(line_text(&full).contains("state "));
    assert!(!line_text(&full).contains("status: "));
}

#[test]
fn render_config_context_pair_handles_special_and_default_labels() {
    let selected = render_config_context_pair("selected", "Model", 24);
    let actions = render_config_context_pair("actions", "Enter save · Esc close", 28);
    let mcp = render_config_context_pair("mcp", "Ctrl-N new server", 22);
    let advanced = render_config_context_pair("advanced", "provider.beta", 28);
    let override_line = render_config_context_pair("override", "env", 20);
    let status = render_config_context_pair("status", "unsaved - save before close", 28);
    let other = render_config_context_pair("owner", "workspace", 24);

    assert!(line_text(&selected).contains("focus Model"));
    assert!(line_text(&actions).contains("actions Enter save"));
    assert!(line_text(&mcp).contains("mcp Ctrl-N"));
    assert!(line_text(&advanced).contains("advanced provider.beta"));
    assert!(line_text(&override_line).contains("source env"));
    assert!(line_text(&status).contains("state status:"));
    assert!(line_text(&other).contains("owner: workspace"));
}

#[test]
fn command_token_helpers_cover_zero_capacity_and_suffix_truncation() {
    let mut spans = Vec::new();
    assert_eq!(
        push_config_command_token(&mut spans, "Enter apply", 0),
        (0, true)
    );

    spans.clear();
    let (width, truncated) = push_config_command_token(&mut spans, "Enter apply", 2);
    let short = spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();

    assert_eq!(width, 2);
    assert!(truncated);
    assert_eq!(short, "En");

    spans.clear();
    let (_, truncated) = push_config_command_token(&mut spans, "Enter apply", 8);
    let mixed = spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();

    assert!(mixed.contains("Enter"));
    assert!(mixed.contains(" ap"));
    assert!(truncated);
}

#[test]
fn render_config_context_info_and_metadata_lines_use_expected_markers() {
    let info = render_config_context_info_line("Move to a field to inspect it.", 24);
    let key = render_config_metadata_line("key", "model", 18);
    let override_line = render_config_metadata_line("override", "env", 18);
    let unknown = render_config_metadata_line("other", "value", 18);

    assert!(line_text(&info).contains("i Move"));
    assert!(line_text(&key).contains("key model"));
    assert!(line_text(&override_line).contains("source env"));
    assert!(line_text(&unknown).contains("meta value"));
}

#[test]
fn render_config_status_line_uses_danger_warning_and_primary_styles() {
    let confirm = render_config_status_line("confirm close - Esc discards", 40);
    let unsaved = render_config_status_line("unsaved - save before close", 40);
    let saved = render_config_status_line("saved", 20);

    assert_eq!(confirm.spans[2].style.fg, Some(theme::config_danger()));
    assert_eq!(unsaved.spans[2].style.fg, Some(theme::config_warning()));
    assert_eq!(saved.spans[2].style.fg, Some(theme::config_primary()));
}

#[test]
fn readonly_and_hint_helpers_return_none_for_unmatched_lines() {
    assert!(render_readonly_line("Documents: 3", 32).is_none());
    assert!(render_hint_line("Type value").is_none());

    let readonly = render_readonly_line("- Documents: 3", 32).expect("readonly row should render");
    let hint = render_hint_line("i Press Enter").expect("hint row should render");

    assert!(line_text(&readonly).contains("read Documents"));
    assert!(line_text(&hint).contains("i Press Enter"));
}

#[test]
fn render_setup_line_covers_title_defaults_warning_form_and_plain_text() {
    let title = render_setup_line("Quick setup");
    let defaults = render_setup_line("defaults: ask");
    let warning = render_setup_line("Enter save");
    let form = render_setup_line("> Model: deepseek-v4-flash  [Enter choose]");
    let plain = render_setup_line("workspace ready");

    assert_eq!(title.style.fg, Some(Color::White));
    assert_eq!(defaults.style.fg, Some(Color::DarkGray));
    assert_eq!(warning.style.fg, Some(Color::Yellow));
    assert!(line_text(&form).contains("[choose]"));
    assert_eq!(plain.style.fg, Some(Color::Gray));
}

#[test]
fn render_form_line_covers_button_rows_actions_and_invalid_input() {
    let button = render_form_line("> [save]", theme::config_primary(), 48)
        .expect("button row should render");
    let action = render_form_line(
        "  Endpoint: https://api.deepseek.com  [Enter input]",
        theme::config_primary(),
        72,
    )
    .expect("form row should render");

    assert!(
        render_form_line(
            "Endpoint https://api.deepseek.com",
            theme::config_primary(),
            72
        )
        .is_none()
    );
    assert!(line_text(&button).contains("[save]"));
    assert!(line_text(&action).contains("[input]"));
    assert!(!line_text(&action).contains("Enter input"));
}

#[test]
fn render_config_line_routes_warning_meta_field_and_muted_variants() {
    let warning = render_config_line(2, "Enter edit", 48);
    let meta = render_config_line(2, "cfg: sigil.toml", 48);
    let field = render_config_line(2, "* Model: deepseek-v4-flash", 48);
    let muted = render_config_line(2, "Provider summary", 48);

    assert_eq!(warning.style.fg, Some(theme::config_warning()));
    assert_eq!(meta.style.fg, Some(theme::dim()));
    assert_eq!(field.style.fg, Some(theme::ink()));
    assert_eq!(muted.style.fg, Some(theme::muted()));
}

#[test]
fn config_width_helpers_and_fit_config_value_cover_edge_cases() {
    assert_eq!(config_action_display_label("Enter choose"), "choose");
    assert_eq!(config_action_display_label("input"), "input");
    assert_eq!(
        config_form_label_width(72, "API key"),
        CONFIG_FORM_LABEL_WIDTH
    );
    assert_eq!(
        config_form_label_width(32, "API key"),
        "API key".chars().count()
    );
    assert_eq!(
        config_action_chip_width("go"),
        CONFIG_FORM_ACTION_CHIP_WIDTH
    );
    assert_eq!(
        config_action_chip_width("very-long-action"),
        "very-long-action".chars().count() + 2
    );
    assert_eq!(available_config_value_width(8, 6, 6), 0);
    assert_eq!(fit_config_value("value", 5), "value");
    assert_eq!(fit_config_value("value", 0), "");
    assert_eq!(fit_config_value("value", 3), "val");
    assert_eq!(fit_config_value("deepseek-v4-flash", 8), "dee...sh");
}

#[test]
fn finish_form_line_title_and_subsection_helpers_preserve_visual_structure() {
    let selected = finish_form_line(
        vec![Span::styled("selected", Style::default())],
        true,
        theme::config_selected_bg(),
    );
    let plain = finish_form_line(
        vec![Span::styled("plain", Style::default())],
        false,
        Color::Black,
    );
    let title_only = render_config_title_line("Provider");
    let title_summary = render_config_title_line("Provider 1/5 · saved");
    let subsection = render_subsection_line("[provider]", Color::Yellow);
    let config_subsection =
        render_config_subsection_line("[provider]", theme::config_primary(), 12);

    assert_eq!(selected.style.bg, Some(theme::config_selected_bg()));
    assert_eq!(plain.style.bg, None);
    assert_eq!(line_text(&title_only), "Provider");
    assert!(line_text(&title_summary).contains("1/5"));
    assert!(line_text(&subsection).contains(" provider "));
    assert!(line_text(&config_subsection).contains(" provider "));
}

#[test]
fn selected_row_and_line_classifier_helpers_cover_known_variants() {
    assert_eq!(selected_row_bg(Color::Yellow), Color::Rgb(51, 43, 14));
    assert_eq!(selected_row_bg(Color::Green), Color::Rgb(14, 36, 22));
    assert_eq!(selected_row_bg(Color::Cyan), Color::Rgb(14, 32, 36));
    assert_eq!(
        selected_row_bg(theme::config_primary()),
        theme::config_selected_bg()
    );
    assert_eq!(selected_row_bg(Color::Blue), Color::Rgb(28, 32, 30));

    assert!(config_line_is_meta("cfg: sigil.toml"));
    assert!(config_line_is_meta("Ctrl-N add server"));
    assert!(!config_line_is_meta("Model: deepseek-v4-flash"));
    assert!(config_line_looks_like_field("> Model"));
    assert!(config_line_looks_like_field(" selected row"));
    assert!(!config_line_looks_like_field("Model"));
}
