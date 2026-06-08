use ratatui::style::Color;

use crate::app::RunPhase;

pub(crate) fn shell_bg() -> Color {
    Color::Rgb(7, 8, 10)
}

pub(crate) fn rail_bg() -> Color {
    Color::Rgb(26, 28, 34)
}

pub(crate) fn composer_bg() -> Color {
    Color::Rgb(24, 26, 31)
}

pub(crate) fn composer_input_bg() -> Color {
    Color::Rgb(18, 20, 25)
}

pub(crate) fn selector_bg() -> Color {
    Color::Rgb(19, 21, 27)
}

pub(crate) fn selector_shadow_bg() -> Color {
    Color::Rgb(10, 11, 15)
}

pub(crate) fn selector_accent() -> Color {
    Color::Rgb(242, 171, 122)
}

pub(crate) fn user_message_bg() -> Color {
    Color::Rgb(20, 22, 27)
}

pub(crate) fn ink() -> Color {
    Color::Rgb(236, 240, 246)
}

pub(crate) fn muted() -> Color {
    Color::Rgb(149, 158, 173)
}

pub(crate) fn dim() -> Color {
    Color::Rgb(99, 109, 126)
}

pub(crate) fn accent_teal() -> Color {
    Color::Rgb(126, 180, 226)
}

pub(crate) fn accent_blue() -> Color {
    Color::Rgb(148, 178, 244)
}

pub(crate) fn accent_gold() -> Color {
    Color::Rgb(196, 176, 128)
}

pub(crate) fn accent_lime() -> Color {
    Color::Rgb(145, 182, 170)
}

pub(crate) fn accent_rose() -> Color {
    Color::Rgb(198, 142, 150)
}

pub(crate) fn badge_bg() -> Color {
    Color::Rgb(30, 35, 43)
}

pub(crate) fn phase_accent(phase: &RunPhase) -> Color {
    match phase {
        RunPhase::Idle => accent_teal(),
        RunPhase::Thinking => accent_gold(),
        RunPhase::Tool(_) => accent_rose(),
        RunPhase::Streaming => accent_blue(),
    }
}
