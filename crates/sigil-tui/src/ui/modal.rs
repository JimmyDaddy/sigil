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
    theme,
};

pub(super) fn render_modal(frame: &mut Frame, app: &AppState) {
    if !app.has_modal() || app.checkpoint_restore_modal_open() {
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
        .enumerate()
        .map(|(index, line)| render_modal_line(index, line, &visual))
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
    let block = Block::default()
        .title(title)
        .title_style(
            Style::default()
                .fg(visual.title_fg)
                .bg(visual.accent)
                .add_modifier(Modifier::BOLD),
        )
        .border_type(BorderType::Rounded)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(visual.border))
        .style(Style::default().bg(visual.modal_bg));
    let content_area = block.inner(area);
    frame.render_widget(block, area);
    render_modal_focus_row_bgs(
        frame,
        content_area,
        &raw_lines,
        desired_inner_width,
        &visual,
    );

    let widget = Paragraph::new(Text::from(lines))
        .style(Style::default().bg(visual.modal_bg))
        .wrap(Wrap { trim: false });
    frame.render_widget(widget, content_area);

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

fn render_modal_focus_row_bgs(
    frame: &mut Frame,
    content_area: ratatui::layout::Rect,
    raw_lines: &[String],
    desired_inner_width: usize,
    visual: &ModalVisual,
) {
    let mut row_offset = 0u16;
    for line in raw_lines {
        let row_height = wrapped_line_rows(line, desired_inner_width)
            .max(1)
            .min(u16::MAX as usize) as u16;
        if modal_line_is_focus(line) && row_offset < content_area.height {
            let height = row_height.min(content_area.height - row_offset);
            frame.render_widget(
                Block::default().style(Style::default().bg(visual.selected_bg)),
                ratatui::layout::Rect {
                    y: content_area.y + row_offset,
                    height,
                    ..content_area
                },
            );
        }
        row_offset = row_offset.saturating_add(row_height);
    }
}

fn modal_line_is_focus(line: &str) -> bool {
    line.starts_with("> ") || line_is_input_value(line)
}

fn line_is_input_value(line: &str) -> bool {
    line.ends_with('|')
        && line
            .split_once(':')
            .is_some_and(|(label, _)| label != "key")
}

fn render_modal_line(index: usize, line: String, visual: &ModalVisual) -> Line<'static> {
    if line.is_empty() {
        return Line::raw(String::new());
    }
    if let Some(rest) = line.strip_prefix("> ") {
        return Line::from(vec![
            Span::styled(
                "> ",
                Style::default()
                    .fg(visual.accent)
                    .bg(visual.selected_bg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                rest.to_owned(),
                Style::default()
                    .fg(visual.text)
                    .bg(visual.selected_bg)
                    .add_modifier(Modifier::BOLD),
            ),
        ])
        .style(Style::default().bg(visual.selected_bg));
    }
    if line.starts_with("Up/Down ") || line.starts_with("Enter apply") {
        return render_modal_command_line(&line, visual);
    }
    if let Some((label, value)) = line.split_once(':') {
        if line_is_input_value(&line) {
            return render_modal_input_line(label, value.trim_start(), visual);
        }
        if label == "key" {
            return render_modal_metadata_line(label, value.trim_start(), visual);
        }
        return Line::from(vec![
            Span::styled(
                label.to_owned(),
                Style::default()
                    .fg(visual.label)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(": ", Style::default().fg(visual.muted)),
            Span::styled(
                value.trim_start().to_owned(),
                Style::default().fg(visual.text),
            ),
        ]);
    }
    if index == 0 {
        return Line::styled(
            line,
            Style::default()
                .fg(visual.text)
                .add_modifier(Modifier::BOLD),
        );
    }
    Line::styled(line, Style::default().fg(visual.muted))
}

fn render_modal_command_line(line: &str, visual: &ModalVisual) -> Line<'static> {
    let mut spans = Vec::new();
    for (index, token) in line
        .split("  ")
        .filter(|token| !token.is_empty())
        .enumerate()
    {
        if index > 0 {
            spans.push(Span::raw("  "));
        }
        push_modal_command_token(&mut spans, token, visual);
    }
    Line::from(spans)
}

fn render_modal_metadata_line(label: &str, value: &str, visual: &ModalVisual) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            "meta ",
            Style::default()
                .fg(visual.muted)
                .bg(visual.command_bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("{label}: "), Style::default().fg(visual.muted)),
        Span::styled(value.to_owned(), Style::default().fg(visual.muted)),
    ])
}

fn push_modal_command_token(spans: &mut Vec<Span<'static>>, token: &str, visual: &ModalVisual) {
    let (key, suffix) = token.split_once(' ').unwrap_or((token, ""));
    spans.push(Span::styled(
        key.to_owned(),
        Style::default()
            .fg(visual.hint)
            .bg(visual.command_bg)
            .add_modifier(Modifier::BOLD),
    ));
    if !suffix.is_empty() {
        spans.push(Span::styled(
            format!(" {suffix}"),
            Style::default().fg(visual.muted),
        ));
    }
}

fn render_modal_input_line(label: &str, value: &str, visual: &ModalVisual) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            label.to_owned(),
            Style::default()
                .fg(visual.label)
                .bg(visual.selected_bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            ": ",
            Style::default().fg(visual.muted).bg(visual.selected_bg),
        ),
        Span::styled(
            value.to_owned(),
            Style::default().fg(visual.text).bg(visual.selected_bg),
        ),
    ])
    .style(Style::default().bg(visual.selected_bg))
}

struct ModalVisual {
    accent: Color,
    title_fg: Color,
    border: Color,
    label: Color,
    hint: Color,
    text: Color,
    muted: Color,
    selected_bg: Color,
    command_bg: Color,
    modal_bg: Color,
    backdrop_bg: Color,
    backdrop_border: Color,
    shadow_bg: Color,
}

fn modal_visual(app: &AppState) -> ModalVisual {
    let theme = theme::resolve_for_app(app);
    let palette = &theme.palette;
    if app.is_config_mode() {
        return ModalVisual {
            accent: palette.config_primary,
            title_fg: palette.button_selected_fg,
            border: palette.config_border,
            label: palette.config_detail,
            hint: palette.config_warning,
            text: palette.text_primary,
            muted: palette.text_secondary,
            selected_bg: palette.config_selected_bg,
            command_bg: palette.config_tab_bg,
            modal_bg: palette.config_bg,
            backdrop_bg: palette.surface_base,
            backdrop_border: palette.config_border,
            shadow_bg: palette.modal_shadow,
        };
    }

    match app.modal_title() {
        Some("API Key") => ModalVisual {
            accent: palette.accent_warning,
            title_fg: palette.button_selected_fg,
            border: palette.accent_warning,
            label: palette.accent_warning,
            hint: palette.accent_warning,
            text: palette.text_primary,
            muted: palette.text_secondary,
            selected_bg: palette.surface_selection,
            command_bg: palette.modal_command_bg,
            modal_bg: palette.modal_bg,
            backdrop_bg: palette.surface_base,
            backdrop_border: palette.accent_warning,
            shadow_bg: palette.modal_shadow,
        },
        Some("Model") | Some("FIM Model") | Some("Model ID") => ModalVisual {
            accent: palette.accent_info,
            title_fg: palette.button_selected_fg,
            border: palette.accent_info,
            label: palette.accent_info,
            hint: palette.accent_info,
            text: palette.text_primary,
            muted: palette.text_secondary,
            selected_bg: palette.surface_selection,
            command_bg: palette.modal_command_bg,
            modal_bg: palette.modal_bg,
            backdrop_bg: palette.surface_base,
            backdrop_border: palette.accent_info,
            shadow_bg: palette.modal_shadow,
        },
        _ => ModalVisual {
            accent: palette.accent_success,
            title_fg: palette.button_selected_fg,
            border: palette.accent_success,
            label: palette.accent_success,
            hint: palette.accent_success,
            text: palette.text_primary,
            muted: palette.text_secondary,
            selected_bg: palette.surface_selection,
            command_bg: palette.modal_command_bg,
            modal_bg: palette.modal_bg,
            backdrop_bg: palette.surface_base,
            backdrop_border: palette.accent_success,
            shadow_bg: palette.modal_shadow,
        },
    }
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/modal_tests.rs"]
mod tests;
