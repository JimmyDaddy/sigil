#![forbid(unsafe_code)]

use ratatui::{
    Terminal, backend::Backend, layout::Rect as RatatuiRect, text::Line, widgets::Paragraph,
};
use sigil_tui_core::{
    CommittedPresentation, PresentFault, PresentNotStarted, PresentOutcome, Rect,
    TrustedPresentReceipt,
};

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
