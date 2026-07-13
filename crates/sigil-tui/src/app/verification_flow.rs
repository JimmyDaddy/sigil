use crossterm::event::{KeyCode, KeyEvent};

use super::{AppAction, AppState, PaneFocus, TimelineRole};
use crate::app::task_sidebar::VerificationCardAction;

impl AppState {
    pub(crate) fn verification_card_focused(&self) -> bool {
        self.verification_card_focused
            && self
                .task_strip_view()
                .is_some_and(|view| view.verification.is_some())
    }

    pub(crate) fn verification_inspect_open(&self) -> bool {
        self.verification_card_focused() && self.verification_inspect_open
    }

    pub(crate) fn verification_card_has_action(&self) -> bool {
        self.task_strip_view()
            .and_then(|view| view.verification)
            .is_some_and(|card| card.action.is_some())
    }

    pub(super) fn focus_verification_card(&mut self) -> bool {
        if self
            .task_strip_view()
            .and_then(|view| view.verification)
            .is_none()
        {
            self.last_notice = Some("no task verification card available".to_owned());
            return false;
        }
        self.verification_card_focused = true;
        self.active_pane = PaneFocus::Activity;
        self.blur_composer_aux_panels();
        self.last_notice = Some("verification card focused".to_owned());
        true
    }

    pub(super) fn blur_verification_card(&mut self) {
        self.verification_card_focused = false;
        self.verification_inspect_open = false;
    }

    pub(super) fn handle_verification_card_key_event(
        &mut self,
        key: KeyEvent,
    ) -> Option<Option<AppAction>> {
        if !self.verification_card_focused || !key.modifiers.is_empty() {
            return None;
        }
        if self
            .task_strip_view()
            .and_then(|view| view.verification)
            .is_none()
        {
            self.blur_verification_card();
            self.active_pane = PaneFocus::Composer;
            return None;
        }
        match key.code {
            KeyCode::Enter => Some(self.activate_verification_card()),
            KeyCode::Char('i' | 'I') => {
                self.verification_inspect_open = !self.verification_inspect_open;
                self.last_notice = Some(if self.verification_inspect_open {
                    "verification evidence expanded".to_owned()
                } else {
                    "verification evidence collapsed".to_owned()
                });
                Some(None)
            }
            KeyCode::Esc => {
                self.blur_verification_card();
                self.active_pane = PaneFocus::Composer;
                Some(None)
            }
            _ => None,
        }
    }

    fn activate_verification_card(&mut self) -> Option<AppAction> {
        if self.runtime.is_busy {
            self.last_notice =
                Some("wait for the active run before running verification".to_owned());
            return None;
        }
        let card = self.task_strip_view()?.verification?;
        match card.action? {
            VerificationCardAction::Rerun(request) => {
                let check_spec_id = request.check_spec_id.clone();
                self.last_notice = Some(format!("running verification check {check_spec_id}"));
                self.push_timeline(
                    TimelineRole::Notice,
                    format!("Running verification check {check_spec_id}."),
                );
                Some(AppAction::RerunTaskVerification { request })
            }
            VerificationCardAction::ReviewApproval { check_spec_id } => {
                self.last_notice = Some(format!("reviewing verification check {check_spec_id}"));
                Some(AppAction::ApproveVerificationCheck { check_spec_id })
            }
        }
    }
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/verification_flow_tests.rs"]
mod tests;
