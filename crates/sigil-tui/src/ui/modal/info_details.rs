use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};

use super::{ModalVisual, text_rows::word_wrap_rows};
use crate::{surface::InfoDetailsSurface, ui::text::sanitize_terminal_text};

const FOOTER: &str = "↑/↓ scroll · PgUp/Dn page · Home/End · Esc/Enter close";

struct DetailsGeometry {
    area: Rect,
    body: Rect,
    footer: Rect,
}

pub(crate) fn info_details_scroll_bounds(
    width: u16,
    height: u16,
    lines: &[String],
) -> (usize, usize) {
    let geometry = details_geometry(Rect::new(0, 0, width, height), lines);
    let rows = details_rows(lines, geometry.body.width as usize, None);
    (
        rows.len().saturating_sub(geometry.body.height as usize),
        geometry.body.height as usize,
    )
}

fn details_geometry(screen: Rect, lines: &[String]) -> DetailsGeometry {
    let width = screen
        .width
        .saturating_sub(4)
        .max(screen.width.min(24))
        .min(112);
    let inner_width = width.saturating_sub(2) as usize;
    let body_rows = details_rows(lines, inner_width, None).len();
    let footer_rows = word_wrap_rows(vec![Line::raw(FOOTER)], inner_width).len();
    let height = body_rows
        .saturating_add(footer_rows)
        .saturating_add(2)
        .min(screen.height.saturating_sub(2).max(screen.height.min(3)) as usize)
        as u16;
    let area = Rect::new(
        screen
            .x
            .saturating_add(screen.width.saturating_sub(width) / 2),
        screen
            .y
            .saturating_add(screen.height.saturating_sub(height) / 2),
        width,
        height,
    );
    let inner = Block::default().borders(Borders::ALL).inner(area);
    let footer_height = footer_rows.min(inner.height.saturating_sub(1) as usize) as u16;
    let body_height = inner.height.saturating_sub(footer_height);
    DetailsGeometry {
        area,
        body: Rect::new(inner.x, inner.y, inner.width, body_height),
        footer: Rect::new(
            inner.x,
            inner.y.saturating_add(body_height),
            inner.width,
            footer_height,
        ),
    }
}

fn details_rows(
    lines: &[String],
    width: usize,
    visual: Option<&ModalVisual>,
) -> Vec<Line<'static>> {
    let text = visual.map_or_else(Style::default, |visual| Style::default().fg(visual.text));
    let label = visual.map_or_else(Style::default, |visual| {
        Style::default()
            .fg(visual.muted)
            .add_modifier(Modifier::BOLD)
    });
    let rows = lines
        .iter()
        .enumerate()
        .map(|(index, line)| {
            let line = line
                .split('\n')
                .map(sanitize_terminal_text)
                .collect::<Vec<_>>()
                .join("\n");
            if let Some((key, value)) = line.split_once(':') {
                Line::from(vec![
                    Span::styled(format!("{key}:"), label),
                    Span::styled(value.to_owned(), text),
                ])
            } else {
                let style = if index == 0 || lines.get(index - 1).is_some_and(String::is_empty) {
                    text.add_modifier(Modifier::BOLD)
                } else {
                    text
                };
                Line::from(Span::styled(line, style))
            }
        })
        .collect();
    // Scroll bounds and pixels consume these same rows without Paragraph wrapping again.
    word_wrap_rows(rows, width)
}

pub(super) fn render_info_details(
    frame: &mut Frame,
    lines: &[String],
    details: &InfoDetailsSurface,
    visual: &ModalVisual,
) {
    let geometry = details_geometry(frame.area(), lines);
    frame.render_widget(Clear, geometry.area);
    frame.render_widget(
        Block::default()
            .title(" Details ")
            .title_style(
                Style::default()
                    .fg(visual.text)
                    .add_modifier(Modifier::BOLD),
            )
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(visual.muted))
            .style(Style::default().bg(visual.modal_bg)),
        geometry.area,
    );
    let rows = details_rows(lines, geometry.body.width as usize, Some(visual));
    let scroll = details
        .scroll_rows
        .min(rows.len().saturating_sub(geometry.body.height as usize));
    frame.render_widget(
        Paragraph::new(Text::from(
            rows.into_iter()
                .skip(scroll)
                .take(geometry.body.height as usize)
                .collect::<Vec<_>>(),
        )),
        geometry.body,
    );
    frame.render_widget(
        Paragraph::new(Text::from(word_wrap_rows(
            vec![Line::styled(FOOTER, Style::default().fg(visual.muted))],
            geometry.footer.width as usize,
        ))),
        geometry.footer,
    );
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "../tests/info_details_tests.rs"]
mod tests;
