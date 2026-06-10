use ratatui::layout::{Constraint, Direction, Layout, Rect};

use crate::app::AppState;

use super::geometry::sidebar_width_for_terminal;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutMode {
    Main,
    Setup,
    Config,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutSnapshot {
    pub screen: Rect,
    pub mode: LayoutMode,
    pub live_panel: Rect,
    pub composer: Rect,
    pub footer: Rect,
    pub info_rail: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ShellLayout {
    pub live_panel: Rect,
    pub composer: Rect,
    pub footer: Rect,
    pub info_rail: Rect,
}

impl LayoutSnapshot {
    pub fn from_app(screen: Rect, app: &AppState) -> Self {
        if app.is_setup_mode() {
            return Self::single(screen, LayoutMode::Setup);
        }
        if app.is_config_mode() {
            return Self::single(screen, LayoutMode::Config);
        }

        let shell = shell_layout(screen, app.footer_strip_height());
        Self {
            screen,
            mode: LayoutMode::Main,
            live_panel: shell.live_panel,
            composer: shell.composer,
            footer: shell.footer,
            info_rail: shell.info_rail,
        }
    }

    fn single(screen: Rect, mode: LayoutMode) -> Self {
        Self {
            screen,
            mode,
            live_panel: Rect::default(),
            composer: Rect::default(),
            footer: Rect::default(),
            info_rail: Rect::default(),
        }
    }
}

pub(super) fn shell_layout(screen: Rect, footer_height: u16) -> ShellLayout {
    let sidebar_width = sidebar_width_for_terminal(screen.width as usize) as u16;
    let shell = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(10), Constraint::Length(sidebar_width)])
        .split(screen);

    let main = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(footer_height),
            Constraint::Length(1),
        ])
        .split(shell[0]);

    ShellLayout {
        live_panel: main[0],
        composer: main[1],
        footer: main[2],
        info_rail: shell[1],
    }
}
