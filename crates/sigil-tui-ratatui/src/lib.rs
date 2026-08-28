#![forbid(unsafe_code)]

use ratatui::{
    Terminal,
    backend::Backend,
    buffer::Buffer,
    layout::Rect as RatatuiRect,
    text::Line,
    widgets::{Paragraph, Widget},
};
use sigil_tui_core::{
    CommittedPresentation, PresentFault, PresentNotStarted, PresentOutcome, Rect,
    TrustedPresentReceipt,
};

/// Ratatui-native scratch renderer for one bounded framework surface.
///
/// The buffer is owned by this adapter and is never exposed through the core package. Scissoring
/// happens before a widget is rendered, so custom widgets cannot paint outside their assignment.
#[derive(Debug)]
pub struct ScratchRenderContext {
    bounds: RatatuiRect,
    scissor: RatatuiRect,
    buffer: Buffer,
}

impl ScratchRenderContext {
    pub fn new(bounds: RatatuiRect, scissor: RatatuiRect) -> Self {
        let scissor = bounds.intersection(scissor);
        Self {
            bounds,
            scissor,
            buffer: Buffer::empty(bounds),
        }
    }

    pub fn bounds(&self) -> RatatuiRect {
        self.bounds
    }

    pub fn scissor(&self) -> RatatuiRect {
        self.scissor
    }

    pub fn render_widget<W: Widget>(&mut self, area: RatatuiRect, widget: W) -> bool {
        let area = self.scissor.intersection(area);
        if area.width == 0 || area.height == 0 {
            return false;
        }
        widget.render(area, &mut self.buffer);
        true
    }

    pub fn buffer(&self) -> &Buffer {
        &self.buffer
    }

    pub fn into_buffer(self) -> Buffer {
        self.buffer
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalEpoch(u64);

impl TerminalEpoch {
    pub const fn new(value: u64) -> Option<Self> {
        if value == 0 { None } else { Some(Self(value)) }
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug)]
pub struct Renderer {
    epoch: TerminalEpoch,
    committed: Option<CommittedPresentation>,
    poisoned: bool,
}

impl Renderer {
    pub fn new(epoch: TerminalEpoch) -> Self {
        Self {
            epoch,
            committed: None,
            poisoned: false,
        }
    }

    pub fn epoch(&self) -> TerminalEpoch {
        self.epoch
    }

    pub fn committed(&self) -> Option<&CommittedPresentation> {
        self.committed.as_ref()
    }

    pub fn poison(&mut self) {
        self.poisoned = true;
        self.committed = None;
    }

    pub fn present_text<B: Backend>(
        &mut self,
        terminal: &mut Terminal<B>,
        text: impl Into<String>,
        generation: u64,
    ) -> PresentOutcome {
        if self.poisoned {
            return PresentOutcome::NotStarted(PresentNotStarted {
                reason: "renderer epoch is poisoned".to_owned(),
            });
        }
        let text = text.into();
        let area = terminal.get_frame().area();
        let result = terminal.draw(|frame| {
            frame.render_widget(Paragraph::new(Line::from(text.clone())), area);
        });
        if let Err(error) = result {
            self.poison();
            return PresentOutcome::IndeterminateAfterIo(PresentFault {
                terminal_epoch: self.epoch.get(),
                reason: error.to_string(),
            });
        }
        let viewport = Rect::new(area.x, area.y, area.width, area.height);
        let digest = format!("{}:{}:{}", self.epoch.get(), generation, text.len());
        let presentation = match CommittedPresentation::new(
            self.epoch.get(),
            viewport,
            generation,
            digest,
            Vec::new(),
        ) {
            Ok(presentation) => presentation,
            Err(error) => {
                self.poison();
                return PresentOutcome::IndeterminateAfterIo(PresentFault {
                    terminal_epoch: self.epoch.get(),
                    reason: error.to_string(),
                });
            }
        };
        self.committed = Some(presentation);
        PresentOutcome::Presented(TrustedPresentReceipt {
            terminal_epoch: self.epoch.get(),
            generation,
        })
    }

    pub fn viewport_from(area: RatatuiRect) -> Rect {
        Rect::new(area.x, area.y, area.width, area.height)
    }
}

#[cfg(test)]
mod tests {
    use ratatui::{layout::Rect, widgets::Block};

    use super::ScratchRenderContext;

    #[test]
    fn scratch_context_scissors_custom_widgets_to_assigned_bounds() {
        let mut context = ScratchRenderContext::new(Rect::new(4, 3, 5, 2), Rect::new(0, 0, 20, 20));
        assert_eq!(context.bounds(), Rect::new(4, 3, 5, 2));
        assert_eq!(context.scissor(), Rect::new(4, 3, 5, 2));
        assert!(context.render_widget(Rect::new(0, 0, 20, 20), Block::default()));
        assert_eq!(context.buffer().area, Rect::new(4, 3, 5, 2));
        assert!(!context.render_widget(Rect::new(0, 0, 0, 0), Block::default()));
    }
}
