#![forbid(unsafe_code)]

pub mod runtime;
pub mod text;

pub use runtime::{
    App, PreparedRender, PresentationSessionState, RenderError, SurfaceUpdate, UiRuntimeDriver,
    UpdateOutcome, Viewport,
};
pub use sigil_tui_core as core;
pub use sigil_tui_core::metrics::{
    CacheHitMetrics, DamageSummary, FrameMetrics, FrameMetricsObserver, PhaseDurations,
};
pub use sigil_tui_core::text::{BidiLineMap, BidiText};
pub use sigil_tui_core::theme::{SemanticTheme, ThemeColor, ThemeRole};
pub use sigil_tui_core::widgets::{WidgetKind, WidgetSpec, WidgetTree};
pub use sigil_tui_core::{
    CommittedPresentation, CoreError, Damage, HeightIndex, HitTarget, InputEvent,
    MAX_HIT_GRID_CELLS, MAX_INPUT_TEXT_BYTES, MAX_SURFACE_NODES, MAX_SURFACE_TEXT_BYTES, NodeId,
    NodeKey, PresentFault, PresentNotStarted, PresentOutcome, ProjectionPageRequest, Rect,
    ScrollAnchor, Surface, SurfaceItemId, SurfaceNode, SurfaceNodeKind, TerminalCapabilities,
    TrustedPresentReceipt, ViewportAnchor, VirtualSequence,
};
pub use sigil_tui_ratatui::{Renderer, ScratchRenderContext, TerminalEpoch};
pub use text::Text;

pub mod prelude {
    pub use crate::{
        App, CacheHitMetrics, CommittedPresentation, Damage, DamageSummary, FrameMetrics,
        FrameMetricsObserver, InputEvent, NodeId, NodeKey, PhaseDurations, PreparedRender,
        PresentationSessionState, Rect, Renderer, SemanticTheme, Surface, SurfaceUpdate, Text,
        ThemeColor, ThemeRole, UiRuntimeDriver, UpdateOutcome, Viewport, WidgetKind, WidgetSpec,
        WidgetTree,
    };
}
