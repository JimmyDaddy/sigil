use std::collections::BTreeMap;

use serde_json::json;
use sigil_kernel::ThemeColorOverrides;
use sigil_runtime::doctor::{DoctorCheck, DoctorReport, DoctorStatus};
use tempfile::tempdir;

use super::super::tests::common::test_config;
use super::*;

#[test]
fn doctor_slash_command_renders_runtime_report_without_secret() -> anyhow::Result<()> {
    let temp = tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    let mut config = test_config();
    config.workspace.root = ".".to_owned();
    config.providers.insert(
        "deepseek".to_owned(),
        json!({
            "base_url": "https://example.com",
            "model": "deepseek-v4-flash",
            "api_key": "super-secret-test-key"
        }),
    );
    config.save(&config_path)?;
    let mut app = AppState::from_root_config(&config_path, &config);
    app.input = "/doctor".to_owned();

    let action = app.submit_input()?;

    assert!(action.is_none());
    assert!(matches!(
        app.last_notice(),
        Some(notice) if notice.starts_with("doctor:")
    ));
    assert!(app.events.iter().any(|event| event.label == "doctor"));
    let rendered = app
        .timeline
        .iter()
        .find(|entry| entry.role == TimelineRole::Notice && entry.text.starts_with("doctor:"))
        .expect("doctor report should be rendered")
        .text
        .clone();
    assert!(rendered.contains("[ok] config:load\n  config parsed"));
    assert!(rendered.contains("summary:"));
    assert!(rendered.contains("needs attention:"));
    assert!(rendered.contains("provider:auth"));
    assert!(rendered.contains("fix: prefer SIGIL_API_KEY"));
    assert!(!rendered.contains("super-secret-test-key"));
    Ok(())
}

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
    app.input = "/doctor".to_owned();

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
    Ok(())
}

#[test]
fn render_doctor_report_includes_summary_and_check_lines() {
    let report = DoctorReport {
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

    assert!(rendered.starts_with("doctor: warn\nsummary: 0 error · 1 warn · 1 ok"));
    assert!(rendered.contains("needs attention:\n- [warn] terminal\n  TERM is not set"));
    assert!(rendered.contains("  fix: set TERM in the shell before launching the TUI"));
    assert!(rendered.contains("checks:\n[ok] config:load\n  config parsed"));
    assert!(rendered.contains("[warn] terminal\n  TERM is not set"));
}

#[test]
fn render_doctor_report_marks_all_ok_reports_ready() {
    let report = DoctorReport {
        checks: vec![DoctorCheck {
            status: DoctorStatus::Ok,
            name: "terminal".to_owned(),
            message: "TERM=xterm-256color".to_owned(),
            remediation: None,
        }],
    };

    let rendered = render_doctor_report(&report);

    assert!(rendered.starts_with("doctor: ok\nsummary: 0 error · 0 warn · 1 ok"));
    assert!(rendered.contains("ready: all checks passed"));
    assert!(!rendered.contains("needs attention:"));
}
