use super::*;

#[test]
fn semantic_theme_is_versioned_and_role_based() {
    let theme = SemanticTheme::default();
    assert_eq!(theme.name(), "default");
    assert_eq!(theme.version(), 1);
    assert_eq!(
        theme.color(ThemeRole::Accent),
        ThemeColor::rgb(90, 170, 255)
    );
    assert!(
        SemanticTheme::new(
            "",
            1,
            theme.background,
            theme.foreground,
            theme.muted,
            theme.accent,
            theme.danger,
            theme.success,
        )
        .is_err()
    );
}
