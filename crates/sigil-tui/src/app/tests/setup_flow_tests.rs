use super::*;
use sigil_provider_deepseek::SIGIL_API_KEY_ENV;

#[test]
fn setup_lines_include_startup_error_and_missing_auth_summary() {
    let app = AppState::from_setup(
        Path::new("sigil.toml").to_path_buf(),
        Path::new(".").to_path_buf(),
        Some("config load failed".to_owned()),
    );

    let lines = app.setup_lines().join("\n");

    assert!(lines.contains("load failed: config load failed"));
    assert!(lines.contains("auth=missing"));
    assert_eq!(app.last_notice(), Some("config load failed"));
}

#[test]
fn setup_ctrl_s_requires_trust_before_completion() -> Result<()> {
    let mut app = AppState::from_setup(
        Path::new("sigil.toml").to_path_buf(),
        Path::new(".").to_path_buf(),
        None,
    );

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))?;

    assert!(action.is_none());
    assert_eq!(
        app.last_notice(),
        Some("trust the current folder before starting sigil")
    );
    assert!(app.events.iter().any(|event| {
        event.label == "setup:error"
            && event.detail == "trust the current folder before starting sigil"
    }));
    Ok(())
}

#[test]
fn setup_navigation_and_trust_toggle_update_state() -> Result<()> {
    let mut app = AppState::from_setup(
        Path::new("sigil.toml").to_path_buf(),
        Path::new(".").to_path_buf(),
        None,
    );

    assert!(app.is_setup_mode());
    let state = app
        .setup_state
        .as_ref()
        .expect("setup state should exist in setup mode");
    assert_eq!(state.selected_field, SetupField::TrustCurrentFolder);
    assert!(!state.trusted_current_folder);

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    let state = app
        .setup_state
        .as_ref()
        .expect("setup state should exist after toggling trust");
    assert!(state.trusted_current_folder);
    assert_eq!(app.last_notice(), Some("trust current folder on"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))?;
    assert_eq!(app.last_notice(), Some("setup field model"));
    let state = app
        .setup_state
        .as_ref()
        .expect("setup state should exist after moving selection");
    assert_eq!(state.selected_field, SetupField::Model);

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT))?;
    assert_eq!(app.last_notice(), Some("setup field trust_current_folder"));
    let state = app
        .setup_state
        .as_ref()
        .expect("setup state should exist after reverse navigation");
    assert_eq!(state.selected_field, SetupField::TrustCurrentFolder);
    Ok(())
}

#[test]
fn typing_in_setup_model_field_opens_text_modal() -> Result<()> {
    let mut app = AppState::from_setup(
        Path::new("sigil.toml").to_path_buf(),
        Path::new(".").to_path_buf(),
        None,
    );
    app.setup_state
        .as_mut()
        .expect("setup state should exist")
        .selected_field = SetupField::Model;

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert!(app.has_modal());
    assert_eq!(app.modal_title(), Some("Model ID"));
    assert_eq!(app.last_notice(), Some("editing model"));
    let lines = app.modal_lines().join("\n");
    assert!(lines.contains("model: g|"));
    Ok(())
}

#[test]
fn setup_screen_toggles_trust_and_opens_inline_field_modals() -> Result<()> {
    let temp = tempdir()?;
    let config_path = temp.path().join("config").join("sigil.toml");
    let workspace_root = temp.path().join("workspace");
    let mut app = AppState::from_setup(
        config_path,
        workspace_root,
        Some("invalid existing config".to_owned()),
    );

    let setup_lines = app.setup_lines().join("\n");
    assert!(setup_lines.contains("Quick setup"));
    assert!(setup_lines.contains("auth=missing"));
    assert!(setup_lines.contains("load failed: invalid existing config"));
    assert!(setup_lines.contains(&format!("env={SIGIL_API_KEY_ENV}")));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert_eq!(app.last_notice(), Some("trust current folder on"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))?;
    assert_eq!(app.last_notice(), Some("setup field model"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE))?;
    assert_eq!(app.modal_title(), Some("Model ID"));
    assert_eq!(app.modal_input_cursor(), Some(("model".to_owned(), 1, 3)));
    assert!(app.modal_lines().join("\n").contains("model: p|"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;
    assert_eq!(app.last_notice(), Some("closed text input"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert_eq!(app.last_notice(), Some("setup field api_key"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE))?;
    assert_eq!(app.modal_title(), Some("API Key"));
    assert_eq!(app.modal_input_cursor(), Some(("api_key".to_owned(), 1, 4)));
    assert!(app.modal_lines().join("\n").contains("api_key: *|"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert_eq!(app.last_notice(), Some("updated api key"));
    assert_eq!(
        app.setup_state.as_ref().map(|state| state.api_key.as_str()),
        Some("s")
    );
    Ok(())
}
