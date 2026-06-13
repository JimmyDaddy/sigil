use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Paragraph, Wrap},
};

use crate::view_model::ComposerViewModel;

use super::{
    geometry::inset_rect,
    text::{pad_display_width, wrap_composer_input},
    theme::{accent_gold, composer_bg, composer_input_bg, ink, muted, phase_accent},
};

const COMPOSER_HORIZONTAL_INSET: u16 = 3;
const COMPOSER_VERTICAL_INSET: u16 = 1;
const COMPOSER_HEADER_HEIGHT: u16 = 1;
const COMPOSER_HEADER_INPUT_GAP: u16 = 1;

pub(crate) fn render_input(frame: &mut Frame, area: Rect, view_model: &ComposerViewModel) {
    let accent = phase_accent(&view_model.phase);
    frame.render_widget(
        Block::default().style(Style::default().bg(composer_bg())),
        area,
    );
    render_composer_gutter(frame, area, accent);

    let inner = inset_rect(area, COMPOSER_HORIZONTAL_INSET, COMPOSER_VERTICAL_INSET);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let input_height = view_model.input_rows.min(
        inner
            .height
            .saturating_sub(COMPOSER_HEADER_HEIGHT + COMPOSER_HEADER_INPUT_GAP)
            .max(1),
    );
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(COMPOSER_HEADER_HEIGHT),
            Constraint::Length(COMPOSER_HEADER_INPUT_GAP),
            Constraint::Min(input_height),
        ])
        .split(inner);
    let header_area = layout[0];
    let input_area = layout[2];

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

pub(crate) fn composer_input_area(area: Rect, input_rows: u16) -> Rect {
    let inner = inset_rect(area, COMPOSER_HORIZONTAL_INSET, COMPOSER_VERTICAL_INSET);
    if inner.width == 0 || inner.height == 0 {
        return Rect::default();
    }
    let input_height = input_rows.min(
        inner
            .height
            .saturating_sub(COMPOSER_HEADER_HEIGHT + COMPOSER_HEADER_INPUT_GAP)
            .max(1),
    );
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(COMPOSER_HEADER_HEIGHT),
            Constraint::Length(COMPOSER_HEADER_INPUT_GAP),
            Constraint::Min(input_height),
        ])
        .split(inner);
    layout[2]
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

#[cfg(test)]
#[path = "tests/composer_tests.rs"]
mod tests;
