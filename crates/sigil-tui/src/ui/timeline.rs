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

const COLLAPSED_THINKING_PREVIEW_LINES: usize = 3;
const COLLAPSED_THINKING_CODE_PREVIEW_LINES: usize = 2;

#[derive(Clone, Default)]
pub(crate) struct TimelineRenderOptions {
    pub expand_tool_previews: bool,
    pub expand_thinking_blocks: bool,
    pub streaming_reasoning_index: Option<usize>,
    pub selected_tool_activity_key: Option<String>,
    pub hovered_tool_activity_key: Option<String>,
    pub expanded_tool_activity_keys: BTreeSet<String>,
    pub collapsed_tool_activity_keys: BTreeSet<String>,
    pub max_content_width: usize,
    pub streaming_assistant_index: Option<usize>,
    pub intermediate_assistant_indices: BTreeSet<usize>,
    pub expanded_thinking_entry_indices: BTreeSet<usize>,
    pub collapsed_thinking_entry_indices: BTreeSet<usize>,
    pub hovered_thinking_entry_index: Option<usize>,
}

pub(crate) fn render_timeline_entry_lines_with_options(
    entry: &TimelineEntry,
    options: &TimelineRenderOptions,
    entry_index: usize,
) -> Vec<Line<'static>> {
    let lines = if entry.role == TimelineRole::User {
        render_user_entry_lines(entry, options.max_content_width)
    } else if entry.role == TimelineRole::Assistant {
        render_assistant_entry_lines(
            entry,
            options.max_content_width,
            options.streaming_assistant_index != Some(entry_index),
            options
                .intermediate_assistant_indices
                .contains(&entry_index),
        )
    } else if entry.role == TimelineRole::Phase {
        render_phase_entry_lines(entry)
    } else if entry.role == TimelineRole::Thinking {
        let active = options.streaming_reasoning_index == Some(entry_index);
        let expanded = active
            || options
                .expanded_thinking_entry_indices
                .contains(&entry_index)
            || (options.expand_thinking_blocks
                && !options
                    .collapsed_thinking_entry_indices
                    .contains(&entry_index));
        render_thinking_entry_lines(
            entry,
            active,
            expanded,
            options.hovered_thinking_entry_index == Some(entry_index),
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
    highlight_code: bool,
    intermediate_info: bool,
) -> Vec<Line<'static>> {
    let accent = accent_blue();
    if entry.text.trim().is_empty() {
        return Vec::new();
    }
    let mut lines = render_markdown_timeline_lines(
        accent,
        Style::default().fg(ink()),
        &entry.text,
        MarkdownRenderOptions {
            highlight_code,
            ..MarkdownRenderOptions::timeline(max_content_width)
        },
    );
    if intermediate_info {
        mark_first_visible_assistant_line(&mut lines);
    }
    lines
}

fn mark_first_visible_assistant_line(lines: &mut [Line<'static>]) {
    for line in lines {
        let visible = line
            .spans
            .iter()
            .any(|span| !span.content.as_ref().trim().is_empty());
        if !visible {
            continue;
        }
        if let Some(first) = line.spans.first_mut()
            && first.content.as_ref() == "  "
        {
            *first = assistant_info_marker_span();
        } else {
            line.spans.insert(0, assistant_info_marker_span());
        }
        return;
    }
}

fn assistant_info_marker_span() -> Span<'static> {
    Span::styled(
        "• ",
        Style::default().fg(dim()).add_modifier(Modifier::BOLD),
    )
}

fn render_thinking_entry_lines(
    entry: &TimelineEntry,
    active: bool,
    expanded: bool,
    hovered: bool,
    max_content_width: usize,
) -> Vec<Line<'static>> {
    let accent = if hovered {
        accent_gold()
    } else {
        Color::Rgb(158, 148, 120)
    };
    let header_modifier = if hovered {
        Modifier::ITALIC | Modifier::BOLD | Modifier::UNDERLINED
    } else {
        Modifier::ITALIC | Modifier::BOLD
    };
    let body_style = Style::default()
        .fg(Color::Rgb(170, 166, 152))
        .add_modifier(Modifier::ITALIC);
    let total_lines = thinking_line_count(&entry.text);
    let preview_lines = thinking_preview_lines(&entry.text, COLLAPSED_THINKING_PREVIEW_LINES);
    let preview_count = preview_lines.len();
    let hidden_lines = total_lines.saturating_sub(preview_count);
    let has_hidden_content = thinking_has_collapsed_content(&entry.text);
    let mut lines = vec![Line::from(vec![
        Span::styled(
            if active { "thinking" } else { "thought" },
            Style::default().fg(accent).add_modifier(header_modifier),
        ),
        Span::raw("  "),
        Span::styled(
            if expanded && has_hidden_content {
                format!("{} · Ctrl-T collapse", thinking_line_label(total_lines))
            } else if !expanded && has_hidden_content {
                format!("showing first {preview_count}/{total_lines} lines · Ctrl-T expand")
            } else {
                thinking_line_label(total_lines)
            },
            Style::default().fg(dim()).add_modifier(Modifier::ITALIC),
        ),
    ])];
    if entry.text.trim().is_empty() {
        return lines;
    }
    if !expanded {
        lines.extend(render_markdown_timeline_lines(
            accent,
            body_style,
            &preview_lines.join("\n"),
            MarkdownRenderOptions::timeline(max_content_width),
        ));
        if hidden_lines > 0 {
            lines.push(timeline_content_line(
                accent,
                vec![Span::styled(
                    format!("… {hidden_lines} more lines hidden"),
                    Style::default()
                        .fg(dim())
                        .add_modifier(Modifier::ITALIC | Modifier::BOLD),
                )],
            ));
        }
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

fn thinking_line_label(count: usize) -> String {
    if count == 1 {
        "1 line".to_owned()
    } else {
        format!("{count} lines")
    }
}

pub(crate) fn thinking_has_collapsed_content(text: &str) -> bool {
    if text.trim().is_empty() {
        return false;
    }
    thinking_line_count(text) > thinking_preview_lines(text, COLLAPSED_THINKING_PREVIEW_LINES).len()
}

fn thinking_preview_lines(text: &str, max_lines: usize) -> Vec<String> {
    let lines = text
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            (!trimmed.is_empty()).then_some(trimmed.to_owned())
        })
        .collect::<Vec<_>>();
    let mut preview = lines.iter().take(max_lines).cloned().collect::<Vec<_>>();
    extend_thinking_preview_code_fence(&lines, &mut preview);
    preview
}

fn extend_thinking_preview_code_fence(lines: &[String], preview: &mut Vec<String>) {
    let Some(fence_index) = preview.iter().rposition(|line| is_markdown_fence(line)) else {
        return;
    };
    if preview
        .iter()
        .filter(|line| is_markdown_fence(line))
        .count()
        % 2
        == 0
    {
        return;
    }

    let mut code_lines = preview[fence_index + 1..]
        .iter()
        .filter(|line| !is_markdown_fence(line))
        .count();
    let mut cursor = preview.len();
    while code_lines < COLLAPSED_THINKING_CODE_PREVIEW_LINES && cursor < lines.len() {
        let line = lines[cursor].clone();
        let is_fence = is_markdown_fence(&line);
        preview.push(line);
        cursor += 1;
        if is_fence {
            break;
        }
        code_lines += 1;
    }
}

fn is_markdown_fence(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("```") || trimmed.starts_with("~~~")
}

fn render_notice_entry_lines(entry: &TimelineEntry) -> Vec<Line<'static>> {
    let tone = notice_tone(&entry.text);
    let accent = notice_accent(tone);
    let body_style = notice_body_style(tone);
    let mut lines = vec![timeline_minor_header_line(
        notice_inline_label(tone),
        accent,
        "",
    )];
    for line in entry.text.lines().filter(|line| !line.trim().is_empty()) {
        let display_text = notice_display_text(line);
        if display_text.is_empty() {
            continue;
        }
        lines.push(timeline_content_line(
            accent,
            render_notice_body_spans(display_text, body_style),
        ));
    }
    lines
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NoticeTone {
    Error,
    Ok,
    Info,
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
        TimelineRole::System | TimelineRole::Phase => render_inline_markdown_spans_with_options(
            line,
            base_style.add_modifier(Modifier::BOLD),
            markdown_options,
        ),
        TimelineRole::Notice => {
            render_inline_markdown_spans_with_options(line, base_style, markdown_options)
        }
        TimelineRole::User => vec![Span::styled(line.to_owned(), base_style)],
    }
}

fn notice_tone(text: &str) -> NoticeTone {
    let lower = text.to_ascii_lowercase();
    if lower.contains("failed")
        || lower.contains("error")
        || lower.contains("deny")
        || lower.contains("missing")
    {
        NoticeTone::Error
    } else if lower.contains("approved")
        || lower.contains("restored")
        || lower.contains("ready")
        || lower.contains("saved")
    {
        NoticeTone::Ok
    } else {
        NoticeTone::Info
    }
}

fn notice_inline_label(tone: NoticeTone) -> &'static str {
    match tone {
        NoticeTone::Error => "error",
        NoticeTone::Ok => "done",
        NoticeTone::Info => "notice",
    }
}

fn notice_accent(tone: NoticeTone) -> Color {
    match tone {
        NoticeTone::Error => accent_rose(),
        NoticeTone::Ok => accent_lime(),
        NoticeTone::Info => accent_gold(),
    }
}

fn notice_body_style(tone: NoticeTone) -> Style {
    let color = match tone {
        NoticeTone::Error => muted(),
        NoticeTone::Ok | NoticeTone::Info => dim(),
    };
    Style::default().fg(color)
}

fn notice_display_text(line: &str) -> &str {
    let trimmed = line.trim();
    if let Some((label, value)) = trimmed.split_once(':') {
        match label.trim().to_ascii_lowercase().as_str() {
            "error" | "info" | "notice" | "ok" => return value.trim_start(),
            _ => {}
        }
    }
    trimmed
}

fn render_notice_body_spans(line: &str, base_style: Style) -> Vec<Span<'static>> {
    render_inline_markdown_spans_with_options(line, base_style, MarkdownRenderOptions::timeline(80))
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/timeline_tests.rs"]
mod tests;
