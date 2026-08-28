#![forbid(unsafe_code)]

pub mod runtime;
pub mod text;

pub use runtime::{App, UpdateOutcome};
pub use sigil_tui_core as core;
pub use sigil_tui_core::{
    CommittedPresentation, CoreError, Damage, HitTarget, InputEvent, MAX_SURFACE_NODES,
    MAX_SURFACE_TEXT_BYTES, NodeId, NodeKey, PresentFault, PresentNotStarted, PresentOutcome, Rect,
    ScrollAnchor, Surface, SurfaceNode, SurfaceNodeKind, TerminalCapabilities,
    TrustedPresentReceipt,
};
pub use sigil_tui_ratatui::{Renderer, TerminalEpoch};
pub use text::Text;

pub mod prelude {
    pub use crate::{
        App, CommittedPresentation, Damage, InputEvent, NodeId, NodeKey, Rect, Renderer, Surface,
        Text, UpdateOutcome,
    };
}
