use std::env;

use anyhow::{Result, bail};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use sigil_kernel::{
    AgentConfig, CompactionConfig, MemoryConfig, PermissionConfig, RootConfig, SessionConfig,
    WorkspaceConfig,
};
use sigil_runtime::{
    default_provider_config_fields, provider_api_key_env_name, set_provider_config_fields,
    supported_provider_name,
};

use super::{
    AppAction, AppState, SetupField, SetupState,
    formatting::persisted_root_config,
    modal_flow::{ModelPickerTarget, SecretInputTarget, TextInputTarget},
};

impl AppState {
    pub fn setup_lines(&self) -> Vec<String> {
        let Some(state) = &self.setup_state else {
            return Vec::new();
        };

        let mut lines = vec![
            "Quick setup".to_owned(),
            "[workspace]".to_owned(),
            "Saving starts Sigil and trusts the current folder.".to_owned(),
            String::new(),
            "[runtime]".to_owned(),
            render_setup_value_row(
                SetupField::Provider,
                state.selected_field,
                "provider",
                &state.provider_name,
                Some("Enter switch"),
            ),
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
            render_setup_action_row(
                SetupField::Save,
                state.selected_field,
                "trust folder, save and start",
            ),
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
        let env_name = state.api_key_env_name().unwrap_or("unavailable");
        lines.push(format!(
            "Tab move  Enter open/switch  Ctrl-S trust+save  Ctrl-C quit  env={env_name}"
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
                let state = self
                    .setup_state
                    .as_mut()
                    .expect("setup state was checked before setup key handling");
                state.selected_field = state.selected_field.next();
                self.last_notice = Some(format!(
                    "setup field {}",
                    setup_field_label(state.selected_field)
                ));
                return Ok(None);
            }
            KeyCode::BackTab | KeyCode::Up => {
                let state = self
                    .setup_state
                    .as_mut()
                    .expect("setup state was checked before setup key handling");
                state.selected_field = state.selected_field.previous();
                self.last_notice = Some(format!(
                    "setup field {}",
                    setup_field_label(state.selected_field)
                ));
                return Ok(None);
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Enter
                if matches!(selected_field, SetupField::Provider) =>
            {
                let state = self
                    .setup_state
                    .as_mut()
                    .expect("setup state was checked before setup key handling");
                state.cycle_provider();
                self.last_notice = Some(format!("provider -> {}", state.provider_name));
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

    pub(super) fn handle_setup_paste_text(&mut self, text: &str) {
        let Some(state) = self.setup_state.as_mut() else {
            return;
        };
        let value = text
            .chars()
            .filter(|character| !character.is_control())
            .collect::<String>();
        if value.is_empty() {
            return;
        }
        match state.selected_field {
            SetupField::Model => {
                state.model = value.clone();
                self.last_notice = Some(format!("updated model {value}"));
            }
            SetupField::ApiKey => {
                state.api_key = value;
                self.last_notice = Some("updated api key".to_owned());
            }
            SetupField::Provider | SetupField::Save => {}
        }
    }

    pub(super) fn complete_setup(&mut self) -> Result<Option<AppAction>> {
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
        SetupField::Provider => "provider",
        SetupField::Model => "model",
        SetupField::ApiKey => "api_key",
        SetupField::Save => "save",
    }
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

fn render_setup_action_row(field: SetupField, selected_field: SetupField, label: &str) -> String {
    format!(
        "{} [{}]",
        if field == selected_field { ">" } else { " " },
        label
    )
}

pub(super) fn validate_setup_state(state: &SetupState) -> Option<String> {
    if let Err(error) = supported_provider_name(&state.provider_name) {
        return Some(error.to_string());
    }
    if state.model.trim().is_empty() {
        return Some("model cannot be empty".to_owned());
    }
    let Some(env_name) = provider_api_key_env_name(&state.provider_name) else {
        return Some(format!("unsupported provider {}", state.provider_name));
    };
    if state.api_key.trim().is_empty() && env::var(env_name).is_err() {
        return Some(format!("provide api_key or export {env_name}"));
    }

    None
}

pub(super) fn build_setup_root_config(state: &SetupState) -> Result<RootConfig> {
    let provider_name = supported_provider_name(&state.provider_name)?;
    let model = state.model.trim();
    if model.is_empty() {
        bail!("model cannot be empty");
    }
    let env_name = provider_api_key_env_name(provider_name)
        .ok_or_else(|| anyhow::anyhow!("unsupported provider {provider_name}"))?;
    if state.api_key.trim().is_empty() && env::var(env_name).is_err() {
        bail!("provide api_key or export {env_name}");
    }

    let mut root_config = RootConfig {
        workspace: WorkspaceConfig {
            root: ".".to_owned(),
        },
        storage: Default::default(),
        session: SessionConfig::default(),
        agent: AgentConfig {
            provider: provider_name.to_owned(),
            model: model.to_owned(),
            max_turns: None,
            tool_timeout_secs: 30,
        },
        model_request: Default::default(),
        permission: PermissionConfig::default(),
        memory: MemoryConfig { enabled: true },
        skills: Default::default(),
        compaction: CompactionConfig {
            enabled: true,
            soft_threshold_ratio: 0.5,
            hard_threshold_ratio: 0.8,
            context_window_tokens: None,
            tail_messages: 6,
        },
        code_intelligence: Default::default(),
        terminal: Default::default(),
        execution: Default::default(),
        verification: Default::default(),
        appearance: Default::default(),
        task: Default::default(),
        providers: std::collections::BTreeMap::new(),
        web: Default::default(),
        mcp_servers: Vec::new(),
    };
    let mut provider_fields = default_provider_config_fields(provider_name, model);
    provider_fields.api_key = state.api_key.trim().to_owned();
    set_provider_config_fields(&mut root_config, provider_name, &provider_fields, None)?;
    Ok(root_config)
}
