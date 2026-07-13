use std::collections::{BTreeMap, BTreeSet};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::json;
use sigil_kernel::{
    CheckpointRestoreConflictReason, ControlEntry, ControlledCheckpointProjection,
    ControlledCheckpointRestoreKind, ControlledCheckpointRestorePreview,
    ControlledCheckpointRestoreRequest, JsonlSessionStore, SessionLogEntry, SessionStreamRecord,
    TypedDomainEvent,
};

use super::{
    AppAction, AppState, ModalState, PaneFocus, TimelineRole,
    formatting::truncate_session_view_text,
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum CheckpointModalOperation {
    #[default]
    Idle,
    Previewing,
    Restoring,
    Forking,
}

#[derive(Debug, Default)]
pub(super) struct CheckpointRestoreModalState {
    context: CheckpointDisplayContext,
    pub(super) operation: CheckpointModalOperation,
    pub(super) error: Option<String>,
    pub(super) scroll: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CheckpointRestoreModalPhase {
    Loading,
    Ready,
    Blocked,
    Restoring,
    Forking,
    Unavailable,
}

#[derive(Debug)]
pub(crate) struct CheckpointRestoreModalView {
    pub(crate) phase: CheckpointRestoreModalPhase,
    pub(crate) phase_detail: String,
    pub(crate) summary_lines: Vec<String>,
    pub(crate) body_title: &'static str,
    pub(crate) body_status: String,
    pub(crate) body_notice_lines: Vec<String>,
    pub(crate) body_lines: Vec<String>,
    pub(crate) body_is_diff: bool,
    pub(crate) scroll: u16,
    pub(crate) can_restore: bool,
    pub(crate) can_fork: bool,
    pub(crate) error: Option<String>,
}

impl AppState {
    pub(super) fn open_checkpoint_restore_modal(&mut self) -> Option<AppAction> {
        if self.approval.pending.is_some() {
            self.last_notice =
                Some("finish the pending approval before opening checkpoint restore".to_owned());
            return None;
        }
        if self.runtime.is_busy {
            self.last_notice = Some("wait for the active run before checkpoint restore".to_owned());
            return None;
        }
        self.blur_verification_card();
        self.blur_composer_aux_panels();
        self.modal_state = Some(ModalState::CheckpointRestore(
            CheckpointRestoreModalState::default(),
        ));
        if self.checkpoint_action_pending
            && self.checkpoint_expected_request.is_some()
            && let Some(state) = self.checkpoint_restore_modal_state_mut()
        {
            state.operation = CheckpointModalOperation::Previewing;
        }
        self.request_checkpoint_restore_preview()
    }

    pub(crate) fn checkpoint_restore_modal_open(&self) -> bool {
        matches!(self.modal_state, Some(ModalState::CheckpointRestore(_)))
    }

    pub(super) fn checkpoint_request_matches(&self, request_id: u64) -> bool {
        self.checkpoint_request_id == Some(request_id)
    }

    pub(super) fn handle_checkpoint_restore_modal_key_event(
        &mut self,
        key: KeyEvent,
    ) -> Option<AppAction> {
        if key.code == KeyCode::Char('r') && key.modifiers == KeyModifiers::CONTROL {
            return self.request_checkpoint_restore_preview();
        }
        if matches!(key.code, KeyCode::Char('f' | 'F'))
            && matches!(key.modifiers, KeyModifiers::NONE | KeyModifiers::SHIFT)
        {
            return self.fork_conversation_from_checkpoint();
        }
        if key.modifiers != KeyModifiers::NONE {
            return None;
        }
        match key.code {
            KeyCode::Enter => self.execute_checkpoint_restore(),
            KeyCode::Esc => {
                if self.checkpoint_mutation_pending() {
                    self.last_notice = Some(
                        "checkpoint operation is already applying and cannot be dismissed"
                            .to_owned(),
                    );
                    return None;
                }
                self.close_checkpoint_restore_modal();
                None
            }
            KeyCode::Up => {
                self.scroll_checkpoint_restore_modal(true, 1);
                None
            }
            KeyCode::Down => {
                self.scroll_checkpoint_restore_modal(false, 1);
                None
            }
            KeyCode::PageUp => {
                self.scroll_checkpoint_restore_modal(true, 8);
                None
            }
            KeyCode::PageDown => {
                self.scroll_checkpoint_restore_modal(false, 8);
                None
            }
            KeyCode::Home => {
                if let Some(state) = self.checkpoint_restore_modal_state_mut() {
                    state.scroll = 0;
                }
                None
            }
            KeyCode::End => {
                let max_scroll = self.checkpoint_restore_max_scroll();
                if let Some(state) = self.checkpoint_restore_modal_state_mut() {
                    state.scroll = max_scroll.min(u16::MAX as usize) as u16;
                }
                None
            }
            _ => None,
        }
    }

    pub(super) fn apply_checkpoint_restore_preview(
        &mut self,
        request_id: u64,
        preview: ControlledCheckpointRestorePreview,
    ) {
        let response_matches = self.checkpoint_request_id == Some(request_id)
            && self
                .checkpoint_expected_request
                .as_ref()
                .is_some_and(|request| {
                    request.checkpoint_id == preview.checkpoint_id
                        && request.checkpoint_digest == preview.checkpoint_digest
                });
        if !response_matches {
            self.push_event(
                "checkpoint",
                format!("ignored stale preview response {request_id}"),
            );
            return;
        }
        self.checkpoint_action_pending = false;
        if !self.checkpoint_restore_modal_open() {
            self.checkpoint_restore_preview = None;
            self.checkpoint_expected_request = None;
            self.checkpoint_request_id = None;
            return;
        }
        let file_count = preview.files.len();
        let context = self.checkpoint_display_context(&preview);
        if let Some(state) = self.checkpoint_restore_modal_state_mut() {
            state.context = context;
            state.operation = CheckpointModalOperation::Idle;
            state.error = None;
            state.scroll = 0;
        }
        if preview.ready {
            self.last_notice = Some(format!(
                "restore preview ready for {file_count} file(s); review the diff and press Enter to restore"
            ));
        } else {
            self.last_notice = Some("checkpoint restore blocked by preflight conflicts".to_owned());
        }
        self.checkpoint_restore_preview = Some(preview);
    }

    pub(super) fn apply_checkpoint_restore_completed(
        &mut self,
        request_id: u64,
        preview: &ControlledCheckpointRestorePreview,
    ) -> bool {
        if self.checkpoint_request_id != Some(request_id) {
            return false;
        }
        self.checkpoint_action_pending = false;
        self.checkpoint_restore_preview = None;
        self.checkpoint_expected_request = None;
        self.checkpoint_request_id = None;
        if self.checkpoint_restore_modal_open() {
            self.modal_state = None;
        }
        self.active_pane = PaneFocus::Composer;
        self.push_timeline(TimelineRole::Tool, checkpoint_restored_payload(preview));
        self.timeline_scroll_back = 0;
        self.last_notice = Some(format!(
            "restored {} controlled file(s); verification is stale",
            preview.files.len()
        ));
        true
    }

    pub(super) fn clear_checkpoint_interaction(&mut self) {
        self.checkpoint_action_pending = false;
        self.checkpoint_restore_preview = None;
        self.checkpoint_expected_request = None;
        self.checkpoint_request_id = None;
        if self.checkpoint_restore_modal_open() {
            self.modal_state = None;
        }
    }

    pub(super) fn apply_checkpoint_operation_failed(
        &mut self,
        request_id: u64,
        error: &str,
    ) -> bool {
        if self.checkpoint_request_id != Some(request_id) {
            return false;
        }
        if !self.checkpoint_action_pending || !self.checkpoint_restore_modal_open() {
            self.clear_checkpoint_interaction();
            return false;
        }
        self.checkpoint_action_pending = false;
        self.checkpoint_expected_request = None;
        self.checkpoint_request_id = None;
        let preview_failed = matches!(
            self.modal_state,
            Some(ModalState::CheckpointRestore(CheckpointRestoreModalState {
                operation: CheckpointModalOperation::Previewing,
                ..
            }))
        );
        if preview_failed {
            self.checkpoint_restore_preview = None;
        }
        if let Some(state) = self.checkpoint_restore_modal_state_mut() {
            if preview_failed {
                state.context = CheckpointDisplayContext::default();
            }
            state.operation = CheckpointModalOperation::Idle;
            state.error = Some(error.to_owned());
        }
        true
    }

    fn request_checkpoint_restore_preview(&mut self) -> Option<AppAction> {
        if self.runtime.is_busy {
            self.set_checkpoint_modal_error("wait for the active run before checkpoint restore");
            return None;
        }
        if self.checkpoint_action_pending {
            self.last_notice = Some("checkpoint operation already in progress".to_owned());
            return None;
        }
        let request = match self.latest_checkpoint_request() {
            Ok(request) => request,
            Err(error) => {
                self.checkpoint_restore_preview = None;
                self.set_checkpoint_modal_error(&error);
                return None;
            }
        };
        self.checkpoint_restore_preview = None;
        self.checkpoint_expected_request = Some(request.clone());
        let request_id = self.next_background_request_id();
        self.checkpoint_request_id = Some(request_id);
        self.checkpoint_action_pending = true;
        if let Some(state) = self.checkpoint_restore_modal_state_mut() {
            state.context = CheckpointDisplayContext::default();
            state.operation = CheckpointModalOperation::Previewing;
            state.error = None;
            state.scroll = 0;
        }
        self.last_notice = Some(format!(
            "building exact restore preview for {}",
            short_checkpoint_id(&request.checkpoint_id)
        ));
        self.push_event("checkpoint", "restore modal opened; preview requested");
        Some(AppAction::PreviewCheckpointRestore {
            request_id,
            request,
        })
    }

    fn execute_checkpoint_restore(&mut self) -> Option<AppAction> {
        if self.checkpoint_action_pending {
            self.last_notice = Some("checkpoint operation already in progress".to_owned());
            return None;
        }
        let Some(preview) = self.checkpoint_restore_preview.as_ref() else {
            self.last_notice = Some("restore preview is not ready yet".to_owned());
            return None;
        };
        if !preview.ready {
            self.last_notice =
                Some("checkpoint restore is blocked by preflight conflicts".to_owned());
            return None;
        }
        let request = ControlledCheckpointRestoreRequest {
            checkpoint_id: preview.checkpoint_id.clone(),
            checkpoint_digest: preview.checkpoint_digest.clone(),
        };
        self.checkpoint_expected_request = Some(request.clone());
        let request_id = self.next_background_request_id();
        self.checkpoint_request_id = Some(request_id);
        self.checkpoint_action_pending = true;
        if let Some(state) = self.checkpoint_restore_modal_state_mut() {
            state.operation = CheckpointModalOperation::Restoring;
            state.error = None;
        }
        self.last_notice = Some("checkpoint restore requested".to_owned());
        Some(AppAction::ExecuteCheckpointRestore {
            request_id,
            request,
        })
    }

    fn fork_conversation_from_checkpoint(&mut self) -> Option<AppAction> {
        if self.runtime.is_busy {
            self.last_notice = Some("wait for the active run before conversation fork".to_owned());
            return None;
        }
        if self.checkpoint_action_pending {
            self.last_notice = Some("checkpoint operation already in progress".to_owned());
            return None;
        }
        let Some(preview) = self.checkpoint_restore_preview.as_ref() else {
            self.last_notice = Some("wait for the exact restore preview before forking".to_owned());
            return None;
        };
        let request = ControlledCheckpointRestoreRequest {
            checkpoint_id: preview.checkpoint_id.clone(),
            checkpoint_digest: preview.checkpoint_digest.clone(),
        };
        self.checkpoint_expected_request = Some(request.clone());
        let request_id = self.next_background_request_id();
        self.checkpoint_request_id = Some(request_id);
        self.checkpoint_action_pending = true;
        if let Some(state) = self.checkpoint_restore_modal_state_mut() {
            state.operation = CheckpointModalOperation::Forking;
            state.error = None;
        }
        self.last_notice = Some(
            "conversation fork requested; workspace files stay shared and unchanged".to_owned(),
        );
        Some(AppAction::ForkConversationAtCheckpoint {
            request_id,
            request,
        })
    }

    pub(crate) fn checkpoint_restore_modal_view(&self) -> Option<CheckpointRestoreModalView> {
        let ModalState::CheckpointRestore(state) = self.modal_state.as_ref()? else {
            return None;
        };
        let preview = self.checkpoint_restore_preview.as_ref();
        let phase = match state.operation {
            CheckpointModalOperation::Previewing => CheckpointRestoreModalPhase::Loading,
            CheckpointModalOperation::Restoring => CheckpointRestoreModalPhase::Restoring,
            CheckpointModalOperation::Forking => CheckpointRestoreModalPhase::Forking,
            CheckpointModalOperation::Idle if state.error.is_some() => {
                CheckpointRestoreModalPhase::Unavailable
            }
            CheckpointModalOperation::Idle if preview.is_some_and(|preview| preview.ready) => {
                CheckpointRestoreModalPhase::Ready
            }
            CheckpointModalOperation::Idle if preview.is_some() => {
                CheckpointRestoreModalPhase::Blocked
            }
            CheckpointModalOperation::Idle => CheckpointRestoreModalPhase::Unavailable,
        };
        let phase_detail = match phase {
            CheckpointRestoreModalPhase::Loading => {
                "Rebuilding exact state from the durable log".to_owned()
            }
            CheckpointRestoreModalPhase::Ready => {
                "Exact preview ready; review the reverse diff before restoring".to_owned()
            }
            CheckpointRestoreModalPhase::Blocked => {
                "Restore is blocked; review the file conflicts below".to_owned()
            }
            CheckpointRestoreModalPhase::Restoring => {
                "Restoring controlled files; this operation cannot be dismissed".to_owned()
            }
            CheckpointRestoreModalPhase::Forking => {
                "Creating a conversation fork; workspace files stay unchanged".to_owned()
            }
            CheckpointRestoreModalPhase::Unavailable => {
                "No exact restore preview is available".to_owned()
            }
        };
        let mut summary_lines = Vec::new();
        if let Some(turn_index) = state.context.turn_index {
            summary_lines.push(format!("Target: state before turn {turn_index}"));
        }
        if let Some(prompt) = state.context.prompt.as_deref() {
            summary_lines.push(format!("Prompt: {prompt}"));
        }
        if let Some(preview) = preview {
            let blocked_count = preview
                .files
                .iter()
                .filter(|file| file.conflict_reason.is_some())
                .count();
            summary_lines.push(format!(
                "Controlled files: {} · {} ready · {blocked_count} blocked",
                preview.files.len(),
                preview.files.len().saturating_sub(blocked_count)
            ));
            if blocked_count > 0 {
                let mut conflicts = BTreeMap::new();
                for reason in preview.files.iter().filter_map(|file| file.conflict_reason) {
                    *conflicts
                        .entry(conflict_reason_label(reason))
                        .or_insert(0usize) += 1;
                }
                summary_lines.push(format!(
                    "Conflicts: {}",
                    conflicts
                        .into_iter()
                        .map(|(reason, count)| format!("{count} {reason}"))
                        .collect::<Vec<_>>()
                        .join(" · ")
                ));
            }
            summary_lines.push(if preview.unknown_mutation_count > 0 {
                format!(
                    "Boundary: excludes {} unknown shell/remote side effect(s)",
                    preview.unknown_mutation_count
                )
            } else {
                "Boundary: shell and remote side effects are not restored".to_owned()
            });
        }

        let mut body_lines = state
            .context
            .reverse_diffs
            .iter()
            .flat_map(|diff| {
                diff.lines
                    .iter()
                    .cloned()
                    .chain(std::iter::once(String::new()))
            })
            .collect::<Vec<_>>();
        if body_lines.last().is_some_and(String::is_empty) {
            body_lines.pop();
        }
        let body_notice_lines = preview
            .into_iter()
            .flat_map(|preview| preview.files.iter())
            .filter_map(|file| {
                file.conflict_reason.map(|reason| {
                    format!(
                        "Blocked: {} · {}",
                        file.path.display(),
                        conflict_reason_label(reason)
                    )
                })
            })
            .collect::<Vec<_>>();
        let body_is_diff = !body_lines.is_empty();
        if !body_is_diff && let Some(preview) = preview {
            body_lines.extend(preview.files.iter().map(|file| {
                let path = file.path.display().to_string();
                let target_hash = state
                    .context
                    .target_hashes
                    .iter()
                    .find_map(|(candidate, hash)| (candidate == &path).then_some(hash.as_deref()))
                    .flatten()
                    .map(short_hash)
                    .unwrap_or("file absent");
                let current_hash = file
                    .actual_current_hash
                    .as_deref()
                    .map(short_hash)
                    .unwrap_or("file absent");
                format!("{path} · current {current_hash} -> restored {target_hash}")
            }));
        }
        if body_lines.is_empty() {
            body_lines.push(match phase {
                CheckpointRestoreModalPhase::Loading => "Loading reverse diff...".to_owned(),
                _ => "No recorded reverse diff is available.".to_owned(),
            });
        }
        let diff_count = state.context.reverse_diffs.len();
        let body_status = if body_is_diff {
            let truncated = state
                .context
                .reverse_diffs
                .iter()
                .any(|diff| diff.truncated);
            let recorded_lines = state
                .context
                .reverse_diffs
                .iter()
                .map(|diff| diff.original_line_count)
                .sum::<usize>();
            format!(
                "{diff_count} file diff(s) · {recorded_lines} recorded line(s){} · Up/Down or PgUp/PgDn scroll",
                if truncated { " · bounded preview" } else { "" }
            )
        } else {
            "Current and restore-target hashes; no durable line diff was recorded".to_owned()
        };

        Some(CheckpointRestoreModalView {
            phase,
            phase_detail,
            summary_lines,
            body_title: if body_is_diff {
                "Reverse diff"
            } else {
                "Restore evidence"
            },
            body_status,
            body_notice_lines,
            body_lines,
            body_is_diff,
            scroll: state.scroll,
            can_restore: phase == CheckpointRestoreModalPhase::Ready,
            can_fork: preview.is_some()
                && matches!(
                    phase,
                    CheckpointRestoreModalPhase::Ready | CheckpointRestoreModalPhase::Blocked
                ),
            error: state.error.clone(),
        })
    }

    pub(super) fn scroll_checkpoint_restore_modal(&mut self, upward: bool, amount: usize) {
        let max_scroll = self.checkpoint_restore_max_scroll();
        let Some(state) = self.checkpoint_restore_modal_state_mut() else {
            return;
        };
        let current = usize::from(state.scroll).min(max_scroll);
        let next = if upward {
            current.saturating_sub(amount)
        } else {
            current.saturating_add(amount).min(max_scroll)
        };
        state.scroll = next.min(u16::MAX as usize) as u16;
    }

    pub(crate) fn checkpoint_mutation_pending(&self) -> bool {
        matches!(
            self.modal_state,
            Some(ModalState::CheckpointRestore(CheckpointRestoreModalState {
                operation: CheckpointModalOperation::Restoring | CheckpointModalOperation::Forking,
                ..
            }))
        )
    }

    fn checkpoint_restore_modal_state_mut(&mut self) -> Option<&mut CheckpointRestoreModalState> {
        let Some(ModalState::CheckpointRestore(state)) = self.modal_state.as_mut() else {
            return None;
        };
        Some(state)
    }

    fn checkpoint_restore_max_scroll(&self) -> usize {
        let Some(view) = self.checkpoint_restore_modal_view() else {
            return 0;
        };
        crate::ui::checkpoint_restore_max_scroll(self.terminal_width, self.terminal_height, &view)
    }

    fn set_checkpoint_modal_error(&mut self, error: &str) {
        self.checkpoint_action_pending = false;
        self.checkpoint_expected_request = None;
        self.checkpoint_request_id = None;
        if let Some(state) = self.checkpoint_restore_modal_state_mut() {
            state.operation = CheckpointModalOperation::Idle;
            state.error = Some(error.to_owned());
        }
        self.last_notice = Some(error.to_owned());
    }

    fn close_checkpoint_restore_modal(&mut self) {
        self.modal_state = None;
        self.checkpoint_restore_preview = None;
        if !self.checkpoint_action_pending {
            self.checkpoint_expected_request = None;
            self.checkpoint_request_id = None;
        }
        self.active_pane = PaneFocus::Composer;
        self.last_notice = Some("checkpoint restore closed".to_owned());
        self.push_event("checkpoint", "restore modal closed");
    }

    fn checkpoint_display_context(
        &self,
        preview: &ControlledCheckpointRestorePreview,
    ) -> CheckpointDisplayContext {
        let Ok(records) = JsonlSessionStore::read_event_records(&self.session_log_path) else {
            return CheckpointDisplayContext::default();
        };
        let Ok(projection) = ControlledCheckpointProjection::from_records(&records) else {
            return CheckpointDisplayContext::default();
        };
        let Some(checkpoint) = projection.checkpoints.iter().find(|checkpoint| {
            checkpoint.checkpoint_id == preview.checkpoint_id
                && checkpoint.checkpoint_digest == preview.checkpoint_digest
        }) else {
            return CheckpointDisplayContext::default();
        };

        let mut context = CheckpointDisplayContext {
            turn_index: Some(checkpoint.turn_index),
            prompt: checkpoint
                .prompt
                .as_deref()
                .map(|prompt| truncate_session_view_text(prompt, 96)),
            target_hashes: checkpoint
                .files
                .iter()
                .map(|file| (file.path.display().to_string(), file.before_hash.clone()))
                .collect(),
            ..CheckpointDisplayContext::default()
        };

        let turn_records = records
            .iter()
            .filter(|record| record.stream_sequence() > checkpoint.turn_boundary_stream_sequence)
            .take_while(|record| {
                !matches!(
                    checkpoint_session_entry(record),
                    Some(SessionLogEntry::User(_))
                )
            })
            .collect::<Vec<_>>();
        let mut prepared_call_ids = BTreeMap::new();
        let mut committed_operation_ids = BTreeSet::new();
        for record in &turn_records {
            let Ok(Some(typed)) = record.typed_domain_event_record() else {
                continue;
            };
            match typed.event {
                TypedDomainEvent::MutationPrepared(prepared) => {
                    if let Some(call_id) = prepared.tool_call_id {
                        prepared_call_ids.insert(prepared.operation_id, call_id);
                    }
                }
                TypedDomainEvent::MutationCommitted(committed) => {
                    committed_operation_ids.insert(committed.operation_id);
                }
                _ => {}
            }
        }
        let committed_call_ids = committed_operation_ids
            .iter()
            .filter_map(|operation_id| prepared_call_ids.get(operation_id))
            .cloned()
            .collect::<BTreeSet<_>>();

        for record in turn_records {
            let Some(entry) = checkpoint_session_entry(record) else {
                continue;
            };
            let SessionLogEntry::Control(ControlEntry::ToolPreviewCaptured(snapshot)) = entry
            else {
                continue;
            };
            if !committed_call_ids.contains(&snapshot.call_id) {
                continue;
            }
            for file in snapshot.file_diffs {
                let Some(restore_file) = preview
                    .files
                    .iter()
                    .find(|preview_file| preview_file.path.to_string_lossy().as_ref() == file.path)
                else {
                    continue;
                };
                if file.diff.trim().is_empty() {
                    continue;
                }
                context.reverse_diffs.push(CheckpointReverseDiff {
                    lines: reverse_recorded_diff(&file.diff, restore_file),
                    truncated: file.truncated,
                    original_line_count: file.original_line_count,
                });
            }
        }
        context.reverse_diffs.reverse();
        context
    }

    fn latest_checkpoint_request(&self) -> Result<ControlledCheckpointRestoreRequest, String> {
        let records = JsonlSessionStore::read_event_records(&self.session_log_path)
            .map_err(|error| format!("checkpoint stream unavailable: {error:#}"))?;
        let projection = ControlledCheckpointProjection::from_records(&records)
            .map_err(|error| format!("checkpoint projection unavailable: {error:#}"))?;
        let checkpoint = projection
            .latest()
            .ok_or_else(|| "no controlled checkpoint is available".to_owned())?;
        Ok(ControlledCheckpointRestoreRequest {
            checkpoint_id: checkpoint.checkpoint_id.clone(),
            checkpoint_digest: checkpoint.checkpoint_digest.clone(),
        })
    }
}

#[derive(Debug, Default)]
struct CheckpointDisplayContext {
    turn_index: Option<usize>,
    prompt: Option<String>,
    target_hashes: Vec<(String, Option<String>)>,
    reverse_diffs: Vec<CheckpointReverseDiff>,
}

#[derive(Debug)]
struct CheckpointReverseDiff {
    lines: Vec<String>,
    truncated: bool,
    original_line_count: usize,
}

fn conflict_reason_label(reason: CheckpointRestoreConflictReason) -> &'static str {
    match reason {
        CheckpointRestoreConflictReason::WorkspaceMismatch => "workspace mismatch",
        CheckpointRestoreConflictReason::CurrentHashMismatch => "current file changed",
        CheckpointRestoreConflictReason::ArtifactUnavailable => "snapshot unavailable",
        CheckpointRestoreConflictReason::SensitiveSnapshot => "sensitive snapshot excluded",
        CheckpointRestoreConflictReason::UnsupportedSnapshot => "snapshot unsupported",
        CheckpointRestoreConflictReason::InvalidBinding => "stale or invalid binding",
    }
}

fn checkpoint_session_entry(record: &SessionStreamRecord) -> Option<SessionLogEntry> {
    match record {
        SessionStreamRecord::Legacy { entry, .. } => Some((**entry).clone()),
        SessionStreamRecord::Stored(event) => event
            .payload
            .get("session_log_entry")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok()),
    }
}

fn reverse_recorded_diff(
    diff: &str,
    restore_file: &sigil_kernel::ControlledCheckpointRestorePreviewFile,
) -> Vec<String> {
    let path = restore_file.path.display();
    let current_header = match restore_file.restore_kind {
        ControlledCheckpointRestoreKind::RestoreContent
            if restore_file.actual_current_hash.is_none() =>
        {
            "--- /dev/null".to_owned()
        }
        _ => format!("--- current/{path}"),
    };
    let restored_header = match restore_file.restore_kind {
        ControlledCheckpointRestoreKind::RestoreContent => format!("+++ restored/{path}"),
        ControlledCheckpointRestoreKind::RemoveCreatedFile => "+++ /dev/null".to_owned(),
    };
    let mut lines = vec![current_header, restored_header];
    for line in diff.lines() {
        if line.starts_with("--- ") || line.starts_with("+++ ") {
            continue;
        }
        if let Some(reversed) = reverse_unified_hunk_header(line) {
            lines.push(reversed);
        } else if let Some(removed) = line.strip_prefix('-') {
            lines.push(format!("+{removed}"));
        } else if let Some(added) = line.strip_prefix('+') {
            lines.push(format!("-{added}"));
        } else {
            lines.push(line.to_owned());
        }
    }
    lines
}

fn reverse_unified_hunk_header(line: &str) -> Option<String> {
    let body = line.strip_prefix("@@ -")?;
    let (old_range, rest) = body.split_once(" +")?;
    let (new_range, suffix) = rest.split_once(" @@")?;
    Some(format!("@@ -{new_range} +{old_range} @@{suffix}"))
}

fn short_hash(hash: &str) -> &str {
    hash.strip_prefix("sha256:")
        .unwrap_or(hash)
        .get(..12)
        .unwrap_or(hash)
}

fn checkpoint_restored_payload(preview: &ControlledCheckpointRestorePreview) -> String {
    let file_count = preview.files.len();
    let file_label = if file_count == 1 { "file" } else { "files" };
    json!({
        "call_id": format!("checkpoint-preview:{}", preview.checkpoint_id),
        "tool_name": "checkpoint_restore",
        "status": "ok",
        "summary": format!("{file_count} controlled {file_label} restored · verification stale"),
        "preview_kind": "text",
        "preview_lines": [
            "Re-run verification before relying on earlier results",
            "Shell and remote side effects were not undone"
        ],
        "hidden_lines": 0,
        "metadata": {
            "changed_files": preview.files.iter().map(|file| file.path.display().to_string()).collect::<Vec<_>>(),
            "details": { "action": "restored" }
        }
    })
    .to_string()
}

fn short_checkpoint_id(checkpoint_id: &str) -> &str {
    checkpoint_id
        .strip_prefix("checkpoint:")
        .and_then(|value| value.get(..8))
        .unwrap_or(checkpoint_id)
}
