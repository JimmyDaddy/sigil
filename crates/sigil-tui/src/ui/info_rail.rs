use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Paragraph},
};

use crate::{view_model::InfoRailViewModel, workspace_git::WorkspaceGitStatus};

use super::{
    geometry::inset_rect,
    status_indicator::{indicator_styles_with_palette, render_marker_symbol},
    text::{terminal_cell_width, truncate_display_width},
    theme::Theme,
};

#[cfg(test)]
use super::theme::{accent_blue, accent_lime, accent_rose, dim, ink};

#[cfg(test)]
pub(crate) fn render_info_rail(frame: &mut Frame, area: Rect, view_model: &InfoRailViewModel) {
    let theme = Theme::default();
    render_info_rail_with_theme(frame, area, view_model, &theme);
}

pub(crate) fn render_info_rail_with_theme(
    frame: &mut Frame,
    area: Rect,
    view_model: &InfoRailViewModel,
    theme: &Theme,
) {
    let palette = &theme.palette;
    frame.render_widget(
        Block::default().style(Style::default().bg(palette.surface_rail)),
        area,
    );
    let body = info_rail_body_area(area);
    if body.width > 0 && body.height > 0 {
        let layout = info_rail_body_layout(view_model, body.width as usize, theme);
        frame.render_widget(
            Paragraph::new(Text::from(layout.lines))
                .style(Style::default().bg(palette.surface_rail)),
            body,
        );
    }
    if let Some(details) = info_rail_details_area(area) {
        frame.render_widget(
            Paragraph::new(truncate_display_width("F4 details", details.width as usize))
                .style(Style::default().fg(palette.text_secondary)),
            details,
        );
    }
}

pub(super) fn info_rail_body_area(area: Rect) -> Rect {
    let inner = inset_rect(area, 3, 1);
    Rect {
        height: inner.height.saturating_sub(1),
        ..inner
    }
}

pub(super) fn info_rail_details_area(area: Rect) -> Option<Rect> {
    let inner = inset_rect(area, 3, 1);
    (inner.width > 0 && inner.height > 0)
        .then(|| Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1))
}

pub(super) struct InfoRailBodyLayout {
    pub(super) lines: Vec<Line<'static>>,
    pub(super) agent_row_offsets: Vec<usize>,
}

pub(super) fn info_rail_body_layout(
    model: &InfoRailViewModel,
    width: usize,
    theme: &Theme,
) -> InfoRailBodyLayout {
    let palette = &theme.palette;
    let mut lines = vec![Line::styled(
        truncate_display_width(&model.workspace_label, width),
        Style::default()
            .fg(palette.text_primary)
            .add_modifier(Modifier::BOLD),
    )];
    if let Some(status) = &model.workspace_git_status {
        lines.extend(render_workspace_git_status(status, width, theme));
    }
    lines.extend(
        model
            .session_lines
            .iter()
            .map(|value| render_info_line_with_theme(value, width, theme)),
    );
    lines.push(Line::raw(String::new()));
    push_info_section(
        &mut lines,
        "attention",
        palette.status_error,
        model.permission_lines.iter().cloned(),
        width,
        theme,
    );
    let agent_start = lines.len().saturating_add(1);
    let agent_row_offsets =
        (agent_start..agent_start.saturating_add(model.agent_lines.len())).collect();
    for (title, values) in [
        ("agents", &model.agent_lines),
        ("task", &model.task_lines),
        ("MCP", &model.mcp_lines),
        ("LSP", &model.code_lines),
    ] {
        push_info_section(
            &mut lines,
            title,
            palette.text_secondary,
            values.iter().cloned(),
            width,
            theme,
        );
    }
    InfoRailBodyLayout {
        lines,
        agent_row_offsets,
    }
}

fn render_workspace_git_status(
    status: &WorkspaceGitStatus,
    width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let content_width = width.saturating_sub(2).max(1);
    let suffix = format!(" · {}", status.change_label());
    let fixed_width = terminal_cell_width("git: ") + terminal_cell_width(&suffix);
    let branch_budget = content_width.saturating_sub(fixed_width);
    let mut lines = if branch_budget >= 4 {
        let branch = truncate_display_width(&status.branch, branch_budget);
        vec![render_info_line_with_theme(
            &format!("git: {branch}{suffix}"),
            width,
            theme,
        )]
    } else {
        let branch = truncate_display_width(
            &status.branch,
            content_width.saturating_sub(terminal_cell_width("git: ")),
        );
        vec![
            render_info_line_with_theme(&format!("git: {branch}"), width, theme),
            render_workspace_git_detail(&status.change_label(), width, theme),
        ]
    };

    let detail_width = width.saturating_sub(7);
    let compact = detail_width < 18;
    let mut details = Vec::new();
    if status.conflicted_entries > 0 {
        details.push(if compact {
            format!("!{}", status.conflicted_entries)
        } else {
            format!("conf {}", status.conflicted_entries)
        });
    }
    if status.ahead > 0 {
        details.push(format!("↑{}", status.ahead));
    }
    if status.behind > 0 {
        details.push(format!("↓{}", status.behind));
    }
    let separator = if compact { " " } else { " · " };
    for detail in pack_workspace_git_details(&details, detail_width, separator) {
        lines.push(render_workspace_git_detail(&detail, width, theme));
    }
    lines
}

fn pack_workspace_git_details(details: &[String], width: usize, separator: &str) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let mut rows = Vec::new();
    let mut current = String::new();
    for detail in details {
        let candidate = if current.is_empty() {
            detail.clone()
        } else {
            format!("{current}{separator}{detail}")
        };
        if terminal_cell_width(&candidate) <= width {
            current = candidate;
            continue;
        }
        if !current.is_empty() {
            rows.push(std::mem::take(&mut current));
        }
        current = truncate_display_width(detail, width);
    }
    if !current.is_empty() {
        rows.push(current);
    }
    rows
}

fn render_workspace_git_detail(value: &str, width: usize, theme: &Theme) -> Line<'static> {
    let indent = 7.min(width.saturating_sub(1));
    let value = truncate_display_width(value, width.saturating_sub(indent));
    Line::from(vec![
        Span::raw(" ".repeat(indent)),
        Span::styled(value, Style::default().fg(theme.palette.text_secondary)),
    ])
}

fn push_info_section<I>(
    lines: &mut Vec<Line<'static>>,
    title: &str,
    accent: Color,
    values: I,
    width: usize,
    theme: &Theme,
) where
    I: IntoIterator<Item = String>,
{
    let mut values = values.into_iter().peekable();
    if values.peek().is_none() {
        return;
    }
    let _ = accent;
    lines.push(Line::from(Span::styled(
        title.to_owned(),
        Style::default()
            .fg(theme.palette.text_secondary)
            .add_modifier(Modifier::BOLD),
    )));
    for value in values {
        lines.push(render_info_line_with_theme(&value, width, theme));
    }
    lines.push(Line::raw(String::new()));
}

#[cfg(test)]
fn render_info_line(value: &str, width: usize) -> Line<'static> {
    let theme = Theme::default();
    render_info_line_with_theme(value, width, &theme)
}

fn render_info_line_with_theme(value: &str, width: usize, theme: &Theme) -> Line<'static> {
    let palette = &theme.palette;
    let clipped = truncate_display_width(value, width.saturating_sub(2).max(1));
    if let Some((marker, after_marker)) = clipped.split_once(' ')
        && let Some((marker_style, _)) = indicator_styles_with_palette(marker, palette)
        && let Some((label, rest)) = after_marker.split_once(": ")
    {
        let mut spans = vec![
            Span::raw("  "),
            Span::styled(format!("{} ", render_marker_symbol(marker)), marker_style),
            Span::styled(
                format!("{label}:"),
                Style::default()
                    .fg(palette.text_muted)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
        ];
        spans.extend(render_info_value_spans(rest, theme));
        return Line::from(spans);
    }

    if let Some((label, rest)) = clipped.split_once(": ") {
        let mut spans = vec![
            Span::raw("  "),
            Span::styled(
                format!("{label}:"),
                Style::default()
                    .fg(palette.text_muted)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
        ];
        spans.extend(render_info_value_spans(rest, theme));
        return Line::from(spans);
    }

    if let Some((marker, rest)) = clipped.split_once(' ')
        && let Some((marker_style, rest_style)) = indicator_styles_with_palette(marker, palette)
    {
        let marker = render_marker_symbol(marker);
        return Line::from(vec![
            Span::raw("  "),
            Span::styled(format!("{marker} "), marker_style),
            Span::styled(rest.to_owned(), rest_style),
        ]);
    }

    Line::from(vec![
        Span::raw("  "),
        Span::styled(clipped, Style::default().fg(palette.text_primary)),
    ])
}

fn render_info_value_spans(value: &str, theme: &Theme) -> Vec<Span<'static>> {
    let palette = &theme.palette;
    if let Some((marker, rest)) = value.split_once(' ')
        && let Some((marker_style, rest_style)) = indicator_styles_with_palette(marker, palette)
    {
        return vec![
            Span::styled(render_marker_symbol(marker).to_owned(), marker_style),
            Span::raw(" "),
            Span::styled(rest.to_owned(), rest_style),
        ];
    }
    vec![Span::styled(
        value.to_owned(),
        Style::default().fg(theme.palette.text_primary),
    )]
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/info_rail_tests.rs"]
mod tests;
