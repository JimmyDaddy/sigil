use super::{AppAction, AppState, PaneFocus};
use crate::commands::UiCommand;
use sigil_kernel::CodeIntelStartup;

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

    pub(super) fn handle_ui_command(&mut self, command: UiCommand) -> bool {
        if command == UiCommand::OpenKeyboardHelp {
            self.open_keyboard_help();
            return true;
        }
        if self.pending_approval.is_some()
            || (self.active_pane == PaneFocus::Composer && !self.input.is_empty())
        {
            return false;
        }

        match command {
            UiCommand::FocusLatestToolCard => self.focus_latest_tool_card(),
            UiCommand::SelectNextToolCard => self.select_adjacent_tool_card(true),
            UiCommand::SelectPreviousToolCard => self.select_adjacent_tool_card(false),
            UiCommand::ToggleSelectedToolCard => self.toggle_selected_tool_card(),
            UiCommand::ClearToolCardFocus => self.clear_tool_card_focus(),
            UiCommand::SubmitPrompt
            | UiCommand::CancelOrQuit
            | UiCommand::ToggleWriteMode
            | UiCommand::ToggleThinking
            | UiCommand::OpenKeyboardHelp
            | UiCommand::OpenConfig
            | UiCommand::CompactNow
            | UiCommand::CheckChangedFilesDiagnostics => false,
        }
    }
}
