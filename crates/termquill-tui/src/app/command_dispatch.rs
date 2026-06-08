use super::*;

impl AppState {
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
            | UiCommand::CompactNow => false,
        }
    }
}
