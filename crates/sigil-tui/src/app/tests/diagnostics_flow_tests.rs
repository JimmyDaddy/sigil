use std::collections::BTreeMap;

use sigil_kernel::{
    ApprovalMode, PermissionConfirmation, PermissionDecision, ThemeColorOverrides, ToolAccess,
    ToolSubject,
};
use sigil_runtime::doctor::{DoctorCheck, DoctorReport, DoctorStatus};
use tempfile::tempdir;

use super::super::tests::common::test_config;
use super::*;

#[test]
fn doctor_slash_command_renders_appearance_warnings() -> anyhow::Result<()> {
    let temp = tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    let mut config = test_config();
    let mut colors = BTreeMap::new();
    colors.insert("surface_base".to_owned(), "#101010".to_owned());
    colors.insert("text_primary".to_owned(), "#101010".to_owned());
    config.appearance.colors = ThemeColorOverrides::new(colors);
    config.save(&config_path)?;
    let mut app = AppState::from_root_config(&config_path, &config);
    app.composer.input = "/doctor".to_owned();

    let action = app.submit_input()?;

    assert!(action.is_none());
    let rendered = app
        .timeline
        .iter()
        .find(|entry| entry.role == TimelineRole::Notice && entry.text.starts_with("doctor:"))
        .expect("doctor report should be rendered")
        .text
        .clone();
    assert!(rendered.contains("appearance:contrast:text-base"));
    assert!(rendered.contains("text_primary on surface_base"));
    assert!(rendered.contains("cutover: epoch=legacy authority=legacy blockers=0"));
    Ok(())
}

#[test]
fn render_doctor_report_includes_summary_and_check_lines() {
    let report = DoctorReport {
        cutover: Default::default(),
        checks: vec![
            DoctorCheck {
                status: DoctorStatus::Ok,
                name: "config:load".to_owned(),
                message: "config parsed".to_owned(),
                remediation: None,
            },
            DoctorCheck {
                status: DoctorStatus::Warn,
                name: "terminal".to_owned(),
                message: "TERM is not set".to_owned(),
                remediation: Some("set TERM in the shell before launching the TUI".to_owned()),
            },
        ],
    };

    let rendered = render_doctor_report(&report);

    assert!(rendered.starts_with(
        "doctor: warn\ncutover: epoch=legacy authority=legacy blockers=0\nsummary: 0 error · 1 warn · 1 ok"
    ));
    assert!(rendered.contains("needs attention:\n- [warn] terminal\n  TERM is not set"));
    assert!(rendered.contains("  fix: set TERM in the shell before launching the TUI"));
    assert!(rendered.contains("checks:\n[ok] config:load\n  config parsed"));
    assert!(rendered.contains("[warn] terminal\n  TERM is not set"));
}

#[test]
fn r71_headless_permission_fixture_matches_kernel_blockers() {
    let mut confirmation = PermissionDecision::new(
        ApprovalMode::Allow,
        "write_file",
        ToolAccess::Write,
        vec![ToolSubject::path("notes.txt", "notes.txt")],
        false,
    );
    confirmation.confirmation = Some(PermissionConfirmation::TypePhrase {
        phrase: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            .to_owned(),
    });
    assert_eq!(
        confirmation.headless_blocker(),
        Some(sigil_kernel::HeadlessPermissionBlockerV1::ConfirmationRequired)
    );

    confirmation.confirmation = None;
    assert_eq!(confirmation.headless_blocker(), None);
    confirmation.mode = ApprovalMode::Ask;
    assert_eq!(
        confirmation.headless_blocker(),
        Some(sigil_kernel::HeadlessPermissionBlockerV1::ApprovalRequired)
    );
}

#[test]
fn render_doctor_report_marks_all_ok_reports_ready() {
    let report = DoctorReport {
        cutover: Default::default(),
        checks: vec![DoctorCheck {
            status: DoctorStatus::Ok,
            name: "terminal".to_owned(),
            message: "TERM=xterm-256color".to_owned(),
            remediation: None,
        }],
    };

    let rendered = render_doctor_report(&report);

    assert!(rendered.starts_with(
        "doctor: ok\ncutover: epoch=legacy authority=legacy blockers=0\nsummary: 0 error · 0 warn · 1 ok"
    ));
    assert!(rendered.contains("ready: all checks passed"));
    assert!(!rendered.contains("needs attention:"));
}
