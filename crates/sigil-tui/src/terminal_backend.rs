use std::{io, ops::Range};

use ratatui::{
    backend::{Backend, ClearType, WindowSize},
    buffer::Cell,
    layout::{Position, Size},
};

/// Backend adapter that captures the terminal cursor before asynchronous input starts.
///
/// Crossterm has one process-global terminal input reader. Once its `EventStream` is active,
/// asking the underlying backend for the cursor position competes with that reader for the
/// terminal's cursor-position response. Ratatui can ask again while resizing an inline viewport,
/// so all later reads must be served from state already observed or written by this adapter.
pub(crate) struct CachedCursorBackend<B> {
    inner: B,
    cursor_position: Position,
}

impl<B: Backend> CachedCursorBackend<B> {
    pub(crate) fn capture(mut inner: B) -> Result<Self, B::Error> {
        let cursor_position = inner.get_cursor_position()?;
        Ok(Self {
            inner,
            cursor_position,
        })
    }

    pub(crate) fn with_position(inner: B, cursor_position: Position) -> Self {
        Self {
            inner,
            cursor_position,
        }
    }
}

impl<B: Backend> Backend for CachedCursorBackend<B> {
    type Error = B::Error;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        let cursor_position = &mut self.cursor_position;
        self.inner.draw(content.inspect(|(x, y, _)| {
            cursor_position.x = x.saturating_add(1);
            cursor_position.y = *y;
        }))
    }

    fn append_lines(&mut self, n: u16) -> Result<(), Self::Error> {
        let size = self.inner.size()?;
        self.inner.append_lines(n)?;
        self.cursor_position.x = 0;
        self.cursor_position.y = self
            .cursor_position
            .y
            .saturating_add(n)
            .min(size.height.saturating_sub(1));
        Ok(())
    }

    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        self.inner.hide_cursor()
    }

    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        self.inner.show_cursor()
    }

    fn get_cursor_position(&mut self) -> Result<Position, Self::Error> {
        Ok(self.cursor_position)
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> Result<(), Self::Error> {
        let position = position.into();
        self.inner.set_cursor_position(position)?;
        self.cursor_position = position;
        Ok(())
    }

    fn clear(&mut self) -> Result<(), Self::Error> {
        self.inner.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> Result<(), Self::Error> {
        self.inner.clear_region(clear_type)
    }

    fn size(&self) -> Result<Size, Self::Error> {
        self.inner.size()
    }

    fn window_size(&mut self) -> Result<WindowSize, Self::Error> {
        self.inner.window_size()
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        self.inner.flush()
    }

    fn scroll_region_up(&mut self, region: Range<u16>, line_count: u16) -> Result<(), Self::Error> {
        self.inner.scroll_region_up(region, line_count)
    }

    fn scroll_region_down(
        &mut self,
        region: Range<u16>,
        line_count: u16,
    ) -> Result<(), Self::Error> {
        self.inner.scroll_region_down(region, line_count)
    }
}

impl<B: io::Write> io::Write for CachedCursorBackend<B> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
#[path = "tests/terminal_backend_tests.rs"]
mod tests;
