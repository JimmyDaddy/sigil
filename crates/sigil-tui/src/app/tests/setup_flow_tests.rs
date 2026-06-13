use super::super::setup_flow::{build_setup_root_config, validate_setup_state};
use super::*;
use crate::setup::SetupState;
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
fn setup_lines_return_empty_when_setup_state_is_absent() {
    let app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    assert!(app.setup_lines().is_empty());
}

#[test]
fn setup_lines_render_selected_actions_for_model_api_key_and_save() {
    let mut app = AppState::from_setup(
        Path::new("sigil.toml").to_path_buf(),
        Path::new(".").to_path_buf(),
        None,
    );
    let state = app.setup_state.as_mut().expect("setup state should exist");
    state.selected_field = SetupField::Model;
    let lines = app.setup_lines().join("\n");
    assert!(lines.contains("> model                 : deepseek-v4-flash  [Enter choose]"));

    app.setup_state
        .as_mut()
        .expect("setup state should exist")
        .selected_field = SetupField::ApiKey;
    let lines = app.setup_lines().join("\n");
    assert!(lines.contains("> api_key"));
    assert!(lines.contains("[Enter input]"));

    app.setup_state
        .as_mut()
        .expect("setup state should exist")
        .selected_field = SetupField::Save;
    let lines = app.setup_lines().join("\n");
    assert!(lines.contains("> [save and start]"));
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
fn setup_ctrl_c_and_missing_state_guards_are_noops() -> Result<()> {
    let mut app = AppState::from_setup(
        Path::new("sigil.toml").to_path_buf(),
        Path::new(".").to_path_buf(),
        None,
    );

    let action =
        app.handle_setup_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))?;
    assert!(action.is_none());
    assert!(app.should_quit);

    app.should_quit = false;
    app.setup_state = None;
    let action = app.handle_setup_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))?;
    assert!(action.is_none());
    assert!(!app.should_quit);
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
fn setup_backspace_and_unhandled_characters_do_not_change_state() -> Result<()> {
    let mut app = AppState::from_setup(
        Path::new("sigil.toml").to_path_buf(),
        Path::new(".").to_path_buf(),
        None,
    );
    app.setup_state
        .as_mut()
        .expect("setup state should exist")
        .selected_field = SetupField::Save;

    let action =
        app.handle_setup_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE))?;
    assert!(action.is_none());
    assert_eq!(
        app.setup_state.as_ref().map(|state| state.selected_field),
        Some(SetupField::Save)
    );

    let action =
        app.handle_setup_key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE))?;
    assert!(action.is_none());
    assert_eq!(
        app.setup_state.as_ref().map(|state| state.selected_field),
        Some(SetupField::Save)
    );
    Ok(())
}

#[test]
fn setup_unmatched_keys_and_missing_state_completion_are_noops() -> Result<()> {
    let mut app = AppState::from_setup(
        Path::new("sigil.toml").to_path_buf(),
        Path::new(".").to_path_buf(),
        None,
    );

    let action = app.handle_setup_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;
    assert!(action.is_none());
    assert!(app.last_notice().is_none());

    app.setup_state = None;
    let action = app.complete_setup()?;
    assert!(action.is_none());
    Ok(())
}

#[test]
fn setup_enter_on_model_and_api_key_open_existing_value_modals() -> Result<()> {
    let mut app = AppState::from_setup(
        Path::new("sigil.toml").to_path_buf(),
        Path::new(".").to_path_buf(),
        None,
    );
    let state = app.setup_state.as_mut().expect("setup state should exist");
    state.selected_field = SetupField::Model;
    state.model = "deepseek-chat".to_owned();

    let action = app.handle_setup_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(action.is_none());
    assert_eq!(app.modal_title(), Some("Model"));

    let _ = app.handle_setup_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;
    let state = app.setup_state.as_mut().expect("setup state should remain");
    state.selected_field = SetupField::ApiKey;
    state.api_key = "secret-key".to_owned();

    let action = app.handle_setup_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(action.is_none());
    assert_eq!(app.modal_title(), Some("API Key"));
    assert!(
        app.modal_lines()
            .join("\n")
            .contains("api_key: **********|")
    );
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
fn setup_validation_and_builder_reject_empty_model_and_auth() {
    let mut state = SetupState::new(Path::new("sigil.toml").to_path_buf(), None);
    state.trusted_current_folder = true;
    state.model = "  ".to_owned();
    state.api_key = "test-key".to_owned();

    assert_eq!(
        validate_setup_state(&state).as_deref(),
        Some("model cannot be empty")
    );
    assert_eq!(
        build_setup_root_config(&state)
            .expect_err("empty model should fail")
            .to_string(),
        "model cannot be empty"
    );

    if std::env::var(SIGIL_API_KEY_ENV).is_err() {
        state.model = "deepseek-v4-flash".to_owned();
        state.api_key.clear();

        assert_eq!(
            validate_setup_state(&state),
            Some(format!("provide api_key or export {SIGIL_API_KEY_ENV}"))
        );
        assert_eq!(
            build_setup_root_config(&state)
                .expect_err("missing auth should fail")
                .to_string(),
            format!("provide api_key or export {SIGIL_API_KEY_ENV}")
        );
    }

    state.trusted_current_folder = false;
    state.model = "deepseek-v4-flash".to_owned();
    state.api_key = "test-key".to_owned();
    assert_eq!(
        build_setup_root_config(&state)
            .expect_err("untrusted folder should fail")
            .to_string(),
        "trust the current folder before starting sigil"
    );
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
