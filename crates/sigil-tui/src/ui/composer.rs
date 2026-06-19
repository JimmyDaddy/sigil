use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Paragraph, Wrap},
};

use crate::view_model::ComposerViewModel;

use super::{
    geometry::inset_rect,
    status_indicator::{FocusKind, StatusIndicator, focus_style},
    text::{pad_display_width, truncate_display_width, wrap_composer_input},
    theme::{
        accent_blue, accent_gold, agent_panel_bg, composer_bg, composer_input_bg, dim, dock_edge,
        ink, muted, phase_accent,
    },
};

const COMPOSER_HORIZONTAL_INSET: u16 = 3;
const COMPOSER_VERTICAL_INSET: u16 = 1;
const COMPOSER_HEADER_HEIGHT: u16 = 1;
const COMPOSER_HEADER_INPUT_GAP: u16 = 1;
const COMPOSER_AGENT_LABEL_WIDTH: usize = 22;

pub(crate) fn render_input(frame: &mut Frame, area: Rect, view_model: &ComposerViewModel) {
    let accent = phase_accent(&view_model.phase);
    frame.render_widget(
        Block::default().style(Style::default().bg(composer_bg())),
        area,
    );
    render_panel_separator(frame, area, composer_bg());
    render_composer_gutter(frame, area, accent);

    let inner = inset_rect(area, COMPOSER_HORIZONTAL_INSET, COMPOSER_VERTICAL_INSET);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let header_area = Rect::new(
        inner.x,
        inner.y,
        inner.width,
        COMPOSER_HEADER_HEIGHT.min(inner.height),
    );
    let input_area = composer_input_area(area, view_model.input_rows);

    let header = Line::from(vec![
        Span::styled(
            view_model.mode_label.clone(),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  ·  "),
        Span::styled(view_model.model_name.clone(), Style::default().fg(ink())),
        Span::raw("  ·  "),
        Span::styled(
            view_model.provider_name.clone(),
            Style::default().fg(muted()),
        ),
        Span::raw("  ·  "),
        Span::styled(
            view_model.reasoning_effort_label.clone(),
            Style::default().fg(accent_gold()),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(Text::from(vec![header]))
            .style(Style::default().bg(composer_bg()))
            .wrap(Wrap { trim: false }),
        header_area,
    );

    let input_bg = composer_input_bg();
    frame.render_widget(
        Block::default().style(Style::default().bg(input_bg)),
        input_area,
    );
    let input_inner = inset_rect(input_area, 0, 0);
    if input_inner.width > 0 && input_inner.height > 0 {
        let input_width = input_inner.width as usize;
        let cursor_row = view_model.cursor_position.1 as usize;
        let visible_rows = input_inner.height as usize;
        let row_offset = cursor_row.saturating_sub(visible_rows.saturating_sub(1));
        let wrapped_rows = wrap_composer_input(&view_model.input, input_width);
        let mut lines = wrapped_rows
            .into_iter()
            .skip(row_offset)
            .take(visible_rows)
            .map(|row| {
                Line::from(vec![Span::styled(
                    pad_display_width(&row, input_width),
                    Style::default().fg(ink()).bg(input_bg),
                )])
            })
            .collect::<Vec<_>>();
        while lines.len() < visible_rows {
            lines.push(Line::from(vec![Span::styled(
                " ".repeat(input_width),
                Style::default().bg(input_bg),
            )]));
        }
        frame.render_widget(
            Paragraph::new(Text::from(lines))
                .style(Style::default().bg(input_bg))
                .wrap(Wrap { trim: false }),
            input_inner,
        );
    }
}

pub(crate) fn composer_cursor_origin(
    area: Rect,
    view_model: &ComposerViewModel,
) -> Option<(u16, u16)> {
    let input_area = composer_input_area(area, view_model.input_rows);
    if input_area.width == 0 || input_area.height == 0 {
        return None;
    }
    let cursor_row = view_model.cursor_position.1;
    let row_offset = cursor_row.saturating_sub(input_area.height.saturating_sub(1));
    Some((
        input_area.x,
        input_area
            .y
            .saturating_add(cursor_row.saturating_sub(row_offset)),
    ))
}

pub(crate) fn composer_input_area(area: Rect, _input_rows: u16) -> Rect {
    let inner = inset_rect(area, COMPOSER_HORIZONTAL_INSET, COMPOSER_VERTICAL_INSET);
    if inner.width == 0 || inner.height == 0 {
        return Rect::default();
    }
    let header_rows = COMPOSER_HEADER_HEIGHT.saturating_add(COMPOSER_HEADER_INPUT_GAP);
    if inner.height <= header_rows {
        return Rect::default();
    }

    let input_height = inner.height.saturating_sub(header_rows).max(1);
    Rect::new(
        inner.x,
        inner.y.saturating_add(header_rows),
        inner.width,
        input_height,
    )
}

pub(crate) fn render_agent_panel(frame: &mut Frame, area: Rect, view_model: &ComposerViewModel) {
    if area.width == 0 || area.height == 0 || view_model.agent_rows.len() <= 1 {
        return;
    }
    frame.render_widget(
        Block::default().style(Style::default().bg(agent_panel_bg())),
        area,
    );
    render_panel_separator(frame, area, agent_panel_bg());
    let content = Rect::new(
        area.x.saturating_add(COMPOSER_HORIZONTAL_INSET),
        area.y.saturating_add(1),
        area.width.saturating_sub(COMPOSER_HORIZONTAL_INSET),
        area.height.saturating_sub(1),
    );
    if content.width == 0 || content.height == 0 {
        return;
    }
    let width = content.width as usize;
    let lines = view_model
        .agent_rows
        .iter()
        .take(content.height as usize)
        .map(|row| render_agent_row(row, width, view_model.agent_panel_focused))
        .collect::<Vec<_>>();

    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .style(Style::default().bg(agent_panel_bg()))
            .wrap(Wrap { trim: false }),
        content,
    );
}

fn render_agent_row(
    row: &crate::timeline::SidebarAgentRow,
    width: usize,
    panel_focused: bool,
) -> Line<'static> {
    let selected = panel_focused && row.selected;
    let focus = row.focus_symbol(panel_focused);
    let label = row.label.strip_prefix("agent ").unwrap_or(&row.label);
    let detail = row.compact_detail();
    let label_text = pad_display_width(
        &truncate_display_width(label, COMPOSER_AGENT_LABEL_WIDTH),
        COMPOSER_AGENT_LABEL_WIDTH,
    );
    let status = StatusIndicator::animated(row.status_kind());
    let reserved_width = 1 + 1 + COMPOSER_AGENT_LABEL_WIDTH + 1 + 1 + 1;
    let detail_text = truncate_display_width(&detail, width.saturating_sub(reserved_width));
    let style = if selected {
        Style::default().fg(Color::Black).bg(accent_gold())
    } else if row.active {
        Style::default()
            .fg(accent_blue())
            .bg(agent_panel_bg())
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(accent_blue()).bg(agent_panel_bg())
    };
    if selected {
        return Line::from(vec![Span::styled(
            truncate_display_width(
                &format!("{focus} {label_text} {} {detail_text}", status.symbol()),
                width,
            ),
            style,
        )]);
    }

    Line::from(vec![
        Span::styled(
            focus.to_owned(),
            focus_style(if row.active {
                FocusKind::Current
            } else if panel_focused && row.selected {
                FocusKind::Selected
            } else {
                FocusKind::None
            }),
        ),
        Span::raw(" "),
        Span::styled(label_text, style),
        Span::raw(" "),
        status.span(),
        Span::raw(" "),
        Span::styled(
            detail_text,
            Style::default()
                .fg(if row.muted { dim() } else { muted() })
                .bg(agent_panel_bg()),
        ),
    ])
}

fn render_composer_gutter(frame: &mut Frame, area: Rect, accent: Color) {
    let gutter = Rect::new(area.x.saturating_add(1), area.y, 1, area.height);
    if gutter.width == 0 || gutter.height == 0 {
        return;
    }
    let lines = (0..gutter.height)
        .map(|_| {
            Line::from(vec![Span::styled(
                "▌",
                Style::default().fg(accent).bg(composer_bg()),
            )])
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .style(Style::default().bg(composer_bg()))
            .wrap(Wrap { trim: false }),
        gutter,
    );
}

fn render_panel_separator(frame: &mut Frame, area: Rect, bg: Color) {
    if area.width == 0 {
        return;
    }
    let line = Line::from(vec![Span::styled(
        "─".repeat(area.width as usize),
        Style::default().fg(dock_edge()).bg(bg),
    )]);
    frame.render_widget(
        Paragraph::new(Text::from(vec![line])).style(Style::default().bg(bg)),
        Rect::new(area.x, area.y, area.width, 1),
    );
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/composer_tests.rs"]
mod tests;
