use super::*;

#[test]
fn config_command_opens_first_editable_step() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "/config".to_owned();

    let action = app.submit_input()?;

    assert!(action.is_none());
    assert!(app.is_config_mode());
    assert_eq!(app.config_section_title(), Some("Provider"));
    assert_eq!(app.config_selected_field_label(), Some("Model"));
    assert_eq!(app.config_status_summary(), "Provider · saved · sigil.toml");
    let nav = app.config_nav_lines().join("\n");
    assert!(nav.contains("Tab section"));
    assert!(!nav.contains("Tab/Left/Right section"));
    Ok(())
}

#[test]
fn config_up_down_moves_between_fields_in_current_step() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_config_panel();

    assert_eq!(app.config_section_title(), Some("Provider"));
    assert_eq!(app.config_selected_field_label(), Some("Model"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert_eq!(app.config_selected_field_label(), Some("API key"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    assert_eq!(app.config_selected_field_label(), Some("Model"));
    Ok(())
}

#[test]
fn config_down_to_footer_focuses_actions() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
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
    assert_eq!(app.config_selected_field_label(), Some("FIM model"));
    Ok(())
}

#[test]
fn config_empty_mcp_footer_can_leave_bottom_focus() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .set_section(ConfigSection::Mcp);
    assert_eq!(app.config_selected_field_label(), None);

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert_eq!(app.config_selected_field_label(), Some("save"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE))?;
    assert_eq!(app.config_section_title(), Some("MCP"));
    assert_eq!(app.config_selected_field_label(), None);
    let state = app
        .config_state
        .as_ref()
        .expect("config state should still exist");
    assert!(!state.footer_selected);
    assert_eq!(state.selected_field, None);

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE))?;
    assert_eq!(app.config_section_title(), Some("Compaction"));
    assert_eq!(app.config_selected_field_label(), Some("Auto compact"));

    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .set_section(ConfigSection::Mcp);
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    assert_eq!(app.config_selected_field_label(), Some("save"));
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE))?;
    assert_eq!(app.config_section_title(), Some("Compaction"));
    assert_eq!(app.config_selected_field_label(), Some("Auto compact"));

    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .set_section(ConfigSection::Mcp);
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))?;
    assert_eq!(app.config_selected_field_label(), Some("save_and_close"));
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))?;
    assert_eq!(app.config_selected_field_label(), Some("activate_mcp"));
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))?;
    assert_eq!(app.config_selected_field_label(), Some("close"));
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))?;
    assert_eq!(app.config_section_title(), Some("Provider"));
    assert_eq!(app.config_selected_field_label(), Some("Model"));
    Ok(())
}

#[test]
fn config_mcp_footer_activate_returns_lazy_activation_action() -> Result<()> {
    let mut config = test_config();
    config.mcp_servers.push(sigil_kernel::McpServerConfig {
        name: "filesystem".to_owned(),
        command: "mcp-filesystem".to_owned(),
        required: false,
        startup: McpServerStartup::Lazy,
        ..Default::default()
    });
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .set_section(ConfigSection::Mcp);
    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .selected_field = Some(ConfigField::McpStartupTimeoutSecs);

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))?;
    assert_eq!(app.config_selected_field_label(), Some("activate_mcp"));
    let detail = app.config_detail_lines().join("\n");
    assert!(detail.contains("Runtime"));
    assert!(detail.contains("deferred"));

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    assert!(matches!(
        action,
        Some(AppAction::ActivateLazyMcp {
            server_name: Some(ref server_name)
        }) if server_name == "filesystem"
    ));
    assert_eq!(app.last_notice(), Some("activating MCP filesystem"));
    assert_eq!(
        app.mcp_server_runtime_status_label("filesystem").as_deref(),
        Some("activating")
    );
    let detail = app.config_detail_lines().join("\n");
    assert!(detail.contains("activating"));
    Ok(())
}

#[test]
fn config_mcp_lifecycle_updates_from_worker_activation_status() -> Result<()> {
    let mut config = test_config();
    config.mcp_servers.push(sigil_kernel::McpServerConfig {
        name: "filesystem".to_owned(),
        command: "mcp-filesystem".to_owned(),
        required: false,
        startup: McpServerStartup::Lazy,
        ..Default::default()
    });
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .set_section(ConfigSection::Mcp);

    app.handle_worker_message(WorkerMessage::McpActivationStatus {
        server_name: Some("filesystem".to_owned()),
        status: McpActivationStatus::Ready { added_tools: 3 },
    })?;

    assert_eq!(
        app.mcp_server_runtime_status_label("filesystem").as_deref(),
        Some("ready 3 tools")
    );
    let detail = app.config_detail_lines().join("\n");
    assert!(detail.contains("ready 3 tools"));

    app.handle_worker_message(WorkerMessage::McpActivationStatus {
        server_name: Some("filesystem".to_owned()),
        status: McpActivationStatus::Failed {
            error: "MCP server filesystem tools/list failed: bad response".to_owned(),
        },
    })?;

    let label = app
        .mcp_server_runtime_status_label("filesystem")
        .expect("status should exist");
    assert!(label.contains("failed:"));
    assert!(label.contains("bad response"));
    let detail = app.config_detail_lines().join("\n");
    assert!(detail.contains("failed:"));
    Ok(())
}

#[test]
fn config_mcp_footer_activate_requires_saved_lazy_server() -> Result<()> {
    let mut config = test_config();
    config.mcp_servers.push(sigil_kernel::McpServerConfig {
        name: "eager".to_owned(),
        command: "mcp-eager".to_owned(),
        startup: McpServerStartup::Eager,
        ..Default::default()
    });
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    app.open_config_panel();
    let state = app
        .config_state
        .as_mut()
        .expect("config state should still exist");
    state.set_section(ConfigSection::Mcp);
    state.focus_footer(ConfigFooterAction::ActivateMcp);

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert_eq!(app.last_notice(), Some("MCP server eager is eager"));

    let state = app
        .config_state
        .as_mut()
        .expect("config state should still exist");
    state.dirty = true;
    state.focus_footer(ConfigFooterAction::ActivateMcp);

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert_eq!(app.last_notice(), Some("save config before activating MCP"));
    Ok(())
}

#[test]
fn config_left_right_switches_steps() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_config_panel();

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))?;
    assert_eq!(app.config_section_title(), Some("Permissions"));
    assert_eq!(app.config_selected_field_label(), Some("Default mode"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE))?;
    assert_eq!(app.config_section_title(), Some("Provider"));
    assert_eq!(app.config_selected_field_label(), Some("Model"));
    Ok(())
}

#[test]
fn config_enter_starts_and_commits_text_edit() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
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
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_config_panel();

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE))?;
    assert!(app.has_modal());
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE))?;
    let detail = app.modal_lines().join("\n");
    assert!(detail.contains("key: model"));
    assert!(detail.contains("value: gp|"));
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
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_config_panel();

    let lines = app.config_detail_lines();
    let detail = lines.join("\n");

    assert_eq!(lines[0], "Provider 1/5 · provider settings");
    assert_eq!(lines[1], "[provider] permissions memory compaction mcp");
    assert_eq!(lines[2], "");
    assert!(detail.contains("[model]"));
    assert!(detail.contains("[authentication]"));
    assert!(detail.contains("[endpoint]"));
    assert!(detail.contains("[advanced]"));
    assert!(detail.contains("[details]"));
    assert!(detail.contains("selected: Model"));
    assert!(detail.contains("key: model"));
    assert!(detail.contains("controls: Tab section · Up/Down field · Enter edit"));
    assert!(detail.contains("actions: Down to actions · Ctrl-S save · Esc close"));
    assert!(!lines.iter().take(3).any(|line| line.contains("Tab")));
    assert!(!detail.contains("beta_base_url"));
    assert!(!detail.contains("user_id_strategy"));
    assert!(!detail.contains("anthropic_base_url"));
    assert!(!detail.contains("strict_tools_mode"));
    assert!(!detail.contains("request_timeout_secs"));
}

#[test]
fn config_permissions_step_uses_policy_summary_and_details() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .set_section(ConfigSection::Permissions);

    let detail = app.config_detail_lines().join("\n");

    assert!(detail.contains("[policy]"));
    assert!(detail.contains("Default mode"));
    assert!(detail.contains("[rules]"));
    assert!(detail.contains("- Rule overrides"));
    assert!(detail.contains("i All unmatched tools use the default mode above"));
    assert!(detail.contains("[details]"));
    assert!(detail.contains("selected: Default mode"));
    assert!(detail.contains("key: default_mode"));
    assert!(detail.contains("controls: Tab section"));
    assert!(!detail.lines().any(|line| line.starts_with("overrides:")));
    assert!(!detail.contains("subject="));
}

#[test]
fn config_memory_step_uses_loaded_context_summary() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .set_section(ConfigSection::Memory);

    let detail = app.config_detail_lines().join("\n");

    assert!(detail.contains("[workspace memory]"));
    assert!(detail.contains("Memory"));
    assert!(detail.contains("[loaded context]"));
    assert!(detail.contains("- Documents"));
    assert!(detail.contains("- Last scan"));
    assert!(detail.contains("- Root files"));
    assert!(detail.contains("[details]"));
    assert!(detail.contains("selected: Memory"));
    assert!(!detail.contains("docs:"));
    assert!(!detail.contains("root docs:"));
}

#[test]
fn config_mcp_step_uses_server_summary_when_empty() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .set_section(ConfigSection::Mcp);

    let detail = app.config_detail_lines().join("\n");

    assert!(detail.contains("[servers]"));
    assert!(detail.contains("- Configured"));
    assert!(detail.contains("0 servers"));
    assert!(detail.contains("i No MCP servers configured"));
    assert!(detail.contains("i Ctrl-N adds a required eager self-hosted server"));
    assert!(detail.contains("mcp: Ctrl-N add · Ctrl-D drop · PgUp/PgDn server"));
    assert!(!detail.contains("servers:"));
    assert!(!detail.contains("args_csv:"));
}

#[test]
fn config_compaction_step_shows_effective_context_window_source() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .set_section(ConfigSection::Compaction);

    let detail = app.config_detail_lines().join("\n");

    assert!(detail.contains("[context]"));
    assert!(detail.contains("- Effective window"));
    assert!(detail.contains("1,000,000 tokens  source=provider"));
    assert!(detail.contains("Fallback window"));
    assert!(detail.contains("[details]"));
    assert!(detail.contains("selected: Auto compact"));
    assert!(!detail.contains("context_window_tokens"));
}

#[test]
fn config_compaction_step_uses_fallback_for_unknown_provider_window() {
    let mut config = test_config();
    config.agent.provider = "custom".to_owned();
    config.agent.model = "custom-model".to_owned();
    config.compaction.context_window_tokens = Some(128_000);
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .set_section(ConfigSection::Compaction);

    let detail = app.config_detail_lines().join("\n");

    assert!(detail.contains("128,000 tokens  source=fallback"));
}

#[test]
fn config_mode_closes_on_escape() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_config_panel();

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert!(!app.is_config_mode());
    Ok(())
}

#[test]
fn config_save_persists_draft_and_returns_reload_action() -> Result<()> {
    let temp = tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    test_config().save(&config_path)?;

    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
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
    state.draft.compaction_context_window_tokens = "64000".to_owned();
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
    assert_eq!(root_config.compaction.context_window_tokens, Some(64_000));
    assert!(!app.config_is_dirty());
    assert_eq!(app.permission_default_mode, "allow");
    assert!(!app.memory_enabled);

    let saved = RootConfig::load(&config_path)?;
    assert_eq!(saved.agent.model, "deepseek-v4-pro");
    assert_eq!(saved.permission.default_mode, ApprovalMode::Allow);
    assert!(!saved.memory.enabled);
    assert_eq!(saved.compaction.soft_threshold_ratio, 0.40);
    assert_eq!(saved.compaction.hard_threshold_ratio, 0.75);
    assert_eq!(saved.compaction.context_window_tokens, Some(64_000));
    let saved_raw = std::fs::read_to_string(&config_path)?;
    assert!(saved_raw.contains("fallback_context_window_tokens = 64000"));
    assert!(
        !saved_raw
            .lines()
            .any(|line| line.trim_start().starts_with("context_window_tokens ="))
    );
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
    let config_path = temp.path().join("sigil.toml");
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
    let config_path = temp.path().join("sigil.toml");
    test_config().save(&config_path)?;

    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
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

    let detail = app.config_detail_lines().join("\n");
    assert!(detail.contains("[server]"));
    assert!(detail.contains("Name"));
    assert!(detail.contains("Command"));
    assert!(detail.contains("Arguments"));
    assert!(detail.contains("[lifecycle]"));
    assert!(detail.contains("Required"));
    assert!(detail.contains("Startup"));
    assert!(detail.contains("Trust"));
    assert!(detail.contains("Secrets"));
    assert!(!detail.contains("args_csv:"));

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
    let config_path = temp.path().join("sigil.toml");
    test_config().save(&config_path)?;

    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
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
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_config_panel();
    let state = app
        .config_state
        .as_mut()
        .expect("config state should exist after opening /config");
    state.draft.provider_model = "deepseek-v4-pro".to_owned();
    state.dirty = true;
    assert_eq!(
        app.config_footer_hint(),
        "status: unsaved - save before close"
    );

    let first = app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;
    assert!(first.is_none());
    assert!(app.is_config_mode());
    assert_eq!(app.config_selected_field_label(), Some("save"));
    assert!(app.config_close_guard_armed());
    assert_eq!(
        app.config_footer_hint(),
        "status: confirm close - Esc discards"
    );
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
    let config_path = temp.path().join("sigil.toml");
    test_config().save(&config_path)?;

    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
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
    let config_path = temp.path().join("sigil.toml");
    test_config().save(&config_path)?;

    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
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
    let config_path = temp.path().join("sigil.toml");
    test_config().save(&config_path)?;

    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
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
    let config_path = temp.path().join("sigil.toml");
    test_config().save(&config_path)?;

    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
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
    let config_path = temp.path().join("config").join("sigil.toml");
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
    assert_eq!(saved.compaction.context_window_tokens, None);
    let saved_raw = std::fs::read_to_string(&saved_path)?;
    assert!(!saved_raw.contains("fallback_context_window_tokens"));
    assert!(
        !saved_raw
            .lines()
            .any(|line| line.trim_start().starts_with("context_window_tokens ="))
    );
    Ok(())
}

#[test]
fn setup_save_requires_credentials() -> Result<()> {
    let temp = tempdir()?;
    let config_path = temp.path().join("config").join("sigil.toml");
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
        Some("provide api_key or export SIGIL_API_KEY")
    );
    Ok(())
}
