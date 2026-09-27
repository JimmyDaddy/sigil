use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use sigil_runtime::application_recovery::ApplicationConversationForkPointView;

use super::{AppAction, AppState, ModalState, PaneFocus};

#[derive(Debug)]
pub(super) struct ConversationForkModalState {
    request_id: u64,
    source_session_id: String,
    points: Vec<ApplicationConversationForkPointView>,
    selected: usize,
    loading: bool,
    forking: bool,
    error: Option<String>,
}

impl ConversationForkModalState {
    pub(super) fn lines(&self) -> Vec<String> {
        let mut lines = vec![
            "Choose a completed turn. The branch keeps history through that turn.".to_owned(),
            "Workspace files stay shared. Your draft stays editable; nothing is sent.".to_owned(),
            format!("Source session: {}", self.source_session_id),
            "↑/↓ choose · Enter branch · R refresh · Esc close".to_owned(),
            String::new(),
        ];
        if self.loading {
            lines.push("Loading completed turns…".to_owned());
        } else if self.forking {
            lines.push("Creating branch…".to_owned());
        } else if self.points.is_empty() {
            lines.push("No completed turns yet.".to_owned());
        } else {
            let first = self.selected.saturating_sub(3);
            for (index, point) in self.points.iter().enumerate().skip(first).take(7) {
                lines.push(format!(
                    "{} Turn {} · {}",
                    if index == self.selected { "›" } else { " " },
                    point.source_turn_index,
                    point
                        .prompt_preview
                        .as_deref()
                        .unwrap_or("(no prompt preview)"),
                ));
            }
            lines.push(format!("{} of {}", self.selected + 1, self.points.len()));
        }
        if let Some(error) = &self.error {
            lines.push(format!("Unable to branch: {error}"));
        }
        lines
    }
}

impl AppState {
    pub(super) fn open_conversation_fork_modal(&mut self) -> Option<AppAction> {
        self.blur_verification_card();
        self.blur_composer_aux_panels();
        let request_id = self.next_background_request_id();
        self.modal_state = Some(ModalState::ConversationFork(Box::new(
            ConversationForkModalState {
                request_id,
                source_session_id: self.session_id.clone(),
                points: Vec::new(),
                selected: 0,
                loading: true,
                forking: false,
                error: None,
            },
        )));
        self.active_pane = PaneFocus::Activity;
        Some(AppAction::LoadConversationForkPoints {
            request_id,
            source_session_id: self.session_id.clone(),
        })
    }

    pub(crate) fn conversation_fork_modal_open(&self) -> bool {
        matches!(self.modal_state, Some(ModalState::ConversationFork(_)))
    }

    pub(crate) fn conversation_fork_applying(&self) -> bool {
        matches!(self.modal_state.as_ref(), Some(ModalState::ConversationFork(state)) if state.forking)
    }

    pub(super) fn conversation_fork_request_matches(&self, request_id: u64) -> bool {
        matches!(self.modal_state.as_ref(), Some(ModalState::ConversationFork(state))
            if state.request_id == request_id && state.source_session_id == self.session_id)
    }

    pub(super) fn apply_conversation_fork_points(
        &mut self,
        request_id: u64,
        source_session_id: &str,
        points: Vec<ApplicationConversationForkPointView>,
    ) {
        if source_session_id != self.session_id
            || !self.conversation_fork_request_matches(request_id)
        {
            return;
        }
        if let Some(ModalState::ConversationFork(state)) = self.modal_state.as_mut() {
            if state.forking {
                return;
            }
            state.selected = points.len().saturating_sub(1);
            state.points = points;
            state.loading = false;
            state.error = None;
        }
    }

    pub(super) fn apply_conversation_fork_failure(&mut self, request_id: u64, error: &str) -> bool {
        if !self.conversation_fork_request_matches(request_id) {
            return false;
        }
        if let Some(ModalState::ConversationFork(state)) = self.modal_state.as_mut() {
            state.loading = false;
            state.forking = false;
            state.error = Some(error.to_owned());
        }
        true
    }

    pub(super) fn handle_conversation_fork_modal_key_event(
        &mut self,
        key: KeyEvent,
    ) -> Option<AppAction> {
        if key.modifiers != KeyModifiers::NONE {
            return None;
        }
        if self.conversation_fork_applying() {
            self.last_notice = Some("creating the branch; wait for the session switch".to_owned());
            return None;
        }
        match key.code {
            KeyCode::Esc => {
                self.modal_state = None;
                self.active_pane = PaneFocus::Composer;
                None
            }
            KeyCode::Char('r' | 'R') => self.open_conversation_fork_modal(),
            KeyCode::Up | KeyCode::Down => {
                if let Some(ModalState::ConversationFork(state)) = self.modal_state.as_mut() {
                    state.selected = if key.code == KeyCode::Up {
                        state.selected.saturating_sub(1)
                    } else {
                        state
                            .selected
                            .saturating_add(1)
                            .min(state.points.len().saturating_sub(1))
                    };
                }
                None
            }
            KeyCode::Enter => self.fork_selected_conversation_turn(),
            _ => None,
        }
    }

    fn fork_selected_conversation_turn(&mut self) -> Option<AppAction> {
        if self.runtime.is_busy || self.approval.has_actionable_pending() {
            self.last_notice =
                Some("finish or stop the current run before switching to a branch".to_owned());
            return None;
        }
        let target_model_ref = match self
            .runtime
            .model_route
            .as_ref()
            .map(|route| route.model_ref.clone())
        {
            Some(model) => model,
            None => {
                self.last_notice = Some("select a configured model before branching".to_owned());
                return None;
            }
        };
        let request_id = self.next_background_request_id();
        let Some(ModalState::ConversationFork(state)) = self.modal_state.as_mut() else {
            return None;
        };
        if state.loading || state.forking || state.source_session_id != self.session_id {
            return None;
        }
        let point = state.points.get(state.selected)?;
        state.request_id = request_id;
        state.forking = true;
        state.error = None;
        Some(AppAction::ForkConversation {
            request_id,
            source_session_id: state.source_session_id.clone(),
            source_turn_digest: point.source_turn_digest.clone(),
            target_model_ref,
        })
    }
}
