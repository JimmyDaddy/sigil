use super::*;

impl AppState {
    pub fn setup_lines(&self) -> Vec<String> {
        let Some(state) = &self.setup_state else {
            return Vec::new();
        };

        let mut lines = vec![
            "Quick setup".to_owned(),
            "[workspace]".to_owned(),
            render_setup_toggle_row(
                SetupField::TrustCurrentFolder,
                state.selected_field,
                "trust_current_folder",
                state.trusted_current_folder,
            ),
            String::new(),
            "[runtime]".to_owned(),
            render_setup_value_row(
                SetupField::Model,
                state.selected_field,
                "model",
                &state.model,
                Some("Enter choose"),
            ),
            render_setup_value_row(
                SetupField::ApiKey,
                state.selected_field,
                "api_key",
                &state.masked_api_key(),
                Some("Enter input"),
            ),
            render_setup_action_row(SetupField::Save, state.selected_field, "save and start"),
            String::new(),
            "[notes]".to_owned(),
            format!("auth={}", state.auth_summary()),
            "defaults: ask / mem on / compact on".to_owned(),
        ];

        if let Some(error) = &state.startup_error {
            lines.push(String::new());
            lines.push(format!("load failed: {error}"));
        }

        lines.push(String::new());
        lines.push(format!(
            "Tab move  Enter open/toggle  Ctrl-S save  Ctrl-C quit  env={TERMQUILL_API_KEY_ENV}"
        ));
        lines
    }

    pub(super) fn handle_setup_key_event(&mut self, key: KeyEvent) -> Result<Option<AppAction>> {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.should_quit = true;
            return Ok(None);
        }
        if self.has_modal() {
            if key.code == KeyCode::Char('s') && key.modifiers.contains(KeyModifiers::CONTROL) {
                let outcome = self.submit_modal();
                self.apply_modal_outcome(outcome);
                return self.complete_setup();
            }
            let outcome = self.handle_modal_key_event(key);
            self.apply_modal_outcome(outcome);
            return Ok(None);
        }

        let Some(selected_field) = self.setup_state.as_ref().map(|state| state.selected_field)
        else {
            return Ok(None);
        };

        match key.code {
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return self.complete_setup();
            }
            KeyCode::Tab | KeyCode::Down => {
                let Some(state) = self.setup_state.as_mut() else {
                    return Ok(None);
                };
                state.selected_field = state.selected_field.next();
                self.last_notice = Some(format!(
                    "setup field {}",
                    setup_field_label(state.selected_field)
                ));
                return Ok(None);
            }
            KeyCode::BackTab | KeyCode::Up => {
                let Some(state) = self.setup_state.as_mut() else {
                    return Ok(None);
                };
                state.selected_field = state.selected_field.previous();
                self.last_notice = Some(format!(
                    "setup field {}",
                    setup_field_label(state.selected_field)
                ));
                return Ok(None);
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Enter
                if matches!(selected_field, SetupField::TrustCurrentFolder) =>
            {
                let Some(state) = self.setup_state.as_mut() else {
                    return Ok(None);
                };
                state.trusted_current_folder = !state.trusted_current_folder;
                self.last_notice = Some(format!(
                    "trust current folder {}",
                    setup_bool_label(state.trusted_current_folder)
                ));
                return Ok(None);
            }
            KeyCode::Enter if matches!(selected_field, SetupField::Save) => {
                return self.complete_setup();
            }
            KeyCode::Enter if matches!(selected_field, SetupField::Model) => {
                let current = self
                    .setup_state
                    .as_ref()
                    .map(|state| state.model.clone())
                    .unwrap_or_default();
                self.open_model_picker(ModelPickerTarget::Setup, &current);
                return Ok(None);
            }
            KeyCode::Enter if matches!(selected_field, SetupField::ApiKey) => {
                let current = self
                    .setup_state
                    .as_ref()
                    .map(|state| state.api_key.clone())
                    .unwrap_or_default();
                self.open_secret_input(SecretInputTarget::SetupApiKey, &current);
                return Ok(None);
            }
            KeyCode::Backspace => {
                return Ok(None);
            }
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                if matches!(selected_field, SetupField::ApiKey) {
                    self.open_secret_input_with_char(SecretInputTarget::SetupApiKey, character);
                    return Ok(None);
                }
                if matches!(selected_field, SetupField::Model) {
                    self.open_text_input_with_char(TextInputTarget::SetupModel, character);
                    return Ok(None);
                }
                return Ok(None);
            }
            _ => {}
        }

        Ok(None)
    }

    fn complete_setup(&mut self) -> Result<Option<AppAction>> {
        let Some(state) = &mut self.setup_state else {
            return Ok(None);
        };

        if let Some(error) = validate_setup_state(state) {
            self.last_notice = Some(error.clone());
            self.push_event("setup:error", error);
            return Ok(None);
        }

        let root_config = match build_setup_root_config(state) {
            Ok(root_config) => {
                let persisted_root_config = persisted_root_config(&root_config);
                persisted_root_config.save(&state.config_path)?;
                root_config
            }
            Err(error) => {
                self.last_notice = Some(error.to_string());
                self.push_event("setup:error", error.to_string());
                return Ok(None);
            }
        };
        self.last_notice = Some(format!("saved config to {}", state.config_path.display()));
        Ok(Some(AppAction::SetupCompleted {
            config_path: state.config_path.clone(),
            root_config: Box::new(root_config),
        }))
    }
}

fn setup_field_label(field: SetupField) -> &'static str {
    match field {
        SetupField::TrustCurrentFolder => "trust_current_folder",
        SetupField::Model => "model",
        SetupField::ApiKey => "api_key",
        SetupField::Save => "save",
    }
}

fn setup_bool_label(enabled: bool) -> &'static str {
    if enabled { "on" } else { "off" }
}

fn render_setup_value_row(
    field: SetupField,
    selected_field: SetupField,
    label: &str,
    value: &str,
    action: Option<&str>,
) -> String {
    if let Some(action) = action.filter(|_| field == selected_field) {
        format!(
            "{} {:<22}: {}  [{}]",
            if field == selected_field { ">" } else { " " },
            label,
            value,
            action
        )
    } else {
        format!(
            "{} {:<22}: {}",
            if field == selected_field { ">" } else { " " },
            label,
            value
        )
    }
}

fn render_setup_toggle_row(
    field: SetupField,
    selected_field: SetupField,
    label: &str,
    enabled: bool,
) -> String {
    render_setup_value_row(
        field,
        selected_field,
        label,
        setup_bool_label(enabled),
        None,
    )
}

fn render_setup_action_row(field: SetupField, selected_field: SetupField, label: &str) -> String {
    format!(
        "{} [{}]",
        if field == selected_field { ">" } else { " " },
        label
    )
}

fn validate_setup_state(state: &SetupState) -> Option<String> {
    if !state.trusted_current_folder {
        return Some("trust the current folder before starting termquill".to_owned());
    }
    if state.model.trim().is_empty() {
        return Some("model cannot be empty".to_owned());
    }
    if state.api_key.trim().is_empty() && env::var(TERMQUILL_API_KEY_ENV).is_err() {
        return Some(format!("provide api_key or export {TERMQUILL_API_KEY_ENV}"));
    }

    None
}

fn build_setup_root_config(state: &SetupState) -> Result<RootConfig> {
    if !state.trusted_current_folder {
        bail!("trust the current folder before starting termquill");
    }
    let model = state.model.trim();
    if model.is_empty() {
        bail!("model cannot be empty");
    }
    if state.api_key.trim().is_empty() && env::var(TERMQUILL_API_KEY_ENV).is_err() {
        bail!("provide api_key or export {TERMQUILL_API_KEY_ENV}");
    }

    let provider_config = DeepSeekProviderConfig {
        base_url: "https://api.deepseek.com".to_owned(),
        beta_base_url: "https://api.deepseek.com/beta".to_owned(),
        anthropic_base_url: "https://api.deepseek.com/anthropic".to_owned(),
        model: model.to_owned(),
        api_key: (!state.api_key.trim().is_empty()).then(|| state.api_key.clone()),
        user_id_strategy: Some("stable_per_end_user".to_owned()),
        strict_tools_mode: StrictToolsMode::Auto,
        fim_model: "deepseek-v4-pro".to_owned(),
        request_timeout_secs: 120,
    };

    let provider_value = serialize_deepseek_provider_value(&provider_config)?;
    Ok(RootConfig {
        workspace: WorkspaceConfig {
            root: ".".to_owned(),
        },
        session: SessionConfig {
            log_dir: ".termquill/sessions".to_owned(),
        },
        agent: AgentConfig {
            provider: "deepseek".to_owned(),
            model: model.to_owned(),
            max_turns: None,
            tool_timeout_secs: 30,
        },
        permission: PermissionConfig::default(),
        memory: MemoryConfig { enabled: true },
        compaction: CompactionConfig {
            enabled: true,
            soft_threshold_ratio: 0.5,
            hard_threshold_ratio: 0.8,
            context_window_tokens: Some(128000),
            tail_messages: 6,
        },
        code_intelligence: Default::default(),
        providers: std::collections::BTreeMap::from([("deepseek".to_owned(), provider_value)]),
        mcp_servers: Vec::new(),
    })
}
