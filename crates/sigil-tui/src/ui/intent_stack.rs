use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap},
};

use crate::app::{AppState, IntentStackModalPhase, IntentStackModalView};

use super::{
    geometry::{centered_rect, halo_rect, shadow_rect},
    theme::{self, ThemePalette},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct IntentStackModalLayout {
    pub(super) area: Rect,
    pub(super) summary: Rect,
    pub(super) list: Rect,
    pub(super) detail: Rect,
    pub(super) actions: Rect,
    pub(super) row_areas: Vec<(usize, Rect)>,
    pub(super) primary_action: Rect,
}

pub(super) fn render_intent_stack_modal(frame: &mut Frame, app: &AppState) {
    let Some(view) = app.intent_stack_modal_view() else {
        return;
    };
    let current_theme = theme::resolve_for_app(app);
    let palette = &current_theme.palette;
    render_intent_stack_modal_view(frame, &view, palette);
}

fn render_intent_stack_modal_view(
    frame: &mut Frame,
    view: &IntentStackModalView,
    palette: &ThemePalette,
) {
    let layout = intent_stack_modal_layout(frame.area(), view);
    let area = layout.area;
    let backdrop = halo_rect(area, frame.area(), 5, 2);
    if backdrop.width > 0 && backdrop.height > 0 {
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
        .title(" Intent Stack ")
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
    frame.render_widget(block, area);

    render_summary(frame, layout.summary, view, palette);
    render_intent_list(frame, layout.list, &layout.row_areas, view, palette);
    render_detail(frame, layout.detail, view, palette);
    render_actions(frame, layout.actions, view, palette);
}

pub(super) fn intent_stack_modal_layout(
    screen: Rect,
    view: &IntentStackModalView,
) -> IntentStackModalLayout {
    let width = screen
        .width
        .saturating_sub(4)
        .clamp(20, 116)
        .min(screen.width);
    let height = screen
        .height
        .saturating_sub(2)
        .clamp(8, 38)
        .min(screen.height);
    let area = centered_rect(width, height, screen);
    let inner = Rect::new(
        area.x.saturating_add(1),
        area.y.saturating_add(1),
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
    );
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(4),
            Constraint::Length(3),
        ])
        .split(inner);
    let (list, detail) = if inner.width >= 78 {
        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(38), Constraint::Percentage(62)])
            .split(vertical[1]);
        (body[0], body[1])
    } else {
        let list_height = (view.rows.len() as u16)
            .saturating_add(2)
            .clamp(3, vertical[1].height.saturating_sub(3).max(3));
        let body = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(list_height), Constraint::Min(3)])
            .split(vertical[1]);
        (body[0], body[1])
    };
    let list_content_height = list.height.saturating_sub(2) as usize;
    let window_start = view
        .selected
        .saturating_add(1)
        .saturating_sub(list_content_height)
        .min(view.rows.len().saturating_sub(list_content_height));
    let row_areas = (window_start..view.rows.len())
        .take(list_content_height)
        .enumerate()
        .map(|(offset, index)| {
            (
                index,
                Rect::new(
                    list.x.saturating_add(1),
                    list.y.saturating_add(1 + offset as u16),
                    list.width.saturating_sub(2),
                    1,
                ),
            )
        })
        .collect::<Vec<_>>();
    let primary_width = (view.primary_action_label.chars().count() as u16)
        .saturating_add(4)
        .min(vertical[2].width.saturating_sub(2));
    let primary_action = Rect::new(
        vertical[2]
            .right()
            .saturating_sub(primary_width.saturating_add(1)),
        vertical[2].y.saturating_add(1),
        primary_width,
        1,
    );
    IntentStackModalLayout {
        area,
        summary: vertical[0],
        list,
        detail,
        actions: vertical[2],
        row_areas,
        primary_action,
    }
}

fn render_summary(
    frame: &mut Frame,
    area: Rect,
    view: &IntentStackModalView,
    palette: &ThemePalette,
) {
    let mut lines = vec![Line::from(vec![
        Span::styled(
            phase_label(view.phase),
            Style::default()
                .fg(phase_color(view.phase, palette))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("  {}", view.phase_detail),
            Style::default().fg(palette.text_secondary),
        ),
    ])];
    lines.extend(view.summary_lines.iter().take(2).cloned().map(Line::raw));
    if let Some(error) = &view.error {
        lines.push(Line::styled(
            format!("Unavailable: {error}"),
            Style::default().fg(palette.accent_danger),
        ));
    }
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .block(
                Block::default()
                    .borders(Borders::BOTTOM)
                    .border_style(Style::default().fg(palette.border_subtle)),
            )
            .style(Style::default().bg(palette.modal_bg))
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_intent_list(
    frame: &mut Frame,
    area: Rect,
    row_areas: &[(usize, Rect)],
    view: &IntentStackModalView,
    palette: &ThemePalette,
) {
    frame.render_widget(
        Block::default()
            .title(format!("Changes ({})", view.rows.len()))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(palette.border_subtle))
            .style(Style::default().bg(palette.modal_bg)),
        area,
    );
    for (index, row_area) in row_areas {
        let Some(row) = view.rows.get(*index) else {
            continue;
        };
        let selected = *index == view.selected;
        let background = if selected {
            palette.surface_selection
        } else {
            palette.modal_bg
        };
        let marker = if selected { "●" } else { " " };
        let text = format!("{marker} {}  [{}]  {}", row.title, row.status, row.detail);
        frame.render_widget(
            Paragraph::new(Line::styled(
                text,
                Style::default()
                    .fg(if selected {
                        palette.text_primary
                    } else {
                        palette.text_secondary
                    })
                    .bg(background)
                    .add_modifier(if selected {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            )),
            *row_area,
        );
    }
}

fn render_detail(
    frame: &mut Frame,
    area: Rect,
    view: &IntentStackModalView,
    palette: &ThemePalette,
) {
    let block = Block::default()
        .title(if view.phase == IntentStackModalPhase::ConfirmingDrop {
            "Exact Drop preview"
        } else {
            "Intent details"
        })
        .borders(Borders::ALL)
        .border_style(Style::default().fg(palette.border_subtle))
        .style(Style::default().bg(palette.modal_bg));
    let content = block.inner(area);
    frame.render_widget(block, area);
    if content.width == 0 || content.height == 0 {
        return;
    }
    let max_scroll = view
        .detail_lines
        .len()
        .saturating_sub(content.height as usize);
    let scroll = (view.detail_scroll as usize).min(max_scroll);
    let lines = view
        .detail_lines
        .iter()
        .skip(scroll)
        .take(content.height as usize)
        .enumerate()
        .map(|(index, line)| {
            let style = if line == "Blocked / manual resolution required" {
                Style::default()
                    .fg(palette.accent_danger)
                    .add_modifier(Modifier::BOLD)
            } else if line == "Acceptance criteria" || line == "Retained artifacts" {
                Style::default()
                    .fg(palette.accent_info)
                    .add_modifier(Modifier::BOLD)
            } else if index == 0 {
                Style::default()
                    .fg(palette.text_primary)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(palette.text_secondary)
            };
            Line::styled(line.clone(), style)
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .style(Style::default().bg(palette.modal_bg))
            .wrap(Wrap { trim: false }),
        content,
    );
}

fn render_actions(
    frame: &mut Frame,
    area: Rect,
    view: &IntentStackModalView,
    palette: &ThemePalette,
) {
    let primary_style = if view.primary_action_enabled {
        Style::default()
            .fg(palette.text_inverse)
            .bg(if view.phase == IntentStackModalPhase::ConfirmingDrop {
                palette.accent_warning
            } else {
                palette.accent_success
            })
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(palette.text_muted)
            .bg(palette.surface_selection)
    };
    let refresh = if view.can_refresh {
        "R Refresh"
    } else {
        "R Wait"
    };
    let lines = vec![
        Line::raw(String::new()),
        Line::from(vec![
            Span::styled(
                "Up/Down Select  PgUp/PgDn Details  ",
                Style::default().fg(palette.text_secondary),
            ),
            Span::styled(
                format!("{refresh}  Esc Close  "),
                Style::default().fg(palette.accent_info),
            ),
            Span::styled(format!(" {} ", view.primary_action_label), primary_style),
        ]),
    ];
    frame.render_widget(
        Paragraph::new(Text::from(lines)).style(Style::default().bg(palette.modal_bg)),
        area,
    );
}

fn phase_label(phase: IntentStackModalPhase) -> &'static str {
    match phase {
        IntentStackModalPhase::Loading => "LOADING",
        IntentStackModalPhase::Ready => "READY",
        IntentStackModalPhase::ReadOnly => "READ ONLY",
        IntentStackModalPhase::HistoryUnavailable => "HISTORY UNAVAILABLE",
        IntentStackModalPhase::PreviewingDrop => "PREVIEWING",
        IntentStackModalPhase::ConfirmingDrop => "CONFIRM DROP",
        IntentStackModalPhase::ApplyingDrop => "APPLYING",
        IntentStackModalPhase::Unavailable => "UNAVAILABLE",
    }
}

fn phase_color(phase: IntentStackModalPhase, palette: &ThemePalette) -> ratatui::style::Color {
    match phase {
        IntentStackModalPhase::Ready => palette.accent_success,
        IntentStackModalPhase::ConfirmingDrop => palette.accent_warning,
        IntentStackModalPhase::Unavailable => palette.accent_danger,
        IntentStackModalPhase::Loading
        | IntentStackModalPhase::ReadOnly
        | IntentStackModalPhase::HistoryUnavailable
        | IntentStackModalPhase::PreviewingDrop
        | IntentStackModalPhase::ApplyingDrop => palette.accent_info,
    }
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/intent_stack_tests.rs"]
mod tests;
