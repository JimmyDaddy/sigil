use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Clear, Paragraph, Wrap},
};

use crate::app::AppState;

use super::{
    geometry::{selector_window_range, shadow_rect},
    layout_snapshot::slash_selector_overlay_rect,
    theme::Theme,
};

#[cfg(test)]
pub(crate) fn render_slash_selector_overlay(
    frame: &mut Frame,
    live_area: Rect,
    composer_area: Rect,
    app: &AppState,
) {
    let theme = Theme::default();
    render_slash_selector_overlay_with_theme(frame, live_area, composer_area, app, &theme);
}

pub(crate) fn render_slash_selector_overlay_with_theme(
    frame: &mut Frame,
    live_area: Rect,
    composer_area: Rect,
    app: &AppState,
    theme: &Theme,
) {
    if !app.has_slash_selector() || live_area.width == 0 || live_area.height == 0 {
        return;
    }
    let palette = &theme.palette;

    let selector_rows = app.slash_selector_rows();
    let visible_rows = app.slash_selector_visible_rows() as usize;
    if visible_rows == 0 {
        return;
    }

    let Some(overlay) = slash_selector_overlay_rect(live_area, composer_area, visible_rows) else {
        return;
    };
    frame.render_widget(Clear, overlay);
    let shadow = shadow_rect(overlay, frame.area());
    frame.render_widget(
        Block::default().style(Style::default().bg(palette.overlay_shadow)),
        shadow,
    );
    frame.render_widget(
        Block::default().style(Style::default().bg(palette.overlay_bg)),
        overlay,
    );

    let accent = palette.selection_bg;
    let gutter = Rect::new(overlay.x, overlay.y, 1, overlay.height);
    frame.render_widget(
        Paragraph::new(Text::from(
            (0..gutter.height)
                .map(|_| {
                    Line::from(vec![Span::styled(
                        "▌",
                        Style::default().fg(accent).bg(palette.overlay_bg),
                    )])
                })
                .collect::<Vec<_>>(),
        ))
        .style(Style::default().bg(palette.overlay_bg))
        .wrap(Wrap { trim: false }),
        gutter,
    );

    let content = Rect::new(
        overlay.x.saturating_add(2),
        overlay.y,
        overlay.width.saturating_sub(4),
        overlay.height,
    );
    if content.width == 0 || content.height == 0 {
        return;
    }

    let mut lines = app
        .slash_selector_title()
        .map(|title| selector_title_line_with_theme(title, theme))
        .into_iter()
        .collect::<Vec<_>>();
    let row_capacity = (content.height as usize).saturating_sub(lines.len());
    if row_capacity == 0 {
        // The title is more useful than a clipped first row when the terminal is tiny.
    } else if selector_rows.is_empty() {
        lines.push(Line::styled(
            app.slash_selector_empty_message()
                .unwrap_or("no slash match"),
            Style::default()
                .fg(palette.accent_danger)
                .bg(palette.overlay_bg),
        ));
    } else {
        let selected_index = app.slash_selector_selected_index().unwrap_or(0);
        let (window_start, window_end) =
            selector_window_range(selector_rows.len(), selected_index, row_capacity);
        lines.extend(
            selector_rows
                .into_iter()
                .enumerate()
                .skip(window_start)
                .take(window_end.saturating_sub(window_start))
                .map(|(index, (command, description))| {
                    let selected = index == selected_index;
                    let marker = if selected { "› " } else { "  " };
                    let style = if selected {
                        Style::default()
                            .fg(palette.selection_fg)
                            .bg(palette.selection_bg)
                    } else {
                        Style::default()
                            .fg(palette.accent_info)
                            .bg(palette.overlay_bg)
                    };
                    Line::from(vec![
                        Span::styled(marker, style.add_modifier(Modifier::BOLD)),
                        Span::styled(format!("{command:<12}"), style.add_modifier(Modifier::BOLD)),
                        Span::styled(
                            description,
                            if selected {
                                style
                            } else {
                                Style::default()
                                    .fg(palette.text_secondary)
                                    .bg(palette.overlay_bg)
                            },
                        ),
                    ])
                })
                .collect::<Vec<_>>(),
        );
    };

    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .style(Style::default().bg(palette.overlay_bg))
            .wrap(Wrap { trim: false }),
        content,
    );
}

fn selector_title_line_with_theme(title: &str, theme: &Theme) -> Line<'static> {
    let palette = &theme.palette;
    Line::from(vec![
        Span::styled(
            title.to_owned(),
            Style::default()
                .fg(palette.selection_bg)
                .bg(palette.overlay_bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "  Enter restore · Up/Down choose",
            Style::default()
                .fg(palette.text_secondary)
                .bg(palette.overlay_bg),
        ),
    ])
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/slash_overlay_tests.rs"]
mod tests;
