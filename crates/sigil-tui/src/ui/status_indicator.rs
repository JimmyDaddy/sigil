use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::{
    style::{Modifier, Style},
    text::Span,
};

use super::theme::{self, ThemePalette};

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

    pub(crate) fn style_with_palette(self, palette: &ThemePalette) -> Style {
        status_style_with_palette(self.kind, palette)
    }

    pub(crate) fn badge_style(self) -> Style {
        self.badge_style_with_palette(&theme::default_palette())
    }

    pub(crate) fn badge_style_with_palette(self, palette: &ThemePalette) -> Style {
        self.style_with_palette(palette)
            .bg(palette.surface_badge)
            .add_modifier(Modifier::BOLD)
    }

    pub(crate) fn span(self) -> Span<'static> {
        Span::styled(self.symbol(), self.style())
    }

    pub(crate) fn span_with_palette(self, palette: &ThemePalette) -> Span<'static> {
        Span::styled(self.symbol(), self.style_with_palette(palette))
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
    status_style_with_palette(kind, &theme::default_palette())
}

pub(crate) fn status_style_with_palette(kind: StatusKind, palette: &ThemePalette) -> Style {
    match kind {
        StatusKind::Idle | StatusKind::Pending | StatusKind::Unknown => {
            Style::default().fg(palette.status_pending)
        }
        StatusKind::Running => Style::default()
            .fg(palette.status_thinking)
            .add_modifier(Modifier::BOLD),
        StatusKind::Success => Style::default().fg(palette.status_success),
        StatusKind::Error => Style::default()
            .fg(palette.status_error)
            .add_modifier(Modifier::BOLD),
        StatusKind::Warning => Style::default()
            .fg(palette.status_warning)
            .add_modifier(Modifier::BOLD),
    }
}

#[cfg(test)]
pub(crate) fn status_rest_style(kind: StatusKind) -> Style {
    status_rest_style_with_palette(kind, &theme::default_palette())
}

pub(crate) fn status_rest_style_with_palette(kind: StatusKind, palette: &ThemePalette) -> Style {
    match kind {
        StatusKind::Idle | StatusKind::Pending | StatusKind::Unknown => {
            Style::default().fg(palette.text_secondary)
        }
        StatusKind::Running => Style::default()
            .fg(palette.status_thinking)
            .add_modifier(Modifier::BOLD),
        StatusKind::Success => Style::default().fg(palette.status_success),
        StatusKind::Error => Style::default()
            .fg(palette.status_error)
            .add_modifier(Modifier::BOLD),
        StatusKind::Warning => Style::default()
            .fg(palette.status_warning)
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
    focus_style_with_palette(kind, &theme::default_palette())
}

pub(crate) fn focus_style_with_palette(kind: FocusKind, palette: &ThemePalette) -> Style {
    match kind {
        FocusKind::Current | FocusKind::Selected => Style::default()
            .fg(palette.accent_info)
            .add_modifier(Modifier::BOLD),
        FocusKind::None => Style::default().fg(palette.text_primary),
    }
}

pub(crate) fn indicator_styles(marker: &str) -> Option<(Style, Style)> {
    indicator_styles_with_palette(marker, &theme::default_palette())
}

pub(crate) fn indicator_styles_with_palette(
    marker: &str,
    palette: &ThemePalette,
) -> Option<(Style, Style)> {
    if let Some(kind) = status_kind_from_symbol(marker) {
        return Some((
            status_style_with_palette(kind, palette),
            status_rest_style_with_palette(kind, palette),
        ));
    }
    let focus_kind = match marker {
        "◉" => Some(FocusKind::Current),
        "▸" => Some(FocusKind::Selected),
        _ => None,
    }?;
    Some((
        focus_style_with_palette(focus_kind, palette),
        Style::default().fg(palette.text_primary),
    ))
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
