use ratatui::style::{Color, Style};

use super::*;

#[test]
fn streaming_render_cursor_reuses_stable_layout_and_matches_document_render() {
    let palette = crate::ui::theme::default_palette();
    let options = MarkdownRenderOptions::timeline(80).with_phase(MarkdownPhase::Streaming);
    let body_style = Style::default().fg(palette.text_primary);
    let first_source = "```text\nvalue\n```\n\nStable paragraph.\n\nLive";
    let mut cursor = MarkdownRenderCursor::default();
    let first = render_markdown_timeline_lines_with_palette_and_cursor(
        Color::Cyan,
        body_style,
        first_source,
        options,
        &palette,
        "assistant-1",
        &mut cursor,
    );
    let first_document = render_projected_markdown_timeline_lines_with_palette(
        Color::Cyan,
        body_style,
        first_source,
        options,
        &palette,
        0,
    );
    assert_eq!(first, first_document);

    let appended_source = "```text\nvalue\n```\n\nStable paragraph.\n\nLive tail";
    let appended = render_markdown_timeline_lines_with_palette_and_cursor(
        Color::Cyan,
        body_style,
        appended_source,
        options,
        &palette,
        "assistant-1",
        &mut cursor,
    );
    let appended_document = render_projected_markdown_timeline_lines_with_palette(
        Color::Cyan,
        body_style,
        appended_source,
        options,
        &palette,
        0,
    );

    assert_eq!(appended, appended_document);
    assert_eq!(cursor.last_reused_stable_blocks(), 2);
}
