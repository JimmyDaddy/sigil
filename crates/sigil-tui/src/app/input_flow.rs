use std::ops::Range;

use crossterm::event::{KeyCode, KeyEvent};
use unicode_width::UnicodeWidthChar;

use super::{
    AppAction, AppState, ComposerPasteSpan, PaneFocus, char_to_byte_index,
    formatting::{normalize_command_prefix_character, sidebar_width_for_terminal},
    has_alt_without_control, has_control_without_alt, is_composer_newline_key,
    is_composer_submit_key, is_composer_text_key,
};

const LARGE_PASTE_COLLAPSE_THRESHOLD_CHARS: usize = 10_000;

impl AppState {
    pub fn input_cursor_visual_position(&self) -> (u16, u16) {
        let width = self.composer_wrap_width();
        let display_input = self.composer_display_input();
        let display_cursor = self.composer_display_cursor();
        let (row, column) = visual_position_for_cursor_in(&display_input, display_cursor, width);
        (column as u16, row as u16)
    }

    pub(crate) fn composer_input_rows(&self) -> u16 {
        self.display_input_last_visual_row().saturating_add(1) as u16
    }

    pub fn composer_height(&self) -> u16 {
        self.composer_input_rows().saturating_add(4).max(5)
    }

    pub(super) fn input_char_len(&self) -> usize {
        self.composer.input.chars().count()
    }

    pub(super) fn set_input_and_cursor(&mut self, input: String) {
        self.composer.input = input;
        self.composer.input_cursor = self.input_char_len();
        self.composer.input_paste_spans.clear();
    }

    pub(super) fn clamp_input_cursor(&mut self) {
        self.composer.input_cursor = self.composer.input_cursor.min(self.input_char_len());
    }

    pub(super) fn input_last_visual_row(&self) -> usize {
        self.visual_position_for_cursor(self.input_char_len(), self.composer_wrap_width())
            .0
    }

    fn display_input_last_visual_row(&self) -> usize {
        let display_input = self.composer_display_input();
        visual_position_for_cursor_in(
            &display_input,
            display_input.chars().count(),
            self.composer_wrap_width(),
        )
        .0
    }

    pub(super) fn input_cursor_visual_row(&self) -> usize {
        self.visual_position_for_cursor(self.composer.input_cursor, self.composer_wrap_width())
            .0
    }

    pub(super) fn insert_input_character(&mut self, character: char) {
        self.composer.input_paste_spans.clear();
        self.discard_cleared_input_draft();
        let byte_index = char_to_byte_index(&self.composer.input, self.composer.input_cursor);
        self.composer.input.insert(byte_index, character);
        self.composer.input_cursor += 1;
    }

    pub(super) fn insert_input_text(&mut self, text: &str) {
        self.composer.input_paste_spans.clear();
        self.insert_input_text_preserving_paste_spans(text);
    }

    fn insert_input_text_preserving_paste_spans(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.discard_cleared_input_draft();
        let byte_index = char_to_byte_index(&self.composer.input, self.composer.input_cursor);
        self.composer.input.insert_str(byte_index, text);
        self.composer.input_cursor += text.chars().count();
    }

    pub fn handle_paste_text(&mut self, text: &str) {
        let pasted = normalize_paste_text(text);
        if pasted.is_empty() {
            return;
        }

        if self.has_modal() {
            let outcome = self.handle_modal_paste_text(&pasted);
            self.apply_modal_outcome(outcome);
            return;
        }

        if self.is_setup_mode() {
            self.handle_setup_paste_text(&pasted);
            return;
        }
        if self.is_config_mode() {
            self.handle_config_paste_text(&pasted);
            return;
        }
        if self.approval.pending.is_some() {
            return;
        }

        self.active_pane = PaneFocus::Composer;
        self.blur_composer_aux_panels();
        self.insert_paste_text(&pasted);
        self.reset_input_history_navigation();
        self.reset_slash_selector();
    }

    fn insert_paste_text(&mut self, text: &str) {
        let start = self.composer.input_cursor;
        let char_count = text.chars().count();
        self.insert_input_text_preserving_paste_spans(text);
        if char_count < LARGE_PASTE_COLLAPSE_THRESHOLD_CHARS {
            return;
        }
        self.composer.input_paste_spans.push(ComposerPasteSpan {
            start,
            end: start + char_count,
            char_count,
            line_count: text.matches('\n').count() + 1,
        });
    }

    pub(super) fn handle_composer_key_event(
        &mut self,
        key: KeyEvent,
    ) -> anyhow::Result<Option<Option<AppAction>>> {
        match key.code {
            KeyCode::Char('z') | KeyCode::Char('Z')
                if self.active_pane == PaneFocus::Composer && has_control_without_alt(key) =>
            {
                if self.restore_cleared_input_draft() {
                    self.reset_input_history_navigation();
                    self.reset_slash_selector();
                    self.last_notice = Some("draft restored".to_owned());
                }
            }
            KeyCode::Char('a') | KeyCode::Char('A')
                if self.active_pane == PaneFocus::Composer && has_control_without_alt(key) =>
            {
                self.move_input_cursor_line_start();
            }
            KeyCode::Char('e') | KeyCode::Char('E')
                if self.active_pane == PaneFocus::Composer && has_control_without_alt(key) =>
            {
                self.move_input_cursor_line_end();
            }
            KeyCode::Char('b') | KeyCode::Char('B')
                if self.active_pane == PaneFocus::Composer && has_control_without_alt(key) =>
            {
                self.move_input_cursor_left();
            }
            KeyCode::Char('f') | KeyCode::Char('F')
                if self.active_pane == PaneFocus::Composer && has_control_without_alt(key) =>
            {
                self.move_input_cursor_right();
            }
            KeyCode::Char('h') | KeyCode::Char('H')
                if self.active_pane == PaneFocus::Composer && has_control_without_alt(key) =>
            {
                self.remove_input_character_before_cursor();
                self.reset_input_history_navigation();
                self.reset_slash_selector();
            }
            KeyCode::Char('w') | KeyCode::Char('W')
                if self.active_pane == PaneFocus::Composer && has_control_without_alt(key) =>
            {
                self.remove_input_word_before_cursor();
                self.reset_input_history_navigation();
                self.reset_slash_selector();
            }
            KeyCode::Char('k') | KeyCode::Char('K')
                if self.active_pane == PaneFocus::Composer && has_control_without_alt(key) =>
            {
                self.kill_input_to_line_end();
                self.reset_input_history_navigation();
                self.reset_slash_selector();
            }
            KeyCode::Char('y') | KeyCode::Char('Y')
                if self.active_pane == PaneFocus::Composer && has_control_without_alt(key) =>
            {
                self.yank_input_kill_buffer();
                self.reset_input_history_navigation();
                self.reset_slash_selector();
            }
            KeyCode::Char('j') | KeyCode::Char('J')
                if self.active_pane == PaneFocus::Composer && has_control_without_alt(key) =>
            {
                self.insert_input_character('\n');
                self.reset_input_history_navigation();
                self.reset_slash_selector();
            }
            KeyCode::Char('b') | KeyCode::Char('B')
                if self.active_pane == PaneFocus::Composer && has_alt_without_control(key) =>
            {
                self.move_input_cursor_word_left();
            }
            KeyCode::Char('f') | KeyCode::Char('F')
                if self.active_pane == PaneFocus::Composer && has_alt_without_control(key) =>
            {
                self.move_input_cursor_word_right();
            }
            KeyCode::Tab
                if self.active_pane == PaneFocus::Composer && self.has_slash_selector() =>
            {
                self.accept_slash_selector();
            }
            KeyCode::BackTab
                if self.active_pane == PaneFocus::Composer && self.has_slash_selector() =>
            {
                self.move_slash_selector(false);
            }
            KeyCode::Tab if self.active_pane == PaneFocus::Composer && key.modifiers.is_empty() => {
                self.focus_composer_queue_panel();
            }
            KeyCode::Up
                if self.active_pane == PaneFocus::Composer
                    && self.composer.input_history_index.is_some()
                    && key.modifiers.is_empty() =>
            {
                self.navigate_input_history(true);
            }
            KeyCode::Down
                if self.active_pane == PaneFocus::Composer
                    && self.composer.input_history_index.is_some()
                    && key.modifiers.is_empty() =>
            {
                self.navigate_input_history(false);
            }
            KeyCode::Up if self.active_pane == PaneFocus::Composer && self.has_slash_selector() => {
                self.move_slash_selector(false)
            }
            KeyCode::Down
                if self.active_pane == PaneFocus::Composer && self.has_slash_selector() =>
            {
                self.move_slash_selector(true)
            }
            KeyCode::Up if self.active_pane == PaneFocus::Composer => {
                if self.input_cursor_visual_row() == 0 {
                    self.navigate_input_history(true);
                } else {
                    self.move_input_cursor_vertical(true);
                }
            }
            KeyCode::Down if self.active_pane == PaneFocus::Composer => {
                if self.input_cursor_visual_row() == self.input_last_visual_row() {
                    self.navigate_input_history(false);
                } else {
                    self.move_input_cursor_vertical(false);
                }
            }
            KeyCode::Home if self.active_pane == PaneFocus::Composer => {
                self.move_input_cursor_home()
            }
            KeyCode::End if self.active_pane == PaneFocus::Composer => self.move_input_cursor_end(),
            KeyCode::Left
                if self.active_pane == PaneFocus::Composer
                    && (has_control_without_alt(key) || has_alt_without_control(key)) =>
            {
                self.move_input_cursor_word_left();
            }
            KeyCode::Right
                if self.active_pane == PaneFocus::Composer
                    && (has_control_without_alt(key) || has_alt_without_control(key)) =>
            {
                self.move_input_cursor_word_right();
            }
            KeyCode::Left if self.active_pane == PaneFocus::Composer => {
                self.move_input_cursor_left()
            }
            KeyCode::Right if self.active_pane == PaneFocus::Composer => {
                self.move_input_cursor_right()
            }
            KeyCode::Backspace
                if self.active_pane == PaneFocus::Composer
                    && (has_control_without_alt(key) || has_alt_without_control(key)) =>
            {
                self.remove_input_word_before_cursor();
                self.reset_input_history_navigation();
                self.reset_slash_selector();
            }
            KeyCode::Backspace => {
                self.active_pane = PaneFocus::Composer;
                self.blur_composer_aux_panels();
                self.remove_input_character_before_cursor();
                self.reset_input_history_navigation();
                self.reset_slash_selector();
            }
            KeyCode::Delete
                if self.active_pane == PaneFocus::Composer
                    && (has_control_without_alt(key) || has_alt_without_control(key)) =>
            {
                self.remove_input_word_after_cursor();
                self.reset_input_history_navigation();
                self.reset_slash_selector();
            }
            KeyCode::Delete if self.active_pane == PaneFocus::Composer => {
                self.remove_input_character_at_cursor();
                self.reset_input_history_navigation();
                self.reset_slash_selector();
            }
            _ if self.active_pane == PaneFocus::Composer && is_composer_newline_key(key) => {
                self.active_pane = PaneFocus::Composer;
                self.blur_composer_aux_panels();
                self.insert_input_character('\n');
                self.reset_input_history_navigation();
                self.reset_slash_selector();
            }
            _ if is_composer_submit_key(key) => {
                self.active_pane = PaneFocus::Composer;
                self.blur_composer_aux_panels();
                if self.should_accept_slash_selector_on_enter() {
                    self.accept_slash_selector();
                    return Ok(Some(None));
                }
                return self.submit_input().map(Some);
            }
            KeyCode::Char(character) if is_composer_text_key(key) => {
                self.active_pane = PaneFocus::Composer;
                self.blur_composer_aux_panels();
                let normalized = if normalize_command_prefix_character(character).is_some()
                    && self.composer.input.trim().is_empty()
                {
                    '/'
                } else {
                    character
                };
                self.insert_input_character(normalized);
                self.reset_input_history_navigation();
                self.reset_slash_selector();
            }
            _ => return Ok(None),
        }
        Ok(Some(None))
    }

    pub(crate) fn composer_display_input(&self) -> String {
        if self.composer.input_paste_spans.is_empty() {
            return self.composer.input.clone();
        }

        let mut display = String::new();
        let mut cursor = 0usize;
        for (index, span) in self.composer.input_paste_spans.iter().enumerate() {
            if span.start < cursor || span.end > self.input_char_len() {
                continue;
            }
            display.push_str(&input_char_range(&self.composer.input, cursor..span.start));
            display.push_str(&paste_span_placeholder(index + 1, span));
            cursor = span.end;
        }
        display.push_str(&input_char_range(
            &self.composer.input,
            cursor..self.input_char_len(),
        ));
        display
    }

    fn composer_display_cursor(&self) -> usize {
        if self.composer.input_paste_spans.is_empty() {
            return self.composer.input_cursor;
        }

        let mut adjustment = 0isize;
        for (index, span) in self.composer.input_paste_spans.iter().enumerate() {
            let placeholder_len = paste_span_placeholder(index + 1, span).chars().count();
            if self.composer.input_cursor <= span.start {
                break;
            }
            if self.composer.input_cursor < span.end {
                let span_start = (span.start as isize + adjustment).max(0) as usize;
                return span_start + placeholder_len;
            }
            adjustment += placeholder_len as isize - span.char_count as isize;
        }
        (self.composer.input_cursor as isize + adjustment).max(0) as usize
    }

    pub(super) fn remove_input_character_before_cursor(&mut self) {
        if self.composer.input_cursor == 0 {
            return;
        }
        self.composer.input_paste_spans.clear();
        self.remove_input_range(self.composer.input_cursor - 1..self.composer.input_cursor);
    }

    pub(super) fn remove_input_character_at_cursor(&mut self) {
        if self.composer.input_cursor >= self.input_char_len() {
            return;
        }
        self.composer.input_paste_spans.clear();
        self.remove_input_range(self.composer.input_cursor..self.composer.input_cursor + 1);
    }

    pub(super) fn remove_input_word_before_cursor(&mut self) {
        let start = self.previous_input_word_start();
        if start == self.composer.input_cursor {
            return;
        }
        self.kill_input_range(start..self.composer.input_cursor);
    }

    pub(super) fn remove_input_word_after_cursor(&mut self) {
        let end = self.next_input_word_end();
        if end == self.composer.input_cursor {
            return;
        }
        self.kill_input_range(self.composer.input_cursor..end);
    }

    pub(super) fn move_input_cursor_left(&mut self) {
        self.composer.input_paste_spans.clear();
        self.composer.input_cursor = self.composer.input_cursor.saturating_sub(1);
    }

    pub(super) fn move_input_cursor_right(&mut self) {
        self.composer.input_paste_spans.clear();
        self.composer.input_cursor = (self.composer.input_cursor + 1).min(self.input_char_len());
    }

    pub(super) fn move_input_cursor_home(&mut self) {
        self.composer.input_paste_spans.clear();
        self.composer.input_cursor = 0;
    }

    pub(super) fn move_input_cursor_end(&mut self) {
        self.composer.input_paste_spans.clear();
        self.composer.input_cursor = self.input_char_len();
    }

    pub(super) fn move_input_cursor_line_start(&mut self) {
        self.composer.input_paste_spans.clear();
        self.composer.input_cursor = self.current_input_line_start();
    }

    pub(super) fn move_input_cursor_line_end(&mut self) {
        self.composer.input_paste_spans.clear();
        self.composer.input_cursor = self.current_input_line_end();
    }

    pub(super) fn move_input_cursor_word_left(&mut self) {
        self.composer.input_paste_spans.clear();
        self.composer.input_cursor = self.previous_input_word_start();
    }

    pub(super) fn move_input_cursor_word_right(&mut self) {
        self.composer.input_paste_spans.clear();
        self.composer.input_cursor = self.next_input_word_end();
    }

    pub(super) fn kill_input_to_line_end(&mut self) {
        let line_end = self.current_input_line_end();
        let range = if self.composer.input_cursor < line_end {
            Some(self.composer.input_cursor..line_end)
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
        if let Some(text) = self.composer.input_kill_buffer.clone() {
            self.insert_input_text(&text);
        }
    }

    pub(super) fn clear_input_preserving_draft(&mut self) {
        if !self.composer.input.is_empty() {
            self.composer.cleared_input_draft = Some(self.composer.input.clone());
        }
        self.composer.input.clear();
        self.composer.input_cursor = 0;
        self.composer.input_paste_spans.clear();
    }

    pub(super) fn restore_cleared_input_draft(&mut self) -> bool {
        if !self.composer.input.is_empty() {
            return false;
        }
        let Some(draft) = self.composer.cleared_input_draft.take() else {
            return false;
        };
        self.set_input_and_cursor(draft);
        true
    }

    pub(super) fn discard_cleared_input_draft(&mut self) {
        self.composer.cleared_input_draft = None;
    }

    pub(super) fn move_input_cursor_vertical(&mut self, up: bool) -> bool {
        self.composer.input_paste_spans.clear();
        let width = self.composer_wrap_width();
        let (row, column) = self.visual_position_for_cursor(self.composer.input_cursor, width);
        if up {
            if row == 0 {
                return false;
            }
            self.composer.input_cursor = self.cursor_for_visual_position(row - 1, column, width);
            return true;
        }

        let next = self.cursor_for_visual_position(row + 1, column, width);
        if next == self.composer.input_cursor {
            return false;
        }
        self.composer.input_cursor = next;
        true
    }

    pub(super) fn visual_position_for_cursor(&self, cursor: usize, width: usize) -> (usize, usize) {
        visual_position_for_cursor_in(&self.composer.input, cursor, width)
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
        self.composer
            .input
            .chars()
            .take(self.composer.input_cursor)
            .enumerate()
            .filter_map(|(index, character)| (character == '\n').then_some(index + 1))
            .last()
            .unwrap_or(0)
    }

    fn current_input_line_end(&self) -> usize {
        self.composer
            .input
            .chars()
            .enumerate()
            .skip(self.composer.input_cursor)
            .find_map(|(index, character)| (character == '\n').then_some(index))
            .unwrap_or_else(|| self.input_char_len())
    }

    fn previous_input_word_start(&self) -> usize {
        let characters = self.composer.input.chars().collect::<Vec<_>>();
        let mut index = self.composer.input_cursor.min(characters.len());
        while index > 0 && !is_input_word_character(characters[index - 1]) {
            index -= 1;
        }
        while index > 0 && is_input_word_character(characters[index - 1]) {
            index -= 1;
        }
        index
    }

    fn next_input_word_end(&self) -> usize {
        let characters = self.composer.input.chars().collect::<Vec<_>>();
        let mut index = self.composer.input_cursor.min(characters.len());
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
            self.composer.input_kill_buffer = Some(removed);
        }
    }

    fn replace_input_range(&mut self, range: Range<usize>, replacement: &str) -> String {
        let input_len = self.input_char_len();
        let start = range.start.min(input_len);
        let end = range.end.min(input_len).max(start);
        let byte_start = char_to_byte_index(&self.composer.input, start);
        let byte_end = char_to_byte_index(&self.composer.input, end);
        let removed = self.composer.input[byte_start..byte_end].to_owned();
        if byte_start == byte_end && replacement.is_empty() {
            return removed;
        }
        self.composer.input_paste_spans.clear();
        self.composer
            .input
            .replace_range(byte_start..byte_end, replacement);
        self.composer.input_cursor = start + replacement.chars().count();
        removed
    }

    fn composer_wrap_width(&self) -> usize {
        let total_width = self.terminal_width.max(24) as usize;
        let sidebar_width = sidebar_width_for_terminal(total_width);
        let composer_width = total_width.saturating_sub(sidebar_width).max(12);
        composer_width.saturating_sub(6).max(1)
    }
}

fn normalize_paste_text(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn input_char_range(input: &str, range: Range<usize>) -> String {
    let byte_start = char_to_byte_index(input, range.start);
    let byte_end = char_to_byte_index(input, range.end);
    input[byte_start..byte_end].to_owned()
}

fn paste_span_placeholder(index: usize, span: &ComposerPasteSpan) -> String {
    format!(
        "[Pasted text #{index}: {} chars, {} lines]",
        span.char_count, span.line_count
    )
}

fn visual_position_for_cursor_in(input: &str, cursor: usize, width: usize) -> (usize, usize) {
    let width = width.max(1);
    let mut row = 0usize;
    let mut column = 0usize;
    for (index, character) in input.chars().enumerate() {
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

fn is_input_word_character(character: char) -> bool {
    character == '_' || character.is_alphanumeric()
}
