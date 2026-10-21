use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};

#[cfg(test)]
use crate::app::AppState;
use crate::app::{CheckpointRestoreModalPhase, CheckpointRestoreModalView};

use super::{
    diff::{
        diff_line_number_text, diff_line_number_width, diff_line_style_for_palette,
        number_unified_diff_lines,
    },
    geometry::{centered_rect, halo_rect, inset_rect, shadow_rect},
    text::wrap_terminal_lines,
    theme::{self, ThemePalette},
};

#[cfg(test)]
pub(super) fn render_checkpoint_restore_modal(frame: &mut Frame, app: &AppState) {
    let Some(view) = app.checkpoint_restore_modal_view() else {
        return;
    };
    let current_theme = theme::resolve_for_app(app);
    render_checkpoint_restore_modal_view(frame, &view, &current_theme);
}

pub(super) fn render_checkpoint_restore_modal_surface(
    frame: &mut Frame,
    view: &CheckpointRestoreModalView,
    theme: &theme::Theme,
) {
    render_checkpoint_restore_modal_view(frame, view, theme);
}

fn render_checkpoint_restore_modal_view(
    frame: &mut Frame,
    view: &CheckpointRestoreModalView,
    current_theme: &theme::Theme,
) {
    let palette = &current_theme.palette;
    let modal_layout = checkpoint_restore_layout(frame.area(), view);
    let area = modal_layout.area;
    let backdrop = halo_rect(area, frame.area(), 5, 2);
    if !compact_state(view) && backdrop.width > 0 && backdrop.height > 0 {
        frame.render_widget(Clear, backdrop);
        frame.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(palette.modal_border))
                .style(Style::default().bg(palette.surface_base)),
            backdrop,
        );
    }
    let shadow = shadow_rect(area, frame.area());
    if shadow.width > 0 && shadow.height > 0 {
        frame.render_widget(
            Block::default().style(Style::default().bg(palette.modal_shadow)),
            shadow,
        );
    }
    frame.render_widget(Clear, area);
    let block = Block::default()
        .title(" Restore Checkpoint ")
        .title_style(
            Style::default()
                .fg(palette.text_inverse)
                .bg(phase_color(view.phase, palette))
                .add_modifier(Modifier::BOLD),
        )
        .border_type(BorderType::Rounded)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(palette.modal_border))
        .style(Style::default().bg(palette.modal_bg));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    if compact_state(view) {
        let rows = compact_status_rows(view, inner.width as usize);
        let scroll = usize::from(view.scroll).min(
            rows.len()
                .saturating_sub(modal_layout.body_content.height as usize),
        );
        frame.render_widget(
            Paragraph::new(Text::from(
                rows.into_iter()
                    .skip(scroll)
                    .take(modal_layout.body_content.height as usize)
                    .collect::<Vec<_>>(),
            ))
            .style(Style::default().fg(if view.error.is_some() {
                palette.accent_danger
            } else {
                palette.text_secondary
            })),
            modal_layout.body_content,
        );
        frame.render_widget(
            Paragraph::new(Text::from(wrap_terminal_lines(
                vec![Line::raw(compact_footer(view))],
                modal_layout.actions.width as usize,
            )))
            .style(Style::default().fg(palette.text_secondary)),
            modal_layout.actions,
        );
        return;
    }

    frame.render_widget(
        Paragraph::new(Text::from(checkpoint_summary_lines(view, palette)))
            .block(
                Block::default()
                    .title("Restore target")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(palette.border_subtle)),
            )
            .style(Style::default().bg(palette.modal_bg)),
        modal_layout.target,
    );

    let body_block = Block::default()
        .title(view.body_title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(palette.border_subtle));
    frame.render_widget(body_block, modal_layout.body);
    if modal_layout.body_status.width > 0 && modal_layout.body_status.height > 0 {
        frame.render_widget(
            Paragraph::new(Line::styled(
                view.body_status.clone(),
                Style::default().fg(palette.text_muted),
            )),
            modal_layout.body_status,
        );
    }
    if modal_layout.body_content.width > 0 && modal_layout.body_content.height > 0 {
        let lines = checkpoint_body_lines(view, palette);
        let max_scroll =
            checkpoint_restore_max_scroll(frame.area().width, frame.area().height, view);
        let scroll = usize::from(view.scroll).min(max_scroll) as u16;
        frame.render_widget(
            Paragraph::new(Text::from(lines))
                .style(Style::default().bg(palette.modal_bg))
                .scroll((scroll, 0)),
            modal_layout.body_content,
        );
    }

    frame.render_widget(
        Paragraph::new(Text::from(checkpoint_action_lines(view, palette)))
            .block(
                Block::default()
                    .title("Actions")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(palette.border_subtle)),
            )
            .style(Style::default().bg(palette.modal_bg)),
        modal_layout.actions,
    );
}

#[derive(Debug, Clone, Copy)]
struct CheckpointRestoreLayout {
    area: Rect,
    target: Rect,
    body: Rect,
    body_status: Rect,
    body_content: Rect,
    actions: Rect,
}

fn checkpoint_restore_layout(
    screen: Rect,
    view: &CheckpointRestoreModalView,
) -> CheckpointRestoreLayout {
    if compact_state(view) {
        let width = screen
            .width
            .saturating_sub(8)
            .max(screen.width.min(24))
            .min(118);
        let inner_width = width.saturating_sub(2) as usize;
        let rows = compact_status_rows(view, inner_width).len();
        let footer_rows =
            wrap_terminal_lines(vec![Line::raw(compact_footer(view))], inner_width).len();
        let height = rows
            .saturating_add(footer_rows)
            .saturating_add(2)
            .min(screen.height.saturating_sub(2).max(screen.height.min(3)) as usize)
            as u16;
        let area = centered_rect(width, height, screen);
        let inner = inset_rect(area, 1, 1);
        let footer_height = (footer_rows as u16).min(inner.height.saturating_sub(1));
        return CheckpointRestoreLayout {
            area,
            target: Rect::default(),
            body: Rect::default(),
            body_status: Rect::default(),
            body_content: Rect::new(
                inner.x,
                inner.y,
                inner.width,
                inner.height.saturating_sub(footer_height),
            ),
            actions: Rect::new(
                inner.x,
                inner.bottom().saturating_sub(footer_height),
                inner.width,
                footer_height,
            ),
        };
    }
    let area = checkpoint_restore_modal_area(screen);
    let inner = inset_rect(area, 1, 1);
    let footer_rows = 4u16.min(inner.height);
    let summary_content_rows = view
        .summary_lines
        .len()
        .saturating_add(1)
        .saturating_add(usize::from(view.error.is_some()));
    let header_rows = (summary_content_rows.saturating_add(2).clamp(5, 9) as u16)
        .min(inner.height.saturating_sub(footer_rows).max(3));
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(header_rows),
            Constraint::Min(4),
            Constraint::Length(footer_rows),
        ])
        .split(inner);
    let target = sections[0];
    let body = sections[1];
    let actions = sections[2];
    let body_inner = inset_rect(body, 1, 1);
    let body_sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(body_inner);
    CheckpointRestoreLayout {
        area,
        target,
        body,
        body_status: body_sections[0],
        body_content: body_sections[1],
        actions,
    }
}

pub(crate) fn checkpoint_restore_max_scroll(
    terminal_width: u16,
    terminal_height: u16,
    view: &CheckpointRestoreModalView,
) -> usize {
    let layout = checkpoint_restore_layout(Rect::new(0, 0, terminal_width, terminal_height), view);
    let rows = if compact_state(view) {
        compact_status_rows(view, layout.body_content.width as usize).len()
    } else {
        checkpoint_body_line_count(view)
    };
    rows.saturating_sub(usize::from(layout.body_content.height).max(1))
}

fn compact_state(view: &CheckpointRestoreModalView) -> bool {
    matches!(
        view.phase,
        CheckpointRestoreModalPhase::Empty | CheckpointRestoreModalPhase::Unavailable
    )
}

fn compact_footer(view: &CheckpointRestoreModalView) -> &'static str {
    if view.phase == CheckpointRestoreModalPhase::Empty {
        "Ctrl-R refresh · Esc close"
    } else {
        "Ctrl-R refresh · PgUp/PgDn scroll · Home/End · Esc close"
    }
}

fn compact_status_rows(view: &CheckpointRestoreModalView, width: usize) -> Vec<Line<'static>> {
    let mut lines = if view.phase == CheckpointRestoreModalPhase::Empty {
        vec![
            Line::raw("No controlled checkpoint yet."),
            Line::default(),
            Line::raw("Controlled file changes appear here after a checkpoint is recorded."),
        ]
    } else {
        vec![
            Line::raw("Checkpoint unavailable"),
            Line::default(),
            Line::from(Span::raw(
                view.error
                    .clone()
                    .unwrap_or_else(|| view.phase_detail.clone()),
            )),
        ]
    };
    // A failed restore can still carry a real target or file evidence; keep those facts reachable.
    if view.phase == CheckpointRestoreModalPhase::Unavailable {
        lines.extend(
            view.summary_lines
                .iter()
                .cloned()
                .map(|line| Line::from(Span::raw(line))),
        );
        lines.extend(
            view.body_notice_lines
                .iter()
                .cloned()
                .map(|line| Line::from(Span::raw(line))),
        );
        if view.body_is_diff || !view.summary_lines.is_empty() || !view.body_notice_lines.is_empty()
        {
            lines.extend(
                view.body_lines
                    .iter()
                    .cloned()
                    .map(|line| Line::from(Span::raw(line))),
            );
        }
    }
    wrap_terminal_lines(lines, width)
}

fn checkpoint_restore_modal_area(screen: ratatui::layout::Rect) -> ratatui::layout::Rect {
    centered_rect(
        screen.width.saturating_sub(8).min(118),
        screen.height.saturating_sub(4).min(34),
        screen,
    )
}

fn checkpoint_summary_lines(
    view: &CheckpointRestoreModalView,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![
        phase_badge(view.phase, palette),
        Span::raw("  "),
        Span::styled(
            view.phase_detail.clone(),
            Style::default().fg(palette.text_primary),
        ),
    ])];
    if let Some(error) = &view.error {
        lines.push(Line::styled(
            format!("Error: {error}"),
            Style::default()
                .fg(palette.accent_danger)
                .add_modifier(Modifier::BOLD),
        ));
    }
    lines.extend(
        view.summary_lines
            .iter()
            .cloned()
            .map(|line| Line::styled(line, Style::default().fg(palette.text_secondary))),
    );
    lines
}

fn checkpoint_body_line_count(view: &CheckpointRestoreModalView) -> usize {
    view.body_notice_lines
        .len()
        .saturating_add(view.body_lines.len())
        .saturating_add(usize::from(
            !view.body_notice_lines.is_empty() && !view.body_lines.is_empty(),
        ))
}

fn checkpoint_body_lines(
    view: &CheckpointRestoreModalView,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    let mut lines = view
        .body_notice_lines
        .iter()
        .cloned()
        .map(|line| {
            Line::styled(
                line,
                Style::default()
                    .fg(palette.accent_danger)
                    .add_modifier(Modifier::BOLD),
            )
        })
        .collect::<Vec<_>>();
    if !lines.is_empty() && !view.body_lines.is_empty() {
        lines.push(Line::default());
    }
    if view.body_is_diff {
        lines.extend(checkpoint_diff_lines(&view.body_lines, palette));
    } else {
        lines.extend(
            view.body_lines
                .iter()
                .cloned()
                .map(|line| Line::styled(line, Style::default().fg(palette.text_secondary))),
        );
    }
    lines
}

fn checkpoint_diff_lines(lines: &[String], palette: &ThemePalette) -> Vec<Line<'static>> {
    let numbered = number_unified_diff_lines(lines.iter().map(String::as_str));
    let number_width = diff_line_number_width(&numbered);
    numbered
        .into_iter()
        .map(|line| {
            let (gutter_color, line_style) = diff_line_style_for_palette(line.kind, palette);
            Line::from(vec![
                Span::styled(
                    diff_line_number_text(line.old_line, number_width),
                    Style::default().fg(gutter_color),
                ),
                Span::raw(" "),
                Span::styled(
                    diff_line_number_text(line.new_line, number_width),
                    Style::default().fg(gutter_color),
                ),
                Span::styled("│ ", Style::default().fg(palette.diff_gutter_fg)),
                Span::styled(line.text.to_owned(), line_style),
            ])
        })
        .collect()
}

fn checkpoint_action_lines(
    view: &CheckpointRestoreModalView,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    let mut actions = Vec::new();
    if view.can_restore {
        actions.extend(action_badge(
            "Enter",
            "restore",
            palette.accent_success,
            palette,
        ));
    }
    if view.can_fork {
        if !actions.is_empty() {
            actions.push(Span::raw("  "));
        }
        actions.extend(action_badge(
            "F",
            "fork (files unchanged)",
            palette.accent_info,
            palette,
        ));
    }
    if actions.is_empty() {
        actions.push(Span::styled(
            match view.phase {
                CheckpointRestoreModalPhase::Loading => "Loading exact preview...",
                CheckpointRestoreModalPhase::Restoring => "Restoring controlled files...",
                CheckpointRestoreModalPhase::Forking => "Creating conversation fork...",
                _ => "No restore action is currently available",
            },
            Style::default().fg(palette.text_muted),
        ));
    }
    let close_hint = if matches!(
        view.phase,
        CheckpointRestoreModalPhase::Restoring | CheckpointRestoreModalPhase::Forking
    ) {
        "Esc locked while applying"
    } else {
        "Esc close · Ctrl-R refresh"
    };
    vec![
        Line::from(actions),
        Line::styled(close_hint, Style::default().fg(palette.text_muted)),
    ]
}

fn action_badge(
    key: &str,
    label: &str,
    color: Color,
    palette: &ThemePalette,
) -> Vec<Span<'static>> {
    vec![
        Span::styled(
            format!(" {key} "),
            Style::default()
                .fg(palette.text_inverse)
                .bg(color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" {label}"),
            Style::default().fg(palette.text_secondary),
        ),
    ]
}

fn phase_badge(phase: CheckpointRestoreModalPhase, palette: &ThemePalette) -> Span<'static> {
    let label = match phase {
        CheckpointRestoreModalPhase::Loading => " LOADING ",
        CheckpointRestoreModalPhase::Ready => " READY ",
        CheckpointRestoreModalPhase::Blocked => " BLOCKED ",
        CheckpointRestoreModalPhase::Restoring => " RESTORING ",
        CheckpointRestoreModalPhase::Forking => " FORKING ",
        CheckpointRestoreModalPhase::Unavailable => " UNAVAILABLE ",
        CheckpointRestoreModalPhase::Empty => " NO CHECKPOINT ",
    };
    Span::styled(
        label,
        Style::default()
            .fg(palette.text_inverse)
            .bg(phase_color(phase, palette))
            .add_modifier(Modifier::BOLD),
    )
}

fn phase_color(phase: CheckpointRestoreModalPhase, palette: &ThemePalette) -> Color {
    match phase {
        CheckpointRestoreModalPhase::Loading | CheckpointRestoreModalPhase::Forking => {
            palette.accent_info
        }
        CheckpointRestoreModalPhase::Ready => palette.accent_success,
        CheckpointRestoreModalPhase::Blocked => palette.accent_danger,
        CheckpointRestoreModalPhase::Restoring => palette.accent_warning,
        CheckpointRestoreModalPhase::Unavailable | CheckpointRestoreModalPhase::Empty => {
            palette.text_muted
        }
    }
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/checkpoint_restore_tests.rs"]
mod tests;
