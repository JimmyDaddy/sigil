use super::{AppAction, AppState};
use crate::commands::UiCommand;
use sigil_kernel::{CodeIntelStartup, TerminalTaskProjection};

impl AppState {
    pub(super) fn request_active_task_pause(&mut self) -> Option<AppAction> {
        if !self.runtime.is_busy {
            self.last_notice = Some("no active task to pause".to_owned());
            return None;
        }
        let projection =
            sigil_kernel::TaskStateProjection::from_entries(&self.session_browser.current_entries);
        let Some(task) = projection.latest_task() else {
            self.last_notice = Some("the active run is not a durable task".to_owned());
            return None;
        };
        if !matches!(
            task.status,
            sigil_kernel::TaskRunStatus::Started | sigil_kernel::TaskRunStatus::Running
        ) {
            self.last_notice = Some(format!(
                "task {} is already {}",
                task.task_id.as_str(),
                super::task_sidebar::task_run_status_label(task.status)
            ));
            return None;
        }
        let Some(plan_version) = task.latest_plan_version else {
            self.last_notice =
                Some("task planning is still in progress; use Ctrl-C to cancel".to_owned());
            return None;
        };
        let request = sigil_kernel::TaskPauseRequest::new(task.task_id.clone(), plan_version);
        self.last_notice = Some(format!("pausing task {}", task.task_id.as_str()));
        self.push_timeline(
            super::TimelineRole::Notice,
            format!("Pause requested for task {}.", task.task_id.as_str()),
        );
        Some(AppAction::PauseTask { request })
    }

    pub(super) fn request_changed_files_diagnostics(&mut self) -> Option<AppAction> {
        if self.approval.has_actionable_pending() {
            self.last_notice =
                Some("finish the pending approval before checking changes".to_owned());
            return None;
        }
        if self.runtime.is_busy {
            self.last_notice = Some("wait for the active run before checking changes".to_owned());
            return None;
        }
        if self.config_snapshot.as_ref().is_some_and(|config| {
            !config.code_intelligence.enabled
                || config.code_intelligence.server_startup == CodeIntelStartup::Off
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
        let projection =
            TerminalTaskProjection::from_entries(&self.session_browser.current_entries);
        let Some(task) = projection.tasks.values().find(|task| {
            task.handle.task_id.as_str() == task_id.as_str() && task.status.is_active()
        }) else {
            self.pending_terminal_cancel_confirmation = None;
            self.last_notice = Some(format!("terminal task {task_id} is not running"));
            return None;
        };
        let Some(identity) = self.terminal_task_control_identities.get(&task_id).cloned() else {
            self.pending_terminal_cancel_confirmation = None;
            self.last_notice = Some(format!(
                "terminal task {task_id} no longer has a live owner route"
            ));
            return None;
        };
        if identity.expected_generation != task.generation {
            self.pending_terminal_cancel_confirmation = None;
            self.last_notice = Some(format!(
                "terminal task {task_id} changed; review its latest state before cancelling"
            ));
            return None;
        }
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
            return Some(AppAction::CancelTerminalTask { identity });
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
        self.timeline_state
            .selected_tool_activity_key
            .as_deref()
            .and_then(|key| key.strip_prefix("terminal_task:"))
            .map(str::to_owned)
    }

    pub(super) fn handle_ui_command(&mut self, command: UiCommand) -> bool {
        if command == UiCommand::OpenKeyboardHelp {
            self.open_keyboard_help();
            return true;
        }
        if command == UiCommand::ToggleInfoRailDetail {
            self.toggle_info_rail_detail();
            return true;
        }
        if command == UiCommand::ToggleInfoRailVisibility {
            self.toggle_info_rail_visibility();
            return true;
        }
        if command == UiCommand::FocusVerificationCard {
            return self.focus_verification_card();
        }
        if matches!(
            command,
            UiCommand::OpenCheckpointRestore | UiCommand::OpenIntentStack
        ) {
            return false;
        }
        if command == UiCommand::CycleAgentView {
            if self.approval.has_actionable_pending() {
                self.last_notice =
                    Some("finish the pending approval before switching agents".to_owned());
                return false;
            }
            return self.cycle_agent_view(false);
        }
        if command == UiCommand::CycleAgentViewPrevious {
            if self.approval.has_actionable_pending() {
                self.last_notice =
                    Some("finish the pending approval before switching agents".to_owned());
                return false;
            }
            return self.cycle_agent_view(true);
        }
        if self.approval.has_actionable_pending() || !self.composer.input.is_empty() {
            return false;
        }

        match command {
            UiCommand::FocusLatestToolCard => self.focus_latest_tool_card(),
            UiCommand::SelectNextToolCard => self.select_adjacent_tool_card(true),
            UiCommand::SelectPreviousToolCard => self.select_adjacent_tool_card(false),
            UiCommand::ToggleSelectedToolCard => {
                if self.has_tool_cards() {
                    self.toggle_selected_tool_card()
                } else {
                    self.toggle_latest_diagram_entry()
                }
            }
            UiCommand::ReadNextToolArtifactPage => self.request_selected_tool_artifact_next_page(),
            UiCommand::SearchToolArtifact => self.open_selected_tool_artifact_search(),
            UiCommand::ClearToolCardFocus => self.clear_tool_card_focus(),
            UiCommand::CancelFocusedTerminalTask => false,
            UiCommand::SubmitPrompt
            | UiCommand::CopyTranscript
            | UiCommand::EnterPlanMode
            | UiCommand::SubmitTask
            | UiCommand::PauseActiveTask
            | UiCommand::CancelOrQuit
            | UiCommand::ToggleWriteMode
            | UiCommand::ToggleThinking
            | UiCommand::ToggleInfoRailVisibility
            | UiCommand::ToggleInfoRailDetail
            | UiCommand::OpenKeyboardHelp
            | UiCommand::OpenConfig
            | UiCommand::OpenDoctor
            | UiCommand::OpenFeedback
            | UiCommand::CheckForUpdate
            | UiCommand::StartNewSession
            | UiCommand::CompactContext
            | UiCommand::CycleAgentView
            | UiCommand::CycleAgentViewPrevious
            | UiCommand::CheckChangedFilesDiagnostics => false,
            UiCommand::FocusVerificationCard
            | UiCommand::OpenCheckpointRestore
            | UiCommand::OpenIntentStack => false,
        }
    }
}
