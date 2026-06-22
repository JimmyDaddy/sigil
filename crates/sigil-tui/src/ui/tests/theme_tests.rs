use std::collections::BTreeMap;

use ratatui::style::Color;
use sigil_kernel::{AppearanceConfig, ThemeColorOverrides, ThemeId};

use super::*;

#[test]
fn builtin_themes_resolve_for_all_theme_ids() {
    for theme_id in ThemeId::all() {
        let theme = Theme::builtin(*theme_id);

        assert_eq!(theme.id, *theme_id);
        assert_ne!(theme.palette.text_primary, theme.palette.surface_base);
    }
}

#[test]
fn default_wrappers_preserve_sigil_dark_baseline() {
    assert_eq!(shell_bg(), Color::Rgb(7, 8, 10));
    assert_eq!(composer_bg(), Color::Rgb(27, 30, 37));
    assert_eq!(ink(), Color::Rgb(236, 240, 246));
    assert_eq!(config_selected_bg(), Color::Rgb(23, 34, 36));
}

#[test]
fn theme_resolve_applies_color_overrides() {
    let mut colors = BTreeMap::new();
    colors.insert("surface_base".to_owned(), "#010203".to_owned());
    colors.insert("diff_added_bg".to_owned(), "#102216".to_owned());
    let appearance = AppearanceConfig {
        theme: ThemeId::SolarizedLight,
        colors: ThemeColorOverrides::new(colors),
    };

    let theme = Theme::try_from_config(&appearance).expect("overrides should apply");

    assert_eq!(theme.id, ThemeId::SolarizedLight);
    assert_eq!(theme.palette.surface_base, Color::Rgb(1, 2, 3));
    assert_eq!(theme.palette.diff_added_bg, Color::Rgb(16, 34, 22));
}

#[test]
fn theme_resolve_rejects_unknown_override_token() {
    let mut colors = BTreeMap::new();
    colors.insert("component_specific_blue".to_owned(), "#010203".to_owned());
    let appearance = AppearanceConfig {
        theme: ThemeId::SigilDark,
        colors: ThemeColorOverrides::new(colors),
    };

    let error = Theme::try_from_config(&appearance).expect_err("unknown token should fail");

    assert!(error.to_string().contains("component_specific_blue"));
}

#[test]
fn theme_resolve_rejects_invalid_hex_override() {
    let mut colors = BTreeMap::new();
    colors.insert("surface_base".to_owned(), "blue".to_owned());
    let appearance = AppearanceConfig {
        theme: ThemeId::SigilDark,
        colors: ThemeColorOverrides::new(colors),
    };

    let error = Theme::try_from_config(&appearance).expect_err("invalid hex should fail");

    assert!(
        error
            .to_string()
            .contains("appearance.colors.surface_base must be #RRGGBB")
    );
}

#[test]
fn contrast_ratio_orders_light_and_dark_colors() {
    let high = contrast::contrast_ratio(Color::White, Color::Black)
        .expect("rgb named colors should calculate contrast");
    let low = contrast::contrast_ratio(Color::Rgb(120, 120, 120), Color::Rgb(130, 130, 130))
        .expect("rgb colors should calculate contrast");

    assert!(high > 20.0);
    assert!(low < 1.2);
}

#[test]
fn color_token_allowlist_contains_documented_core_tokens() {
    for token in [
        "surface_base",
        "text_primary",
        "accent_primary",
        "diff_added_bg",
        "approval_bg",
        "markdown_code_bg",
        "config_selected_bg",
    ] {
        assert!(COLOR_TOKEN_NAMES.contains(&token));
    }
}
