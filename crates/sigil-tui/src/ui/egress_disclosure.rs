use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

use crate::app::{AppState, EGRESS_DISCLOSURE_HEIGHT};

use super::{geometry::inset_rect, theme::Theme};

pub(crate) fn egress_disclosure_layout(area: Rect, app: &AppState) -> (Option<Rect>, Rect) {
    let reserved_rows = app.egress_disclosure_reserved_rows(area.height);
    if reserved_rows == 0 {
        return (None, area);
    }
    let disclosure = Rect::new(area.x, area.y, area.width, reserved_rows);
    let content = Rect::new(
        area.x,
        area.y.saturating_add(reserved_rows),
        area.width,
        area.height.saturating_sub(reserved_rows),
    );
    (Some(disclosure), content)
}

/// Renders the active disclosure card. The launcher acknowledges it only after `Terminal::draw`
/// succeeds, so drawing this widget alone never grants network egress.
pub(crate) fn render_active_egress_disclosure_card(
    frame: &mut Frame,
    area: Rect,
    app: &AppState,
    theme: &Theme,
) -> bool {
    let Some(card) = app.active_egress_disclosure_card() else {
        return false;
    };
    if area.width < 24 || area.height < EGRESS_DISCLOSURE_HEIGHT {
        return false;
    }
    let card_area = Rect::new(
        area.x.saturating_add(1),
        area.y,
        area.width.saturating_sub(2),
        EGRESS_DISCLOSURE_HEIGHT,
    );
    if card_area.width < 22 || card_area.height < 5 {
        return false;
    }
    let palette = &theme.palette;
    frame.render_widget(Clear, card_area);
    let block = Block::default()
        .title(" Network disclosure ")
        .borders(Borders::ALL)
        .style(Style::default().bg(palette.surface_panel_alt));
    let inner = inset_rect(block.inner(card_area), 1, 0);
    frame.render_widget(block, card_area);
    let lines = vec![
        Line::from(Span::styled(
            card.title,
            Style::default()
                .fg(palette.text_primary)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(card.destination),
        Line::from(format!(
            "{} · {} · active",
            card.route, card.data_categories
        )),
    ];
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .style(Style::default().bg(palette.surface_panel_alt))
            .wrap(Wrap { trim: true }),
        inner,
    );
    app.mark_egress_disclosure_rendered();
    true
}

#[cfg(test)]
#[path = "tests/egress_disclosure_tests.rs"]
mod tests;
