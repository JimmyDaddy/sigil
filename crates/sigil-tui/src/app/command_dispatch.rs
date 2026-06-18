use super::{AppAction, AppState};
use crate::commands::UiCommand;
use sigil_kernel::{CodeIntelStartup, TerminalTaskProjection};

impl AppState {
    pub(super) fn request_changed_files_diagnostics(&mut self) -> Option<AppAction> {
        if self.pending_approval.is_some() {
            self.last_notice =
                Some("finish the pending approval before checking changes".to_owned());
            return None;
        }
        if self.is_busy {
            self.last_notice = Some("wait for the active run before checking changes".to_owned());
            return None;
        }
        if self.config_snapshot.as_ref().is_some_and(|config| {
            !config.code_intelligence.enabled
                || config.code_intelligence.startup == CodeIntelStartup::Off
        }) {
            self.last_notice = Some("code intelligence is off".to_owned());
            return None;
        }

        self.last_notice = Some("checking changed files".to_owned());
        self.push_event("code:check", "changed files");
        Some(AppAction::CheckChangedFilesDiagnostics)
    }

    pub(super) fn request_focused_terminal_task_cancel(&mut self) -> Option<AppAction> {
        let Some(task_id) = self.focused_terminal_task_id() else {
            self.pending_terminal_cancel_confirmation = None;
            self.last_notice = Some("focus a terminal task first".to_owned());
            return None;
        };
        if self.is_busy {
            self.pending_terminal_cancel_confirmation = None;
            self.last_notice =
                Some("wait for the active run before cancelling terminal task".to_owned());
            return None;
        }
        let projection = TerminalTaskProjection::from_entries(&self.current_session_entries);
        let Some(task) = projection.tasks.values().find(|task| {
            task.handle.task_id.as_str() == task_id.as_str() && task.status.is_active()
        }) else {
            self.pending_terminal_cancel_confirmation = None;
            self.last_notice = Some(format!("terminal task {task_id} is not running"));
            return None;
        };
        if self
            .pending_terminal_cancel_confirmation
            .as_deref()
            .is_some_and(|pending| pending == task_id)
        {
            self.pending_terminal_cancel_confirmation = None;
            self.last_notice = Some(format!("cancelling terminal task {task_id}"));
            self.push_timeline(
                super::TimelineRole::Notice,
                format!("Cancel requested for terminal task {task_id}."),
            );
            return Some(AppAction::CancelTerminalTask { task_id });
        }

        self.pending_terminal_cancel_confirmation = Some(task_id.clone());
        self.last_notice = Some(format!("Alt-X again to cancel terminal task {task_id}"));
        self.push_timeline(
            super::TimelineRole::Notice,
            format!(
                "Press Alt-X again to cancel terminal task {}.",
                task.handle.task_id.as_str()
            ),
        );
        None
    }

    pub(crate) fn focused_terminal_task_id(&self) -> Option<String> {
        self.selected_tool_activity_key
            .as_deref()
            .and_then(|key| key.strip_prefix("terminal_task:"))
            .map(str::to_owned)
    }

    pub(super) fn handle_ui_command(&mut self, command: UiCommand) -> bool {
        if command == UiCommand::OpenKeyboardHelp {
            self.open_keyboard_help();
            return true;
        }
        if self.pending_approval.is_some() || !self.input.is_empty() {
            return false;
        }

        match command {
            UiCommand::FocusLatestToolCard => self.focus_latest_tool_card(),
            UiCommand::SelectNextToolCard => self.select_adjacent_tool_card(true),
            UiCommand::SelectPreviousToolCard => self.select_adjacent_tool_card(false),
            UiCommand::ToggleSelectedToolCard => self.toggle_selected_tool_card(),
            UiCommand::ClearToolCardFocus => self.clear_tool_card_focus(),
            UiCommand::CancelFocusedTerminalTask => false,
            UiCommand::SubmitPrompt
            | UiCommand::SubmitPlan
            | UiCommand::CancelOrQuit
            | UiCommand::ToggleWriteMode
            | UiCommand::ToggleThinking
            | UiCommand::OpenKeyboardHelp
            | UiCommand::OpenConfig
            | UiCommand::OpenDoctor
            | UiCommand::StartNewSession
            | UiCommand::CompactNow
            | UiCommand::CheckChangedFilesDiagnostics => false,
        }
    }
}
