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
#[path = "tests/timeline_tests.rs"]
mod tests;
