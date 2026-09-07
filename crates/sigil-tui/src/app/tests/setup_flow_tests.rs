use super::super::setup_flow::{build_setup_root_config, validate_setup_state};
use super::*;
use crate::setup::SetupCredentialSource;
use crate::setup::SetupState;
use sigil_kernel::{MultiAgentMode, PermissionMode, TaskRoutingPolicy};
use sigil_runtime::DEFAULT_SETUP_API_KEY_ENV;

#[test]
fn setup_lines_include_invalid_config_error_and_missing_auth_summary() -> Result<()> {
    let _env_guard = crate::test_env::lock();
    let _api_key = crate::test_env::EnvScope::unset("SIGIL_API_KEY");
    let temp = tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    std::fs::write(&config_path, "this = [is malformed")?;
    let app = AppState::from_setup(
        config_path,
        temp.path().to_path_buf(),
        Some("config load failed".to_owned()),
    );

    let lines = app.setup_lines().join("\n");

    assert!(lines.contains("configuration is invalid: config load failed"));
    assert!(lines.contains("explicitly save this reviewed current-schema replacement"));
    assert!(lines.contains("> DeepSeek"));
    assert!(lines.contains("SIGIL_API_KEY not set"));
    assert_eq!(app.last_notice(), Some("config load failed"));
    Ok(())
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
    let lines = app.setup_lines().join("\n");
    assert!(lines.contains("> DeepSeek"));
    assert!(lines.contains("Enter continue"));

    let _ = app
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .expect("provider choice should succeed");

    let state = app.setup_state.as_mut().expect("setup state should exist");
    state.selected_field = SetupField::Model;
    let lines = app.setup_lines().join("\n");
    assert!(
        lines.contains(
            "> model                 : deepseek-v4-flash  [Enter choose · type model ID]"
        )
    );

    app.setup_state
        .as_mut()
        .expect("setup state should exist")
        .selected_field = SetupField::ApiKey;
    let lines = app.setup_lines().join("\n");
    assert!(lines.contains("> authentication"));
    assert!(lines.contains("[Left/Right choose · Enter continue]"));

    app.setup_state
        .as_mut()
        .expect("setup state should exist")
        .selected_field = SetupField::Save;
    let lines = app.setup_lines().join("\n");
    assert!(lines.contains("> [review, trust folder, save and start]"));
    assert!(lines.contains("orchestration: manual / explicit_request_only"));
    assert!(lines.contains("current session: starts with this route"));
    assert!(lines.contains("max output tokens: automatic"));
}

#[test]
fn setup_ctrl_s_saves_and_starts_without_a_separate_trust_toggle() -> Result<()> {
    let _env_guard = crate::test_env::lock();
    let _api_key = crate::test_env::EnvScope::set("SIGIL_API_KEY", "test-key");
    let temp = tempdir()?;
    let config_path = temp.path().join("config").join("sigil.toml");
    std::fs::create_dir_all(
        config_path
            .parent()
            .expect("config path should have a parent"),
    )?;
    let mut app = AppState::from_setup(config_path.clone(), temp.path().to_path_buf(), None);
    app.setup_state
        .as_mut()
        .expect("setup state should exist")
        .credential_source = SetupCredentialSource::Environment;
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))?;

    let Some(AppAction::SetupCompleted {
        config_path: saved_path,
        root_config,
    }) = action
    else {
        panic!("Ctrl-S should complete setup")
    };
    assert_eq!(saved_path, config_path);
    assert_eq!(root_config.config_version, sigil_kernel::CONFIG_VERSION_V2);
    assert!(root_config.agent.runtime_provider.is_empty());
    assert_eq!(
        root_config.agent.connection.as_ref().map(|id| id.as_str()),
        Some("deepseek-default")
    );
    assert_eq!(root_config.task.routing_policy, TaskRoutingPolicy::Auto);
    assert_eq!(
        root_config.task.multi_agent_mode,
        MultiAgentMode::ExplicitRequestOnly
    );
    assert!(saved_path.exists());
    assert!(!std::fs::read_to_string(saved_path)?.contains("test-key"));
    let setup = app
        .setup_state
        .as_ref()
        .expect("setup state should remain available");
    assert_eq!(
        setup
            .startup_config
            .as_ref()
            .and_then(|snapshot| snapshot.parsed())
            .map(|config| config.config_version),
        Some(sigil_kernel::CONFIG_VERSION_V2)
    );
    Ok(())
}

#[test]
fn setup_enter_on_replacement_action_saves_and_starts() -> Result<()> {
    let _env_guard = crate::test_env::lock();
    let _api_key = crate::test_env::EnvScope::set("SIGIL_API_KEY", "test-key");
    let temp = tempdir()?;
    let config_path = temp.path().join("config").join("sigil.toml");
    std::fs::create_dir_all(
        config_path
            .parent()
            .expect("config path should have a parent"),
    )?;
    std::fs::write(&config_path, "this = [is malformed")?;
    let mut app = AppState::from_setup(
        config_path.clone(),
        temp.path().to_path_buf(),
        Some("invalid TOML".to_owned()),
    );
    let state = app.setup_state.as_mut().expect("setup state should exist");
    state.credential_source = SetupCredentialSource::Environment;
    state.selected_field = SetupField::Save;

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    assert!(matches!(action, Some(AppAction::SetupCompleted { .. })));
    let persisted = std::fs::read_to_string(&config_path)?;
    assert!(persisted.contains("config_version = 2"));
    assert!(!persisted.contains("this = [is malformed"));
    Ok(())
}

#[test]
fn setup_enter_retries_after_a_valid_config_boot_failure_and_accepts_return_code() -> Result<()> {
    let _env_guard = crate::test_env::lock();
    let _sigil_api_key = crate::test_env::EnvScope::set("SIGIL_API_KEY", " ");
    let _deepseek_api_key = crate::test_env::EnvScope::set("DEEPSEEK_API_KEY", "test-key");
    let temp = tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    let mut existing = test_config();
    existing.permission.mode = PermissionMode::ReadOnly;
    existing.model_request.max_output_tokens = Some(8_000);
    existing.connections.insert(
        "local-secondary".to_owned(),
        serde_json::json!({
            "label": "Local secondary",
            "provider": "custom",
            "protocol": "chat_completions",
            "base_url": "http://127.0.0.1:11434/v1",
            "credential": { "source": "none" }
        }),
    );
    existing
        .connections
        .get_mut("deepseek-default")
        .and_then(serde_json::Value::as_object_mut)
        .expect("default connection object")
        .insert(
            "model_context_windows".to_owned(),
            serde_json::json!({ "deepseek-v4-flash": 128000 }),
        );
    existing.save(&config_path)?;
    let mut app = AppState::from_setup_with_recovery(
        config_path.clone(),
        temp.path().to_path_buf(),
        Some("authority state requires reconciliation".to_owned()),
        Some(sigil_kernel::PublicRouteRecoveryCode::AuthorityUnavailable),
    );
    let state = app.setup_state.as_mut().expect("setup state should exist");
    assert_eq!(state.provider_name, "deepseek");
    assert_eq!(state.model, "deepseek-v4-flash");
    assert_eq!(state.context_window_tokens, "128000");
    assert_eq!(state.max_output_tokens, "8000");
    state.selected_field = SetupField::Save;
    let setup_lines = app.setup_lines().join("\n");
    assert!(setup_lines.contains("current configuration is valid"));
    assert!(setup_lines.contains("inspect the current configuration and storage permissions"));

    let action =
        app.handle_setup_key_event(KeyEvent::new(KeyCode::Char('\r'), KeyModifiers::NONE))?;

    assert!(matches!(action, Some(AppAction::SetupCompleted { .. })));
    let saved = sigil_kernel::RootConfig::load_persisted(&config_path)?;
    assert_eq!(saved.permission.mode, PermissionMode::ReadOnly);
    assert_eq!(saved.model_request.max_output_tokens, Some(8_000));
    assert!(saved.connections.contains_key("local-secondary"));
    assert_eq!(
        saved.connections["deepseek-default"]["credential"]["name"],
        "SIGIL_API_KEY"
    );
    assert_eq!(
        saved.connections["deepseek-default"]["model_context_windows"]["deepseek-v4-flash"],
        128_000
    );
    Ok(())
}

#[test]
fn setup_exact_cas_failure_stays_visible_and_retryable() -> Result<()> {
    let _env_guard = crate::test_env::lock();
    let _api_key = crate::test_env::EnvScope::set("SIGIL_API_KEY", "test-key");
    let temp = tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    test_config().save(&config_path)?;
    let mut app = AppState::from_setup(
        config_path.clone(),
        temp.path().to_path_buf(),
        Some("authority state requires reconciliation".to_owned()),
    );
    app.setup_state
        .as_mut()
        .expect("setup state")
        .selected_field = SetupField::Save;
    let concurrently_changed = format!(
        "{}\n# concurrent edit\n",
        std::fs::read_to_string(&config_path)?
    );
    std::fs::write(&config_path, &concurrently_changed)?;

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert!(app.is_setup_mode());
    let lines = app.setup_lines().join("\n");
    assert!(lines.contains("Setup did not complete:"));
    assert!(lines.contains("config changed since Provider settings were loaded"));
    assert!(lines.contains("press Enter on review or Ctrl-S to retry"));
    assert_eq!(std::fs::read_to_string(config_path)?, concurrently_changed);
    Ok(())
}

#[test]
fn setup_write_failure_stays_visible_and_retryable() -> Result<()> {
    let _env_guard = crate::test_env::lock();
    let _api_key = crate::test_env::EnvScope::set("SIGIL_API_KEY", "test-key");
    let temp = tempdir()?;
    let blocked_parent = temp.path().join("config-parent-is-a-file");
    std::fs::write(&blocked_parent, b"not a directory")?;
    let config_path = blocked_parent.join("sigil.toml");
    let mut app = AppState::from_setup(config_path, temp.path().to_path_buf(), None);
    app.setup_state
        .as_mut()
        .expect("setup state")
        .selected_field = SetupField::Save;

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert!(app.is_setup_mode());
    let lines = app.setup_lines().join("\n");
    assert!(lines.contains("Setup did not complete:"));
    assert!(lines.contains("config update transaction lock failed"));
    assert!(lines.contains("press Enter on review or Ctrl-S to retry"));
    Ok(())
}

#[test]
fn setup_token_fields_accept_units_and_common_presets() -> Result<()> {
    let _env_guard = crate::test_env::lock();
    let _api_key = crate::test_env::EnvScope::set("SIGIL_API_KEY", "test-key");
    let temp = tempdir()?;
    let config_path = temp.path().join("config").join("sigil.toml");
    let mut app = AppState::from_setup(config_path, temp.path().to_path_buf(), None);
    {
        let state = app.setup_state.as_mut().expect("setup state should exist");
        state.credential_source = SetupCredentialSource::Environment;
        state.selected_field = SetupField::ContextWindow;
    }
    for character in "256K".chars() {
        let _ =
            app.handle_key_event(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE))?;
    }
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    {
        let state = app.setup_state.as_ref().expect("setup state should exist");
        assert_eq!(state.context_window_tokens, "256K");
    }
    {
        let state = app.setup_state.as_mut().expect("setup state should exist");
        state.selected_field = SetupField::MaxOutputTokens;
    }
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))?;
    let state = app.setup_state.as_ref().expect("setup state should exist");
    assert_eq!(state.max_output_tokens, "4K");
    Ok(())
}

#[test]
fn setup_explicitly_replaces_an_existing_malformed_config() -> Result<()> {
    let _env_guard = crate::test_env::lock();
    let _api_key = crate::test_env::EnvScope::unset(DEFAULT_SETUP_API_KEY_ENV);
    let temp = tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    std::fs::write(&config_path, "this = [is malformed")?;
    let mut app = AppState::from_setup(
        config_path.clone(),
        temp.path().to_path_buf(),
        Some("invalid TOML".to_owned()),
    );
    let state = app.setup_state.as_mut().expect("setup state should exist");
    state.api_key = SecretString::new("staged-only");
    state.selected_field = SetupField::Save;
    assert!(
        app.setup_lines()
            .join("\n")
            .contains("replace invalid config and start")
    );

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))?;

    assert!(matches!(action, Some(AppAction::SetupCompleted { .. })));
    let persisted = std::fs::read_to_string(&config_path)?;
    assert!(persisted.contains("config_version = 2"));
    assert!(!persisted.contains("this = [is malformed"));
    assert!(!persisted.contains("staged-only"));
    Ok(())
}

#[test]
fn setup_startup_recovery_error_blocks_publish_when_config_is_missing() -> Result<()> {
    let _env_guard = crate::test_env::lock();
    let _api_key = crate::test_env::EnvScope::unset(DEFAULT_SETUP_API_KEY_ENV);
    let temp = tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    let mut app = AppState::from_setup(
        config_path.clone(),
        temp.path().to_path_buf(),
        Some("provider configuration recovery is pending".to_owned()),
    );
    let state = app.setup_state.as_mut().expect("setup state should exist");
    state.api_key = SecretString::new("staged-only");
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))?;

    assert!(action.is_none());
    assert!(!config_path.exists());
    assert!(
        app.last_notice()
            .is_some_and(|notice| notice.contains("config changed since Provider settings"))
    );
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
fn setup_navigation_and_provider_switch_update_state() -> Result<()> {
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
    assert_eq!(state.selected_field, SetupField::Provider);
    assert_eq!(state.provider_name, "deepseek");

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    let state = app
        .setup_state
        .as_ref()
        .expect("setup state should exist after switching provider");
    assert_eq!(state.provider_name, "openai_responses");
    assert_eq!(state.model, "gpt-4.1");
    assert_eq!(app.last_notice(), Some("provider -> OpenAI"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert_eq!(app.last_notice(), Some("provider selected: OpenAI"));
    let state = app
        .setup_state
        .as_ref()
        .expect("setup state should exist after moving selection");
    assert_eq!(state.selected_field, SetupField::ApiKey);

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT))?;
    assert_eq!(app.last_notice(), Some("setup field provider"));
    let state = app
        .setup_state
        .as_ref()
        .expect("setup state should exist after reverse navigation");
    assert_eq!(state.selected_field, SetupField::Provider);
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
    state.credential_source = SetupCredentialSource::SecureStore;
    state.api_key = SecretString::new("secret-key");

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
fn setup_paste_updates_model_and_api_key_fields() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.handle_setup_paste_text("ignored");
    assert!(app.last_notice().is_none());

    let mut app = AppState::from_setup(
        Path::new("sigil.toml").to_path_buf(),
        Path::new(".").to_path_buf(),
        None,
    );
    app.handle_setup_paste_text("\n\u{0007}");
    assert!(app.last_notice().is_none());

    app.setup_state
        .as_mut()
        .expect("setup state should exist")
        .selected_field = SetupField::Model;
    app.handle_setup_paste_text("deepseek\nv4");
    assert_eq!(app.last_notice(), Some("updated model deepseekv4"));
    assert_eq!(
        app.setup_state.as_ref().map(|state| state.model.as_str()),
        Some("deepseekv4")
    );

    app.setup_state
        .as_mut()
        .expect("setup state should exist")
        .selected_field = SetupField::ApiKey;
    app.setup_state
        .as_mut()
        .expect("setup state should exist")
        .credential_source = SetupCredentialSource::SecureStore;
    app.handle_setup_paste_text("sk-test\n");
    assert_eq!(
        app.last_notice(),
        Some("staged API key for protected credential store")
    );
    assert_eq!(
        app.setup_state
            .as_ref()
            .map(|state| state.api_key.expose_secret()),
        Some("sk-test")
    );

    app.setup_state
        .as_mut()
        .expect("setup state should exist")
        .selected_field = SetupField::Save;
    app.handle_setup_paste_text("ignored");
    assert_eq!(
        app.last_notice(),
        Some("staged API key for protected credential store")
    );
}

#[test]
fn setup_validation_and_builder_reject_empty_model_and_auth() {
    // Pin the environment: the environment credential branch must be deterministic regardless of
    // the developer machine's shell state (other setup tests mutate the same variables under the
    // global lock).
    let _env_guard = crate::test_env::lock();
    let _unset_api_key = crate::test_env::EnvScope::unset("SIGIL_API_KEY");
    let mut state = SetupState::new(Path::new("sigil.toml").to_path_buf(), None);
    state.model = "  ".to_owned();
    state.api_key = SecretString::new("test-key");

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

    // With the environment pinned absent, the default credential source is the secure store and
    // an empty key is rejected deterministically.
    state.model = "deepseek-v4-flash".to_owned();
    state.api_key.clear();
    assert_eq!(
        validate_setup_state(&state),
        Some("enter an API key to save in the protected credential store".to_owned())
    );
    assert_eq!(
        build_setup_root_config(&state)
            .expect_err("missing auth should fail")
            .to_string(),
        format!("provide api_key or export {DEFAULT_SETUP_API_KEY_ENV}")
    );

    state.provider_name = "unsupported".to_owned();
    state.model = "test-model".to_owned();
    state.credential_source = SetupCredentialSource::SecureStore;
    state.api_key = SecretString::new("test-key");
    let unsupported_error =
        validate_setup_state(&state).expect("unsupported provider should fail validation");
    assert!(
        unsupported_error.contains("unsupported setup provider"),
        "unexpected validation error: {unsupported_error}"
    );
}

#[test]
fn setup_manual_model_is_validated_locally_without_catalog_admission() {
    let mut state = SetupState::new(Path::new("sigil.toml").to_path_buf(), None);
    state.credential_source = SetupCredentialSource::SecureStore;
    state.api_key = SecretString::new("test-key");
    state.model = "remote-manual-model".to_owned();
    assert_eq!(validate_setup_state(&state), None);

    state.model = "different-unverified-model".to_owned();
    assert_eq!(validate_setup_state(&state), None);

    state.model = "auth-rejected-model".to_owned();
    assert_eq!(validate_setup_state(&state), None);
}

#[test]
fn setup_builder_persists_the_selected_provider() -> Result<()> {
    let mut state = SetupState::new(Path::new("sigil.toml").to_path_buf(), None);
    state.provider_name = "anthropic".to_owned();
    state.model = "claude-sonnet-4-5".to_owned();
    state.context_window_tokens = "200000".to_owned();
    state.credential_source = SetupCredentialSource::SecureStore;
    state.api_key = SecretString::new("anthropic-test-key");

    let root_config = build_setup_root_config(&state)?;

    assert_eq!(root_config.config_version, sigil_kernel::CONFIG_VERSION_V2);
    assert!(root_config.agent.runtime_provider.is_empty());
    assert_eq!(
        root_config.agent.connection.as_ref().map(|id| id.as_str()),
        Some("anthropic-default")
    );
    assert_eq!(root_config.agent.model, "claude-sonnet-4-5");
    assert!(root_config.connections.contains_key("anthropic-default"));
    assert_eq!(
        sigil_runtime::provider_connections::load_provider_connections(&root_config).connections
            [&sigil_kernel::ConnectionId::new("anthropic-default")?]
            .config
            .model_context_windows
            .get("claude-sonnet-4-5"),
        Some(&200_000)
    );
    assert!(!toml::to_string(&root_config)?.contains("anthropic-test-key"));
    Ok(())
}

#[test]
fn setup_builder_rejects_an_invalid_optional_context_window() {
    let mut state = SetupState::new(Path::new("sigil.toml").to_path_buf(), None);
    state.credential_source = SetupCredentialSource::SecureStore;
    state.api_key = SecretString::new("test-key");
    state.context_window_tokens = "0".to_owned();

    let error = build_setup_root_config(&state).expect_err("zero must not be saved");

    assert!(error.to_string().contains("greater than 0"));
}

#[test]
fn setup_builder_rejects_output_budget_that_consumes_the_context_window() {
    let _env_guard = crate::test_env::lock();
    let _api_key = crate::test_env::EnvScope::set("SIGIL_API_KEY", "test-key");
    let temp = tempdir().expect("tempdir");
    let mut app = AppState::from_setup(
        temp.path().join("config").join("sigil.toml"),
        temp.path().to_path_buf(),
        None,
    );
    let state = app.setup_state.as_mut().expect("setup state");
    state.credential_source = SetupCredentialSource::Environment;
    state.context_window_tokens = "256K".to_owned();
    state.max_output_tokens = "256K".to_owned();

    let error = validate_setup_state(state).expect("budget must be rejected");
    assert!(error.contains("leave insufficient input budget"));
}

#[test]
fn setup_builder_rejects_output_budget_above_the_known_provider_limit() {
    let _env_guard = crate::test_env::lock();
    let _api_key = crate::test_env::EnvScope::set("GEMINI_API_KEY", "test-key");
    let mut state = SetupState::new(Path::new("sigil.toml").to_path_buf(), None);
    state.provider_name = "gemini".to_owned();
    state.model = "gemini-2.5-pro".to_owned();
    state.context_window_tokens = "1M".to_owned();
    state.max_output_tokens = "256K".to_owned();
    state.credential_source = SetupCredentialSource::Environment;

    let error = build_setup_root_config(&state)
        .expect_err("the setup flow must reject a provider-impossible output cap");

    assert!(error.to_string().contains("provider limit"));
}

#[test]
fn setup_screen_switches_provider_and_opens_inline_field_modals() -> Result<()> {
    let _env_guard = crate::test_env::lock();
    let _api_key = crate::test_env::EnvScope::unset("SIGIL_API_KEY");
    let temp = tempdir()?;
    let config_path = temp.path().join("config").join("sigil.toml");
    let workspace_root = temp.path().join("workspace");
    let mut app = AppState::from_setup(
        config_path,
        workspace_root,
        Some("invalid existing config".to_owned()),
    );

    let setup_lines = app.setup_lines().join("\n");
    assert!(setup_lines.contains("Set up a model connection"));
    assert!(setup_lines.contains("> DeepSeek"));
    assert!(setup_lines.contains("SIGIL_API_KEY not set"));
    assert!(setup_lines.contains("startup recovery could not capture the config source"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert_eq!(app.last_notice(), Some("provider -> OpenAI"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert_eq!(app.last_notice(), Some("provider selected: OpenAI"));
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))?;
    assert_eq!(app.last_notice(), Some("setup field model"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE))?;
    assert_eq!(app.modal_title(), Some("Model ID"));
    assert_eq!(app.modal_input_cursor(), Some(("model".to_owned(), 1, 3)));
    assert!(app.modal_lines().join("\n").contains("model: p|"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;
    assert_eq!(app.last_notice(), Some("closed text input"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    assert_eq!(app.last_notice(), Some("setup field authentication"));
    app.setup_state
        .as_mut()
        .expect("setup state")
        .credential_source = SetupCredentialSource::SecureStore;

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE))?;
    assert_eq!(app.modal_title(), Some("API Key"));
    assert_eq!(app.modal_input_cursor(), Some(("api_key".to_owned(), 1, 4)));
    assert!(app.modal_lines().join("\n").contains("api_key: *|"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert_eq!(app.last_notice(), Some("updated api key"));
    assert_eq!(
        app.setup_state
            .as_ref()
            .map(|state| state.api_key.expose_secret()),
        Some("s")
    );
    Ok(())
}
