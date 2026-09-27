use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use sigil_runtime::application_branch_knowledge::{
    ApplicationBranchKnowledgeImportReceipt, ApplicationBranchKnowledgeImportRequest,
    ApplicationBranchKnowledgePreview, ApplicationBranchLineageView,
};

use super::{AppAction, AppState, ModalState, TimelineRole};

#[derive(Debug)]
pub(super) struct BranchKnowledgeModalState {
    request_id: u64,
    target_session_id: String,
    source_session_id: String,
    preview: Option<ApplicationBranchKnowledgePreview>,
    lineage: Option<ApplicationBranchLineageView>,
    selected: usize,
    scroll: usize,
    applying: bool,
    error: Option<String>,
}

impl BranchKnowledgeModalState {
    pub(super) fn lines(&self) -> Vec<String> {
        let mut lines = vec![
            "Select a completed conclusion to bring into this conversation.".to_owned(),
            "Unverified knowledge; details may be missing. No permissions or verification transfer.".to_owned(),
            "↑/↓ choose · PgUp/PgDn read · Enter import · Esc close. Your draft stays editable; nothing is sent.".to_owned(),
            format!("Source: {}", self.source_session_id),
        ];
        if let Some(lineage) = &self.lineage {
            if let Some(parent) = &lineage.parent {
                lines.push(format!(
                    "Parent: {} · turn {}",
                    parent.session_id, parent.source_turn_index
                ));
            }
            lines.push(format!(
                "{} child branches · {} unavailable entries",
                lineage.children.len(),
                lineage.unavailable_count
            ));
        }
        match &self.preview {
            None if self.error.is_none() => lines.push("Loading completed conclusions…".to_owned()),
            Some(_) if self.applying => lines.push("Importing selected knowledge…".to_owned()),
            Some(preview) if preview.points.is_empty() => {
                lines.push("No completed text conclusions in this session.".to_owned())
            }
            Some(preview) => {
                lines.push(format!(
                    "Conclusion {} of {}",
                    self.selected + 1,
                    preview.points.len()
                ));
                if let Some(point) = preview.points.get(self.selected) {
                    if point.truncated {
                        lines.push("The source exceeds the preview budget; only this excerpt will be imported.".to_owned());
                    }
                    lines.extend(
                        point
                            .summary
                            .lines()
                            .skip(self.scroll)
                            .take(14)
                            .map(ToOwned::to_owned),
                    );
                    if point.summary.lines().count() > self.scroll + 14 {
                        lines.push("PgDn shows the remaining lines of this excerpt.".to_owned());
                    }
                }
            }
            None => {}
        }
        if let Some(error) = &self.error {
            lines.push(format!("Import unavailable: {error}"));
        }
        lines
    }
}

impl AppState {
    pub(super) fn open_branch_knowledge_modal(
        &mut self,
        source_session_ref: sigil_kernel::SessionRef,
        source_session_id: String,
    ) -> AppAction {
        let request_id = self.next_background_request_id();
        self.modal_state = Some(ModalState::BranchKnowledge(Box::new(
            BranchKnowledgeModalState {
                request_id,
                target_session_id: self.session_id.clone(),
                source_session_id: source_session_id.clone(),
                preview: None,
                lineage: None,
                selected: 0,
                scroll: 0,
                applying: false,
                error: None,
            },
        )));
        AppAction::LoadBranchKnowledge {
            request_id,
            target_session_id: self.session_id.clone(),
            source_session_ref,
            source_session_id,
        }
    }

    pub(crate) fn branch_knowledge_modal_open(&self) -> bool {
        matches!(self.modal_state, Some(ModalState::BranchKnowledge(_)))
    }

    pub(crate) fn branch_knowledge_applying(&self) -> bool {
        matches!(&self.modal_state, Some(ModalState::BranchKnowledge(state)) if state.applying)
    }

    pub(super) fn apply_branch_knowledge_preview(
        &mut self,
        request_id: u64,
        target_session_id: &str,
        preview: ApplicationBranchKnowledgePreview,
        lineage: ApplicationBranchLineageView,
    ) {
        if let Some(ModalState::BranchKnowledge(state)) = self.modal_state.as_mut()
            && state.request_id == request_id
            && state.target_session_id == target_session_id
            && self.session_id == target_session_id
            && state.source_session_id == preview.source_session_id
        {
            state.selected = preview.points.len().saturating_sub(1);
            state.preview = Some(preview);
            state.lineage = Some(lineage);
        }
    }

    pub(super) fn apply_branch_knowledge_error(&mut self, request_id: u64, error: &str) -> bool {
        let Some(ModalState::BranchKnowledge(state)) = self.modal_state.as_mut() else {
            return false;
        };
        if state.request_id != request_id || state.target_session_id != self.session_id {
            return false;
        }
        state.applying = false;
        state.error = Some(error.to_owned());
        true
    }

    pub(super) fn apply_branch_knowledge_receipt(
        &mut self,
        request_id: u64,
        target_session_id: &str,
        receipt: &ApplicationBranchKnowledgeImportReceipt,
    ) -> bool {
        if !matches!(&self.modal_state, Some(ModalState::BranchKnowledge(state))
            if state.request_id == request_id && state.target_session_id == target_session_id && state.applying)
            || self.session_id != target_session_id
        {
            return false;
        }
        self.modal_state = None;
        let notice = if receipt.already_imported {
            "This conclusion was already imported. Your draft is unchanged."
        } else {
            "Branch conclusion imported as unverified knowledge. Your draft is unchanged; send when ready."
        };
        self.last_notice = Some(notice.to_owned());
        self.push_timeline(TimelineRole::Notice, notice);
        true
    }

    pub(super) fn handle_branch_knowledge_key_event(&mut self, key: KeyEvent) -> Option<AppAction> {
        if key.modifiers != KeyModifiers::NONE {
            return None;
        }
        let Some(ModalState::BranchKnowledge(state)) = self.modal_state.as_mut() else {
            return None;
        };
        if state.applying {
            return None;
        }
        if key.code == KeyCode::Esc {
            self.modal_state = None;
            return None;
        }
        let preview = state.preview.as_ref()?;
        match key.code {
            KeyCode::Up => {
                state.selected = state.selected.saturating_sub(1);
                state.scroll = 0;
            }
            KeyCode::Down => {
                state.selected = state
                    .selected
                    .saturating_add(1)
                    .min(preview.points.len().saturating_sub(1));
                state.scroll = 0;
            }
            KeyCode::PageUp => state.scroll = state.scroll.saturating_sub(10),
            KeyCode::PageDown => {
                let lines = preview
                    .points
                    .get(state.selected)
                    .map_or(0, |point| point.summary.lines().count());
                state.scroll = state
                    .scroll
                    .saturating_add(10)
                    .min(lines.saturating_sub(14));
            }
            KeyCode::Enter => {
                if self.runtime.is_busy || self.approval.has_actionable_pending() {
                    state.error =
                        Some("wait for the current run or approval before importing".to_owned());
                    return None;
                }
                let point = preview.points.get(state.selected)?;
                state.applying = true;
                state.error = None;
                return Some(AppAction::ImportBranchKnowledge {
                    request_id: state.request_id,
                    target_session_id: state.target_session_id.clone(),
                    request: ApplicationBranchKnowledgeImportRequest {
                        source_session_ref: preview.source_session_ref.clone(),
                        source_session_id: preview.source_session_id.clone(),
                        source_turn_digest: point.source_turn_digest.clone(),
                        source_message_id: point.source_message_id.clone(),
                        source_text_sha256: point.source_text_sha256.clone(),
                        summary_sha256: point.summary_sha256.clone(),
                    },
                });
            }
            _ => {}
        }
        None
    }
}
