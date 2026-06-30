use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use serde_json::Value;
use sigil_kernel::SyntaxThemeId;
use unicode_width::UnicodeWidthStr;

use crate::{
    agent_display::{AgentDisplayNameInput, resolve_agent_display_name},
    app::TimelineEntry,
};

use super::{
    TimelineRenderOptions,
    diff::{
        DiffLineKind, NumberedDiffLine, diff_line_kind, diff_line_number_text,
        diff_line_number_width, diff_line_style_for_palette, number_unified_diff_lines,
    },
    markdown::{
        MarkdownRenderOptions, render_code_line_spans_with_bg,
        render_markdown_timeline_lines_with_palette,
    },
    primitives::{
        section_badge_with_palette, timeline_badge_with_palette, timeline_content_line,
        timeline_section_line_with_palette,
    },
    status_indicator::{StatusIndicator, StatusKind, status_kind_from_label},
    text::truncate_inline_text,
    theme::ThemePalette,
};

const COLLAPSED_TOOL_PREVIEW_VISIBLE_ROWS: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolActivityView {
    pub(crate) key: String,
    pub(crate) title: String,
    pub(crate) is_inspection: bool,
    pub(crate) defaults_expanded: bool,
}

pub(crate) fn tool_activity_view(
    entry: &TimelineEntry,
    _entry_index: usize,
) -> Option<ToolActivityView> {
    let summary = parse_tool_summary(&entry.text);
    Some(build_tool_activity_view(&summary, &entry.text))
}

pub(crate) fn render_tool_entry_lines(
    entry: &TimelineEntry,
    options: &TimelineRenderOptions,
    _entry_index: usize,
) -> Vec<Line<'static>> {
    let summary = parse_tool_summary(&entry.text);
    let display = build_tool_card_display(&summary);
    let activity = build_tool_activity_view(&summary, &entry.text);
    let palette = &options.theme.palette;
    let accent = palette.accent_danger;
    let selected = options
        .selected_tool_activity_key
        .as_deref()
        .is_some_and(|selected| selected == activity.key.as_str());
    let hovered = options
        .hovered_tool_activity_key
        .as_deref()
        .is_some_and(|hovered| hovered == activity.key.as_str());
    let activity_marker_style =
        tool_card_activity_marker_style(display.status.kind, hovered, palette);
    let default_expanded = activity.defaults_expanded;
    let expanded = options.expand_tool_previews
        || options.expanded_tool_activity_keys.contains(&activity.key)
        || (default_expanded && !options.collapsed_tool_activity_keys.contains(&activity.key));
    let mut lines = vec![tool_card_header_line(
        &display,
        activity_marker_style,
        expanded,
        options.max_content_width,
        palette,
    )];
    if let Some(summary_line) = display.summary.clone() {
        lines.push(timeline_content_line(
            accent,
            vec![Span::styled(
                summary_line,
                Style::default()
                    .fg(palette.text_muted)
                    .add_modifier(Modifier::ITALIC),
            )],
        ));
    }
    if tool_has_preview(&summary) {
        if expanded {
            lines.extend(render_tool_preview_body_with_palette(
                &summary,
                accent,
                options.max_content_width,
                options.theme.syntax_theme,
                palette,
            ));
        } else {
            lines.extend(render_tool_collapsed_preview_body_with_palette(
                &summary,
                accent,
                options.max_content_width,
                options.theme.syntax_theme,
                palette,
            ));
        }
    }
    tool_card_frame_lines(
        lines,
        selected,
        options.max_content_width,
        activity_marker_style,
        palette,
    )
}

fn tool_has_preview(summary: &ToolCardRender) -> bool {
    terminal_task_tool(summary)
        || !summary.preview_lines.is_empty()
        || summary.preview_value.is_some()
        || summary.diff.is_some()
}

#[cfg(test)]
fn render_tool_collapsed_preview_body(
    summary: &ToolCardRender,
    accent: Color,
    max_content_width: usize,
) -> Vec<Line<'static>> {
    let palette = crate::ui::theme::default_palette();
    render_tool_collapsed_preview_body_with_palette(
        summary,
        accent,
        max_content_width,
        SyntaxThemeId::default(),
        &palette,
    )
}

fn render_tool_collapsed_preview_body_with_palette(
    summary: &ToolCardRender,
    accent: Color,
    max_content_width: usize,
    syntax_theme: SyntaxThemeId,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    let body = render_tool_preview_body_with_palette(
        summary,
        accent,
        max_content_width,
        syntax_theme,
        palette,
    );
    if body.len() <= COLLAPSED_TOOL_PREVIEW_VISIBLE_ROWS {
        return body;
    }

    let visible_rows = COLLAPSED_TOOL_PREVIEW_VISIBLE_ROWS;
    let hidden_rows = collapsed_tool_hidden_rows(summary, body.len(), visible_rows);
    let mut lines = body.into_iter().take(visible_rows).collect::<Vec<_>>();
    lines.extend(render_tool_hidden_tail(accent, hidden_rows, palette));
    lines
}

fn collapsed_tool_hidden_rows(
    summary: &ToolCardRender,
    body_rows: usize,
    visible_rows: usize,
) -> usize {
    let omitted_rows = body_rows.saturating_sub(visible_rows);
    if summary.hidden_lines == 0 {
        return omitted_rows;
    }
    summary
        .hidden_lines
        .saturating_add(omitted_rows.saturating_sub(1))
}

fn tool_card_header_line(
    display: &ToolCardDisplay,
    marker_style: Style,
    expanded: bool,
    max_content_width: usize,
    palette: &ThemePalette,
) -> Line<'static> {
    let mut spans = vec![Span::styled("●", marker_style), Span::raw(" ")];
    spans.extend(tool_title_spans_with_palette(
        &display.title,
        tool_title_width(display, max_content_width),
        palette,
    ));
    spans.push(Span::raw("  "));
    let status_indicator = StatusIndicator::animated(display.status.kind);
    spans.push(Span::styled(
        format!(" {} {} ", status_indicator.symbol(), display.status.label),
        tool_status_style(display.status.kind, palette),
    ));
    if let Some(detail) = &display.status.detail {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            detail.clone(),
            if display.status.is_error {
                Style::default()
                    .fg(palette.accent_danger)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(palette.text_muted)
            },
        ));
    }
    if expanded {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            "▾",
            Style::default().fg(palette.accent_success),
        ));
    }
    Line::from(spans)
}

fn tool_card_frame_lines(
    lines: Vec<Line<'static>>,
    selected: bool,
    max_content_width: usize,
    marker_style: Style,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    ToolCardFrame {
        selected,
        max_content_width,
        marker_style,
        palette,
    }
    .render(lines)
}

struct ToolCardFrame<'a> {
    selected: bool,
    max_content_width: usize,
    marker_style: Style,
    palette: &'a ThemePalette,
}

impl ToolCardFrame<'_> {
    fn render(&self, lines: Vec<Line<'static>>) -> Vec<Line<'static>> {
        let card_width = if self.max_content_width == 0 {
            160
        } else {
            self.max_content_width
        };
        lines
            .into_iter()
            .enumerate()
            .map(|(index, line)| {
                let line = if index == 0 {
                    line
                } else {
                    tool_card_body_frame_line(line, index == 1, self.marker_style, self.palette)
                };
                if self.selected {
                    tool_card_selected_line(line, card_width, self.palette)
                } else {
                    line
                }
            })
            .collect()
    }
}

fn tool_card_body_frame_line(
    line: Line<'static>,
    first_body_line: bool,
    marker_style: Style,
    palette: &ThemePalette,
) -> Line<'static> {
    let marker = if first_body_line { "└ " } else { "  " };
    let branch_style = if first_body_line {
        marker_style
    } else {
        Style::default().fg(palette.text_muted)
    };
    let mut spans = vec![Span::styled(marker, branch_style)];
    spans.extend(strip_timeline_content_indent(line.spans));
    Line::from(spans)
}

fn tool_card_activity_marker_style(
    status: StatusKind,
    hovered: bool,
    palette: &ThemePalette,
) -> Style {
    if hovered {
        Style::default()
            .fg(palette.accent_warning)
            .add_modifier(Modifier::BOLD)
    } else {
        StatusIndicator::static_kind(status).style_with_palette(palette)
    }
}

fn strip_timeline_content_indent(spans: Vec<Span<'static>>) -> Vec<Span<'static>> {
    let mut iter = spans.into_iter();
    let Some(first) = iter.next() else {
        return Vec::new();
    };
    let mut stripped = Vec::new();
    let first_text = first.content.as_ref();
    if first_text == "  " {
        // Drop the generic timeline indent; the tool-card frame supplies it.
    } else if let Some(rest) = first_text.strip_prefix("  ") {
        if !rest.is_empty() {
            stripped.push(Span::styled(rest.to_owned(), first.style));
        }
    } else {
        stripped.push(first);
    }
    stripped.extend(iter);
    stripped
}

fn tool_card_selected_line(
    line: Line<'static>,
    card_width: usize,
    palette: &ThemePalette,
) -> Line<'static> {
    let bg = palette.surface_selection;
    let mut spans = line
        .spans
        .into_iter()
        .map(|span| {
            let mut style = span.style;
            style.bg = Some(bg);
            Span::styled(span.content, style)
        })
        .collect::<Vec<_>>();
    let width = spans_display_width(&spans);
    if card_width > width {
        spans.push(Span::styled(
            " ".repeat(card_width - width),
            Style::default().bg(bg),
        ));
    }
    Line::from(spans)
}

fn spans_display_width(spans: &[Span<'static>]) -> usize {
    spans
        .iter()
        .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
        .sum()
}

fn tool_title_width(display: &ToolCardDisplay, max_content_width: usize) -> usize {
    if max_content_width == 0 {
        return 160;
    }
    max_content_width
        .saturating_sub(display.status.label.chars().count() + 12)
        .clamp(32, 160)
}

#[cfg(test)]
fn render_tool_preview_body(
    summary: &ToolCardRender,
    accent: Color,
    max_content_width: usize,
) -> Vec<Line<'static>> {
    let palette = crate::ui::theme::default_palette();
    render_tool_preview_body_with_palette(
        summary,
        accent,
        max_content_width,
        SyntaxThemeId::default(),
        &palette,
    )
}

fn render_tool_preview_body_with_palette(
    summary: &ToolCardRender,
    accent: Color,
    max_content_width: usize,
    syntax_theme: SyntaxThemeId,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    if (tool_name_matches(&summary.tool_name, "ls")
        || tool_name_matches(&summary.tool_name, "glob"))
        && let Some(lines) = render_path_list_preview_with_palette(summary, accent, palette)
    {
        return lines;
    }
    if tool_name_matches(&summary.tool_name, "grep")
        && let Some(lines) = render_grep_preview_with_palette(summary, accent, palette)
    {
        return lines;
    }
    if tool_name_matches(&summary.tool_name, "bash") {
        return render_bash_preview_with_palette(summary, accent, palette);
    }
    if terminal_task_tool(summary) {
        return render_terminal_task_preview_with_palette(summary, accent, palette);
    }
    if agent_tool(summary) {
        return render_agent_tool_preview(
            summary,
            accent,
            max_content_width,
            syntax_theme,
            palette,
        );
    }
    if file_change_tool(summary)
        && let Some(lines) = render_file_change_preview_with_palette(summary, accent, palette)
    {
        return lines;
    }
    if tool_name_matches(&summary.tool_name, "read_file") {
        return render_read_file_preview_with_palette(
            summary,
            accent,
            max_content_width,
            syntax_theme,
            palette,
        );
    }
    if code_intelligence_tool(summary) {
        return render_code_intelligence_preview_with_palette(
            summary,
            accent,
            max_content_width,
            syntax_theme,
            palette,
        );
    }
    render_generic_tool_preview_with_palette(
        summary,
        accent,
        max_content_width,
        syntax_theme,
        palette,
    )
}

#[cfg(test)]
fn render_read_file_preview(
    summary: &ToolCardRender,
    accent: Color,
    max_content_width: usize,
) -> Vec<Line<'static>> {
    let palette = crate::ui::theme::default_palette();
    render_read_file_preview_with_palette(
        summary,
        accent,
        max_content_width,
        SyntaxThemeId::default(),
        &palette,
    )
}

fn render_read_file_preview_with_palette(
    summary: &ToolCardRender,
    accent: Color,
    max_content_width: usize,
    syntax_theme: SyntaxThemeId,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    let mut lines = vec![timeline_section_line_with_palette(
        accent,
        if summary.preview_kind == ToolPreviewKind::Markdown {
            "doc"
        } else {
            "file"
        },
        palette.accent_info,
        vec![Span::styled(
            if summary.preview_kind == ToolPreviewKind::Markdown {
                "document excerpt"
            } else {
                "file excerpt"
            },
            Style::default().fg(palette.text_muted),
        )],
        palette,
    )];
    match summary.preview_kind {
        ToolPreviewKind::Markdown => {
            lines.extend(render_markdown_timeline_lines_with_palette(
                accent,
                Style::default().fg(palette.text_primary),
                &summary.preview_lines.join("\n"),
                MarkdownRenderOptions::tool_preview(max_content_width)
                    .with_syntax_theme(syntax_theme),
                palette,
            ));
        }
        ToolPreviewKind::Json | ToolPreviewKind::Text => {
            lines.extend(render_code_preview_lines_with_palette(
                accent,
                &summary.preview_lines,
                palette.markdown_code_bg,
                palette,
            ));
        }
    }
    lines.extend(render_tool_hidden_tail(
        accent,
        summary.hidden_lines,
        palette,
    ));
    lines
}

#[cfg(test)]
fn render_path_list_preview(summary: &ToolCardRender, accent: Color) -> Option<Vec<Line<'static>>> {
    let palette = crate::ui::theme::default_palette();
    render_path_list_preview_with_palette(summary, accent, &palette)
}

fn render_path_list_preview_with_palette(
    summary: &ToolCardRender,
    accent: Color,
    palette: &ThemePalette,
) -> Option<Vec<Line<'static>>> {
    let entries = summary
        .preview_value
        .as_ref()
        .and_then(json_string_list)
        .or_else(|| Some(infer_string_list_preview(&summary.preview_lines)))
        .filter(|entries| !entries.is_empty())?;

    let mut lines = vec![timeline_section_line_with_palette(
        accent,
        if tool_name_matches(&summary.tool_name, "glob") {
            "matches"
        } else {
            "files"
        },
        palette.accent_info,
        vec![Span::styled(
            format!("{} paths", entries.len() + summary.hidden_lines),
            Style::default().fg(palette.text_muted),
        )],
        palette,
    )];
    for path in entries {
        lines.push(timeline_content_line(
            accent,
            vec![
                Span::styled(
                    "• ",
                    Style::default()
                        .fg(palette.accent_warning)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(path, Style::default().fg(palette.text_primary)),
            ],
        ));
    }
    lines.extend(render_tool_hidden_tail(
        accent,
        summary.hidden_lines,
        palette,
    ));
    Some(lines)
}

#[cfg(test)]
fn render_grep_preview(summary: &ToolCardRender, accent: Color) -> Option<Vec<Line<'static>>> {
    let palette = crate::ui::theme::default_palette();
    render_grep_preview_with_palette(summary, accent, &palette)
}

fn render_grep_preview_with_palette(
    summary: &ToolCardRender,
    accent: Color,
    palette: &ThemePalette,
) -> Option<Vec<Line<'static>>> {
    let matches = summary.preview_value.as_ref().and_then(json_grep_matches)?;
    if matches.is_empty() {
        return None;
    }

    let mut grouped = Vec::<(String, Vec<(u64, String)>)>::new();
    for (path, line, text) in matches {
        if let Some((_, rows)) = grouped.iter_mut().find(|(existing, _)| existing == &path) {
            rows.push((line, text));
        } else {
            grouped.push((path, vec![(line, text)]));
        }
    }

    let mut lines = vec![timeline_section_line_with_palette(
        accent,
        "matches",
        palette.accent_info,
        vec![Span::styled(
            format!("{} files", grouped.len()),
            Style::default().fg(palette.text_muted),
        )],
        palette,
    )];
    for (path, rows) in grouped {
        lines.push(timeline_content_line(
            accent,
            vec![
                section_badge_with_palette("file", palette.accent_secondary, palette),
                Span::raw(" "),
                Span::styled(
                    path,
                    Style::default()
                        .fg(palette.text_primary)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::styled(
                    format!("{} hits", rows.len()),
                    Style::default().fg(palette.text_muted),
                ),
            ],
        ));
        for (line_number, text) in rows {
            lines.push(timeline_content_line(
                accent,
                vec![
                    Span::styled(
                        format!("L{line_number:<4}"),
                        Style::default()
                            .fg(palette.accent_warning)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(" "),
                    Span::styled(
                        truncate_inline_text(&text, 140),
                        Style::default().fg(palette.text_primary),
                    ),
                ],
            ));
        }
    }
    lines.extend(render_tool_hidden_tail(
        accent,
        summary.hidden_lines,
        palette,
    ));
    Some(lines)
}

#[cfg(test)]
fn render_bash_preview(summary: &ToolCardRender, accent: Color) -> Vec<Line<'static>> {
    let palette = crate::ui::theme::default_palette();
    render_bash_preview_with_palette(summary, accent, &palette)
}

fn render_bash_preview_with_palette(
    summary: &ToolCardRender,
    accent: Color,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    let section = bash_preview_section_label(summary);
    let subtitle = match (&summary.summary, summary.metadata.exit_code) {
        (Some(summary), Some(code)) => format!("exit {code} · {summary}"),
        (Some(summary), None) => summary.clone(),
        (None, Some(code)) => format!("exit {code}"),
        (None, None) => "terminal tail".to_owned(),
    };
    let mut lines = vec![timeline_section_line_with_palette(
        accent,
        section,
        palette.accent_warning,
        vec![Span::styled(
            subtitle,
            Style::default().fg(palette.text_muted),
        )],
        palette,
    )];
    if summary.preview_lines.is_empty() {
        lines.push(timeline_content_line(
            accent,
            vec![Span::styled(
                "(no output)".to_owned(),
                Style::default().fg(palette.text_muted),
            )],
        ));
    } else {
        lines.extend(render_code_preview_lines_with_palette(
            accent,
            &summary.preview_lines,
            palette.markdown_code_bg,
            palette,
        ));
    }
    lines.extend(render_tool_hidden_tail(
        accent,
        summary.hidden_lines,
        palette,
    ));
    lines
}

fn render_terminal_task_preview_with_palette(
    summary: &ToolCardRender,
    accent: Color,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    let subtitle = summary
        .metadata
        .terminal_command
        .as_deref()
        .map(|command| truncate_inline_text(command, 120))
        .or_else(|| summary.metadata.terminal_log_path.clone())
        .unwrap_or_else(|| "terminal task".to_owned());
    let mut lines = vec![timeline_section_line_with_palette(
        accent,
        "terminal",
        palette.accent_warning,
        vec![Span::styled(
            subtitle,
            Style::default().fg(palette.text_muted),
        )],
        palette,
    )];
    if let Some(log_path) = &summary.metadata.terminal_log_path {
        lines.push(timeline_content_line(
            accent,
            vec![
                section_badge_with_palette("log", palette.accent_secondary, palette),
                Span::raw(" "),
                Span::styled(log_path.clone(), Style::default().fg(palette.text_muted)),
            ],
        ));
    }
    if summary.preview_lines.is_empty() {
        lines.push(timeline_content_line(
            accent,
            vec![Span::styled(
                "(no output preview)".to_owned(),
                Style::default().fg(palette.text_muted),
            )],
        ));
    } else {
        lines.extend(render_code_preview_lines_with_palette(
            accent,
            &summary.preview_lines,
            palette.markdown_code_bg,
            palette,
        ));
    }
    lines.extend(render_tool_hidden_tail(
        accent,
        summary.hidden_lines,
        palette,
    ));
    lines
}

fn bash_preview_section_label(summary: &ToolCardRender) -> &'static str {
    if summary.is_error {
        if summary.metadata.stderr_bytes.unwrap_or(0) > 0 {
            return "stderr";
        }
        if summary.metadata.stdout_bytes.unwrap_or(0) > 0 {
            return "stdout";
        }
    }
    "output"
}

fn terminal_task_tool(summary: &ToolCardRender) -> bool {
    tool_name_matches(&summary.tool_name, "terminal_task")
        || summary.metadata.terminal_task_id.is_some()
}

fn terminal_task_is_active(summary: &ToolCardRender) -> bool {
    matches!(
        summary.metadata.terminal_status.as_deref(),
        Some("starting" | "running")
    )
}

fn terminal_task_display_status(summary: &ToolCardRender) -> ToolCardDisplayStatus {
    let label = match summary.metadata.terminal_status.as_deref() {
        Some("starting") => "STARTING",
        Some("running") => "RUNNING",
        Some("exited") => "EXITED",
        Some("failed") => "FAILED",
        Some("cancelled") => "CANCELLED",
        Some("interrupted") => "INTERRUPTED",
        _ if summary.is_error => "ERROR",
        _ => "OK",
    };
    let mut details = Vec::new();
    match summary.metadata.terminal_status.as_deref() {
        Some("exited") => {
            if let Some(code) = summary.metadata.terminal_exit_code {
                details.push(format!("exit {code}"));
            }
        }
        Some("failed") => {
            if let Some(reason) = summary.metadata.terminal_failed_reason.as_deref() {
                details.push(truncate_inline_text(reason, 80));
            }
        }
        _ => {}
    }
    if let Some(boundary) = terminal_execution_boundary_detail(summary) {
        details.push(boundary);
    }
    if let Some(cleanup_status) = summary
        .metadata
        .terminal_cleanup_status
        .as_deref()
        .filter(|status| *status != "not_needed")
    {
        details.push(format!("cleanup {cleanup_status}"));
    }
    ToolCardDisplayStatus {
        label,
        detail: (!details.is_empty()).then(|| details.join(" · ")),
        kind: terminal_task_status_kind(summary),
        is_error: summary.is_error
            || matches!(summary.metadata.terminal_status.as_deref(), Some("failed")),
    }
}

fn terminal_execution_boundary_detail(summary: &ToolCardRender) -> Option<String> {
    let backend = summary.metadata.terminal_enforcement_backend.as_deref();
    let profile = summary.metadata.terminal_sandbox_profile.as_deref();
    match (backend, profile) {
        (Some("local"), Some("unconfined")) => Some("local unconfined".to_owned()),
        (Some(backend), Some(profile)) => Some(format!("{backend} {profile}")),
        (Some(backend), None) => Some(backend.to_owned()),
        (None, Some(profile)) => Some(profile.to_owned()),
        (None, None) => None,
    }
}

fn terminal_task_status_kind(summary: &ToolCardRender) -> StatusKind {
    match summary.metadata.terminal_status.as_deref() {
        Some("starting" | "running") => StatusKind::Running,
        Some("exited") => StatusKind::Success,
        Some("failed" | "cancelled" | "interrupted") => StatusKind::Error,
        _ if summary.is_error => StatusKind::Error,
        _ => StatusKind::Success,
    }
}

#[cfg(test)]
fn render_file_change_preview(
    summary: &ToolCardRender,
    accent: Color,
) -> Option<Vec<Line<'static>>> {
    let palette = crate::ui::theme::default_palette();
    render_file_change_preview_with_palette(summary, accent, &palette)
}

fn render_file_change_preview_with_palette(
    summary: &ToolCardRender,
    accent: Color,
    palette: &ThemePalette,
) -> Option<Vec<Line<'static>>> {
    if summary.metadata.changed_files.is_empty() && summary.diff.is_none() {
        return None;
    }
    let mut lines = Vec::new();
    if !summary.metadata.changed_files.is_empty() {
        lines.push(timeline_section_line_with_palette(
            accent,
            "files",
            palette.accent_info,
            vec![Span::styled(
                format!(
                    "{} {}",
                    summary.metadata.changed_files.len(),
                    file_change_count_label(summary)
                ),
                Style::default().fg(palette.text_muted),
            )],
            palette,
        ));
        for path in &summary.metadata.changed_files {
            lines.push(timeline_content_line(
                accent,
                vec![
                    Span::styled(
                        "• ",
                        Style::default()
                            .fg(palette.accent_success)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(path.clone(), Style::default().fg(palette.text_primary)),
                ],
            ));
        }
    }
    if let Some(diff) = &summary.diff {
        lines.extend(render_tool_diff_preview_with_palette(
            summary, diff, accent, palette,
        ));
    }
    if !summary.preview_lines.is_empty() {
        lines.push(timeline_section_line_with_palette(
            accent,
            "result",
            palette.accent_warning,
            vec![Span::styled(
                file_change_result_label(summary),
                Style::default().fg(palette.text_muted),
            )],
            palette,
        ));
        lines.extend(render_code_preview_lines_with_palette(
            accent,
            &summary.preview_lines,
            palette.markdown_code_bg,
            palette,
        ));
    }
    Some(lines)
}

#[cfg(test)]
fn render_code_intelligence_preview(
    summary: &ToolCardRender,
    accent: Color,
    max_content_width: usize,
) -> Vec<Line<'static>> {
    let palette = crate::ui::theme::default_palette();
    render_code_intelligence_preview_with_palette(
        summary,
        accent,
        max_content_width,
        SyntaxThemeId::default(),
        &palette,
    )
}

fn render_code_intelligence_preview_with_palette(
    summary: &ToolCardRender,
    accent: Color,
    max_content_width: usize,
    syntax_theme: SyntaxThemeId,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    let Some(value) = &summary.preview_value else {
        return render_generic_tool_preview_with_palette(
            summary,
            accent,
            max_content_width,
            syntax_theme,
            palette,
        );
    };
    let server = value
        .get("server")
        .and_then(Value::as_str)
        .or(summary.metadata.code_server.as_deref())
        .unwrap_or("code");
    let capability = value
        .get("capability")
        .and_then(Value::as_str)
        .or(summary.metadata.code_capability.as_deref())
        .unwrap_or("inspect");
    let returned = value
        .get("metadata")
        .and_then(|metadata| metadata.get("returned"))
        .and_then(Value::as_u64)
        .or(summary.metadata.returned_entries)
        .unwrap_or(0);
    let total = value
        .get("metadata")
        .and_then(|metadata| metadata.get("total"))
        .and_then(Value::as_u64)
        .or(summary.metadata.total_entries)
        .unwrap_or(returned);
    let mut lines = vec![timeline_section_line_with_palette(
        accent,
        code_intelligence_section(summary),
        palette.accent_info,
        vec![Span::styled(
            format!(
                "{} · {} · {} · {returned}/{total}",
                code_intelligence_source_label(server, capability),
                server,
                code_intelligence_capability_label(capability)
            ),
            Style::default().fg(palette.text_muted),
        )],
        palette,
    )];
    if let Some(server_line) = code_intelligence_servers_line_with_palette(value, palette) {
        lines.push(timeline_content_line(accent, server_line));
    }
    if let Some(items) = code_intelligence_items(summary, value, palette) {
        for item in items.into_iter().take(16) {
            lines.push(timeline_content_line(accent, item));
        }
        let hidden = total
            .saturating_sub(returned)
            .saturating_add(returned.saturating_sub(16));
        lines.extend(render_tool_hidden_tail(accent, hidden as usize, palette));
    } else {
        lines.extend(render_generic_tool_preview_with_palette(
            summary,
            accent,
            max_content_width,
            syntax_theme,
            palette,
        ));
    }
    lines
}

fn code_intelligence_section(summary: &ToolCardRender) -> &'static str {
    if tool_name_matches(&summary.tool_name, "code_diagnostics") {
        "diagnostics"
    } else if tool_name_matches(&summary.tool_name, "code_definition") {
        "definition"
    } else if tool_name_matches(&summary.tool_name, "code_references") {
        "references"
    } else if tool_name_matches(&summary.tool_name, "code_actions") {
        "actions"
    } else {
        "symbols"
    }
}

fn code_intelligence_items(
    summary: &ToolCardRender,
    value: &Value,
    palette: &ThemePalette,
) -> Option<Vec<Vec<Span<'static>>>> {
    let key = if tool_name_matches(&summary.tool_name, "code_diagnostics") {
        "diagnostics"
    } else if tool_name_matches(&summary.tool_name, "code_definition") {
        "definition"
    } else if tool_name_matches(&summary.tool_name, "code_references") {
        "references"
    } else if tool_name_matches(&summary.tool_name, "code_actions") {
        "code_actions"
    } else if tool_name_matches(&summary.tool_name, "code_workspace_symbols") {
        "workspace_symbols"
    } else {
        "symbols"
    };
    let array = value
        .get(key)
        .or_else(|| value.get("results"))
        .and_then(Value::as_array)?;
    let rows = array
        .iter()
        .filter_map(|entry| code_intelligence_row_with_palette(summary, entry, palette))
        .collect::<Vec<_>>();
    Some(rows)
}

fn code_intelligence_row_with_palette(
    summary: &ToolCardRender,
    entry: &Value,
    palette: &ThemePalette,
) -> Option<Vec<Span<'static>>> {
    if tool_name_matches(&summary.tool_name, "code_diagnostics") {
        let severity = entry.get("severity")?.as_str()?.to_owned();
        let path = entry.get("path")?.as_str()?.to_owned();
        let message = entry.get("message")?.as_str()?.to_owned();
        let source = entry
            .get("source")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let mut spans = vec![
            section_badge_with_palette(
                &severity,
                diagnostic_severity_color_with_palette(&severity, palette),
                palette,
            ),
            Span::raw(" "),
            Span::styled(
                code_location_label(&path, entry),
                Style::default().fg(palette.accent_info),
            ),
            Span::raw(" "),
        ];
        if let Some(source) = source {
            spans.push(Span::styled(
                format!("{source}: "),
                Style::default().fg(palette.text_muted),
            ));
        }
        spans.push(Span::styled(
            truncate_inline_text(&message, 120),
            Style::default().fg(palette.text_primary),
        ));
        return Some(spans);
    }
    if tool_name_matches(&summary.tool_name, "code_actions") {
        let title = entry.get("title")?.as_str()?.to_owned();
        let label = entry
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("action")
            .to_owned();
        let capability = if entry.get("has_edit").and_then(Value::as_bool) == Some(true) {
            "edit"
        } else if entry.get("has_command").and_then(Value::as_bool) == Some(true) {
            "command"
        } else {
            "inspect"
        };
        return Some(vec![
            section_badge_with_palette(&label, palette.accent_secondary, palette),
            Span::raw(" "),
            Span::styled(capability, Style::default().fg(palette.accent_info)),
            Span::raw(" "),
            Span::styled(
                truncate_inline_text(&title, 120),
                Style::default().fg(palette.text_primary),
            ),
        ]);
    }
    let path = entry.get("path")?.as_str()?.to_owned();
    let label = entry
        .get("kind")
        .and_then(Value::as_str)
        .or_else(|| {
            if tool_name_matches(&summary.tool_name, "code_definition") {
                Some("def")
            } else if tool_name_matches(&summary.tool_name, "code_references") {
                Some("ref")
            } else {
                None
            }
        })
        .unwrap_or("code")
        .to_owned();
    let name = entry
        .get("name")
        .and_then(Value::as_str)
        .or_else(|| entry.get("preview").and_then(Value::as_str))
        .unwrap_or("")
        .to_owned();
    let mut spans = vec![
        section_badge_with_palette(&label, palette.accent_secondary, palette),
        Span::raw(" "),
        Span::styled(
            code_location_label(&path, entry),
            Style::default().fg(palette.accent_info),
        ),
        Span::raw(" "),
        Span::styled(
            truncate_inline_text(&name, 120),
            Style::default().fg(palette.text_primary),
        ),
    ];
    if let Some(container) = entry.get("container_name").and_then(Value::as_str) {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!("in {container}"),
            Style::default().fg(palette.text_muted),
        ));
    }
    Some(spans)
}

fn code_location_label(path: &str, entry: &Value) -> String {
    let line = range_start_line(entry);
    match range_start_character(entry) {
        Some(character) if character > 0 => format!("{path}:{line}:{character}"),
        _ => format!("{path}:{line}"),
    }
}

fn range_start_line(entry: &Value) -> u64 {
    entry
        .get("range")
        .and_then(|range| range.get("start_line"))
        .and_then(Value::as_u64)
        .unwrap_or(1)
}

fn range_start_character(entry: &Value) -> Option<u64> {
    entry
        .get("range")
        .and_then(|range| range.get("start_character"))
        .and_then(Value::as_u64)
}

fn code_intelligence_source_label(server: &str, capability: &str) -> &'static str {
    if server.starts_with("tree-sitter") || capability.starts_with("tree_sitter/") {
        "Tree-sitter"
    } else if capability.starts_with("textDocument/") || capability.starts_with("workspace/") {
        "LSP"
    } else {
        "Code"
    }
}

fn code_intelligence_capability_label(capability: &str) -> String {
    match capability {
        "textDocument/documentSymbol" | "tree_sitter/document_symbols" => {
            "document symbols".to_owned()
        }
        "workspace/symbol" | "tree_sitter/workspace_symbols" => "workspace symbols".to_owned(),
        "textDocument/definition" => "definition".to_owned(),
        "textDocument/references" => "references".to_owned(),
        "textDocument/diagnostic"
        | "textDocument/publishDiagnostics"
        | "tree_sitter/diagnostics" => "diagnostics".to_owned(),
        other => other.replace('/', " / "),
    }
}

fn code_intelligence_servers_line_with_palette(
    value: &Value,
    palette: &ThemePalette,
) -> Option<Vec<Span<'static>>> {
    let servers = value.get("servers").and_then(Value::as_array)?;
    if servers.len() <= 1 {
        return None;
    }
    let mut labels = servers
        .iter()
        .take(3)
        .filter_map(code_intelligence_server_label)
        .collect::<Vec<_>>();
    let hidden = servers.len().saturating_sub(labels.len());
    if hidden > 0 {
        labels.push(format!("+{hidden} more"));
    }
    if labels.is_empty() {
        return None;
    }
    Some(vec![
        section_badge_with_palette("servers", palette.accent_info, palette),
        Span::raw(" "),
        Span::styled(labels.join(" · "), Style::default().fg(palette.text_muted)),
    ])
}

fn code_intelligence_server_label(value: &Value) -> Option<String> {
    let server = value.get("server").and_then(Value::as_str)?;
    let status = value
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("ready");
    let languages = value
        .get("languages")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .take(2)
                .collect::<Vec<_>>()
                .join(",")
        })
        .filter(|languages| !languages.is_empty());
    Some(match languages {
        Some(languages) => format!("{server} {status} ({languages})"),
        None => format!("{server} {status}"),
    })
}

#[cfg(test)]
fn diagnostic_severity_color(severity: &str) -> Color {
    let palette = crate::ui::theme::default_palette();
    diagnostic_severity_color_with_palette(severity, &palette)
}

fn diagnostic_severity_color_with_palette(severity: &str, palette: &ThemePalette) -> Color {
    match severity {
        "error" => palette.status_error,
        "warning" => palette.status_warning,
        _ => palette.accent_secondary,
    }
}

fn file_change_tool(summary: &ToolCardRender) -> bool {
    tool_name_matches(&summary.tool_name, "write_file")
        || tool_name_matches(&summary.tool_name, "edit_file")
        || tool_name_matches(&summary.tool_name, "delete_file")
        || tool_name_matches(&summary.tool_name, "code_action")
        || tool_name_matches(&summary.tool_name, "code_rename")
}

fn code_intelligence_tool(summary: &ToolCardRender) -> bool {
    tool_name_matches(&summary.tool_name, "code_symbols")
        || tool_name_matches(&summary.tool_name, "code_workspace_symbols")
        || tool_name_matches(&summary.tool_name, "code_definition")
        || tool_name_matches(&summary.tool_name, "code_references")
        || tool_name_matches(&summary.tool_name, "code_diagnostics")
        || tool_name_matches(&summary.tool_name, "code_actions")
}

fn agent_tool(summary: &ToolCardRender) -> bool {
    tool_name_matches(&summary.tool_name, "spawn_agent")
        || tool_name_matches(&summary.tool_name, "wait_agent")
        || tool_name_matches(&summary.tool_name, "read_agent_result")
        || tool_name_matches(&summary.tool_name, "message_agent")
        || tool_name_matches(&summary.tool_name, "close_agent")
}

fn render_agent_tool_preview(
    summary: &ToolCardRender,
    accent: Color,
    max_content_width: usize,
    syntax_theme: SyntaxThemeId,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    if tool_name_matches(&summary.tool_name, "read_agent_result") {
        return render_agent_result_page_preview(summary, accent, max_content_width, palette);
    }
    if tool_name_matches(&summary.tool_name, "spawn_agent")
        && summary.preview_kind == ToolPreviewKind::Markdown
        && !summary.preview_lines.is_empty()
    {
        return render_agent_summary_preview(
            summary,
            accent,
            max_content_width,
            syntax_theme,
            palette,
        );
    }
    render_agent_status_preview(summary, accent, palette)
}

fn render_agent_result_page_preview(
    summary: &ToolCardRender,
    accent: Color,
    _max_content_width: usize,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    let mut lines = vec![timeline_section_line_with_palette(
        accent,
        "result",
        palette.accent_info,
        vec![Span::styled(
            agent_result_page_summary(summary).unwrap_or_else(|| "agent result page".to_owned()),
            Style::default().fg(palette.text_muted),
        )],
        palette,
    )];
    lines.extend(render_agent_status_preview(summary, accent, palette));
    lines
}

fn render_agent_summary_preview(
    summary: &ToolCardRender,
    accent: Color,
    max_content_width: usize,
    syntax_theme: SyntaxThemeId,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    let mut lines = vec![timeline_section_line_with_palette(
        accent,
        "summary",
        palette.accent_info,
        vec![Span::styled(
            agent_status_detail(summary),
            Style::default().fg(palette.text_muted),
        )],
        palette,
    )];
    lines.extend(render_markdown_timeline_lines_with_palette(
        accent,
        Style::default().fg(palette.text_primary),
        &summary.preview_lines.join("\n"),
        MarkdownRenderOptions::tool_preview(max_content_width).with_syntax_theme(syntax_theme),
        palette,
    ));
    lines.extend(render_tool_hidden_tail(
        accent,
        summary.hidden_lines,
        palette,
    ));
    if agent_payload_bool(summary, "summary_truncated").unwrap_or(false) {
        lines.push(timeline_content_line(
            accent,
            vec![Span::styled(
                "Use read_agent_result for the complete result.",
                Style::default()
                    .fg(palette.text_muted)
                    .add_modifier(Modifier::ITALIC),
            )],
        ));
    }
    lines
}

fn render_agent_status_preview(
    summary: &ToolCardRender,
    accent: Color,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    let mut details = vec![Span::styled(
        agent_status_detail(summary),
        Style::default().fg(palette.text_muted),
    )];
    if agent_payload_bool(summary, "result_available").unwrap_or(false) {
        details.push(Span::raw(" · "));
        details.push(Span::styled(
            "result ready",
            Style::default()
                .fg(palette.accent_success)
                .add_modifier(Modifier::BOLD),
        ));
    }
    let mut lines = vec![timeline_section_line_with_palette(
        accent,
        "agent",
        palette.accent_info,
        details,
        palette,
    )];
    if let Some(reason) =
        agent_payload_string(summary, "reason").filter(|reason| !reason.is_empty())
    {
        lines.push(timeline_content_line(
            accent,
            vec![Span::styled(
                reason,
                Style::default()
                    .fg(palette.text_muted)
                    .add_modifier(Modifier::ITALIC),
            )],
        ));
    }
    if let Some(action_hint) = agent_payload_string(summary, "action_hint")
        .or_else(|| agent_payload_string(summary, "next_action"))
        .filter(|hint| !hint.is_empty())
    {
        lines.push(timeline_content_line(
            accent,
            vec![
                Span::styled("action", Style::default().fg(palette.text_muted)),
                Span::raw(" "),
                Span::styled(
                    action_hint,
                    Style::default()
                        .fg(palette.accent_warning)
                        .add_modifier(Modifier::BOLD),
                ),
            ],
        ));
    }
    if agent_result_read_tool(summary).is_some() {
        lines.push(timeline_content_line(
            accent,
            vec![
                Span::styled("read", Style::default().fg(palette.text_muted)),
                Span::raw(" "),
                Span::styled(
                    "read_agent_result",
                    Style::default().fg(palette.accent_info),
                ),
            ],
        ));
    }
    lines
}

fn agent_tool_display_status(status: &str) -> ToolCardDisplayStatus {
    let status = status.trim();
    let kind = status_kind_from_label(status);
    ToolCardDisplayStatus {
        label: agent_status_display_label(status),
        detail: None,
        kind,
        is_error: kind == StatusKind::Error,
    }
}

fn agent_tool_display_summary(summary: &ToolCardRender) -> Option<String> {
    if tool_name_matches(&summary.tool_name, "read_agent_result") {
        return agent_result_page_summary(summary);
    }
    if tool_name_matches(&summary.tool_name, "wait_agent") {
        if agent_payload_bool(summary, "result_available").unwrap_or(false) {
            return Some("result ready".to_owned());
        }
        return Some("result pending".to_owned());
    }
    if tool_name_matches(&summary.tool_name, "spawn_agent")
        && agent_payload_bool(summary, "summary_truncated").unwrap_or(false)
    {
        return Some("summary truncated · read_agent_result available".to_owned());
    }
    if tool_name_matches(&summary.tool_name, "spawn_agent")
        && !agent_payload_bool(summary, "result_available").unwrap_or(false)
    {
        return Some("result pending".to_owned());
    }
    None
}

fn agent_tool_title(summary: &ToolCardRender) -> ToolCardTitle {
    let thread = agent_thread_label(summary);
    if tool_name_matches(&summary.tool_name, "spawn_agent") {
        if thread == "agent" {
            return ToolCardTitle::new("Started", "agent", None);
        }
        return ToolCardTitle::new("Started", "agent", Some(thread));
    }
    if tool_name_matches(&summary.tool_name, "wait_agent") {
        return ToolCardTitle::new("Checked", "agent", Some(thread));
    }
    if tool_name_matches(&summary.tool_name, "read_agent_result") {
        return ToolCardTitle::new("Read", "agent result", Some(thread));
    }
    if tool_name_matches(&summary.tool_name, "message_agent") {
        return ToolCardTitle::new("Messaged", "agent", Some(thread));
    }
    if tool_name_matches(&summary.tool_name, "close_agent") {
        return ToolCardTitle::new("Closed", "agent", Some(thread));
    }
    ToolCardTitle::new("Called", "agent", Some(thread))
}

fn agent_status_display_label(status: &str) -> &'static str {
    match status {
        "idle" => "IDLE",
        "started" => "STARTED",
        "running" => "RUNNING",
        "completed" => "DONE",
        "failed" => "FAILED",
        "blocked" => "BLOCKED",
        "cancelled" => "CANCELLED",
        "interrupted" => "INTERRUPTED",
        "closed" => "CLOSED",
        "unavailable" => "UNAVAILABLE",
        "unknown" => "UNKNOWN",
        _ => "AGENT",
    }
}

fn agent_status_detail(summary: &ToolCardRender) -> String {
    let status = agent_payload_string(summary, "status").unwrap_or_else(|| "unknown".to_owned());
    format!("{} · {}", status, agent_thread_label(summary))
}

fn agent_result_page_summary(summary: &ToolCardRender) -> Option<String> {
    let page = agent_payload_value(summary)?.get("page")?;
    let offset = page.get("offset_chars").and_then(Value::as_u64)?;
    let returned = page.get("returned_chars").and_then(Value::as_u64)?;
    let total = page.get("total_chars").and_then(Value::as_u64)?;
    let more = if page
        .get("truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        " · more"
    } else {
        ""
    };
    Some(format!("chars {offset}+{returned}/{total}{more}"))
}

fn agent_result_read_tool(summary: &ToolCardRender) -> Option<String> {
    let value = agent_payload_value(summary)?;
    value
        .get("result_ref")
        .and_then(|result_ref| result_ref.get("read_tool"))
        .or_else(|| {
            value
                .get("result_fetch")
                .and_then(|result_fetch| result_fetch.get("tool"))
        })
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn agent_thread_label(summary: &ToolCardRender) -> String {
    let display_name = agent_payload_string(summary, "display_name");
    let objective = agent_payload_string(summary, "objective");
    let profile_id = agent_payload_string(summary, "profile_id")
        .or_else(|| call_argument(summary, "profile_id"));
    let thread_id =
        agent_payload_string(summary, "thread_id").or_else(|| call_argument(summary, "thread_id"));
    truncate_inline_text(
        &resolve_agent_display_name(AgentDisplayNameInput {
            display_name: display_name.as_deref(),
            objective: objective.as_deref(),
            profile_id: profile_id.as_deref(),
            thread_id: thread_id.as_deref(),
            ..AgentDisplayNameInput::default()
        })
        .label,
        48,
    )
}

fn agent_payload_value(summary: &ToolCardRender) -> Option<&Value> {
    summary.preview_value.as_ref()
}

fn agent_payload_string(summary: &ToolCardRender, key: &str) -> Option<String> {
    let value = agent_payload_value(summary)?.get(key)?;
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Null => None,
        _ => Some(value.to_string()),
    }
}

fn agent_payload_bool(summary: &ToolCardRender, key: &str) -> Option<bool> {
    agent_payload_value(summary)?.get(key)?.as_bool()
}

fn file_change_count_label(summary: &ToolCardRender) -> &'static str {
    if summary.metadata.action.as_deref() == Some("delete")
        || tool_name_matches(&summary.tool_name, "delete_file")
    {
        "deleted"
    } else {
        "changed"
    }
}

fn file_change_result_label(summary: &ToolCardRender) -> &'static str {
    if summary.metadata.action.as_deref() == Some("delete")
        || tool_name_matches(&summary.tool_name, "delete_file")
    {
        "delete summary"
    } else if tool_name_matches(&summary.tool_name, "edit_file") {
        "edit summary"
    } else if tool_name_matches(&summary.tool_name, "write_file") {
        "write summary"
    } else {
        "file summary"
    }
}

#[cfg(test)]
fn render_tool_diff_preview(
    summary: &ToolCardRender,
    diff: &ToolCardDiff,
    accent: Color,
) -> Vec<Line<'static>> {
    let palette = crate::ui::theme::default_palette();
    render_tool_diff_preview_with_palette(summary, diff, accent, &palette)
}

fn render_tool_diff_preview_with_palette(
    summary: &ToolCardRender,
    diff: &ToolCardDiff,
    accent: Color,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    let mut lines = vec![timeline_section_line_with_palette(
        accent,
        "diff",
        palette.accent_warning,
        vec![Span::styled(
            diff.summary.clone(),
            Style::default().fg(palette.text_muted),
        )],
        palette,
    )];
    for file in &diff.files {
        lines.push(timeline_content_line(
            accent,
            vec![
                timeline_badge_with_palette(
                    tool_diff_file_label(summary, file),
                    palette.accent_info,
                    palette,
                ),
                Span::raw(" "),
                Span::styled(file.path.clone(), Style::default().fg(palette.text_primary)),
                Span::raw(" "),
                Span::styled(
                    diff_hunk_summary(file),
                    Style::default().fg(palette.text_muted),
                ),
            ],
        ));
        let numbered_lines = number_unified_diff_lines(file.lines.iter().map(String::as_str));
        let line_number_width = diff_line_number_width(&numbered_lines);
        for line in numbered_lines {
            if matches!(line.kind, DiffLineKind::Hunk) {
                continue;
            }
            lines.push(render_tool_diff_line_with_palette(
                accent,
                line,
                line_number_width,
                palette,
            ));
        }
        if file.truncated {
            let hidden = file
                .original_line_count
                .saturating_sub(file.rendered_line_count);
            lines.push(timeline_content_line(
                accent,
                vec![Span::styled(
                    format!("diff truncated · {hidden} lines hidden"),
                    Style::default()
                        .fg(palette.accent_warning)
                        .add_modifier(Modifier::BOLD),
                )],
            ));
        }
    }
    if diff.truncated && diff.files.iter().all(|file| !file.truncated) {
        let hidden = diff
            .original_line_count
            .saturating_sub(diff.rendered_line_count);
        lines.push(timeline_content_line(
            accent,
            vec![Span::styled(
                format!("diff truncated · {hidden} lines hidden"),
                Style::default()
                    .fg(palette.accent_warning)
                    .add_modifier(Modifier::BOLD),
            )],
        ));
    }
    lines
}

fn diff_hunk_summary(file: &ToolCardDiffFile) -> String {
    let count = file
        .lines
        .iter()
        .filter(|line| matches!(diff_line_kind(line), DiffLineKind::Hunk))
        .count();
    match count {
        0 => "0 hunks".to_owned(),
        1 => "1 hunk".to_owned(),
        count => format!("{count} hunks"),
    }
}

fn tool_diff_file_label(summary: &ToolCardRender, file: &ToolCardDiffFile) -> &'static str {
    if summary.metadata.action.as_deref() == Some("delete")
        || tool_name_matches(&summary.tool_name, "delete_file")
    {
        return "deleted";
    }
    let (added, removed) = file_diff_line_stats(file);
    match (added > 0, removed > 0) {
        (true, false) => "created",
        (false, true) => "deleted",
        (true, true) => "modified",
        (false, false) => "changed",
    }
}

fn file_diff_line_stats(file: &ToolCardDiffFile) -> (usize, usize) {
    file.lines.iter().fold((0, 0), |(added, removed), line| {
        match diff_line_kind(line) {
            DiffLineKind::Added => (added + 1, removed),
            DiffLineKind::Removed => (added, removed + 1),
            _ => (added, removed),
        }
    })
}

fn render_tool_diff_line_with_palette(
    accent: Color,
    line: NumberedDiffLine<'_>,
    line_number_width: usize,
    palette: &ThemePalette,
) -> Line<'static> {
    let (marker_color, body_style) = diff_line_style_for_palette(line.kind, palette);
    timeline_content_line(
        accent,
        vec![
            Span::styled("│", Style::default().fg(marker_color)),
            Span::styled(
                diff_line_number_text(line.old_line, line_number_width),
                tool_diff_old_line_number_style_with_palette(line, palette),
            ),
            Span::styled(" ", Style::default().fg(palette.text_muted)),
            Span::styled(
                diff_line_number_text(line.new_line, line_number_width),
                tool_diff_new_line_number_style_with_palette(line, palette),
            ),
            Span::styled("│ ", Style::default().fg(palette.text_muted)),
            Span::styled(
                if line.text.is_empty() {
                    " ".to_owned()
                } else {
                    line.text.to_owned()
                },
                body_style,
            ),
        ],
    )
}

fn tool_diff_old_line_number_style_with_palette(
    line: NumberedDiffLine<'_>,
    palette: &ThemePalette,
) -> Style {
    if line.old_line.is_none() {
        return Style::default().fg(palette.text_muted);
    }
    let style = Style::default().fg(palette.diff_removed_fg);
    if matches!(line.kind, DiffLineKind::Removed) {
        style.add_modifier(Modifier::BOLD)
    } else {
        style
    }
}

fn tool_diff_new_line_number_style_with_palette(
    line: NumberedDiffLine<'_>,
    palette: &ThemePalette,
) -> Style {
    if line.new_line.is_none() {
        return Style::default().fg(palette.text_muted);
    }
    let style = Style::default().fg(palette.diff_added_fg);
    if matches!(line.kind, DiffLineKind::Added) {
        style.add_modifier(Modifier::BOLD)
    } else {
        style
    }
}

#[cfg(test)]
fn render_generic_tool_preview(
    summary: &ToolCardRender,
    accent: Color,
    max_content_width: usize,
) -> Vec<Line<'static>> {
    let palette = crate::ui::theme::default_palette();
    render_generic_tool_preview_with_palette(
        summary,
        accent,
        max_content_width,
        SyntaxThemeId::default(),
        &palette,
    )
}

fn render_generic_tool_preview_with_palette(
    summary: &ToolCardRender,
    accent: Color,
    max_content_width: usize,
    syntax_theme: SyntaxThemeId,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if let Some(value) = &summary.preview_value {
        lines.push(timeline_section_line_with_palette(
            accent,
            "tree",
            palette.accent_info,
            vec![Span::styled(
                "structured payload",
                Style::default().fg(palette.text_muted),
            )],
            palette,
        ));
        for line in render_json_tree_preview(value) {
            lines.push(timeline_content_line(
                accent,
                render_code_line_spans_with_bg(
                    &line,
                    palette.accent_info,
                    Style::default().fg(palette.markdown_code_fg),
                    palette.markdown_code_bg,
                ),
            ));
        }
    } else if summary.preview_kind == ToolPreviewKind::Markdown {
        lines.push(timeline_section_line_with_palette(
            accent,
            "md",
            palette.accent_info,
            vec![Span::styled(
                "formatted preview",
                Style::default().fg(palette.text_muted),
            )],
            palette,
        ));
        lines.extend(render_markdown_timeline_lines_with_palette(
            accent,
            Style::default().fg(palette.text_primary),
            &summary.preview_lines.join("\n"),
            MarkdownRenderOptions::tool_preview(max_content_width).with_syntax_theme(syntax_theme),
            palette,
        ));
    } else {
        lines.push(timeline_section_line_with_palette(
            accent,
            summary.preview_kind.label(),
            palette.accent_info,
            vec![Span::styled(
                summary.preview_kind.description(),
                Style::default().fg(palette.text_muted),
            )],
            palette,
        ));
        lines.extend(render_code_preview_lines_with_palette(
            accent,
            &summary.preview_lines,
            palette.markdown_code_bg,
            palette,
        ));
    }
    lines.extend(render_tool_hidden_tail(
        accent,
        summary.hidden_lines,
        palette,
    ));
    lines
}

#[allow(dead_code)]
fn render_code_preview_lines(accent: Color, lines: &[String], bg: Color) -> Vec<Line<'static>> {
    let palette = crate::ui::theme::default_palette();
    render_code_preview_lines_with_palette(accent, lines, bg, &palette)
}

fn render_code_preview_lines_with_palette(
    accent: Color,
    lines: &[String],
    bg: Color,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    lines
        .iter()
        .map(|line| {
            timeline_content_line(
                accent,
                render_code_line_spans_with_bg(
                    line,
                    palette.accent_info,
                    Style::default().fg(palette.markdown_code_fg),
                    bg,
                ),
            )
        })
        .collect()
}

fn render_tool_hidden_tail(
    accent: Color,
    hidden_lines: usize,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    if hidden_lines == 0 {
        return Vec::new();
    }
    vec![timeline_content_line(
        accent,
        vec![Span::styled(
            format!("… {} more lines hidden", hidden_lines),
            Style::default()
                .fg(palette.text_muted)
                .add_modifier(Modifier::BOLD),
        )],
    )]
}

fn render_json_tree_preview(value: &Value) -> Vec<String> {
    let mut lines = Vec::new();
    push_json_tree_lines(value, "", None, &mut lines);
    if lines.is_empty() {
        lines.push("(empty)".to_owned());
    }
    lines
}

fn push_json_tree_lines(value: &Value, prefix: &str, key: Option<&str>, lines: &mut Vec<String>) {
    match value {
        Value::Object(object) => {
            if let Some(key) = key {
                lines.push(format!("{prefix}{key}: {{}}"));
            } else if prefix.is_empty() {
                lines.push("{object}".to_owned());
            }
            let len = object.len();
            for (index, (child_key, child_value)) in object.iter().enumerate() {
                let branch = if index + 1 == len {
                    "└─ "
                } else {
                    "├─ "
                };
                let next_prefix = if index + 1 == len {
                    format!("{prefix}   ")
                } else {
                    format!("{prefix}│  ")
                };
                if json_tree_is_leaf(child_value) {
                    lines.push(format!(
                        "{prefix}{branch}{child_key}: {}",
                        json_tree_leaf_text(child_value)
                    ));
                } else {
                    lines.push(format!(
                        "{prefix}{branch}{child_key}: {}",
                        json_tree_container_label(child_value)
                    ));
                    push_json_tree_lines(child_value, &next_prefix, None, lines);
                }
            }
        }
        Value::Array(items) => {
            if let Some(key) = key {
                lines.push(format!("{prefix}{key}: [{}]", items.len()));
            } else if prefix.is_empty() {
                lines.push(format!("[array] {}", items.len()));
            }
            for (index, item) in items.iter().enumerate() {
                let branch = if index + 1 == items.len() {
                    "└─ "
                } else {
                    "├─ "
                };
                let next_prefix = if index + 1 == items.len() {
                    format!("{prefix}   ")
                } else {
                    format!("{prefix}│  ")
                };
                if json_tree_is_leaf(item) {
                    lines.push(format!(
                        "{prefix}{branch}[{index}] {}",
                        json_tree_leaf_text(item)
                    ));
                } else {
                    lines.push(format!(
                        "{prefix}{branch}[{index}] {}",
                        json_tree_container_label(item)
                    ));
                    push_json_tree_lines(item, &next_prefix, None, lines);
                }
            }
        }
        _ => {
            let leaf = json_tree_leaf_text(value);
            if let Some(key) = key {
                lines.push(format!("{prefix}{key}: {leaf}"));
            } else {
                lines.push(format!("{prefix}{leaf}"));
            }
        }
    }
}

fn json_tree_is_leaf(value: &Value) -> bool {
    !matches!(value, Value::Object(_) | Value::Array(_))
}

fn json_tree_leaf_text(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(boolean) => boolean.to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => format!("\"{}\"", truncate_inline_text(text, 100)),
        Value::Array(items) => format!("[{}]", items.len()),
        Value::Object(object) => format!("{{{}}}", object.len()),
    }
}

fn json_tree_container_label(value: &Value) -> String {
    match value {
        Value::Array(items) => format!("[{} items]", items.len()),
        Value::Object(object) => format!("{{{} keys}}", object.len()),
        _ => json_tree_leaf_text(value),
    }
}

fn json_string_list(value: &Value) -> Option<Vec<String>> {
    let entries = value
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    Some(entries)
}

fn infer_string_list_preview(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .map(|line| {
            line.trim()
                .trim_end_matches(',')
                .trim_matches('"')
                .to_owned()
        })
        .filter(|line| !line.is_empty() && line != "[" && line != "]")
        .collect()
}

fn json_grep_matches(value: &Value) -> Option<Vec<(String, u64, String)>> {
    let array = value.as_array()?;
    let mut matches = Vec::new();
    for entry in array {
        let object = entry.as_object()?;
        let path = object.get("path")?.as_str()?.to_owned();
        let line = object.get("line")?.as_u64()?;
        let text = object.get("text")?.as_str()?.to_owned();
        matches.push((path, line, text));
    }
    Some(matches)
}

fn tool_name_matches(tool_name: &str, expected: &str) -> bool {
    tool_name == expected || tool_name.ends_with(&format!("_{expected}"))
}

struct ToolCardRender {
    call_id: Option<String>,
    tool_name: String,
    is_error: bool,
    error_kind: Option<String>,
    summary: Option<String>,
    metadata: ToolCardMetadata,
    preview_kind: ToolPreviewKind,
    preview_lines: Vec<String>,
    hidden_lines: usize,
    preview_value: Option<Value>,
    diff: Option<ToolCardDiff>,
}

#[derive(Default)]
struct ToolCardMetadata {
    exit_code: Option<i64>,
    stdout_bytes: Option<u64>,
    stderr_bytes: Option<u64>,
    changed_files: Vec<String>,
    call_summary: Option<String>,
    action: Option<String>,
    mcp_server: Option<String>,
    mcp_tool: Option<String>,
    mcp_trust_class: Option<String>,
    code_server: Option<String>,
    code_capability: Option<String>,
    returned_entries: Option<u64>,
    total_entries: Option<u64>,
    execution_backend: Option<String>,
    execution_network_policy: Option<String>,
    execution_timeout_source: Option<String>,
    execution_cleanup_status: Option<String>,
    terminal_enforcement_backend: Option<String>,
    terminal_sandbox_profile: Option<String>,
    terminal_cleanup_status: Option<String>,
    terminal_task_id: Option<String>,
    terminal_status: Option<String>,
    terminal_command: Option<String>,
    terminal_log_path: Option<String>,
    terminal_exit_code: Option<i64>,
    terminal_failed_reason: Option<String>,
}

struct ToolCardDisplay {
    title: ToolCardTitle,
    status: ToolCardDisplayStatus,
    summary: Option<String>,
}

struct ToolCardTitle {
    action: String,
    subject: String,
    args: Option<String>,
}

impl ToolCardTitle {
    fn new(action: impl Into<String>, subject: impl Into<String>, args: Option<String>) -> Self {
        Self {
            action: action.into(),
            subject: subject.into(),
            args,
        }
    }

    fn plain(&self) -> String {
        match &self.args {
            Some(args) if !args.is_empty() => {
                format!("{} {} {}", self.action, self.subject, args)
            }
            _ => format!("{} {}", self.action, self.subject),
        }
    }
}

struct ToolCardDisplayStatus {
    label: &'static str,
    detail: Option<String>,
    kind: StatusKind,
    is_error: bool,
}

fn build_tool_card_display(summary: &ToolCardRender) -> ToolCardDisplay {
    ToolCardDisplay {
        title: tool_action_title(summary),
        status: tool_display_status(summary),
        summary: tool_display_summary(summary),
    }
}

fn build_tool_activity_view(summary: &ToolCardRender, source: &str) -> ToolActivityView {
    let display = build_tool_card_display(summary);
    ToolActivityView {
        key: tool_activity_key(summary, source),
        title: display.title.plain(),
        is_inspection: tool_activity_is_inspection_summary(summary),
        defaults_expanded: summary.diff.is_some() || terminal_task_is_active(summary),
    }
}

fn tool_display_status(summary: &ToolCardRender) -> ToolCardDisplayStatus {
    if terminal_task_tool(summary) {
        return terminal_task_display_status(summary);
    }
    if agent_tool(summary)
        && !summary.is_error
        && let Some(status) = agent_payload_string(summary, "status")
    {
        return agent_tool_display_status(&status);
    }
    let label = if summary.is_error {
        match summary.error_kind.as_deref() {
            Some("approval_denied") | Some("permission_denied") => "DENIED",
            Some("interrupted") => "INTERRUPTED",
            Some("timeout") => "TIMEOUT",
            _ => "ERROR",
        }
    } else {
        "OK"
    };
    let detail = if tool_name_matches(&summary.tool_name, "bash") {
        let mut details = Vec::new();
        if let Some(code) = summary.metadata.exit_code {
            details.push(format!("exit {code}"));
        }
        if let Some(network_policy) = &summary.metadata.execution_network_policy {
            let network_label = summary
                .metadata
                .execution_backend
                .as_deref()
                .map(|backend| format!("{backend} network {network_policy}"))
                .unwrap_or_else(|| format!("network {network_policy}"));
            details.push(network_label);
        }
        if let Some(timeout_source) = summary
            .metadata
            .execution_timeout_source
            .as_deref()
            .filter(|source| *source != "none")
        {
            details.push(format!("timeout {timeout_source}"));
        }
        if let Some(cleanup_status) = summary
            .metadata
            .execution_cleanup_status
            .as_deref()
            .filter(|status| *status != "not_needed")
        {
            details.push(format!("cleanup {cleanup_status}"));
        }
        if details.is_empty() {
            None
        } else {
            Some(details.join(" · "))
        }
    } else {
        summary
            .metadata
            .mcp_trust_class
            .as_deref()
            .map(|trust_class| format!("trust {trust_class}"))
    };
    ToolCardDisplayStatus {
        label,
        detail,
        kind: if summary.is_error {
            StatusKind::Error
        } else {
            StatusKind::Success
        },
        is_error: summary.is_error,
    }
}

fn tool_display_summary(summary: &ToolCardRender) -> Option<String> {
    if agent_tool(summary)
        && let Some(summary) = agent_tool_display_summary(summary)
    {
        return Some(summary);
    }
    if tool_name_matches(&summary.tool_name, "bash")
        && !summary.is_error
        && summary.preview_lines.is_empty()
        && summary.preview_value.is_none()
        && summary.hidden_lines == 0
    {
        return Some("(no output)".to_owned());
    }
    if let Some(diff) = &summary.diff {
        return Some(format!("diff {}", diff.summary));
    }
    summary.summary.clone()
}

fn tool_action_title(summary: &ToolCardRender) -> ToolCardTitle {
    if terminal_task_tool(summary) {
        return ToolCardTitle::new(
            "Terminal",
            summary
                .metadata
                .terminal_task_id
                .clone()
                .unwrap_or_else(|| "task".to_owned()),
            summary
                .metadata
                .terminal_command
                .as_deref()
                .map(|command| truncate_inline_text(command, 96)),
        );
    }
    if tool_name_matches(&summary.tool_name, "bash") {
        let command = call_argument(summary, "command")
            .or_else(|| summary.metadata.call_summary.clone())
            .unwrap_or_else(|| summary.tool_name.clone());
        if !summary.is_error
            && let Some(search) = classify_simple_shell_search(&command)
        {
            return ToolCardTitle::new(
                "Searched",
                search.pattern,
                search.location.map(|location| format!("in {location}")),
            );
        }
        return shell_command_title("Ran", &command);
    }
    if tool_name_matches(&summary.tool_name, "read_file") {
        return ToolCardTitle::new("Read", primary_path(summary), None);
    }
    if tool_name_matches(&summary.tool_name, "write_file") {
        return ToolCardTitle::new(write_file_action(summary), primary_path(summary), None);
    }
    if tool_name_matches(&summary.tool_name, "edit_file") {
        return ToolCardTitle::new("Edited", primary_path(summary), None);
    }
    if tool_name_matches(&summary.tool_name, "delete_file") {
        return ToolCardTitle::new("Deleted", primary_path(summary), None);
    }
    if tool_name_matches(&summary.tool_name, "code_action") {
        return ToolCardTitle::new(
            "Applied",
            primary_path(summary),
            Some("code action".to_owned()),
        );
    }
    if tool_name_matches(&summary.tool_name, "code_rename") {
        return ToolCardTitle::new("Renamed", primary_path(summary), Some("symbol".to_owned()));
    }
    if tool_name_matches(&summary.tool_name, "grep") {
        let pattern = call_argument(summary, "pattern").unwrap_or_else(|| "pattern".to_owned());
        let path = call_argument(summary, "path").unwrap_or_else(|| "workspace".to_owned());
        return ToolCardTitle::new("Searched", pattern, Some(format!("in {path}")));
    }
    if tool_name_matches(&summary.tool_name, "glob") {
        return ToolCardTitle::new(
            "Searched",
            call_argument(summary, "pattern").unwrap_or_else(|| summary.tool_name.clone()),
            None,
        );
    }
    if tool_name_matches(&summary.tool_name, "ls") {
        return ToolCardTitle::new("Listed", primary_path(summary), None);
    }
    if tool_name_matches(&summary.tool_name, "code_symbols") {
        return ToolCardTitle::new(
            "Inspected",
            primary_path(summary),
            Some("symbols".to_owned()),
        );
    }
    if tool_name_matches(&summary.tool_name, "code_workspace_symbols") {
        return ToolCardTitle::new(
            "Searched",
            call_argument(summary, "query").unwrap_or_else(|| "symbols".to_owned()),
            Some("workspace".to_owned()),
        );
    }
    if tool_name_matches(&summary.tool_name, "code_definition") {
        return ToolCardTitle::new(
            "Located",
            primary_path(summary),
            Some("definition".to_owned()),
        );
    }
    if tool_name_matches(&summary.tool_name, "code_references") {
        return ToolCardTitle::new(
            "Searched",
            primary_path(summary),
            Some("references".to_owned()),
        );
    }
    if tool_name_matches(&summary.tool_name, "code_actions") {
        return ToolCardTitle::new(
            "Inspected",
            primary_path(summary),
            Some("actions".to_owned()),
        );
    }
    if tool_name_matches(&summary.tool_name, "code_diagnostics") {
        return ToolCardTitle::new(
            "Checked",
            primary_path(summary),
            Some("diagnostics".to_owned()),
        );
    }
    if agent_tool(summary) {
        return agent_tool_title(summary);
    }
    if let Some(mcp) = mcp_tool_display(summary) {
        return ToolCardTitle::new("Called", mcp.tool, Some(format!("on {}", mcp.server)));
    }
    match &summary.metadata.call_summary {
        Some(call_summary) => ToolCardTitle::new(
            "Called",
            summary.tool_name.clone(),
            Some(sanitize_call_summary(call_summary)),
        ),
        None => ToolCardTitle::new("Called", summary.tool_name.clone(), None),
    }
}

fn tool_title_spans_with_palette(
    title: &ToolCardTitle,
    max_chars: usize,
    palette: &ThemePalette,
) -> Vec<Span<'static>> {
    let action_style = Style::default()
        .fg(palette.accent_warning)
        .add_modifier(Modifier::BOLD);
    let subject_style = Style::default()
        .fg(palette.accent_info)
        .add_modifier(Modifier::BOLD);
    let args_style = Style::default().fg(palette.text_primary);
    let segments = title_segments(title, action_style, subject_style, args_style);
    let plain_len = title.plain().chars().count();
    if plain_len <= max_chars {
        return segments
            .into_iter()
            .map(|(segment, style)| Span::styled(segment, style))
            .collect();
    }

    let mut remaining = max_chars.saturating_sub(3).max(1);
    let mut spans = Vec::new();
    for (segment, style) in segments {
        let segment_len = segment.chars().count();
        if segment_len <= remaining {
            spans.push(Span::styled(segment, style));
            remaining -= segment_len;
            if remaining == 0 {
                spans.push(Span::styled("...", style));
                break;
            }
            continue;
        }
        let truncated = segment.chars().take(remaining).collect::<String>();
        spans.push(Span::styled(format!("{truncated}..."), style));
        break;
    }
    if spans.is_empty() {
        spans.push(Span::styled("...", args_style));
    }
    spans
}

fn title_segments(
    title: &ToolCardTitle,
    action_style: Style,
    subject_style: Style,
    args_style: Style,
) -> Vec<(String, Style)> {
    let mut segments = vec![
        (title.action.clone(), action_style),
        (" ".to_owned(), Style::default()),
        (title.subject.clone(), subject_style),
    ];
    if let Some(args) = &title.args
        && !args.is_empty()
    {
        segments.push((" ".to_owned(), Style::default()));
        segments.push((args.clone(), args_style));
    }
    segments
}

fn shell_command_title(action: &'static str, command: &str) -> ToolCardTitle {
    let command = command.trim();
    let mut parts = command.splitn(2, char::is_whitespace);
    let subject = parts
        .next()
        .filter(|part| !part.is_empty())
        .unwrap_or(command);
    let args = parts
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    ToolCardTitle::new(action, subject, args)
}

struct ShellSearch {
    pattern: String,
    location: Option<String>,
}

fn classify_simple_shell_search(command: &str) -> Option<ShellSearch> {
    if command_contains_shell_control(command) {
        return None;
    }
    let tokens = simple_shell_tokens(command)?;
    let (program, args) = tokens.split_first()?;
    if program.contains('=') {
        return None;
    }
    let program = program.rsplit('/').next().unwrap_or(program.as_str());
    match program {
        "rg" => classify_pattern_search_args(args, &["-e", "--regexp", "-g", "--glob"]),
        "grep" => classify_pattern_search_args(args, &["-e", "--regexp", "-f", "--file"]),
        "fd" => classify_pattern_search_args(args, &["-e", "--extension", "-t", "--type"]),
        "find" => classify_find_search_args(args),
        _ => None,
    }
}

fn command_contains_shell_control(command: &str) -> bool {
    command
        .chars()
        .any(|character| matches!(character, '|' | '>' | '<' | ';' | '&' | '`' | '$'))
}

fn simple_shell_tokens(command: &str) -> Option<Vec<String>> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    for character in command.trim().chars() {
        match quote {
            Some(active) if character == active => quote = None,
            Some(_) => current.push(character),
            None if character == '\'' || character == '"' => quote = Some(character),
            None if character.is_whitespace() => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            None => current.push(character),
        }
    }
    if quote.is_some() {
        return None;
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    (!tokens.is_empty()).then_some(tokens)
}

fn classify_pattern_search_args(
    args: &[String],
    options_with_values: &[&str],
) -> Option<ShellSearch> {
    let mut positional = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let token = &args[index];
        if token == "--" {
            positional.extend(args[index + 1..].iter().cloned());
            break;
        }
        if options_with_values.iter().any(|option| token == option) {
            index += 2;
            continue;
        }
        if token.starts_with("--") && token.contains('=') {
            index += 1;
            continue;
        }
        if token.starts_with('-') {
            index += 1;
            continue;
        }
        positional.push(token.clone());
        index += 1;
    }
    let pattern = positional.first()?.clone();
    Some(ShellSearch {
        pattern,
        location: positional.get(1).cloned(),
    })
}

fn classify_find_search_args(args: &[String]) -> Option<ShellSearch> {
    let location = args
        .iter()
        .find(|token| !token.starts_with('-') && token.as_str() != ".")
        .cloned()
        .or_else(|| args.first().cloned());
    let pattern = args
        .windows(2)
        .find_map(|window| {
            matches!(
                window[0].as_str(),
                "-name" | "-iname" | "-path" | "-ipath" | "-regex"
            )
            .then(|| window[1].clone())
        })
        .or_else(|| {
            args.iter()
                .find(|token| token.contains('*') || token.contains('?'))
                .cloned()
        })?;
    Some(ShellSearch { pattern, location })
}

fn tool_activity_is_inspection_summary(summary: &ToolCardRender) -> bool {
    if tool_name_matches(&summary.tool_name, "read_file")
        || tool_name_matches(&summary.tool_name, "grep")
        || tool_name_matches(&summary.tool_name, "glob")
        || tool_name_matches(&summary.tool_name, "ls")
    {
        return true;
    }
    tool_name_matches(&summary.tool_name, "bash")
        && !summary.is_error
        && call_argument(summary, "command")
            .or_else(|| summary.metadata.call_summary.clone())
            .and_then(|command| classify_simple_shell_search(&command))
            .is_some()
}

fn tool_activity_key(summary: &ToolCardRender, source: &str) -> String {
    if terminal_task_tool(summary)
        && let Some(task_id) = &summary.metadata.terminal_task_id
    {
        return format!("terminal_task:{task_id}");
    }
    summary
        .call_id
        .as_ref()
        .map(|call_id| format!("call:{call_id}"))
        .unwrap_or_else(|| format!("hash:{:016x}", stable_tool_activity_hash(source)))
}

fn stable_tool_activity_hash(source: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in source.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn write_file_action(summary: &ToolCardRender) -> &'static str {
    if summary
        .diff
        .as_ref()
        .is_some_and(|diff| diff.files.iter().all(diff_file_is_create))
    {
        "Created"
    } else {
        "Wrote"
    }
}

fn diff_file_is_create(file: &ToolCardDiffFile) -> bool {
    let (added, removed) = file_diff_line_stats(file);
    added > 0 && removed == 0
}

fn primary_path(summary: &ToolCardRender) -> String {
    call_argument(summary, "path")
        .or_else(|| summary.metadata.changed_files.first().cloned())
        .or_else(|| {
            summary
                .diff
                .as_ref()?
                .files
                .first()
                .map(|file| file.path.clone())
        })
        .unwrap_or_else(|| "workspace".to_owned())
}

fn call_argument(summary: &ToolCardRender, key: &str) -> Option<String> {
    let call_summary = summary.metadata.call_summary.as_deref()?;
    call_summary_argument(call_summary, key)
}

fn call_summary_argument(call_summary: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    let start = call_summary.find(&prefix)? + prefix.len();
    if key == "command" {
        return Some(call_summary[start..].trim().to_owned());
    }
    let tail = &call_summary[start..];
    let end = tail
        .find(|character: char| character.is_whitespace())
        .unwrap_or(tail.len());
    Some(tail[..end].trim().to_owned()).filter(|value| !value.is_empty())
}

struct McpToolDisplay {
    server: String,
    tool: String,
}

fn mcp_tool_display(summary: &ToolCardRender) -> Option<McpToolDisplay> {
    let (server_from_name, tool_from_name) = parse_mcp_provider_name(&summary.tool_name)
        .map(|(server, tool)| (Some(server), Some(tool)))
        .unwrap_or((None, None));
    let server = summary.metadata.mcp_server.clone().or(server_from_name)?;
    let tool = summary
        .metadata
        .mcp_tool
        .clone()
        .or(tool_from_name)
        .unwrap_or_else(|| summary.tool_name.clone());
    Some(McpToolDisplay { server, tool })
}

fn parse_mcp_provider_name(tool_name: &str) -> Option<(String, String)> {
    let remainder = tool_name.strip_prefix("mcp__")?;
    let (server, tool) = remainder.split_once("__")?;
    if server.is_empty() || tool.is_empty() {
        return None;
    }
    Some((server.to_owned(), tool.to_owned()))
}

fn sanitize_call_summary(call_summary: &str) -> String {
    truncate_inline_text(
        &call_summary
            .split_whitespace()
            .filter(|part| !part.starts_with("call_") && !part.starts_with("id="))
            .collect::<Vec<_>>()
            .join(" "),
        120,
    )
}

struct ToolCardDiff {
    summary: String,
    truncated: bool,
    original_line_count: usize,
    rendered_line_count: usize,
    files: Vec<ToolCardDiffFile>,
}

struct ToolCardDiffFile {
    path: String,
    lines: Vec<String>,
    truncated: bool,
    original_line_count: usize,
    rendered_line_count: usize,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum ToolPreviewKind {
    Markdown,
    Json,
    #[default]
    Text,
}

impl ToolPreviewKind {
    fn label(self) -> &'static str {
        match self {
            Self::Markdown => "md",
            Self::Json => "json",
            Self::Text => "text",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Markdown => "formatted preview",
            Self::Json => "structured preview",
            Self::Text => "captured output",
        }
    }

    fn from_value(value: &str) -> Self {
        match value {
            "markdown" => Self::Markdown,
            "json" => Self::Json,
            _ => Self::Text,
        }
    }
}

fn parse_tool_summary(text: &str) -> ToolCardRender {
    let fallback = ToolCardRender {
        call_id: None,
        tool_name: "result".to_owned(),
        is_error: false,
        error_kind: None,
        summary: None,
        metadata: ToolCardMetadata::default(),
        preview_kind: ToolPreviewKind::Text,
        preview_lines: text.lines().take(8).map(str::to_owned).collect(),
        hidden_lines: text.lines().count().saturating_sub(8),
        preview_value: None,
        diff: None,
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return fallback;
    };
    let Some(object) = value.as_object() else {
        return fallback;
    };
    let Some(tool_name) = object
        .get("tool_name")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
    else {
        return fallback;
    };
    let status = object
        .get("status")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("ok")
        .to_uppercase();
    let is_error = status == "ERROR";
    let error_kind = object
        .get("error_kind")
        .and_then(Value::as_str)
        .or_else(|| {
            object
                .get("error")
                .and_then(|error| error.get("kind"))
                .and_then(Value::as_str)
        })
        .map(str::to_owned);
    let metadata = object
        .get("metadata")
        .map(parse_tool_metadata)
        .unwrap_or_default();
    let summary = object
        .get("summary")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let preview_kind = object
        .get("preview_kind")
        .and_then(serde_json::Value::as_str)
        .map(ToolPreviewKind::from_value)
        .or_else(|| object.get("content").map(legacy_tool_preview_kind))
        .unwrap_or_default();
    let preview_value = object.get("preview_value").cloned().or_else(|| {
        object
            .get("content")
            .cloned()
            .filter(|value| matches!(value, Value::Array(_) | Value::Object(_)))
    });
    let (preview_lines, hidden_lines) = object
        .get("preview_lines")
        .and_then(serde_json::Value::as_array)
        .map(|lines| {
            let preview = lines
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let hidden = object
                .get("hidden_lines")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0) as usize;
            (preview, hidden)
        })
        .unwrap_or_else(|| legacy_tool_preview(object.get("content"), preview_kind));
    let diff = object.get("diff").and_then(parse_tool_diff);

    ToolCardRender {
        call_id: object
            .get("call_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
        tool_name,
        is_error,
        error_kind,
        summary,
        metadata,
        preview_kind,
        preview_lines,
        hidden_lines,
        preview_value,
        diff,
    }
}

fn parse_tool_diff(value: &Value) -> Option<ToolCardDiff> {
    let object = value.as_object()?;
    let files = object
        .get("files")
        .and_then(Value::as_array)?
        .iter()
        .filter_map(parse_tool_diff_file)
        .collect::<Vec<_>>();
    if files.is_empty() {
        return None;
    }
    Some(ToolCardDiff {
        summary: object
            .get("summary")
            .and_then(Value::as_str)
            .unwrap_or("file diff")
            .to_owned(),
        truncated: object
            .get("truncated")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        original_line_count: object
            .get("original_line_count")
            .and_then(Value::as_u64)
            .unwrap_or_else(|| {
                files
                    .iter()
                    .map(|file| file.original_line_count as u64)
                    .sum()
            }) as usize,
        rendered_line_count: object
            .get("rendered_line_count")
            .and_then(Value::as_u64)
            .unwrap_or_else(|| files.iter().map(|file| file.lines.len() as u64).sum())
            as usize,
        files,
    })
}

fn parse_tool_diff_file(value: &Value) -> Option<ToolCardDiffFile> {
    let object = value.as_object()?;
    let lines = object
        .get("lines")
        .and_then(Value::as_array)?
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    Some(ToolCardDiffFile {
        path: object
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned(),
        truncated: object
            .get("truncated")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        original_line_count: object
            .get("original_line_count")
            .and_then(Value::as_u64)
            .unwrap_or(lines.len() as u64) as usize,
        rendered_line_count: object
            .get("rendered_line_count")
            .and_then(Value::as_u64)
            .unwrap_or(lines.len() as u64) as usize,
        lines,
    })
}

fn legacy_tool_preview_kind(value: &serde_json::Value) -> ToolPreviewKind {
    match value {
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => ToolPreviewKind::Json,
        serde_json::Value::String(content)
            if content.trim_start().starts_with('#')
                || content.contains("```")
                || content.contains("\n- ")
                || content.contains("\n|") =>
        {
            ToolPreviewKind::Markdown
        }
        _ => ToolPreviewKind::Text,
    }
}

fn legacy_tool_preview(
    value: Option<&serde_json::Value>,
    preview_kind: ToolPreviewKind,
) -> (Vec<String>, usize) {
    let Some(value) = value else {
        return (Vec::new(), 0);
    };
    let source = match value {
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
            serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
        }
        serde_json::Value::String(text) => text.clone(),
        _ => value.to_string(),
    };
    let limit = match preview_kind {
        ToolPreviewKind::Markdown => 18,
        ToolPreviewKind::Json => 12,
        ToolPreviewKind::Text => 12,
    };
    let lines = source.lines().map(str::to_owned).collect::<Vec<_>>();
    let hidden_lines = lines.len().saturating_sub(limit);
    let preview_lines = lines.into_iter().take(limit).collect::<Vec<_>>();
    (preview_lines, hidden_lines)
}

fn parse_tool_metadata(value: &Value) -> ToolCardMetadata {
    let Some(object) = value.as_object() else {
        return ToolCardMetadata::default();
    };
    let details = object.get("details");
    let call_context = details.and_then(|details| details.get("call"));
    let (subject_mcp_server, subject_mcp_tool, subject_mcp_trust_class) =
        parse_mcp_call_subjects(call_context);
    let terminal_context = details
        .and_then(|details| details.get("terminal_task"))
        .or(details);
    ToolCardMetadata {
        exit_code: object.get("exit_code").and_then(Value::as_i64),
        stdout_bytes: object.get("stdout_bytes").and_then(Value::as_u64),
        stderr_bytes: object.get("stderr_bytes").and_then(Value::as_u64),
        changed_files: object
            .get("changed_files")
            .and_then(Value::as_array)
            .map(|files| {
                files
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
        call_summary: object
            .get("details")
            .and_then(|details| details.get("call"))
            .and_then(|call| call.get("summary"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        action: object
            .get("details")
            .and_then(|details| details.get("action"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        mcp_server: subject_mcp_server.or_else(|| {
            details
                .and_then(|details| details.get("mcp"))
                .and_then(|mcp| mcp.get("server"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        }),
        mcp_tool: subject_mcp_tool.or_else(|| {
            details
                .and_then(|details| details.get("mcp"))
                .and_then(|mcp| mcp.get("tool"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        }),
        mcp_trust_class: subject_mcp_trust_class.or_else(|| {
            details
                .and_then(|details| details.get("mcp"))
                .and_then(|mcp| mcp.get("trust_class"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        }),
        code_server: object
            .get("details")
            .and_then(|details| details.get("code_intelligence"))
            .and_then(|details| details.get("server"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        code_capability: object
            .get("details")
            .and_then(|details| details.get("code_intelligence"))
            .and_then(|details| details.get("capability"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        returned_entries: object
            .get("returned_entries")
            .and_then(Value::as_u64)
            .or_else(|| {
                object
                    .get("details")
                    .and_then(|details| details.get("code_intelligence"))
                    .and_then(|details| details.get("returned"))
                    .and_then(Value::as_u64)
            }),
        total_entries: object
            .get("total_entries")
            .and_then(Value::as_u64)
            .or_else(|| {
                object
                    .get("details")
                    .and_then(|details| details.get("code_intelligence"))
                    .and_then(|details| details.get("total"))
                    .and_then(Value::as_u64)
            }),
        execution_backend: details
            .and_then(|details| details.get("execution"))
            .and_then(|execution| execution.get("backend"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        execution_network_policy: details
            .and_then(|details| details.get("execution"))
            .and_then(|execution| execution.get("network"))
            .and_then(|network| network.get("policy"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        execution_timeout_source: details
            .and_then(|details| details.get("execution"))
            .and_then(|execution| execution.get("resources"))
            .and_then(|resources| resources.get("timeout_source"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        execution_cleanup_status: details
            .and_then(|details| details.get("execution"))
            .and_then(|execution| execution.get("resources"))
            .and_then(|resources| resources.get("cleanup"))
            .and_then(|cleanup| cleanup.get("status"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        terminal_enforcement_backend: terminal_context
            .and_then(|details| details.get("enforcement_backend"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        terminal_sandbox_profile: terminal_context
            .and_then(|details| details.get("sandbox_profile"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        terminal_cleanup_status: terminal_context
            .and_then(|details| details.get("cleanup"))
            .and_then(|cleanup| cleanup.get("status"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        terminal_task_id: terminal_context
            .and_then(|details| details.get("task_id"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        terminal_status: terminal_context
            .and_then(|details| details.get("status"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        terminal_command: terminal_context
            .and_then(|details| details.get("command"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        terminal_log_path: terminal_context
            .and_then(|details| details.get("log_path"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        terminal_exit_code: terminal_context
            .and_then(|details| details.get("status_detail"))
            .and_then(|status| status.get("exit_code"))
            .and_then(Value::as_i64),
        terminal_failed_reason: terminal_context
            .and_then(|details| details.get("status_detail"))
            .and_then(|status| status.get("reason"))
            .and_then(Value::as_str)
            .map(str::to_owned),
    }
}

fn parse_mcp_call_subjects(
    call_context: Option<&Value>,
) -> (Option<String>, Option<String>, Option<String>) {
    let mut mcp_server = None;
    let mut mcp_tool = None;
    let mut mcp_trust_class = None;
    let Some(subjects) = call_context
        .and_then(|call| call.get("subjects"))
        .and_then(Value::as_array)
    else {
        return (mcp_server, mcp_tool, mcp_trust_class);
    };
    for subject in subjects.iter().filter_map(Value::as_str) {
        let mut parts = subject.splitn(3, ':');
        let _scope = parts.next();
        let Some(kind) = parts.next() else {
            continue;
        };
        let Some(target) = parts.next() else {
            continue;
        };
        match kind {
            "mcp_tool" => {
                if let Some((server, tool)) = parse_mcp_provider_name(target) {
                    mcp_server = mcp_server.or(Some(server));
                    mcp_tool = mcp_tool.or(Some(tool));
                }
            }
            "mcp_trust_class" => {
                if let Some(trust_class) = target.strip_prefix("mcp_trust_class:") {
                    mcp_trust_class = mcp_trust_class.or(Some(trust_class.to_owned()));
                }
            }
            _ => {}
        }
    }
    (mcp_server, mcp_tool, mcp_trust_class)
}

fn tool_status_style(kind: StatusKind, palette: &ThemePalette) -> Style {
    StatusIndicator::static_kind(kind).badge_style_with_palette(palette)
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/tool_card_tests.rs"]
mod tests;
