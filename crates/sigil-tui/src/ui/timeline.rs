use std::collections::{BTreeMap, BTreeSet};

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use sigil_kernel::SyntaxThemeId;

use crate::app::{TimelineEntry, TimelineRole};

use super::{
    command_text::known_slash_command_token,
    markdown::{
        MarkdownRenderOptions, MarkdownRenderState, render_code_line_spans_with_bg,
        render_inline_markdown_spans_with_palette, render_markdown_spans_with_palette,
        render_markdown_timeline_lines_with_palette,
    },
    primitives::{
        spans_with_background, timeline_content_line, timeline_header_line_with_palette,
        timeline_minor_header_line_with_palette,
    },
    text::{pad_display_width, wrap_display_width},
    theme::{self, ThemePalette},
    tool_card::render_tool_entry_lines,
};

const COLLAPSED_THINKING_PREVIEW_LINES: usize = 2;

#[cfg(test)]
use super::theme::{accent_blue, accent_gold, ink};

#[cfg(test)]
use super::theme::user_message_bg;

#[derive(Clone, Default)]
pub(crate) struct TimelineRenderOptions {
    pub expand_tool_previews: bool,
    pub expand_thinking_blocks: bool,
    pub streaming_reasoning_index: Option<usize>,
    pub selected_tool_activity_key: Option<String>,
    pub hovered_tool_activity_key: Option<String>,
    pub expanded_tool_activity_keys: BTreeSet<String>,
    pub collapsed_tool_activity_keys: BTreeSet<String>,
    pub tool_activity_visible_rows: BTreeMap<String, usize>,
    pub max_content_width: usize,
    pub streaming_assistant_index: Option<usize>,
    pub intermediate_assistant_indices: BTreeSet<usize>,
    pub expanded_thinking_entry_indices: BTreeSet<usize>,
    pub collapsed_thinking_entry_indices: BTreeSet<usize>,
    pub hovered_thinking_entry_index: Option<usize>,
    pub theme: theme::Theme,
}

pub(crate) fn render_timeline_entry_lines_with_options(
    entry: &TimelineEntry,
    options: &TimelineRenderOptions,
    entry_index: usize,
) -> Vec<Line<'static>> {
    let lines = if entry.role == TimelineRole::User {
        render_user_entry_lines(entry, options.max_content_width, &options.theme.palette)
    } else if entry.role == TimelineRole::Assistant {
        render_assistant_entry_lines(
            entry,
            options.max_content_width,
            options.streaming_assistant_index != Some(entry_index),
            options
                .intermediate_assistant_indices
                .contains(&entry_index),
            options.theme.syntax_theme,
            &options.theme.palette,
        )
    } else if entry.role == TimelineRole::Phase {
        render_phase_entry_lines(entry, &options.theme.palette)
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
            options.theme.syntax_theme,
            &options.theme.palette,
        )
    } else if entry.role == TimelineRole::Tool {
        render_tool_entry_lines(entry, options, entry_index)
    } else if entry.role == TimelineRole::Notice {
        render_notice_entry_lines(entry, &options.theme.palette)
    } else {
        let mut lines = vec![timeline_header_line_with_palette(
            "system",
            options.theme.palette.accent_info,
            "",
            &options.theme.palette,
        )];
        let mut markdown_state = MarkdownRenderState::default();
        let markdown_options = MarkdownRenderOptions::timeline(options.max_content_width)
            .with_syntax_theme(options.theme.syntax_theme);
        if !entry.text.is_empty() {
            for chunk in entry.text.split('\n') {
                let content = render_timeline_content_spans_with_palette(
                    entry.role,
                    chunk,
                    Style::default().fg(options.theme.palette.text_secondary),
                    &mut markdown_state,
                    markdown_options,
                    &options.theme.palette,
                );
                lines.push(timeline_content_line(
                    options.theme.palette.accent_info,
                    content,
                ));
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

fn render_user_entry_lines(
    entry: &TimelineEntry,
    max_content_width: usize,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    let accent = palette.accent_primary;
    let bubble_bg = palette.surface_user_message;
    let mut lines = Vec::new();
    if entry.text.trim().is_empty() {
        return lines;
    }
    let content_width = max_content_width.saturating_sub(8).max(18);
    lines.push(user_bubble_padding_line(accent, bubble_bg, content_width));
    for line in entry.text.lines() {
        if line.trim().is_empty() {
            lines.push(user_bubble_content_line(
                "",
                accent,
                bubble_bg,
                content_width,
                palette,
            ));
            continue;
        }
        for row in wrap_display_width(line, content_width) {
            lines.push(user_bubble_content_line(
                &row,
                accent,
                bubble_bg,
                content_width,
                palette,
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
    palette: &ThemePalette,
) -> Line<'static> {
    let padded = pad_display_width(row, content_width);
    let mut spans = vec![Span::styled(
        "▌  ",
        Style::default()
            .fg(accent)
            .bg(bubble_bg)
            .add_modifier(Modifier::BOLD),
    )];
    spans.extend(user_bubble_body_spans(
        &padded,
        content_width,
        bubble_bg,
        palette,
    ));
    spans.push(Span::styled("  ", Style::default().bg(bubble_bg)));
    Line::from(spans)
}

fn user_bubble_body_spans(
    padded: &str,
    content_width: usize,
    bubble_bg: Color,
    palette: &ThemePalette,
) -> Vec<Span<'static>> {
    let body_style = Style::default()
        .fg(palette.text_primary)
        .add_modifier(Modifier::BOLD);
    let Some(command) = known_slash_command_token(padded) else {
        return spans_with_background(
            render_inline_markdown_spans_with_palette(
                padded,
                body_style,
                MarkdownRenderOptions::timeline(content_width),
                palette,
            ),
            bubble_bg,
        );
    };

    let mut spans = Vec::new();
    if command.start > 0 {
        let markdown_options = MarkdownRenderOptions::timeline(content_width);
        spans.extend(spans_with_background(
            render_inline_markdown_spans_with_palette(
                &padded[..command.start],
                body_style,
                markdown_options,
                palette,
            ),
            bubble_bg,
        ));
    }
    let command_style = Style::default()
        .fg(palette.accent_info)
        .bg(bubble_bg)
        .add_modifier(Modifier::BOLD);
    spans.push(Span::styled(command.token.to_owned(), command_style));
    spans.extend(spans_with_background(
        render_inline_markdown_spans_with_palette(
            &padded[command.end..],
            body_style,
            MarkdownRenderOptions::timeline(content_width),
            palette,
        ),
        bubble_bg,
    ));
    spans
}

fn render_assistant_entry_lines(
    entry: &TimelineEntry,
    max_content_width: usize,
    highlight_code: bool,
    intermediate_info: bool,
    syntax_theme: SyntaxThemeId,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    let accent = palette.accent_info;
    if entry.text.trim().is_empty() {
        return Vec::new();
    }
    let mut lines = render_markdown_timeline_lines_with_palette(
        accent,
        Style::default().fg(palette.text_primary),
        &entry.text,
        MarkdownRenderOptions {
            highlight_code,
            ..MarkdownRenderOptions::timeline(max_content_width)
        }
        .with_syntax_theme(syntax_theme),
        palette,
    );
    if intermediate_info {
        mark_first_visible_assistant_line(&mut lines, palette);
    }
    lines
}

fn mark_first_visible_assistant_line(lines: &mut [Line<'static>], palette: &ThemePalette) {
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
            *first = assistant_info_marker_span(palette);
        } else {
            line.spans.insert(0, assistant_info_marker_span(palette));
        }
        return;
    }
}

fn assistant_info_marker_span(palette: &ThemePalette) -> Span<'static> {
    Span::styled(
        "• ",
        Style::default()
            .fg(palette.text_muted)
            .add_modifier(Modifier::BOLD),
    )
}

fn render_thinking_entry_lines(
    entry: &TimelineEntry,
    active: bool,
    expanded: bool,
    hovered: bool,
    max_content_width: usize,
    syntax_theme: SyntaxThemeId,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    if entry.text.trim().is_empty() {
        return Vec::new();
    }
    let accent = if hovered {
        palette.accent_warning
    } else {
        palette.status_thinking
    };
    let marker_style = Style::default().fg(accent).add_modifier(Modifier::BOLD);
    let header_modifier = if hovered {
        Modifier::ITALIC | Modifier::BOLD | Modifier::UNDERLINED
    } else {
        Modifier::ITALIC | Modifier::BOLD
    };
    let body_style = Style::default()
        .fg(palette.text_secondary)
        .add_modifier(Modifier::ITALIC);
    let total_lines = thinking_line_count(&entry.text);
    let has_hidden_content = thinking_has_collapsed_content(&entry.text);
    let preview_lines = collapsed_thinking_preview_lines(&entry.text);
    let hidden_lines = total_lines.saturating_sub(preview_lines.len());
    let mut lines = vec![Line::from(vec![
        Span::styled("●", marker_style),
        Span::raw(" "),
        Span::styled(
            if active { "thinking" } else { "thought" },
            Style::default().fg(accent).add_modifier(header_modifier),
        ),
        Span::raw("  "),
        Span::styled(
            if expanded && has_hidden_content {
                format!("{} · Ctrl-T collapse", thinking_line_label(total_lines))
            } else if !expanded && has_hidden_content {
                format!("{} · Ctrl-T expand", thinking_line_label(total_lines))
            } else {
                thinking_line_label(total_lines)
            },
            Style::default()
                .fg(palette.text_muted)
                .add_modifier(Modifier::ITALIC),
        ),
    ])];
    if !expanded && has_hidden_content {
        let preview = preview_lines.join("\n");
        let mut body = render_markdown_timeline_lines_with_palette(
            accent,
            body_style,
            &preview,
            MarkdownRenderOptions::timeline(max_content_width).with_syntax_theme(syntax_theme),
            palette,
        );
        if hidden_lines > 0 {
            body.push(timeline_content_line(
                accent,
                vec![Span::styled(
                    format!("… {} hidden", thinking_line_label(hidden_lines)),
                    Style::default()
                        .fg(palette.text_muted)
                        .add_modifier(Modifier::ITALIC | Modifier::BOLD),
                )],
            ));
        }
        lines.extend(frame_thinking_body_lines(body, marker_style, palette));
        return lines;
    }
    let body = render_markdown_timeline_lines_with_palette(
        accent,
        body_style,
        &entry.text,
        MarkdownRenderOptions::timeline(max_content_width).with_syntax_theme(syntax_theme),
        palette,
    );
    lines.extend(frame_thinking_body_lines(body, marker_style, palette));
    lines
}

fn frame_thinking_body_lines(
    lines: Vec<Line<'static>>,
    marker_style: Style,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    lines
        .into_iter()
        .enumerate()
        .map(|(index, line)| thinking_body_frame_line(line, index == 0, marker_style, palette))
        .collect()
}

fn thinking_body_frame_line(
    line: Line<'static>,
    first_body_line: bool,
    marker_style: Style,
    palette: &ThemePalette,
) -> Line<'static> {
    let marker = if first_body_line { "└ " } else { "  " };
    let marker_style = if first_body_line {
        marker_style
    } else {
        Style::default().fg(palette.text_muted)
    };
    let mut spans = vec![Span::styled(marker, marker_style)];
    spans.extend(strip_timeline_content_indent_spans(line.spans));
    Line::from(spans)
}

fn strip_timeline_content_indent_spans(spans: Vec<Span<'static>>) -> Vec<Span<'static>> {
    let mut iter = spans.into_iter();
    let Some(first) = iter.next() else {
        return Vec::new();
    };
    if first.content.as_ref() == "  " {
        return iter.collect();
    }
    std::iter::once(first).chain(iter).collect()
}

fn render_phase_entry_lines(entry: &TimelineEntry, palette: &ThemePalette) -> Vec<Line<'static>> {
    let (kind, detail) = entry
        .text
        .split_once('|')
        .map(|(kind, detail)| (kind, Some(detail)))
        .unwrap_or((entry.text.as_str(), None));
    let (label, accent, summary) = match kind {
        "thinking" => (
            "thinking",
            palette.status_thinking,
            detail
                .map(|model| format!("reasoning with {model}"))
                .unwrap_or_else(|| "reasoning".to_owned()),
        ),
        "tool" => (
            "tool",
            palette.status_tool,
            detail
                .map(|tool| format!("running {tool}"))
                .unwrap_or_else(|| "running tool".to_owned()),
        ),
        "streaming" => (
            "streaming",
            palette.status_streaming,
            "writing the reply".to_owned(),
        ),
        _ => ("phase", palette.text_secondary, entry.text.clone()),
    };

    vec![
        timeline_minor_header_line_with_palette(label, accent, "live", palette),
        timeline_content_line(
            accent,
            vec![Span::styled(
                summary,
                Style::default().fg(palette.text_muted),
            )],
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
    text.lines().filter(|line| !line.trim().is_empty()).count() > COLLAPSED_THINKING_PREVIEW_LINES
}

fn collapsed_thinking_preview_lines(text: &str) -> Vec<&str> {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .take(COLLAPSED_THINKING_PREVIEW_LINES)
        .collect()
}

fn render_notice_entry_lines(entry: &TimelineEntry, palette: &ThemePalette) -> Vec<Line<'static>> {
    let tone = notice_tone(&entry.text);
    let accent = notice_accent(tone, palette);
    let body_style = notice_body_style(tone, palette);
    let mut lines = vec![timeline_minor_header_line_with_palette(
        notice_inline_label(tone),
        accent,
        "",
        palette,
    )];
    for line in entry.text.lines().filter(|line| !line.trim().is_empty()) {
        let display_text = notice_display_text(line);
        if display_text.is_empty() {
            continue;
        }
        lines.push(timeline_content_line(
            accent,
            render_notice_body_spans(display_text, body_style, palette),
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

#[cfg(test)]
fn render_timeline_content_spans(
    role: TimelineRole,
    line: &str,
    base_style: Style,
    state: &mut MarkdownRenderState,
    markdown_options: MarkdownRenderOptions,
) -> Vec<Span<'static>> {
    let palette = theme::default_palette();
    render_timeline_content_spans_with_palette(
        role,
        line,
        base_style,
        state,
        markdown_options,
        &palette,
    )
}

fn render_timeline_content_spans_with_palette(
    role: TimelineRole,
    line: &str,
    base_style: Style,
    state: &mut MarkdownRenderState,
    markdown_options: MarkdownRenderOptions,
    palette: &ThemePalette,
) -> Vec<Span<'static>> {
    match role {
        TimelineRole::Assistant => {
            render_markdown_spans_with_palette(line, base_style, state, markdown_options, palette)
        }
        TimelineRole::Thinking => {
            render_markdown_spans_with_palette(line, base_style, state, markdown_options, palette)
        }
        TimelineRole::Tool => render_code_line_spans_with_bg(
            line,
            palette.accent_danger,
            Style::default().fg(palette.markdown_code_fg),
            palette.markdown_code_bg,
        ),
        TimelineRole::System | TimelineRole::Phase => render_inline_markdown_spans_with_palette(
            line,
            base_style.add_modifier(Modifier::BOLD),
            markdown_options,
            palette,
        ),
        TimelineRole::Notice => {
            render_inline_markdown_spans_with_palette(line, base_style, markdown_options, palette)
        }
        TimelineRole::User => vec![Span::styled(line.to_owned(), base_style)],
    }
}

fn notice_tone(text: &str) -> NoticeTone {
    if let Some(tone) = doctor_notice_tone(text) {
        return tone;
    }
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

fn doctor_notice_tone(text: &str) -> Option<NoticeTone> {
    let first_line = text.lines().find(|line| !line.trim().is_empty())?;
    let (label, status) = first_line.trim().split_once(':')?;
    if !label.trim().eq_ignore_ascii_case("doctor") {
        return None;
    }
    let normalized_status = status.trim().to_ascii_lowercase();
    match normalized_status.as_str() {
        "error" => Some(NoticeTone::Error),
        "ok" => Some(NoticeTone::Ok),
        _ => Some(NoticeTone::Info),
    }
}

fn notice_inline_label(tone: NoticeTone) -> &'static str {
    match tone {
        NoticeTone::Error => "error",
        NoticeTone::Ok => "done",
        NoticeTone::Info => "notice",
    }
}

fn notice_accent(tone: NoticeTone, palette: &ThemePalette) -> Color {
    match tone {
        NoticeTone::Error => palette.status_error,
        NoticeTone::Ok => palette.status_success,
        NoticeTone::Info => palette.status_warning,
    }
}

fn notice_body_style(tone: NoticeTone, palette: &ThemePalette) -> Style {
    let color = match tone {
        NoticeTone::Error => palette.text_secondary,
        NoticeTone::Ok | NoticeTone::Info => palette.text_muted,
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

fn render_notice_body_spans(
    line: &str,
    base_style: Style,
    palette: &ThemePalette,
) -> Vec<Span<'static>> {
    render_inline_markdown_spans_with_palette(
        line,
        base_style,
        MarkdownRenderOptions::timeline(80),
        palette,
    )
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/timeline_tests.rs"]
mod tests;
