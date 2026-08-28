#![forbid(unsafe_code)]

pub use sigil_tui_core as core;
pub use sigil_tui_core::{
    CommittedPresentation, CoreError, Damage, HitTarget, InputEvent, NodeId, NodeKey, PresentFault,
    PresentNotStarted, PresentOutcome, Rect, ScrollAnchor, TerminalCapabilities,
    TrustedPresentReceipt,
};
pub use sigil_tui_ratatui::{Renderer, TerminalEpoch};

pub mod prelude {
    pub use crate::{CommittedPresentation, Damage, InputEvent, NodeId, NodeKey, Rect, Renderer};
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Text(String);

impl Text {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}
