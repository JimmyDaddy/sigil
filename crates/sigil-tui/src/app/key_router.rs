use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{
    AppAction, AppState, ApprovalAction, PaneFocus, QueueMoveDirection, SidebarCard,
    approval_flow::spawn_agent_background_args_json,
    formatting::normalize_command_prefix_character, has_alt_without_control,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InputContext {
    ApprovalModal,
    ComposerQueuePanel,
    ComposerAgentPanel,
    ActivityAgentList,
    ActivitySidebar,
    Composer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RoutedKeyCommand {
    QueueActionNext,
    QueueActionPrevious,
    QueueSelectionNext,
    QueueSelectionPrevious,
    QueueMoveUp,
    QueueMoveDown,
    QueueExecute,
    QueueCancel,
    QueueBlur,
    QueueInsertCharacter(char),
    AgentSelectionNext,
    AgentSelectionPrevious,
    AgentActivate,
    AgentClose,
    AgentMessage,
    AgentBlur,
    ActivityAgentNext,
    ActivityAgentPrevious,
    ActivityAgentActivate,
    ApprovalAllowOnce,
    ApprovalDeny,
    ApprovalBackground,
    ApprovalSelect,
    ApprovalActionNext,
    ApprovalActionPrevious,
    ApprovalToggleMetadata,
    ApprovalPreviousHunk,
    ApprovalNextHunk,
    ApprovalPreviousFile,
    ApprovalNextFile,
    ApprovalDiffMode,
    ApprovalScrollUp,
    ApprovalScrollDown,
    ApprovalPageUp,
    ApprovalPageDown,
    ApprovalHome,
    ApprovalEnd,
    ApprovalSlashComposer,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct KeyBindingView {
    pub(crate) context: InputContext,
    pub(crate) key: &'static str,
    pub(crate) command: RoutedKeyCommand,
}

impl AppState {
    pub(super) fn handle_key_router_event(
        &mut self,
        key: KeyEvent,
    ) -> anyhow::Result<Option<Option<AppAction>>> {
        let context = resolve_input_context(self, key);
        let Some(command) = resolve_binding(context, key) else {
            return Ok(None);
        };
        Ok(Some(self.execute_routed_key_command(command)?))
    }

    fn execute_routed_key_command(
        &mut self,
        command: RoutedKeyCommand,
    ) -> anyhow::Result<Option<AppAction>> {
        let action = match command {
            RoutedKeyCommand::QueueActionNext => {
                self.cycle_composer_queue_action(true);
                None
            }
            RoutedKeyCommand::QueueActionPrevious => {
                self.cycle_composer_queue_action(false);
                None
            }
            RoutedKeyCommand::QueueSelectionNext => {
                self.move_composer_queue_selection(true);
                None
            }
            RoutedKeyCommand::QueueSelectionPrevious => {
                self.move_composer_queue_selection(false);
                None
            }
            RoutedKeyCommand::QueueMoveUp => self.move_selected_queue_item(QueueMoveDirection::Up),
            RoutedKeyCommand::QueueMoveDown => {
                self.move_selected_queue_item(QueueMoveDirection::Down)
            }
            RoutedKeyCommand::QueueExecute => self.execute_selected_queue_action(),
            RoutedKeyCommand::QueueCancel => self.cancel_selected_queue_item(),
            RoutedKeyCommand::QueueBlur => {
                self.blur_composer_queue_panel();
                self.active_pane = PaneFocus::Composer;
                None
            }
            RoutedKeyCommand::QueueInsertCharacter(character) => {
                self.blur_composer_queue_panel();
                self.active_pane = PaneFocus::Composer;
                let normalized = if normalize_command_prefix_character(character).is_some()
                    && self.composer.input.trim().is_empty()
                {
                    '/'
                } else {
                    character
                };
                self.insert_input_character(normalized);
                self.reset_input_history_navigation();
                self.reset_slash_selector();
                None
            }
            RoutedKeyCommand::AgentSelectionNext => {
                if !self.composer.agent_panel_focused {
                    self.focus_composer_agent_panel();
                }
                self.move_composer_agent_selection(true);
                None
            }
            RoutedKeyCommand::AgentSelectionPrevious => {
                if !self.composer.agent_panel_focused {
                    self.focus_composer_agent_panel();
                }
                if !self.move_composer_agent_selection(false) {
                    self.blur_composer_agent_panel();
                }
                None
            }
            RoutedKeyCommand::AgentActivate => {
                self.activate_selected_agent_view();
                None
            }
            RoutedKeyCommand::AgentClose => self.close_selected_agent_from_panel()?,
            RoutedKeyCommand::AgentMessage => {
                self.begin_message_selected_agent_from_panel();
                None
            }
            RoutedKeyCommand::AgentBlur => {
                self.blur_composer_agent_panel();
                None
            }
            RoutedKeyCommand::ActivityAgentNext => {
                self.move_activity_agent_selection(true);
                None
            }
            RoutedKeyCommand::ActivityAgentPrevious => {
                self.move_activity_agent_selection(false);
                None
            }
            RoutedKeyCommand::ActivityAgentActivate => {
                self.activate_selected_agent_view();
                None
            }
            RoutedKeyCommand::ApprovalAllowOnce => self.approval_decision(true),
            RoutedKeyCommand::ApprovalDeny => self.approval_decision(false),
            RoutedKeyCommand::ApprovalBackground => self.approval_background_decision(),
            RoutedKeyCommand::ApprovalSelect => self.approval_selected_decision(),
            RoutedKeyCommand::ApprovalActionNext => {
                self.move_approval_action(true);
                None
            }
            RoutedKeyCommand::ApprovalActionPrevious => {
                self.move_approval_action(false);
                None
            }
            RoutedKeyCommand::ApprovalToggleMetadata => {
                self.toggle_approval_metadata();
                None
            }
            RoutedKeyCommand::ApprovalPreviousHunk => {
                self.jump_approval_hunk(false);
                None
            }
            RoutedKeyCommand::ApprovalNextHunk => {
                self.jump_approval_hunk(true);
                None
            }
            RoutedKeyCommand::ApprovalPreviousFile => {
                self.switch_approval_file(false);
                None
            }
            RoutedKeyCommand::ApprovalNextFile => {
                self.switch_approval_file(true);
                None
            }
            RoutedKeyCommand::ApprovalDiffMode => {
                self.cycle_approval_diff_mode();
                None
            }
            RoutedKeyCommand::ApprovalScrollUp => {
                self.scroll_active_pane(1);
                None
            }
            RoutedKeyCommand::ApprovalScrollDown => {
                self.unscroll_active_pane(1);
                None
            }
            RoutedKeyCommand::ApprovalPageUp => {
                self.scroll_active_pane(8);
                None
            }
            RoutedKeyCommand::ApprovalPageDown => {
                self.unscroll_active_pane(8);
                None
            }
            RoutedKeyCommand::ApprovalHome => {
                self.scroll_active_pane(usize::MAX / 2);
                None
            }
            RoutedKeyCommand::ApprovalEnd => {
                self.unscroll_active_pane(usize::MAX / 2);
                None
            }
            RoutedKeyCommand::ApprovalSlashComposer => {
                self.active_pane = PaneFocus::Composer;
                self.insert_input_character('/');
                self.reset_input_history_navigation();
                self.reset_slash_selector();
                None
            }
        };
        Ok(action)
    }

    fn approval_decision(&self, approved: bool) -> Option<AppAction> {
        self.approval
            .pending
            .as_ref()
            .map(|pending| AppAction::ApprovalDecision {
                call_id: pending.call.id.clone(),
                approved,
            })
    }

    fn approval_background_decision(&self) -> Option<AppAction> {
        let pending = self.approval.pending.as_ref()?;
        let args_json =
            spawn_agent_background_args_json(&pending.call.name, &pending.call.args_json)?;
        Some(AppAction::ApprovalDecisionWithArgs {
            call_id: pending.call.id.clone(),
            args_json,
        })
    }

    fn approval_selected_decision(&self) -> Option<AppAction> {
        let pending = self.approval.pending.as_ref()?;
        let selected = self
            .approval
            .selected_action
            .normalized(pending.session_grant_available);
        Some(match selected {
            ApprovalAction::AllowOnce => AppAction::ApprovalDecision {
                call_id: pending.call.id.clone(),
                approved: true,
            },
            ApprovalAction::AllowSession => AppAction::ApprovalSessionDecision {
                call_id: pending.call.id.clone(),
            },
            ApprovalAction::Deny => AppAction::ApprovalDecision {
                call_id: pending.call.id.clone(),
                approved: false,
            },
        })
    }

    fn move_approval_action(&mut self, forward: bool) {
        let session_grant_available = self
            .approval
            .pending
            .as_ref()
            .is_some_and(|pending| pending.session_grant_available);
        self.approval.selected_action = self
            .approval
            .selected_action
            .next(session_grant_available, forward);
        self.push_event("approval:action", self.approval.selected_action.label());
    }

    fn move_activity_agent_selection(&mut self, next: bool) {
        let rows = self.agent_sidebar_rows();
        if rows.is_empty() {
            return;
        }
        let last = rows.len().saturating_sub(1);
        self.agent_panel.selected = if next {
            (self.agent_panel.selected + 1).min(last)
        } else {
            self.agent_panel.selected.saturating_sub(1)
        };
    }
}

pub(crate) fn resolve_input_context(app: &AppState, key: KeyEvent) -> InputContext {
    if app.approval.pending.is_some() {
        return InputContext::ApprovalModal;
    }
    if app.composer.queue_panel_focused {
        return InputContext::ComposerQueuePanel;
    }
    if app.composer.agent_panel_focused {
        return InputContext::ComposerAgentPanel;
    }
    if app.active_pane == PaneFocus::Composer
        && app.composer.input.trim().is_empty()
        && !app.has_slash_selector()
        && app.composer_agent_rows().len() > 1
        && is_implicit_composer_agent_panel_key(key)
        && !composer_history_should_handle_key(app, key)
    {
        return InputContext::ComposerAgentPanel;
    }
    if app.active_pane == PaneFocus::Activity
        && app.sidebar_selected_card == SidebarCard::Agents
        && app.agent_sidebar_rows().len() > 1
    {
        return InputContext::ActivityAgentList;
    }
    if app.active_pane == PaneFocus::Activity {
        return InputContext::ActivitySidebar;
    }
    InputContext::Composer
}

fn is_implicit_composer_agent_panel_key(key: KeyEvent) -> bool {
    matches!(
        key.code,
        KeyCode::Up | KeyCode::Down | KeyCode::Enter | KeyCode::Esc
    ) && key.modifiers.is_empty()
}

fn composer_history_should_handle_key(app: &AppState, key: KeyEvent) -> bool {
    if !key.modifiers.is_empty() {
        return false;
    }
    match key.code {
        KeyCode::Up => true,
        KeyCode::Down => app.composer.input_history_index.is_some(),
        _ => false,
    }
}

pub(crate) fn resolve_binding(context: InputContext, key: KeyEvent) -> Option<RoutedKeyCommand> {
    match context {
        InputContext::ComposerQueuePanel => resolve_queue_binding(key),
        InputContext::ComposerAgentPanel => resolve_agent_panel_binding(key),
        InputContext::ActivityAgentList => resolve_activity_agent_binding(key),
        InputContext::ApprovalModal => resolve_approval_binding(key),
        InputContext::ActivitySidebar | InputContext::Composer => None,
    }
}

fn resolve_approval_binding(key: KeyEvent) -> Option<RoutedKeyCommand> {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return None;
    }
    match key.code {
        KeyCode::Char('y' | 'Y') => Some(RoutedKeyCommand::ApprovalAllowOnce),
        KeyCode::Char('n' | 'N') => Some(RoutedKeyCommand::ApprovalDeny),
        KeyCode::Char('b' | 'B') => Some(RoutedKeyCommand::ApprovalBackground),
        KeyCode::Enter if key.modifiers.is_empty() => Some(RoutedKeyCommand::ApprovalSelect),
        KeyCode::Left | KeyCode::BackTab => Some(RoutedKeyCommand::ApprovalActionPrevious),
        KeyCode::Right | KeyCode::Tab => Some(RoutedKeyCommand::ApprovalActionNext),
        KeyCode::Char('m' | 'M') => Some(RoutedKeyCommand::ApprovalToggleMetadata),
        KeyCode::Char('[') => Some(RoutedKeyCommand::ApprovalPreviousHunk),
        KeyCode::Char(']') => Some(RoutedKeyCommand::ApprovalNextHunk),
        KeyCode::Char(',') => Some(RoutedKeyCommand::ApprovalPreviousFile),
        KeyCode::Char('.') => Some(RoutedKeyCommand::ApprovalNextFile),
        KeyCode::Char('v' | 'V') => Some(RoutedKeyCommand::ApprovalDiffMode),
        KeyCode::Up => Some(RoutedKeyCommand::ApprovalScrollUp),
        KeyCode::Down => Some(RoutedKeyCommand::ApprovalScrollDown),
        KeyCode::PageUp => Some(RoutedKeyCommand::ApprovalPageUp),
        KeyCode::PageDown => Some(RoutedKeyCommand::ApprovalPageDown),
        KeyCode::Home => Some(RoutedKeyCommand::ApprovalHome),
        KeyCode::End => Some(RoutedKeyCommand::ApprovalEnd),
        KeyCode::Char(character) if normalize_command_prefix_character(character).is_some() => {
            Some(RoutedKeyCommand::ApprovalSlashComposer)
        }
        _ => None,
    }
}

fn resolve_queue_binding(key: KeyEvent) -> Option<RoutedKeyCommand> {
    match key.code {
        KeyCode::Tab | KeyCode::BackTab => Some(RoutedKeyCommand::QueueBlur),
        KeyCode::Right if key.modifiers.is_empty() => Some(RoutedKeyCommand::QueueActionNext),
        KeyCode::Left if key.modifiers.is_empty() => Some(RoutedKeyCommand::QueueActionPrevious),
        KeyCode::Up if has_alt_without_control(key) => Some(RoutedKeyCommand::QueueMoveUp),
        KeyCode::Down if has_alt_without_control(key) => Some(RoutedKeyCommand::QueueMoveDown),
        KeyCode::Up if key.modifiers.is_empty() => Some(RoutedKeyCommand::QueueSelectionPrevious),
        KeyCode::Down if key.modifiers.is_empty() => Some(RoutedKeyCommand::QueueSelectionNext),
        KeyCode::Esc if key.modifiers.is_empty() => Some(RoutedKeyCommand::QueueBlur),
        KeyCode::Enter if key.modifiers.is_empty() => Some(RoutedKeyCommand::QueueExecute),
        KeyCode::Backspace | KeyCode::Delete if key.modifiers.is_empty() => {
            Some(RoutedKeyCommand::QueueCancel)
        }
        KeyCode::Char(character) if key.modifiers.is_empty() => {
            Some(RoutedKeyCommand::QueueInsertCharacter(character))
        }
        _ => None,
    }
}

fn resolve_agent_panel_binding(key: KeyEvent) -> Option<RoutedKeyCommand> {
    match key.code {
        KeyCode::Up if key.modifiers.is_empty() => Some(RoutedKeyCommand::AgentSelectionPrevious),
        KeyCode::Down if key.modifiers.is_empty() => Some(RoutedKeyCommand::AgentSelectionNext),
        KeyCode::Esc if key.modifiers.is_empty() => Some(RoutedKeyCommand::AgentBlur),
        KeyCode::Enter if key.modifiers.is_empty() => Some(RoutedKeyCommand::AgentActivate),
        KeyCode::Char('c' | 'C') if has_alt_without_control(key) => {
            Some(RoutedKeyCommand::AgentClose)
        }
        KeyCode::Char('m' | 'M') if has_alt_without_control(key) => {
            Some(RoutedKeyCommand::AgentMessage)
        }
        _ => None,
    }
}

fn resolve_activity_agent_binding(key: KeyEvent) -> Option<RoutedKeyCommand> {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return None;
    }
    match key.code {
        KeyCode::Up => Some(RoutedKeyCommand::ActivityAgentPrevious),
        KeyCode::Down => Some(RoutedKeyCommand::ActivityAgentNext),
        KeyCode::Enter => Some(RoutedKeyCommand::ActivityAgentActivate),
        _ => None,
    }
}

#[cfg(test)]
pub(crate) fn key_binding_snapshot() -> Vec<KeyBindingView> {
    vec![
        KeyBindingView {
            context: InputContext::ApprovalModal,
            key: "Enter",
            command: RoutedKeyCommand::ApprovalSelect,
        },
        KeyBindingView {
            context: InputContext::ApprovalModal,
            key: "Tab",
            command: RoutedKeyCommand::ApprovalActionNext,
        },
        KeyBindingView {
            context: InputContext::ComposerQueuePanel,
            key: "Down",
            command: RoutedKeyCommand::QueueSelectionNext,
        },
        KeyBindingView {
            context: InputContext::ComposerQueuePanel,
            key: "Right",
            command: RoutedKeyCommand::QueueActionNext,
        },
        KeyBindingView {
            context: InputContext::ComposerQueuePanel,
            key: "Tab",
            command: RoutedKeyCommand::QueueBlur,
        },
        KeyBindingView {
            context: InputContext::ComposerAgentPanel,
            key: "Down",
            command: RoutedKeyCommand::AgentSelectionNext,
        },
        KeyBindingView {
            context: InputContext::ActivityAgentList,
            key: "Down",
            command: RoutedKeyCommand::ActivityAgentNext,
        },
        KeyBindingView {
            context: InputContext::ActivityAgentList,
            key: "Enter",
            command: RoutedKeyCommand::ActivityAgentActivate,
        },
    ]
}

#[cfg(test)]
#[path = "tests/key_router_tests.rs"]
mod tests;
