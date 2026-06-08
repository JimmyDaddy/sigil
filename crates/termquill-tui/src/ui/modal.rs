use ratatui::{
    Frame,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap},
};

use crate::app::AppState;

use super::{
    geometry::{centered_rect, halo_rect, shadow_rect},
    text::wrapped_line_rows,
};

pub(super) fn render_modal(frame: &mut Frame, app: &AppState) {
    if !app.has_modal() {
        return;
    }

    let visual = modal_visual(app);
    let raw_lines = app.modal_lines();
    let title = app.modal_title().unwrap_or("Modal");
    let max_inner_width = frame.area().width.saturating_sub(8).max(24) as usize;
    let desired_inner_width = raw_lines
        .iter()
        .map(|line| line.chars().count())
        .chain(std::iter::once(title.chars().count()))
        .max()
        .unwrap_or(24)
        .saturating_add(2)
        .clamp(24, max_inner_width);
    let body_height = raw_lines
        .iter()
        .map(|line| wrapped_line_rows(line, desired_inner_width))
        .sum::<usize>()
        .max(4) as u16
        + 2;
    let area = centered_rect(
        desired_inner_width as u16 + 2,
        body_height.min(frame.area().height.saturating_sub(2)),
        frame.area(),
    );
    let lines = raw_lines
        .iter()
        .cloned()
        .map(|line| render_modal_line(line, visual.accent))
        .collect::<Vec<_>>();

    let backdrop = halo_rect(area, frame.area(), 4, 1);
    if backdrop.width > 0 && backdrop.height > 0 {
        frame.render_widget(Clear, backdrop);
        frame.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(visual.backdrop_border))
                .style(Style::default().bg(visual.backdrop_bg)),
            backdrop,
        );
    }
    let shadow = shadow_rect(area, frame.area());
    if shadow.width > 0 && shadow.height > 0 {
        frame.render_widget(
            Block::default().style(Style::default().bg(visual.shadow_bg)),
            shadow,
        );
    }
    frame.render_widget(Clear, area);
    let widget = Paragraph::new(Text::from(lines))
        .style(Style::default().bg(visual.modal_bg))
        .block(
            Block::default()
                .title(title)
                .title_style(
                    Style::default()
                        .fg(Color::Black)
                        .bg(visual.accent)
                        .add_modifier(Modifier::BOLD),
                )
                .border_type(BorderType::Rounded)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(visual.accent))
                .style(Style::default().bg(visual.modal_bg)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(widget, area);

    if let Some((label, offset, line_index)) = app.modal_input_cursor() {
        let rows_before = raw_lines
            .iter()
            .take(line_index)
            .map(|line| wrapped_line_rows(line, desired_inner_width))
            .sum::<usize>() as u16;
        let max_offset = desired_inner_width.saturating_sub(label.chars().count() + 2);
        let cursor_x = area
            .x
            .saturating_add(1 + format!("{label}: ").len() as u16 + offset.min(max_offset) as u16);
        let cursor_y = area.y.saturating_add(1 + rows_before);
        frame.set_cursor_position((cursor_x, cursor_y));
    }
}

fn render_modal_line(line: String, accent: Color) -> Line<'static> {
    if line.is_empty() {
        return Line::raw(String::new());
    }
    if line.starts_with("> ") {
        return Line::styled(
            line,
            Style::default()
                .fg(Color::Black)
                .bg(accent)
                .add_modifier(Modifier::BOLD),
        );
    }
    if line.starts_with("Up/Down ") || line.starts_with("Enter apply") {
        return Line::styled(line, Style::default().fg(accent));
    }
    if let Some((label, value)) = line.split_once(':') {
        return Line::from(vec![
            Span::styled(label.to_owned(), Style::default().fg(accent)),
            Span::styled(": ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                value.trim_start().to_owned(),
                Style::default().fg(Color::White),
            ),
        ]);
    }
    Line::styled(line, Style::default().fg(Color::White))
}

struct ModalVisual {
    accent: Color,
    modal_bg: Color,
    backdrop_bg: Color,
    backdrop_border: Color,
    shadow_bg: Color,
}

fn modal_visual(app: &AppState) -> ModalVisual {
    match app.modal_title() {
        Some("API Key") => ModalVisual {
            accent: Color::Yellow,
            modal_bg: Color::Rgb(28, 26, 18),
            backdrop_bg: Color::Rgb(17, 16, 12),
            backdrop_border: Color::Rgb(90, 82, 30),
            shadow_bg: Color::Rgb(8, 8, 6),
        },
        Some("Model") | Some("FIM Model") | Some("Model ID") => ModalVisual {
            accent: Color::Cyan,
            modal_bg: Color::Rgb(18, 24, 30),
            backdrop_bg: Color::Rgb(12, 18, 22),
            backdrop_border: Color::Rgb(38, 84, 92),
            shadow_bg: Color::Rgb(6, 10, 12),
        },
        _ => ModalVisual {
            accent: Color::Green,
            modal_bg: Color::Rgb(19, 26, 22),
            backdrop_bg: Color::Rgb(12, 18, 15),
            backdrop_border: Color::Rgb(42, 88, 58),
            shadow_bg: Color::Rgb(6, 10, 8),
        },
    }
}
