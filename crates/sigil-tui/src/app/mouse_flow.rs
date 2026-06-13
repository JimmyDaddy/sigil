use super::{AppAction, AppState, PaneFocus, SetupField};
use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::config_panel::{ConfigField, ConfigFooterAction, ConfigSection};

impl AppState {
    pub fn handle_mouse_event(
        &mut self,
        input: crate::mouse::MouseInput,
        layout: &crate::ui::LayoutSnapshot,
    ) -> Result<crate::mouse::AppMouseOutcome> {
        let target = layout.hit_target(input.column, input.row);
        match input.kind {
            crate::mouse::MouseInputKind::ScrollUp => self.handle_mouse_scroll_target(target, true),
            crate::mouse::MouseInputKind::ScrollDown => {
                self.handle_mouse_scroll_target(target, false)
            }
            crate::mouse::MouseInputKind::LeftDown => match target {
                crate::mouse::HitTarget::SetupField { index } if self.is_setup_mode() => {
                    self.handle_setup_mouse_field(index)
                }
                crate::mouse::HitTarget::ConfigSection { index } if self.is_config_mode() => {
                    Ok(self.handle_config_mouse_section(index))
                }
                crate::mouse::HitTarget::ConfigField { index } if self.is_config_mode() => {
                    self.handle_config_mouse_field(index)
                }
                crate::mouse::HitTarget::ConfigFooterAction { index } if self.is_config_mode() => {
                    self.handle_config_mouse_footer_action(index)
                }
                crate::mouse::HitTarget::ApprovalFileRow { index }
                    if self.pending_approval.is_some() =>
                {
                    if self.select_approval_file_index(index) {
                        Ok(crate::mouse::AppMouseOutcome::Redraw)
                    } else {
                        Ok(crate::mouse::AppMouseOutcome::Noop)
                    }
                }
                crate::mouse::HitTarget::ApprovalHunkPrevious
                    if self.pending_approval.is_some() =>
                {
                    if self.jump_approval_hunk(false) {
                        Ok(crate::mouse::AppMouseOutcome::Redraw)
                    } else {
                        Ok(crate::mouse::AppMouseOutcome::Noop)
                    }
                }
                crate::mouse::HitTarget::ApprovalHunkNext if self.pending_approval.is_some() => {
                    if self.jump_approval_hunk(true) {
                        Ok(crate::mouse::AppMouseOutcome::Redraw)
                    } else {
                        Ok(crate::mouse::AppMouseOutcome::Noop)
                    }
                }
                crate::mouse::HitTarget::ApprovalDiffViewToggle
                    if self.pending_approval.is_some() =>
                {
                    self.cycle_approval_diff_mode();
                    Ok(crate::mouse::AppMouseOutcome::Redraw)
                }
                crate::mouse::HitTarget::ApprovalMetadataToggle
                    if self.pending_approval.is_some() =>
                {
                    self.toggle_approval_metadata();
                    Ok(crate::mouse::AppMouseOutcome::Redraw)
                }
                crate::mouse::HitTarget::ApprovalAction { approved }
                    if self.pending_approval.is_some() =>
                {
                    let call_id = self
                        .pending_approval
                        .as_ref()
                        .map(|pending| pending.call.id.clone())
                        .expect("approval action target requires pending approval");
                    Ok(crate::mouse::AppMouseOutcome::Action(
                        crate::app::AppAction::ApprovalDecision { call_id, approved },
                    ))
                }
                crate::mouse::HitTarget::SlashCandidate { index }
                    if self.pending_approval.is_none() =>
                {
                    self.clear_timeline_text_selection();
                    self.active_pane = PaneFocus::Composer;
                    match self.handle_mouse_slash_candidate(index)? {
                        Some(action) => Ok(crate::mouse::AppMouseOutcome::Action(action)),
                        None => Ok(crate::mouse::AppMouseOutcome::Redraw),
                    }
                }
                crate::mouse::HitTarget::ToolCard { entry_index }
                    if self.pending_approval.is_none() =>
                {
                    if let Some(position) = layout.live_text_position_at(input.column, input.row) {
                        self.begin_timeline_text_selection_at(position.line_index, position.column);
                    } else {
                        self.clear_timeline_text_selection();
                    }
                    if self.select_tool_activity_entry(entry_index) {
                        Ok(crate::mouse::AppMouseOutcome::Redraw)
                    } else {
                        Ok(crate::mouse::AppMouseOutcome::Noop)
                    }
                }
                crate::mouse::HitTarget::Composer if self.pending_approval.is_none() => {
                    self.clear_timeline_text_selection();
                    self.active_pane = PaneFocus::Composer;
                    Ok(crate::mouse::AppMouseOutcome::Redraw)
                }
                crate::mouse::HitTarget::InfoRail if self.pending_approval.is_none() => {
                    self.clear_timeline_text_selection();
                    self.active_pane = PaneFocus::Activity;
                    Ok(crate::mouse::AppMouseOutcome::Redraw)
                }
                _ if self.pending_approval.is_none() => {
                    if let Some(position) = layout.live_text_position_at(input.column, input.row) {
                        if self
                            .begin_timeline_text_selection_at(position.line_index, position.column)
                        {
                            Ok(crate::mouse::AppMouseOutcome::Redraw)
                        } else {
                            Ok(crate::mouse::AppMouseOutcome::Noop)
                        }
                    } else if self.clear_timeline_text_selection() {
                        Ok(crate::mouse::AppMouseOutcome::Redraw)
                    } else {
                        Ok(crate::mouse::AppMouseOutcome::Noop)
                    }
                }
                _ => Ok(crate::mouse::AppMouseOutcome::Noop),
            },
            crate::mouse::MouseInputKind::Drag if self.pending_approval.is_none() => {
                if let Some(position) = layout.live_text_position_at(input.column, input.row) {
                    if self.update_timeline_text_selection_at(position.line_index, position.column)
                    {
                        Ok(crate::mouse::AppMouseOutcome::Redraw)
                    } else {
                        Ok(crate::mouse::AppMouseOutcome::Noop)
                    }
                } else {
                    Ok(crate::mouse::AppMouseOutcome::Noop)
                }
            }
            crate::mouse::MouseInputKind::LeftUp if self.pending_approval.is_none() => {
                if self.finish_timeline_text_selection() {
                    Ok(crate::mouse::AppMouseOutcome::Redraw)
                } else {
                    Ok(crate::mouse::AppMouseOutcome::Noop)
                }
            }
            _ => Ok(crate::mouse::AppMouseOutcome::Noop),
        }
    }

    fn handle_mouse_scroll_target(
        &mut self,
        target: crate::mouse::HitTarget,
        upward: bool,
    ) -> Result<crate::mouse::AppMouseOutcome> {
        if self.pending_approval.is_some() {
            return match target {
                crate::mouse::HitTarget::ApprovalModal
                | crate::mouse::HitTarget::ApprovalDiffArea
                | crate::mouse::HitTarget::ApprovalFileRow { .. }
                | crate::mouse::HitTarget::ApprovalAction { .. }
                | crate::mouse::HitTarget::ApprovalHunkPrevious
                | crate::mouse::HitTarget::ApprovalHunkNext
                | crate::mouse::HitTarget::ApprovalDiffViewToggle
                | crate::mouse::HitTarget::ApprovalMetadataToggle => {
                    self.scroll_approval_with_mouse(upward);
                    Ok(crate::mouse::AppMouseOutcome::Redraw)
                }
                _ => Ok(crate::mouse::AppMouseOutcome::Noop),
            };
        }

        match target {
            crate::mouse::HitTarget::ApprovalFileRow { .. }
            | crate::mouse::HitTarget::ApprovalAction { .. }
            | crate::mouse::HitTarget::ApprovalDiffArea
            | crate::mouse::HitTarget::ApprovalHunkPrevious
            | crate::mouse::HitTarget::ApprovalHunkNext
            | crate::mouse::HitTarget::ApprovalDiffViewToggle
            | crate::mouse::HitTarget::ApprovalMetadataToggle
            | crate::mouse::HitTarget::SetupField { .. }
            | crate::mouse::HitTarget::ConfigSection { .. }
            | crate::mouse::HitTarget::ConfigField { .. }
            | crate::mouse::HitTarget::ConfigFooterAction { .. } => {
                Ok(crate::mouse::AppMouseOutcome::Noop)
            }
            crate::mouse::HitTarget::InfoRail => {
                self.active_pane = PaneFocus::Activity;
                self.move_sidebar_selection(!upward);
                Ok(crate::mouse::AppMouseOutcome::Redraw)
            }
            crate::mouse::HitTarget::SlashCandidate { .. }
            | crate::mouse::HitTarget::SlashOverlay => {
                self.active_pane = PaneFocus::Composer;
                self.move_slash_selector(!upward);
                Ok(crate::mouse::AppMouseOutcome::Redraw)
            }
            crate::mouse::HitTarget::LivePanel | crate::mouse::HitTarget::Background => {
                self.handle_mouse_scroll(upward);
                Ok(crate::mouse::AppMouseOutcome::Redraw)
            }
            crate::mouse::HitTarget::ToolCard { .. } => {
                self.handle_mouse_scroll(upward);
                Ok(crate::mouse::AppMouseOutcome::Redraw)
            }
            crate::mouse::HitTarget::ApprovalModal => Ok(crate::mouse::AppMouseOutcome::Noop),
            crate::mouse::HitTarget::Composer => Ok(crate::mouse::AppMouseOutcome::Noop),
        }
    }

    fn handle_setup_mouse_field(&mut self, index: usize) -> Result<crate::mouse::AppMouseOutcome> {
        let Some(field) = SetupField::from_index(index) else {
            return Ok(crate::mouse::AppMouseOutcome::Noop);
        };
        let activate = self
            .setup_state
            .as_ref()
            .is_some_and(|state| state.selected_field == field)
            || field == SetupField::Save;
        let state = self
            .setup_state
            .as_mut()
            .expect("setup mouse target requires setup state");

        state.selected_field = field;
        self.last_notice = Some(format!("setup field {}", field.label()));
        if activate {
            return Ok(mouse_action_outcome(
                self.handle_setup_key_event(enter_key())?,
            ));
        }
        Ok(crate::mouse::AppMouseOutcome::Redraw)
    }

    fn handle_config_mouse_section(&mut self, index: usize) -> crate::mouse::AppMouseOutcome {
        let Some(section) = ConfigSection::from_flow_index(index) else {
            return crate::mouse::AppMouseOutcome::Noop;
        };
        let config_state = self
            .config_state
            .as_mut()
            .expect("config section mouse target requires config state");
        if config_state.selected_section == section && !config_state.footer_selected {
            return crate::mouse::AppMouseOutcome::Noop;
        }
        config_state.set_section(section);
        self.last_notice = Some(format!("step {}", section.title().to_lowercase()));
        crate::mouse::AppMouseOutcome::Redraw
    }

    fn handle_config_mouse_field(&mut self, index: usize) -> Result<crate::mouse::AppMouseOutcome> {
        let section = self
            .config_selected_section()
            .expect("config field mouse target requires config state");
        let Some(field) = ConfigField::field_for_section_index(section, index) else {
            return Ok(crate::mouse::AppMouseOutcome::Noop);
        };
        let activate = self
            .config_state
            .as_ref()
            .is_some_and(|state| !state.footer_selected && state.selected_field == Some(field));
        let config_state = self
            .config_state
            .as_mut()
            .expect("config field mouse target requires config state");
        if !config_state.focus_field(field) {
            return Ok(crate::mouse::AppMouseOutcome::Noop);
        }
        self.last_notice = Some(format!("config field {}", field.label()));
        if activate {
            return Ok(mouse_action_outcome(
                self.handle_config_key_event(enter_key())?,
            ));
        }
        Ok(crate::mouse::AppMouseOutcome::Redraw)
    }

    fn handle_config_mouse_footer_action(
        &mut self,
        index: usize,
    ) -> Result<crate::mouse::AppMouseOutcome> {
        let section = self
            .config_selected_section()
            .expect("config footer mouse target requires config state");
        let Some(action) = ConfigFooterAction::action_for_section_index(section, index) else {
            return Ok(crate::mouse::AppMouseOutcome::Noop);
        };
        let config_state = self
            .config_state
            .as_mut()
            .expect("config footer mouse target requires config state");
        config_state.focus_footer(action);
        self.last_notice = Some(format!("action {}", action.field_label()));
        Ok(mouse_action_outcome(
            self.handle_config_key_event(enter_key())?,
        ))
    }

    fn select_approval_file_index(&mut self, index: usize) -> bool {
        let Some(file_count) = self
            .pending_approval
            .as_ref()
            .and_then(|pending| pending.preview.as_ref())
            .map(|preview| preview.file_diffs.len())
        else {
            return false;
        };
        if index >= file_count || self.approval_selected_file_index == index {
            return false;
        }
        self.approval_selected_file_index = index;
        self.approval_selected_hunk_index = 0;
        self.approval_scroll_back = 0;
        true
    }

    fn scroll_approval_with_mouse(&mut self, upward: bool) {
        let delta = 3;
        if upward {
            self.approval_scroll_back = self.approval_scroll_back.saturating_sub(delta);
        } else {
            self.approval_scroll_back = self.approval_scroll_back.saturating_add(delta);
        }
    }
}

fn enter_key() -> KeyEvent {
    KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
}

fn mouse_action_outcome(action: Option<AppAction>) -> crate::mouse::AppMouseOutcome {
    match action {
        Some(action) => crate::mouse::AppMouseOutcome::Action(action),
        None => crate::mouse::AppMouseOutcome::Redraw,
    }
}
