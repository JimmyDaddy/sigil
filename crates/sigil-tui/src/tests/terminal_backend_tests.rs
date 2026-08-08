use std::{
    cell::RefCell,
    io::{self, Write},
    rc::Rc,
};

use ratatui::{
    backend::{Backend, ClearType, CrosstermBackend, TestBackend},
    buffer::Cell,
    layout::{Position, Size},
};

use super::CachedCursorBackend;

#[derive(Clone, Default)]
struct RecordingWriter(Rc<RefCell<Vec<u8>>>);

impl RecordingWriter {
    fn snapshot(&self) -> Vec<u8> {
        self.0.borrow().clone()
    }
}

impl Write for RecordingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

type WriterBackend = CachedCursorBackend<CrosstermBackend<RecordingWriter>>;

fn writer_backend(
    cursor_position: Position,
    terminal_size: Size,
) -> (WriterBackend, RecordingWriter) {
    let writer = RecordingWriter::default();
    let backend = WriterBackend {
        inner: CrosstermBackend::new(writer.clone()),
        cursor_position,
        terminal_size: std::cell::Cell::new(Some(terminal_size)),
    };
    (backend, writer)
}

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
fn cursor_writes_and_appended_lines_preserve_the_column_in_the_cache() {
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
        Position::new(4, 9)
    );
}

#[test]
fn draw_normalizes_the_real_writer_cursor_after_a_wide_symbol() {
    let (mut backend, writer) = writer_backend(Position::ORIGIN, Size::new(20, 10));
    let cell = Cell::new("界");

    backend
        .draw(std::iter::once((2, 3, &cell)))
        .expect("wide cell should draw");

    assert_eq!(
        backend
            .get_cursor_position()
            .expect("cached cursor should be available"),
        Position::new(4, 3),
        "the two-column symbol must not be tracked as a one-column cell"
    );
    let output = writer.snapshot();
    assert!(
        output.starts_with(b"\x1b[4;3H"),
        "Crossterm should position the writer at the cell before printing"
    );
    assert!(
        output
            .windows("界".len())
            .any(|window| window == "界".as_bytes()),
        "the real writer sequence should contain the wide symbol"
    );
    assert!(
        output.ends_with(b"\x1b[4;5H"),
        "the adapter should explicitly align the real cursor with its cache"
    );
}

#[test]
fn draw_uses_ratatui_width_for_halfwidth_katakana_marks() {
    let (mut backend, _) = writer_backend(Position::ORIGIN, Size::new(20, 10));
    let cell = Cell::new("ｶﾞ");

    backend
        .draw(std::iter::once((2, 3, &cell)))
        .expect("halfwidth katakana cell should draw");

    assert_eq!(
        backend
            .get_cursor_position()
            .expect("cached cursor should be available"),
        Position::new(4, 3)
    );
}

#[test]
fn appended_lines_preserve_column_in_the_real_writer_and_cache() {
    let (mut backend, writer) = writer_backend(Position::new(4, 5), Size::new(20, 10));

    backend.append_lines(2).expect("lines should append");

    assert_eq!(
        backend
            .get_cursor_position()
            .expect("cached cursor should be available"),
        Position::new(4, 7)
    );
    assert_eq!(writer.snapshot(), b"\n\n\x1b[8;5H");
}

#[test]
fn scrolling_regions_home_the_real_writer_and_cache() {
    let (mut up, up_writer) = writer_backend(Position::new(7, 6), Size::new(20, 10));
    up.scroll_region_up(2..6, 2)
        .expect("region should scroll up");
    assert_eq!(
        up.get_cursor_position()
            .expect("cached cursor should be available"),
        Position::ORIGIN
    );
    assert_eq!(up_writer.snapshot(), b"\x1b[3;6r\x1b[2S\x1b[r\x1b[1;1H");

    let (mut down, down_writer) = writer_backend(Position::new(7, 6), Size::new(20, 10));
    down.scroll_region_down(2..6, 1)
        .expect("region should scroll down");
    assert_eq!(
        down.get_cursor_position()
            .expect("cached cursor should be available"),
        Position::ORIGIN
    );
    assert_eq!(down_writer.snapshot(), b"\x1b[3;6r\x1b[1T\x1b[r\x1b[1;1H");
}

#[test]
fn clear_region_preserves_the_real_writer_cursor_and_cache() {
    let (mut backend, writer) = writer_backend(Position::ORIGIN, Size::new(20, 10));
    backend
        .set_cursor_position(Position::new(4, 5))
        .expect("cursor should move");

    backend
        .clear_region(ClearType::CurrentLine)
        .expect("current line should clear");

    assert_eq!(
        backend
            .get_cursor_position()
            .expect("cached cursor should be available"),
        Position::new(4, 5)
    );
    assert_eq!(writer.snapshot(), b"\x1b[6;5H\x1b[2K");
}

#[test]
fn refreshed_size_bounds_the_normalized_cursor_after_resize() {
    let inner = TestBackend::new(20, 10);
    let mut backend = CachedCursorBackend::with_position(inner, Position::new(8, 7));
    backend.size().expect("initial size should be available");
    backend.inner.resize(5, 4);
    assert_eq!(
        backend.size().expect("resized dimensions should refresh"),
        Size::new(5, 4)
    );
    let cell = Cell::new("x");

    backend
        .draw(std::iter::once((4, 3, &cell)))
        .expect("edge cell should draw after resize");

    assert_eq!(
        backend
            .get_cursor_position()
            .expect("cached cursor should be available"),
        Position::new(4, 3)
    );
}

#[test]
fn cursor_reads_clamp_stale_positions_after_terminal_shrink() {
    let inner = TestBackend::new(20, 10);
    let mut backend = CachedCursorBackend::with_position(inner, Position::new(18, 8));
    backend.size().expect("initial size should be available");
    backend.inner.resize(5, 4);
    backend.size().expect("resized dimensions should refresh");

    assert_eq!(
        backend
            .get_cursor_position()
            .expect("cached cursor should be clamped to the resized terminal"),
        Position::new(4, 3)
    );
    assert_eq!(
        backend
            .inner
            .get_cursor_position()
            .expect("inner cursor should match the normalized cache"),
        Position::new(4, 3)
    );
}

#[test]
fn cursor_writes_clamp_to_the_known_terminal_bounds() {
    let inner = TestBackend::new(5, 4);
    let mut backend = CachedCursorBackend::with_position(inner, Position::ORIGIN);

    backend
        .set_cursor_position(Position::new(99, 88))
        .expect("out-of-bounds cursor write should be normalized");

    assert_eq!(
        backend
            .get_cursor_position()
            .expect("normalized cursor should be available"),
        Position::new(4, 3)
    );
    assert_eq!(
        backend
            .inner
            .get_cursor_position()
            .expect("inner cursor should match the normalized cache"),
        Position::new(4, 3)
    );
}
