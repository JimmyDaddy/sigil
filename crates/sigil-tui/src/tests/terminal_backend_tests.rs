use ratatui::{
    backend::{Backend, TestBackend},
    layout::Position,
};

use super::CachedCursorBackend;

#[test]
fn capture_seeds_the_cache_before_async_input_ownership_begins() {
    let mut inner = TestBackend::new(20, 10);
    inner
        .set_cursor_position(Position::new(7, 6))
        .expect("test backend cursor should move");

    let mut backend =
        CachedCursorBackend::capture(inner).expect("initial cursor capture should succeed");

    assert_eq!(
        backend
            .get_cursor_position()
            .expect("captured cursor should be available"),
        Position::new(7, 6)
    );
}

#[test]
fn cached_cursor_reads_do_not_query_the_inner_backend() {
    let mut inner = TestBackend::new(20, 10);
    inner
        .set_cursor_position(Position::new(9, 8))
        .expect("test backend cursor should move");
    let mut backend = CachedCursorBackend::with_position(inner, Position::new(2, 3));

    assert_eq!(
        backend
            .get_cursor_position()
            .expect("cached cursor should be available"),
        Position::new(2, 3)
    );
    assert_eq!(
        backend
            .inner
            .get_cursor_position()
            .expect("inner cursor should remain available"),
        Position::new(9, 8),
        "a cached read must not consult or rewrite the terminal reader"
    );
}

#[test]
fn cursor_writes_and_appended_lines_update_the_cache() {
    let inner = TestBackend::new(20, 10);
    let mut backend = CachedCursorBackend::with_position(inner, Position::new(2, 3));

    backend
        .set_cursor_position(Position::new(4, 5))
        .expect("cursor write should succeed");
    backend
        .append_lines(20)
        .expect("appended lines should succeed");

    assert_eq!(
        backend
            .get_cursor_position()
            .expect("cached cursor should be available"),
        Position::new(0, 9)
    );
}
