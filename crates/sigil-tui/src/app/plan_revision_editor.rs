use std::{cell::Cell, rc::Rc};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_segmentation::UnicodeSegmentation;

use super::PendingUserInputForm;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PlanRevisionEditorState {
    /// UTF-8 byte offset at a grapheme boundary in the authoritative form draft.
    pub(crate) cursor: usize,
    preferred_column: Option<usize>,
    pub(crate) submitting: bool,
    pub(crate) error: Option<String>,
    // Surface snapshots share the renderer's measured viewport with the input owner.
    pub(crate) viewport: Rc<Cell<(usize, usize)>>,
}

pub(crate) struct PlanRevisionEditorLayout {
    pub(crate) rows: Vec<String>,
    pub(crate) cursor_position: (usize, usize),
    positions: Vec<(usize, usize, usize)>,
}

impl PendingUserInputForm {
    pub(crate) fn is_plan_revision_editor(&self) -> bool {
        self.recovery_command.is_none()
            && self.request.as_ref().is_some_and(|request| {
                matches!(
                    request.source,
                    sigil_kernel::UserInputSourceV1::PlanRevision { .. }
                )
            })
            && matches!(
                self.view.questions.as_slice(),
                [sigil_kernel::UserInputQuestionV1 {
                    options,
                    multiple: false,
                    ..
                }] if options.is_empty()
            )
    }
}

impl PlanRevisionEditorState {
    pub(crate) fn insert(&mut self, value: &mut String, text: &str) {
        if self.submitting || text.is_empty() {
            return;
        }
        self.cursor = boundary_at_or_before(value, self.cursor);
        value.insert_str(self.cursor, text);
        self.cursor += text.len();
        // Inserting a combining character can join the following grapheme as well.
        self.cursor = boundary_at_or_after(value, self.cursor);
        self.preferred_column = None;
        self.error = None;
    }

    pub(crate) fn handle_key(&mut self, value: &mut String, key: KeyEvent) {
        if self.submitting {
            return;
        }
        self.cursor = boundary_at_or_before(value, self.cursor);
        let plain = key.modifiers.is_empty();
        match key.code {
            KeyCode::Left if plain => {
                self.cursor = previous_boundary(value, self.cursor);
            }
            KeyCode::Right if plain => {
                self.cursor = next_boundary(value, self.cursor);
            }
            KeyCode::Home if plain => {
                self.cursor = value[..self.cursor]
                    .rfind('\n')
                    .map_or(0, |index| index + 1);
            }
            KeyCode::End if plain => {
                self.cursor = value[self.cursor..]
                    .find('\n')
                    .map_or(value.len(), |index| self.cursor + index);
            }
            KeyCode::Home if key.modifiers == KeyModifiers::CONTROL => {
                self.cursor = 0;
            }
            KeyCode::End if key.modifiers == KeyModifiers::CONTROL => {
                self.cursor = value.len();
            }
            KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown if plain => {
                let distance = if matches!(key.code, KeyCode::PageUp | KeyCode::PageDown) {
                    self.viewport.get().1.max(1)
                } else {
                    1
                };
                self.move_vertical(
                    value,
                    matches!(key.code, KeyCode::Up | KeyCode::PageUp),
                    distance,
                );
                return;
            }
            KeyCode::Backspace if plain => {
                let start = previous_boundary(value, self.cursor);
                value.replace_range(start..self.cursor, "");
                self.cursor = start;
                self.error = None;
            }
            KeyCode::Delete if plain => {
                let end = next_boundary(value, self.cursor);
                value.replace_range(self.cursor..end, "");
                self.error = None;
            }
            KeyCode::Enter if key.modifiers == KeyModifiers::SHIFT => {
                self.insert(value, "\n");
                return;
            }
            KeyCode::Char('j' | 'J') if key.modifiers == KeyModifiers::CONTROL => {
                self.insert(value, "\n");
                return;
            }
            KeyCode::Char(character)
                if (plain || key.modifiers == KeyModifiers::SHIFT) && !character.is_control() =>
            {
                self.insert(value, &character.to_string());
                return;
            }
            _ => return,
        }
        self.preferred_column = None;
    }

    fn move_vertical(&mut self, value: &str, up: bool, distance: usize) {
        let width = match self.viewport.get().0 {
            0 => 80,
            width => width,
        };
        let layout = self.layout(value, width);
        let (row, column) = layout.cursor_position;
        let column = *self.preferred_column.get_or_insert(column);
        let target_row = if up {
            row.saturating_sub(distance)
        } else {
            row.saturating_add(distance)
                .min(layout.rows.len().saturating_sub(1))
        };
        if let Some((offset, _, _)) = layout
            .positions
            .iter()
            .filter(|(_, row, _)| *row == target_row)
            .min_by_key(|(offset, _, candidate)| {
                (candidate.abs_diff(column), std::cmp::Reverse(*offset))
            })
        {
            self.cursor = *offset;
        }
    }

    pub(crate) fn layout(&self, value: &str, width: usize) -> PlanRevisionEditorLayout {
        let width = width.max(1);
        let cursor = boundary_at_or_before(value, self.cursor);
        let mut rows = Vec::new();
        let mut current = String::new();
        let mut column = 0usize;
        let mut positions = Vec::new();
        for (offset, grapheme) in value.grapheme_indices(true) {
            if grapheme == "\n" {
                positions.push(normalized_position(offset, rows.len(), column, width));
                rows.push(std::mem::take(&mut current));
                column = 0;
                continue;
            }
            let raw_width = crate::ui::terminal_cell_width(grapheme);
            let grapheme_width = raw_width.min(width);
            if column > 0 && column.saturating_add(grapheme_width) > width {
                rows.push(std::mem::take(&mut current));
                column = 0;
            }
            positions.push(normalized_position(offset, rows.len(), column, width));
            current.push_str(if raw_width > width { "?" } else { grapheme });
            column = column.saturating_add(grapheme_width);
        }
        positions.push(normalized_position(value.len(), rows.len(), column, width));
        rows.push(current);
        let cursor_position = positions
            .iter()
            .find_map(|(offset, row, column)| (*offset == cursor).then_some((*row, *column)))
            .unwrap_or((0, 0));
        let last_row = positions.last().map_or(0, |(_, row, _)| *row);
        while rows.len() <= last_row {
            rows.push(String::new());
        }
        PlanRevisionEditorLayout {
            rows,
            cursor_position,
            positions,
        }
    }
}

fn normalized_position(
    offset: usize,
    row: usize,
    column: usize,
    width: usize,
) -> (usize, usize, usize) {
    if column >= width {
        (offset, row.saturating_add(1), 0)
    } else {
        (offset, row, column)
    }
}

fn previous_boundary(value: &str, cursor: usize) -> usize {
    value
        .grapheme_indices(true)
        .map(|(offset, _)| offset)
        .take_while(|offset| *offset < cursor)
        .last()
        .unwrap_or(0)
}

fn next_boundary(value: &str, cursor: usize) -> usize {
    value
        .grapheme_indices(true)
        .map(|(offset, _)| offset)
        .find(|offset| *offset > cursor)
        .unwrap_or(value.len())
}

fn boundary_at_or_before(value: &str, cursor: usize) -> usize {
    value
        .grapheme_indices(true)
        .map(|(offset, _)| offset)
        .chain(std::iter::once(value.len()))
        .take_while(|offset| *offset <= cursor)
        .last()
        .unwrap_or(0)
}

fn boundary_at_or_after(value: &str, cursor: usize) -> usize {
    value
        .grapheme_indices(true)
        .map(|(offset, _)| offset)
        .find(|offset| *offset >= cursor)
        .unwrap_or(value.len())
}

#[cfg(test)]
#[path = "tests/plan_revision_editor_tests.rs"]
mod tests;
