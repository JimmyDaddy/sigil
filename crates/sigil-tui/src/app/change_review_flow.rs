use std::thread::JoinHandle;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use sigil_application::{ApplicationCheckpointReview, ReviewAnnotation, ReviewDiffSide, SafeText};
use sigil_runtime::application_recovery::ApplicationCheckpointView;

use super::{AppAction, AppState, ModalState, PaneFocus, RunPhase, TimelineRole};

#[derive(Debug, PartialEq, Eq)]
enum Stage {
    Checkpoints,
    Diff,
    Comment,
}

#[derive(Debug)]
pub(super) struct ChangeReviewModalState {
    request_id: u64,
    session_id: String,
    stage: Stage,
    checkpoints: Vec<ApplicationCheckpointView>,
    checkpoint: usize,
    review: Option<ApplicationCheckpointReview>,
    file: usize,
    line: usize,
    anchor: Option<usize>,
    side: ReviewDiffSide,
    comment: String,
    annotations: Vec<ReviewAnnotation>,
    loading: bool,
    ready_prompt: Option<String>,
    submitted_prompt: Option<String>,
    submitted_intent: Option<std::sync::Arc<()>>,
    error: Option<String>,
}

#[derive(Debug)]
enum ReviewResult {
    Checkpoints(Vec<ApplicationCheckpointView>),
    Diff(ApplicationCheckpointReview),
    Prompt(String),
}

pub(super) struct ChangeReviewTask {
    request_id: u64,
    session_id: String,
    handle: Option<JoinHandle<Result<ReviewResult, String>>>,
}

impl std::fmt::Debug for ChangeReviewTask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChangeReviewTask")
            .field("request_id", &self.request_id)
            .finish_non_exhaustive()
    }
}

impl Drop for ChangeReviewTask {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl ChangeReviewModalState {
    pub(super) fn lines(&self) -> Vec<String> {
        let mut lines = vec![
            format!(
                "Recorded changes · {} comments · {}",
                self.annotations.len(),
                self.session_id
            ),
            "Review comments request another turn; they do not authorize writes or restore files."
                .to_owned(),
        ];
        if self.loading {
            lines.push("Reading and validating recorded changes…".to_owned());
        }
        if self.submitted_prompt.is_some() {
            lines.push(
                "Waiting for send/queue receipt. Your original draft is unchanged.".to_owned(),
            );
        }
        match self.stage {
            Stage::Checkpoints => {
                lines.push(
                    "↑/↓ choose turn · Enter review · Ctrl-S send comments · Esc close".to_owned(),
                );
                if !self.loading && self.checkpoints.is_empty() {
                    lines.push("No recorded checkpoints yet.".to_owned());
                }
                for (index, checkpoint) in self
                    .checkpoints
                    .iter()
                    .enumerate()
                    .skip(self.checkpoint.saturating_sub(3))
                    .take(8)
                {
                    lines.push(format!(
                        "{} Turn {} · {} recorded files",
                        if index == self.checkpoint { "›" } else { " " },
                        checkpoint.turn_index,
                        checkpoint.files.len()
                    ));
                }
            }
            Stage::Diff => {
                lines.push(
                    "←/→ file · ↑/↓ line · Shift-↑/↓ range · Tab old/new · Enter comment"
                        .to_owned(),
                );
                lines.push(
                    "B turns · Ctrl-S send batch (queues while running) · Esc close".to_owned(),
                );
                if let Some(review) = &self.review {
                    if review.truncated {
                        lines.push("Some recorded changes exceed the review budget.".to_owned());
                    }
                    if review.diffs.is_empty() {
                        lines.push("No recorded forward diff in this checkpoint.".to_owned());
                    }
                    if let Some(diff) = review.diffs.get(self.file) {
                        lines.push(format!(
                            "{} · {:?} now · {:?} side{}",
                            diff.path,
                            diff.file_state,
                            self.side,
                            if diff.truncated {
                                " · truncated diff"
                            } else {
                                ""
                            }
                        ));
                        let from = self.anchor.unwrap_or(self.line).min(self.line);
                        let to = self.anchor.unwrap_or(self.line).max(self.line);
                        for (index, line) in diff
                            .lines
                            .iter()
                            .enumerate()
                            .skip(self.line.saturating_sub(5))
                            .take(12)
                        {
                            lines.push(format!(
                                "{} {:>5} {:>5} {}",
                                if (from..=to).contains(&index) {
                                    "›"
                                } else {
                                    " "
                                },
                                line.old_line.map_or_else(String::new, |n| n.to_string()),
                                line.new_line.map_or_else(String::new, |n| n.to_string()),
                                line.text
                            ));
                        }
                    }
                }
            }
            Stage::Comment => {
                lines.push("Type/paste comment · Enter add to batch · Esc back to diff".to_owned());
                lines.extend(self.comment.lines().map(ToOwned::to_owned));
                if self.comment.is_empty() {
                    lines.push("Comment: ".to_owned());
                }
            }
        }
        if let Some(error) = &self.error {
            lines.push(format!("Review: {error}"));
        }
        lines
    }

    fn annotation(&self) -> anyhow::Result<ReviewAnnotation> {
        let review = self
            .review
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("choose a recorded change"))?;
        let diff = review
            .diffs
            .get(self.file)
            .ok_or_else(|| anyhow::anyhow!("choose a recorded file"))?;
        let from = self.anchor.unwrap_or(self.line).min(self.line);
        let to = self.anchor.unwrap_or(self.line).max(self.line);
        let numbers = diff
            .lines
            .iter()
            .skip(from)
            .take(to - from + 1)
            .filter_map(|line| match self.side {
                ReviewDiffSide::Old => line.old_line,
                ReviewDiffSide::New => line.new_line,
            })
            .collect::<Vec<_>>();
        let start_line = *numbers
            .first()
            .ok_or_else(|| anyhow::anyhow!("selected side has no source line"))?;
        let end_line = *numbers.last().expect("first checked");
        anyhow::ensure!(
            end_line - start_line < 200,
            "select at most 200 source lines"
        );
        anyhow::ensure!(
            !self.comment.trim().is_empty() && self.comment.len() <= 4096,
            "comment must contain 1–4096 bytes"
        );
        anyhow::ensure!(
            self.annotations.len() < 16,
            "send this batch before adding more comments"
        );
        Ok(ReviewAnnotation {
            checkpoint_id: review.checkpoint_id.clone(),
            checkpoint_digest: review.checkpoint_digest.clone(),
            source_call_id: diff.source_call_id.clone(),
            diff_digest: diff.diff_digest.clone(),
            path: diff.path.clone(),
            side: self.side,
            start_line,
            end_line,
            comment: SafeText::new(self.comment.clone())?,
        })
    }
}

impl AppState {
    pub(crate) fn change_review_modal_open(&self) -> bool {
        matches!(self.modal_state, Some(ModalState::ChangeReview(_)))
    }

    pub(super) fn open_change_review(&mut self) {
        if self.change_review_task.is_some() {
            self.last_notice = Some("previous review read is still finishing".to_owned());
            return;
        }
        let request_id = self.next_background_request_id();
        self.modal_state = Some(ModalState::ChangeReview(Box::new(ChangeReviewModalState {
            request_id,
            session_id: self.session_id.clone(),
            stage: Stage::Checkpoints,
            checkpoints: Vec::new(),
            checkpoint: 0,
            review: None,
            file: 0,
            line: 0,
            anchor: None,
            side: ReviewDiffSide::New,
            comment: String::new(),
            annotations: Vec::new(),
            loading: false,
            ready_prompt: None,
            submitted_prompt: None,
            submitted_intent: None,
            error: None,
        })));
        self.active_pane = PaneFocus::Activity;
        let path = self.session_log_path.clone();
        let scope = self.session_id.clone();
        self.start_change_review_task(move || {
            sigil_runtime::application_recovery::application_conversation_recovery_view(
                &path, &scope,
            )
            .map(|view| ReviewResult::Checkpoints(view.checkpoints))
            .map_err(|error| error.to_string())
        });
    }

    fn start_change_review_task(
        &mut self,
        work: impl FnOnce() -> Result<ReviewResult, String> + Send + 'static,
    ) {
        if self.change_review_task.is_some() {
            return;
        }
        let request_id = self.next_background_request_id();
        let Some(ModalState::ChangeReview(state)) = self.modal_state.as_mut() else {
            return;
        };
        state.request_id = request_id;
        state.loading = true;
        state.error = None;
        match std::thread::Builder::new()
            .name("sigil-change-review".to_owned())
            .spawn(work)
        {
            Ok(handle) => {
                self.change_review_task = Some(ChangeReviewTask {
                    request_id,
                    session_id: state.session_id.clone(),
                    handle: Some(handle),
                })
            }
            Err(error) => {
                state.loading = false;
                state.error = Some(error.to_string());
            }
        }
    }

    pub(super) fn poll_change_review(&mut self) -> bool {
        let Some(task) = self.change_review_task.as_ref() else {
            return false;
        };
        if task
            .handle
            .as_ref()
            .is_some_and(|handle| !handle.is_finished())
        {
            return false;
        }
        let mut task = self.change_review_task.take().expect("task checked");
        let result = task
            .handle
            .take()
            .expect("owned review handle")
            .join()
            .unwrap_or_else(|_| Err("review reader stopped unexpectedly".to_owned()));
        let Some(ModalState::ChangeReview(state)) = self.modal_state.as_mut() else {
            return true;
        };
        if state.request_id != task.request_id
            || state.session_id != task.session_id
            || self.session_id != task.session_id
        {
            return true;
        }
        state.loading = false;
        match result {
            Ok(ReviewResult::Checkpoints(checkpoints)) => {
                state.checkpoint = checkpoints.len().saturating_sub(1);
                state.checkpoints = checkpoints;
            }
            Ok(ReviewResult::Diff(review)) => {
                state.review = Some(review);
                state.stage = Stage::Diff;
                state.file = 0;
                state.line = 0;
                state.anchor = None;
            }
            Ok(ReviewResult::Prompt(prompt)) => state.ready_prompt = Some(prompt),
            Err(error) => state.error = Some(error),
        }
        true
    }

    pub(crate) fn take_change_review_submission(&mut self) -> Option<AppAction> {
        let Some(ModalState::ChangeReview(state)) = self.modal_state.as_mut() else {
            return None;
        };
        if state.session_id != self.session_id {
            state.ready_prompt = None;
            return None;
        }
        let prompt = state.ready_prompt.take()?;
        if self.composer.pending_user_input.is_some() {
            state.error = Some(
                "answer or cancel the pending input before sending review comments".to_owned(),
            );
            return None;
        }
        state.submitted_prompt = Some(prompt.clone());
        if self.runtime.is_busy {
            let (kind, target) = self.active_conversation_queue_submission();
            let safe_prompt = sigil_kernel::safe_persistence_text(&prompt);
            self.push_optimistic_conversation_queue_item(safe_prompt.clone(), kind, target.clone());
            self.push_optimistic_conversation_timeline_entry(safe_prompt, &target);
            return Some(AppAction::QueueConversationInput {
                prompt,
                kind,
                target,
            });
        }
        self.begin_run_submission_intent();
        if let Some(ModalState::ChangeReview(state)) = self.modal_state.as_mut() {
            state.submitted_intent =
                Some(std::sync::Arc::clone(&self.runtime.run_submission_intent));
        }
        self.runtime.is_busy = true;
        self.runtime.allow_projection_run_recovery = true;
        self.runtime.run_phase = RunPhase::Preparing;
        self.last_notice = Some("preparing review comments".to_owned());
        self.push_timeline(
            TimelineRole::User,
            sigil_kernel::safe_persistence_text(&prompt),
        );
        Some(AppAction::SubmitPrompt(prompt))
    }

    pub(crate) fn settle_change_review_submission(
        &mut self,
        action: &AppAction,
        error: Option<&str>,
    ) -> bool {
        let prompt = match action {
            AppAction::SubmitPrompt(prompt) | AppAction::QueueConversationInput { prompt, .. } => {
                prompt
            }
            _ => return false,
        };
        let Some(ModalState::ChangeReview(state)) = self.modal_state.as_mut() else {
            return false;
        };
        if state.session_id != self.session_id || state.submitted_prompt.as_ref() != Some(prompt) {
            return false;
        }
        if let Some(error) = error {
            state.submitted_prompt = None;
            state.submitted_intent = None;
            state.error = Some(error.to_owned());
        } else {
            self.modal_state = None;
            self.active_pane = PaneFocus::Composer;
        }
        true
    }

    pub(super) fn observe_change_review_run_message(
        &mut self,
        message: &crate::runner::WorkerMessage,
    ) {
        let Some(ModalState::ChangeReview(state)) = self.modal_state.as_mut() else {
            return;
        };
        if state.session_id != self.session_id
            || !state.submitted_intent.as_ref().is_some_and(|intent| {
                std::sync::Arc::ptr_eq(intent, &self.runtime.run_submission_intent)
            })
        {
            return;
        }
        match message {
            crate::runner::WorkerMessage::RunStarted { prompt }
                if state.submitted_prompt.as_ref().is_some_and(|submitted| {
                    sigil_kernel::safe_persistence_text(submitted) == *prompt
                }) =>
            {
                self.modal_state = None;
                self.active_pane = PaneFocus::Composer;
            }
            crate::runner::WorkerMessage::RunFailed(error) => {
                state.submitted_prompt = None;
                state.submitted_intent = None;
                state.error = Some(error.clone());
            }
            _ => {}
        }
    }

    pub(super) fn paste_change_review_comment(&mut self, text: &str) {
        if let Some(ModalState::ChangeReview(state)) = self.modal_state.as_mut()
            && state.stage == Stage::Comment
            && !state.loading
            && state.submitted_prompt.is_none()
        {
            let text = text
                .chars()
                .filter(|ch| !ch.is_control() || *ch == '\n' || *ch == '\t')
                .collect::<String>();
            if state.comment.len().saturating_add(text.len()) <= 4096 {
                state.comment.push_str(&text);
            } else {
                state.error =
                    Some("comment exceeds 4096 bytes; shorten it before adding".to_owned());
            }
        }
    }

    pub(super) fn handle_change_review_key(&mut self, key: KeyEvent) {
        let Some(ModalState::ChangeReview(state)) = self.modal_state.as_mut() else {
            return;
        };
        if state.submitted_prompt.is_some() {
            return;
        }
        if key.code == KeyCode::Esc {
            if state.stage == Stage::Comment {
                state.stage = Stage::Diff;
            } else {
                self.modal_state = None;
                self.active_pane = PaneFocus::Composer;
            }
            return;
        }
        if state.loading {
            return;
        }
        if key.code == KeyCode::Char('s') && key.modifiers == KeyModifiers::CONTROL {
            if state.stage == Stage::Comment && !state.comment.is_empty() {
                match state.annotation() {
                    Ok(annotation) => {
                        state.annotations.push(annotation);
                        state.comment.clear();
                        state.stage = Stage::Diff;
                    }
                    Err(error) => {
                        state.error = Some(error.to_string());
                        return;
                    }
                }
            }
            if state.annotations.is_empty() {
                state.error = Some("add at least one comment to the batch".to_owned());
                return;
            }
            let annotations = state.annotations.clone();
            let path = self.session_log_path.clone();
            let scope = self.session_id.clone();
            let root = self.workspace_root.clone();
            self.start_change_review_task(move || {
                sigil_runtime::materialize_queued_review_annotations(
                    &path,
                    &scope,
                    &root,
                    &annotations,
                )
                .map(ReviewResult::Prompt)
                .map_err(|error| error.to_string())
            });
            return;
        }
        if state.stage == Stage::Comment {
            match key.code {
                KeyCode::Enter
                    if key.modifiers.is_empty() || key.modifiers == KeyModifiers::CONTROL =>
                {
                    match state.annotation() {
                        Ok(annotation) => {
                            state.annotations.push(annotation);
                            state.comment.clear();
                            state.stage = Stage::Diff;
                            state.error = None;
                        }
                        Err(error) => state.error = Some(error.to_string()),
                    }
                }
                KeyCode::Backspace => {
                    state.comment.pop();
                }
                KeyCode::Char(ch)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    self.paste_change_review_comment(&ch.to_string())
                }
                _ => {}
            }
            return;
        }
        if state.stage == Stage::Checkpoints {
            match key.code {
                KeyCode::Up => state.checkpoint = state.checkpoint.saturating_sub(1),
                KeyCode::Down => {
                    state.checkpoint =
                        (state.checkpoint + 1).min(state.checkpoints.len().saturating_sub(1))
                }
                KeyCode::Enter => {
                    let Some(checkpoint) = state.checkpoints.get(state.checkpoint).cloned() else {
                        return;
                    };
                    let path = self.session_log_path.clone();
                    let scope = self.session_id.clone();
                    let root = self.workspace_root.clone();
                    self.start_change_review_task(move || {
                        sigil_runtime::application_checkpoint_review(
                            &path,
                            &scope,
                            &root,
                            &checkpoint.checkpoint_id,
                            &checkpoint.checkpoint_digest,
                        )
                        .map(ReviewResult::Diff)
                        .map_err(|error| error.to_string())
                    });
                }
                _ => {}
            }
            return;
        }
        let Some(review) = &state.review else {
            return;
        };
        match key.code {
            KeyCode::Char('b' | 'B') => state.stage = Stage::Checkpoints,
            KeyCode::Left | KeyCode::Right => {
                state.file = if key.code == KeyCode::Left {
                    state.file.saturating_sub(1)
                } else {
                    (state.file + 1).min(review.diffs.len().saturating_sub(1))
                };
                state.line = 0;
                state.anchor = None;
            }
            KeyCode::Up | KeyCode::Down => {
                if key.modifiers == KeyModifiers::SHIFT {
                    state.anchor.get_or_insert(state.line);
                } else {
                    state.anchor = None;
                }
                let count = review
                    .diffs
                    .get(state.file)
                    .map_or(0, |diff| diff.lines.len());
                state.line = if key.code == KeyCode::Up {
                    state.line.saturating_sub(1)
                } else {
                    (state.line + 1).min(count.saturating_sub(1))
                };
            }
            KeyCode::Tab => {
                state.side = match state.side {
                    ReviewDiffSide::New => ReviewDiffSide::Old,
                    ReviewDiffSide::Old => ReviewDiffSide::New,
                }
            }
            KeyCode::Enter => {
                state.stage = Stage::Comment;
                state.error = None;
            }
            _ => {}
        }
    }
}
