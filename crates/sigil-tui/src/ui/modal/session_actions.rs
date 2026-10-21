use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};

use super::ModalVisual;
use crate::{
    surface::{SessionActionsSurface, SessionModalAction},
    ui::text::{sanitize_terminal_text, wrap_terminal_lines},
};

const SCROLL_HINT: &str = "↑/↓ scroll · PgUp/PgDn · Home/End";
const BACK: &str = "[back] Session actions · Esc/Backspace";

pub(in crate::ui) struct SessionActionsLayout {
    pub(crate) area: Rect,
    body: Rect,
    footer: Rect,
    pub(crate) actions: Vec<(SessionModalAction, Rect)>,
    rows: Vec<Line<'static>>,
    scroll: usize,
    max_scroll: usize,
}

pub(crate) fn session_actions_scroll_bounds(
    width: u16,
    height: u16,
    lines: &[String],
    view: &SessionActionsSurface,
) -> (usize, usize) {
    let layout = session_actions_layout(Rect::new(0, 0, width, height), lines, &[], view);
    (layout.max_scroll, layout.body.height as usize)
}

pub(in crate::ui) fn session_actions_layout(
    screen: Rect,
    lines: &[String],
    actions: &[(SessionModalAction, String)],
    view: &SessionActionsSurface,
) -> SessionActionsLayout {
    let width = screen
        .width
        .saturating_sub(4)
        .max(screen.width.min(24))
        .min(112);
    let body_width = width.saturating_sub(2);
    let mut rows = Vec::new();
    let mut action_ranges = Vec::new();
    for line in lines {
        let start = rows.len();
        rows.extend(wrap_terminal_lines(
            vec![Line::from(Span::raw(
                line.split('\n')
                    .map(sanitize_terminal_text)
                    .collect::<Vec<_>>()
                    .join("\n"),
            ))],
            body_width as usize,
        ));
        if !view.details_open
            && let Some((action, _)) = actions.iter().find(|(_, label)| label == line)
        {
            action_ranges.push((*action, start..rows.len()));
        }
    }
    let footer_lines = footer_lines(view);
    let footer_rows = wrap_terminal_lines(footer_lines, body_width as usize).len();
    let height =
        rows.len()
            .saturating_add(footer_rows)
            .saturating_add(2)
            .min(screen.height.saturating_sub(2).max(screen.height.min(3)) as usize) as u16;
    let area = Rect::new(
        screen.x + screen.width.saturating_sub(width) / 2,
        screen.y + screen.height.saturating_sub(height) / 2,
        width,
        height,
    );
    let inner = Block::default().borders(Borders::ALL).inner(area);
    let footer_height = (footer_rows as u16).min(inner.height.saturating_sub(1));
    let body = Rect::new(
        inner.x,
        inner.y,
        inner.width,
        inner.height.saturating_sub(footer_height),
    );
    let footer = Rect::new(inner.x, body.bottom(), inner.width, footer_height);
    let max_scroll = rows.len().saturating_sub(body.height as usize);
    let scroll = view.scroll_rows.min(max_scroll);
    let mut hit_actions = action_ranges
        .into_iter()
        .filter_map(|(action, range)| {
            let start = range.start.max(scroll);
            let end = range.end.min(scroll.saturating_add(body.height as usize));
            (start < end).then(|| {
                (
                    action,
                    Rect::new(
                        body.x,
                        body.y + (start - scroll) as u16,
                        body.width,
                        (end - start) as u16,
                    ),
                )
            })
        })
        .collect::<Vec<_>>();
    if footer.height > 0 {
        let hint_rows =
            wrap_terminal_lines(vec![Line::raw(SCROLL_HINT)], footer.width as usize).len();
        let back_y = footer
            .y
            .saturating_add(hint_rows as u16)
            .min(footer.bottom().saturating_sub(1));
        hit_actions.push((
            if view.details_open {
                SessionModalAction::Back
            } else {
                SessionModalAction::Close
            },
            Rect::new(
                footer.x,
                back_y,
                footer.width,
                footer.bottom().saturating_sub(back_y),
            ),
        ));
    }
    SessionActionsLayout {
        area,
        body,
        footer,
        actions: hit_actions,
        rows,
        scroll,
        max_scroll,
    }
}

fn footer_lines(view: &SessionActionsSurface) -> Vec<Line<'static>> {
    vec![
        Line::raw(SCROLL_HINT),
        Line::raw(if view.details_open { BACK } else { "Esc close" }),
    ]
}

pub(super) fn render_session_actions(
    frame: &mut Frame,
    lines: &[String],
    actions: &[(SessionModalAction, String)],
    view: &SessionActionsSurface,
    visual: &ModalVisual,
) {
    let layout = session_actions_layout(frame.area(), lines, actions, view);
    frame.render_widget(Clear, layout.area);
    frame.render_widget(
        Block::default()
            .title(if view.details_open {
                " Session details "
            } else {
                " Session Actions "
            })
            .title_style(
                Style::default()
                    .fg(visual.text)
                    .add_modifier(Modifier::BOLD),
            )
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(visual.muted))
            .style(Style::default().bg(visual.modal_bg)),
        layout.area,
    );
    frame.render_widget(
        Paragraph::new(Text::from(
            layout
                .rows
                .into_iter()
                .skip(layout.scroll)
                .take(layout.body.height as usize)
                .collect::<Vec<_>>(),
        ))
        .style(Style::default().fg(visual.text).bg(visual.modal_bg)),
        layout.body,
    );
    frame.render_widget(
        Paragraph::new(Text::from(wrap_terminal_lines(
            footer_lines(view),
            layout.footer.width as usize,
        )))
        .style(Style::default().fg(visual.muted).bg(visual.modal_bg)),
        layout.footer,
    );
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "../tests/session_actions_tests.rs"]
mod tests;
