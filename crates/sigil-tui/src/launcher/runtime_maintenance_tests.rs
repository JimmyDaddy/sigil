use super::*;

#[test]
fn attachment_failure_is_not_provider_failure_or_a_ready_run() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let mut config = crate::app::tests::common::test_config();
    config.workspace.root = fixture.path().display().to_string();
    let mut app = AppState::from_root_config(&fixture.path().join("sigil.toml"), &config);
    app.handle_worker_message(WorkerMessage::WorkerReady)?;
    app.runtime.is_busy = true;
    report_maintenance_failure(
        &mut app,
        "runtime lifecycle incomplete: local attachment failed",
    )?;
    assert!(!app.worker_ready());
    assert!(!app.runtime.is_busy);
    assert!(app.runtime.pending_session_route_recovery_code.is_none());
    assert_eq!(
        app.last_notice(),
        Some("runtime lifecycle incomplete: local attachment failed")
    );
    Ok(())
}
