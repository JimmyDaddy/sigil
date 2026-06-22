use std::collections::BTreeMap;

use sigil_kernel::{AppearanceConfig, ThemeColorOverrides, ThemeId};
use sigil_runtime::doctor::DoctorStatus;

use super::*;

#[test]
fn appearance_doctor_checks_reports_ok_for_builtin_theme() {
    let checks = appearance_doctor_checks(&AppearanceConfig::default());

    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].status, DoctorStatus::Ok);
    assert_eq!(checks[0].name, "appearance:contrast");
    assert!(checks[0].message.contains("theme=sigil_dark"));
}

#[test]
fn appearance_doctor_checks_reports_invalid_override_as_appearance_colors() {
    let mut colors = BTreeMap::new();
    colors.insert("surface_base".to_owned(), "blue".to_owned());
    let appearance = AppearanceConfig {
        theme: ThemeId::SigilDark,
        colors: ThemeColorOverrides::new(colors),
    };

    let checks = appearance_doctor_checks(&appearance);

    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].status, DoctorStatus::Warn);
    assert_eq!(checks[0].name, "appearance:colors");
    assert!(checks[0].message.contains("surface_base"));
    assert!(
        checks[0]
            .remediation
            .as_deref()
            .is_some_and(|remediation| remediation.contains("[appearance.colors]"))
    );
}

#[test]
fn appearance_doctor_checks_reports_low_contrast_override() {
    let mut colors = BTreeMap::new();
    colors.insert("surface_base".to_owned(), "#101010".to_owned());
    colors.insert("text_primary".to_owned(), "#111111".to_owned());
    let appearance = AppearanceConfig {
        theme: ThemeId::SigilDark,
        colors: ThemeColorOverrides::new(colors),
    };

    let checks = appearance_doctor_checks(&appearance);

    assert!(checks.iter().any(|check| {
        check.status == DoctorStatus::Warn
            && check.name == "appearance:contrast:text-base"
            && check
                .message
                .contains("text_primary on surface_base contrast")
    }));
}

#[test]
fn appearance_doctor_checks_groups_semantic_and_structural_warnings() {
    let mut colors = BTreeMap::new();
    colors.insert("status_success".to_owned(), "#445566".to_owned());
    colors.insert("status_warning".to_owned(), "#445567".to_owned());
    colors.insert("border_subtle".to_owned(), "#202020".to_owned());
    colors.insert("surface_panel".to_owned(), "#202020".to_owned());
    let appearance = AppearanceConfig {
        theme: ThemeId::SigilDark,
        colors: ThemeColorOverrides::new(colors),
    };

    let checks = appearance_doctor_checks(&appearance);

    assert!(checks.iter().any(|check| {
        check.name == "appearance:semantic"
            && check.message.contains("status_success/status_warning")
    }));
    assert!(checks.iter().any(|check| {
        check.name == "appearance:structural"
            && check.message.contains("border_subtle/surface_panel")
    }));
}
