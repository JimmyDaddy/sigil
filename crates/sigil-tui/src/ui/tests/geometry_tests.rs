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

#[test]
fn selector_window_range_centers_selection_when_list_exceeds_viewport() {
    assert_eq!(selector_window_range(20, 0, 5), (0, 5));
    assert_eq!(selector_window_range(20, 10, 5), (8, 13));
    assert_eq!(selector_window_range(20, 19, 5), (15, 20));
}
