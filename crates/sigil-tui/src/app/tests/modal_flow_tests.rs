use super::*;

#[test]
fn f1_opens_keyboard_help_modal_from_composer() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "draft".to_owned();
    app.push_timeline(
        TimelineRole::Tool,
        r##"{
  "tool_name": "ls",
  "status": "ok",
  "preview_kind": "json",
  "summary": "first 1/1 lines · 8 B",
  "preview_lines": ["[\".git\"]"],
  "preview_value": [".git"],
  "hidden_lines": 0
}"##,
    );

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE))?;

    assert_eq!(app.input, "draft");
    assert_eq!(app.modal_title(), Some("Keyboard Help"));
    let lines = app.modal_lines();
    assert!(lines.iter().any(|line| line.contains("F1:")));
    assert!(lines.iter().any(|line| line.contains("Ctrl-G:")));
    assert!(lines.iter().any(|line| line.starts_with("/model:")));
    assert!(!lines.iter().any(|line| line.starts_with("/tool:")));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    assert!(!app.has_modal());
    assert_eq!(app.last_notice(), Some("closed keyboard help"));
    Ok(())
}

#[test]
fn esc_closes_keyboard_help_before_interrupting_busy_run() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.input = "long task".to_owned();
    assert!(matches!(
        app.submit_input()?,
        Some(AppAction::SubmitPrompt(prompt)) if prompt == "long task"
    ));
    assert!(app.is_busy);

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE))?;
    assert!(app.has_modal());

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert!(!app.has_modal());
    assert!(app.is_busy);
    assert_eq!(app.last_notice(), Some("closed keyboard help"));
    assert!(
        !app.timeline
            .iter()
            .any(|entry| entry.text.contains("cancel requested"))
    );
    Ok(())
}

#[test]
fn ctrl_c_quits_while_keyboard_help_modal_is_open() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE))?;
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))?;

    assert!(action.is_none());
    assert!(!app.has_modal());
    assert!(app.should_quit);
    Ok(())
}

#[test]
fn model_picker_opens_with_local_options_before_remote_refresh() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.open_model_picker(ModelPickerTarget::Provider, "custom-model");

    assert!(matches!(app.modal_state, Some(ModalState::ModelPicker(_))));
    assert!(app.model_picker_refresh_rx.is_none());
    assert_eq!(app.last_notice(), Some("using local model list"));
    let lines = app.modal_lines().join("\n");
    assert!(lines.contains("deepseek-v4-flash"));
    assert!(lines.contains("custom-model"));
    Ok(())
}

#[test]
fn mcp_elicitation_modal_accepts_text_input() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let (response_tx, response_rx) = tokio::sync::oneshot::channel();

    app.handle_worker_message(WorkerMessage::McpElicitationRequest {
        request: McpElicitationRequest {
            server_name: "filesystem".to_owned(),
            message: "Need target path".to_owned(),
            requested_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "title": "Path",
                        "description": "Workspace-relative path"
                    }
                },
                "required": ["path"]
            }),
        },
        response_tx,
    })?;

    assert_eq!(app.modal_title(), Some("MCP Elicitation"));
    let lines = app.modal_lines().join("\n");
    assert!(lines.contains("Need target path"));
    assert!(lines.contains("server: filesystem"));
    assert!(lines.contains("Path *: |"));

    for character in "src/lib.rs".chars() {
        let action =
            app.handle_key_event(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE))?;
        assert!(action.is_none());
    }
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert!(!app.has_modal());
    assert_eq!(app.last_notice(), Some("submitted MCP input to filesystem"));
    let response = futures::executor::block_on(response_rx)?;
    assert_eq!(response.action, McpElicitationAction::Accept);
    assert_eq!(response.content, Some(json!({ "path": "src/lib.rs" })));
    Ok(())
}

#[test]
fn mcp_elicitation_modal_declines_with_ctrl_d() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let (response_tx, response_rx) = tokio::sync::oneshot::channel();

    app.handle_worker_message(WorkerMessage::McpElicitationRequest {
        request: McpElicitationRequest {
            server_name: "filesystem".to_owned(),
            message: "Need target path".to_owned(),
            requested_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "title": "Path" }
                }
            }),
        },
        response_tx,
    })?;

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL))?;

    assert!(action.is_none());
    assert!(!app.has_modal());
    assert_eq!(
        app.last_notice(),
        Some("declined MCP input request from filesystem")
    );
    let response = futures::executor::block_on(response_rx)?;
    assert_eq!(response.action, McpElicitationAction::Decline);
    assert_eq!(response.content, None);
    Ok(())
}

#[test]
fn mcp_elicitation_modal_cancels_on_escape() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let (response_tx, response_rx) = tokio::sync::oneshot::channel();

    app.handle_worker_message(WorkerMessage::McpElicitationRequest {
        request: McpElicitationRequest {
            server_name: "filesystem".to_owned(),
            message: "Need target path".to_owned(),
            requested_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "title": "Path" }
                }
            }),
        },
        response_tx,
    })?;

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert!(!app.has_modal());
    assert_eq!(
        app.last_notice(),
        Some("cancelled MCP input request from filesystem")
    );
    let response = futures::executor::block_on(response_rx)?;
    assert_eq!(response.action, McpElicitationAction::Cancel);
    assert_eq!(response.content, None);
    Ok(())
}

#[test]
fn model_picker_remote_refresh_updates_open_modal_options() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_model_picker(ModelPickerTarget::Provider, "custom-model");

    let changed = app.apply_model_picker_refresh(ModelPickerRefresh {
        target: ModelPickerTarget::Provider,
        current: "custom-model".to_owned(),
        base_url: "https://example.com".to_owned(),
        result: Ok(vec![
            "remote-model-a".to_owned(),
            "remote-model-b".to_owned(),
        ]),
    });

    assert!(changed);
    assert_eq!(
        app.last_notice(),
        Some("loaded provider model list (https://example.com)")
    );
    let lines = app.modal_lines().join("\n");
    assert!(lines.contains("remote-model-a"));
    assert!(lines.contains("remote-model-b"));
    assert!(lines.contains("custom-model"));
    Ok(())
}

#[test]
fn model_picker_refresh_mismatch_is_ignored() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_model_picker(ModelPickerTarget::Provider, "custom-model");
    let before = app.modal_lines().join("\n");

    let changed = app.apply_model_picker_refresh(ModelPickerRefresh {
        target: ModelPickerTarget::ProviderFim,
        current: "custom-model".to_owned(),
        base_url: "https://example.com".to_owned(),
        result: Ok(vec!["remote-model".to_owned()]),
    });

    assert!(!changed);
    assert_eq!(app.last_notice(), Some("using local model list"));
    assert_eq!(app.modal_lines().join("\n"), before);
    let lines = app.modal_lines().join("\n");
    assert!(lines.contains("custom-model"));
    assert!(!lines.contains("remote-model"));
    Ok(())
}

#[test]
fn model_picker_remote_refresh_error_keeps_local_options() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_model_picker(ModelPickerTarget::Provider, "custom-model");
    let before = app.modal_lines().join("\n");

    let changed = app.apply_model_picker_refresh(ModelPickerRefresh {
        target: ModelPickerTarget::Provider,
        current: "custom-model".to_owned(),
        base_url: "https://example.com".to_owned(),
        result: Err("network timeout".to_owned()),
    });

    assert!(changed);
    assert_eq!(
        app.last_notice(),
        Some("using local model list: network timeout")
    );
    assert_eq!(app.modal_lines().join("\n"), before);
    assert!(
        app.events
            .iter()
            .any(|event| event.label == "model_list" && event.detail.contains("network timeout"))
    );
    Ok(())
}

#[test]
fn config_numeric_text_modal_rejects_invalid_characters() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .set_section(ConfigSection::Compaction);
    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .selected_field = Some(ConfigField::CompactionContextWindowTokens);

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert_eq!(app.modal_title(), Some("Fallback window"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE))?;

    assert_eq!(app.last_notice(), Some("value does not accept 'x'"));
    let lines = app.modal_lines().join("\n");
    assert!(lines.contains("value: |"));
    Ok(())
}

#[test]
fn mcp_elicitation_required_field_blocks_submission() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let (response_tx, mut response_rx) = tokio::sync::oneshot::channel();

    app.handle_worker_message(WorkerMessage::McpElicitationRequest {
        request: McpElicitationRequest {
            server_name: "filesystem".to_owned(),
            message: "Need target path".to_owned(),
            requested_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "title": "Path" }
                },
                "required": ["path"]
            }),
        },
        response_tx,
    })?;

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert!(app.has_modal());
    assert_eq!(app.last_notice(), Some("Path is required"));
    assert!(response_rx.try_recv().is_err());
    Ok(())
}

#[test]
fn mcp_elicitation_validates_numeric_fields_before_submit() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let (response_tx, mut response_rx) = tokio::sync::oneshot::channel();

    app.handle_worker_message(WorkerMessage::McpElicitationRequest {
        request: McpElicitationRequest {
            server_name: "planner".to_owned(),
            message: "Set retries".to_owned(),
            requested_schema: json!({
                "type": "object",
                "properties": {
                    "retries": { "type": "integer", "title": "Retries" }
                },
                "required": ["retries"]
            }),
        },
        response_tx,
    })?;

    for character in "1.5".chars() {
        let _ =
            app.handle_key_event(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE))?;
    }
    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert!(app.has_modal());
    assert_eq!(app.last_notice(), Some("Retries must be an integer"));
    assert!(response_rx.try_recv().is_err());
    Ok(())
}

#[test]
fn mcp_elicitation_cycles_enum_and_boolean_fields() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let (response_tx, response_rx) = tokio::sync::oneshot::channel();

    app.handle_worker_message(WorkerMessage::McpElicitationRequest {
        request: McpElicitationRequest {
            server_name: "planner".to_owned(),
            message: "Choose mode".to_owned(),
            requested_schema: json!({
                "type": "object",
                "properties": {
                    "mode": {
                        "type": "string",
                        "title": "Mode",
                        "enum": ["safe", "fast"]
                    },
                    "confirm": {
                        "type": "boolean",
                        "title": "Confirm",
                        "default": false
                    }
                }
            }),
        },
        response_tx,
    })?;

    if app
        .modal_input_cursor()
        .as_ref()
        .map(|(label, _, _)| label.as_str())
        != Some("Mode")
    {
        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    }
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))?;
    if app
        .modal_input_cursor()
        .as_ref()
        .map(|(label, _, _)| label.as_str())
        != Some("Confirm")
    {
        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    }
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE))?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    assert!(!app.has_modal());
    let response = futures::executor::block_on(response_rx)?;
    assert_eq!(response.action, McpElicitationAction::Accept);
    assert_eq!(
        response.content,
        Some(json!({
            "mode": "fast",
            "confirm": true
        }))
    );
    Ok(())
}

#[test]
fn config_text_field_uses_modal_and_applies_value() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .selected_field = Some(ConfigField::ProviderBaseUrl);

    assert!(!app.config_is_editing());

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(app.config_is_editing());
    assert_eq!(app.modal_title(), Some("Endpoint"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE))?;
    let detail = app.modal_lines().join("\n");
    assert!(detail.contains("OpenAI-compatible DeepSeek endpoint"));
    assert!(detail.contains("key: base_url"));
    assert!(detail.contains("value: https://api.deepseek.comx|"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(!app.config_is_editing());
    let state = app
        .config_state
        .as_ref()
        .expect("config state should still exist");
    assert_eq!(state.draft.provider_base_url, "https://api.deepseek.comx");
    assert!(state.dirty);
    Ok(())
}

#[test]
fn config_escape_cancels_text_modal_before_closing_panel() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should still exist")
        .selected_field = Some(ConfigField::ProviderBaseUrl);

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE))?;
    assert!(app.config_is_editing());

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;
    assert!(app.is_config_mode());
    assert!(!app.config_is_editing());
    let state = app
        .config_state
        .as_ref()
        .expect("config state should still exist");
    assert_eq!(state.draft.provider_base_url, "https://api.deepseek.com");

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;
    assert!(!app.is_config_mode());
    Ok(())
}

#[test]
fn config_inline_api_key_uses_secret_modal_and_persists_to_disk() -> Result<()> {
    let temp = tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    test_config().save(&config_path)?;

    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.config_path = config_path.clone();
    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should exist after opening /config")
        .selected_field = Some(ConfigField::ProviderApiKey);

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(app.has_modal());
    assert_eq!(app.modal_title(), Some("API Key"));

    for character in "runtime-secret".chars() {
        let _ =
            app.handle_key_event(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE))?;
    }
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))?;

    let Some(AppAction::ConfigSaved { root_config }) = action else {
        panic!("expected config save action");
    };
    assert_eq!(
        root_config
            .providers
            .get("deepseek")
            .and_then(|value| value.get("api_key"))
            .and_then(|value| value.as_str()),
        Some("runtime-secret")
    );

    let saved = RootConfig::load(&config_path)?;
    assert_eq!(
        saved
            .providers
            .get("deepseek")
            .and_then(|value| value.get("api_key"))
            .and_then(|value| value.as_str()),
        Some("runtime-secret")
    );
    Ok(())
}

#[test]
fn config_modal_ctrl_s_applies_field_and_saves() -> Result<()> {
    let temp = tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    test_config().save(&config_path)?;

    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.config_path = config_path.clone();
    app.open_config_panel();
    app.config_state
        .as_mut()
        .expect("config state should exist after opening /config")
        .selected_field = Some(ConfigField::ProviderApiKey);

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    for character in "saved-from-modal".chars() {
        let _ =
            app.handle_key_event(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE))?;
    }

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))?;

    let Some(AppAction::ConfigSaved { root_config }) = action else {
        panic!("expected config save action");
    };
    assert!(!app.has_modal());
    assert_eq!(
        root_config
            .providers
            .get("deepseek")
            .and_then(|value| value.get("api_key"))
            .and_then(|value| value.as_str()),
        Some("saved-from-modal")
    );
    let saved = RootConfig::load(&config_path)?;
    assert_eq!(
        saved
            .providers
            .get("deepseek")
            .and_then(|value| value.get("api_key"))
            .and_then(|value| value.as_str()),
        Some("saved-from-modal")
    );
    Ok(())
}

#[test]
fn setup_modal_ctrl_s_applies_field_and_saves_config() -> Result<()> {
    let temp = tempdir()?;
    let config_path = temp.path().join("config").join("sigil.toml");
    let workspace_root = temp.path().join("workspace");
    let mut app = AppState::from_setup(config_path.clone(), workspace_root, None);
    {
        let state = app
            .setup_state
            .as_mut()
            .expect("setup state should exist in setup mode");
        state.trusted_current_folder = true;
        state.selected_field = SetupField::ApiKey;
    }

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    for character in "setup-saved-key".chars() {
        let _ =
            app.handle_key_event(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE))?;
    }

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))?;

    let Some(AppAction::SetupCompleted {
        config_path: saved_path,
        root_config,
    }) = action
    else {
        panic!("expected setup completion action");
    };
    assert_eq!(saved_path, config_path);
    assert!(!app.has_modal());
    assert_eq!(
        root_config
            .providers
            .get("deepseek")
            .and_then(|value| value.get("api_key"))
            .and_then(|value| value.as_str()),
        Some("setup-saved-key")
    );

    let saved = RootConfig::load(&saved_path)?;
    assert_eq!(
        saved
            .providers
            .get("deepseek")
            .and_then(|value| value.get("api_key"))
            .and_then(|value| value.as_str()),
        Some("setup-saved-key")
    );
    Ok(())
}
