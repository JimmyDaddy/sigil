use crate::config_panel::CONFIG_CONTROLS_HINT;

use super::*;

#[test]
fn config_context_commands_truncate_to_content_width() {
    let line = render_config_context_line(CONFIG_CONTROLS_HINT, 36);
    let text = line_text(&line);
    let highlighted_width = line
        .spans
        .iter()
        .filter(|span| span.style.bg == Some(theme::config_tab_bg()))
        .map(|span| span.content.chars().count())
        .sum::<usize>();

    assert!(text.chars().count() <= 36);
    assert!(text.contains("controls: Tab section"));
    assert!(text.contains("..."));
    assert!(!text.contains("Enter edit"));
    assert!((10..20).contains(&highlighted_width));
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
    assert!(text.contains("meta key:"));
    assert!(text.contains("..."));
    assert_eq!(highlighted_width, "meta ".chars().count());
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

fn line_text(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}
