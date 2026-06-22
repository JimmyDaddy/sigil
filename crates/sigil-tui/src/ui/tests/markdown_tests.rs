use ratatui::{
    style::{Color, Modifier, Style},
    text::Span,
};

use super::*;

fn plain_text(lines: &[Line<'static>]) -> String {
    lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect::<String>()
}

fn line_plain_text(line: &Line<'static>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

#[test]
fn markdown_timeline_lines_render_code_blocks() {
    let lines = render_markdown_timeline_lines(
        Color::Cyan,
        Style::default(),
        "```rust\nfn main() {}\n```",
        MarkdownRenderOptions::timeline(80),
    );
    let plain = plain_text(&lines);
    assert!(plain.contains("rust"));
    assert!(plain.contains("fn main() {}"));
}

#[test]
fn markdown_timeline_lines_highlight_fenced_code_blocks() {
    let lines = render_markdown_timeline_lines(
        Color::Cyan,
        Style::default(),
        "```rust\nfn main() {}\n```",
        MarkdownRenderOptions::timeline(80),
    );
    let highlighted_fn = lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .find(|span| span.content.as_ref() == "fn")
        .expect("expected rust keyword span");

    assert_ne!(
        highlighted_fn.style,
        Style::default().fg(ink()).bg(Color::Rgb(28, 33, 41))
    );
}

#[test]
fn markdown_timeline_lines_apply_code_wrap_options() {
    let source = "```text\nabcdefghijklmnopqrstuvwxyz\n```";
    let wrapped = render_markdown_timeline_lines(
        Color::Cyan,
        Style::default(),
        source,
        MarkdownRenderOptions {
            code_wrap: CodeWrapMode::Wrap,
            ..MarkdownRenderOptions::timeline(20)
        },
    );
    let truncated = render_markdown_timeline_lines(
        Color::Cyan,
        Style::default(),
        source,
        MarkdownRenderOptions {
            code_wrap: CodeWrapMode::Truncate,
            ..MarkdownRenderOptions::timeline(20)
        },
    );

    assert!(wrapped.len() > truncated.len());
    assert!(plain_text(&truncated).contains("..."));
}

#[test]
fn markdown_table_options_compact_or_preserve_widths() {
    let source = "| name | description |\n| --- | --- |\n| 文件 | very very long value |";
    let compact = render_markdown_timeline_lines(
        Color::Cyan,
        Style::default(),
        source,
        MarkdownRenderOptions::timeline(24),
    );
    let preserve = render_markdown_timeline_lines(
        Color::Cyan,
        Style::default(),
        source,
        MarkdownRenderOptions {
            table_mode: TableRenderMode::Preserve,
            ..MarkdownRenderOptions::timeline(24)
        },
    );

    let compact_widest = compact
        .iter()
        .map(|line| UnicodeWidthStr::width(plain_text(std::slice::from_ref(line)).as_str()))
        .max()
        .unwrap_or(0);
    let preserve_widest = preserve
        .iter()
        .map(|line| UnicodeWidthStr::width(plain_text(std::slice::from_ref(line)).as_str()))
        .max()
        .unwrap_or(0);

    assert!(compact_widest <= preserve_widest);
    assert!(plain_text(&compact).contains("文件"));
}

#[test]
fn markdown_lists_keep_nesting_and_task_markers_readable() {
    let mut state = MarkdownRenderState::default();
    let nested = render_markdown_spans(
        "  - child",
        Style::default(),
        &mut state,
        MarkdownRenderOptions::timeline(80),
    );
    let task = render_markdown_spans(
        "    - [x] done",
        Style::default(),
        &mut MarkdownRenderState::default(),
        MarkdownRenderOptions::timeline(80),
    );

    assert_eq!(nested[0].content.as_ref(), "  • ");
    assert_eq!(task[0].content.as_ref(), "    [x] ");
}

#[test]
fn markdown_timeline_wraps_list_continuations_with_hanging_indent() {
    let lines = render_markdown_timeline_lines(
        Color::Cyan,
        Style::default(),
        "- abcdefghijklmnopqrstuvwxyz",
        MarkdownRenderOptions::timeline(12),
    );
    let rows = lines.iter().map(line_plain_text).collect::<Vec<_>>();

    assert!(rows.len() > 1);
    assert!(rows[0].starts_with("  • "));
    assert!(rows[1].starts_with("    "));
    assert!(!rows[1].starts_with("  ij"));
}

#[test]
fn markdown_timeline_wraps_quote_continuations_with_quote_prefix() {
    let lines = render_markdown_timeline_lines(
        Color::Cyan,
        Style::default(),
        "> abcdefghijklmnopqrstuvwxyz",
        MarkdownRenderOptions::timeline(12),
    );
    let rows = lines.iter().map(line_plain_text).collect::<Vec<_>>();

    assert!(rows.len() > 2);
    assert!(rows[1..].iter().all(|row| row.starts_with("  ▌ ")));
}

#[test]
fn markdown_timeline_wraps_code_block_rows_with_code_prefix() {
    let lines = render_markdown_timeline_lines(
        Color::Cyan,
        Style::default(),
        "```plain\nabcdefghijklmnopqrstuvwxyz\n```",
        MarkdownRenderOptions::timeline(12),
    );
    let rows = lines.iter().map(line_plain_text).collect::<Vec<_>>();
    let code_rows = rows
        .iter()
        .filter(|row| row.starts_with("  │ "))
        .collect::<Vec<_>>();

    assert!(code_rows.len() > 1);
    assert!(code_rows.iter().all(|row| row.starts_with("  │ ")));
}

#[test]
fn markdown_links_can_hide_or_truncate_urls() {
    let visible = render_inline_markdown_spans_with_options(
        "[docs](https://example.com/very/long/path)",
        Style::default(),
        MarkdownRenderOptions::timeline(24),
    );
    let hidden = render_inline_markdown_spans_with_options(
        "[docs](https://example.com/very/long/path)",
        Style::default(),
        MarkdownRenderOptions {
            show_link_urls: false,
            ..MarkdownRenderOptions::timeline(24)
        },
    );

    let visible_text = visible
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    let hidden_text = hidden
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();

    assert!(visible_text.contains("docs <https://"));
    assert!(visible_text.contains("..."));
    assert_eq!(hidden_text, "docs");
}

#[test]
fn markdown_emphasis_does_not_split_intraword_underscores() {
    let spans = render_inline_markdown_spans_with_options(
        "src/my_file_name.rs",
        Style::default(),
        MarkdownRenderOptions::timeline(80),
    );
    let text = spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();

    assert_eq!(text, "src/my_file_name.rs");
    assert_eq!(spans.len(), 1);
}

#[test]
fn markdown_plain_text_strips_inline_markup() {
    assert_eq!(
        markdown_plain_text("**Phase 3** [doc](https://example.com)"),
        "Phase 3 doc"
    );
}

#[test]
fn markdown_render_options_normalize_surface_widths() {
    let timeline = MarkdownRenderOptions::timeline(0);
    let preview = MarkdownRenderOptions::tool_preview(3);
    let modal = MarkdownRenderOptions::modal(19);

    assert_eq!(timeline.max_content_width, 80);
    assert_eq!(preview.max_content_width, 20);
    assert_eq!(modal.max_content_width, 20);
    assert_eq!(preview.code_wrap, CodeWrapMode::Preserve);
    assert!(preview.highlight_code);
    assert!(preview.show_link_urls);
}

#[test]
fn markdown_timeline_lines_render_empty_plain_fenced_code_blocks() {
    let empty = render_markdown_timeline_lines(
        Color::Cyan,
        Style::default(),
        "```\n```",
        MarkdownRenderOptions::timeline(21),
    );
    let empty_rows = empty.iter().map(line_plain_text).collect::<Vec<_>>();

    assert!(empty_rows.iter().any(|row| row.contains("plain")));
    assert!(empty_rows.iter().any(|row| row.starts_with("  │ ")));

    let no_highlight = render_markdown_timeline_lines(
        Color::Cyan,
        Style::default(),
        "```rust\nfn main() {}\n```",
        MarkdownRenderOptions {
            highlight_code: false,
            ..MarkdownRenderOptions::timeline(21)
        },
    );
    let code_line = no_highlight
        .iter()
        .find(|line| line_plain_text(line).starts_with("  │ "))
        .expect("expected rendered code row");

    assert!(
        code_line
            .spans
            .iter()
            .any(|span| span.content.as_ref() == "fn main() {}")
    );
    assert!(
        !code_line
            .spans
            .iter()
            .any(|span| span.content.as_ref() == "fn")
    );
}

#[test]
fn markdown_inline_markup_leaves_unterminated_sequences_literal() {
    let spans = render_inline_markdown_spans_with_options(
        "**bold `code [link](oops",
        Style::default(),
        MarkdownRenderOptions::timeline(80),
    );
    let text = spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();

    assert_eq!(text, "**bold `code [link](oops");
}

#[test]
fn markdown_render_spans_cover_table_dividers_and_fenced_code_state() {
    let divider = render_markdown_spans(
        "| --- | :--- |",
        Style::default(),
        &mut MarkdownRenderState::default(),
        MarkdownRenderOptions::timeline(80),
    );
    let divider_text = divider
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();

    assert!(divider_text.contains("┄"));

    let code = render_markdown_spans(
        "let value = 1;",
        Style::default(),
        &mut MarkdownRenderState {
            in_fenced_code: true,
        },
        MarkdownRenderOptions::timeline(80),
    );

    assert_eq!(code[0].content.as_ref(), "│ ");
    assert_eq!(code[1].content.as_ref(), "let value = 1;");
}

#[test]
fn markdown_heading_levels_only_underline_top_sections() {
    let lines = render_markdown_timeline_lines(
        Color::Cyan,
        Style::default(),
        "# Major\n#### Minor",
        MarkdownRenderOptions::timeline(24),
    );
    let rows = lines.iter().map(line_plain_text).collect::<Vec<_>>();

    assert_eq!(rows[0], "Major");
    assert!(rows[1].chars().all(|character| character == '─'));
    assert_eq!(rows[2], "Minor");
    assert_eq!(rows.len(), 3);
}

#[test]
fn markdown_wrapped_line_helpers_cover_rules_lists_quotes_and_code_blocks() {
    let options = MarkdownRenderOptions::timeline(24);

    let rule = render_wrapped_markdown_line(Color::Cyan, "***", Style::default(), options);
    let unchecked =
        render_wrapped_markdown_line(Color::Cyan, "* [ ] pending item", Style::default(), options);
    let ordered =
        render_wrapped_markdown_line(Color::Cyan, "12. ordered", Style::default(), options);
    let quoted = render_wrapped_markdown_line(Color::Cyan, "> quoted", Style::default(), options);
    let code = render_wrapped_markdown_line(Color::Cyan, "{ json", Style::default(), options);

    assert!(line_plain_text(&rule[0]).contains("─"));
    assert!(line_plain_text(&unchecked[0]).contains("[ ] pending item"));
    assert_eq!(unchecked[0].spans[1].style.fg, Some(accent_gold()));
    assert!(line_plain_text(&ordered[0]).contains("12. ordered"));
    assert!(line_plain_text(&quoted[0]).starts_with("  ▌ "));
    assert!(line_plain_text(&code[0]).starts_with("  │ "));
}

#[test]
fn markdown_parser_helpers_cover_rules_lists_tables_and_links() {
    assert_eq!(fenced_code_language("```rust no_run"), Some("rust no_run"));
    assert_eq!(markdown_code_language_token("rust no_run"), "rust");
    assert_eq!(markdown_heading("###### tail"), Some((6, "tail")));
    assert_eq!(markdown_heading("###   "), None);
    assert!(markdown_rule("--- ---"));
    assert_eq!(
        markdown_task_item("* [ ] pending"),
        Some((false, "pending"))
    );
    assert_eq!(markdown_task_item("* [x] done"), Some((true, "done")));
    assert_eq!(markdown_bullet_item("* bullet"), Some("bullet"));
    assert_eq!(markdown_ordered_item("42. answer"), Some(("42", "answer")));
    assert_eq!(markdown_ordered_item("42 answer"), None);
    assert_eq!(markdown_list_indent("\t  - item"), 3);
    assert_eq!(markdown_quote("> quote"), Some("quote"));
    assert_eq!(markdown_quote(">> quote"), None);
    assert!(markdown_table_line("| a | b |"));
    assert_eq!(
        markdown_link("[docs](https://example.com)"),
        Some(("docs", "https://example.com", 27))
    );
}

#[test]
fn markdown_inline_and_plain_text_helpers_cover_emphasis_code_and_fallbacks() {
    let spans = render_inline_markdown_spans_with_options(
        "*italics* `code` tail",
        Style::default(),
        MarkdownRenderOptions::timeline(80),
    );
    let italic = spans
        .iter()
        .find(|span| span.content.as_ref() == "italics")
        .expect("expected italic span");
    let code = spans
        .iter()
        .find(|span| span.content.as_ref() == "code")
        .expect("expected inline code span");

    assert!(italic.style.add_modifier.contains(Modifier::ITALIC));
    assert_eq!(code.style.bg, Some(Color::Rgb(35, 40, 48)));
    assert!(code.style.add_modifier.contains(Modifier::BOLD));
    assert_eq!(markdown_plain_text("`code` _em_"), "code em");
    assert_eq!(markdown_plain_text("`unterminated"), "`unterminated");
    assert_eq!(markdown_emphasis("**bold**"), None);
    assert_eq!(markdown_emphasis("*ok*"), Some(("ok", 4)));
    assert_eq!(emphasis_end("a_b", '_'), None);
    assert_eq!(emphasis_end("ok_", '_'), Some(2));
    assert_eq!(next_inline_marker("text `code`"), Some(5));
    assert_eq!(next_underscore_marker("a_b _c_"), Some(4));
}

#[test]
fn markdown_table_and_code_helpers_cover_padding_empty_and_highlight_paths() {
    let empty_table = render_markdown_table_block(
        Color::Cyan,
        Style::default(),
        &[],
        MarkdownRenderOptions::timeline(32),
    );
    let loose_table = render_markdown_table_block(
        Color::Cyan,
        Style::default(),
        &["| head |", "| body |"],
        MarkdownRenderOptions::timeline(32),
    );
    let row = markdown_table_row(&["x".to_owned()], &[3]);
    let row_lines = markdown_table_row_lines(&["alpha beta".to_owned()], &[5, 3]);
    let empty_code =
        render_code_line_spans_with_bg("", Color::Cyan, Style::default(), Color::Black);
    let highlighted_empty =
        render_highlighted_code_line_spans(&[], Color::Cyan, Style::default(), Color::Black);

    assert!(empty_table.is_empty());
    assert!(plain_text(&loose_table).contains("1 cols"));
    assert_eq!(row, "│ x   │");
    assert!(row_lines.len() > 1);
    assert_eq!(empty_code[1].content.as_ref(), " ");
    assert_eq!(highlighted_empty[1].content.as_ref(), " ");
    assert_eq!(clamp_table_widths(&[], 20), Vec::<usize>::new());
    assert_eq!(clamp_table_widths(&[4, 4], 5), vec![4, 4]);
    assert_eq!(markdown_table_total_width(&[]), 0);
}

#[test]
fn markdown_span_renderers_cover_headings_tables_and_code_detection_markers() {
    let heading = render_markdown_heading_block(
        3,
        "Heading",
        Style::default(),
        MarkdownRenderOptions::timeline(32),
    );
    let heading_span = heading[0]
        .spans
        .iter()
        .find(|span| span.content.as_ref() == "Heading")
        .expect("expected heading span");
    let ordered = render_markdown_spans(
        "7. seven",
        Style::default(),
        &mut MarkdownRenderState::default(),
        MarkdownRenderOptions::timeline(80),
    );
    let table = render_markdown_spans(
        "| left | |",
        Style::default(),
        &mut MarkdownRenderState::default(),
        MarkdownRenderOptions::timeline(80),
    );

    assert_eq!(heading_span.style.fg, Some(accent_lime()));
    assert_eq!(ordered[0].content.as_ref(), "7. ");
    assert_eq!(ordered[0].style.fg, Some(accent_gold()));
    assert!(
        table
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
            .contains("│ left │   │")
    );

    assert!(line_looks_like_code("\tindent"));
    assert!(line_looks_like_code("tree │ line"));
    assert!(line_looks_like_code("tree └ line"));
    assert!(line_looks_like_code("tree ├ line"));
    assert!(line_looks_like_code("tree ┌ line"));
    assert!(line_looks_like_code("rule ─ line"));
    assert!(line_looks_like_code("{ object"));
    assert!(line_looks_like_code("} object"));
    assert!(line_looks_like_code("[ array"));
    assert!(line_looks_like_code("] array"));
    assert!(!line_looks_like_code("plain text"));
}

#[test]
fn markdown_span_helpers_cover_headings_rules_lists_quotes_and_code_fallbacks() {
    let palette = crate::ui::theme::default_palette();
    for (level, accent) in [
        (1, palette.markdown_heading),
        (2, palette.accent_info),
        (3, palette.accent_success),
        (4, palette.accent_secondary),
    ] {
        let heading = render_markdown_spans(
            &format!("{} title", "#".repeat(level)),
            Style::default(),
            &mut MarkdownRenderState::default(),
            MarkdownRenderOptions::timeline(80),
        );
        assert_eq!(heading[0].style.fg, Some(accent));
        assert!(heading[0].style.add_modifier.contains(Modifier::BOLD));
    }

    let rule = render_markdown_spans(
        "---",
        Style::default(),
        &mut MarkdownRenderState::default(),
        MarkdownRenderOptions::timeline(80),
    );
    assert_eq!(rule[0].content.as_ref(), "────────────────────────────────");

    let unchecked = render_markdown_spans(
        "- [ ] todo",
        Style::default(),
        &mut MarkdownRenderState::default(),
        MarkdownRenderOptions::timeline(80),
    );
    assert_eq!(unchecked[0].content.as_ref(), "[ ] ");
    assert_eq!(unchecked[0].style.fg, Some(accent_gold()));

    let ordered = render_markdown_spans(
        "12. next",
        Style::default(),
        &mut MarkdownRenderState::default(),
        MarkdownRenderOptions::timeline(80),
    );
    assert_eq!(ordered[0].content.as_ref(), "12. ");
    assert_eq!(ordered[0].style.fg, Some(accent_gold()));

    let quote = render_markdown_spans(
        "> note",
        Style::default(),
        &mut MarkdownRenderState::default(),
        MarkdownRenderOptions::timeline(80),
    );
    assert_eq!(quote[0].content.as_ref(), "│ ");
    assert_eq!(quote[0].style.fg, Some(accent_teal()));
    assert_eq!(quote[1].style.fg, Some(muted()));

    let codeish = render_markdown_spans(
        "{ \"tool\": true }",
        Style::default(),
        &mut MarkdownRenderState::default(),
        MarkdownRenderOptions::timeline(80),
    );
    assert_eq!(codeish[0].content.as_ref(), "│ ");
}

#[test]
fn markdown_inline_and_code_helpers_cover_literal_markers_and_empty_rows() {
    let style = Style::default().fg(Color::White);
    let mut spans = vec![Span::styled("hello".to_owned(), style)];
    push_styled_text(&mut spans, "", style);
    push_styled_text(&mut spans, " world", style);
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].content.as_ref(), "hello world");

    assert_eq!(next_underscore_marker("foo_bar_baz"), None);
    assert_eq!(next_underscore_marker("foo _bar"), Some(4));
    assert_eq!(markdown_plain_text("see [broken"), "see [broken");

    let code = render_code_line_spans_with_bg("", Color::Cyan, Style::default(), Color::Black);
    assert_eq!(code[1].content.as_ref(), " ");

    let highlighted = render_highlighted_code_line_spans(
        &[],
        Color::Cyan,
        Style::default().fg(Color::White),
        Color::Black,
    );
    assert_eq!(highlighted[1].content.as_ref(), " ");
}
