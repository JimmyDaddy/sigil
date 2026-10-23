use ratatui::{
    Frame,
    layout::{Margin, Rect},
    style::{Modifier, Style},
    text::Line,
    widgets::{Block, BorderType, Borders, Paragraph},
};

use crate::surface::WorkspaceTrustSurface;

use super::{ThemePalette, wrap_config_visual_line};

const TITLE: &str = "Trust this workspace?";
const TRUST_EFFECT: &str = "Trust loads repo-local instructions/checks and allows agent execution.";
const UNTRUSTED_EFFECT: &str = "Without trust: data-only; normal agent execution stays blocked.";
const ACTIONS: &str = "Enter trust and continue · Esc / Ctrl-C quit";

struct WorkspaceTrustLayout {
    area: Rect,
    explanation: Rect,
    explanation_rows: Vec<Line<'static>>,
    path: Rect,
    path_rows: Vec<Line<'static>>,
    scroll: usize,
    max_scroll: usize,
    scroll_hint: Option<Rect>,
    actions: Rect,
    action_rows: Vec<Line<'static>>,
}

fn workspace_trust_layout(screen: Rect, view: &WorkspaceTrustSurface) -> WorkspaceTrustLayout {
    let width = screen.width.min(100);
    let width_cells = width.saturating_sub(2).max(1) as usize;
    let explanation_rows = [TRUST_EFFECT, UNTRUSTED_EFFECT]
        .into_iter()
        .flat_map(|text| wrap_config_visual_line(Line::from(text), width_cells))
        .collect::<Vec<_>>();
    let path_rows = wrap_config_visual_line(Line::from(view.workspace_label.clone()), width_cells);
    let action_rows = wrap_config_visual_line(Line::from(ACTIONS), width_cells);
    let desired_height = explanation_rows.len() + path_rows.len() + action_rows.len() + 4;
    let top_margin = u16::from(screen.height > 14);
    let area = Rect::new(
        screen.x + screen.width.saturating_sub(width) / 2,
        screen.y + top_margin,
        width,
        screen
            .height
            .saturating_sub(top_margin.saturating_mul(2))
            .min(desired_height.min(u16::MAX as usize) as u16),
    );
    let inner = area.inner(Margin::new(1, 1));
    let action_height = (action_rows.len() as u16).min(inner.height);
    let explanation_height =
        (explanation_rows.len() as u16).min(inner.height.saturating_sub(action_height));
    let available = inner
        .height
        .saturating_sub(action_height)
        .saturating_sub(explanation_height);
    let gap = u16::from(available > 2);
    let path_height = available.saturating_sub(gap.saturating_mul(2));
    let show_scroll = path_rows.len() > path_height as usize;
    let path = Rect::new(
        inner.x,
        inner.y + explanation_height + gap,
        inner.width,
        path_height.saturating_sub(u16::from(show_scroll)),
    );
    let max_scroll = path_rows.len().saturating_sub(path.height as usize);
    WorkspaceTrustLayout {
        area,
        explanation: Rect::new(inner.x, inner.y, inner.width, explanation_height),
        explanation_rows,
        path,
        path_rows,
        scroll: view.path_scroll_rows.min(max_scroll),
        max_scroll,
        scroll_hint: show_scroll.then_some(Rect::new(path.x, path.bottom(), path.width, 1)),
        actions: Rect::new(
            inner.x,
            inner.bottom().saturating_sub(action_height),
            inner.width,
            action_height,
        ),
        action_rows,
    }
}

pub(crate) fn workspace_trust_scroll_bounds(
    width: u16,
    height: u16,
    view: &WorkspaceTrustSurface,
) -> (usize, usize) {
    let layout = workspace_trust_layout(Rect::new(0, 0, width, height), view);
    (layout.max_scroll, (layout.path.height as usize).max(1))
}

pub(super) fn render_workspace_trust(
    frame: &mut Frame,
    view: &WorkspaceTrustSurface,
    palette: &ThemePalette,
) {
    let layout = workspace_trust_layout(frame.area(), view);
    frame.render_widget(
        Block::default()
            .title(TITLE)
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
    frame.render_widget(
        Paragraph::new(layout.explanation_rows).style(Style::default().fg(palette.text_secondary)),
        layout.explanation,
    );
    let total_rows = layout.path_rows.len();
    frame.render_widget(
        Paragraph::new(
            layout
                .path_rows
                .into_iter()
                .skip(layout.scroll)
                .take(layout.path.height as usize)
                .collect::<Vec<_>>(),
        )
        .style(Style::default().fg(palette.text_primary)),
        layout.path,
    );
    if let Some(area) = layout.scroll_hint {
        frame.render_widget(
            Paragraph::new(format!(
                "Folder lines {}-{}/{} · ↑/↓ · PgUp/PgDn · Home/End",
                layout.scroll.saturating_add(1),
                layout.scroll.saturating_add(layout.path.height as usize),
                total_rows,
            ))
            .style(Style::default().fg(palette.text_secondary)),
            area,
        );
    }
    frame.render_widget(
        Paragraph::new(layout.action_rows).style(Style::default().fg(palette.config_warning)),
        layout.actions,
    );
}
