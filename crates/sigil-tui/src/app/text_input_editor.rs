use std::{collections::VecDeque, ops::Range};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct TextCursor {
    pub(super) position: usize,
    anchor: Option<usize>,
}

impl TextCursor {
    pub(super) fn at_end(text: &str) -> Self {
        Self {
            position: text.chars().count(),
            anchor: None,
        }
    }

    pub(super) fn selection(&self) -> Option<Range<usize>> {
        self.anchor
            .filter(|anchor| *anchor != self.position)
            .map(|anchor| anchor.min(self.position)..anchor.max(self.position))
    }

    pub(super) fn move_for_key(&mut self, text: &str, key: KeyEvent) -> bool {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let extending = key.modifiers.contains(KeyModifiers::SHIFT);
        let end = text.chars().count();
        let next = match key.code {
            KeyCode::Home => 0,
            KeyCode::End => end,
            KeyCode::Char('a' | 'A') if control => 0,
            KeyCode::Char('e' | 'E') if control => end,
            KeyCode::Left if !extending && self.selection().is_some() => {
                self.selection().expect("selection checked").start
            }
            KeyCode::Right if !extending && self.selection().is_some() => {
                self.selection().expect("selection checked").end
            }
            KeyCode::Left => previous_boundary(text, self.position),
            KeyCode::Right => next_boundary(text, self.position),
            _ => return false,
        };
        if extending {
            self.anchor.get_or_insert(self.position);
        } else {
            self.anchor = None;
        }
        self.position = next;
        true
    }

    pub(super) fn replace(&mut self, text: &mut String, value: &str) {
        let range = self.selection().unwrap_or(self.position..self.position);
        let start = byte_offset(text, range.start);
        let end = byte_offset(text, range.end);
        text.replace_range(start..end, value);
        let inserted_end = range.start + value.chars().count();
        self.position = boundary_at_or_after(text, inserted_end);
        self.anchor = None;
    }

    pub(super) fn delete_for_key(&mut self, text: &mut String, key: KeyEvent) -> bool {
        let range = match key.code {
            KeyCode::Char('u' | 'U') if key.modifiers == KeyModifiers::CONTROL => {
                0..text.chars().count()
            }
            KeyCode::Backspace => self
                .selection()
                .unwrap_or_else(|| previous_boundary(text, self.position)..self.position),
            KeyCode::Delete => self
                .selection()
                .unwrap_or_else(|| self.position..next_boundary(text, self.position)),
            _ => return false,
        };
        self.position = range.start;
        self.anchor = Some(range.end);
        self.replace(text, "");
        true
    }
}

#[derive(Debug, Clone)]
struct EditSnapshot {
    text: String,
    cursor: TextCursor,
}

#[derive(Debug, Clone, Default)]
pub(super) struct TextInputEditor {
    pub(super) cursor: TextCursor,
    undo: VecDeque<EditSnapshot>,
    redo: VecDeque<EditSnapshot>,
}

impl TextInputEditor {
    pub(super) fn new(text: &str) -> Self {
        Self {
            cursor: TextCursor::at_end(text),
            ..Self::default()
        }
    }

    fn record(&mut self, text: &str) {
        self.redo.clear();
        self.undo.push_back(EditSnapshot {
            text: text.to_owned(),
            cursor: self.cursor.clone(),
        });
        self.trim();
    }

    fn trim(&mut self) {
        while self.undo.len() + self.redo.len() > 100
            || self
                .undo
                .iter()
                .chain(&self.redo)
                .map(|entry| entry.text.len())
                .sum::<usize>()
                > 4 * 1024 * 1024
        {
            if self.undo.pop_front().is_none() {
                let _ = self.redo.pop_front();
            }
        }
    }

    pub(super) fn insert(&mut self, text: &mut String, value: &str) {
        if value.is_empty() {
            return;
        }
        self.record(text);
        self.cursor.replace(text, value);
    }

    pub(super) fn handle_key(&mut self, text: &mut String, key: KeyEvent) -> bool {
        if self.cursor.move_for_key(text, key) {
            return true;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('z' | 'Z' | 'y' | 'Y'))
        {
            let redo = matches!(key.code, KeyCode::Char('y' | 'Y'))
                || key.modifiers.contains(KeyModifiers::SHIFT);
            let source = if redo { &mut self.redo } else { &mut self.undo };
            if let Some(snapshot) = source.pop_back() {
                let current = EditSnapshot {
                    text: std::mem::replace(text, snapshot.text),
                    cursor: self.cursor.clone(),
                };
                self.cursor = snapshot.cursor;
                if redo {
                    self.undo.push_back(current);
                } else {
                    self.redo.push_back(current);
                }
                self.trim();
            }
            return true;
        }
        if !(matches!(key.code, KeyCode::Backspace | KeyCode::Delete)
            || matches!(key.code, KeyCode::Char('u' | 'U'))
                && key.modifiers == KeyModifiers::CONTROL)
        {
            return false;
        }
        let before = EditSnapshot {
            text: text.clone(),
            cursor: self.cursor.clone(),
        };
        if !self.cursor.delete_for_key(text, key) {
            return false;
        }
        if before.text != *text {
            self.redo.clear();
            self.undo.push_back(before);
            self.trim();
        }
        true
    }
}

fn byte_offset(text: &str, position: usize) -> usize {
    text.char_indices()
        .nth(position)
        .map_or(text.len(), |(offset, _)| offset)
}

fn previous_boundary(text: &str, position: usize) -> usize {
    grapheme_boundaries(text)
        .take_while(|offset| *offset < position)
        .last()
        .unwrap_or(0)
}

fn next_boundary(text: &str, position: usize) -> usize {
    grapheme_boundaries(text)
        .find(|offset| *offset > position)
        .unwrap_or_else(|| text.chars().count())
}

fn boundary_at_or_after(text: &str, position: usize) -> usize {
    grapheme_boundaries(text)
        .find(|offset| *offset >= position)
        .unwrap_or_else(|| text.chars().count())
}

fn grapheme_boundaries(text: &str) -> impl Iterator<Item = usize> + '_ {
    text.graphemes(true).scan(0, |position, grapheme| {
        let start = *position;
        *position += grapheme.chars().count();
        Some(start)
    })
}

#[cfg(test)]
#[path = "tests/text_input_editor_tests.rs"]
mod tests;
