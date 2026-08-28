use sigil_tui_core::{CoreError, Damage, InputEvent, Rect, Surface};

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
