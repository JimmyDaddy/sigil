use std::ops::Range;

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
        self.discard_cleared_input_draft();
        let byte_index = char_to_byte_index(&self.input, self.input_cursor);
        self.input.insert(byte_index, character);
        self.input_cursor += 1;
    }

    pub(super) fn insert_input_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.discard_cleared_input_draft();
        let byte_index = char_to_byte_index(&self.input, self.input_cursor);
        self.input.insert_str(byte_index, text);
        self.input_cursor += text.chars().count();
    }

    pub(super) fn remove_input_character_before_cursor(&mut self) {
        if self.input_cursor == 0 {
            return;
        }
        self.remove_input_range(self.input_cursor - 1..self.input_cursor);
    }

    pub(super) fn remove_input_character_at_cursor(&mut self) {
        if self.input_cursor >= self.input_char_len() {
            return;
        }
        self.remove_input_range(self.input_cursor..self.input_cursor + 1);
    }

    pub(super) fn remove_input_word_before_cursor(&mut self) {
        let start = self.previous_input_word_start();
        if start == self.input_cursor {
            return;
        }
        self.kill_input_range(start..self.input_cursor);
    }

    pub(super) fn remove_input_word_after_cursor(&mut self) {
        let end = self.next_input_word_end();
        if end == self.input_cursor {
            return;
        }
        self.kill_input_range(self.input_cursor..end);
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

    pub(super) fn move_input_cursor_line_start(&mut self) {
        self.input_cursor = self.current_input_line_start();
    }

    pub(super) fn move_input_cursor_line_end(&mut self) {
        self.input_cursor = self.current_input_line_end();
    }

    pub(super) fn move_input_cursor_word_left(&mut self) {
        self.input_cursor = self.previous_input_word_start();
    }

    pub(super) fn move_input_cursor_word_right(&mut self) {
        self.input_cursor = self.next_input_word_end();
    }

    pub(super) fn kill_input_to_line_end(&mut self) {
        let line_end = self.current_input_line_end();
        let range = if self.input_cursor < line_end {
            Some(self.input_cursor..line_end)
        } else if line_end < self.input_char_len() {
            Some(line_end..line_end + 1)
        } else {
            None
        };
        if let Some(range) = range {
            self.kill_input_range(range);
        }
    }

    pub(super) fn yank_input_kill_buffer(&mut self) {
        if let Some(text) = self.input_kill_buffer.clone() {
            self.insert_input_text(&text);
        }
    }

    pub(super) fn clear_input_preserving_draft(&mut self) {
        if !self.input.is_empty() {
            self.cleared_input_draft = Some(self.input.clone());
        }
        self.input.clear();
        self.input_cursor = 0;
    }

    pub(super) fn restore_cleared_input_draft(&mut self) -> bool {
        if !self.input.is_empty() {
            return false;
        }
        let Some(draft) = self.cleared_input_draft.take() else {
            return false;
        };
        self.set_input_and_cursor(draft);
        true
    }

    pub(super) fn discard_cleared_input_draft(&mut self) {
        self.cleared_input_draft = None;
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

    fn current_input_line_start(&self) -> usize {
        self.input
            .chars()
            .take(self.input_cursor)
            .enumerate()
            .filter_map(|(index, character)| (character == '\n').then_some(index + 1))
            .last()
            .unwrap_or(0)
    }

    fn current_input_line_end(&self) -> usize {
        self.input
            .chars()
            .enumerate()
            .skip(self.input_cursor)
            .find_map(|(index, character)| (character == '\n').then_some(index))
            .unwrap_or_else(|| self.input_char_len())
    }

    fn previous_input_word_start(&self) -> usize {
        let characters = self.input.chars().collect::<Vec<_>>();
        let mut index = self.input_cursor.min(characters.len());
        while index > 0 && !is_input_word_character(characters[index - 1]) {
            index -= 1;
        }
        while index > 0 && is_input_word_character(characters[index - 1]) {
            index -= 1;
        }
        index
    }

    fn next_input_word_end(&self) -> usize {
        let characters = self.input.chars().collect::<Vec<_>>();
        let mut index = self.input_cursor.min(characters.len());
        while index < characters.len() && !is_input_word_character(characters[index]) {
            index += 1;
        }
        while index < characters.len() && is_input_word_character(characters[index]) {
            index += 1;
        }
        index
    }

    fn remove_input_range(&mut self, range: Range<usize>) -> String {
        let removed = self.replace_input_range(range, "");
        if !removed.is_empty() {
            self.discard_cleared_input_draft();
        }
        removed
    }

    fn kill_input_range(&mut self, range: Range<usize>) {
        let removed = self.remove_input_range(range);
        if !removed.is_empty() {
            self.input_kill_buffer = Some(removed);
        }
    }

    fn replace_input_range(&mut self, range: Range<usize>, replacement: &str) -> String {
        let input_len = self.input_char_len();
        let start = range.start.min(input_len);
        let end = range.end.min(input_len).max(start);
        let byte_start = char_to_byte_index(&self.input, start);
        let byte_end = char_to_byte_index(&self.input, end);
        let removed = self.input[byte_start..byte_end].to_owned();
        if byte_start == byte_end && replacement.is_empty() {
            return removed;
        }
        self.input.replace_range(byte_start..byte_end, replacement);
        self.input_cursor = start + replacement.chars().count();
        removed
    }

    fn composer_wrap_width(&self) -> usize {
        let total_width = self.terminal_width.max(24) as usize;
        let sidebar_width = sidebar_width_for_terminal(total_width);
        let composer_width = total_width.saturating_sub(sidebar_width).max(12);
        composer_width.saturating_sub(6).max(1)
    }
}

fn is_input_word_character(character: char) -> bool {
    character == '_' || character.is_alphanumeric()
}
