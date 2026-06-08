use std::collections::BTreeSet;

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use crate::app::{TimelineEntry, TimelineRole};

use super::{
    markdown::{
        MarkdownRenderOptions, MarkdownRenderState, render_code_line_spans,
        render_inline_markdown_spans_with_options, render_markdown_spans,
        render_markdown_timeline_lines,
    },
    primitives::{
        spans_with_background, timeline_content_line, timeline_header_line,
        timeline_minor_header_line,
    },
    text::{pad_display_width, wrap_display_width},
    theme::{
        accent_blue, accent_gold, accent_lime, accent_rose, dim, ink, muted, selector_accent,
        user_message_bg,
    },
    tool_card::render_tool_entry_lines,
};

#[derive(Clone, Default)]
pub(crate) struct TimelineRenderOptions {
    pub expand_tool_previews: bool,
    pub expand_thinking_blocks: bool,
    pub selected_tool_entry: Option<usize>,
    pub expanded_tool_entries: BTreeSet<usize>,
    pub max_content_width: usize,
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn render_timeline_entry_lines(entry: &TimelineEntry) -> Vec<Line<'static>> {
    let options = TimelineRenderOptions::default();
    render_timeline_entry_lines_with_options(entry, &options, 0)
}

pub(crate) fn render_timeline_entry_lines_with_options(
    entry: &TimelineEntry,
    options: &TimelineRenderOptions,
    entry_index: usize,
) -> Vec<Line<'static>> {
    let lines = if entry.role == TimelineRole::User {
        render_user_entry_lines(entry, options.max_content_width)
    } else if entry.role == TimelineRole::Assistant {
        render_assistant_entry_lines(entry, options.max_content_width)
    } else if entry.role == TimelineRole::Phase {
        render_phase_entry_lines(entry)
    } else if entry.role == TimelineRole::Thinking {
        render_thinking_entry_lines(
            entry,
            options.expand_thinking_blocks,
            options.max_content_width,
        )
    } else if entry.role == TimelineRole::Tool {
        render_tool_entry_lines(entry, options, entry_index)
    } else if entry.role == TimelineRole::Notice {
        render_notice_entry_lines(entry)
    } else {
        let mut lines = vec![timeline_header_line("system", Color::Cyan, "")];
        let mut markdown_state = MarkdownRenderState::default();
        let markdown_options = MarkdownRenderOptions::timeline(options.max_content_width);
        if !entry.text.is_empty() {
            for chunk in entry.text.split('\n') {
                let content = render_timeline_content_spans(
                    entry.role,
                    chunk,
                    Style::default().fg(muted()),
                    &mut markdown_state,
                    markdown_options,
                );
                lines.push(timeline_content_line(Color::Cyan, content));
            }
        }
        lines
    };
    append_entry_gap(lines)
}

fn append_entry_gap(mut lines: Vec<Line<'static>>) -> Vec<Line<'static>> {
    if !lines.is_empty() {
        lines.push(Line::raw(String::new()));
    }
    lines
}

fn render_user_entry_lines(entry: &TimelineEntry, max_content_width: usize) -> Vec<Line<'static>> {
    let accent = selector_accent();
    let bubble_bg = user_message_bg();
    let mut lines = Vec::new();
    if entry.text.trim().is_empty() {
        return lines;
    }
    let content_width = max_content_width.saturating_sub(8).max(18);
    lines.push(user_bubble_padding_line(accent, bubble_bg, content_width));
    for line in entry.text.lines() {
        if line.trim().is_empty() {
            lines.push(Line::raw(String::new()));
            continue;
        }
        for row in wrap_display_width(line, content_width) {
            lines.push(user_bubble_content_line(
                &row,
                accent,
                bubble_bg,
                content_width,
            ));
        }
    }
    lines.push(user_bubble_padding_line(accent, bubble_bg, content_width));
    lines
}

fn user_bubble_padding_line(
    accent: Color,
    bubble_bg: Color,
    content_width: usize,
) -> Line<'static> {
    let mut spans = vec![Span::styled(
        "▌  ",
        Style::default()
            .fg(accent)
            .bg(bubble_bg)
            .add_modifier(Modifier::BOLD),
    )];
    spans.push(Span::styled(
        " ".repeat(content_width),
        Style::default().bg(bubble_bg),
    ));
    spans.push(Span::styled("  ", Style::default().bg(bubble_bg)));
    Line::from(spans)
}

fn user_bubble_content_line(
    row: &str,
    accent: Color,
    bubble_bg: Color,
    content_width: usize,
) -> Line<'static> {
    let padded = pad_display_width(row, content_width);
    let mut spans = vec![Span::styled(
        "▌  ",
        Style::default()
            .fg(accent)
            .bg(bubble_bg)
            .add_modifier(Modifier::BOLD),
    )];
    spans.extend(spans_with_background(
        render_inline_markdown_spans_with_options(
            &padded,
            Style::default()
                .fg(Color::Rgb(230, 236, 244))
                .add_modifier(Modifier::BOLD),
            MarkdownRenderOptions::timeline(content_width),
        ),
        bubble_bg,
    ));
    spans.push(Span::styled("  ", Style::default().bg(bubble_bg)));
    Line::from(spans)
}

fn render_assistant_entry_lines(
    entry: &TimelineEntry,
    max_content_width: usize,
) -> Vec<Line<'static>> {
    let accent = accent_blue();
    if entry.text.trim().is_empty() {
        return Vec::new();
    }
    render_markdown_timeline_lines(
        accent,
        Style::default().fg(ink()),
        &entry.text,
        MarkdownRenderOptions::timeline(max_content_width),
    )
}

fn render_thinking_entry_lines(
    entry: &TimelineEntry,
    expanded: bool,
    max_content_width: usize,
) -> Vec<Line<'static>> {
    let accent = Color::Rgb(158, 148, 120);
    let body_style = Style::default()
        .fg(Color::Rgb(170, 166, 152))
        .add_modifier(Modifier::ITALIC);
    let mut lines = vec![Line::from(vec![
        Span::styled(
            "thought",
            Style::default()
                .fg(accent)
                .add_modifier(Modifier::ITALIC | Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            if expanded {
                format!(
                    "{} lines · Ctrl-T collapse",
                    thinking_line_count(&entry.text)
                )
            } else {
                format!(
                    "{} · {} lines · Ctrl-T expand",
                    summarize_thinking_text(&entry.text, 64),
                    thinking_line_count(&entry.text)
                )
            },
            Style::default().fg(dim()).add_modifier(Modifier::ITALIC),
        ),
    ])];
    if entry.text.trim().is_empty() {
        return lines;
    }
    if !expanded {
        return lines;
    }
    lines.extend(render_markdown_timeline_lines(
        accent,
        body_style,
        &entry.text,
        MarkdownRenderOptions::timeline(max_content_width),
    ));
    lines
}

fn render_phase_entry_lines(entry: &TimelineEntry) -> Vec<Line<'static>> {
    let (kind, detail) = entry
        .text
        .split_once('|')
        .map(|(kind, detail)| (kind, Some(detail)))
        .unwrap_or((entry.text.as_str(), None));
    let (label, accent, summary) = match kind {
        "thinking" => (
            "thinking",
            accent_gold(),
            detail
                .map(|model| format!("reasoning with {model}"))
                .unwrap_or_else(|| "reasoning".to_owned()),
        ),
        "tool" => (
            "tool",
            accent_rose(),
            detail
                .map(|tool| format!("running {tool}"))
                .unwrap_or_else(|| "running tool".to_owned()),
        ),
        "streaming" => ("streaming", accent_blue(), "writing the reply".to_owned()),
        _ => ("phase", muted(), entry.text.clone()),
    };

    vec![
        timeline_minor_header_line(label, accent, "live"),
        timeline_content_line(
            accent,
            vec![Span::styled(summary, Style::default().fg(dim()))],
        ),
    ]
}

fn thinking_line_count(text: &str) -> usize {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .count()
        .max(1)
}

fn summarize_thinking_text(text: &str, max_chars: usize) -> String {
    let first = text
        .lines()
        .find_map(|line| {
            let trimmed = line.trim();
            (!trimmed.is_empty()).then_some(trimmed)
        })
        .unwrap_or("thinking hidden");
    if first.chars().count() <= max_chars {
        return first.to_owned();
    }
    let truncated = first.chars().take(max_chars).collect::<String>();
    format!("{truncated}...")
}

fn render_notice_entry_lines(entry: &TimelineEntry) -> Vec<Line<'static>> {
    let accent = notice_accent(&entry.text);
    let mut lines = vec![timeline_header_line(
        "notice",
        accent,
        notice_tone_label(accent),
    )];
    for line in entry.text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        lines.push(timeline_content_line(
            accent,
            render_notice_body_spans(line, accent),
        ));
    }
    lines
}

fn notice_tone_label(accent: Color) -> &'static str {
    if accent == accent_rose() {
        "error"
    } else if accent == accent_lime() {
        "ok"
    } else {
        "info"
    }
}

fn render_timeline_content_spans(
    role: TimelineRole,
    line: &str,
    base_style: Style,
    state: &mut MarkdownRenderState,
    markdown_options: MarkdownRenderOptions,
) -> Vec<Span<'static>> {
    match role {
        TimelineRole::Assistant => render_markdown_spans(line, base_style, state, markdown_options),
        TimelineRole::Thinking => render_markdown_spans(line, base_style, state, markdown_options),
        TimelineRole::Tool => {
            render_code_line_spans(line, accent_rose(), Style::default().fg(ink()))
        }
        TimelineRole::System | TimelineRole::Phase | TimelineRole::Notice => {
            render_inline_markdown_spans_with_options(
                line,
                base_style.add_modifier(Modifier::BOLD),
                markdown_options,
            )
        }
        TimelineRole::User => vec![Span::styled(line.to_owned(), base_style)],
    }
}

fn notice_accent(text: &str) -> Color {
    let lower = text.to_ascii_lowercase();
    if lower.contains("failed")
        || lower.contains("error")
        || lower.contains("deny")
        || lower.contains("missing")
    {
        accent_rose()
    } else if lower.contains("approved")
        || lower.contains("restored")
        || lower.contains("ready")
        || lower.contains("saved")
    {
        accent_lime()
    } else {
        accent_gold()
    }
}

fn render_notice_body_spans(line: &str, accent: Color) -> Vec<Span<'static>> {
    if let Some((label, value)) = line.split_once(':') {
        let mut spans = vec![];
        spans.push(Span::styled(
            format!("{label}:"),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw(" "));
        spans.extend(render_inline_markdown_spans_with_options(
            value.trim_start(),
            Style::default().fg(ink()),
            MarkdownRenderOptions::timeline(80),
        ));
        return spans;
    }
    render_inline_markdown_spans_with_options(
        line,
        Style::default().fg(ink()),
        MarkdownRenderOptions::timeline(80),
    )
}

#[cfg(test)]
mod tests {
    use unicode_width::UnicodeWidthStr;

    use super::*;

    #[test]
    fn render_timeline_entry_lines_preserves_multiline_blocks() {
        let entry = TimelineEntry {
            role: TimelineRole::Tool,
            text: "first line\nsecond line\nthird line".to_owned(),
        };

        let lines = render_timeline_entry_lines_with_options(
            &entry,
            &TimelineRenderOptions {
                expand_tool_previews: true,
                ..TimelineRenderOptions::default()
            },
            0,
        );

        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("text"))
        }));
        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("first line"))
        }));
    }

    #[test]
    fn render_timeline_entry_lines_separates_tool_header_and_json_body() {
        let entry = TimelineEntry {
            role: TimelineRole::Tool,
            text: r#"{"tool_name":"ls","status":"ok","call_id":"call_123","metadata":{"exit_code":0}}"#
                .to_owned(),
        };

        let lines = render_timeline_entry_lines(&entry);

        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("call_123"))
        }));
        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("meta"))
        }));
    }

    #[test]
    fn render_timeline_entry_lines_styles_basic_markdown() {
        let entry = TimelineEntry {
            role: TimelineRole::Assistant,
            text: "## Title\n- **bold** and `code`".to_owned(),
        };

        let lines = render_timeline_entry_lines(&entry);

        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("Title"))
        }));
        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("─"))
        }));
        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("bold"))
        }));
        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("code"))
        }));
    }

    #[test]
    fn render_timeline_entry_lines_compacts_assistant_blank_lines() {
        let entry = TimelineEntry {
            role: TimelineRole::Assistant,
            text: "hello\n\n## Title\n\n- item".to_owned(),
        };

        let lines = render_timeline_entry_lines(&entry);

        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("hello"))
        }));
        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("Title"))
        }));
        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("─"))
        }));
        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("item"))
        }));
    }

    #[test]
    fn render_timeline_entry_lines_preserves_cjk_adjacency() {
        let entry = TimelineEntry {
            role: TimelineRole::Assistant,
            text: "你好！很高兴再次见到你。".to_owned(),
        };

        let lines = render_timeline_entry_lines(&entry);
        let rendered = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert!(rendered.contains("你好！很高兴再次见到你。"));
        assert!(!rendered.contains("你 好"));
    }

    #[test]
    fn render_timeline_entry_lines_show_phase_block() {
        let entry = TimelineEntry {
            role: TimelineRole::Phase,
            text: "thinking|deepseek-v4-flash".to_owned(),
        };

        let lines = render_timeline_entry_lines(&entry);

        assert!(
            lines[0]
                .spans
                .iter()
                .any(|span| span.content.as_ref().contains("thinking"))
        );
        assert!(lines[1].spans.iter().any(|span| {
            span.content
                .as_ref()
                .contains("reasoning with deepseek-v4-flash")
        }));
    }

    #[test]
    fn render_timeline_entry_lines_show_thinking_trace_block() {
        let entry = TimelineEntry {
            role: TimelineRole::Thinking,
            text: "step 1\nstep 2".to_owned(),
        };

        let lines = render_timeline_entry_lines(&entry);

        assert!(
            lines[0]
                .spans
                .iter()
                .any(|span| span.content.as_ref().contains("thought"))
        );
        assert!(
            lines[0]
                .spans
                .iter()
                .any(|span| span.content.as_ref().contains("Ctrl-T expand"))
        );
        assert!(
            lines[0]
                .spans
                .iter()
                .any(|span| span.content.as_ref().contains("step 1"))
        );

        let expanded = render_timeline_entry_lines_with_options(
            &entry,
            &TimelineRenderOptions {
                expand_thinking_blocks: true,
                ..TimelineRenderOptions::default()
            },
            0,
        );
        assert!(
            expanded[0]
                .spans
                .iter()
                .any(|span| span.content.as_ref().contains("Ctrl-T collapse"))
        );
        assert!(expanded.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("step 1"))
        }));
        assert!(expanded.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("step 2"))
        }));
    }

    #[test]
    fn render_timeline_entry_lines_make_user_and_assistant_distinct() {
        let user = TimelineEntry {
            role: TimelineRole::User,
            text: "hello".to_owned(),
        };
        let assistant = TimelineEntry {
            role: TimelineRole::Assistant,
            text: "hello back".to_owned(),
        };

        let user_lines = render_timeline_entry_lines(&user);
        let assistant_lines = render_timeline_entry_lines(&assistant);

        assert!(
            user_lines[0]
                .spans
                .iter()
                .any(|span| span.style.bg == Some(user_message_bg()))
        );
        assert!(
            !assistant_lines
                .iter()
                .flat_map(|line| line.spans.iter())
                .any(|span| span.style.bg == Some(user_message_bg()))
        );
    }

    #[test]
    fn render_timeline_entry_lines_make_headings_primary_and_flush_left() {
        let entry = TimelineEntry {
            role: TimelineRole::Assistant,
            text: "## 关键架构决策\n正文内容".to_owned(),
        };

        let lines = render_timeline_entry_lines(&entry);
        let heading_plain = lines[0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        let body_plain = lines[2]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert!(heading_plain.starts_with("关键架构决策"));
        assert!(!heading_plain.starts_with("▏"));
        assert_eq!(body_plain.trim_start(), "正文内容");
    }

    #[test]
    fn render_timeline_entry_lines_supports_task_lists_and_links() {
        let entry = TimelineEntry {
            role: TimelineRole::Assistant,
            text: "- [x] shipped [README](https://example.com)".to_owned(),
        };

        let lines = render_timeline_entry_lines(&entry);

        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("[x]"))
        }));
        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("README"))
        }));
        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("example.com"))
        }));
    }

    #[test]
    fn render_timeline_entry_lines_groups_paragraphs_and_code_blocks() {
        let entry = TimelineEntry {
            role: TimelineRole::Assistant,
            text: "first line\nsecond line\n\n```rust\nfn main() {}\n```".to_owned(),
        };

        let lines = render_timeline_entry_lines(&entry);

        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("first line"))
        }));
        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("second line"))
        }));
        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("rust"))
        }));
        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("fn main() {}"))
        }));
    }

    #[test]
    fn render_timeline_entry_lines_formats_markdown_tables_as_grid() {
        let entry = TimelineEntry {
            role: TimelineRole::Assistant,
            text: "| file | role |\n| --- | --- |\n| Cargo.toml | root |\n| src/lib.rs | core |"
                .to_owned(),
        };

        let lines = render_timeline_entry_lines(&entry);

        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("table"))
        }));
        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("┌"))
        }));
        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("Cargo.toml"))
        }));
    }

    #[test]
    fn render_timeline_entry_lines_wrap_markdown_tables_to_panel_width() {
        let entry = TimelineEntry {
            role: TimelineRole::Assistant,
            text: "| Phase | 内容 | 状态 |\n| --- | --- | --- |\n| **Phase 3** | planner/executor + compaction + memory + subagent + workspace confinement | 部分完成 |".to_owned(),
        };

        let lines = render_timeline_entry_lines_with_options(
            &entry,
            &TimelineRenderOptions {
                max_content_width: 48,
                ..TimelineRenderOptions::default()
            },
            0,
        );

        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("planner/executor"))
        }));
        assert!(lines.iter().all(|line| {
            let plain = line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>();
            UnicodeWidthStr::width(plain.as_str()) <= 50
        }));
        assert!(lines.iter().all(|line| {
            let plain = line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>();
            !plain.contains("**Phase 3**")
        }));
    }

    #[test]
    fn render_timeline_entry_lines_formats_tool_cards() {
        let entry = TimelineEntry {
            role: TimelineRole::Tool,
            text: r#"{
  "call_id": "call-1",
  "tool_name": "ls",
  "status": "ok",
  "preview_kind": "json",
  "summary": "first 2/4 lines · 64 B",
  "preview_lines": ["[", "  \".git\",", "  \"Cargo.toml\"", "]"],
  "preview_value": [".git", "Cargo.toml"],
  "hidden_lines": 0,
  "metadata_line": "bytes=64",
  "metadata": {"bytes": 64}
}"#
            .to_owned(),
        };

        let lines = render_timeline_entry_lines_with_options(
            &entry,
            &TimelineRenderOptions {
                expand_tool_previews: true,
                ..TimelineRenderOptions::default()
            },
            0,
        );

        assert!(
            lines[0]
                .spans
                .iter()
                .any(|span| span.content.as_ref().contains("ls"))
        );
        assert!(
            lines[1]
                .spans
                .iter()
                .any(|span| span.content.as_ref().contains("OK"))
        );
        assert!(
            lines[2]
                .spans
                .iter()
                .any(|span| span.content.as_ref().contains("bytes=64"))
        );
        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("files"))
        }));
        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("Cargo.toml"))
        }));
    }

    #[test]
    fn render_timeline_entry_lines_formats_grep_cards_by_file() {
        let entry = TimelineEntry {
            role: TimelineRole::Tool,
            text: r#"{
  "tool_name": "grep",
  "status": "ok",
  "preview_kind": "json",
  "summary": "first 2/2 lines · 91 B",
  "preview_lines": ["[]"],
  "preview_value": [
    {"path": "src/lib.rs", "line": 12, "text": "fn helper()"},
    {"path": "src/lib.rs", "line": 29, "text": "helper();"}
  ],
  "hidden_lines": 0
}"#
            .to_owned(),
        };

        let lines = render_timeline_entry_lines_with_options(
            &entry,
            &TimelineRenderOptions {
                expand_tool_previews: true,
                ..TimelineRenderOptions::default()
            },
            0,
        );

        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("matches"))
        }));
        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("src/lib.rs"))
        }));
        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("L12"))
        }));
    }

    #[test]
    fn render_timeline_entry_lines_renders_generic_json_tree_preview() {
        let entry = TimelineEntry {
            role: TimelineRole::Tool,
            text: r#"{
  "tool_name": "custom_tool",
  "status": "ok",
  "preview_kind": "json",
  "summary": "first 3/3 lines · 44 B",
  "preview_lines": ["{}"],
  "preview_value": {"root": {"leaf": "value"}},
  "hidden_lines": 0
}"#
            .to_owned(),
        };

        let lines = render_timeline_entry_lines_with_options(
            &entry,
            &TimelineRenderOptions {
                expand_tool_previews: true,
                ..TimelineRenderOptions::default()
            },
            0,
        );

        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("tree"))
        }));
        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("root"))
        }));
        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("leaf"))
        }));
    }

    #[test]
    fn render_timeline_entry_lines_hide_tool_preview_by_default() {
        let entry = TimelineEntry {
            role: TimelineRole::Tool,
            text: r##"{
  "tool_name": "read_file",
  "status": "ok",
  "preview_kind": "markdown",
  "summary": "first 2/2 lines · 18 B",
  "preview_lines": ["# Title", "- item"],
  "hidden_lines": 0
}"##
            .to_owned(),
        };

        let lines = render_timeline_entry_lines(&entry);

        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("preview hidden"))
        }));
        assert!(!lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains("# Title"))
        }));
    }
}
