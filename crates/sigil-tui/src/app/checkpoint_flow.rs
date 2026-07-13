use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use sigil_kernel::{
    CheckpointRestoreConflictReason, ControlledCheckpointProjection,
    ControlledCheckpointRestoreKind, ControlledCheckpointRestorePreview,
    ControlledCheckpointRestoreRequest, JsonlSessionStore,
};

use super::{AppAction, AppState, PaneFocus, SidebarCard, TimelineRole};

impl AppState {
    pub(super) fn focus_checkpoint_review(&mut self) -> bool {
        let Ok(request) = self.latest_checkpoint_request() else {
            self.last_notice = Some("checkpoint review unavailable for this session".to_owned());
            return false;
        };
        self.checkpoint_restore_preview = None;
        self.info_rail_detail = true;
        self.active_pane = PaneFocus::Activity;
        self.sidebar_selected_card = SidebarCard::Review;
        self.blur_composer_aux_panels();
        self.last_notice = Some(format!(
            "checkpoint review focused: {}",
            short_checkpoint_id(&request.checkpoint_id)
        ));
        self.push_event("checkpoint", "review focused");
        true
    }

    pub(super) fn handle_checkpoint_review_key_event(
        &mut self,
        key: KeyEvent,
    ) -> Option<Option<AppAction>> {
        if self.active_pane != PaneFocus::Activity
            || self.sidebar_selected_card != SidebarCard::Review
            || key.modifiers != KeyModifiers::NONE
        {
            return None;
        }
        match key.code {
            KeyCode::Enter => Some(self.preview_or_execute_checkpoint_restore()),
            KeyCode::Char('f' | 'F') => Some(self.fork_conversation_from_checkpoint()),
            KeyCode::Char('i' | 'I') => {
                let lines = self.session_review_sidebar_lines();
                self.last_notice = Some("checkpoint evidence copied to timeline".to_owned());
                for line in lines {
                    self.push_timeline(TimelineRole::Notice, line);
                }
                Some(None)
            }
            _ => None,
        }
    }

    pub(super) fn apply_checkpoint_restore_preview(
        &mut self,
        preview: ControlledCheckpointRestorePreview,
    ) {
        let file_count = preview.files.len();
        let unknown_count = preview.unknown_mutation_count;
        for file in &preview.files {
            let direction = match file.restore_kind {
                ControlledCheckpointRestoreKind::RestoreContent => "restore content",
                ControlledCheckpointRestoreKind::RemoveCreatedFile => "remove created file",
            };
            let status = file
                .conflict_reason
                .map(conflict_reason_label)
                .unwrap_or("ready");
            self.push_timeline(
                TimelineRole::Notice,
                format!(
                    "checkpoint · {} · {direction} · {status}",
                    file.path.display()
                ),
            );
        }
        if unknown_count > 0 {
            self.push_timeline(
                TimelineRole::Notice,
                format!("checkpoint excludes {unknown_count} unknown shell/remote side effect(s)"),
            );
        }
        if preview.ready {
            self.last_notice = Some(format!(
                "restore preview ready for {file_count} file(s); press Enter again to confirm"
            ));
            self.push_timeline(
                TimelineRole::Notice,
                "Press Enter again to restore controlled files. Shell and remote side effects are not undone.",
            );
            self.checkpoint_restore_preview = Some(preview);
        } else {
            self.last_notice = Some("checkpoint restore blocked by preflight conflicts".to_owned());
            self.checkpoint_restore_preview = None;
        }
    }

    pub(super) fn apply_checkpoint_restore_completed(
        &mut self,
        preview: &ControlledCheckpointRestorePreview,
    ) {
        self.checkpoint_restore_preview = None;
        self.last_notice = Some(format!(
            "restored {} controlled file(s); verification is stale",
            preview.files.len()
        ));
        self.push_timeline(
            TimelineRole::Notice,
            format!(
                "Checkpoint restored {} controlled file(s). Re-run verification; shell and remote side effects were not undone.",
                preview.files.len()
            ),
        );
    }

    pub(super) fn clear_checkpoint_interaction(&mut self) {
        self.checkpoint_restore_preview = None;
        if self.sidebar_selected_card == SidebarCard::Review {
            self.sidebar_selected_card = SidebarCard::Usage;
        }
    }

    fn preview_or_execute_checkpoint_restore(&mut self) -> Option<AppAction> {
        if self.runtime.is_busy {
            self.last_notice = Some("wait for the active run before checkpoint restore".to_owned());
            return None;
        }
        let request = match self.latest_checkpoint_request() {
            Ok(request) => request,
            Err(error) => {
                self.checkpoint_restore_preview = None;
                self.last_notice = Some(error);
                return None;
            }
        };
        if self
            .checkpoint_restore_preview
            .as_ref()
            .is_some_and(|preview| {
                preview.ready
                    && preview.checkpoint_id == request.checkpoint_id
                    && preview.checkpoint_digest == request.checkpoint_digest
            })
        {
            self.checkpoint_restore_preview = None;
            self.last_notice = Some("checkpoint restore requested".to_owned());
            return Some(AppAction::ExecuteCheckpointRestore { request });
        }
        self.checkpoint_restore_preview = None;
        self.last_notice = Some("building exact checkpoint restore preview".to_owned());
        Some(AppAction::PreviewCheckpointRestore { request })
    }

    fn fork_conversation_from_checkpoint(&mut self) -> Option<AppAction> {
        if self.runtime.is_busy {
            self.last_notice = Some("wait for the active run before conversation fork".to_owned());
            return None;
        }
        let request = match self.latest_checkpoint_request() {
            Ok(request) => request,
            Err(error) => {
                self.last_notice = Some(error);
                return None;
            }
        };
        self.checkpoint_restore_preview = None;
        self.last_notice = Some(
            "conversation fork requested; workspace files stay shared and unchanged".to_owned(),
        );
        Some(AppAction::ForkConversationAtCheckpoint { request })
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

fn short_checkpoint_id(checkpoint_id: &str) -> &str {
    checkpoint_id
        .strip_prefix("checkpoint:")
        .and_then(|value| value.get(..8))
        .unwrap_or(checkpoint_id)
}
