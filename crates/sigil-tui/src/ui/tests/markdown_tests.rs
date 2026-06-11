use ratatui::style::{Color, Style};

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
