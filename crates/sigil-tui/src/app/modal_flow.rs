#[cfg(not(any(test, coverage)))]
use std::{sync::mpsc, thread};

use super::*;
#[cfg(not(any(test, coverage)))]
use crate::provider_status::fetch_remote_model_ids;
use crate::slash::SLASH_COMMANDS;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ModelPickerTarget {
    Setup,
    Provider,
    ProviderFim,
}

impl ModelPickerTarget {
    fn title(self) -> &'static str {
        match self {
            Self::Setup | Self::Provider => "Model",
            Self::ProviderFim => "FIM Model",
        }
    }

    fn summary(self) -> &'static str {
        match self {
            Self::Setup | Self::Provider => "Choose a known model. Esc to type your own.",
            Self::ProviderFim => "Choose FIM model. Esc to type your own.",
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct ModelPickerState {
    pub(super) target: ModelPickerTarget,
    pub(super) current: String,
    pub(super) options: Vec<String>,
    pub(super) selected: usize,
}

#[derive(Debug)]
pub(super) struct ModelPickerRefresh {
    pub(super) target: ModelPickerTarget,
    pub(super) current: String,
    pub(super) base_url: String,
    pub(super) result: Result<Vec<String>, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SecretInputTarget {
    SetupApiKey,
    ConfigProviderApiKey,
}

impl SecretInputTarget {
    fn title(self) -> &'static str {
        match self {
            Self::SetupApiKey | Self::ConfigProviderApiKey => "API Key",
        }
    }

    fn summary(self) -> &'static str {
        match self {
            Self::SetupApiKey => "Saved with setup. SIGIL_API_KEY can override at runtime.",
            Self::ConfigProviderApiKey => "Saved on Ctrl-S. SIGIL_API_KEY can override at runtime.",
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct SecretInputState {
    pub(super) target: SecretInputTarget,
    pub(super) buffer: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TextInputTarget {
    SetupModel,
    ConfigField(ConfigField),
}

impl TextInputTarget {
    fn title(self) -> &'static str {
        match self {
            Self::SetupModel => "Model ID",
            Self::ConfigField(field) => field.display_label(),
        }
    }

    fn summary(self) -> &'static str {
        match self {
            Self::SetupModel => "Custom model id.",
            Self::ConfigField(field) => field.help_text(),
        }
    }

    fn prompt_label(self) -> &'static str {
        match self {
            Self::SetupModel => "model",
            Self::ConfigField(_) => "value",
        }
    }

    fn config_key(self) -> Option<&'static str> {
        match self {
            Self::SetupModel => None,
            Self::ConfigField(field) => Some(field.label()),
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct TextInputState {
    pub(super) target: TextInputTarget,
    pub(super) buffer: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ElicitationFieldKind {
    String,
    Number,
    Integer,
    Boolean,
    Enum { values: Vec<String> },
}

#[derive(Debug, Clone)]
pub(super) struct ElicitationFieldState {
    pub(super) name: String,
    pub(super) label: String,
    pub(super) description: Option<String>,
    pub(super) required: bool,
    pub(super) kind: ElicitationFieldKind,
    pub(super) buffer: String,
}

#[derive(Debug)]
pub(super) struct McpElicitationModalState {
    pub(super) request: McpElicitationRequest,
    pub(super) fields: Vec<ElicitationFieldState>,
    pub(super) selected: usize,
    response_tx: Option<crate::runner::McpElicitationResponseTx>,
}

impl McpElicitationModalState {
    fn send_response(&mut self, response: McpElicitationResponse) {
        if let Some(response_tx) = self.response_tx.take() {
            let _ = response_tx.send(response);
        }
    }
}

impl Drop for McpElicitationModalState {
    fn drop(&mut self) {
        self.send_response(McpElicitationResponse::cancel());
    }
}

#[derive(Debug)]
pub(super) enum ModalState {
    ModelPicker(ModelPickerState),
    SecretInput(SecretInputState),
    TextInput(TextInputState),
    McpElicitation(McpElicitationModalState),
    KeyboardHelp,
}

#[derive(Debug, Clone)]
pub(super) enum ModalOutcome {
    None,
    Dismissed(String),
    ModelSelected {
        target: ModelPickerTarget,
        value: String,
    },
    SecretSubmitted {
        target: SecretInputTarget,
        value: String,
    },
    TextSubmitted {
        target: TextInputTarget,
        value: String,
    },
}

impl AppState {
    pub fn modal_title(&self) -> Option<&'static str> {
        match self.modal_state.as_ref()? {
            ModalState::ModelPicker(state) => Some(state.target.title()),
            ModalState::SecretInput(state) => Some(state.target.title()),
            ModalState::TextInput(state) => Some(state.target.title()),
            ModalState::McpElicitation(_) => Some("MCP Elicitation"),
            ModalState::KeyboardHelp => Some("Keyboard Help"),
        }
    }

    pub fn modal_lines(&self) -> Vec<String> {
        match self.modal_state.as_ref() {
            Some(ModalState::ModelPicker(state)) => {
                let mut lines = vec![
                    state.target.summary().to_owned(),
                    "Up/Down choose  Enter apply  F2 save  F3 save+close  Esc cancel".to_owned(),
                    String::new(),
                ];
                for (index, option) in state.options.iter().enumerate() {
                    let marker = if index == state.selected { ">" } else { " " };
                    let suffix = if option == &state.current {
                        "  [current]"
                    } else {
                        ""
                    };
                    lines.push(format!("{marker} {option}{suffix}"));
                }
                lines
            }
            Some(ModalState::SecretInput(state)) => vec![
                state.target.summary().to_owned(),
                "key: api_key".to_owned(),
                "Enter apply  F2 save  F3 save+close  Esc cancel".to_owned(),
                String::new(),
                format!("api_key: {}|", "*".repeat(state.buffer.chars().count())),
            ],
            Some(ModalState::TextInput(state)) => {
                let mut lines = vec![state.target.summary().to_owned()];
                if let Some(key) = state.target.config_key() {
                    lines.push(format!("key: {key}"));
                }
                lines.extend([
                    "Enter apply  F2 save  F3 save+close  Esc cancel".to_owned(),
                    String::new(),
                    format!("{}: {}|", state.target.prompt_label(), state.buffer),
                ]);
                lines
            }
            Some(ModalState::McpElicitation(state)) => {
                let mut lines = vec![
                    state.request.message.clone(),
                    format!("server: {}", state.request.server_name),
                    format!("fields: {}", state.fields.len()),
                    "Up/Down field  Left/Right option  Space toggle  Enter accept  Ctrl-D decline  Esc cancel".to_owned(),
                    String::new(),
                ];
                for (index, field) in state.fields.iter().enumerate() {
                    let required = if field.required { " *" } else { "" };
                    let value = elicitation_field_display_value(field);
                    if index == state.selected {
                        lines.push(format!("{}{}: {}|", field.label, required, value));
                    } else {
                        lines.push(format!("{}{}: {}", field.label, required, value));
                    }
                }
                if let Some(field) = state.fields.get(state.selected)
                    && let Some(description) = &field.description
                {
                    lines.push(String::new());
                    lines.push(format!("selected: {description}"));
                }
                lines
            }
            Some(ModalState::KeyboardHelp) => {
                let mut lines = keyboard_help_lines(self.has_tool_cards());
                lines.push(String::new());
                lines.push("Slash commands".to_owned());
                lines.extend(metadata_slash_help_lines());
                let metadata_slash_commands = metadata_slash_commands().collect::<Vec<_>>();
                lines.extend(SLASH_COMMANDS.iter().filter_map(|spec| {
                    if metadata_slash_commands.contains(&spec.canonical) {
                        return None;
                    }
                    let suffix = if spec.aliases.is_empty() {
                        String::new()
                    } else {
                        format!(" (aliases: {})", spec.aliases.join(", "))
                    };
                    Some(format!(
                        "{}: {}{}",
                        spec.canonical, spec.description, suffix
                    ))
                }));
                lines.push(String::new());
                lines.push("Use / or 、 to open the command palette.".to_owned());
                lines.push("Enter or Esc closes this help.".to_owned());
                lines
            }
            None => Vec::new(),
        }
    }

    pub fn modal_input_cursor(&self) -> Option<(String, usize, usize)> {
        match self.modal_state.as_ref()? {
            ModalState::SecretInput(state) => {
                Some(("api_key".to_owned(), state.buffer.chars().count(), 4))
            }
            ModalState::TextInput(state) => {
                let line_index = if state.target.config_key().is_some() {
                    4
                } else {
                    3
                };
                Some((
                    state.target.prompt_label().to_owned(),
                    state.buffer.chars().count(),
                    line_index,
                ))
            }
            ModalState::McpElicitation(state) => {
                let field = state.fields.get(state.selected)?;
                Some((
                    field.label.clone(),
                    elicitation_field_display_value(field).chars().count(),
                    5 + state.selected,
                ))
            }
            ModalState::ModelPicker(_) => None,
            ModalState::KeyboardHelp => None,
        }
    }

    pub(super) fn open_model_picker(&mut self, target: ModelPickerTarget, current: &str) {
        let options = build_model_picker_options(current, Vec::new());
        let selected = options
            .iter()
            .position(|option| option == current)
            .unwrap_or(0);
        self.modal_state = Some(ModalState::ModelPicker(ModelPickerState {
            target,
            current: current.to_owned(),
            options,
            selected,
        }));
        let notice = self.schedule_model_picker_refresh(target, current);
        self.last_notice = Some(notice);
    }

    fn schedule_model_picker_refresh(
        &mut self,
        target: ModelPickerTarget,
        current: &str,
    ) -> String {
        self.model_picker_refresh_rx = None;
        #[cfg(any(test, coverage))]
        {
            let _ = (target, current);
            "using local model list".to_owned()
        }
        #[cfg(not(any(test, coverage)))]
        {
            let provider_config = match self
                .provider_config_for_model_picker(target, current)
                .resolved()
            {
                Ok(config) => config,
                Err(error) => return format!("model list unavailable: {error}"),
            };
            let base_url = provider_config.base_url.clone();
            let (tx, rx) = mpsc::channel();
            self.model_picker_refresh_rx = Some(rx);
            let current = current.to_owned();
            let notice = format!("loading provider model list ({base_url})");
            thread::spawn(move || {
                let result =
                    fetch_remote_model_ids(&provider_config).map_err(|error| format!("{error:#}"));
                let _ = tx.send(ModelPickerRefresh {
                    target,
                    current,
                    base_url,
                    result,
                });
            });
            notice
        }
    }

    pub(super) fn apply_model_picker_refresh(&mut self, refresh: ModelPickerRefresh) -> bool {
        let mut notice = None;
        if let Some(ModalState::ModelPicker(state)) = self.modal_state.as_mut() {
            if state.target != refresh.target || state.current != refresh.current {
                return false;
            }
            match refresh.result {
                Ok(remote) if !remote.is_empty() => {
                    let selected_value = state
                        .options
                        .get(state.selected)
                        .cloned()
                        .unwrap_or_else(|| state.current.clone());
                    state.options = build_model_picker_options(&state.current, remote);
                    state.selected = state
                        .options
                        .iter()
                        .position(|option| option == &selected_value)
                        .or_else(|| {
                            state
                                .options
                                .iter()
                                .position(|option| option == &state.current)
                        })
                        .unwrap_or(0);
                    notice = Some(format!("loaded provider model list ({})", refresh.base_url));
                }
                Ok(_) => {
                    notice = Some("using local model list".to_owned());
                }
                Err(error) => {
                    notice = Some(format!("using local model list: {error}"));
                }
            }
        }
        if let Some(notice) = notice {
            self.last_notice = Some(notice.clone());
            self.push_event("model_list", notice);
            return true;
        }
        false
    }

    #[cfg_attr(coverage, allow(dead_code))]
    fn provider_config_for_model_picker(
        &self,
        target: ModelPickerTarget,
        current: &str,
    ) -> DeepSeekProviderConfig {
        if let Some(state) = &self.config_state {
            return DeepSeekProviderConfig {
                base_url: non_empty_or(&state.draft.provider_base_url, "https://api.deepseek.com"),
                beta_base_url: non_empty_or(
                    &state.draft.provider_beta_base_url,
                    "https://api.deepseek.com/beta",
                ),
                anthropic_base_url: non_empty_or(
                    &state.draft.provider_anthropic_base_url,
                    "https://api.deepseek.com/anthropic",
                ),
                model: match target {
                    ModelPickerTarget::ProviderFim => state.draft.provider_model.clone(),
                    _ => current.trim().to_owned(),
                },
                api_key: (!state.draft.provider_api_key.trim().is_empty())
                    .then(|| state.draft.provider_api_key.trim().to_owned()),
                user_id_strategy: (!state.draft.provider_user_id_strategy.trim().is_empty())
                    .then(|| state.draft.provider_user_id_strategy.trim().to_owned()),
                strict_tools_mode: state.draft.provider_strict_tools_mode,
                fim_model: match target {
                    ModelPickerTarget::ProviderFim => current.trim().to_owned(),
                    _ => state.draft.provider_fim_model.clone(),
                },
                request_timeout_secs: state
                    .draft
                    .provider_request_timeout_secs
                    .trim()
                    .parse::<u64>()
                    .ok()
                    .filter(|value| *value > 0)
                    .unwrap_or(120),
            };
        }

        if let Some(state) = &self.setup_state {
            let mut provider_config = default_deepseek_provider_config(current);
            provider_config.model = current.trim().to_owned();
            provider_config.api_key =
                (!state.api_key.trim().is_empty()).then(|| state.api_key.trim().to_owned());
            return provider_config;
        }

        self.config_snapshot
            .as_ref()
            .and_then(load_deepseek_provider_config)
            .unwrap_or_else(|| default_deepseek_provider_config(current))
    }

    pub(super) fn open_secret_input(&mut self, target: SecretInputTarget, current: &str) {
        self.modal_state = Some(ModalState::SecretInput(SecretInputState {
            target,
            buffer: current.to_owned(),
        }));
        self.last_notice = Some(format!("editing {}", target.title().to_lowercase()));
    }

    pub(super) fn open_secret_input_with_char(
        &mut self,
        target: SecretInputTarget,
        character: char,
    ) {
        self.modal_state = Some(ModalState::SecretInput(SecretInputState {
            target,
            buffer: character.to_string(),
        }));
        self.last_notice = Some(format!("editing {}", target.title().to_lowercase()));
    }

    pub(super) fn open_text_input(&mut self, target: TextInputTarget, current: &str) {
        self.modal_state = Some(ModalState::TextInput(TextInputState {
            target,
            buffer: current.to_owned(),
        }));
        self.last_notice = Some(format!("editing {}", target.prompt_label()));
    }

    pub(super) fn open_text_input_with_char(&mut self, target: TextInputTarget, character: char) {
        self.modal_state = Some(ModalState::TextInput(TextInputState {
            target,
            buffer: character.to_string(),
        }));
        self.last_notice = Some(format!("editing {}", target.prompt_label()));
    }

    pub(super) fn open_keyboard_help(&mut self) {
        self.modal_state = Some(ModalState::KeyboardHelp);
        self.last_notice = Some("keyboard help".to_owned());
    }

    pub(super) fn open_mcp_elicitation(
        &mut self,
        request: McpElicitationRequest,
        response_tx: crate::runner::McpElicitationResponseTx,
    ) {
        let fields = elicitation_fields_from_schema(&request.requested_schema);
        let server_name = request.server_name.clone();
        self.modal_state = Some(ModalState::McpElicitation(McpElicitationModalState {
            request,
            fields,
            selected: 0,
            response_tx: Some(response_tx),
        }));
        self.active_pane = PaneFocus::Activity;
        self.last_notice = Some(format!("MCP server {server_name} requested input"));
        self.push_timeline(
            TimelineRole::Notice,
            format!("MCP server {server_name} requested input."),
        );
        self.push_event("mcp:elicitation", format!("request {server_name}"));
    }

    pub(super) fn handle_modal_key_event(&mut self, key: KeyEvent) -> ModalOutcome {
        if matches!(self.modal_state, Some(ModalState::McpElicitation(_))) {
            return self.handle_mcp_elicitation_key_event(key);
        }

        let Some(modal_state) = self.modal_state.as_mut() else {
            return ModalOutcome::None;
        };

        match modal_state {
            ModalState::ModelPicker(state) => match key.code {
                KeyCode::Esc => {
                    self.modal_state = None;
                    ModalOutcome::Dismissed("closed picker".to_owned())
                }
                KeyCode::Up => {
                    if state.selected == 0 {
                        state.selected = state.options.len().saturating_sub(1);
                    } else {
                        state.selected -= 1;
                    }
                    self.last_notice = Some(format!(
                        "{} {}",
                        state.target.title().to_lowercase(),
                        state
                            .options
                            .get(state.selected)
                            .cloned()
                            .unwrap_or_default()
                    ));
                    ModalOutcome::None
                }
                KeyCode::Down => {
                    if !state.options.is_empty() {
                        state.selected = (state.selected + 1) % state.options.len();
                    }
                    self.last_notice = Some(format!(
                        "{} {}",
                        state.target.title().to_lowercase(),
                        state
                            .options
                            .get(state.selected)
                            .cloned()
                            .unwrap_or_default()
                    ));
                    ModalOutcome::None
                }
                KeyCode::Enter => {
                    let Some(value) = state.options.get(state.selected).cloned() else {
                        self.modal_state = None;
                        return ModalOutcome::Dismissed("closed picker".to_owned());
                    };
                    let target = state.target;
                    self.modal_state = None;
                    ModalOutcome::ModelSelected { target, value }
                }
                _ => ModalOutcome::None,
            },
            ModalState::SecretInput(state) => match key.code {
                KeyCode::Esc => {
                    self.modal_state = None;
                    ModalOutcome::Dismissed("closed secret input".to_owned())
                }
                KeyCode::Backspace => {
                    let _ = state.buffer.pop();
                    self.last_notice = Some("editing api key".to_owned());
                    ModalOutcome::None
                }
                KeyCode::Enter => {
                    let target = state.target;
                    let value = state.buffer.clone();
                    self.modal_state = None;
                    ModalOutcome::SecretSubmitted { target, value }
                }
                KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    state.buffer.push(character);
                    self.last_notice = Some("editing api key".to_owned());
                    ModalOutcome::None
                }
                _ => ModalOutcome::None,
            },
            ModalState::TextInput(state) => match key.code {
                KeyCode::Esc => {
                    self.modal_state = None;
                    ModalOutcome::Dismissed("closed text input".to_owned())
                }
                KeyCode::Backspace => {
                    let _ = state.buffer.pop();
                    self.last_notice = Some(format!("editing {}", state.target.prompt_label()));
                    ModalOutcome::None
                }
                KeyCode::Enter => {
                    let target = state.target;
                    let value = state.buffer.clone();
                    self.modal_state = None;
                    ModalOutcome::TextSubmitted { target, value }
                }
                KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if !text_input_target_accepts_char(state.target, character) {
                        self.last_notice = Some(format!(
                            "{} does not accept '{character}'",
                            state.target.prompt_label()
                        ));
                        return ModalOutcome::None;
                    }
                    state.buffer.push(character);
                    self.last_notice = Some(format!("editing {}", state.target.prompt_label()));
                    ModalOutcome::None
                }
                _ => ModalOutcome::None,
            },
            ModalState::McpElicitation(_) => ModalOutcome::None,
            ModalState::KeyboardHelp => match key.code {
                KeyCode::Esc | KeyCode::Enter => {
                    self.modal_state = None;
                    ModalOutcome::Dismissed("closed keyboard help".to_owned())
                }
                _ => ModalOutcome::None,
            },
        }
    }

    pub(super) fn submit_modal(&mut self) -> ModalOutcome {
        let Some(modal_state) = self.modal_state.as_ref() else {
            return ModalOutcome::None;
        };

        match modal_state {
            ModalState::ModelPicker(state) => {
                let Some(value) = state.options.get(state.selected).cloned() else {
                    self.modal_state = None;
                    return ModalOutcome::Dismissed("closed picker".to_owned());
                };
                let target = state.target;
                self.modal_state = None;
                ModalOutcome::ModelSelected { target, value }
            }
            ModalState::SecretInput(state) => {
                let target = state.target;
                let value = state.buffer.clone();
                self.modal_state = None;
                ModalOutcome::SecretSubmitted { target, value }
            }
            ModalState::TextInput(state) => {
                let target = state.target;
                let value = state.buffer.clone();
                self.modal_state = None;
                ModalOutcome::TextSubmitted { target, value }
            }
            ModalState::McpElicitation(_) => self.accept_mcp_elicitation(),
            ModalState::KeyboardHelp => {
                self.modal_state = None;
                ModalOutcome::Dismissed("closed keyboard help".to_owned())
            }
        }
    }

    pub(super) fn apply_modal_outcome(&mut self, outcome: ModalOutcome) {
        match outcome {
            ModalOutcome::None => {}
            ModalOutcome::Dismissed(message) => {
                self.last_notice = Some(message);
            }
            ModalOutcome::ModelSelected { target, value } => match target {
                ModelPickerTarget::Setup => {
                    if let Some(state) = self.setup_state.as_mut() {
                        state.model = value.clone();
                    }
                    self.last_notice = Some(format!("selected model {value}"));
                }
                ModelPickerTarget::Provider => {
                    if let Some(state) = self.config_state.as_mut() {
                        state.draft.provider_model = value.clone();
                        state.dirty = true;
                    }
                    self.last_notice = Some(format!("selected model {value}"));
                }
                ModelPickerTarget::ProviderFim => {
                    if let Some(state) = self.config_state.as_mut() {
                        state.draft.provider_fim_model = value.clone();
                        state.dirty = true;
                    }
                    self.last_notice = Some(format!("selected fim model {value}"));
                }
            },
            ModalOutcome::SecretSubmitted { target, value } => match target {
                SecretInputTarget::SetupApiKey => {
                    if let Some(state) = self.setup_state.as_mut() {
                        state.api_key = value;
                    }
                    self.last_notice = Some("updated api key".to_owned());
                }
                SecretInputTarget::ConfigProviderApiKey => {
                    if let Some(state) = self.config_state.as_mut() {
                        state.draft.provider_api_key = value;
                        state.dirty = true;
                    }
                    self.last_notice = Some("updated api key".to_owned());
                }
            },
            ModalOutcome::TextSubmitted { target, value } => match target {
                TextInputTarget::SetupModel => {
                    if let Some(state) = self.setup_state.as_mut() {
                        state.model = value.clone();
                    }
                    self.last_notice = Some(format!("updated model {value}"));
                }
                TextInputTarget::ConfigField(field) => {
                    if let Some(state) = self.config_state.as_mut()
                        && let Some(target) = state.field_text_value_mut(field)
                    {
                        let changed = *target != value;
                        *target = value.clone();
                        if changed {
                            state.dirty = true;
                        }
                    }
                    self.last_notice = Some(format!("updated {}", field.label()));
                }
            },
        }
    }

    fn handle_mcp_elicitation_key_event(&mut self, key: KeyEvent) -> ModalOutcome {
        match key.code {
            KeyCode::Esc => self.finish_mcp_elicitation(McpElicitationResponse::cancel()),
            KeyCode::Char('d') | KeyCode::Char('D')
                if key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.finish_mcp_elicitation(McpElicitationResponse::decline())
            }
            KeyCode::Enter => self.accept_mcp_elicitation(),
            KeyCode::Up => {
                if let Some(ModalState::McpElicitation(state)) = self.modal_state.as_mut()
                    && !state.fields.is_empty()
                {
                    state.selected = if state.selected == 0 {
                        state.fields.len() - 1
                    } else {
                        state.selected - 1
                    };
                    self.last_notice = Some(format!(
                        "editing {}",
                        state
                            .fields
                            .get(state.selected)
                            .map(|field| field.label.as_str())
                            .unwrap_or("field")
                    ));
                }
                ModalOutcome::None
            }
            KeyCode::Down => {
                if let Some(ModalState::McpElicitation(state)) = self.modal_state.as_mut()
                    && !state.fields.is_empty()
                {
                    state.selected = (state.selected + 1) % state.fields.len();
                    self.last_notice = Some(format!(
                        "editing {}",
                        state
                            .fields
                            .get(state.selected)
                            .map(|field| field.label.as_str())
                            .unwrap_or("field")
                    ));
                }
                ModalOutcome::None
            }
            KeyCode::Left => {
                self.cycle_selected_elicitation_option(false);
                ModalOutcome::None
            }
            KeyCode::Right => {
                self.cycle_selected_elicitation_option(true);
                ModalOutcome::None
            }
            KeyCode::Char(' ') => {
                self.toggle_selected_elicitation_bool();
                ModalOutcome::None
            }
            KeyCode::Backspace => {
                if let Some(field) = self.selected_elicitation_field_mut()
                    && elicitation_field_accepts_text(field)
                {
                    let _ = field.buffer.pop();
                    self.last_notice = Some(format!("editing {}", field.label));
                }
                ModalOutcome::None
            }
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(field) = self.selected_elicitation_field_mut() {
                    match field.kind {
                        ElicitationFieldKind::Boolean => {
                            if matches!(character, 't' | 'T' | 'y' | 'Y' | '1') {
                                field.buffer = "true".to_owned();
                            } else if matches!(character, 'f' | 'F' | 'n' | 'N' | '0') {
                                field.buffer = "false".to_owned();
                            }
                        }
                        ElicitationFieldKind::Enum { .. } => {}
                        ElicitationFieldKind::Number | ElicitationFieldKind::Integer => {
                            if matches!(character, '0'..='9' | '-' | '+' | '.' | 'e' | 'E') {
                                field.buffer.push(character);
                            }
                        }
                        ElicitationFieldKind::String => {
                            if !character.is_control() {
                                field.buffer.push(character);
                            }
                        }
                    }
                    self.last_notice = Some(format!("editing {}", field.label));
                }
                ModalOutcome::None
            }
            _ => ModalOutcome::None,
        }
    }

    fn accept_mcp_elicitation(&mut self) -> ModalOutcome {
        let Some(ModalState::McpElicitation(state)) = self.modal_state.as_ref() else {
            return ModalOutcome::None;
        };
        let content = match elicitation_content_from_fields(&state.fields) {
            Ok(content) => content,
            Err(message) => {
                self.last_notice = Some(message);
                return ModalOutcome::None;
            }
        };
        self.finish_mcp_elicitation(McpElicitationResponse::accept(content))
    }

    fn finish_mcp_elicitation(&mut self, response: McpElicitationResponse) -> ModalOutcome {
        let Some(ModalState::McpElicitation(mut state)) = self.modal_state.take() else {
            return ModalOutcome::None;
        };
        let server_name = state.request.server_name.clone();
        let notice = match response.action {
            sigil_runtime::McpElicitationAction::Accept => {
                format!("submitted MCP input to {server_name}")
            }
            sigil_runtime::McpElicitationAction::Decline => {
                format!("declined MCP input request from {server_name}")
            }
            sigil_runtime::McpElicitationAction::Cancel => {
                format!("cancelled MCP input request from {server_name}")
            }
        };
        state.send_response(response);
        self.active_pane = PaneFocus::Composer;
        self.push_event("mcp:elicitation", notice.clone());
        ModalOutcome::Dismissed(notice)
    }

    fn selected_elicitation_field_mut(&mut self) -> Option<&mut ElicitationFieldState> {
        let ModalState::McpElicitation(state) = self.modal_state.as_mut()? else {
            return None;
        };
        state.fields.get_mut(state.selected)
    }

    fn cycle_selected_elicitation_option(&mut self, forward: bool) {
        let Some(field) = self.selected_elicitation_field_mut() else {
            return;
        };
        match &field.kind {
            ElicitationFieldKind::Boolean => {
                field.buffer = if field.buffer == "true" {
                    "false".to_owned()
                } else {
                    "true".to_owned()
                };
            }
            ElicitationFieldKind::Enum { values } if !values.is_empty() => {
                let current = values
                    .iter()
                    .position(|value| value == &field.buffer)
                    .unwrap_or(0);
                let next = if forward {
                    (current + 1) % values.len()
                } else if current == 0 {
                    values.len() - 1
                } else {
                    current - 1
                };
                field.buffer = values[next].clone();
            }
            _ => {}
        }
        self.last_notice = Some(format!("editing {}", field.label));
    }

    fn toggle_selected_elicitation_bool(&mut self) {
        let Some(field) = self.selected_elicitation_field_mut() else {
            return;
        };
        if matches!(field.kind, ElicitationFieldKind::Boolean) {
            field.buffer = if field.buffer == "true" {
                "false".to_owned()
            } else {
                "true".to_owned()
            };
            self.last_notice = Some(format!("editing {}", field.label));
        }
    }
}

fn text_input_target_accepts_char(target: TextInputTarget, character: char) -> bool {
    match target {
        TextInputTarget::SetupModel => !character.is_control(),
        TextInputTarget::ConfigField(field) => config_field_accepts_char(field, character),
    }
}

fn elicitation_fields_from_schema(schema: &serde_json::Value) -> Vec<ElicitationFieldState> {
    let required = schema
        .get("required")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect::<std::collections::BTreeSet<_>>()
        })
        .unwrap_or_default();
    let Some(properties) = schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
    else {
        return Vec::new();
    };
    properties
        .iter()
        .map(|(name, property)| {
            let enum_values = property
                .get("enum")
                .and_then(serde_json::Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .map(ToOwned::to_owned)
                        .collect::<Vec<_>>()
                })
                .filter(|values| !values.is_empty());
            let kind = if let Some(values) = enum_values {
                ElicitationFieldKind::Enum { values }
            } else {
                match property
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("string")
                {
                    "boolean" => ElicitationFieldKind::Boolean,
                    "integer" => ElicitationFieldKind::Integer,
                    "number" => ElicitationFieldKind::Number,
                    _ => ElicitationFieldKind::String,
                }
            };
            let default_value = elicitation_default_value(property, &kind);
            ElicitationFieldState {
                name: name.clone(),
                label: property
                    .get("title")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(name)
                    .to_owned(),
                description: property
                    .get("description")
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned),
                required: required.contains(name.as_str()),
                kind,
                buffer: default_value,
            }
        })
        .collect()
}

fn elicitation_default_value(property: &serde_json::Value, kind: &ElicitationFieldKind) -> String {
    if let Some(default) = property.get("default") {
        match default {
            serde_json::Value::String(value) => return value.clone(),
            serde_json::Value::Bool(value) => return value.to_string(),
            serde_json::Value::Number(value) => return value.to_string(),
            _ => {}
        }
    }
    match kind {
        ElicitationFieldKind::Boolean => "false".to_owned(),
        ElicitationFieldKind::Enum { values } => values.first().cloned().unwrap_or_default(),
        _ => String::new(),
    }
}

fn elicitation_field_display_value(field: &ElicitationFieldState) -> String {
    match field.kind {
        ElicitationFieldKind::Boolean if field.buffer == "true" => "true".to_owned(),
        ElicitationFieldKind::Boolean => "false".to_owned(),
        _ => field.buffer.clone(),
    }
}

fn elicitation_field_accepts_text(field: &ElicitationFieldState) -> bool {
    matches!(
        field.kind,
        ElicitationFieldKind::String | ElicitationFieldKind::Number | ElicitationFieldKind::Integer
    )
}

fn elicitation_content_from_fields(
    fields: &[ElicitationFieldState],
) -> std::result::Result<serde_json::Value, String> {
    let mut object = serde_json::Map::new();
    for field in fields {
        let value = field.buffer.trim();
        if field.required
            && value.is_empty()
            && !matches!(field.kind, ElicitationFieldKind::Boolean)
        {
            return Err(format!("{} is required", field.label));
        }
        match field.kind {
            ElicitationFieldKind::String | ElicitationFieldKind::Enum { .. } => {
                if !value.is_empty() || field.required {
                    object.insert(
                        field.name.clone(),
                        serde_json::Value::String(value.to_owned()),
                    );
                }
            }
            ElicitationFieldKind::Boolean => {
                object.insert(
                    field.name.clone(),
                    serde_json::Value::Bool(field.buffer == "true"),
                );
            }
            ElicitationFieldKind::Integer => {
                if value.is_empty() {
                    continue;
                }
                let parsed = value
                    .parse::<i64>()
                    .map_err(|_| format!("{} must be an integer", field.label))?;
                object.insert(
                    field.name.clone(),
                    serde_json::Value::Number(serde_json::Number::from(parsed)),
                );
            }
            ElicitationFieldKind::Number => {
                if value.is_empty() {
                    continue;
                }
                let parsed = value
                    .parse::<f64>()
                    .map_err(|_| format!("{} must be a number", field.label))?;
                let number = serde_json::Number::from_f64(parsed)
                    .ok_or_else(|| format!("{} must be a finite number", field.label))?;
                object.insert(field.name.clone(), serde_json::Value::Number(number));
            }
        }
    }
    Ok(serde_json::Value::Object(object))
}

#[cfg(test)]
#[path = "tests/modal_flow_detail_tests.rs"]
mod tests;
