use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};

use super::{ModalVisual, centered_rect, shadow_rect};
use crate::ui::text::{terminal_cell_width, wrap_terminal_lines};
use crate::{commands::KeyboardHelpPage, surface::KeyboardHelpSurface};

const FOOTER: &str = "Tab/Shift-Tab sections · ↑/↓ scroll · PgUp/Dn page · Esc/Enter close";

struct HelpGeometry {
    area: Rect,
    tabs: Rect,
    body: Rect,
    footer: Rect,
}

pub(crate) fn keyboard_help_scroll_bounds(
    width: u16,
    height: u16,
    lines: &[String],
) -> (usize, usize) {
    let geometry = help_geometry(Rect::new(0, 0, width, height), lines);
    let rows = help_rows(lines, geometry.body.width as usize, None);
    (
        rows.len().saturating_sub(geometry.body.height as usize),
        geometry.body.height as usize,
    )
}

fn help_geometry(screen: Rect, lines: &[String]) -> HelpGeometry {
    let max_width = screen.width.saturating_sub(8).max(1) as usize;
    let width = lines
        .iter()
        .map(|line| terminal_cell_width(line))
        .chain(std::iter::once(terminal_cell_width(FOOTER)))
        .max()
        .unwrap_or(24)
        .saturating_add(2)
        .clamp(24.min(max_width), max_width);
    let body_rows = help_rows(lines, width, None).len();
    let footer_rows = wrap_terminal_lines(vec![Line::raw(FOOTER)], width).len();
    let desired_height = body_rows.saturating_add(footer_rows).saturating_add(3);
    let area = centered_rect(
        width.saturating_add(2).min(u16::MAX as usize) as u16,
        desired_height.min(screen.height.saturating_sub(2) as usize) as u16,
        screen,
    );
    let inner = Block::default().borders(Borders::ALL).inner(area);
    let tabs_height = inner.height.min(1);
    let remaining = inner.height.saturating_sub(tabs_height);
    let footer_height = footer_rows.min(remaining.saturating_sub(1) as usize) as u16;
    let body_height = remaining.saturating_sub(footer_height);
    HelpGeometry {
        area,
        tabs: Rect::new(inner.x, inner.y, inner.width, tabs_height),
        body: Rect::new(
            inner.x,
            inner.y.saturating_add(tabs_height),
            inner.width,
            body_height,
        ),
        footer: Rect::new(
            inner.x,
            inner
                .y
                .saturating_add(tabs_height)
                .saturating_add(body_height),
            inner.width,
            footer_height,
        ),
    }
}

fn help_rows(lines: &[String], width: usize, visual: Option<&ModalVisual>) -> Vec<Line<'static>> {
    let key_style = visual.map_or_else(Style::default, |visual| {
        Style::default()
            .fg(visual.label)
            .add_modifier(Modifier::BOLD)
    });
    let text_style = visual.map_or_else(Style::default, |visual| Style::default().fg(visual.text));
    let muted_style =
        visual.map_or_else(Style::default, |visual| Style::default().fg(visual.muted));
    let lines = lines
        .iter()
        .enumerate()
        .map(|(index, line)| {
            if let Some((key, description)) = line.split_once(':') {
                Line::from(vec![
                    Span::styled(key.to_owned(), key_style),
                    Span::styled(": ", muted_style),
                    Span::styled(description.trim_start().to_owned(), text_style),
                ])
            } else {
                Line::styled(
                    line.clone(),
                    if index == 0 { key_style } else { muted_style },
                )
            }
        })
        .collect();
    // Count and draw these same visual rows; Paragraph must not wrap them a second time.
    wrap_terminal_lines(lines, width)
}

pub(super) fn render_keyboard_help(
    frame: &mut Frame,
    lines: &[String],
    help: &KeyboardHelpSurface,
    visual: &ModalVisual,
) {
    let geometry = help_geometry(frame.area(), lines);
    let area = geometry.area;
    frame.render_widget(
        Block::default().style(Style::default().bg(visual.shadow_bg)),
        shadow_rect(area, frame.area()),
    );
    frame.render_widget(Clear, area);
    frame.render_widget(
        Block::default()
            .title("Keyboard Help")
            .title_style(
                Style::default()
                    .fg(visual.title_fg)
                    .bg(visual.accent)
                    .add_modifier(Modifier::BOLD),
            )
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(visual.border))
            .style(Style::default().bg(visual.modal_bg)),
        area,
    );
    let selected = Style::default()
        .fg(visual.label)
        .add_modifier(Modifier::BOLD);
    let other = Style::default().fg(visual.muted);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                if help.page == KeyboardHelpPage::Common {
                    "[Common]"
                } else {
                    " Common "
                },
                if help.page == KeyboardHelpPage::Common {
                    selected
                } else {
                    other
                },
            ),
            Span::raw("  "),
            Span::styled(
                if help.page == KeyboardHelpPage::All {
                    "[All shortcuts]"
                } else {
                    " All shortcuts "
                },
                if help.page == KeyboardHelpPage::All {
                    selected
                } else {
                    other
                },
            ),
        ])),
        geometry.tabs,
    );
    let rows = help_rows(lines, geometry.body.width as usize, Some(visual));
    let scroll = help
        .scroll_rows
        .min(rows.len().saturating_sub(geometry.body.height as usize));
    frame.render_widget(
        Paragraph::new(Text::from(rows)).scroll((scroll.min(u16::MAX as usize) as u16, 0)),
        geometry.body,
    );
    frame.render_widget(
        Paragraph::new(Text::from(wrap_terminal_lines(
            vec![Line::styled(FOOTER, other)],
            geometry.footer.width as usize,
        ))),
        geometry.footer,
    );
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "../tests/keyboard_help_tests.rs"]
mod tests;
