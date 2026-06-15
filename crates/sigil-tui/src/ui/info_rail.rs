use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Paragraph, Wrap},
};

use crate::view_model::InfoRailViewModel;

use super::{
    geometry::inset_rect,
    primitives::section_badge,
    text::truncate_display_width,
    theme::{
        accent_blue, accent_gold, accent_lime, accent_rose, accent_teal, dim, ink, muted, rail_bg,
    },
};

pub(crate) fn render_info_rail(frame: &mut Frame, area: Rect, view_model: &InfoRailViewModel) {
    frame.render_widget(Block::default().style(Style::default().bg(rail_bg())), area);
    let inner = inset_rect(area, 3, 1);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let mut lines = vec![Line::from(vec![
        Span::styled(
            "info",
            Style::default()
                .fg(accent_blue())
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            truncate_display_width(&view_model.session_title, inner.width as usize),
            Style::default().fg(ink()).add_modifier(Modifier::BOLD),
        ),
    ])];
    lines.push(Line::from(vec![Span::styled(
        truncate_display_width(&view_model.workspace_label, inner.width as usize),
        Style::default().fg(dim()),
    )]));
    lines.push(Line::raw(String::new()));

    push_info_section(
        &mut lines,
        "session",
        accent_blue(),
        view_model.session_lines.iter().cloned(),
        inner.width as usize,
    );
    push_info_section(
        &mut lines,
        "permissions",
        accent_gold(),
        view_model.permission_lines.iter().cloned(),
        inner.width as usize,
    );
    push_info_section(
        &mut lines,
        "agents",
        accent_lime(),
        view_model.agent_lines.iter().cloned(),
        inner.width as usize,
    );
    if !view_model.task_lines.is_empty() {
        push_info_section(
            &mut lines,
            "task",
            accent_teal(),
            view_model.task_lines.iter().cloned(),
            inner.width as usize,
        );
    }
    if !view_model.mcp_lines.is_empty() {
        push_info_section(
            &mut lines,
            "MCP",
            accent_gold(),
            view_model.mcp_lines.iter().cloned(),
            inner.width as usize,
        );
    }
    push_info_section(
        &mut lines,
        "LSP",
        accent_teal(),
        view_model.code_lines.iter().cloned(),
        inner.width as usize,
    );
    push_info_section(
        &mut lines,
        "usage",
        accent_blue(),
        view_model.usage_lines.iter().cloned(),
        inner.width as usize,
    );
    push_info_section(
        &mut lines,
        "controls",
        accent_rose(),
        view_model.controls.iter().cloned(),
        inner.width as usize,
    );

    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .style(Style::default().bg(rail_bg()))
            .wrap(Wrap { trim: false }),
        inner,
    );
}

fn push_info_section<I>(
    lines: &mut Vec<Line<'static>>,
    title: &str,
    accent: Color,
    values: I,
    width: usize,
) where
    I: IntoIterator<Item = String>,
{
    lines.push(Line::from(vec![section_badge(title, accent)]));
    for value in values {
        lines.push(render_info_line(&value, width));
    }
    lines.push(Line::raw(String::new()));
}

fn render_info_line(value: &str, width: usize) -> Line<'static> {
    let clipped = truncate_display_width(value, width.saturating_sub(2).max(1));
    if let Some((label, rest)) = clipped.split_once(": ") {
        return Line::from(vec![
            Span::raw("  "),
            Span::styled(
                format!("{label}:"),
                Style::default().fg(dim()).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(rest.to_owned(), Style::default().fg(ink())),
        ]);
    }

    if let Some((marker, rest)) = clipped.split_once(' ')
        && let Some((marker_style, rest_style)) = marker_styles(marker)
    {
        return Line::from(vec![
            Span::raw("  "),
            Span::styled(format!("{marker} "), marker_style),
            Span::styled(rest.to_owned(), rest_style),
        ]);
    }

    Line::from(vec![
        Span::raw("  "),
        Span::styled(clipped, Style::default().fg(ink())),
    ])
}

fn marker_styles(marker: &str) -> Option<(Style, Style)> {
    match marker {
        ">" => Some((
            Style::default()
                .fg(accent_blue())
                .add_modifier(Modifier::BOLD),
            Style::default().fg(ink()),
        )),
        "-" | "·" => Some((Style::default().fg(dim()), Style::default().fg(muted()))),
        "▶" => Some((
            Style::default()
                .fg(accent_blue())
                .add_modifier(Modifier::BOLD),
            Style::default()
                .fg(accent_blue())
                .add_modifier(Modifier::BOLD),
        )),
        "✓" => Some((
            Style::default().fg(accent_lime()),
            Style::default().fg(accent_lime()),
        )),
        "!" => Some((
            Style::default()
                .fg(accent_rose())
                .add_modifier(Modifier::BOLD),
            Style::default()
                .fg(accent_rose())
                .add_modifier(Modifier::BOLD),
        )),
        "×" => Some((
            Style::default().fg(accent_gold()),
            Style::default().fg(accent_gold()),
        )),
        "⏸" => Some((
            Style::default()
                .fg(accent_gold())
                .add_modifier(Modifier::BOLD),
            Style::default()
                .fg(accent_gold())
                .add_modifier(Modifier::BOLD),
        )),
        _ => None,
    }
}

#[cfg(test)]
#[path = "tests/info_rail_tests.rs"]
mod tests;
