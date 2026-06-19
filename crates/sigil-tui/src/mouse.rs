use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use crate::app::AppAction;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseInput {
    pub column: u16,
    pub row: u16,
    pub kind: MouseInputKind,
    pub modifiers: KeyModifiers,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseInputKind {
    LeftDown,
    LeftUp,
    RightDown,
    ScrollUp,
    ScrollDown,
    Drag,
    Moved,
    Unsupported,
}

#[derive(Debug, Clone)]
pub enum AppMouseOutcome {
    Noop,
    Redraw,
    Action(AppAction),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitTarget {
    ApprovalModal,
    ApprovalDiffArea,
    ApprovalFileRow { index: usize },
    ApprovalAction { approved: bool },
    ApprovalHunkPrevious,
    ApprovalHunkNext,
    ApprovalDiffViewToggle,
    ApprovalMetadataToggle,
    SetupField { index: usize },
    ConfigSection { index: usize },
    ConfigField { index: usize },
    ConfigFooterAction { index: usize },
    SlashOverlay,
    SlashCandidate { index: usize },
    ToolCardHeader { entry_index: usize },
    ToolCardHiddenPreview { entry_index: usize },
    ToolCard { entry_index: usize },
    ThinkingBlock { entry_index: usize },
    Composer,
    LivePanel,
    InfoRailAgentRow { index: usize },
    InfoRail,
    Background,
}

impl From<MouseEvent> for MouseInput {
    fn from(event: MouseEvent) -> Self {
        Self {
            column: event.column,
            row: event.row,
            kind: MouseInputKind::from(event.kind),
            modifiers: event.modifiers,
        }
    }
}

impl From<MouseEventKind> for MouseInputKind {
    fn from(kind: MouseEventKind) -> Self {
        match kind {
            MouseEventKind::Down(MouseButton::Left) => Self::LeftDown,
            MouseEventKind::Up(MouseButton::Left) => Self::LeftUp,
            MouseEventKind::Down(MouseButton::Right) => Self::RightDown,
            MouseEventKind::ScrollUp => Self::ScrollUp,
            MouseEventKind::ScrollDown => Self::ScrollDown,
            MouseEventKind::Drag(_) => Self::Drag,
            MouseEventKind::Moved => Self::Moved,
            _ => Self::Unsupported,
        }
    }
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/mouse_tests.rs"]
mod tests;
