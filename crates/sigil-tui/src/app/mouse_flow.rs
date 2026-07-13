use super::{AppAction, AppState, PaneFocus, SetupField};
use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::config_panel::{ConfigField, ConfigFooterAction, ConfigSection};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ToolCardMouseAnchor {
    pub(super) entry_line_offset: usize,
    pub(super) viewport_line_offset: usize,
    pub(super) visible_rows: usize,
}

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
            crate::mouse::MouseInputKind::LeftDown => {
                self.mark_mouse_left_down();
                self.handle_mouse_left_down_target(target, input, layout)
            }
            crate::mouse::MouseInputKind::Drag if self.approval.pending.is_none() => {
                self.mark_mouse_left_down();
                self.cancel_tool_card_body_click();
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
            crate::mouse::MouseInputKind::LeftUp if self.approval.pending.is_none() => {
                let had_left_down = self.take_mouse_left_down();
                if let Some(entry_index) = self.take_pending_tool_card_body_click(target) {
                    let anchor =
                        self.tool_card_mouse_anchor(entry_index, input.column, input.row, layout);
                    self.clear_timeline_text_selection();
                    if self.toggle_tool_activity_entry(entry_index) {
                        self.restore_tool_card_mouse_anchor(entry_index, anchor);
                        Ok(crate::mouse::AppMouseOutcome::Redraw)
                    } else {
                        Ok(crate::mouse::AppMouseOutcome::Noop)
                    }
                } else if self.finish_timeline_text_selection() {
                    self.cancel_tool_card_body_click();
                    Ok(crate::mouse::AppMouseOutcome::Redraw)
                } else if !had_left_down {
                    self.handle_mouse_left_up_click_fallback(target, input, layout)
                } else {
                    self.cancel_tool_card_body_click();
                    Ok(crate::mouse::AppMouseOutcome::Noop)
                }
            }
            crate::mouse::MouseInputKind::Moved => {
                if self.set_mouse_hover_target(hover_target_for(target)) {
                    Ok(crate::mouse::AppMouseOutcome::Redraw)
                } else {
                    Ok(crate::mouse::AppMouseOutcome::Noop)
                }
            }
            _ => Ok(crate::mouse::AppMouseOutcome::Noop),
        }
    }

    pub(super) fn tool_card_mouse_anchor(
        &self,
        entry_index: usize,
        column: u16,
        row: u16,
        layout: &crate::ui::LayoutSnapshot,
    ) -> Option<ToolCardMouseAnchor> {
        let position = layout.live_text_position_at(column, row)?;
        let visible_start = layout.live_text_rows.first()?.line_index;
        let entry_range = self.timeline_entry_render_range(entry_index)?;
        if !entry_range.contains(&position.line_index) {
            return None;
        }

        Some(ToolCardMouseAnchor {
            entry_line_offset: position.line_index.saturating_sub(entry_range.start),
            viewport_line_offset: position.line_index.saturating_sub(visible_start),
            visible_rows: layout.live_text_rows.len().max(1),
        })
    }

    pub(super) fn restore_tool_card_mouse_anchor(
        &mut self,
        entry_index: usize,
        anchor: Option<ToolCardMouseAnchor>,
    ) {
        let Some(anchor) = anchor else {
            return;
        };
        let Some(entry_range) = self.timeline_entry_render_range(entry_index) else {
            return;
        };
        let effective_len = self.effective_timeline_render_len();
        if effective_len == 0 || entry_range.is_empty() {
            return;
        }

        let entry_len = entry_range.end.saturating_sub(entry_range.start).max(1);
        let target_line = entry_range
            .start
            .saturating_add(anchor.entry_line_offset.min(entry_len.saturating_sub(1)))
            .min(effective_len.saturating_sub(1));
        let visible_start = target_line.saturating_sub(anchor.viewport_line_offset);
        let visible_end = visible_start
            .saturating_add(anchor.visible_rows.max(1))
            .min(effective_len);
        self.timeline_scroll_back = effective_len
            .saturating_sub(visible_end)
            .min(self.max_timeline_scroll_back());
    }

    fn handle_mouse_left_down_target(
        &mut self,
        target: crate::mouse::HitTarget,
        input: crate::mouse::MouseInput,
        layout: &crate::ui::LayoutSnapshot,
    ) -> Result<crate::mouse::AppMouseOutcome> {
        match target {
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
                if self.approval.pending.is_some() =>
            {
                if self.select_approval_file_index(index) {
                    Ok(crate::mouse::AppMouseOutcome::Redraw)
                } else {
                    Ok(crate::mouse::AppMouseOutcome::Noop)
                }
            }
            crate::mouse::HitTarget::ApprovalHunkPrevious if self.approval.pending.is_some() => {
                if self.jump_approval_hunk(false) {
                    Ok(crate::mouse::AppMouseOutcome::Redraw)
                } else {
                    Ok(crate::mouse::AppMouseOutcome::Noop)
                }
            }
            crate::mouse::HitTarget::ApprovalHunkNext if self.approval.pending.is_some() => {
                if self.jump_approval_hunk(true) {
                    Ok(crate::mouse::AppMouseOutcome::Redraw)
                } else {
                    Ok(crate::mouse::AppMouseOutcome::Noop)
                }
            }
            crate::mouse::HitTarget::ApprovalDiffViewToggle if self.approval.pending.is_some() => {
                self.cycle_approval_diff_mode();
                Ok(crate::mouse::AppMouseOutcome::Redraw)
            }
            crate::mouse::HitTarget::ApprovalMetadataToggle if self.approval.pending.is_some() => {
                self.toggle_approval_metadata();
                Ok(crate::mouse::AppMouseOutcome::Redraw)
            }
            crate::mouse::HitTarget::ApprovalAction { action }
                if self.approval.pending.is_some() =>
            {
                let call_id = self
                    .approval
                    .pending
                    .as_ref()
                    .map(|pending| pending.call.id.clone())
                    .expect("approval action target requires pending approval");
                Ok(crate::mouse::AppMouseOutcome::Action(match action {
                    crate::app::ApprovalAction::AllowOnce => {
                        crate::app::AppAction::ApprovalDecision {
                            call_id,
                            approved: true,
                        }
                    }
                    crate::app::ApprovalAction::AllowSession => {
                        crate::app::AppAction::ApprovalSessionDecision { call_id }
                    }
                    crate::app::ApprovalAction::Deny => crate::app::AppAction::ApprovalDecision {
                        call_id,
                        approved: false,
                    },
                }))
            }
            crate::mouse::HitTarget::SlashCandidate { index }
                if self.approval.pending.is_none() =>
            {
                self.click_slash_candidate(index)
            }
            crate::mouse::HitTarget::ToolCardHeader { entry_index }
            | crate::mouse::HitTarget::ToolCardHiddenPreview { entry_index }
                if self.approval.pending.is_none() =>
            {
                self.click_tool_card_toggle_target(entry_index, input, layout, target)
            }
            crate::mouse::HitTarget::ToolCard { entry_index }
                if self.approval.pending.is_none() =>
            {
                self.begin_tool_card_body_click(entry_index);
                self.set_mouse_hover_target(Some(target));
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
            crate::mouse::HitTarget::ThinkingBlock { entry_index }
                if self.approval.pending.is_none() =>
            {
                Ok(self.click_thinking_block(entry_index, target))
            }
            crate::mouse::HitTarget::VerificationCard if self.approval.pending.is_none() => {
                self.cancel_tool_card_body_click();
                self.set_mouse_hover_target(Some(target));
                self.clear_timeline_text_selection();
                if self.focus_verification_card() {
                    Ok(crate::mouse::AppMouseOutcome::Redraw)
                } else {
                    Ok(crate::mouse::AppMouseOutcome::Noop)
                }
            }
            crate::mouse::HitTarget::Composer if self.approval.pending.is_none() => {
                Ok(self.click_composer(input, layout))
            }
            crate::mouse::HitTarget::InfoRailAgentRow { index }
                if self.approval.pending.is_none() =>
            {
                Ok(self.click_info_rail_agent_row(index, target))
            }
            crate::mouse::HitTarget::InfoRail if self.approval.pending.is_none() => {
                Ok(self.click_info_rail(target))
            }
            _ if self.approval.pending.is_none() => {
                Ok(self.click_live_text_or_background(input, layout))
            }
            _ => Ok(crate::mouse::AppMouseOutcome::Noop),
        }
    }

    fn handle_mouse_left_up_click_fallback(
        &mut self,
        target: crate::mouse::HitTarget,
        input: crate::mouse::MouseInput,
        layout: &crate::ui::LayoutSnapshot,
    ) -> Result<crate::mouse::AppMouseOutcome> {
        match target {
            crate::mouse::HitTarget::SlashCandidate { index } => self.click_slash_candidate(index),
            crate::mouse::HitTarget::ToolCardHeader { entry_index }
            | crate::mouse::HitTarget::ToolCardHiddenPreview { entry_index }
            | crate::mouse::HitTarget::ToolCard { entry_index } => {
                self.click_tool_card_toggle_target(entry_index, input, layout, target)
            }
            crate::mouse::HitTarget::ThinkingBlock { entry_index } => {
                Ok(self.click_thinking_block(entry_index, target))
            }
            crate::mouse::HitTarget::Composer => Ok(self.click_composer(input, layout)),
            crate::mouse::HitTarget::InfoRailAgentRow { index } => {
                Ok(self.click_info_rail_agent_row(index, target))
            }
            crate::mouse::HitTarget::InfoRail => Ok(self.click_info_rail(target)),
            crate::mouse::HitTarget::VerificationCard => {
                self.cancel_tool_card_body_click();
                self.set_mouse_hover_target(Some(target));
                self.clear_timeline_text_selection();
                if self.focus_verification_card() {
                    Ok(crate::mouse::AppMouseOutcome::Redraw)
                } else {
                    Ok(crate::mouse::AppMouseOutcome::Noop)
                }
            }
            _ => {
                self.cancel_tool_card_body_click();
                Ok(crate::mouse::AppMouseOutcome::Noop)
            }
        }
    }

    fn click_slash_candidate(&mut self, index: usize) -> Result<crate::mouse::AppMouseOutcome> {
        self.cancel_tool_card_body_click();
        self.set_mouse_hover_target(None);
        self.clear_timeline_text_selection();
        self.active_pane = PaneFocus::Composer;
        self.blur_composer_aux_panels();
        match self.handle_mouse_slash_candidate(index)? {
            Some(action) => Ok(crate::mouse::AppMouseOutcome::Action(action)),
            None => Ok(crate::mouse::AppMouseOutcome::Redraw),
        }
    }

    fn click_tool_card_toggle_target(
        &mut self,
        entry_index: usize,
        input: crate::mouse::MouseInput,
        layout: &crate::ui::LayoutSnapshot,
        target: crate::mouse::HitTarget,
    ) -> Result<crate::mouse::AppMouseOutcome> {
        let anchor = self.tool_card_mouse_anchor(entry_index, input.column, input.row, layout);
        self.blur_verification_card();
        self.cancel_tool_card_body_click();
        self.set_mouse_hover_target(Some(target));
        self.clear_timeline_text_selection();
        if self.toggle_tool_activity_entry(entry_index) {
            self.restore_tool_card_mouse_anchor(entry_index, anchor);
            Ok(crate::mouse::AppMouseOutcome::Redraw)
        } else {
            Ok(crate::mouse::AppMouseOutcome::Noop)
        }
    }

    fn click_composer(
        &mut self,
        input: crate::mouse::MouseInput,
        layout: &crate::ui::LayoutSnapshot,
    ) -> crate::mouse::AppMouseOutcome {
        self.cancel_tool_card_body_click();
        self.set_mouse_hover_target(Some(crate::mouse::HitTarget::Composer));
        self.clear_timeline_text_selection();
        self.blur_verification_card();
        self.active_pane = PaneFocus::Composer;
        self.blur_composer_aux_panels();
        self.position_input_cursor_from_mouse(input.column, input.row, layout);
        crate::mouse::AppMouseOutcome::Redraw
    }

    fn click_thinking_block(
        &mut self,
        entry_index: usize,
        target: crate::mouse::HitTarget,
    ) -> crate::mouse::AppMouseOutcome {
        self.cancel_tool_card_body_click();
        self.set_mouse_hover_target(Some(target));
        self.clear_timeline_text_selection();
        if self.toggle_thinking_entry(entry_index) {
            crate::mouse::AppMouseOutcome::Redraw
        } else {
            crate::mouse::AppMouseOutcome::Noop
        }
    }

    fn click_info_rail_agent_row(
        &mut self,
        index: usize,
        target: crate::mouse::HitTarget,
    ) -> crate::mouse::AppMouseOutcome {
        self.cancel_tool_card_body_click();
        self.set_mouse_hover_target(Some(target));
        self.clear_timeline_text_selection();
        self.blur_verification_card();
        self.active_pane = PaneFocus::Activity;
        self.blur_composer_aux_panels();
        if self.activate_agent_view_at_index(index) {
            crate::mouse::AppMouseOutcome::Redraw
        } else {
            crate::mouse::AppMouseOutcome::Noop
        }
    }

    fn click_info_rail(
        &mut self,
        target: crate::mouse::HitTarget,
    ) -> crate::mouse::AppMouseOutcome {
        self.cancel_tool_card_body_click();
        self.set_mouse_hover_target(Some(target));
        self.clear_timeline_text_selection();
        self.blur_verification_card();
        self.active_pane = PaneFocus::Activity;
        if self.info_rail_detail_enabled() && !self.session_review_sidebar_lines().is_empty() {
            self.sidebar_selected_card = super::SidebarCard::Review;
        }
        self.blur_composer_aux_panels();
        crate::mouse::AppMouseOutcome::Redraw
    }

    fn click_live_text_or_background(
        &mut self,
        input: crate::mouse::MouseInput,
        layout: &crate::ui::LayoutSnapshot,
    ) -> crate::mouse::AppMouseOutcome {
        self.cancel_tool_card_body_click();
        self.set_mouse_hover_target(None);
        if let Some(position) = layout.live_text_position_at(input.column, input.row) {
            if self.begin_timeline_text_selection_at(position.line_index, position.column) {
                crate::mouse::AppMouseOutcome::Redraw
            } else {
                crate::mouse::AppMouseOutcome::Noop
            }
        } else if self.clear_timeline_text_selection() {
            crate::mouse::AppMouseOutcome::Redraw
        } else {
            crate::mouse::AppMouseOutcome::Noop
        }
    }

    fn mark_mouse_left_down(&mut self) {
        self.pending_mouse_left_down = true;
    }

    fn take_mouse_left_down(&mut self) -> bool {
        let value = self.pending_mouse_left_down;
        self.pending_mouse_left_down = false;
        value
    }

    fn handle_mouse_scroll_target(
        &mut self,
        target: crate::mouse::HitTarget,
        upward: bool,
    ) -> Result<crate::mouse::AppMouseOutcome> {
        if self.approval.pending.is_some() {
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
            crate::mouse::HitTarget::InfoRailAgentRow { .. } => {
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
            crate::mouse::HitTarget::ThinkingBlock { .. }
            | crate::mouse::HitTarget::VerificationCard
            | crate::mouse::HitTarget::LivePanel
            | crate::mouse::HitTarget::Background => {
                self.handle_mouse_scroll(upward);
                Ok(crate::mouse::AppMouseOutcome::Redraw)
            }
            crate::mouse::HitTarget::ToolCardHeader { .. }
            | crate::mouse::HitTarget::ToolCardHiddenPreview { .. }
            | crate::mouse::HitTarget::ToolCard { .. } => {
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
            .approval
            .pending
            .as_ref()
            .and_then(|pending| pending.preview.as_ref())
            .map(|preview| preview.file_diffs.len())
        else {
            return false;
        };
        if index >= file_count || self.approval.selected_file_index == index {
            return false;
        }
        self.approval.selected_file_index = index;
        self.approval.selected_hunk_index = 0;
        self.approval.scroll_back = 0;
        true
    }

    fn scroll_approval_with_mouse(&mut self, upward: bool) {
        let delta = self.terminal_scroll_sensitivity();
        if upward {
            self.approval.scroll_back = self.approval.scroll_back.saturating_sub(delta);
        } else {
            self.approval.scroll_back = self.approval.scroll_back.saturating_add(delta);
        }
    }

    fn position_input_cursor_from_mouse(
        &mut self,
        column: u16,
        row: u16,
        layout: &crate::ui::LayoutSnapshot,
    ) -> bool {
        let Some(position) = layout.composer_input_position_at(column, row) else {
            return false;
        };
        let width = layout.composer_input.width.max(1) as usize;
        let visible_rows = layout.composer_input.height.max(1) as usize;
        let current_row = self.input_cursor_visual_row();
        let row_offset = current_row.saturating_sub(visible_rows.saturating_sub(1));
        let next_cursor = self.cursor_for_visual_position(
            row_offset.saturating_add(position.row),
            position.column,
            width,
        );
        if next_cursor == self.composer.input_cursor {
            return false;
        }
        self.composer.input_paste_spans.clear();
        self.composer.input_cursor = next_cursor;
        self.reset_input_history_navigation();
        self.reset_slash_selector();
        true
    }

    fn set_mouse_hover_target(&mut self, target: Option<crate::mouse::HitTarget>) -> bool {
        if self.mouse_hover_target == target {
            return false;
        }
        let previous_tool = tool_hover_entry_index(self.mouse_hover_target);
        let next_tool = tool_hover_entry_index(target);
        let previous_thinking = thinking_hover_entry_index(self.mouse_hover_target);
        let next_thinking = thinking_hover_entry_index(target);
        self.mouse_hover_target = target;
        if previous_tool != next_tool {
            if let Some(index) = previous_tool {
                self.rerender_timeline_entry(index);
            }
            if let Some(index) = next_tool {
                self.rerender_timeline_entry(index);
            }
        }
        if previous_thinking != next_thinking {
            if let Some(index) = previous_thinking {
                self.rerender_timeline_entry(index);
            }
            if let Some(index) = next_thinking {
                self.rerender_timeline_entry(index);
            }
        }
        true
    }
}

fn enter_key() -> KeyEvent {
    KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
}

fn hover_target_for(target: crate::mouse::HitTarget) -> Option<crate::mouse::HitTarget> {
    match target {
        crate::mouse::HitTarget::Background => None,
        _ => Some(target),
    }
}

fn tool_hover_entry_index(target: Option<crate::mouse::HitTarget>) -> Option<usize> {
    match target? {
        crate::mouse::HitTarget::ToolCardHeader { entry_index }
        | crate::mouse::HitTarget::ToolCardHiddenPreview { entry_index }
        | crate::mouse::HitTarget::ToolCard { entry_index } => Some(entry_index),
        _ => None,
    }
}

fn thinking_hover_entry_index(target: Option<crate::mouse::HitTarget>) -> Option<usize> {
    match target? {
        crate::mouse::HitTarget::ThinkingBlock { entry_index } => Some(entry_index),
        _ => None,
    }
}

fn mouse_action_outcome(action: Option<AppAction>) -> crate::mouse::AppMouseOutcome {
    match action {
        Some(action) => crate::mouse::AppMouseOutcome::Action(action),
        None => crate::mouse::AppMouseOutcome::Redraw,
    }
}
