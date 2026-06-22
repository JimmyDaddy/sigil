use ratatui::style::{Modifier, Style};

use super::*;
use crate::ui::theme::{Theme, accent_blue, accent_gold, accent_lime, accent_rose, dim, muted};

#[test]
fn status_indicator_maps_semantic_symbols_and_styles() {
    assert_eq!(status_symbol(StatusKind::Idle), "○");
    assert_eq!(status_symbol(StatusKind::Running), "◐");
    assert_eq!(status_symbol(StatusKind::Success), "✓");
    assert_eq!(status_symbol(StatusKind::Error), "✕");
    assert_eq!(status_symbol(StatusKind::Warning), "△");
    assert_eq!(status_symbol(StatusKind::Unknown), "◇");

    assert_eq!(status_style(StatusKind::Idle), Style::default().fg(dim()));
    assert_eq!(
        status_style(StatusKind::Running),
        Style::default()
            .fg(accent_gold())
            .add_modifier(Modifier::BOLD)
    );
    assert_eq!(
        status_style(StatusKind::Success),
        Style::default().fg(accent_lime())
    );
    assert_eq!(
        status_style(StatusKind::Error),
        Style::default()
            .fg(accent_rose())
            .add_modifier(Modifier::BOLD)
    );
    assert_eq!(
        status_rest_style(StatusKind::Unknown),
        Style::default().fg(muted())
    );
}

#[test]
fn focus_indicator_maps_current_and_selection_symbols() {
    assert_eq!(focus_symbol(FocusKind::Current), "◉");
    assert_eq!(focus_symbol(FocusKind::Selected), "▸");
    assert_eq!(focus_symbol(FocusKind::None), " ");
    assert_eq!(
        focus_style(FocusKind::Current),
        Style::default()
            .fg(accent_blue())
            .add_modifier(Modifier::BOLD)
    );
}

#[test]
fn running_indicator_animates_across_known_frames() {
    let indicator = StatusIndicator::animated(StatusKind::Running);
    assert_eq!(indicator.symbol_at(0), "◐");
    assert_eq!(indicator.symbol_at(1), "◓");
    assert_eq!(indicator.symbol_at(2), "◑");
    assert_eq!(indicator.symbol_at(3), "◒");
    assert_eq!(indicator.symbol_at(4), "◐");
    assert_eq!(
        StatusIndicator::animated(StatusKind::Success).symbol_at(1),
        "✓"
    );
}

#[test]
fn indicator_styles_cover_focus_and_status_markers() {
    let (marker_style, rest_style) = indicator_styles("◉").expect("focus style");
    assert_eq!(
        marker_style,
        Style::default()
            .fg(accent_blue())
            .add_modifier(Modifier::BOLD)
    );
    assert_ne!(rest_style, Style::default().fg(muted()));

    let (marker_style, rest_style) = indicator_styles("✕").expect("error style");
    assert_eq!(
        marker_style,
        Style::default()
            .fg(accent_rose())
            .add_modifier(Modifier::BOLD)
    );
    assert_eq!(
        rest_style,
        Style::default()
            .fg(accent_rose())
            .add_modifier(Modifier::BOLD)
    );
    assert!(indicator_styles(">").is_none());
}

#[test]
fn status_indicator_palette_variants_use_current_theme() {
    let palette = Theme::builtin(sigil_kernel::ThemeId::SolarizedLight).palette;
    let indicator = StatusIndicator::static_kind(StatusKind::Success);

    assert_eq!(
        indicator.span_with_palette(&palette).style.fg,
        Some(palette.status_success)
    );
    assert_eq!(
        indicator.badge_style_with_palette(&palette).bg,
        Some(palette.surface_badge)
    );
    assert_eq!(
        status_rest_style_with_palette(StatusKind::Error, &palette).fg,
        Some(palette.status_error)
    );
    assert_eq!(
        focus_style_with_palette(FocusKind::Selected, &palette).fg,
        Some(palette.accent_info)
    );
    let (marker_style, rest_style) =
        indicator_styles_with_palette("✕", &palette).expect("error style");
    assert_eq!(marker_style.fg, Some(palette.status_error));
    assert_eq!(rest_style.fg, Some(palette.status_error));
}

#[test]
fn status_kind_from_label_normalizes_common_state_words() {
    assert_eq!(status_kind_from_label("idle"), StatusKind::Idle);
    assert_eq!(status_kind_from_label("started"), StatusKind::Running);
    assert_eq!(status_kind_from_label("running"), StatusKind::Running);
    assert_eq!(status_kind_from_label("completed"), StatusKind::Success);
    assert_eq!(status_kind_from_label("failed"), StatusKind::Error);
    assert_eq!(status_kind_from_label("paused"), StatusKind::Warning);
    assert_eq!(status_kind_from_label("unknown"), StatusKind::Unknown);
}
