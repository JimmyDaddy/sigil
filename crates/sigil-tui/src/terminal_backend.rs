use std::{cell::Cell as StateCell, io, ops::Range};

use ratatui::{
    backend::{Backend, ClearType, WindowSize},
    buffer::{Cell as BufferCell, CellWidth},
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
    terminal_size: StateCell<Option<Size>>,
}

impl<B: Backend> CachedCursorBackend<B> {
    pub(crate) fn capture(mut inner: B) -> Result<Self, B::Error> {
        let cursor_position = inner.get_cursor_position()?;
        Ok(Self {
            inner,
            cursor_position,
            terminal_size: StateCell::new(None),
        })
    }

    pub(crate) fn with_position(inner: B, cursor_position: Position) -> Self {
        Self {
            inner,
            cursor_position,
            terminal_size: StateCell::new(None),
        }
    }

    fn known_terminal_size(&self) -> Result<Size, B::Error> {
        if let Some(size) = self.terminal_size.get() {
            return Ok(size);
        }
        let size = self.inner.size()?;
        self.terminal_size.set(Some(size));
        Ok(size)
    }

    fn normalize_cursor_position(&mut self, position: Position) -> Result<(), B::Error> {
        self.inner.set_cursor_position(position)?;
        self.cursor_position = position;
        Ok(())
    }
}

fn clamp_cursor_position(position: Position, size: Size) -> Position {
    Position::new(
        position.x.min(size.width.saturating_sub(1)),
        position.y.min(size.height.saturating_sub(1)),
    )
}

fn cursor_position_after_cell(x: u16, y: u16, cell: &BufferCell, size: Size) -> Position {
    let symbol_width = usize::from(cell.cell_width());
    let next_x = usize::from(x)
        .saturating_add(symbol_width)
        .min(usize::from(size.width.saturating_sub(1))) as u16;
    clamp_cursor_position(Position::new(next_x, y), size)
}

impl<B: Backend> Backend for CachedCursorBackend<B> {
    type Error = B::Error;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a BufferCell)>,
    {
        let size = self.known_terminal_size()?;
        let mut cursor_position = None;
        self.inner.draw(content.inspect(|(x, y, cell)| {
            cursor_position = Some(cursor_position_after_cell(*x, *y, cell, size));
        }))?;
        if let Some(position) = cursor_position {
            // Backend::draw does not define a final cursor position. Normalize it explicitly so
            // cached reads remain exact without racing Crossterm's process-global input reader.
            self.normalize_cursor_position(position)?;
        }
        Ok(())
    }

    fn append_lines(&mut self, n: u16) -> Result<(), Self::Error> {
        let size = self.known_terminal_size()?;
        self.inner.append_lines(n)?;
        let position = clamp_cursor_position(
            Position::new(
                self.cursor_position.x,
                self.cursor_position.y.saturating_add(n),
            ),
            size,
        );
        // Crossterm appends LF bytes. LF advances the row but does not guarantee column zero;
        // writing the computed position makes that postcondition deterministic across terminals.
        self.normalize_cursor_position(position)
    }

    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        self.inner.hide_cursor()
    }

    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        self.inner.show_cursor()
    }

    fn get_cursor_position(&mut self) -> Result<Position, Self::Error> {
        let position = clamp_cursor_position(self.cursor_position, self.known_terminal_size()?);
        if position != self.cursor_position {
            self.normalize_cursor_position(position)?;
        }
        Ok(position)
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> Result<(), Self::Error> {
        let position = clamp_cursor_position(position.into(), self.known_terminal_size()?);
        self.normalize_cursor_position(position)
    }

    fn clear(&mut self) -> Result<(), Self::Error> {
        self.inner.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> Result<(), Self::Error> {
        self.inner.clear_region(clear_type)
    }

    fn size(&self) -> Result<Size, Self::Error> {
        let size = self.inner.size()?;
        self.terminal_size.set(Some(size));
        Ok(size)
    }

    fn window_size(&mut self) -> Result<WindowSize, Self::Error> {
        let size = self.inner.window_size()?;
        self.terminal_size.set(Some(size.columns_rows));
        Ok(size)
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        self.inner.flush()
    }

    fn scroll_region_up(&mut self, region: Range<u16>, line_count: u16) -> Result<(), Self::Error> {
        self.inner.scroll_region_up(region, line_count)?;
        let position = if line_count == 0 {
            self.cursor_position
        } else {
            Position::ORIGIN
        };
        // Ratatui leaves the post-scroll cursor undefined. Crossterm's DECSTBM reset homes it;
        // make that postcondition explicit so the wrapped backend and cache cannot diverge.
        self.normalize_cursor_position(position)
    }

    fn scroll_region_down(
        &mut self,
        region: Range<u16>,
        line_count: u16,
    ) -> Result<(), Self::Error> {
        self.inner.scroll_region_down(region, line_count)?;
        let position = if line_count == 0 {
            self.cursor_position
        } else {
            Position::ORIGIN
        };
        self.normalize_cursor_position(position)
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
