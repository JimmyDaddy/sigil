use super::*;

#[test]
fn alt_d_requests_changed_file_diagnostics_when_code_intelligence_is_enabled() -> Result<()> {
    let temp = tempdir()?;
    let mut config = test_config();
    config.workspace.root = temp.path().display().to_string();
    config.code_intelligence.enabled = true;
    let mut app = AppState::from_root_config(&temp.path().join("sigil.toml"), &config);

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::ALT))?;

    assert!(matches!(
        action,
        Some(AppAction::CheckChangedFilesDiagnostics)
    ));
    assert!(
        app.events
            .iter()
            .any(|event| event.label == "code:check" && event.detail == "changed files")
    );
    Ok(())
}

#[test]
fn alt_d_does_not_request_diagnostics_when_code_intelligence_is_disabled() -> Result<()> {
    let temp = tempdir()?;
    let mut config = test_config();
    config.workspace.root = temp.path().display().to_string();
    let mut app = AppState::from_root_config(&temp.path().join("sigil.toml"), &config);

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::ALT))?;

    assert!(action.is_none());
    assert_eq!(app.last_notice.as_deref(), Some("code intelligence is off"));
    Ok(())
}

#[test]
fn alt_d_does_not_request_diagnostics_while_busy() -> Result<()> {
    let temp = tempdir()?;
    let mut config = test_config();
    config.workspace.root = temp.path().display().to_string();
    config.code_intelligence.enabled = true;
    let mut app = AppState::from_root_config(&temp.path().join("sigil.toml"), &config);
    app.is_busy = true;

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::ALT))?;

    assert!(action.is_none());
    assert_eq!(
        app.last_notice.as_deref(),
        Some("wait for the active run before checking changes")
    );
    Ok(())
}
