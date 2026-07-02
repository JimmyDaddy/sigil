use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{
    AppAction, AppState, PaneFocus, QueueMoveDirection, SidebarCard, has_alt_without_control,
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
    AgentSelectionNext,
    AgentSelectionPrevious,
    AgentActivate,
    AgentClose,
    AgentMessage,
    AgentBlur,
    ActivityAgentNext,
    ActivityAgentPrevious,
    ActivityAgentActivate,
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
        let context = resolve_input_context(self);
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
                None
            }
            RoutedKeyCommand::AgentSelectionNext => {
                self.move_composer_agent_selection(true);
                None
            }
            RoutedKeyCommand::AgentSelectionPrevious => {
                if self.selected_composer_agent_is_first() {
                    if !self.focus_composer_queue_panel() {
                        self.blur_composer_agent_panel();
                    }
                } else {
                    self.move_composer_agent_selection(false);
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
        };
        Ok(action)
    }

    fn move_activity_agent_selection(&mut self, next: bool) {
        let rows = self.agent_sidebar_rows();
        if rows.is_empty() {
            return;
        }
        let last = rows.len().saturating_sub(1);
        self.sidebar_agent_selected = if next {
            (self.sidebar_agent_selected + 1).min(last)
        } else {
            self.sidebar_agent_selected.saturating_sub(1)
        };
    }
}

pub(crate) fn resolve_input_context(app: &AppState) -> InputContext {
    if app.approval.pending.is_some() {
        return InputContext::ApprovalModal;
    }
    if app.composer.queue_panel_focused {
        return InputContext::ComposerQueuePanel;
    }
    if app.composer.agent_panel_focused {
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

pub(crate) fn resolve_binding(context: InputContext, key: KeyEvent) -> Option<RoutedKeyCommand> {
    match context {
        InputContext::ComposerQueuePanel => resolve_queue_binding(key),
        InputContext::ComposerAgentPanel => resolve_agent_panel_binding(key),
        InputContext::ActivityAgentList => resolve_activity_agent_binding(key),
        InputContext::ApprovalModal | InputContext::ActivitySidebar | InputContext::Composer => {
            None
        }
    }
}

fn resolve_queue_binding(key: KeyEvent) -> Option<RoutedKeyCommand> {
    match key.code {
        KeyCode::Tab => Some(RoutedKeyCommand::QueueActionNext),
        KeyCode::BackTab => Some(RoutedKeyCommand::QueueActionPrevious),
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
        _ => None,
    }
}

fn resolve_agent_panel_binding(key: KeyEvent) -> Option<RoutedKeyCommand> {
    match key.code {
        KeyCode::Up if key.modifiers.is_empty() => Some(RoutedKeyCommand::AgentSelectionPrevious),
        KeyCode::Down if key.modifiers.is_empty() => Some(RoutedKeyCommand::AgentSelectionNext),
        KeyCode::Esc if key.modifiers.is_empty() => Some(RoutedKeyCommand::AgentBlur),
        KeyCode::Enter if key.modifiers.is_empty() => Some(RoutedKeyCommand::AgentActivate),
        KeyCode::Char('c' | 'C') if key.modifiers.is_empty() => Some(RoutedKeyCommand::AgentClose),
        KeyCode::Char('m' | 'M') if key.modifiers.is_empty() => {
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
            context: InputContext::ComposerQueuePanel,
            key: "Down",
            command: RoutedKeyCommand::QueueSelectionNext,
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
