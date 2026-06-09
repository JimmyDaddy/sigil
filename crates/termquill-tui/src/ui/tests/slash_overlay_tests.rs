use ratatui::layout::Rect;

use super::*;

#[test]
fn slash_selector_overlay_rect_tracks_composer_width() {
    let live = Rect::new(0, 0, 120, 24);
    let composer = Rect::new(0, 20, 120, 4);

    assert_eq!(
        slash_selector_overlay_rect(live, composer, 6),
        Some(Rect::new(1, 14, 118, 6))
    );
}
