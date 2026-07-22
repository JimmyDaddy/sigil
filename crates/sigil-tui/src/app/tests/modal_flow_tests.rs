use super::super::modal_flow::ModalOutcome;
use super::*;

#[test]
fn f1_opens_keyboard_help_modal_from_composer() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.composer.input = "draft".to_owned();
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

    assert_eq!(app.composer.input, "draft");
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
    app.composer.input = "long task".to_owned();
    assert!(matches!(
        app.submit_input()?,
        Some(AppAction::SubmitPrompt(prompt)) if prompt == "long task"
    ));
    assert!(app.runtime.is_busy);

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE))?;
    assert!(app.has_modal());

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;

    assert!(action.is_none());
    assert!(!app.has_modal());
    assert!(app.runtime.is_busy);
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
    assert!(app.runtime.active_model_picker_refresh.is_some());
    assert!(matches!(
        app.runtime.pending_worker_commands.last(),
        Some(WorkerCommand::RefreshProviderModels { .. })
    ));
    assert_eq!(
        app.last_notice(),
        Some("loading provider model list (https://api.deepseek.com)")
    );
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
    assert_eq!(
        app.last_notice(),
        Some("loading provider model list (https://api.deepseek.com)")
    );
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
fn model_picker_empty_refresh_keeps_local_options() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_model_picker(ModelPickerTarget::Provider, "custom-model");
    let before = app.modal_lines().join("\n");

    let changed = app.apply_model_picker_refresh(ModelPickerRefresh {
        target: ModelPickerTarget::Provider,
        current: "custom-model".to_owned(),
        base_url: "https://empty.example".to_owned(),
        result: Ok(Vec::new()),
    });

    assert!(changed);
    assert_eq!(app.last_notice(), Some("using local model list"));
    assert_eq!(app.modal_lines().join("\n"), before);
    Ok(())
}

#[test]
fn model_picker_submit_updates_setup_and_fim_targets() -> Result<()> {
    let temp = tempdir()?;
    let mut setup_app = AppState::from_setup(
        temp.path().join("sigil.toml"),
        temp.path().join("workspace"),
        None,
    );
    setup_app.open_model_picker(ModelPickerTarget::Setup, "deepseek-v4-flash");

    assert!(matches!(
        setup_app.handle_modal_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
        ModalOutcome::None
    ));
    assert_eq!(setup_app.last_notice(), Some("model deepseek-v4-pro"));

    let outcome = setup_app.submit_modal();
    setup_app.apply_modal_outcome(outcome);

    assert_eq!(
        setup_app
            .setup_state
            .as_ref()
            .map(|state| state.model.as_str()),
        Some("deepseek-v4-pro")
    );
    assert_eq!(
        setup_app.last_notice(),
        Some("selected model deepseek-v4-pro")
    );

    let mut config_app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    config_app.open_config_panel();
    config_app.open_model_picker(ModelPickerTarget::ProviderFim, "deepseek-v4-pro");

    assert!(matches!(
        config_app.handle_modal_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
        ModalOutcome::None
    ));
    assert_eq!(
        config_app.last_notice(),
        Some("fim model deepseek-v4-flash")
    );

    let outcome = config_app.submit_modal();
    config_app.apply_modal_outcome(outcome);

    let state = config_app
        .config_state
        .as_ref()
        .expect("config state should remain open");
    assert_eq!(state.draft.provider_fim_model, "deepseek-v4-flash");
    assert!(state.dirty);
    assert_eq!(
        config_app.last_notice(),
        Some("selected fim model deepseek-v4-flash")
    );
    Ok(())
}

#[test]
fn skill_arguments_modal_outcome_has_default_notice_fallback() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    app.apply_modal_outcome(ModalOutcome::TextSubmitted {
        target: super::super::modal_flow::TextInputTarget::SkillArguments,
        value: "module=runtime".to_owned(),
    });

    assert_eq!(app.last_notice(), Some("skill arguments submitted"));
}

#[test]
fn model_picker_key_edges_cover_up_decrement_and_empty_selection() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_model_picker(ModelPickerTarget::Provider, "deepseek-v4-flash");

    assert!(matches!(
        app.handle_modal_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
        ModalOutcome::None
    ));
    assert_eq!(app.last_notice(), Some("model deepseek-v4-pro"));
    assert!(matches!(
        app.handle_modal_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)),
        ModalOutcome::None
    ));
    assert_eq!(app.last_notice(), Some("model deepseek-v4-flash"));

    if let Some(ModalState::ModelPicker(state)) = app.modal_state.as_mut() {
        state.options.clear();
        state.selected = 0;
    }
    assert!(matches!(
        app.handle_modal_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        ModalOutcome::Dismissed(message) if message == "closed picker"
    ));
    assert!(!app.has_modal());

    app.open_model_picker(ModelPickerTarget::Provider, "deepseek-v4-flash");
    if let Some(ModalState::ModelPicker(state)) = app.modal_state.as_mut() {
        state.options.clear();
        state.selected = 0;
    }
    assert!(matches!(
        app.submit_modal(),
        ModalOutcome::Dismissed(message) if message == "closed picker"
    ));
    assert!(!app.has_modal());
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
fn config_tail_messages_text_modal_updates_value_after_validation() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_config_panel();
    let state = app
        .config_state
        .as_mut()
        .expect("config state should exist after opening /config");
    state.set_section(ConfigSection::Compaction);
    state.selected_field = Some(ConfigField::CompactionTailMessages);

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert_eq!(app.modal_title(), Some("Tail messages"));
    assert_eq!(app.modal_input_cursor(), Some(("value".to_owned(), 1, 4)));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE))?;
    assert_eq!(app.last_notice(), Some("value does not accept 'x'"));
    assert!(app.modal_lines().join("\n").contains("value: 6|"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE))?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('9'), KeyModifiers::NONE))?;
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

    let state = app
        .config_state
        .as_ref()
        .expect("config state should remain open");
    assert!(!app.has_modal());
    assert_eq!(state.draft.compaction_tail_messages, "9");
    assert!(state.dirty);
    Ok(())
}

#[test]
fn mcp_elicitation_modal_validates_multiple_field_kinds_and_accepts_response() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let (response_tx, response_rx) = tokio::sync::oneshot::channel();

    app.handle_worker_message(WorkerMessage::McpElicitationRequest {
        request: McpElicitationRequest {
            server_name: "planner".to_owned(),
            message: "Need execution parameters".to_owned(),
            requested_schema: json!({
                "type": "object",
                "properties": {
                    "mode": {
                        "type": "string",
                        "title": "Mode",
                        "description": "Execution mode",
                        "enum": ["read", "write"],
                        "default": "read"
                    },
                    "force": {
                        "type": "boolean",
                        "title": "Force",
                        "description": "Force overwrite",
                        "default": false
                    },
                    "count": {
                        "type": "integer",
                        "title": "Count",
                        "description": "Number of items"
                    },
                    "threshold": {
                        "type": "number",
                        "title": "Threshold",
                        "description": "Retry threshold"
                    }
                },
                "required": ["count"]
            }),
        },
        response_tx,
    })?;

    let lines = app.modal_lines().join("\n");
    assert!(lines.contains("Need execution parameters"));
    assert!(lines.contains("server: planner"));
    assert!(lines.contains("fields: 4"));

    while app
        .modal_input_cursor()
        .as_ref()
        .map(|(label, _, _)| label.as_str())
        != Some("Mode")
    {
        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    }
    assert!(app.modal_lines().join("\n").contains("Mode: read|"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))?;
    assert_eq!(app.last_notice(), Some("editing Mode"));
    assert!(app.modal_lines().join("\n").contains("Mode: write|"));

    while app
        .modal_input_cursor()
        .as_ref()
        .map(|(label, _, _)| label.as_str())
        != Some("Force")
    {
        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    }
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE))?;
    assert!(app.modal_lines().join("\n").contains("Force: true|"));

    while app
        .modal_input_cursor()
        .as_ref()
        .map(|(label, _, _)| label.as_str())
        != Some("Count")
    {
        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    }

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(app.has_modal());
    assert_eq!(app.last_notice(), Some("Count is required"));

    for character in "12".chars() {
        let _ =
            app.handle_key_event(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE))?;
    }
    while app
        .modal_input_cursor()
        .as_ref()
        .map(|(label, _, _)| label.as_str())
        != Some("Threshold")
    {
        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    }
    for character in "1e309".chars() {
        let _ =
            app.handle_key_event(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE))?;
    }
    assert!(app.modal_lines().join("\n").contains("Threshold: 1e309|"));

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(app.has_modal());
    assert_eq!(app.last_notice(), Some("Threshold must be a finite number"));

    for _ in 0..5 {
        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE))?;
    }
    for character in "2.5".chars() {
        let _ =
            app.handle_key_event(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE))?;
    }

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(!app.has_modal());
    assert_eq!(app.last_notice(), Some("submitted MCP input to planner"));

    let response = futures::executor::block_on(response_rx)?;
    assert_eq!(response.action, McpElicitationAction::Accept);
    assert_eq!(
        response.content,
        Some(json!({
            "mode": "write",
            "force": true,
            "count": 12,
            "threshold": 2.5
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
    assert!(detail.contains("Provider API base URL"));
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
fn config_color_override_modal_stays_config_file_only() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_config_panel();
    {
        let state = app
            .config_state
            .as_mut()
            .expect("config state should still exist");
        state.set_section(ConfigSection::Appearance);
        assert!(!state.focus_field(ConfigField::AppearanceColorOverride));
    }

    assert!(!app.config_is_editing());
    let state = app
        .config_state
        .as_ref()
        .expect("config state should still exist");
    assert!(matches!(
        state.selected_field,
        Some(ConfigField::AppearanceInfoRail)
            | Some(ConfigField::AppearanceTheme)
            | Some(ConfigField::AppearanceSyntaxTheme)
            | Some(ConfigField::AppearanceUsageCostCurrency)
    ));

    let detail = app.config_detail_lines().join("\n");
    assert!(detail.contains("Fine-grained color token overrides are edited in sigil.toml"));
    assert!(!detail.contains("Color token:"));
    assert!(!detail.contains("Color group:"));
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

#[test]
fn model_picker_refresh_ignores_stale_results_and_reports_fallbacks() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_model_picker(ModelPickerTarget::Provider, "custom-model");

    assert!(!app.apply_model_picker_refresh(ModelPickerRefresh {
        target: ModelPickerTarget::Setup,
        current: "custom-model".to_owned(),
        base_url: "https://example.com".to_owned(),
        result: Ok(vec!["remote-model".to_owned()]),
    }));

    assert!(app.apply_model_picker_refresh(ModelPickerRefresh {
        target: ModelPickerTarget::Provider,
        current: "custom-model".to_owned(),
        base_url: "https://example.com".to_owned(),
        result: Ok(Vec::new()),
    }));
    assert_eq!(app.last_notice(), Some("using local model list"));

    assert!(app.apply_model_picker_refresh(ModelPickerRefresh {
        target: ModelPickerTarget::Provider,
        current: "custom-model".to_owned(),
        base_url: "https://example.com".to_owned(),
        result: Err("network down".to_owned()),
    }));
    assert_eq!(
        app.last_notice(),
        Some("using local model list: network down")
    );
    Ok(())
}

#[test]
fn text_input_modal_rejects_invalid_characters_for_numeric_fields() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.open_text_input(
        super::super::modal_flow::TextInputTarget::ConfigField(
            ConfigField::CompactionContextWindowTokens,
        ),
        "",
    );

    assert!(matches!(
        app.handle_modal_key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)),
        ModalOutcome::None
    ));
    assert_eq!(app.last_notice(), Some("value does not accept 'x'"));
    assert!(app.modal_lines().join("\n").contains("value: |"));

    assert!(matches!(
        app.handle_modal_key_event(KeyEvent::new(KeyCode::Char('4'), KeyModifiers::NONE)),
        ModalOutcome::None
    ));
    assert!(app.modal_lines().join("\n").contains("value: 4|"));
}

#[test]
fn mcp_elicitation_validates_required_and_numeric_fields() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let (response_tx, response_rx) = tokio::sync::oneshot::channel();

    app.handle_worker_message(WorkerMessage::McpElicitationRequest {
        request: McpElicitationRequest {
            server_name: "filesystem".to_owned(),
            message: "Need parameters".to_owned(),
            requested_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "title": "Path" },
                    "retries": { "type": "integer", "title": "Retries" },
                    "threshold": { "type": "number", "title": "Threshold" },
                    "mode": { "enum": ["safe", "fast"], "title": "Mode" }
                },
                "required": ["path"]
            }),
        },
        response_tx,
    })?;

    assert!(
        app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?
            .is_none()
    );
    assert_eq!(app.last_notice(), Some("Path is required"));
    assert!(app.has_modal());

    if let Some(ModalState::McpElicitation(state)) = app.modal_state.as_mut() {
        state
            .fields
            .iter_mut()
            .find(|field| field.name == "path")
            .expect("path field should exist")
            .buffer = "src/lib.rs".to_owned();
        state
            .fields
            .iter_mut()
            .find(|field| field.name == "retries")
            .expect("retries field should exist")
            .buffer = "abc".to_owned();
    }

    assert!(
        app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?
            .is_none()
    );
    assert_eq!(app.last_notice(), Some("Retries must be an integer"));

    if let Some(ModalState::McpElicitation(state)) = app.modal_state.as_mut() {
        state
            .fields
            .iter_mut()
            .find(|field| field.name == "retries")
            .expect("retries field should exist")
            .buffer = "3".to_owned();
        state
            .fields
            .iter_mut()
            .find(|field| field.name == "threshold")
            .expect("threshold field should exist")
            .buffer = "1e999".to_owned();
    }

    assert!(
        app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?
            .is_none()
    );
    assert_eq!(app.last_notice(), Some("Threshold must be a finite number"));

    if let Some(ModalState::McpElicitation(state)) = app.modal_state.as_mut() {
        state
            .fields
            .iter_mut()
            .find(|field| field.name == "threshold")
            .expect("threshold field should exist")
            .buffer = "0.25".to_owned();
    }

    assert!(
        app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?
            .is_none()
    );
    assert!(!app.has_modal());
    assert_eq!(app.last_notice(), Some("submitted MCP input to filesystem"));

    let response = futures::executor::block_on(response_rx)?;
    assert_eq!(response.action, McpElicitationAction::Accept);
    assert_eq!(
        response.content,
        Some(json!({
            "mode": "safe",
            "path": "src/lib.rs",
            "retries": 3,
            "threshold": 0.25
        }))
    );
    Ok(())
}

#[test]
fn mcp_elicitation_cycles_boolean_and_enum_fields() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let (response_tx, response_rx) = tokio::sync::oneshot::channel();

    app.handle_worker_message(WorkerMessage::McpElicitationRequest {
        request: McpElicitationRequest {
            server_name: "planner".to_owned(),
            message: "Choose execution mode".to_owned(),
            requested_schema: json!({
                "type": "object",
                "properties": {
                    "confirm": {
                        "type": "boolean",
                        "title": "Confirm",
                        "description": "Allow execution"
                    },
                    "mode": {
                        "enum": ["safe", "fast"],
                        "title": "Mode",
                        "description": "Execution mode"
                    }
                }
            }),
        },
        response_tx,
    })?;

    let lines = app.modal_lines().join("\n");
    assert!(lines.contains("selected: Allow execution"));
    assert_eq!(app.modal_input_cursor(), Some(("Confirm".to_owned(), 5, 5)));

    assert!(
        app.handle_key_event(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE))?
            .is_none()
    );
    assert!(app.modal_lines().join("\n").contains("Confirm: true|"));

    assert!(
        app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?
            .is_none()
    );
    assert_eq!(app.last_notice(), Some("editing Mode"));
    assert!(
        app.modal_lines()
            .join("\n")
            .contains("selected: Execution mode")
    );

    assert!(
        app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))?
            .is_none()
    );
    assert!(app.modal_lines().join("\n").contains("Mode: fast|"));

    assert!(
        app.handle_key_event(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE))?
            .is_none()
    );
    assert!(app.modal_lines().join("\n").contains("Mode: safe|"));

    assert!(
        app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?
            .is_none()
    );
    let response = futures::executor::block_on(response_rx)?;
    assert_eq!(
        response.content,
        Some(json!({
            "confirm": true,
            "mode": "safe"
        }))
    );
    Ok(())
}

#[test]
fn mcp_elicitation_key_edges_cover_boolean_enum_number_and_string_input() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let (response_tx, response_rx) = tokio::sync::oneshot::channel();

    app.handle_worker_message(WorkerMessage::McpElicitationRequest {
        request: McpElicitationRequest {
            server_name: "planner".to_owned(),
            message: "Fill fields".to_owned(),
            requested_schema: json!({
                "type": "object",
                "properties": {
                    "confirm": { "type": "boolean", "title": "Confirm", "default": false },
                    "mode": { "enum": ["safe", "fast"], "title": "Mode", "default": "safe" },
                    "count": { "type": "integer", "title": "Count" },
                    "note": { "type": "string", "title": "Note" }
                }
            }),
        },
        response_tx,
    })?;

    assert_eq!(app.modal_input_cursor(), Some(("Confirm".to_owned(), 5, 5)));
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE))?;
    assert!(app.modal_lines().join("\n").contains("Confirm: true|"));
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE))?;
    assert!(app.modal_lines().join("\n").contains("Confirm: false|"));
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE))?;
    assert!(app.modal_lines().join("\n").contains("Confirm: true|"));
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE))?;
    assert!(app.modal_lines().join("\n").contains("Confirm: false|"));

    for _ in 0..4 {
        if app
            .modal_input_cursor()
            .as_ref()
            .map(|(label, _, _)| label.as_str())
            == Some("Mode")
        {
            break;
        }
        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    }
    assert_eq!(
        app.modal_input_cursor()
            .as_ref()
            .map(|(label, _, _)| label.as_str()),
        Some("Mode")
    );
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE))?;
    assert!(app.modal_lines().join("\n").contains("Mode: fast|"));

    for _ in 0..4 {
        if app
            .modal_input_cursor()
            .as_ref()
            .map(|(label, _, _)| label.as_str())
            == Some("Count")
        {
            break;
        }
        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    }
    for character in "+12".chars() {
        let _ =
            app.handle_key_event(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE))?;
    }
    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE))?;
    assert!(app.modal_lines().join("\n").contains("Count: +12|"));

    for _ in 0..4 {
        if app
            .modal_input_cursor()
            .as_ref()
            .map(|(label, _, _)| label.as_str())
            == Some("Note")
        {
            break;
        }
        let _ = app.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))?;
    }
    for character in "memo".chars() {
        let _ =
            app.handle_key_event(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE))?;
    }

    let _ = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    let response = futures::executor::block_on(response_rx)?;

    assert_eq!(response.action, McpElicitationAction::Accept);
    assert_eq!(
        response.content,
        Some(json!({
            "confirm": false,
            "mode": "fast",
            "count": 12,
            "note": "memo"
        }))
    );
    Ok(())
}
