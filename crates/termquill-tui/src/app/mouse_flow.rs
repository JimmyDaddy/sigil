use super::*;

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
                crate::mouse::HitTarget::SlashCandidate { index }
                    if self.pending_approval.is_none() =>
                {
                    self.active_pane = PaneFocus::Composer;
                    match self.handle_mouse_slash_candidate(index)? {
                        Some(action) => Ok(crate::mouse::AppMouseOutcome::Action(action)),
                        None => Ok(crate::mouse::AppMouseOutcome::Redraw),
                    }
                }
                crate::mouse::HitTarget::ToolCard { entry_index }
                    if self.pending_approval.is_none() =>
                {
                    if self.select_tool_activity_entry(entry_index) {
                        Ok(crate::mouse::AppMouseOutcome::Redraw)
                    } else {
                        Ok(crate::mouse::AppMouseOutcome::Noop)
                    }
                }
                crate::mouse::HitTarget::Composer if self.pending_approval.is_none() => {
                    self.active_pane = PaneFocus::Composer;
                    Ok(crate::mouse::AppMouseOutcome::Redraw)
                }
                crate::mouse::HitTarget::InfoRail if self.pending_approval.is_none() => {
                    self.active_pane = PaneFocus::Activity;
                    Ok(crate::mouse::AppMouseOutcome::Redraw)
                }
                _ => Ok(crate::mouse::AppMouseOutcome::Noop),
            },
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
                crate::mouse::HitTarget::ApprovalModal => {
                    self.scroll_approval_with_mouse(upward);
                    Ok(crate::mouse::AppMouseOutcome::Redraw)
                }
                _ => Ok(crate::mouse::AppMouseOutcome::Noop),
            };
        }

        match target {
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

    fn scroll_approval_with_mouse(&mut self, upward: bool) {
        let delta = 3;
        if upward {
            self.approval_scroll_back = self.approval_scroll_back.saturating_sub(delta);
        } else {
            self.approval_scroll_back = self.approval_scroll_back.saturating_add(delta);
        }
    }
}
