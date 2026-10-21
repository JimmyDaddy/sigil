use std::ops::Range;

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph},
};

use crate::{setup::SetupRecoveryAction, surface::SetupRecoverySurface};

use super::{ThemePalette, render_setup_line_with_palette, wrap_config_visual_line};

pub(in crate::ui) struct SetupContentLayout {
    pub area: Rect,
    pub body: Rect,
    pub rows: Vec<Line<'static>>,
    pub logical_ranges: Vec<Range<usize>>,
    pub scroll: usize,
    pub max_scroll: usize,
    pub actions: Vec<(SetupRecoveryAction, Rect)>,
    pub scroll_hint: Option<Rect>,
}

pub(in crate::ui) fn setup_content_layout(
    screen: Rect,
    lines: &[String],
    recovery: Option<&SetupRecoverySurface>,
    palette: &ThemePalette,
) -> SetupContentLayout {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(4), Constraint::Min(0)])
        .split(screen);
    let area = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(10),
            Constraint::Min(68),
            Constraint::Percentage(10),
        ])
        .split(outer[1])[1];
    let inner = area.inner(ratatui::layout::Margin::new(1, 1));
    let action_count = recovery.map_or(0, |view| view.actions.len());
    let footer_rows = (action_count + usize::from(recovery.is_some_and(|view| view.diagnostics)))
        .min(inner.height as usize) as u16;
    let body = Rect::new(
        inner.x,
        inner.y,
        inner.width,
        inner.height.saturating_sub(footer_rows),
    );
    let mut rows = Vec::new();
    let mut logical_ranges = Vec::with_capacity(lines.len());
    for (index, source) in lines.iter().enumerate() {
        let start = rows.len();
        let line = if recovery.is_some() || source.contains(['\n', '\r']) {
            Line::from(Span::styled(
                source.clone(),
                Style::default().fg(if index == 0 {
                    palette.text_primary
                } else {
                    palette.text_secondary
                }),
            ))
        } else {
            render_setup_line_with_palette(source, palette)
        };
        rows.extend(wrap_config_visual_line(line, inner.width.max(1) as usize));
        logical_ranges.push(start..rows.len());
    }
    let max_scroll = rows.len().saturating_sub(body.height as usize);
    let scroll = recovery
        .map_or_else(
            || {
                let selected = lines.iter().position(|line| line.starts_with("> "));
                selected
                    .map(|index| {
                        logical_ranges[index]
                            .end
                            .saturating_sub(body.height as usize)
                    })
                    .unwrap_or(0)
            },
            |view| view.scroll_rows,
        )
        .min(max_scroll);
    let actions = recovery.map_or_else(Vec::new, |view| {
        view.actions
            .iter()
            .enumerate()
            .filter_map(|(index, action)| {
                let row = body.height.saturating_add(index as u16);
                (row < inner.height).then_some((
                    *action,
                    Rect::new(inner.x, inner.y.saturating_add(row), inner.width, 1),
                ))
            })
            .collect()
    });
    let scroll_hint = recovery.filter(|view| view.diagnostics).and_then(|_| {
        let row = body.height as usize + action_count;
        (row < inner.height as usize).then_some(Rect::new(
            inner.x,
            inner.y.saturating_add(row as u16),
            inner.width,
            1,
        ))
    });
    SetupContentLayout {
        area,
        body,
        rows,
        logical_ranges,
        scroll,
        max_scroll,
        actions,
        scroll_hint,
    }
}

pub(crate) fn setup_recovery_scroll_bounds(
    width: u16,
    height: u16,
    lines: &[String],
    recovery: &SetupRecoverySurface,
) -> (usize, usize) {
    let layout = setup_content_layout(
        Rect::new(0, 0, width, height),
        lines,
        Some(recovery),
        &super::theme::default_palette(),
    );
    (layout.max_scroll, (layout.body.height as usize).max(1))
}

pub(super) fn render_setup_content(
    frame: &mut Frame,
    title: &str,
    lines: &[String],
    recovery: Option<&SetupRecoverySurface>,
    palette: &ThemePalette,
) {
    let layout = setup_content_layout(frame.area(), lines, recovery, palette);
    frame.render_widget(
        Block::default()
            .title(title)
            .title_style(
                Style::default()
                    .fg(palette.text_primary)
                    .add_modifier(Modifier::BOLD),
            )
            .border_type(BorderType::Rounded)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(palette.border_subtle))
            .style(Style::default().bg(palette.setup_bg)),
        layout.area,
    );
    let visible = layout
        .rows
        .into_iter()
        .skip(layout.scroll)
        .take(layout.body.height as usize)
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(visible), layout.body);
    for (action, area) in layout.actions {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                action.label(),
                Style::default().fg(palette.config_warning),
            ))),
            area,
        );
    }
    if let Some(area) = layout.scroll_hint {
        frame.render_widget(
            Paragraph::new("Up/Down scroll · PgUp/PgDn page · Home/End"),
            area,
        );
    }
}
