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
    primitives::section_badge_with_palette,
    status_indicator::{indicator_styles_with_palette, render_marker_symbol},
    text::truncate_display_width,
    theme::Theme,
};

#[cfg(test)]
use super::theme::{accent_blue, accent_gold, accent_lime, accent_rose, dim, ink};

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
    let inner = inset_rect(area, 3, 1);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let mut lines = vec![Line::from(vec![
        Span::styled(
            "info",
            Style::default()
                .fg(palette.accent_info)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            truncate_display_width(&view_model.session_title, inner.width as usize),
            Style::default()
                .fg(palette.text_primary)
                .add_modifier(Modifier::BOLD),
        ),
    ])];
    lines.push(Line::from(vec![Span::styled(
        truncate_display_width(&view_model.workspace_label, inner.width as usize),
        Style::default().fg(palette.text_muted),
    )]));
    lines.push(Line::raw(String::new()));

    push_info_section(
        &mut lines,
        "session",
        palette.accent_info,
        view_model.session_lines.iter().cloned(),
        inner.width as usize,
        theme,
    );
    push_info_section(
        &mut lines,
        "permissions",
        palette.accent_warning,
        view_model.permission_lines.iter().cloned(),
        inner.width as usize,
        theme,
    );
    push_info_section(
        &mut lines,
        "agents",
        palette.accent_success,
        view_model.agent_lines.iter().cloned(),
        inner.width as usize,
        theme,
    );
    if !view_model.task_lines.is_empty() {
        push_info_section(
            &mut lines,
            "task",
            palette.accent_secondary,
            view_model.task_lines.iter().cloned(),
            inner.width as usize,
            theme,
        );
    }
    if !view_model.mcp_lines.is_empty() {
        push_info_section(
            &mut lines,
            "MCP",
            palette.accent_warning,
            view_model.mcp_lines.iter().cloned(),
            inner.width as usize,
            theme,
        );
    }
    push_info_section(
        &mut lines,
        "LSP",
        palette.accent_secondary,
        view_model.code_lines.iter().cloned(),
        inner.width as usize,
        theme,
    );
    push_info_section(
        &mut lines,
        "usage",
        palette.accent_info,
        view_model.usage_lines.iter().cloned(),
        inner.width as usize,
        theme,
    );
    push_info_section(
        &mut lines,
        "controls",
        palette.accent_danger,
        view_model.controls.iter().cloned(),
        inner.width as usize,
        theme,
    );

    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .style(Style::default().bg(palette.surface_rail))
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
    theme: &Theme,
) where
    I: IntoIterator<Item = String>,
{
    lines.push(Line::from(vec![section_badge_with_palette(
        title,
        accent,
        &theme.palette,
    )]));
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
