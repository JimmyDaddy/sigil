use std::collections::VecDeque;

use super::{AppState, ComposerPasteSpan};

const MAX_EDIT_STEPS: usize = 100;
const MAX_EDIT_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone)]
pub(super) struct ComposerEditSnapshot {
    input: String,
    cursor: usize,
    selection_anchor: Option<usize>,
    paste_spans: Vec<ComposerPasteSpan>,
}

impl ComposerEditSnapshot {
    fn retained_bytes(&self) -> usize {
        self.input.len().saturating_add(
            self.paste_spans
                .len()
                .saturating_mul(std::mem::size_of::<ComposerPasteSpan>()),
        )
    }
}

#[derive(Debug, Clone, Default)]
pub(super) struct ComposerEditHistory {
    undo: VecDeque<ComposerEditSnapshot>,
    redo: VecDeque<ComposerEditSnapshot>,
}

impl ComposerEditHistory {
    fn record(&mut self, snapshot: ComposerEditSnapshot) {
        self.redo.clear();
        self.undo.push_back(snapshot);
        self.trim();
    }

    fn trim(&mut self) {
        while self.undo.len() + self.redo.len() > MAX_EDIT_STEPS
            || self
                .undo
                .iter()
                .chain(&self.redo)
                .map(ComposerEditSnapshot::retained_bytes)
                .sum::<usize>()
                > MAX_EDIT_BYTES
        {
            if self.undo.pop_front().is_none() {
                let _ = self.redo.pop_front();
            }
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct ComposerEditContext {
    snapshot: ComposerEditSnapshot,
    history: ComposerEditHistory,
    cleared_draft: Option<String>,
    kill_buffer: Option<String>,
}

impl AppState {
    fn composer_edit_snapshot(&self) -> ComposerEditSnapshot {
        ComposerEditSnapshot {
            input: self.composer.input.clone(),
            cursor: self.composer.input_cursor,
            selection_anchor: self.composer.input_selection_anchor,
            paste_spans: self.composer.input_paste_spans.clone(),
        }
    }

    fn bounded_composer_edit_snapshot(&self) -> Option<ComposerEditSnapshot> {
        let bytes = self.composer.input.len().saturating_add(
            self.composer
                .input_paste_spans
                .len()
                .saturating_mul(std::mem::size_of::<ComposerPasteSpan>()),
        );
        (bytes <= MAX_EDIT_BYTES).then(|| self.composer_edit_snapshot())
    }

    fn apply_composer_edit_snapshot(&mut self, snapshot: ComposerEditSnapshot) {
        self.composer.input = snapshot.input;
        self.composer.input_cursor = snapshot.cursor;
        self.composer.input_selection_anchor = snapshot.selection_anchor;
        self.composer.input_paste_spans = snapshot.paste_spans;
        self.clamp_input_cursor();
    }

    pub(super) fn edit_composer_input(&mut self, edit: impl FnOnce(&mut Self)) {
        let before = self.bounded_composer_edit_snapshot();
        edit(self);
        match before {
            Some(before) if before.input != self.composer.input => {
                self.composer.edit_history.record(before);
            }
            None => self.composer.edit_history = ComposerEditHistory::default(),
            _ => {}
        }
    }

    pub(super) fn undo_composer_edit(&mut self) -> bool {
        let Some(previous) = self.composer.edit_history.undo.pop_back() else {
            if self.restore_cleared_input_draft() {
                self.reset_input_history_navigation();
                self.reset_slash_selector();
                self.last_notice = Some("draft restored".to_owned());
                return true;
            }
            return false;
        };
        let cleared = self.composer.input.is_empty() && self.composer.cleared_input_draft.is_some();
        if let Some(current) = self.bounded_composer_edit_snapshot() {
            self.composer.edit_history.redo.push_back(current);
            self.composer.edit_history.trim();
        } else {
            self.composer.edit_history.redo.clear();
        }
        self.apply_composer_edit_snapshot(previous);
        self.discard_cleared_input_draft();
        self.reset_input_history_navigation();
        self.reset_slash_selector();
        self.last_notice = Some(
            if cleared {
                "draft restored"
            } else {
                "edit undone"
            }
            .to_owned(),
        );
        true
    }

    pub(super) fn redo_composer_edit(&mut self) -> bool {
        let Some(next) = self.composer.edit_history.redo.pop_back() else {
            return false;
        };
        if let Some(current) = self.bounded_composer_edit_snapshot() {
            self.composer.edit_history.undo.push_back(current);
            self.composer.edit_history.trim();
        } else {
            self.composer.edit_history.undo.clear();
        }
        self.apply_composer_edit_snapshot(next);
        self.discard_cleared_input_draft();
        self.reset_input_history_navigation();
        self.reset_slash_selector();
        self.last_notice = Some("edit redone".to_owned());
        true
    }

    pub(super) fn capture_composer_edit_context(&self) -> ComposerEditContext {
        ComposerEditContext {
            snapshot: self.composer_edit_snapshot(),
            history: self.composer.edit_history.clone(),
            cleared_draft: self.composer.cleared_input_draft.clone(),
            kill_buffer: self.composer.input_kill_buffer.clone(),
        }
    }

    pub(super) fn restore_composer_edit_context(&mut self, context: ComposerEditContext) {
        self.apply_composer_edit_snapshot(context.snapshot);
        self.composer.edit_history = context.history;
        self.composer.cleared_input_draft = context.cleared_draft;
        self.composer.input_kill_buffer = context.kill_buffer;
    }

    pub(super) fn reset_composer_edit_context(&mut self) {
        self.composer.edit_history = ComposerEditHistory::default();
        self.discard_cleared_input_draft();
        self.composer.input_kill_buffer = None;
        self.reset_input_history_navigation();
    }
}

#[cfg(test)]
#[path = "tests/composer_edit_history_tests.rs"]
mod tests;
