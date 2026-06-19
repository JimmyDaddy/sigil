use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::{
    style::{Modifier, Style},
    text::Span,
};

use super::theme::{accent_blue, accent_gold, accent_lime, accent_rose, badge_bg, dim, ink, muted};

const RUNNING_FRAMES: &[&str] = &["◐", "◓", "◑", "◒"];
const SPINNER_FRAME_MILLIS: u128 = 120;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StatusKind {
    Idle,
    Running,
    Success,
    Error,
    Warning,
    Pending,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FocusKind {
    Current,
    Selected,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StatusIndicator {
    kind: StatusKind,
    animated: bool,
}

impl StatusIndicator {
    pub(crate) fn static_kind(kind: StatusKind) -> Self {
        Self {
            kind,
            animated: false,
        }
    }

    pub(crate) fn animated(kind: StatusKind) -> Self {
        Self {
            kind,
            animated: true,
        }
    }

    pub(crate) fn symbol(self) -> &'static str {
        self.symbol_at(status_tick())
    }

    pub(crate) fn symbol_at(self, tick: u128) -> &'static str {
        if self.animated && self.kind == StatusKind::Running {
            return RUNNING_FRAMES[(tick as usize) % RUNNING_FRAMES.len()];
        }
        status_symbol(self.kind)
    }

    pub(crate) fn style(self) -> Style {
        status_style(self.kind)
    }

    pub(crate) fn badge_style(self) -> Style {
        self.style().bg(badge_bg()).add_modifier(Modifier::BOLD)
    }

    pub(crate) fn span(self) -> Span<'static> {
        Span::styled(self.symbol(), self.style())
    }
}

pub(crate) fn status_symbol(kind: StatusKind) -> &'static str {
    match kind {
        StatusKind::Idle => "○",
        StatusKind::Running => "◐",
        StatusKind::Success => "✓",
        StatusKind::Error => "✕",
        StatusKind::Warning => "△",
        StatusKind::Pending | StatusKind::Unknown => "◇",
    }
}

pub(crate) fn status_style(kind: StatusKind) -> Style {
    match kind {
        StatusKind::Idle | StatusKind::Pending | StatusKind::Unknown => Style::default().fg(dim()),
        StatusKind::Running => Style::default()
            .fg(accent_gold())
            .add_modifier(Modifier::BOLD),
        StatusKind::Success => Style::default().fg(accent_lime()),
        StatusKind::Error => Style::default()
            .fg(accent_rose())
            .add_modifier(Modifier::BOLD),
        StatusKind::Warning => Style::default()
            .fg(accent_gold())
            .add_modifier(Modifier::BOLD),
    }
}

pub(crate) fn status_rest_style(kind: StatusKind) -> Style {
    match kind {
        StatusKind::Idle | StatusKind::Pending | StatusKind::Unknown => {
            Style::default().fg(muted())
        }
        StatusKind::Running => Style::default()
            .fg(accent_gold())
            .add_modifier(Modifier::BOLD),
        StatusKind::Success => Style::default().fg(accent_lime()),
        StatusKind::Error => Style::default()
            .fg(accent_rose())
            .add_modifier(Modifier::BOLD),
        StatusKind::Warning => Style::default()
            .fg(accent_gold())
            .add_modifier(Modifier::BOLD),
    }
}

pub(crate) fn focus_symbol(kind: FocusKind) -> &'static str {
    match kind {
        FocusKind::Current => "◉",
        FocusKind::Selected => "▸",
        FocusKind::None => " ",
    }
}

pub(crate) fn focus_style(kind: FocusKind) -> Style {
    match kind {
        FocusKind::Current | FocusKind::Selected => Style::default()
            .fg(accent_blue())
            .add_modifier(Modifier::BOLD),
        FocusKind::None => Style::default().fg(ink()),
    }
}

pub(crate) fn indicator_styles(marker: &str) -> Option<(Style, Style)> {
    if let Some(kind) = status_kind_from_symbol(marker) {
        return Some((status_style(kind), status_rest_style(kind)));
    }
    let focus_kind = match marker {
        "◉" => Some(FocusKind::Current),
        "▸" => Some(FocusKind::Selected),
        _ => None,
    }?;
    Some((focus_style(focus_kind), Style::default().fg(ink())))
}

pub(crate) fn render_marker_symbol(marker: &str) -> &str {
    if status_kind_from_symbol(marker) == Some(StatusKind::Running) {
        return StatusIndicator::animated(StatusKind::Running).symbol();
    }
    marker
}

pub(crate) fn status_kind_from_label(label: &str) -> StatusKind {
    match label {
        "idle" => StatusKind::Idle,
        "started" | "running" | "starting" => StatusKind::Running,
        "completed" | "ok" | "exited" => StatusKind::Success,
        "failed" | "blocked" | "cancelled" | "interrupted" | "unavailable" | "denied" | "error" => {
            StatusKind::Error
        }
        "paused" | "deferred" | "warn" | "warning" => StatusKind::Warning,
        "pending" => StatusKind::Pending,
        _ => StatusKind::Unknown,
    }
}

fn status_kind_from_symbol(symbol: &str) -> Option<StatusKind> {
    match symbol {
        "○" => Some(StatusKind::Idle),
        "◐" | "◓" | "◑" | "◒" => Some(StatusKind::Running),
        "✓" => Some(StatusKind::Success),
        "✕" => Some(StatusKind::Error),
        "△" => Some(StatusKind::Warning),
        "◇" => Some(StatusKind::Unknown),
        _ => None,
    }
}

fn status_tick() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() / SPINNER_FRAME_MILLIS)
        .unwrap_or(0)
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/status_indicator_tests.rs"]
mod tests;
