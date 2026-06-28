use super::*;
use crate::app::tests::common::test_config;
use crate::config_panel::ConfigState;

#[test]
fn provider_status_config_for_model_picker_prefers_config_setup_and_snapshot() {
    let root_config = test_config();
    let mut app = AppState::from_root_config(std::path::Path::new("sigil.toml"), &root_config);
    app.config_state = Some(ConfigState::from_root_config(&root_config));
    {
        let state = app
            .config_state
            .as_mut()
            .expect("config state should exist");
        state.draft.provider_base_url = "https://models.example.com".to_owned();
        state.draft.provider_beta_base_url = "https://beta.example.com".to_owned();
        state.draft.provider_anthropic_base_url = "https://anthropic.example.com".to_owned();
        state.draft.provider_api_key = "config-secret".to_owned();
        state.draft.provider_model = "base-model".to_owned();
        state.draft.provider_fim_model = "base-fim".to_owned();
        state.draft.provider_request_timeout_secs = "45".to_owned();
    }

    let provider = app
        .provider_status_config_for_model_picker(ModelPickerTarget::Provider, "picked")
        .expect("provider status config should resolve");
    assert_eq!(provider.base_url, "https://models.example.com");
    assert_eq!(provider.api_key.as_deref(), Some("config-secret"));
    assert_eq!(provider.request_timeout_secs, 45);

    let fim = app
        .provider_status_config_for_model_picker(ModelPickerTarget::ProviderFim, "picked-fim")
        .expect("fim provider status config should resolve");
    assert_eq!(fim.base_url, "https://models.example.com");
    assert_eq!(fim.request_timeout_secs, 45);

    let temp = tempfile::tempdir().expect("tempdir should be created");
    let mut setup_app = AppState::from_setup(
        temp.path().join("sigil.toml"),
        temp.path().join("workspace"),
        None,
    );
    if let Some(state) = setup_app.setup_state.as_mut() {
        state.api_key = "setup-secret".to_owned();
    }
    let setup_provider = setup_app
        .provider_status_config_for_model_picker(ModelPickerTarget::Setup, "setup-model")
        .expect("setup provider status config should resolve");
    assert_eq!(setup_provider.api_key.as_deref(), Some("setup-secret"));

    let snapshot_provider =
        AppState::from_root_config(std::path::Path::new("sigil.toml"), &root_config)
            .provider_status_config_for_model_picker(ModelPickerTarget::Provider, "snapshot")
            .expect("snapshot provider status config should resolve");
    assert!(!snapshot_provider.base_url.is_empty());
}

#[test]
fn modal_outcomes_update_setup_and_config_state() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let mut setup_app = AppState::from_setup(
        temp.path().join("sigil.toml"),
        temp.path().join("workspace"),
        None,
    );
    setup_app.apply_modal_outcome(ModalOutcome::ModelSelected {
        target: ModelPickerTarget::Setup,
        value: "setup-model".to_owned(),
    });
    setup_app.apply_modal_outcome(ModalOutcome::SecretSubmitted {
        target: SecretInputTarget::SetupApiKey,
        value: "setup-secret".to_owned(),
    });
    setup_app.apply_modal_outcome(ModalOutcome::TextSubmitted {
        target: TextInputTarget::SetupModel,
        value: "typed-model".to_owned(),
    });
    let setup = setup_app.setup_state.expect("setup state should exist");
    assert_eq!(setup.model, "typed-model");
    assert_eq!(setup.api_key, "setup-secret");

    let root_config = test_config();
    let mut config_app =
        AppState::from_root_config(std::path::Path::new("sigil.toml"), &root_config);
    config_app.config_state = Some(ConfigState::from_root_config(&root_config));
    config_app.apply_modal_outcome(ModalOutcome::ModelSelected {
        target: ModelPickerTarget::Provider,
        value: "provider-model".to_owned(),
    });
    config_app.apply_modal_outcome(ModalOutcome::ModelSelected {
        target: ModelPickerTarget::ProviderFim,
        value: "fim-model".to_owned(),
    });
    config_app.apply_modal_outcome(ModalOutcome::SecretSubmitted {
        target: SecretInputTarget::ConfigProviderApiKey,
        value: "config-secret".to_owned(),
    });
    config_app.apply_modal_outcome(ModalOutcome::TextSubmitted {
        target: TextInputTarget::ConfigField(ConfigField::ProviderBaseUrl),
        value: "https://alt.example.com".to_owned(),
    });
    let state = config_app.config_state.expect("config state should exist");
    assert_eq!(state.draft.provider_model, "provider-model");
    assert_eq!(state.draft.provider_fim_model, "fim-model");
    assert_eq!(state.draft.provider_api_key, "config-secret");
    assert_eq!(state.draft.provider_base_url, "https://alt.example.com");
    assert!(state.dirty);
}

#[test]
fn elicitation_helpers_cover_defaults_and_content_validation() -> anyhow::Result<()> {
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "name": { "type": "string", "title": "Name", "description": "User name" },
            "enabled": { "type": "boolean", "title": "Enabled" },
            "mode": { "enum": ["safe", "fast"], "title": "Mode" },
            "count": { "type": "integer", "title": "Count", "default": 7 },
            "ratio": { "type": "number", "title": "Ratio", "default": 0.5 }
        },
        "required": ["name"]
    });
    let mut fields = elicitation_fields_from_schema(&schema);
    let name = fields
        .iter()
        .find(|field| field.name == "name")
        .expect("name field should exist");
    let enabled = fields
        .iter()
        .find(|field| field.name == "enabled")
        .expect("enabled field should exist");
    let mode = fields
        .iter()
        .find(|field| field.name == "mode")
        .expect("mode field should exist");
    let count = fields
        .iter()
        .find(|field| field.name == "count")
        .expect("count field should exist");
    let ratio = fields
        .iter()
        .find(|field| field.name == "ratio")
        .expect("ratio field should exist");
    assert_eq!(name.description.as_deref(), Some("User name"));
    assert_eq!(enabled.buffer, "false");
    assert_eq!(mode.buffer, "safe");
    assert_eq!(count.buffer, "7");
    assert_eq!(ratio.buffer, "0.5");
    assert_eq!(elicitation_field_display_value(enabled), "false");
    assert!(elicitation_field_accepts_text(name));
    assert!(!elicitation_field_accepts_text(enabled));

    assert_eq!(
        elicitation_content_from_fields(&fields),
        Err("Name is required".to_owned())
    );

    fields
        .iter_mut()
        .find(|field| field.name == "name")
        .expect("name field should exist")
        .buffer = "Alice".to_owned();
    fields
        .iter_mut()
        .find(|field| field.name == "count")
        .expect("count field should exist")
        .buffer = "NaN".to_owned();
    assert_eq!(
        elicitation_content_from_fields(&fields),
        Err("Count must be an integer".to_owned())
    );

    fields
        .iter_mut()
        .find(|field| field.name == "count")
        .expect("count field should exist")
        .buffer = "3".to_owned();
    fields
        .iter_mut()
        .find(|field| field.name == "ratio")
        .expect("ratio field should exist")
        .buffer = "NaN".to_owned();
    assert_eq!(
        elicitation_content_from_fields(&fields),
        Err("Ratio must be a finite number".to_owned())
    );

    fields
        .iter_mut()
        .find(|field| field.name == "ratio")
        .expect("ratio field should exist")
        .buffer = "0.75".to_owned();
    let content = elicitation_content_from_fields(&fields).map_err(anyhow::Error::msg)?;
    assert_eq!(
        content,
        serde_json::json!({
            "count": 3,
            "enabled": false,
            "mode": "safe",
            "name": "Alice",
            "ratio": 0.75
        })
    );
    Ok(())
}

#[test]
fn text_input_targets_and_submit_modal_cover_edge_cases() {
    assert!(text_input_target_accepts_char(
        TextInputTarget::SetupModel,
        'a'
    ));
    assert!(!text_input_target_accepts_char(
        TextInputTarget::ConfigField(ConfigField::CompactionContextWindowTokens),
        'x'
    ));
    assert_eq!(ModelPickerTarget::ProviderFim.title(), "FIM Model");
    assert_eq!(
        SecretInputTarget::SetupApiKey.summary(),
        "Saved as plaintext with setup. SIGIL_API_KEY can override at runtime."
    );
    assert_eq!(
        TextInputTarget::ConfigField(ConfigField::ProviderBaseUrl).config_key(),
        Some("base_url")
    );

    let mut app = AppState::from_root_config(std::path::Path::new("sigil.toml"), &test_config());
    app.modal_state = Some(ModalState::ModelPicker(ModelPickerState {
        target: ModelPickerTarget::Provider,
        current: "current".to_owned(),
        options: Vec::new(),
        selected: 0,
    }));
    assert!(matches!(
        app.submit_modal(),
        ModalOutcome::Dismissed(message) if message == "closed picker"
    ));
}

#[test]
fn modal_titles_lines_and_cursors_cover_secret_text_and_none_states() {
    let mut app = AppState::from_root_config(std::path::Path::new("sigil.toml"), &test_config());

    assert_eq!(
        ModelPickerTarget::ProviderFim.summary(),
        "Choose FIM model. Esc to type your own."
    );
    assert_eq!(SecretInputTarget::ConfigProviderApiKey.title(), "API Key");
    assert_eq!(TextInputTarget::SetupModel.title(), "Model ID");
    assert_eq!(TextInputTarget::SetupModel.summary(), "Custom model id.");
    assert_eq!(TextInputTarget::SetupModel.prompt_label(), "model");
    assert_eq!(TextInputTarget::SetupModel.config_key(), None);

    app.open_secret_input(SecretInputTarget::SetupApiKey, "abc");
    assert_eq!(app.modal_title(), Some("API Key"));
    let secret_lines = app.modal_lines().join("\n");
    assert!(secret_lines.contains("SIGIL_API_KEY can override"));
    assert!(secret_lines.contains("api_key: ***|"));
    assert_eq!(app.modal_input_cursor(), Some(("api_key".to_owned(), 3, 4)));

    app.open_text_input(TextInputTarget::SetupModel, "flash");
    assert_eq!(app.modal_title(), Some("Model ID"));
    let text_lines = app.modal_lines().join("\n");
    assert!(text_lines.contains("Custom model id."));
    assert!(text_lines.contains("model: flash|"));
    assert_eq!(app.modal_input_cursor(), Some(("model".to_owned(), 5, 3)));

    app.open_text_input(
        TextInputTarget::ConfigField(ConfigField::ProviderBaseUrl),
        "https://api.deepseek.com",
    );
    assert_eq!(app.modal_title(), Some("Endpoint"));
    assert_eq!(app.modal_input_cursor(), Some(("value".to_owned(), 24, 4)));

    app.open_keyboard_help();
    assert!(app.modal_input_cursor().is_none());

    app.modal_state = None;
    assert!(app.modal_lines().is_empty());
    assert!(app.modal_input_cursor().is_none());
}

#[test]
fn modal_paste_text_updates_secret_and_text_inputs() {
    let mut app = AppState::from_root_config(std::path::Path::new("sigil.toml"), &test_config());

    assert!(matches!(
        app.handle_modal_paste_text("ignored"),
        ModalOutcome::None
    ));

    app.open_secret_input(SecretInputTarget::SetupApiKey, "");
    assert!(matches!(
        app.handle_modal_paste_text("sk-test\nwith-control\u{0007}"),
        ModalOutcome::None
    ));
    assert_eq!(app.last_notice.as_deref(), Some("editing api key"));
    assert!(matches!(
        app.submit_modal(),
        ModalOutcome::SecretSubmitted {
            target: SecretInputTarget::SetupApiKey,
            value
        } if value == "sk-testwith-control"
    ));

    app.open_text_input(
        TextInputTarget::ConfigField(ConfigField::ProviderModel),
        "deepseek",
    );
    assert!(matches!(
        app.handle_modal_paste_text("\n-v4-pro\u{0007}"),
        ModalOutcome::None
    ));
    assert_eq!(app.last_notice.as_deref(), Some("editing value"));
    assert!(matches!(
        app.submit_modal(),
        ModalOutcome::TextSubmitted {
            target: TextInputTarget::ConfigField(ConfigField::ProviderModel),
            value
        } if value == "deepseek-v4-pro"
    ));

    app.open_model_picker(ModelPickerTarget::Provider, "deepseek-v4-flash");
    assert!(matches!(
        app.handle_modal_paste_text("ignored"),
        ModalOutcome::None
    ));
    app.open_keyboard_help();
    assert!(matches!(
        app.handle_modal_paste_text("ignored"),
        ModalOutcome::None
    ));
}

#[test]
fn model_picker_and_input_key_events_cover_wrap_dismiss_and_submission() {
    let mut app = AppState::from_root_config(std::path::Path::new("sigil.toml"), &test_config());
    app.open_model_picker(ModelPickerTarget::Provider, "deepseek-v4-flash");
    if let Some(ModalState::ModelPicker(state)) = app.modal_state.as_mut() {
        state.selected = 0;
    }

    assert!(matches!(
        app.handle_modal_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)),
        ModalOutcome::None
    ));
    assert!(
        app.last_notice
            .as_deref()
            .unwrap_or_default()
            .starts_with("model ")
    );

    assert!(matches!(
        app.handle_modal_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
        ModalOutcome::None
    ));

    assert!(matches!(
        app.handle_modal_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        ModalOutcome::Dismissed(message) if message == "closed picker"
    ));

    app.modal_state = Some(ModalState::ModelPicker(ModelPickerState {
        target: ModelPickerTarget::Provider,
        current: "current".to_owned(),
        options: Vec::new(),
        selected: 0,
    }));
    assert!(matches!(
        app.handle_modal_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        ModalOutcome::Dismissed(message) if message == "closed picker"
    ));

    app.open_secret_input_with_char(SecretInputTarget::SetupApiKey, 'x');
    assert!(matches!(
        app.handle_modal_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)),
        ModalOutcome::None
    ));
    assert_eq!(app.last_notice.as_deref(), Some("editing api key"));
    assert!(matches!(
        app.handle_modal_key_event(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE)),
        ModalOutcome::None
    ));
    assert!(matches!(
        app.handle_modal_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        ModalOutcome::SecretSubmitted {
            target: SecretInputTarget::SetupApiKey,
            value
        } if value == "y"
    ));

    app.open_text_input_with_char(
        TextInputTarget::ConfigField(ConfigField::CompactionContextWindowTokens),
        '1',
    );
    assert!(matches!(
        app.handle_modal_key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)),
        ModalOutcome::None
    ));
    assert_eq!(
        app.last_notice.as_deref(),
        Some("value does not accept 'x'")
    );
    assert!(matches!(
        app.handle_modal_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)),
        ModalOutcome::None
    ));
    assert!(matches!(
        app.handle_modal_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        ModalOutcome::TextSubmitted {
            target: TextInputTarget::ConfigField(ConfigField::CompactionContextWindowTokens),
            value
        } if value.is_empty()
    ));

    app.open_keyboard_help();
    assert!(matches!(
        app.handle_modal_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        ModalOutcome::Dismissed(message) if message == "closed keyboard help"
    ));

    app.open_keyboard_help();
    assert!(matches!(
        app.submit_modal(),
        ModalOutcome::Dismissed(message) if message == "closed keyboard help"
    ));
}

#[test]
fn mcp_elicitation_key_events_cover_wrapping_and_empty_state_guards() {
    let mut app = AppState::from_root_config(std::path::Path::new("sigil.toml"), &test_config());
    let (response_tx, _response_rx) = tokio::sync::oneshot::channel();
    app.open_mcp_elicitation(
        McpElicitationRequest {
            server_name: "filesystem".to_owned(),
            message: "Need values".to_owned(),
            requested_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "title": "Name" },
                    "enabled": { "type": "boolean", "title": "Enabled" },
                    "mode": { "enum": ["safe", "fast"], "title": "Mode" },
                    "count": { "type": "integer", "title": "Count" }
                }
            }),
        },
        response_tx,
    );

    if let Some(ModalState::McpElicitation(state)) = app.modal_state.as_mut() {
        state.selected = 0;
    }
    assert!(matches!(
        app.handle_modal_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)),
        ModalOutcome::None
    ));
    assert!(
        app.last_notice
            .as_deref()
            .unwrap_or_default()
            .starts_with("editing ")
    );

    assert!(matches!(
        app.handle_modal_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
        ModalOutcome::None
    ));

    if let Some(ModalState::McpElicitation(state)) = app.modal_state.as_mut() {
        state.selected = state
            .fields
            .iter()
            .position(|field| field.name == "name")
            .expect("name field should exist");
        state.fields[state.selected].buffer = "ab".to_owned();
    }
    assert!(matches!(
        app.handle_modal_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)),
        ModalOutcome::None
    ));

    if let Some(ModalState::McpElicitation(state)) = app.modal_state.as_mut() {
        state.selected = state
            .fields
            .iter()
            .position(|field| field.name == "enabled")
            .expect("enabled field should exist");
    }
    let _ = app.handle_modal_key_event(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
    let _ = app.handle_modal_key_event(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));

    let mode_index = if let Some(ModalState::McpElicitation(state)) = app.modal_state.as_mut() {
        let index = state
            .fields
            .iter()
            .position(|field| field.name == "mode")
            .expect("mode field should exist");
        state.selected = index;
        index
    } else {
        panic!("expected elicitation modal");
    };
    let before = match app.modal_state.as_ref() {
        Some(ModalState::McpElicitation(state)) => state.fields[mode_index].buffer.clone(),
        _ => panic!("expected elicitation modal"),
    };
    let _ = app.handle_modal_key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
    let after = match app.modal_state.as_ref() {
        Some(ModalState::McpElicitation(state)) => state.fields[mode_index].buffer.clone(),
        _ => panic!("expected elicitation modal"),
    };
    assert_eq!(before, after);

    if let Some(ModalState::McpElicitation(state)) = app.modal_state.as_mut() {
        state.selected = state
            .fields
            .iter()
            .position(|field| field.name == "count")
            .expect("count field should exist");
    }
    let _ = app.handle_modal_key_event(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE));

    app.modal_state = None;
    assert!(matches!(
        app.finish_mcp_elicitation(sigil_runtime::McpElicitationResponse::cancel()),
        ModalOutcome::None
    ));
    app.cycle_selected_elicitation_option(true);
    app.toggle_selected_elicitation_bool();
    assert!(app.selected_elicitation_field_mut().is_none());
}

#[test]
fn modal_helper_edge_cases_cover_refresh_defaults_and_empty_values() {
    let mut app = AppState::from_root_config(std::path::Path::new("sigil.toml"), &test_config());

    assert_eq!(
        SecretInputTarget::ConfigProviderApiKey.summary(),
        "Saved as plaintext on Ctrl-S. SIGIL_API_KEY can override at runtime."
    );

    app.open_model_picker(ModelPickerTarget::Provider, "current-model");
    if let Some(ModalState::ModelPicker(state)) = app.modal_state.as_mut() {
        state.options = vec!["stale-model".to_owned()];
        state.selected = 0;
    }
    assert!(app.apply_model_picker_refresh(ModelPickerRefresh {
        target: ModelPickerTarget::Provider,
        current: "current-model".to_owned(),
        base_url: "https://example.com".to_owned(),
        result: Ok(vec!["current-model".to_owned(), "remote-model".to_owned()]),
    }));
    if let Some(ModalState::ModelPicker(state)) = app.modal_state.as_ref() {
        assert_eq!(state.selected, 0);
        assert_eq!(state.options[0], "current-model");
    } else {
        panic!("expected model picker modal");
    }

    app.modal_state = None;
    assert!(!app.apply_model_picker_refresh(ModelPickerRefresh {
        target: ModelPickerTarget::Provider,
        current: "current-model".to_owned(),
        base_url: "https://example.com".to_owned(),
        result: Ok(vec!["remote-model".to_owned()]),
    }));
    assert!(matches!(
        app.handle_modal_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        ModalOutcome::None
    ));
    assert!(matches!(app.submit_modal(), ModalOutcome::None));

    app.open_secret_input(SecretInputTarget::ConfigProviderApiKey, "secret");
    assert!(matches!(
        app.handle_modal_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        ModalOutcome::Dismissed(message) if message == "closed secret input"
    ));

    app.open_secret_input(SecretInputTarget::ConfigProviderApiKey, "secret");
    assert!(matches!(
        app.handle_modal_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)),
        ModalOutcome::None
    ));

    app.open_text_input(TextInputTarget::SetupModel, "flash");
    assert!(matches!(
        app.handle_modal_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)),
        ModalOutcome::None
    ));

    app.open_keyboard_help();
    assert!(matches!(
        app.handle_modal_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)),
        ModalOutcome::None
    ));

    app.open_model_picker(ModelPickerTarget::ProviderFim, "fim-model");
    assert!(matches!(
        app.submit_modal(),
        ModalOutcome::ModelSelected {
            target: ModelPickerTarget::ProviderFim,
            value
        } if value == "fim-model"
    ));

    let root_config = test_config();
    app.config_state = Some(ConfigState::from_root_config(&root_config));
    app.apply_modal_outcome(ModalOutcome::TextSubmitted {
        target: TextInputTarget::ConfigField(ConfigField::ProviderBaseUrl),
        value: "https://api.deepseek.com".to_owned(),
    });
    assert!(
        !app.config_state
            .as_ref()
            .expect("config state should exist")
            .dirty
    );

    assert!(elicitation_fields_from_schema(&serde_json::json!({})).is_empty());
    assert_eq!(
        elicitation_default_value(
            &serde_json::json!({ "default": "value" }),
            &ElicitationFieldKind::String
        ),
        "value"
    );
    assert_eq!(
        elicitation_default_value(
            &serde_json::json!({ "default": true }),
            &ElicitationFieldKind::Boolean
        ),
        "true"
    );
    assert_eq!(
        elicitation_default_value(
            &serde_json::json!({ "default": 1.5 }),
            &ElicitationFieldKind::Number
        ),
        "1.5"
    );

    let integer_empty = ElicitationFieldState {
        name: "count".to_owned(),
        label: "Count".to_owned(),
        description: None,
        required: false,
        kind: ElicitationFieldKind::Integer,
        buffer: String::new(),
    };
    let number_empty = ElicitationFieldState {
        name: "ratio".to_owned(),
        label: "Ratio".to_owned(),
        description: None,
        required: false,
        kind: ElicitationFieldKind::Number,
        buffer: String::new(),
    };
    assert_eq!(
        elicitation_content_from_fields(&[integer_empty, number_empty]),
        Ok(serde_json::json!({}))
    );
}
