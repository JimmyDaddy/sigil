use crossterm::event::{KeyCode, KeyEvent};

use super::{AppAction, AppState, PaneFocus, TimelineRole};
use crate::app::task_sidebar::VerificationCardAction;

impl AppState {
    pub(crate) fn verification_card_focused(&self) -> bool {
        self.review.verification_card_focused
            && self
                .task_strip_view()
                .is_some_and(|view| view.verification.is_some())
    }

    pub(crate) fn verification_inspect_open(&self) -> bool {
        self.verification_card_focused() && self.review.verification_inspect_open
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
        self.review.verification_card_focused = true;
        self.active_pane = PaneFocus::Activity;
        self.blur_composer_aux_panels();
        self.last_notice = Some("verification card focused".to_owned());
        true
    }

    pub(super) fn blur_verification_card(&mut self) {
        self.review.verification_card_focused = false;
        self.review.verification_inspect_open = false;
    }

    pub(super) fn handle_verification_card_key_event(
        &mut self,
        key: KeyEvent,
    ) -> Option<Option<AppAction>> {
        if !self.review.verification_card_focused || !key.modifiers.is_empty() {
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
                self.review.verification_inspect_open = !self.review.verification_inspect_open;
                self.last_notice = Some(if self.review.verification_inspect_open {
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
            VerificationCardAction::ReviewIntegration(request) => {
                self.review.integration_review_request = Some(request.clone());
                self.review.integration_review_diff_lines.clear();
                self.last_notice = Some(format!(
                    "loading exact integration diff for plan v{}",
                    request.plan_version
                ));
                Some(AppAction::ReviewTaskIntegration { request })
            }
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

    pub(super) fn clear_integration_review(&mut self) {
        self.review.integration_review_request = None;
        self.review.integration_review_diff_lines.clear();
    }

    pub(super) fn reconcile_integration_review(&mut self) {
        let Some(expected) = self.review.integration_review_request.as_ref() else {
            return;
        };
        let current =
            sigil_kernel::task_integration_review_product(&self.session_browser.current_entries);
        if current
            .as_ref()
            .is_none_or(|product| product.request != *expected)
        {
            self.clear_integration_review();
        }
    }
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/verification_flow_tests.rs"]
mod tests;
