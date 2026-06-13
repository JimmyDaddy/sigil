use serde_json::json;
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
    assert!(rendered.contains("[ok] config:load - config parsed"));
    assert!(rendered.contains("provider:auth"));
    assert!(!rendered.contains("super-secret-test-key"));
    Ok(())
}

#[test]
fn render_doctor_report_includes_summary_and_check_lines() {
    let report = DoctorReport {
        checks: vec![DoctorCheck {
            status: DoctorStatus::Warn,
            name: "terminal".to_owned(),
            message: "TERM is not set".to_owned(),
        }],
    };

    let rendered = render_doctor_report(&report);

    assert_eq!(rendered, "doctor: warn\n[warn] terminal - TERM is not set");
}
