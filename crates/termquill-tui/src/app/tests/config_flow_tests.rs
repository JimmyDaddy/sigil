use super::*;

#[test]
fn config_command_opens_first_editable_step() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.input = "/config".to_owned();

    let action = app.submit_input()?;

    assert!(action.is_none());
    assert!(app.is_config_mode());
    assert_eq!(app.config_section_title(), Some("Provider"));
    assert_eq!(app.config_selected_field_label(), Some("model"));
    Ok(())
}

#[test]
fn config_up_down_moves_between_fields_in_current_step() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.open_config_panel();

    assert_eq!(app.config_section_title(), Some("Provider"));
    assert_eq!(app.config_selected_field_label(), Some("model"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert_eq!(app.config_selected_field_label(), Some("api_key"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    assert_eq!(app.config_selected_field_label(), Some("model"));
    Ok(())
}

#[test]
fn config_down_to_footer_focuses_actions() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .selected_field = Some(ConfigField::ProviderFimModel);

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert_eq!(app.config_selected_field_label(), Some("save"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))?;
    assert_eq!(app.config_selected_field_label(), Some("save_and_close"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))?;
    assert_eq!(app.config_selected_field_label(), Some("close"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    assert_eq!(app.config_selected_field_label(), Some("fim_model"));
    Ok(())
}

#[test]
fn config_left_right_switches_steps() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.open_config_panel();

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))?;
    assert_eq!(app.config_section_title(), Some("Permissions"));
    assert_eq!(app.config_selected_field_label(), Some("default_mode"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE))?;
    assert_eq!(app.config_section_title(), Some("Provider"));
    assert_eq!(app.config_selected_field_label(), Some("model"));
    Ok(())
}

#[test]
fn config_enter_starts_and_commits_text_edit() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.open_config_panel();

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(app.has_modal());
    assert_eq!(app.modal_title(), Some("Model"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    let state = app
        .config_state
        .as_ref()
        .expect("config state should still exist");
    assert_eq!(state.draft.provider_model, "deepseek-v4-pro");
    assert!(state.dirty);
    Ok(())
}

#[test]
fn config_direct_typing_replaces_selected_text_value() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.open_config_panel();

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE))?;
    assert!(app.has_modal());
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE))?;
    let detail = app.modal_lines().join("\n");
    assert!(detail.contains("model: gp|"));
    assert!(!detail.contains("deepseek-v4-flashg"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    let state = app
        .config_state
        .as_ref()
        .expect("config state should still exist");
    assert_eq!(state.draft.provider_model, "gp");
    Ok(())
}

#[test]
fn config_provider_flow_hides_advanced_provider_fields() {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.open_config_panel();

    let detail = app.config_detail_lines().join("\n");

    assert!(!detail.contains("beta_base_url"));
    assert!(!detail.contains("user_id_strategy"));
    assert!(!detail.contains("anthropic_base_url"));
    assert!(!detail.contains("strict_tools_mode"));
    assert!(!detail.contains("request_timeout_secs"));
}

#[test]
fn config_mode_closes_on_escape() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.open_config_panel();

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert!(!app.is_config_mode());
    Ok(())
}

#[test]
fn config_save_persists_draft_and_returns_reload_action() -> Result<()> {
    let temp = tempdir()?;
    let config_path = temp.path().join("termquill.toml");
    test_config().save(&config_path)?;

    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.config_path = config_path.clone();
    app.open_config_panel();
    let state = app
        .config_state
        .as_mut()
        .expect("config state should exist after opening /config");
    state.set_section(ConfigSection::Provider);
    state.selected_field = Some(ConfigField::ProviderModel);
    state.draft.provider_model = "deepseek-v4-pro".to_owned();
    state.draft.provider_base_url = "https://example.invalid/api".to_owned();
    state.draft.provider_user_id_strategy = "stable_per_workspace".to_owned();
    state.draft.provider_fim_model = "deepseek-v4-flash".to_owned();
    state.draft.permission_default_mode = ApprovalMode::Allow;
    state.draft.memory_enabled = false;
    state.draft.compaction_soft_threshold_ratio = "0.40".to_owned();
    state.draft.compaction_hard_threshold_ratio = "0.75".to_owned();
    state.dirty = true;

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))?;

    let Some(AppAction::ConfigSaved { root_config }) = action else {
        panic!("expected config save action");
    };
    assert_eq!(root_config.agent.model, "deepseek-v4-pro");
    assert_eq!(root_config.permission.default_mode, ApprovalMode::Allow);
    assert!(!root_config.memory.enabled);
    assert_eq!(root_config.compaction.soft_threshold_ratio, 0.40);
    assert_eq!(root_config.compaction.hard_threshold_ratio, 0.75);
    assert!(!app.config_is_dirty());
    assert_eq!(app.permission_default_mode, "allow");
    assert!(!app.memory_enabled);

    let saved = RootConfig::load(&config_path)?;
    assert_eq!(saved.agent.model, "deepseek-v4-pro");
    assert_eq!(saved.permission.default_mode, ApprovalMode::Allow);
    assert!(!saved.memory.enabled);
    assert_eq!(saved.compaction.soft_threshold_ratio, 0.40);
    assert_eq!(saved.compaction.hard_threshold_ratio, 0.75);
    assert_eq!(
        saved
            .providers
            .get("deepseek")
            .and_then(|value| value.get("model"))
            .and_then(|value| value.as_str()),
        Some("deepseek-v4-pro")
    );
    assert_eq!(
        saved
            .providers
            .get("deepseek")
            .and_then(|value| value.get("base_url"))
            .and_then(|value| value.as_str()),
        Some("https://example.invalid/api")
    );
    assert_eq!(
        saved
            .providers
            .get("deepseek")
            .and_then(|value| value.get("user_id_strategy"))
            .and_then(|value| value.as_str()),
        Some("stable_per_workspace")
    );
    assert_eq!(
        saved
            .providers
            .get("deepseek")
            .and_then(|value| value.get("fim_model"))
            .and_then(|value| value.as_str()),
        Some("deepseek-v4-flash")
    );
    Ok(())
}

#[test]
fn runtime_permission_toggle_persists_default_mode_to_config() -> Result<()> {
    let temp = tempdir()?;
    let config_path = temp.path().join("termquill.toml");
    let root_config = test_config();
    root_config.save(&config_path)?;

    let mut app = AppState::from_root_config(&config_path, &root_config);

    let action = app.handle_key_event(KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE))?;

    let Some(AppAction::RuntimeConfigUpdated { root_config }) = action else {
        panic!("expected runtime config update action");
    };
    assert_eq!(root_config.permission.default_mode, ApprovalMode::Deny);
    assert_eq!(app.permission_default_mode, "deny");

    let saved = RootConfig::load(&config_path)?;
    assert_eq!(saved.permission.default_mode, ApprovalMode::Deny);
    let reopened = AppState::from_root_config(&config_path, &saved);
    assert_eq!(reopened.permission_default_mode, "deny");
    Ok(())
}

#[test]
fn config_can_add_and_persist_mcp_server() -> Result<()> {
    let temp = tempdir()?;
    let config_path = temp.path().join("termquill.toml");
    test_config().save(&config_path)?;

    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.config_path = config_path.clone();
    app.open_config_panel();
    {
        let state = app
            .config_state
            .as_mut()
            .expect("config state should exist after opening /config");
        state.set_section(ConfigSection::Mcp);
    }

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL))?;
    assert!(action.is_none());
    let state = app
        .config_state
        .as_mut()
        .expect("config state should still exist");
    assert_eq!(state.draft.mcp_servers.len(), 1);
    state.draft.mcp_servers[0].name = "filesystem".to_owned();
    state.draft.mcp_servers[0].command = "npx".to_owned();
    state.draft.mcp_servers[0].args_csv =
        "-y, @modelcontextprotocol/server-filesystem, .".to_owned();
    state.draft.mcp_servers[0].startup_timeout_secs = "15".to_owned();

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))?;

    let Some(AppAction::ConfigSaved { root_config }) = action else {
        panic!("expected config save action");
    };
    assert_eq!(root_config.mcp_servers.len(), 1);
    assert_eq!(root_config.mcp_servers[0].name, "filesystem");
    assert_eq!(root_config.mcp_servers[0].command, "npx");
    assert_eq!(
        root_config.mcp_servers[0].args,
        vec![
            "-y".to_owned(),
            "@modelcontextprotocol/server-filesystem".to_owned(),
            ".".to_owned()
        ]
    );
    assert_eq!(root_config.mcp_servers[0].startup_timeout_secs, 15);
    assert!(root_config.mcp_servers[0].required);
    assert_eq!(root_config.mcp_servers[0].startup, McpServerStartup::Eager);
    assert_eq!(
        root_config.mcp_servers[0].trust.trust_class,
        McpTrustClass::SelfHosted
    );
    assert_eq!(
        root_config.mcp_servers[0].trust.approval_default,
        ApprovalMode::Ask
    );

    let saved = RootConfig::load(&config_path)?;
    assert_eq!(saved.mcp_servers.len(), 1);
    assert_eq!(saved.mcp_servers[0].name, "filesystem");
    assert_eq!(saved.mcp_servers[0].command, "npx");
    assert_eq!(
        saved.mcp_servers[0].args,
        vec![
            "-y".to_owned(),
            "@modelcontextprotocol/server-filesystem".to_owned(),
            ".".to_owned()
        ]
    );
    assert_eq!(saved.mcp_servers[0].startup_timeout_secs, 15);
    assert!(saved.mcp_servers[0].required);
    assert_eq!(saved.mcp_servers[0].startup, McpServerStartup::Eager);
    assert_eq!(
        saved.mcp_servers[0].trust.trust_class,
        McpTrustClass::SelfHosted
    );
    assert_eq!(
        saved.mcp_servers[0].trust.approval_default,
        ApprovalMode::Ask
    );
    Ok(())
}

#[test]
fn config_save_is_blocked_while_busy() -> Result<()> {
    let temp = tempdir()?;
    let config_path = temp.path().join("termquill.toml");
    test_config().save(&config_path)?;

    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.config_path = config_path.clone();
    app.open_config_panel();
    let state = app
        .config_state
        .as_mut()
        .expect("config state should exist after opening /config");
    state.set_section(ConfigSection::Provider);
    state.selected_field = Some(ConfigField::ProviderModel);
    state.draft.provider_model = "deepseek-v4-pro".to_owned();
    state.dirty = true;
    app.is_busy = true;

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))?;

    assert!(action.is_none());
    assert_eq!(app.last_notice(), Some("busy; save later"));
    let saved = RootConfig::load(&config_path)?;
    assert_eq!(saved.agent.model, "deepseek-v4-flash");
    Ok(())
}

#[test]
fn config_close_requires_second_escape_when_dirty() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.open_config_panel();
    let state = app
        .config_state
        .as_mut()
        .expect("config state should exist after opening /config");
    state.draft.provider_model = "deepseek-v4-pro".to_owned();
    state.dirty = true;

    let first = app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;
    assert!(first.is_none());
    assert!(app.is_config_mode());
    assert_eq!(app.config_selected_field_label(), Some("save"));
    assert_eq!(
        app.last_notice(),
        Some("unsaved changes; Down footer to save, Esc discard")
    );

    let second = app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;
    assert!(second.is_none());
    assert!(!app.is_config_mode());
    assert_eq!(app.last_notice(), Some("closed config; discarded changes"));
    Ok(())
}

#[test]
fn config_f2_saves_and_keeps_config_open() -> Result<()> {
    let temp = tempdir()?;
    let config_path = temp.path().join("termquill.toml");
    test_config().save(&config_path)?;

    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.config_path = config_path.clone();
    app.open_config_panel();
    let state = app
        .config_state
        .as_mut()
        .expect("config state should exist after opening /config");
    state.draft.provider_api_key = "saved-from-f2".to_owned();
    state.dirty = true;

    let action = app.handle_key_event(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE))?;

    let Some(AppAction::ConfigSaved { root_config }) = action else {
        panic!("expected config save action");
    };
    assert!(app.is_config_mode());
    assert_eq!(app.last_notice(), Some("saved config"));
    assert_eq!(
        root_config
            .providers
            .get("deepseek")
            .and_then(|value| value.get("api_key"))
            .and_then(|value| value.as_str()),
        Some("saved-from-f2")
    );

    let saved = RootConfig::load(&config_path)?;
    assert_eq!(
        saved
            .providers
            .get("deepseek")
            .and_then(|value| value.get("api_key"))
            .and_then(|value| value.as_str()),
        Some("saved-from-f2")
    );
    Ok(())
}

#[test]
fn config_footer_enter_saves_without_function_keys() -> Result<()> {
    let temp = tempdir()?;
    let config_path = temp.path().join("termquill.toml");
    test_config().save(&config_path)?;

    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.config_path = config_path.clone();
    app.open_config_panel();
    let state = app
        .config_state
        .as_mut()
        .expect("config state should exist after opening /config");
    state.selected_field = Some(ConfigField::ProviderFimModel);
    state.draft.provider_api_key = "saved-from-footer".to_owned();
    state.dirty = true;

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    let Some(AppAction::ConfigSaved { root_config }) = action else {
        panic!("expected config save action");
    };
    assert!(app.is_config_mode());
    assert_eq!(app.last_notice(), Some("saved config"));
    assert_eq!(
        root_config
            .providers
            .get("deepseek")
            .and_then(|value| value.get("api_key"))
            .and_then(|value| value.as_str()),
        Some("saved-from-footer")
    );

    let saved = RootConfig::load(&config_path)?;
    assert_eq!(
        saved
            .providers
            .get("deepseek")
            .and_then(|value| value.get("api_key"))
            .and_then(|value| value.as_str()),
        Some("saved-from-footer")
    );
    Ok(())
}

#[test]
fn config_f3_saves_and_closes_without_switching_step() -> Result<()> {
    let temp = tempdir()?;
    let config_path = temp.path().join("termquill.toml");
    test_config().save(&config_path)?;

    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.config_path = config_path.clone();
    app.open_config_panel();
    let state = app
        .config_state
        .as_mut()
        .expect("config state should exist after opening /config");
    state.draft.provider_api_key = "saved-from-f3".to_owned();
    state.dirty = true;
    state.set_section(ConfigSection::Provider);
    state.selected_field = Some(ConfigField::ProviderApiKey);

    let action = app.handle_key_event(KeyEvent::new(KeyCode::F(3), KeyModifiers::NONE))?;

    let Some(AppAction::ConfigSaved { root_config }) = action else {
        panic!("expected config save action");
    };
    assert!(!app.is_config_mode());
    assert_eq!(app.last_notice(), Some("saved config and closed"));
    assert_eq!(
        root_config
            .providers
            .get("deepseek")
            .and_then(|value| value.get("api_key"))
            .and_then(|value| value.as_str()),
        Some("saved-from-f3")
    );
    Ok(())
}

#[test]
fn config_footer_save_and_close_works_without_function_keys() -> Result<()> {
    let temp = tempdir()?;
    let config_path = temp.path().join("termquill.toml");
    test_config().save(&config_path)?;

    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());
    app.config_path = config_path.clone();
    app.open_config_panel();
    let state = app
        .config_state
        .as_mut()
        .expect("config state should exist after opening /config");
    state.selected_field = Some(ConfigField::ProviderFimModel);
    state.draft.provider_api_key = "saved-from-footer-close".to_owned();
    state.dirty = true;

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))?;
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    let Some(AppAction::ConfigSaved { root_config }) = action else {
        panic!("expected config save action");
    };
    assert!(!app.is_config_mode());
    assert_eq!(app.last_notice(), Some("saved config and closed"));
    assert_eq!(
        root_config
            .providers
            .get("deepseek")
            .and_then(|value| value.get("api_key"))
            .and_then(|value| value.as_str()),
        Some("saved-from-footer-close")
    );

    let saved = RootConfig::load(&config_path)?;
    assert_eq!(
        saved
            .providers
            .get("deepseek")
            .and_then(|value| value.get("api_key"))
            .and_then(|value| value.as_str()),
        Some("saved-from-footer-close")
    );
    Ok(())
}

#[test]
fn setup_mode_saves_config_and_returns_runtime_boot_action() -> Result<()> {
    let temp = tempdir()?;
    let config_path = temp.path().join("config").join("termquill.toml");
    let workspace_root = temp.path().join("workspace");
    let mut app = AppState::from_setup(config_path.clone(), workspace_root.clone(), None);

    assert!(app.is_setup_mode());

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(action.is_none());
    app.setup_state
        .as_mut()
        .expect("setup state should exist in setup mode")
        .selected_field = SetupField::Model;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(app.has_modal());
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    app.setup_state
        .as_mut()
        .expect("setup state should exist in setup mode")
        .selected_field = SetupField::ApiKey;
    for character in "test-inline-key".chars() {
        let _ =
            app.handle_key_event(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE))?;
    }
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    app.setup_state
        .as_mut()
        .expect("setup state should exist in setup mode")
        .selected_field = SetupField::Save;
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    let Some(AppAction::SetupCompleted {
        config_path: saved_path,
        root_config,
    }) = action
    else {
        panic!("expected setup completion action");
    };
    assert_eq!(saved_path, config_path);
    assert_eq!(root_config.workspace.root, ".");
    assert_eq!(root_config.agent.model, "deepseek-v4-pro");
    let saved = RootConfig::load(&saved_path)?;
    assert_eq!(saved.agent.provider, "deepseek");
    assert_eq!(saved.agent.model, "deepseek-v4-pro");
    assert_eq!(
        saved
            .providers
            .get("deepseek")
            .and_then(|value| value.get("api_key"))
            .and_then(|value| value.as_str()),
        Some("test-inline-key")
    );
    assert_eq!(
        root_config
            .providers
            .get("deepseek")
            .and_then(|value| value.get("api_key"))
            .and_then(|value| value.as_str()),
        Some("test-inline-key")
    );
    assert!(saved.memory.enabled);
    assert!(saved.compaction.enabled);
    Ok(())
}

#[test]
fn setup_save_requires_credentials() -> Result<()> {
    let temp = tempdir()?;
    let config_path = temp.path().join("config").join("termquill.toml");
    let workspace_root = temp.path().join("workspace");
    let mut app = AppState::from_setup(config_path, workspace_root, None);
    let state = app
        .setup_state
        .as_mut()
        .expect("setup state should exist in setup mode");
    state.selected_field = SetupField::Save;
    state.trusted_current_folder = true;
    state.api_key.clear();

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    assert!(action.is_none());
    let state = app
        .setup_state
        .as_ref()
        .expect("setup state should exist in setup mode");
    assert_eq!(state.selected_field, SetupField::Save);
    assert_eq!(
        app.last_notice(),
        Some("provide api_key or export TERMQUILL_API_KEY")
    );
    Ok(())
}
