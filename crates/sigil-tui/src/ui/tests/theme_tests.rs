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
fn builtin_themes_keep_core_text_and_ui_contrast_readable() {
    for theme_id in ThemeId::all() {
        let theme = Theme::builtin(*theme_id);
        let palette = &theme.palette;
        for (label, foreground, background, minimum) in [
            (
                "text_primary/surface_base",
                palette.text_primary,
                palette.surface_base,
                4.5,
            ),
            (
                "text_primary/surface_panel",
                palette.text_primary,
                palette.surface_panel,
                4.5,
            ),
            (
                "text_secondary/surface_base",
                palette.text_secondary,
                palette.surface_base,
                3.0,
            ),
            (
                "text_muted/surface_base",
                palette.text_muted,
                palette.surface_base,
                3.0,
            ),
            (
                "selection_fg/selection_bg",
                palette.selection_fg,
                palette.selection_bg,
                3.0,
            ),
            (
                "button_selected_fg/button_selected_bg",
                palette.button_selected_fg,
                palette.button_selected_bg,
                3.0,
            ),
            (
                "diff_added_fg/diff_added_bg",
                palette.diff_added_fg,
                palette.diff_added_bg,
                3.0,
            ),
            (
                "diff_removed_fg/diff_removed_bg",
                palette.diff_removed_fg,
                palette.diff_removed_bg,
                3.0,
            ),
            (
                "markdown_code_fg/markdown_code_bg",
                palette.markdown_code_fg,
                palette.markdown_code_bg,
                4.5,
            ),
            (
                "text_primary/approval_bg",
                palette.text_primary,
                palette.approval_bg,
                4.5,
            ),
        ] {
            let ratio = contrast::contrast_ratio(foreground, background)
                .unwrap_or_else(|| panic!("{theme_id:?} {label} should use contrastable colors"));
            assert!(
                ratio >= minimum,
                "{theme_id:?} {label} contrast {ratio:.2} is below {minimum:.1}"
            );
        }
    }
}

#[test]
fn theme_diagnostics_passes_builtin_themes() {
    for theme_id in ThemeId::all() {
        let appearance = AppearanceConfig {
            theme: *theme_id,
            colors: ThemeColorOverrides::default(),
        };

        let report =
            diagnostics::diagnose_appearance(&appearance).expect("built-in theme should resolve");

        assert!(report.checked > 0);
        assert!(
            report.diagnostics.is_empty(),
            "{theme_id:?} should not emit diagnostics: {:?}",
            report.diagnostics
        );
    }
}

#[test]
fn theme_diagnostics_reports_low_contrast_override() {
    let mut colors = BTreeMap::new();
    colors.insert("surface_base".to_owned(), "#101010".to_owned());
    colors.insert("text_primary".to_owned(), "#111111".to_owned());
    let appearance = AppearanceConfig {
        theme: ThemeId::SigilDark,
        colors: ThemeColorOverrides::new(colors),
    };

    let report = diagnostics::diagnose_appearance(&appearance).expect("override should resolve");

    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic.name == "text-base"
            && diagnostic.kind == diagnostics::ThemeDiagnosticKind::ContrastPair
            && diagnostic.metric == diagnostics::ThemeDiagnosticMetric::ContrastRatio
            && diagnostic.actual < diagnostic.minimum
    }));
}

#[test]
fn theme_diagnostics_reports_surface_pair_override() {
    let mut colors = BTreeMap::new();
    colors.insert("surface_input".to_owned(), "#101010".to_owned());
    colors.insert("text_primary".to_owned(), "#111111".to_owned());
    let appearance = AppearanceConfig {
        theme: ThemeId::SigilDark,
        colors: ThemeColorOverrides::new(colors),
    };

    let report = diagnostics::diagnose_appearance(&appearance).expect("override should resolve");

    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic.name == "composer-input"
            && diagnostic.tier == diagnostics::ThemeDiagnosticTier::Surface
    }));
}

#[test]
fn theme_diagnostics_reports_visible_foreground_overrides() {
    let mut colors = BTreeMap::new();
    for token in [
        "markdown_heading",
        "markdown_link",
        "diff_header_fg",
        "diff_hunk_fg",
        "diff_context_fg",
        "diff_gutter_fg",
        "config_warning",
        "config_danger",
    ] {
        colors.insert(token.to_owned(), "#07080A".to_owned());
    }
    let appearance = AppearanceConfig {
        theme: ThemeId::SigilDark,
        colors: ThemeColorOverrides::new(colors),
    };

    let report = diagnostics::diagnose_appearance(&appearance).expect("override should resolve");

    for expected in [
        "markdown-heading",
        "markdown-link",
        "diff-header",
        "diff-hunk",
        "diff-context",
        "diff-gutter",
        "config-warning",
        "config-danger",
    ] {
        assert!(
            report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.name == expected),
            "{expected} should report a contrast warning"
        );
    }
}

#[test]
fn theme_diagnostics_reports_semantic_similarity() {
    let mut colors = BTreeMap::new();
    colors.insert("status_success".to_owned(), "#445566".to_owned());
    colors.insert("status_warning".to_owned(), "#445567".to_owned());
    let appearance = AppearanceConfig {
        theme: ThemeId::SigilDark,
        colors: ThemeColorOverrides::new(colors),
    };

    let report = diagnostics::diagnose_appearance(&appearance).expect("override should resolve");

    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic.name == "status-success-warning"
            && diagnostic.kind == diagnostics::ThemeDiagnosticKind::SemanticSeparation
            && diagnostic.metric == diagnostics::ThemeDiagnosticMetric::SrgbDistance
            && diagnostic.actual < diagnostic.minimum
    }));
}

#[test]
fn theme_diagnostics_reports_structural_cue_warning() {
    let mut colors = BTreeMap::new();
    colors.insert("border_subtle".to_owned(), "#202020".to_owned());
    colors.insert("surface_panel".to_owned(), "#202020".to_owned());
    let appearance = AppearanceConfig {
        theme: ThemeId::SigilDark,
        colors: ThemeColorOverrides::new(colors),
    };

    let report = diagnostics::diagnose_appearance(&appearance).expect("override should resolve");

    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic.name == "border-subtle-panel"
            && diagnostic.kind == diagnostics::ThemeDiagnosticKind::StructuralCue
            && diagnostic.tier == diagnostics::ThemeDiagnosticTier::Structural
    }));
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
