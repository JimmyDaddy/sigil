use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use sigil_runtime::{
    LocalSessionCatalogEntry, SessionDeleteOutput, SessionDeletePreview, SessionExportOutput,
    SessionRetentionOutput, SessionRetentionPolicy, SessionRetentionPreview,
};

use super::{AppAction, AppState, TimelineRole, modal_flow::ModalState};

#[derive(Debug, Clone)]
pub(crate) enum SessionRetentionMaintenancePreview {
    Pending { request_id: u64 },
    Ready { preview: SessionRetentionPreview },
    Unavailable { error: String },
}

impl Default for SessionRetentionMaintenancePreview {
    fn default() -> Self {
        Self::Unavailable {
            error: "not loaded".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionModalAction {
    Resume,
    Fork,
    ImportKnowledge,
    Export,
    TogglePin,
    PreviewDelete,
    ConfirmDelete,
    ApplyRetention,
    Back,
    Details,
    Close,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SessionActionsModalPhase {
    Loading,
    Ready,
    Working(String),
    DeleteReview,
    Error(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionActionsFrameIdentity {
    request_id: u64,
    target_path: PathBuf,
    phase: SessionActionsModalPhase,
    details_open: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct SessionActionsModalState {
    pub(crate) request_id: u64,
    pub(crate) target_path: PathBuf,
    pub(crate) is_current: bool,
    pub(crate) entry: Option<LocalSessionCatalogEntry>,
    pub(crate) delete_preview: Option<SessionDeletePreview>,
    pub(crate) phase: SessionActionsModalPhase,
    pub(crate) result_message: Option<String>,
    pub(crate) details_scroll: Option<usize>,
    pub(crate) scroll_rows: usize,
}

impl SessionActionsModalState {
    fn action_rows(&self) -> Vec<(SessionModalAction, String)> {
        if self.details_scroll.is_some() {
            return vec![(
                SessionModalAction::Back,
                "[back] Session actions".to_owned(),
            )];
        }
        match &self.phase {
            SessionActionsModalPhase::Loading | SessionActionsModalPhase::Working(_) => vec![(
                SessionModalAction::Close,
                "[close] Close this operation view".to_owned(),
            )],
            SessionActionsModalPhase::Ready => {
                let mut rows = vec![(
                    SessionModalAction::Resume,
                    "[Enter resume] Resume selected session".to_owned(),
                )];
                if self
                    .entry
                    .as_ref()
                    .is_some_and(|entry| entry.finalized_turn_count > 0)
                {
                    rows.push((
                        SessionModalAction::Fork,
                        "[F fork] Fork latest finalized turn and switch".to_owned(),
                    ));
                }
                rows.push((
                    SessionModalAction::ImportKnowledge,
                    "[K conclusion] Bring into the current conversation".to_owned(),
                ));
                rows.push((
                    SessionModalAction::Export,
                    "[E export] Export safe transcript".to_owned(),
                ));
                let pin_label = if self.entry.as_ref().is_some_and(|entry| entry.pinned) {
                    "[P unpin] Allow explicit cleanup again"
                } else {
                    "[P pin] Protect from explicit cleanup"
                };
                rows.push((SessionModalAction::TogglePin, pin_label.to_owned()));
                if !self.is_current {
                    rows.push((
                        SessionModalAction::PreviewDelete,
                        "[D delete] Preview exact local deletion".to_owned(),
                    ));
                }
                rows.push((
                    SessionModalAction::Details,
                    "[I details] Full session identity".to_owned(),
                ));
                rows.push((
                    SessionModalAction::Close,
                    "[Esc close] Keep session unchanged".to_owned(),
                ));
                rows
            }
            SessionActionsModalPhase::DeleteReview => vec![
                (
                    SessionModalAction::ConfirmDelete,
                    "[Enter confirm delete] Delete the exact reviewed file".to_owned(),
                ),
                (
                    SessionModalAction::Back,
                    "[Backspace back] Return to session actions".to_owned(),
                ),
                (
                    SessionModalAction::Close,
                    "[Esc close] Keep session unchanged".to_owned(),
                ),
            ],
            SessionActionsModalPhase::Error(_) if self.entry.is_some() => vec![
                (
                    SessionModalAction::Back,
                    "[Backspace back] Return to session actions".to_owned(),
                ),
                (SessionModalAction::Close, "[close] Close".to_owned()),
            ],
            SessionActionsModalPhase::Error(_) => {
                vec![(SessionModalAction::Close, "[close] Close".to_owned())]
            }
        }
    }

    pub(crate) fn lines(&self) -> Vec<String> {
        if self.details_scroll.is_some() {
            let mut lines = vec!["Session identity".to_owned()];
            if let Some(entry) = &self.entry {
                lines.extend([
                    format!(
                        "title: {}",
                        entry.title.as_deref().unwrap_or("untitled session")
                    ),
                    format!(
                        "UUID: {}",
                        entry.session_id.as_deref().unwrap_or("unavailable")
                    ),
                    format!("file: {}", self.target_path.display()),
                    format!("size: {} bytes", entry.bytes),
                    format!("modified (Unix ms): {}", entry.modified_at_unix_ms),
                    format!("finalized turns: {}", entry.finalized_turn_count),
                    format!("pinned: {} · current: {}", entry.pinned, self.is_current),
                    format!("status: {:?}", entry.state),
                ]);
            } else {
                lines.push(format!("file: {}", self.target_path.display()));
            }
            return lines;
        }
        let mut lines = Vec::new();
        if let Some(entry) = &self.entry {
            lines.push(format!(
                "title: {}",
                super::formatting::truncate_session_view_text(
                    entry.title.as_deref().unwrap_or("untitled session"),
                    44
                )
            ));
            lines.push(format!(
                "{} · {} turns{}{}",
                super::formatting::relative_age_label(entry.modified_at_unix_ms / 1000),
                entry.finalized_turn_count,
                if self.is_current { " · current" } else { "" },
                if entry.pinned { " · pinned" } else { "" }
            ));
            if entry.state != sigil_runtime::LocalSessionCatalogState::Ready {
                lines.push(format!("Session status: {:?}", entry.state));
            }
        }
        lines.push(match &self.phase {
            SessionActionsModalPhase::Loading => "Loading selected session…".to_owned(),
            SessionActionsModalPhase::Ready => "Choose an action; draft is preserved.".to_owned(),
            SessionActionsModalPhase::Working(action) => format!("{action} in progress…"),
            SessionActionsModalPhase::DeleteReview => {
                "Review the exact file before deletion.".to_owned()
            }
            SessionActionsModalPhase::Error(error) => format!("Operation failed: {error}"),
        });
        if let Some(preview) = &self.delete_preview {
            lines.extend([
                format!("file: {}", preview.source_path.display()),
                format!("UUID: {}", preview.source_session_id),
                format!("delete bytes: {}", preview.source_bytes),
                format!("content sha256: {}", preview.source_content_sha256),
                format!("resource bytes: {}", preview.resource_bytes),
                format!("resource sha256: {}", preview.resource_tree_sha256),
                format!("preview digest: {}", preview.preview_digest),
                "shell and remote side effects are not affected".to_owned(),
            ]);
        }
        if let Some(message) = &self.result_message {
            lines.push(format!("result: {message}"));
        }
        lines.push(String::new());
        lines.extend(
            self.action_rows()
                .into_iter()
                .filter(|(action, _)| *action != SessionModalAction::Close)
                .map(|(_, line)| line),
        );
        lines
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SessionRetentionModalPhase {
    Review,
    Applying,
    Completed(String),
    Error(String),
}

#[derive(Debug, Clone)]
pub(crate) struct SessionRetentionModalState {
    pub(crate) request_id: Option<u64>,
    pub(crate) preview: SessionRetentionPreview,
    pub(crate) phase: SessionRetentionModalPhase,
}

impl SessionRetentionModalState {
    fn action_rows(&self) -> Vec<(SessionModalAction, String)> {
        match self.phase {
            SessionRetentionModalPhase::Review => vec![
                (
                    SessionModalAction::ApplyRetention,
                    "[apply cleanup] Delete every exact reviewed candidate".to_owned(),
                ),
                (
                    SessionModalAction::Close,
                    "[close] Keep all sessions unchanged".to_owned(),
                ),
            ],
            SessionRetentionModalPhase::Applying => vec![(
                SessionModalAction::Close,
                "[close] Close; late response will be ignored".to_owned(),
            )],
            SessionRetentionModalPhase::Completed(_) => {
                vec![(SessionModalAction::Close, "[close] Close".to_owned())]
            }
            SessionRetentionModalPhase::Error(_) => vec![
                (
                    SessionModalAction::Back,
                    "[back] Return to the exact preview".to_owned(),
                ),
                (SessionModalAction::Close, "[close] Close".to_owned()),
            ],
        }
    }

    pub(crate) fn lines(&self) -> Vec<String> {
        let mut lines = vec![match &self.phase {
            SessionRetentionModalPhase::Review => {
                "Review explicit local session cleanup.".to_owned()
            }
            SessionRetentionModalPhase::Applying => "Applying exact retention batch...".to_owned(),
            SessionRetentionModalPhase::Completed(message) => format!("Completed: {message}"),
            SessionRetentionModalPhase::Error(error) => format!("Cleanup failed: {error}"),
        }];
        lines.push(String::new());
        lines.push(match self.phase {
            SessionRetentionModalPhase::Review => "Enter apply  Esc close".to_owned(),
            SessionRetentionModalPhase::Applying => {
                "Esc close; late worker response will be ignored".to_owned()
            }
            SessionRetentionModalPhase::Completed(_) => "Enter or Esc close".to_owned(),
            SessionRetentionModalPhase::Error(_) => "Backspace back  Esc close".to_owned(),
        });
        lines.extend(self.action_rows().into_iter().map(|(_, line)| line));
        lines.push(String::new());
        lines.extend([
            format!("ready sessions: {}", self.preview.total_ready_sessions),
            format!("ready bytes: {}", self.preview.total_ready_bytes),
            format!("protected: {}", self.preview.protected_sessions),
            format!("pinned: {}", self.preview.pinned_sessions),
            format!("ineligible: {}", self.preview.ineligible_sessions),
            format!("candidates: {}", self.preview.candidates.len()),
            format!("release bytes: {}", self.preview.selected_bytes),
            format!(
                "constraints satisfied: {}",
                self.preview.constraints_satisfied
            ),
        ]);
        for candidate in self.preview.candidates.iter().take(5) {
            lines.push(format!(
                "- {} · {:?}",
                candidate
                    .delete_preview
                    .source_session_ref
                    .as_path()
                    .display(),
                candidate.reasons
            ));
        }
        let hidden = self.preview.candidates.len().saturating_sub(5);
        if hidden > 0 {
            lines.push(format!("... {hidden} more candidates"));
        }
        lines
    }
}

impl AppState {
    pub(crate) fn session_actions_view(&self) -> Option<crate::surface::SessionActionsSurface> {
        let Some(ModalState::SessionActions(state)) = self.modal_state.as_ref() else {
            return None;
        };
        Some(crate::surface::SessionActionsSurface {
            details_open: state.details_scroll.is_some(),
            scroll_rows: state.details_scroll.unwrap_or(state.scroll_rows),
        })
    }

    pub(super) fn scroll_session_actions(&mut self, up: bool, amount: usize) {
        let Some(view) = self.session_actions_view() else {
            return;
        };
        let (max_scroll, _) = crate::ui::session_actions_scroll_bounds(
            self.terminal_width,
            self.terminal_height,
            &self.modal_lines(),
            &view,
        );
        if let Some(ModalState::SessionActions(state)) = self.modal_state.as_mut() {
            let scroll = state
                .details_scroll
                .as_mut()
                .unwrap_or(&mut state.scroll_rows);
            let current = (*scroll).min(max_scroll);
            *scroll = if up {
                current.saturating_sub(amount)
            } else {
                current.saturating_add(amount).min(max_scroll)
            };
        }
    }

    pub(crate) fn session_actions_frame_identity(&self) -> Option<SessionActionsFrameIdentity> {
        let Some(ModalState::SessionActions(state)) = self.modal_state.as_ref() else {
            return None;
        };
        Some(SessionActionsFrameIdentity {
            request_id: state.request_id,
            target_path: state.target_path.clone(),
            phase: state.phase.clone(),
            details_open: state.details_scroll.is_some(),
        })
    }

    pub(super) fn resume_session_selector_active(&self) -> bool {
        self.selected_slash_entry()
            .is_some_and(|entry| entry.resolved.canonical == "/resume")
    }

    pub(super) fn open_selected_session_actions(&mut self) -> Option<AppAction> {
        if self.runtime.is_busy {
            self.last_notice = Some("busy; session actions are available after the run".to_owned());
            return None;
        }
        let entry = self.selected_slash_entry()?;
        if entry.resolved.canonical != "/resume" {
            return None;
        }
        let target_path = PathBuf::from(entry.resolved.arg);
        let is_current = target_path == self.session_log_path;
        let request_id = self.next_background_request_id();
        self.modal_state = Some(ModalState::SessionActions(Box::new(
            SessionActionsModalState {
                request_id,
                target_path: target_path.clone(),
                is_current,
                entry: None,
                delete_preview: None,
                phase: SessionActionsModalPhase::Loading,
                result_message: None,
                details_scroll: None,
                scroll_rows: 0,
            },
        )));
        self.last_notice = Some("loading session actions".to_owned());
        Some(AppAction::InspectLocalSession {
            request_id,
            source_path: target_path,
        })
    }

    pub(crate) fn session_lifecycle_modal_open(&self) -> bool {
        matches!(
            self.modal_state,
            Some(ModalState::SessionActions(_) | ModalState::SessionRetention(_))
        )
    }

    pub(super) fn local_session_action_request_matches(&self, request_id: u64) -> bool {
        matches!(
            self.modal_state,
            Some(ModalState::SessionActions(ref state)) if state.request_id == request_id
        )
    }

    pub(crate) fn session_modal_action_rows(&self) -> Vec<(SessionModalAction, String)> {
        match self.modal_state.as_ref() {
            Some(ModalState::SessionActions(state)) => state.action_rows(),
            Some(ModalState::SessionRetention(state)) => state.action_rows(),
            _ => Vec::new(),
        }
    }

    pub(super) fn handle_session_lifecycle_modal_key(
        &mut self,
        key: KeyEvent,
    ) -> Option<AppAction> {
        if matches!(key.modifiers, KeyModifiers::NONE | KeyModifiers::SHIFT)
            && self.session_actions_view().is_some()
        {
            match key.code {
                KeyCode::Up => {
                    self.scroll_session_actions(true, 1);
                    return None;
                }
                KeyCode::Down => {
                    self.scroll_session_actions(false, 1);
                    return None;
                }
                KeyCode::PageUp => {
                    self.scroll_session_actions(true, 8);
                    return None;
                }
                KeyCode::PageDown => {
                    self.scroll_session_actions(false, 8);
                    return None;
                }
                KeyCode::Home => {
                    self.scroll_session_actions(true, usize::MAX);
                    return None;
                }
                KeyCode::End => {
                    self.scroll_session_actions(false, usize::MAX);
                    return None;
                }
                _ => {}
            }
            if self
                .session_actions_view()
                .is_some_and(|view| view.details_open)
            {
                if matches!(key.code, KeyCode::Esc | KeyCode::Backspace) {
                    self.handle_session_modal_action(SessionModalAction::Back);
                }
                return None;
            }
        }
        if !matches!(key.modifiers, KeyModifiers::NONE | KeyModifiers::SHIFT) {
            return None;
        }
        if key.code == KeyCode::Esc {
            self.modal_state = None;
            self.last_notice = Some("closed session maintenance".to_owned());
            return None;
        }
        let action = match self.modal_state.as_ref() {
            Some(ModalState::SessionActions(state)) => match (&state.phase, key.code) {
                (SessionActionsModalPhase::Ready, KeyCode::Enter) => {
                    Some(SessionModalAction::Resume)
                }
                (SessionActionsModalPhase::Ready, KeyCode::Char('k' | 'K')) => {
                    Some(SessionModalAction::ImportKnowledge)
                }
                (SessionActionsModalPhase::Ready, KeyCode::Char('f' | 'F')) => {
                    Some(SessionModalAction::Fork)
                }
                (SessionActionsModalPhase::Ready, KeyCode::Char('e' | 'E')) => {
                    Some(SessionModalAction::Export)
                }
                (SessionActionsModalPhase::Ready, KeyCode::Char('p' | 'P')) => {
                    Some(SessionModalAction::TogglePin)
                }
                (SessionActionsModalPhase::Ready, KeyCode::Char('d' | 'D')) => {
                    Some(SessionModalAction::PreviewDelete)
                }
                (SessionActionsModalPhase::Ready, KeyCode::Char('i' | 'I')) => {
                    Some(SessionModalAction::Details)
                }
                (SessionActionsModalPhase::DeleteReview, KeyCode::Enter) => {
                    Some(SessionModalAction::ConfirmDelete)
                }
                (
                    SessionActionsModalPhase::DeleteReview | SessionActionsModalPhase::Error(_),
                    KeyCode::Backspace,
                ) => Some(SessionModalAction::Back),
                _ => None,
            },
            Some(ModalState::SessionRetention(state)) => match (&state.phase, key.code) {
                (SessionRetentionModalPhase::Review, KeyCode::Enter) => {
                    Some(SessionModalAction::ApplyRetention)
                }
                (SessionRetentionModalPhase::Completed(_), KeyCode::Enter) => {
                    Some(SessionModalAction::Close)
                }
                (SessionRetentionModalPhase::Error(_), KeyCode::Backspace) => {
                    Some(SessionModalAction::Back)
                }
                _ => None,
            },
            _ => None,
        }?;
        self.handle_session_modal_action(action)
    }

    pub(super) fn handle_session_modal_action(
        &mut self,
        action: SessionModalAction,
    ) -> Option<AppAction> {
        if let Some(ModalState::SessionActions(state)) = self.modal_state.as_mut() {
            if state.details_scroll.is_some() {
                if action == SessionModalAction::Back {
                    state.details_scroll = None;
                }
                return None;
            }
            if !state
                .action_rows()
                .iter()
                .any(|(available, _)| *available == action)
            {
                return None;
            }
            if action == SessionModalAction::Details {
                state.details_scroll = Some(0);
                return None;
            }
            state.scroll_rows = 0;
        }
        if action == SessionModalAction::Close {
            self.modal_state = None;
            self.last_notice = Some("closed session maintenance".to_owned());
            return None;
        }
        if action == SessionModalAction::Back {
            match self.modal_state.as_mut() {
                Some(ModalState::SessionActions(state)) if state.entry.is_some() => {
                    state.phase = SessionActionsModalPhase::Ready;
                    state.delete_preview = None;
                }
                Some(ModalState::SessionRetention(state)) => {
                    state.phase = SessionRetentionModalPhase::Review;
                    state.request_id = None;
                }
                _ => {}
            }
            return None;
        }

        match self.modal_state.as_ref() {
            Some(ModalState::SessionActions(state)) => {
                let target_path = state.target_path.clone();
                let pinned = state.entry.as_ref().is_some_and(|entry| entry.pinned);
                let delete_preview = state.delete_preview.clone();
                match action {
                    SessionModalAction::ImportKnowledge => {
                        let entry = state.entry.as_ref()?;
                        let source_ref = entry.session_ref.clone();
                        let source_id = entry.session_id.clone()?;
                        Some(self.open_branch_knowledge_modal(source_ref, source_id))
                    }
                    SessionModalAction::Resume => {
                        self.modal_state = None;
                        Some(AppAction::SwitchSession {
                            session_log_path: target_path,
                        })
                    }
                    SessionModalAction::Fork
                    | SessionModalAction::Export
                    | SessionModalAction::TogglePin
                    | SessionModalAction::PreviewDelete
                    | SessionModalAction::ConfirmDelete => {
                        if action == SessionModalAction::ConfirmDelete && delete_preview.is_none() {
                            self.last_notice =
                                Some("delete preview is unavailable; review it again".to_owned());
                            return None;
                        }
                        let current_model_route = if action == SessionModalAction::Fork {
                            let Some(route) = self.runtime.model_route.clone() else {
                                self.last_notice = Some(
                                    "cannot fork because the current model route is unavailable"
                                        .to_owned(),
                                );
                                return None;
                            };
                            Some(route)
                        } else {
                            None
                        };
                        let request_id = self.next_background_request_id();
                        if let Some(ModalState::SessionActions(state)) = self.modal_state.as_mut() {
                            state.request_id = request_id;
                            state.result_message = None;
                            state.phase = SessionActionsModalPhase::Working(
                                match action {
                                    SessionModalAction::Fork => "creating conversation fork",
                                    SessionModalAction::Export => "exporting safe transcript",
                                    SessionModalAction::TogglePin => {
                                        if pinned {
                                            "unpinning session"
                                        } else {
                                            "pinning session"
                                        }
                                    }
                                    SessionModalAction::PreviewDelete => "preparing delete preview",
                                    SessionModalAction::ConfirmDelete => {
                                        "deleting reviewed session"
                                    }
                                    _ => unreachable!(),
                                }
                                .to_owned(),
                            );
                        }
                        match action {
                            SessionModalAction::Fork => Some(AppAction::ForkLocalSession {
                                request_id,
                                source_path: target_path,
                                current_model_route: current_model_route
                                    .expect("fork route checked before dispatch"),
                            }),
                            SessionModalAction::Export => Some(AppAction::ExportLocalSession {
                                request_id,
                                source_path: target_path,
                            }),
                            SessionModalAction::TogglePin => Some(AppAction::SetLocalSessionPin {
                                request_id,
                                source_path: target_path,
                                pinned: !pinned,
                            }),
                            SessionModalAction::PreviewDelete => {
                                Some(AppAction::PreviewLocalSessionDelete {
                                    request_id,
                                    source_path: target_path,
                                })
                            }
                            SessionModalAction::ConfirmDelete => {
                                delete_preview.map(|preview| AppAction::ApplyLocalSessionDelete {
                                    request_id,
                                    preview,
                                })
                            }
                            _ => unreachable!(),
                        }
                    }
                    _ => None,
                }
            }
            Some(ModalState::SessionRetention(state))
                if action == SessionModalAction::ApplyRetention
                    && matches!(state.phase, SessionRetentionModalPhase::Review) =>
            {
                let preview = state.preview.clone();
                if preview.candidates.is_empty() {
                    self.last_notice =
                        Some("retention preview has no deletion candidates".to_owned());
                    return None;
                }
                let request_id = self.next_background_request_id();
                if let Some(ModalState::SessionRetention(state)) = self.modal_state.as_mut() {
                    state.request_id = Some(request_id);
                    state.phase = SessionRetentionModalPhase::Applying;
                }
                Some(AppAction::ApplySessionRetention {
                    request_id,
                    preview,
                })
            }
            _ => None,
        }
    }

    pub(super) fn schedule_session_retention_preview(&mut self) -> Option<AppAction> {
        let retention = self
            .config_snapshot
            .as_ref()
            .map(|config| config.session.retention.clone())?;
        let request_id = self.next_background_request_id();
        let policy = SessionRetentionPolicy::from(&retention);
        self.runtime.session_retention_preview =
            SessionRetentionMaintenancePreview::Pending { request_id };
        Some(AppAction::PreviewSessionRetention { request_id, policy })
    }

    pub(super) fn open_session_retention_modal(&mut self) -> Option<AppAction> {
        let preview = match &self.runtime.session_retention_preview {
            SessionRetentionMaintenancePreview::Ready { preview } => preview.clone(),
            SessionRetentionMaintenancePreview::Pending { .. } => {
                self.last_notice = Some("session retention preview is still loading".to_owned());
                return None;
            }
            SessionRetentionMaintenancePreview::Unavailable { .. } => {
                let action = self.schedule_session_retention_preview();
                self.last_notice = Some("refreshing session retention preview".to_owned());
                return action;
            }
        };
        self.modal_state = Some(ModalState::SessionRetention(Box::new(
            SessionRetentionModalState {
                request_id: None,
                preview,
                phase: SessionRetentionModalPhase::Review,
            },
        )));
        self.last_notice = Some("reviewing explicit session cleanup".to_owned());
        None
    }

    pub(super) fn apply_local_session_inspected(
        &mut self,
        request_id: u64,
        entry: LocalSessionCatalogEntry,
    ) -> bool {
        let Some(ModalState::SessionActions(state)) = self.modal_state.as_mut() else {
            return false;
        };
        if state.request_id != request_id {
            return false;
        }
        state.target_path = entry.path.clone();
        state.entry = Some(entry);
        state.phase = SessionActionsModalPhase::Ready;
        true
    }

    pub(super) fn apply_local_session_exported(
        &mut self,
        request_id: u64,
        output: &SessionExportOutput,
    ) -> bool {
        let Some(ModalState::SessionActions(state)) = self.modal_state.as_mut() else {
            return false;
        };
        if state.request_id != request_id {
            return false;
        }
        state.phase = SessionActionsModalPhase::Ready;
        state.result_message = Some(format!(
            "exported {} safe message(s) to {}",
            output.message_count,
            output.path.display()
        ));
        true
    }

    pub(super) fn apply_local_session_pin_changed(
        &mut self,
        request_id: u64,
        entry: LocalSessionCatalogEntry,
    ) -> bool {
        let Some(ModalState::SessionActions(state)) = self.modal_state.as_mut() else {
            return false;
        };
        if state.request_id != request_id {
            return false;
        }
        state.target_path = entry.path.clone();
        let pinned = entry.pinned;
        state.entry = Some(entry);
        state.phase = SessionActionsModalPhase::Ready;
        state.result_message = Some(if pinned {
            "session pinned".to_owned()
        } else {
            "session unpinned".to_owned()
        });
        true
    }

    pub(super) fn apply_local_session_delete_preview(
        &mut self,
        request_id: u64,
        preview: SessionDeletePreview,
    ) -> bool {
        let Some(ModalState::SessionActions(state)) = self.modal_state.as_mut() else {
            return false;
        };
        if state.request_id != request_id {
            return false;
        }
        state.target_path = preview.source_path.clone();
        state.delete_preview = Some(preview);
        state.phase = SessionActionsModalPhase::DeleteReview;
        true
    }

    pub(super) fn apply_local_session_deleted(
        &mut self,
        request_id: u64,
        output: &SessionDeleteOutput,
    ) -> bool {
        let Some(ModalState::SessionActions(state)) = self.modal_state.as_ref() else {
            return false;
        };
        if state.request_id != request_id {
            return false;
        }
        self.modal_state = None;
        self.refresh_session_history();
        self.last_notice = Some(format!(
            "deleted local session ({} bytes)",
            output.deleted_bytes
        ));
        self.push_timeline(
            TimelineRole::Notice,
            format!(
                "Deleted one reviewed local session ({} bytes).",
                output.deleted_bytes
            ),
        );
        true
    }

    pub(super) fn apply_session_retention_preview(
        &mut self,
        request_id: u64,
        preview: SessionRetentionPreview,
    ) -> bool {
        if !matches!(
            self.runtime.session_retention_preview,
            SessionRetentionMaintenancePreview::Pending { request_id: pending } if pending == request_id
        ) {
            return false;
        }
        self.runtime.session_retention_preview =
            SessionRetentionMaintenancePreview::Ready { preview };
        true
    }

    pub(super) fn apply_session_retention_output(
        &mut self,
        request_id: u64,
        output: &SessionRetentionOutput,
    ) -> bool {
        let Some(ModalState::SessionRetention(state)) = self.modal_state.as_mut() else {
            return false;
        };
        if state.request_id != Some(request_id) {
            return false;
        }
        state.phase = SessionRetentionModalPhase::Completed(format!(
            "deleted {} session(s), {} bytes",
            output.deleted_sessions, output.deleted_bytes
        ));
        state.request_id = None;
        self.refresh_session_history();
        true
    }

    pub(super) fn apply_local_session_lifecycle_failed(
        &mut self,
        request_id: u64,
        error: String,
    ) -> bool {
        if let Some(ModalState::SessionActions(state)) = self.modal_state.as_mut()
            && state.request_id == request_id
        {
            state.phase = SessionActionsModalPhase::Error(error);
            return true;
        }
        if let Some(ModalState::SessionRetention(state)) = self.modal_state.as_mut()
            && state.request_id == Some(request_id)
        {
            state.phase = SessionRetentionModalPhase::Error(error);
            state.request_id = None;
            return true;
        }
        if matches!(
            self.runtime.session_retention_preview,
            SessionRetentionMaintenancePreview::Pending { request_id: pending } if pending == request_id
        ) {
            self.runtime.session_retention_preview =
                SessionRetentionMaintenancePreview::Unavailable { error };
            return true;
        }
        false
    }
}
