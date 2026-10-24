use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::modal_flow::{SecretInputTarget, TextInputTarget};
use super::{AppAction, AppState, ModalState, TimelineRole};

#[derive(Debug)]
pub(super) struct QuitConfirmation {
    previous_modal: Option<ModalState>,
    previous_notice: Option<String>,
    config_changes: bool,
}

impl AppState {
    pub(super) fn request_quit(&mut self) -> Option<AppAction> {
        if self.quit_confirmation.is_some() {
            return None;
        }
        if self.checkpoint_mutation_pending()
            || self.intent_drop_applying()
            || self.conversation_fork_applying()
            || self.branch_knowledge_applying()
        {
            self.last_notice = Some(
                "a reviewed workspace operation is applying; wait for completion before quitting"
                    .to_owned(),
            );
            return None;
        }
        if self.runtime.is_busy {
            self.modal_state = None;
            self.last_notice = Some("cancellation requested".to_owned());
            self.push_timeline(TimelineRole::Notice, "cancel requested");
            return Some(AppAction::CancelRun);
        }
        let config_changes = self.config_state.as_ref().is_some_and(|state| {
            state.dirty
                || match self.modal_state.as_ref() {
                    Some(ModalState::TextInput(input)) => match input.target {
                        TextInputTarget::ConfigManualModel => {
                            input.buffer != state.draft.provider_model
                        }
                        TextInputTarget::ConfigField(field) => state
                            .field_text_value(field)
                            .is_some_and(|value| input.buffer != value),
                        _ => false,
                    },
                    Some(ModalState::SecretInput(input))
                        if input.target == SecretInputTarget::ConfigProviderApiKey =>
                    {
                        input.buffer.expose_secret() != state.draft.provider_api_key.expose_secret()
                    }
                    _ => false,
                }
        });
        if self.composer.input.is_empty()
            && self.composer.image_attachments.is_empty()
            && !config_changes
        {
            self.config_state = None;
            self.modal_state = None;
            self.should_quit = true;
            return None;
        }
        self.quit_confirmation = Some(QuitConfirmation {
            previous_modal: self.modal_state.take(),
            previous_notice: self.last_notice.take(),
            config_changes,
        });
        None
    }

    pub(crate) fn quit_confirmation_open(&self) -> bool {
        self.quit_confirmation.is_some()
    }

    pub(super) fn quit_confirmation_lines(&self) -> Option<Vec<String>> {
        let confirmation = self.quit_confirmation.as_ref()?;
        let mut lines = vec!["Your unsent draft will be lost.".to_owned()];
        if confirmation.config_changes {
            lines[0] =
                if self.composer.input.is_empty() && self.composer.image_attachments.is_empty() {
                    "Your unsaved settings will be lost."
                } else {
                    "Your unsent draft and unsaved settings will be lost."
                }
                .to_owned();
        }
        lines.extend([
            String::new(),
            "Enter / Esc: keep editing".to_owned(),
            "Ctrl-D: discard and quit".to_owned(),
        ]);
        Some(lines)
    }

    pub(super) fn handle_quit_confirmation_key(&mut self, key: KeyEvent) -> Option<AppAction> {
        let discard =
            key.code == KeyCode::Char('d') && key.modifiers.contains(KeyModifiers::CONTROL);
        let keep = matches!(key.code, KeyCode::Enter | KeyCode::Esc)
            || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL));
        if discard {
            if self.runtime.is_busy {
                self.last_notice = Some("cancellation requested".to_owned());
                return Some(AppAction::CancelRun);
            }
            if self.checkpoint_mutation_pending()
                || self.intent_drop_applying()
                || self.conversation_fork_applying()
                || self.branch_knowledge_applying()
            {
                return None;
            }
            self.config_state = None;
            self.modal_state = None;
            self.quit_confirmation = None;
            self.should_quit = true;
        } else if keep && let Some(confirmation) = self.quit_confirmation.take() {
            self.modal_state = confirmation.previous_modal;
            self.last_notice = confirmation.previous_notice;
        }
        None
    }
}
