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
fn config_header_notice_degrades_to_marker_when_width_is_tiny() {
    let spans = render_config_header_notice(CONFIG_HEADER_NOTICE, 3);
    let text = spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();

    assert_eq!(spans.len(), 1);
    assert_eq!(text, "hin");
}

#[test]
fn config_subsection_line_uses_available_separator_width() {
    let line = render_config_subsection_line("[provider settings]", theme::config_primary(), 28);
    let text = line_text(&line);

    assert!(text.contains(" provider settings "));
    assert!(text.contains("─"));
    assert!(text.starts_with("  "));
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
