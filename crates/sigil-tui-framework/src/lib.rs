#![forbid(unsafe_code)]

pub mod runtime;
pub mod text;

pub use runtime::{App, UpdateOutcome};
pub use sigil_tui_core as core;
pub use sigil_tui_core::theme::{SemanticTheme, ThemeColor, ThemeRole};
pub use sigil_tui_core::{
    CommittedPresentation, CoreError, Damage, HeightIndex, HitTarget, InputEvent,
    MAX_INPUT_TEXT_BYTES, MAX_SURFACE_NODES, MAX_SURFACE_TEXT_BYTES, NodeId, NodeKey, PresentFault,
    PresentNotStarted, PresentOutcome, ProjectionPageRequest, Rect, ScrollAnchor, Surface,
    SurfaceItemId, SurfaceNode, SurfaceNodeKind, TerminalCapabilities, TrustedPresentReceipt,
    ViewportAnchor, VirtualSequence,
};
pub use sigil_tui_ratatui::{Renderer, ScratchRenderContext, TerminalEpoch};
pub use text::Text;

pub mod prelude {
    pub use crate::{
        App, CommittedPresentation, Damage, InputEvent, NodeId, NodeKey, Rect, Renderer,
        SemanticTheme, Surface, Text, ThemeColor, ThemeRole, UpdateOutcome,
    };
}
