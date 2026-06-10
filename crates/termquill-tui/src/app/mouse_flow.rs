use super::*;

impl AppState {
    pub fn handle_mouse_event(
        &mut self,
        input: crate::mouse::MouseInput,
        _layout: &crate::ui::LayoutSnapshot,
    ) -> Result<crate::mouse::AppMouseOutcome> {
        match input.kind {
            crate::mouse::MouseInputKind::ScrollUp => {
                self.handle_mouse_scroll(true);
                Ok(crate::mouse::AppMouseOutcome::Redraw)
            }
            crate::mouse::MouseInputKind::ScrollDown => {
                self.handle_mouse_scroll(false);
                Ok(crate::mouse::AppMouseOutcome::Redraw)
            }
            _ => Ok(crate::mouse::AppMouseOutcome::Noop),
        }
    }
}
