use ratatui::layout::{Constraint, Direction, Layout, Rect};

pub(crate) fn sidebar_width_for_terminal(total_width: usize) -> usize {
    let min = if total_width < 72 { 16 } else { 24 };
    let max = if total_width < 72 { 24 } else { 42 };
    ((total_width * 30) / 100).clamp(min, max)
}

pub(crate) fn selector_window_range(
    total: usize,
    selected: usize,
    visible: usize,
) -> (usize, usize) {
    if total <= visible || visible == 0 {
        return (0, total);
    }

    let half = visible / 2;
    let max_start = total.saturating_sub(visible);
    let start = selected.saturating_sub(half).min(max_start);
    (start, start + visible)
}

pub(crate) fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let popup_width = width.min(area.width.saturating_sub(2)).max(24);
    let popup_height = height.min(area.height.saturating_sub(2)).max(6);
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Fill(1),
            Constraint::Length(popup_height),
            Constraint::Fill(1),
        ])
        .split(area);
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Fill(1),
            Constraint::Length(popup_width),
            Constraint::Fill(1),
        ])
        .split(vertical[1]);
    horizontal[1]
}

pub(crate) fn shadow_rect(area: Rect, bounds: Rect) -> Rect {
    let x = area.x.saturating_add(1);
    let y = area.y.saturating_add(1);
    let right = bounds.x.saturating_add(bounds.width);
    let bottom = bounds.y.saturating_add(bounds.height);
    Rect {
        x,
        y,
        width: area.width.min(right.saturating_sub(x)),
        height: area.height.min(bottom.saturating_sub(y)),
    }
}

pub(crate) fn inset_rect(area: Rect, x_pad: u16, y_pad: u16) -> Rect {
    let width = area.width.saturating_sub(x_pad.saturating_mul(2));
    let height = area.height.saturating_sub(y_pad.saturating_mul(2));
    Rect {
        x: area.x.saturating_add(x_pad),
        y: area.y.saturating_add(y_pad),
        width,
        height,
    }
}

pub(crate) fn halo_rect(area: Rect, bounds: Rect, x_pad: u16, y_pad: u16) -> Rect {
    let x = area.x.saturating_sub(x_pad);
    let y = area.y.saturating_sub(y_pad);
    let right = bounds.x.saturating_add(bounds.width);
    let bottom = bounds.y.saturating_add(bounds.height);
    let expanded_right = area.x.saturating_add(area.width).saturating_add(x_pad);
    let expanded_bottom = area.y.saturating_add(area.height).saturating_add(y_pad);
    Rect {
        x,
        y,
        width: expanded_right.min(right).saturating_sub(x),
        height: expanded_bottom.min(bottom).saturating_sub(y),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shadow_rect_offsets_and_stays_within_bounds() {
        let bounds = Rect::new(0, 0, 40, 20);
        let area = Rect::new(10, 4, 20, 6);
        assert_eq!(shadow_rect(area, bounds), Rect::new(11, 5, 20, 6));

        let clipped = shadow_rect(Rect::new(30, 18, 10, 4), bounds);
        assert_eq!(clipped, Rect::new(31, 19, 9, 1));
    }

    #[test]
    fn halo_rect_expands_and_clips_to_bounds() {
        let bounds = Rect::new(0, 0, 40, 20);
        let area = Rect::new(10, 4, 20, 6);
        assert_eq!(halo_rect(area, bounds, 4, 1), Rect::new(6, 3, 28, 8));

        let clipped = halo_rect(Rect::new(1, 1, 10, 4), bounds, 4, 2);
        assert_eq!(clipped, Rect::new(0, 0, 15, 7));
    }
}
