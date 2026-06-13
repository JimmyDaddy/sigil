use unicode_width::UnicodeWidthChar;

use super::{AppState, char_to_byte_index, formatting::sidebar_width_for_terminal};

impl AppState {
    pub fn input_cursor_visual_position(&self) -> (u16, u16) {
        let width = self.composer_wrap_width();
        let (row, column) = self.visual_position_for_cursor(self.input_cursor, width);
        (column as u16, row as u16)
    }

    pub(crate) fn composer_input_rows(&self) -> u16 {
        self.input_last_visual_row().saturating_add(1) as u16
    }

    pub fn composer_height(&self) -> u16 {
        self.composer_input_rows().saturating_add(4).max(5)
    }

    pub(super) fn input_char_len(&self) -> usize {
        self.input.chars().count()
    }

    pub(super) fn set_input_and_cursor(&mut self, input: String) {
        self.input = input;
        self.input_cursor = self.input_char_len();
    }

    pub(super) fn clamp_input_cursor(&mut self) {
        self.input_cursor = self.input_cursor.min(self.input_char_len());
    }

    pub(super) fn input_last_visual_row(&self) -> usize {
        self.visual_position_for_cursor(self.input_char_len(), self.composer_wrap_width())
            .0
    }

    pub(super) fn input_cursor_visual_row(&self) -> usize {
        self.visual_position_for_cursor(self.input_cursor, self.composer_wrap_width())
            .0
    }

    pub(super) fn insert_input_character(&mut self, character: char) {
        let byte_index = char_to_byte_index(&self.input, self.input_cursor);
        self.input.insert(byte_index, character);
        self.input_cursor += 1;
    }

    pub(super) fn remove_input_character_before_cursor(&mut self) {
        if self.input_cursor == 0 {
            return;
        }
        let end = char_to_byte_index(&self.input, self.input_cursor);
        let start = char_to_byte_index(&self.input, self.input_cursor - 1);
        self.input.replace_range(start..end, "");
        self.input_cursor -= 1;
    }

    pub(super) fn move_input_cursor_left(&mut self) {
        self.input_cursor = self.input_cursor.saturating_sub(1);
    }

    pub(super) fn move_input_cursor_right(&mut self) {
        self.input_cursor = (self.input_cursor + 1).min(self.input_char_len());
    }

    pub(super) fn move_input_cursor_home(&mut self) {
        self.input_cursor = 0;
    }

    pub(super) fn move_input_cursor_end(&mut self) {
        self.input_cursor = self.input_char_len();
    }

    pub(super) fn move_input_cursor_vertical(&mut self, up: bool) -> bool {
        let width = self.composer_wrap_width();
        let (row, column) = self.visual_position_for_cursor(self.input_cursor, width);
        if up {
            if row == 0 {
                return false;
            }
            self.input_cursor = self.cursor_for_visual_position(row - 1, column, width);
            return true;
        }

        let next = self.cursor_for_visual_position(row + 1, column, width);
        if next == self.input_cursor {
            return false;
        }
        self.input_cursor = next;
        true
    }

    pub(super) fn visual_position_for_cursor(&self, cursor: usize, width: usize) -> (usize, usize) {
        let width = width.max(1);
        let mut row = 0usize;
        let mut column = 0usize;
        for (index, character) in self.input.chars().enumerate() {
            if index == cursor {
                break;
            }
            if character == '\n' {
                row += 1;
                column = 0;
                continue;
            }
            let char_width = UnicodeWidthChar::width(character).unwrap_or(1).max(1);
            if column + char_width > width {
                row += 1;
                column = 0;
            }
            column += char_width;
            if column >= width {
                row += column / width;
                column %= width;
            }
        }
        (row, column)
    }

    pub(super) fn cursor_for_visual_position(
        &self,
        target_row: usize,
        target_column: usize,
        width: usize,
    ) -> usize {
        let width = width.max(1);
        let mut best_index = self.input_char_len();
        let mut best_distance = usize::MAX;
        for index in 0..=self.input_char_len() {
            let (row, column) = self.visual_position_for_cursor(index, width);
            if row < target_row {
                continue;
            }
            if row > target_row {
                break;
            }
            let distance = column.abs_diff(target_column);
            if distance <= best_distance {
                best_index = index;
                best_distance = distance;
            } else {
                break;
            }
        }
        best_index
    }

    pub(super) fn record_input_history(&mut self, prompt: String) {
        if self
            .input_history
            .last()
            .map(|last| last == &prompt)
            .unwrap_or(false)
        {
            return;
        }
        self.input_history.push(prompt);
        if self.input_history.len() > 100 {
            self.input_history.remove(0);
        }
    }

    pub(super) fn reset_input_history_navigation(&mut self) {
        self.input_history_index = None;
        self.input_history_draft = None;
    }

    pub(super) fn navigate_input_history(&mut self, older: bool) {
        if self.input_history.is_empty() {
            return;
        }

        if older {
            match self.input_history_index {
                Some(0) => {}
                Some(index) => {
                    self.input_history_index = Some(index - 1);
                }
                None => {
                    self.input_history_draft = Some(self.input.clone());
                    self.input_history_index = Some(self.input_history.len() - 1);
                }
            }
        } else {
            match self.input_history_index {
                Some(index) if index + 1 < self.input_history.len() => {
                    self.input_history_index = Some(index + 1);
                }
                Some(_) => {
                    let draft = self.input_history_draft.take().unwrap_or_default();
                    self.set_input_and_cursor(draft);
                    self.input_history_index = None;
                    self.reset_slash_selector();
                    return;
                }
                None => return,
            }
        }

        if let Some(index) = self.input_history_index
            && let Some(value) = self.input_history.get(index)
        {
            self.set_input_and_cursor(value.clone());
            self.reset_slash_selector();
        }
    }

    fn composer_wrap_width(&self) -> usize {
        let total_width = self.terminal_width.max(24) as usize;
        let sidebar_width = sidebar_width_for_terminal(total_width);
        let composer_width = total_width.saturating_sub(sidebar_width).max(12);
        composer_width.saturating_sub(6).max(1)
    }
}
