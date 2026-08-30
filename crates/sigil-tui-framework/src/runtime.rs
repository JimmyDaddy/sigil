use std::time::Instant;

use sigil_tui_core::metrics::FrameMetrics;
use sigil_tui_core::{
    CommittedPresentation, CoreError, Damage, InputEvent, PresentOutcome, Rect, Surface,
};

/// A viewport supplied by the host. It is deliberately independent of Ratatui geometry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Viewport {
    pub width: u16,
    pub height: u16,
}

impl Viewport {
    pub const fn new(width: u16, height: u16) -> Self {
        Self { width, height }
    }
}

/// External updates are already owned by the application adapter; the framework only tracks
/// their bounded invalidation and, when supplied, the next surface snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SurfaceUpdate {
    Damage(Damage),
    Replace { surface: Surface, damage: Damage },
}

/// Prepared render data cannot be queried by input dispatch. The host may only pass it to its
/// presentation adapter and then report the resulting outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedRender {
    generation: u64,
    surface: Surface,
    presentation: CommittedPresentation,
    metrics: FrameMetrics,
}

impl PreparedRender {
    pub fn new(
        generation: u64,
        surface: Surface,
        presentation: CommittedPresentation,
        metrics: FrameMetrics,
    ) -> Result<Self, CoreError> {
        if generation == 0 || surface.generation() != generation {
            return Err(CoreError::InvalidValue(
                "prepared render generation does not match surface",
            ));
        }
        if presentation.generation != generation {
            return Err(CoreError::InvalidValue(
                "prepared render generation does not match presentation",
            ));
        }
        if metrics.generation != generation {
            return Err(CoreError::InvalidValue(
                "prepared render generation does not match metrics",
            ));
        }
        Ok(Self {
            generation,
            surface,
            presentation,
            metrics,
        })
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn surface(&self) -> &Surface {
        &self.surface
    }

    pub fn presentation(&self) -> &CommittedPresentation {
        &self.presentation
    }

    /// Returns the counters captured for this render transaction.
    pub fn metrics(&self) -> &FrameMetrics {
        &self.metrics
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RenderError {
    Invalid(CoreError),
    Closed,
    Poisoned,
}

impl From<CoreError> for RenderError {
    fn from(error: CoreError) -> Self {
        Self::Invalid(error)
    }
}

/// Renderer-neutral state exposed for host scheduling and diagnostics.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresentationSessionState {
    Synchronized {
        generation: u64,
        terminal_epoch: u64,
    },
    AwaitingPresent {
        generation: u64,
        terminal_epoch: u64,
    },
    Presenting {
        generation: u64,
        terminal_epoch: u64,
    },
    Poisoned {
        terminal_epoch: u64,
    },
    Recovering {
        terminal_epoch: u64,
    },
    Closed,
}

/// The framework-facing update result. Domain/application commands remain opaque to the
/// framework and are returned to the host as the consumer's own action type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdateOutcome<Action> {
    Ignored,
    Redraw(Damage),
    Action(Action),
}

impl<Action> UpdateOutcome<Action> {
    pub fn damage(&self) -> Damage {
        match self {
            Self::Ignored | Self::Action(_) => Damage::NONE,
            Self::Redraw(damage) => *damage,
        }
    }
}

/// Minimal application-neutral contract used by framework consumers.
///
/// The framework owns input normalization, bounded surface construction and presentation
/// identity. The host owns command execution, persistence and any async runtime.
pub trait App {
    type Action;

    fn handle_input(&mut self, input: InputEvent) -> UpdateOutcome<Self::Action>;

    fn build_surface(&self, viewport: Rect, generation: u64) -> Result<Surface, CoreError>;
}

/// Host-owned event/presentation loop contract.
///
/// The framework never creates a runtime or performs I/O. A host decides how to await
/// application work, present a prepared surface, and recover a poisoned terminal epoch.
pub trait UiRuntimeDriver {
    type Action;

    fn handle_input(&mut self, input: InputEvent) -> UpdateOutcome<Self::Action>;

    fn apply_external(&mut self, update: SurfaceUpdate) -> Damage;

    fn next_wake(&self) -> Option<Instant>;

    fn needs_present(&self) -> bool;

    fn prepare(&mut self, viewport: Viewport) -> Result<PreparedRender, RenderError>;

    fn finish_present(&mut self, outcome: PresentOutcome);

    fn presentation_state(&self) -> PresentationSessionState;
}
