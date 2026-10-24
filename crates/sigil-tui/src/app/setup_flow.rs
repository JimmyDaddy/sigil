use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use sigil_kernel::{ConnectionId, ModelRef, RootConfig, SecretString};
use sigil_runtime::apply_new_install_orchestration_rollout;
#[cfg(not(test))]
use sigil_runtime::provider_connections::ConfiguredProviderCredentialStore;
use sigil_runtime::provider_connections::{
    ConfigPublishOutcome, ConnectionCredentialUpdate, ConnectionSaveDraft, CredentialRefConfig,
    PreparedCredential, ProviderConnectionConfig, ProviderFamily, ProviderProtocol,
    RootConfigPublisher, default_setup_root_config, load_provider_connections,
    materialize_root_config, provider_connection_template, save_connection_config,
    save_connection_config_replacing_invalid, save_connection_config_with_base_from_snapshot,
};

use super::{
    AppAction, AppState, SetupField, SetupState,
    formatting::persisted_root_config,
    modal_flow::{ModelPickerTarget, SecretInputTarget, TextInputTarget},
};
use crate::setup::{
    SETUP_PROVIDER_ORDER, SetupAction, SetupCredentialSource, SetupFrameIdentity,
    SetupRecoveryAction, SetupRecoveryFrameIdentity, SetupStep, StartupBlocker,
};
use crate::token_units::{
    MAX_OUTPUT_TOKEN_PRESETS, cycle_context_window_preset, cycle_token_preset,
    is_token_count_character, parse_optional_token_count, token_count_display,
};

#[derive(Debug, Default)]
pub(crate) struct SetupPresentation {
    pub(crate) lines: Vec<String>,
    pub(crate) fields: Vec<(usize, usize)>,
    pub(crate) actions: Vec<(SetupAction, usize)>,
    pub(crate) focused_line: Option<usize>,
}

impl SetupPresentation {
    fn field(
        &mut self,
        state: &SetupState,
        field: SetupField,
        label: &str,
        value: &str,
        hint: Option<&str>,
    ) {
        let row = self.lines.len();
        let selected = state.selected_action.is_none() && state.selected_field == field;
        self.fields.push((field.index(state.is_custom()), row));
        if selected {
            self.focused_line = Some(row);
        }
        let mut line = render_setup_value_row(field, state.selected_field, label, value, hint);
        if !selected && line.starts_with('>') {
            line.replace_range(..1, " ");
        }
        self.lines.push(line);
    }
    fn action(&mut self, state: &SetupState, action: SetupAction, label: &str) {
        let row = self.lines.len();
        let selected = state.selected_action == Some(action);
        self.actions.push((action, row));
        if selected {
            self.focused_line = Some(row);
        }
        self.lines
            .push(format!("{} [{label}]", if selected { ">" } else { " " }));
    }
}

impl AppState {
    pub(crate) fn setup_frame_identity(&self) -> Option<SetupFrameIdentity> {
        let state = self.setup_state.as_ref()?;
        (!state.startup_recovery_visible()).then(|| SetupFrameIdentity {
            step: state.selected_field.step(),
            provider_name: state.provider_name.clone(),
            draft_revision: state.draft_revision,
            model_options_open: state.model_options_open,
        })
    }

    pub(crate) fn setup_recovery_view(&self) -> Option<crate::surface::SetupRecoverySurface> {
        let state = self.setup_state.as_ref()?;
        state
            .startup_recovery_visible()
            .then(|| crate::surface::SetupRecoverySurface {
                diagnostics: state.startup_diagnostics_open,
                connection_changes_unsaved: state.startup_connection_changes_unsaved(),
                scroll_rows: state.startup_diagnostics_scroll,
                actions: if state.startup_diagnostics_open {
                    vec![SetupRecoveryAction::Back, SetupRecoveryAction::Quit]
                } else {
                    vec![
                        if state.startup_connection_changes_unsaved() {
                            SetupRecoveryAction::SaveChangesAndRetry
                        } else {
                            SetupRecoveryAction::Retry
                        },
                        SetupRecoveryAction::Diagnostics,
                        SetupRecoveryAction::EditConnection,
                        SetupRecoveryAction::Quit,
                    ]
                },
            })
    }

    pub(crate) fn setup_recovery_frame_identity(&self) -> Option<SetupRecoveryFrameIdentity> {
        let state = self.setup_state.as_ref()?;
        state
            .startup_recovery_visible()
            .then_some(SetupRecoveryFrameIdentity {
                diagnostics: state.startup_diagnostics_open,
                draft_revision: state.draft_revision,
                blocker: state.startup_blocker,
                connection_changes_unsaved: state.startup_connection_changes_unsaved(),
            })
    }

    pub(super) fn handle_setup_recovery_action(
        &mut self,
        action: SetupRecoveryAction,
    ) -> Result<Option<AppAction>> {
        if self
            .setup_recovery_view()
            .is_none_or(|view| !view.actions.contains(&action))
        {
            return Ok(None);
        }
        if matches!(
            action,
            SetupRecoveryAction::Retry | SetupRecoveryAction::SaveChangesAndRetry
        ) {
            return self.complete_setup();
        }
        let state = self
            .setup_state
            .as_mut()
            .expect("visible setup recovery has state");
        match action {
            SetupRecoveryAction::Diagnostics => state.startup_diagnostics_open = true,
            SetupRecoveryAction::Back => state.startup_diagnostics_open = false,
            SetupRecoveryAction::EditConnection => {
                state.startup_editing_connection = true;
                state.selected_field = SetupField::Provider;
            }
            SetupRecoveryAction::Quit => self.should_quit = true,
            SetupRecoveryAction::Retry | SetupRecoveryAction::SaveChangesAndRetry => {
                unreachable!("retry was handled above")
            }
        }
        state.startup_diagnostics_scroll = 0;
        Ok(None)
    }

    pub(super) fn scroll_setup_diagnostics(&mut self, upward: bool, amount: usize) {
        let Some(view) = self.setup_recovery_view().filter(|view| view.diagnostics) else {
            return;
        };
        let (max, _) = crate::ui::setup_recovery_scroll_bounds(
            self.terminal_width,
            self.terminal_height,
            &self.setup_lines(),
            &view,
        );
        let current = view.scroll_rows.min(max);
        if let Some(state) = self.setup_state.as_mut() {
            state.startup_diagnostics_scroll = if upward {
                current.saturating_sub(amount)
            } else {
                current.saturating_add(amount).min(max)
            };
        }
    }

    #[cfg(test)]
    pub(crate) fn setup_field_line_indices(&self) -> Vec<(usize, usize)> {
        self.setup_presentation().fields
    }

    pub fn setup_lines(&self) -> Vec<String> {
        self.setup_presentation().lines
    }

    pub(crate) fn setup_presentation(&self) -> SetupPresentation {
        let Some(state) = &self.setup_state else {
            return SetupPresentation::default();
        };
        if state.startup_recovery_visible() {
            return SetupPresentation {
                lines: setup_startup_recovery_lines(state),
                ..SetupPresentation::default()
            };
        }
        let mut view = SetupPresentation::default();
        let step = state.selected_field.step();
        let (number, title) = match step {
            SetupStep::Provider => (1, "Choose a provider"),
            SetupStep::Connection => (2, "Connect your account"),
            SetupStep::Model => (3, "Choose a model"),
            SetupStep::Review => (4, "Ready to start"),
        };
        view.lines
            .extend([format!("{number}/4 · {title}"), String::new()]);
        if step == SetupStep::Provider {
            for (index, name) in SETUP_PROVIDER_ORDER.iter().enumerate() {
                let row = view.lines.len();
                let selected = index == state.provider_index();
                view.fields.push((index, row));
                if selected {
                    view.focused_line = Some(row);
                }
                view.lines.push(format!(
                    "{} {}",
                    if selected { ">" } else { " " },
                    SetupState::provider_choice_label(name)
                ));
            }
            view.lines.push(SetupState::provider_choice_auth_summary(
                &state.provider_name,
            ));
        } else {
            view.lines
                .push(format!("Provider: {}", state.provider_label()));
            match step {
                SetupStep::Connection => {
                    if state.is_custom() {
                        view.field(
                            state,
                            SetupField::Protocol,
                            "protocol",
                            state.protocol.label(),
                            Some("←/→ change"),
                        );
                        view.field(
                            state,
                            SetupField::Endpoint,
                            "endpoint",
                            &state.base_url,
                            Some("Enter edit"),
                        );
                    }
                    view.field(
                        state,
                        SetupField::ApiKey,
                        "authentication",
                        &state.auth_summary(),
                        Some("←/→ source · Enter edit"),
                    );
                    view.action(state, SetupAction::Continue, "Continue to model");
                    view.action(state, SetupAction::Back, "Back");
                }
                SetupStep::Model => {
                    view.field(
                        state,
                        SetupField::Model,
                        "model",
                        &state.model,
                        Some("Enter choose · type ID"),
                    );
                    if state.model_options_open
                        || matches!(
                            state.selected_field,
                            SetupField::ContextWindow | SetupField::MaxOutputTokens
                        )
                    {
                        view.field(
                            state,
                            SetupField::ContextWindow,
                            "context window",
                            &token_count_display(&state.context_window_tokens, "automatic"),
                            Some("Enter edit"),
                        );
                        view.field(
                            state,
                            SetupField::MaxOutputTokens,
                            "max output tokens",
                            &token_count_display(&state.max_output_tokens, "automatic"),
                            Some("Enter edit"),
                        );
                    }
                    view.action(
                        state,
                        SetupAction::Options,
                        if state.model_options_open {
                            "Hide model options"
                        } else {
                            "Model options"
                        },
                    );
                    view.action(state, SetupAction::Continue, "Review and start");
                    view.action(state, SetupAction::Back, "Back");
                }
                SetupStep::Review => {
                    view.lines.extend([
                        format!("Model: {}", state.model),
                        format!("Authentication: {}", state.auth_summary()),
                    ]);
                    let label = if state.existing_config_repair_required() {
                        "Replace invalid config and start"
                    } else if state.valid_config_boot_retry_required() {
                        "Save changes and retry start"
                    } else {
                        "Trust this folder, save and start"
                    };
                    let row = view.lines.len();
                    view.fields
                        .push((SetupField::Save.index(state.is_custom()), row));
                    if state.selected_action.is_none() {
                        view.focused_line = Some(row);
                    }
                    view.lines.push(render_setup_action_row(
                        SetupField::Save,
                        if state.selected_action.is_none() {
                            SetupField::Save
                        } else {
                            SetupField::Model
                        },
                        label,
                    ));
                    view.action(state, SetupAction::Back, "Back to model");
                }
                SetupStep::Provider => unreachable!("provider is rendered separately"),
            }
        }
        append_setup_startup_recovery_lines(&mut view.lines, state);
        append_setup_save_error_lines(&mut view.lines, state);
        view.lines.push(String::new());
        view.lines.push(
            if state.startup_editing_connection {
                "↑/↓ move · Enter choose · Esc Startup · Ctrl-S review"
            } else if step == SetupStep::Provider {
                "↑/↓ choose · Enter continue · Ctrl-C quit"
            } else if step == SetupStep::Review {
                "↑/↓ move · Enter start · Esc back · Ctrl-C quit"
            } else {
                "↑/↓ move · Enter choose · Esc back · Ctrl-S review"
            }
            .to_owned(),
        );
        view
    }

    fn move_setup_focus(&mut self, forward: bool) {
        let view = self.setup_presentation();
        let Some(state) = self.setup_state.as_mut() else {
            return;
        };
        if state.selected_field == SetupField::Provider {
            state.selected_field = if state.is_custom() {
                SetupField::Protocol
            } else {
                SetupField::ApiKey
            };
            state.selected_action = None;
            return;
        }
        let mut targets = view
            .fields
            .into_iter()
            .map(|(index, row)| (row, Some(index), None))
            .chain(
                view.actions
                    .into_iter()
                    .map(|(action, row)| (row, None, Some(action))),
            )
            .collect::<Vec<_>>();
        targets.sort_by_key(|(row, _, _)| *row);
        let current = targets
            .iter()
            .position(|(_, field, action)| {
                *action == state.selected_action
                    && (action.is_some()
                        || *field == Some(state.selected_field.index(state.is_custom())))
            })
            .unwrap_or(0);
        if targets.is_empty() {
            return;
        }
        let next = if forward {
            (current + 1) % targets.len()
        } else {
            (current + targets.len() - 1) % targets.len()
        };
        let (_, field, action) = targets[next];
        state.selected_action = action;
        if let Some(index) = field {
            state.selected_field =
                SetupField::from_index(index, state.is_custom()).expect("typed setup field");
        }
    }

    pub(super) fn handle_setup_action(&mut self, action: SetupAction) -> Result<Option<AppAction>> {
        if !self
            .setup_presentation()
            .actions
            .iter()
            .any(|(available, _)| *available == action)
        {
            return Ok(None);
        }
        let Some(state) = self.setup_state.as_mut() else {
            return Ok(None);
        };
        match action {
            SetupAction::Continue => {
                let error = if state.selected_field.step() == SetupStep::Connection {
                    validate_setup_connection(state)
                } else {
                    validate_setup_state(state)
                };
                if let Some(error) = error {
                    state.save_error = Some(error);
                    return Ok(None);
                }
                state.selected_field = if state.selected_field.step() == SetupStep::Connection {
                    SetupField::Model
                } else {
                    SetupField::Save
                };
            }
            SetupAction::Back => {
                state.selected_field = match state.selected_field.step() {
                    SetupStep::Provider | SetupStep::Connection => SetupField::Provider,
                    SetupStep::Model => SetupField::ApiKey,
                    SetupStep::Review => SetupField::Model,
                };
            }
            SetupAction::Options => {
                state.model_options_open = !state.model_options_open;
                state.selected_field = SetupField::Model;
            }
        }
        state.selected_action = None;
        state.save_error = None;
        Ok(None)
    }

    pub(super) fn handle_setup_key_event(&mut self, key: KeyEvent) -> Result<Option<AppAction>> {
        // Some terminal/event adapters deliver the Return key as a literal CR/LF character.
        // Normalize it at the product boundary too, so callers cannot turn the selected Save
        // action into a silent no-op by bypassing the launcher adapter.
        let mut key = key;
        if key.modifiers.is_empty() && matches!(key.code, KeyCode::Char('\r' | '\n')) {
            key.code = KeyCode::Enter;
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.should_quit = true;
            return Ok(None);
        }
        if self.has_modal() {
            if key.code == KeyCode::Char('s') && key.modifiers.contains(KeyModifiers::CONTROL) {
                let outcome = self.submit_modal();
                self.apply_modal_outcome(outcome);
                if let Some(state) = self.setup_state.as_mut() {
                    state.selected_field = SetupField::Save;
                    state.selected_action = None;
                }
                return Ok(None);
            }
            let outcome = self.handle_modal_key_event(key);
            self.apply_modal_outcome(outcome);
            return Ok(None);
        }

        if let Some(view) = self.setup_recovery_view() {
            let plain = key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT;
            if view.diagnostics {
                let (_, page) = crate::ui::setup_recovery_scroll_bounds(
                    self.terminal_width,
                    self.terminal_height,
                    &self.setup_lines(),
                    &view,
                );
                match key.code {
                    KeyCode::Esc => {
                        return self.handle_setup_recovery_action(SetupRecoveryAction::Back);
                    }
                    KeyCode::Up => self.scroll_setup_diagnostics(true, 1),
                    KeyCode::Down => self.scroll_setup_diagnostics(false, 1),
                    KeyCode::PageUp => self.scroll_setup_diagnostics(true, page),
                    KeyCode::PageDown => self.scroll_setup_diagnostics(false, page),
                    KeyCode::Home => self.scroll_setup_diagnostics(true, usize::MAX),
                    KeyCode::End => self.scroll_setup_diagnostics(false, usize::MAX),
                    _ => {}
                }
                return Ok(None);
            }
            let retry = if view.connection_changes_unsaved {
                SetupRecoveryAction::SaveChangesAndRetry
            } else {
                SetupRecoveryAction::Retry
            };
            let action = match key.code {
                KeyCode::Enter if key.modifiers.is_empty() => Some(retry),
                KeyCode::Char('s') if key.modifiers == KeyModifiers::CONTROL => Some(retry),
                KeyCode::Char('i' | 'I') if plain => Some(SetupRecoveryAction::Diagnostics),
                KeyCode::Char('e' | 'E') if plain => Some(SetupRecoveryAction::EditConnection),
                KeyCode::F(1) if key.modifiers.is_empty() => Some(SetupRecoveryAction::Diagnostics),
                KeyCode::F(2) if key.modifiers.is_empty() => {
                    Some(SetupRecoveryAction::EditConnection)
                }
                _ => None,
            };
            return match action {
                Some(action) => self.handle_setup_recovery_action(action),
                None => Ok(None),
            };
        }
        if key.code == KeyCode::Esc
            && let Some(state) = self
                .setup_state
                .as_mut()
                .filter(|state| state.startup_editing_connection)
        {
            state.startup_editing_connection = false;
            state.selected_field = SetupField::Save;
            return Ok(None);
        }

        let Some(selected_field) = self.setup_state.as_ref().map(|state| state.selected_field)
        else {
            return Ok(None);
        };

        let action = self
            .setup_state
            .as_ref()
            .and_then(|state| state.selected_action);
        if key.code == KeyCode::Enter
            && let Some(action) = action
        {
            return self.handle_setup_action(action);
        }
        if key.code == KeyCode::Esc {
            return self.handle_setup_action(SetupAction::Back);
        }
        let review_shortcut =
            key.code == KeyCode::Char('s') && key.modifiers.contains(KeyModifiers::CONTROL);
        if action.is_some()
            && !matches!(
                key.code,
                KeyCode::Tab | KeyCode::BackTab | KeyCode::Up | KeyCode::Down
            )
            && !review_shortcut
        {
            return Ok(None);
        }
        match key.code {
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if selected_field == SetupField::Save {
                    return self.complete_setup();
                }
                if let Some(state) = self.setup_state.as_mut() {
                    state.selected_field = SetupField::Save;
                    state.selected_action = None;
                }
                return Ok(None);
            }
            KeyCode::Tab => {
                self.move_setup_focus(true);
                return Ok(None);
            }
            KeyCode::BackTab => {
                self.move_setup_focus(false);
                return Ok(None);
            }
            KeyCode::Down | KeyCode::Right if selected_field == SetupField::Provider => {
                let state = self
                    .setup_state
                    .as_mut()
                    .expect("setup state was checked before setup key handling");
                state.cycle_provider();
                self.last_notice = Some(format!("provider -> {}", state.provider_label()));
                return Ok(None);
            }
            KeyCode::Up | KeyCode::Left if selected_field == SetupField::Provider => {
                let state = self
                    .setup_state
                    .as_mut()
                    .expect("setup state was checked before setup key handling");
                state.cycle_provider_previous();
                self.last_notice = Some(format!("provider -> {}", state.provider_label()));
                return Ok(None);
            }
            KeyCode::Enter if selected_field == SetupField::Provider => {
                let state = self
                    .setup_state
                    .as_mut()
                    .expect("setup state was checked before setup key handling");
                state.selected_action = None;
                state.selected_field = if state.is_custom() {
                    SetupField::Protocol
                } else {
                    SetupField::ApiKey
                };
                self.last_notice = Some(format!("provider selected: {}", state.provider_label()));
                return Ok(None);
            }
            KeyCode::Down => {
                self.move_setup_focus(true);
                return Ok(None);
            }
            KeyCode::Up => {
                self.move_setup_focus(false);
                return Ok(None);
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Enter
                if selected_field == SetupField::Protocol =>
            {
                let state = self
                    .setup_state
                    .as_mut()
                    .expect("setup state was checked before setup key handling");
                state.cycle_protocol();
                self.last_notice = Some(format!("protocol -> {}", state.protocol.label()));
                return Ok(None);
            }
            KeyCode::Left | KeyCode::Right if selected_field == SetupField::ApiKey => {
                let state = self
                    .setup_state
                    .as_mut()
                    .expect("setup state was checked before setup key handling");
                state.cycle_credential_source();
                self.last_notice = Some(format!(
                    "authentication -> {}",
                    state.credential_source.label()
                ));
                return Ok(None);
            }
            KeyCode::Left | KeyCode::Right if selected_field == SetupField::ContextWindow => {
                let state = self
                    .setup_state
                    .as_mut()
                    .expect("setup state was checked before setup key handling");
                let backwards = key.code == KeyCode::Left;
                state.context_window_tokens =
                    cycle_context_window_preset(&state.context_window_tokens, backwards).to_owned();
                state.bump_revision();
                self.last_notice = Some(format!(
                    "context window -> {}",
                    token_count_display(&state.context_window_tokens, "automatic")
                ));
                return Ok(None);
            }
            KeyCode::Left | KeyCode::Right if selected_field == SetupField::MaxOutputTokens => {
                let state = self
                    .setup_state
                    .as_mut()
                    .expect("setup state was checked before setup key handling");
                let backwards = key.code == KeyCode::Left;
                state.max_output_tokens = cycle_token_preset(
                    &state.max_output_tokens,
                    &MAX_OUTPUT_TOKEN_PRESETS,
                    backwards,
                )
                .to_owned();
                state.bump_revision();
                self.last_notice = Some(format!(
                    "max output tokens -> {}",
                    token_count_display(&state.max_output_tokens, "automatic")
                ));
                return Ok(None);
            }
            KeyCode::Enter if selected_field == SetupField::ApiKey => {
                let Some(state) = self.setup_state.as_ref() else {
                    return Ok(None);
                };
                if state.credential_source == SetupCredentialSource::SecureStore {
                    let current = state.api_key.clone();
                    self.open_secret_input(SecretInputTarget::SetupApiKey, &current);
                } else {
                    return self.handle_setup_action(SetupAction::Continue);
                }
                return Ok(None);
            }
            KeyCode::Enter if selected_field == SetupField::Endpoint => {
                let current = self
                    .setup_state
                    .as_ref()
                    .map(|state| state.base_url.clone())
                    .unwrap_or_default();
                self.open_text_input(TextInputTarget::SetupEndpoint, &current);
                return Ok(None);
            }
            KeyCode::Enter if selected_field == SetupField::Save => {
                return self.complete_setup();
            }
            KeyCode::Enter if selected_field == SetupField::Model => {
                let current = self
                    .setup_state
                    .as_ref()
                    .map(|state| state.model.clone())
                    .unwrap_or_default();
                self.open_model_picker(ModelPickerTarget::Setup, &current);
                return Ok(None);
            }
            KeyCode::Enter if selected_field == SetupField::ContextWindow => {
                let current = self
                    .setup_state
                    .as_ref()
                    .map(|state| state.context_window_tokens.clone())
                    .unwrap_or_default();
                self.open_text_input(TextInputTarget::SetupContextWindow, &current);
                return Ok(None);
            }
            KeyCode::Enter if selected_field == SetupField::MaxOutputTokens => {
                let current = self
                    .setup_state
                    .as_ref()
                    .map(|state| state.max_output_tokens.clone())
                    .unwrap_or_default();
                self.open_text_input(TextInputTarget::SetupMaxOutputTokens, &current);
                return Ok(None);
            }
            KeyCode::Backspace => return Ok(None),
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                if selected_field == SetupField::ApiKey
                    && self.setup_state.as_ref().is_some_and(|state| {
                        state.credential_source == SetupCredentialSource::SecureStore
                    })
                {
                    self.open_secret_input_with_char(SecretInputTarget::SetupApiKey, character);
                    return Ok(None);
                }
                if selected_field == SetupField::Model {
                    self.open_text_input_with_char(TextInputTarget::SetupModel, character);
                    return Ok(None);
                }
                if (selected_field == SetupField::ContextWindow
                    || selected_field == SetupField::MaxOutputTokens)
                    && is_token_count_character(character)
                {
                    let target = if selected_field == SetupField::ContextWindow {
                        TextInputTarget::SetupContextWindow
                    } else {
                        TextInputTarget::SetupMaxOutputTokens
                    };
                    self.open_text_input_with_char(target, character);
                    return Ok(None);
                }
                if selected_field == SetupField::Endpoint {
                    self.open_text_input_with_char(TextInputTarget::SetupEndpoint, character);
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
        if state.startup_recovery_visible() || state.selected_action.is_some() {
            return;
        }
        let value = text
            .chars()
            .filter(|character| !character.is_control())
            .collect::<String>();
        if value.is_empty() {
            return;
        }
        match state.selected_field {
            SetupField::Model => {
                state.set_model(value.clone());
                self.last_notice = Some(format!("updated model {value}"));
            }
            SetupField::ContextWindow | SetupField::MaxOutputTokens => {
                match parse_optional_token_count(&value) {
                    Ok(_) => {
                        if state.selected_field == SetupField::ContextWindow {
                            state.context_window_tokens = value;
                        } else {
                            state.max_output_tokens = value;
                        }
                        state.bump_revision();
                        self.last_notice =
                            Some(format!("updated {}", state.selected_field.label()));
                    }
                    Err(error) => self.last_notice = Some(error.to_string()),
                }
            }
            SetupField::Endpoint if state.is_custom() => {
                state.base_url = value;
                state.bump_revision();
                self.last_notice = Some("updated custom endpoint".to_owned());
            }
            SetupField::ApiKey if state.credential_source == SetupCredentialSource::SecureStore => {
                state.api_key = SecretString::new(value);
                state.bump_revision();
                self.last_notice = Some("staged API key for protected credential store".to_owned());
            }
            SetupField::Provider
            | SetupField::Protocol
            | SetupField::Endpoint
            | SetupField::ApiKey
            | SetupField::Save => {}
        }
    }

    pub(super) fn complete_setup(&mut self) -> Result<Option<AppAction>> {
        let Some(state) = &mut self.setup_state else {
            return Ok(None);
        };
        state.save_error = None;

        if let Some(error) = validate_setup_state(state) {
            state.save_error = Some(error.clone());
            self.last_notice = Some(error.clone());
            self.push_event("setup:error", error);
            return Ok(None);
        }

        let (root_config, publish_outcome, old_credential_cleanup_warning) =
            match save_setup_state(state) {
                Ok(root_config) => root_config,
                Err(error) => {
                    let message = format!("{error:#}");
                    state.save_error = Some(message.clone());
                    self.last_notice = Some(message.clone());
                    self.push_event("setup:error", message);
                    return Ok(None);
                }
            };
        if let ConfigPublishOutcome::PublishedVisibilityUncertain { recovery_path } =
            &publish_outcome
        {
            let message = recovery_path.as_ref().map_or_else(
                || {
                    "config replacement visibility is uncertain; inspect the config path before starting"
                        .to_owned()
                },
                |path| {
                    format!(
                        "config replacement visibility is uncertain; reconcile the previous config at {} before starting",
                        path.display()
                    )
                },
            );
            state.save_error = Some(message.clone());
            self.last_notice = Some(message.clone());
            self.push_event("setup:error", message);
            return Ok(None);
        }
        if publish_outcome == ConfigPublishOutcome::PublishedDurabilityUncertain {
            // The replacement may already be visible, but the parent-directory fsync did not
            // complete. Keep Setup active and keep the staged secret so the user can explicitly
            // retry after the filesystem is healthy; booting here would announce success for a
            // config that may disappear after a crash.
            if let Ok(snapshot) = sigil_runtime::provider_connections::PersistedConfigSnapshot::load(
                &state.config_path,
            ) {
                state.startup_config = Some(snapshot);
            }
            let message = format!(
                "config was published but filesystem durability is uncertain; verify {} and press Enter to retry before starting",
                state.config_path.display()
            );
            state.save_error = Some(message.clone());
            self.last_notice = Some(message.clone());
            self.push_event("setup:error", message);
            return Ok(None);
        }
        let persisted_snapshot =
            match sigil_runtime::provider_connections::PersistedConfigSnapshot::load(
                &state.config_path,
            ) {
                Ok(snapshot) if snapshot.parsed().is_some() => snapshot,
                Ok(_) => {
                    let message = format!(
                        "config was published but could not be reloaded as valid current-schema config; verify {} before starting",
                        state.config_path.display()
                    );
                    state.save_error = Some(message.clone());
                    self.last_notice = Some(message.clone());
                    self.push_event("setup:error", message);
                    return Ok(None);
                }
                Err(error) => {
                    let message = format!(
                        "config was published but could not be reloaded: {error:#}; verify {} before starting",
                        state.config_path.display()
                    );
                    state.save_error = Some(message.clone());
                    self.last_notice = Some(message.clone());
                    self.push_event("setup:error", message);
                    return Ok(None);
                }
            };
        // Keep the exact bytes that were actually published. If a later authority phase fails,
        // the displayed retry must use this new source as its CAS frontier rather than the
        // pre-edit snapshot that was loaded when Setup opened.
        state.startup_config = Some(persisted_snapshot);
        state.clear_staged_secrets();
        state.mark_startup_configuration_saved();
        let saved_notice = format!(
            "saved config to {}; orchestration={} / {}",
            state.config_path.display(),
            root_config.task.routing_policy.as_str(),
            root_config.task.multi_agent_mode.as_str()
        );
        let saved_notice = if state.credential_source == SetupCredentialSource::Environment
            && !state.environment_detected()
        {
            format!("{saved_notice}; configured environment credential is not set")
        } else {
            saved_notice
        };
        self.last_notice = Some(if old_credential_cleanup_warning {
            format!(
                "{saved_notice}; an unreferenced stored credential could not be cleaned up and was retained"
            )
        } else {
            saved_notice
        });
        Ok(Some(AppAction::SetupCompleted {
            config_path: state.config_path.clone(),
            root_config: Box::new(root_config),
        }))
    }
}

fn setup_startup_recovery_lines(state: &SetupState) -> Vec<String> {
    let unsaved = state.startup_connection_changes_unsaved();
    let retry = if unsaved {
        "save changes and retry"
    } else {
        "retry Start"
    };
    if state.startup_diagnostics_open {
        let mut lines = vec![
            "Startup diagnostics".to_owned(),
            if unsaved {
                "Connection changes are not saved. Sigil has not started."
            } else {
                "Configuration is saved. Sigil has not started."
            }
            .to_owned(),
        ];
        if let Some(error) = &state.save_error {
            lines.extend([String::new(), "Last retry:".to_owned(), error.clone()]);
        }
        if let Some(error) = &state.startup_error {
            lines.extend([String::new(), "Startup failure:".to_owned(), error.clone()]);
        }
        lines.extend([
            String::new(),
            format!("Configuration: {}", state.config_path.display()),
        ]);
        return lines;
    }
    let mut lines = vec![
        if unsaved {
            "Connection changes not saved · Not started"
        } else {
            "Configuration saved · Not started"
        }
        .to_owned(),
        String::new(),
    ];
    if state.save_error.is_some() {
        lines.extend([
            "The last retry could not complete.".to_owned(),
            format!("Open Diagnostics for the complete error. Resolve that issue, then {retry}."),
        ]);
    } else if state.startup_blocker == Some(StartupBlocker::OwnerActive) {
        lines.extend([
            "Another Sigil instance is still using this workspace.".to_owned(),
            format!("Close the other Sigil instance first. Then {retry}."),
        ]);
    } else {
        lines.extend([
            "Startup could not complete.".to_owned(),
            format!("Open Diagnostics for the reported cause. Resolve that issue, then {retry}."),
        ]);
    }
    lines
}

fn append_setup_startup_recovery_lines(lines: &mut Vec<String>, state: &SetupState) {
    let Some(error) = &state.startup_error else {
        return;
    };
    lines.push(String::new());
    if state.existing_config_repair_required() {
        lines.push(format!("configuration is invalid: {error}"));
        lines.push(
            "The invalid file remains unchanged until you explicitly save this reviewed current-schema replacement."
                .to_owned(),
        );
    } else {
        lines.push(format!(
            "startup recovery could not capture the config source: {error}"
        ));
        lines.push(
            "Restart Setup to reload the file before saving; no replacement will be published from this state."
                .to_owned(),
        );
    }
}

fn append_setup_save_error_lines(lines: &mut Vec<String>, state: &SetupState) {
    let Some(error) = &state.save_error else {
        return;
    };
    lines.push(String::new());
    lines.push(format!("Setup did not complete: {error}"));
    lines.push(
        "The reviewed values remain in Setup. Resolve the error, then press Enter on review or Ctrl-S to retry."
            .to_owned(),
    );
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
    if let Some(error) = validate_setup_connection(state) {
        return Some(error);
    }
    if state.model.trim().is_empty() {
        return Some("model cannot be empty".to_owned());
    }
    build_setup_root_config(state)
        .err()
        .map(|error| format!("{error:#}"))
}

fn validate_setup_connection(state: &SetupState) -> Option<String> {
    if state.startup_config_snapshot_missing() {
        return Some(
            "config changed since Provider settings were loaded; restart setup and retry"
                .to_owned(),
        );
    }
    if state.is_custom() {
        match url::Url::parse(state.base_url.trim()) {
            Err(error) => return Some(format!("custom endpoint is invalid: {error}")),
            Ok(url) if !matches!(url.scheme(), "http" | "https") => {
                return Some("custom endpoint must use HTTP or HTTPS".to_owned());
            }
            Ok(_) => {}
        }
    }
    if let Err(error) = setup_connection_identity(state) {
        return Some(format!("{error:#}"));
    }
    match state.credential_source {
        SetupCredentialSource::SecureStore
            if state.api_key.expose_secret().trim().is_empty()
                && !state.can_reuse_stored_credential() =>
        {
            Some("enter an API key to save in the protected credential store".to_owned())
        }
        SetupCredentialSource::NoAuthentication if !state.no_authentication_allowed() => Some(
            "no authentication is only allowed for an explicit loopback custom endpoint".to_owned(),
        ),
        _ => None,
    }
}

pub(super) fn build_setup_root_config(state: &SetupState) -> Result<RootConfig> {
    let (base, connections, default_model) = build_setup_draft(state)?;
    let mut root_config = materialize_root_config(&base, &connections, &default_model)?;
    let (provider_name, route) =
        sigil_runtime::provider_connections::resolve_default_model_route(&root_config)
            .map_err(anyhow::Error::new)?;
    sigil_runtime::validate_output_token_budget(
        sigil_runtime::resolve_model_context_window_tokens(
            &root_config,
            &route.model_ref,
            &provider_name,
        )
        .tokens,
        root_config.model_request.max_output_tokens,
    )?;
    if let Some(provider_limit) =
        sigil_runtime::configured_provider_maximum_output_tokens(&root_config, &route.model_ref)
        && let Some(max_output_tokens) = root_config.model_request.max_output_tokens
    {
        anyhow::ensure!(
            max_output_tokens <= provider_limit,
            "max output tokens ({max_output_tokens}) exceed the provider limit ({provider_limit}) for model {}",
            route.model_ref.model_id
        );
    }
    if !state.valid_config_boot_retry_required() {
        let _ = apply_new_install_orchestration_rollout(&mut root_config);
    }
    Ok(root_config)
}

fn build_setup_draft(
    state: &SetupState,
) -> Result<(
    RootConfig,
    BTreeMap<ConnectionId, ProviderConnectionConfig>,
    ModelRef,
)> {
    let model = state.model.trim();
    if model.is_empty() {
        bail!("model cannot be empty");
    }
    match state.credential_source {
        SetupCredentialSource::SecureStore
            if state.api_key.expose_secret().trim().is_empty()
                && !state.can_reuse_stored_credential() =>
        {
            bail!(
                "provide api_key or export {}",
                state
                    .api_key_env_name()
                    .unwrap_or("the provider credential environment variable")
            )
        }
        SetupCredentialSource::NoAuthentication if !state.no_authentication_allowed() => {
            bail!("no authentication requires an explicit loopback custom endpoint")
        }
        _ => {}
    }
    let (family, protocol, connection_id, label) = setup_connection_identity(state)?;
    let existing_root = state
        .startup_config
        .as_ref()
        .and_then(|snapshot| snapshot.parsed())
        .cloned();
    let mut base = existing_root
        .clone()
        .unwrap_or_else(default_setup_root_config);
    let existing_loaded = existing_root.as_ref().map(load_provider_connections);
    let mut connections = existing_loaded
        .as_ref()
        .map(|loaded| {
            loaded
                .connections
                .iter()
                .map(|(id, loaded)| (id.clone(), loaded.config.clone()))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    let mut connection = if let Some(existing) = connections
        .get(&connection_id)
        .filter(|existing| existing.provider == family && existing.protocol == protocol)
    {
        existing.clone()
    } else {
        provider_connection_template(family, protocol, connection_id.clone(), label)?.0
    };
    connection.base_url = state.base_url.trim().to_owned();
    connection.credential = match state.credential_source {
        SetupCredentialSource::Environment => CredentialRefConfig::Environment {
            name: state
                .environment_credential_name()
                .context("provider does not declare an environment credential")?
                .to_owned(),
        },
        // A staged key replaces this non-secret reference during copy-on-write save. An empty
        // value is accepted only when this exact current connection already owns a stored ref.
        SetupCredentialSource::SecureStore => connection.credential.clone(),
        SetupCredentialSource::NoAuthentication => CredentialRefConfig::None,
    };
    connection.model_context_windows.remove(model);
    if let Some(tokens) = parse_optional_token_count(&state.context_window_tokens)
        .map_err(|error| anyhow::anyhow!("context window: {error}"))?
    {
        connection
            .model_context_windows
            .insert(model.to_owned(), tokens);
    }
    connection.validate()?;
    connections.insert(connection_id.clone(), connection);
    let default_model = ModelRef::new(connection_id, model)?;
    base.model_request.max_output_tokens = parse_optional_token_count(&state.max_output_tokens)
        .map_err(|error| anyhow::anyhow!("max output tokens: {error}"))?;
    Ok((base, connections, default_model))
}

pub(super) fn setup_connection_identity(
    state: &SetupState,
) -> Result<(ProviderFamily, ProviderProtocol, ConnectionId, String)> {
    if let Some(root_config) = state
        .startup_config
        .as_ref()
        .and_then(|snapshot| snapshot.parsed())
    {
        let loaded = load_provider_connections(root_config);
        if let Some(model_ref) = loaded.default_model.as_ref()
            && let Some(existing) = loaded.connections.get(&model_ref.connection_id)
            && setup_state_matches_connection(state, &existing.config)
        {
            return Ok((
                existing.config.provider,
                existing.config.protocol,
                existing.config.id.clone(),
                existing.config.label.clone(),
            ));
        }
    }

    let (family, protocol, id, label) = match state.provider_name.as_str() {
        "deepseek" => (
            ProviderFamily::DeepSeek,
            ProviderProtocol::DeepSeek,
            "deepseek-default",
            "DeepSeek".to_owned(),
        ),
        "openai_responses" => (
            ProviderFamily::OpenAi,
            ProviderProtocol::OpenAiResponses,
            "openai-default",
            "OpenAI".to_owned(),
        ),
        "anthropic" => (
            ProviderFamily::Anthropic,
            ProviderProtocol::AnthropicMessages,
            "anthropic-default",
            "Anthropic".to_owned(),
        ),
        "gemini" => (
            ProviderFamily::Gemini,
            ProviderProtocol::GeminiGenerateContent,
            "gemini-default",
            "Google Gemini".to_owned(),
        ),
        "openai_compat" => (
            ProviderFamily::Custom,
            state.protocol,
            "custom-default",
            "Custom endpoint".to_owned(),
        ),
        _ => bail!("unsupported setup provider"),
    };
    Ok((
        family,
        protocol,
        available_setup_connection_id(state, id)?,
        label,
    ))
}

fn setup_state_matches_connection(
    state: &SetupState,
    connection: &ProviderConnectionConfig,
) -> bool {
    let provider_matches = match state.provider_name.as_str() {
        "deepseek" => connection.provider == ProviderFamily::DeepSeek,
        "openai_responses" => connection.provider == ProviderFamily::OpenAi,
        "anthropic" => connection.provider == ProviderFamily::Anthropic,
        "gemini" => connection.provider == ProviderFamily::Gemini,
        "openai_compat" => matches!(
            connection.provider,
            ProviderFamily::Custom | ProviderFamily::OpenAi
        ),
        _ => false,
    };
    provider_matches && connection.protocol == state.protocol
}

fn available_setup_connection_id(state: &SetupState, base: &str) -> Result<ConnectionId> {
    let Some(root_config) = state
        .startup_config
        .as_ref()
        .and_then(|snapshot| snapshot.parsed())
    else {
        return ConnectionId::new(base).map_err(Into::into);
    };
    let loaded = load_provider_connections(root_config);
    let base_id = ConnectionId::new(base)?;
    if !loaded.connections.contains_key(&base_id) {
        return Ok(base_id);
    }
    for suffix in 2_u32.. {
        let candidate = format!("{base}-{suffix}");
        let candidate = ConnectionId::new(candidate)?;
        if !loaded.connections.contains_key(&candidate) {
            return Ok(candidate);
        }
    }
    unreachable!("u32 connection suffix space exhausted")
}

fn save_setup_state(state: &SetupState) -> Result<(RootConfig, ConfigPublishOutcome, bool)> {
    let root_config = build_setup_root_config(state)?;
    let loaded = load_provider_connections(&root_config);
    anyhow::ensure!(
        loaded.issues.is_empty(),
        "prepared setup connection is invalid"
    );
    let default_model = loaded
        .default_model
        .context("prepared setup route is missing")?;
    let connection_id = default_model.connection_id.clone();
    let family = loaded
        .connections
        .get(&connection_id)
        .context("prepared setup connection is missing")?
        .config
        .provider;
    let connections = loaded
        .connections
        .into_iter()
        .map(|(id, loaded)| (id, loaded.config))
        .collect();
    let credential_updates = if state.credential_source == SetupCredentialSource::SecureStore
        && !state.api_key.expose_secret().trim().is_empty()
    {
        vec![ConnectionCredentialUpdate {
            connection_id,
            prepared: PreparedCredential::api_key(
                family,
                state.api_key.expose_secret().trim().to_owned(),
            ),
        }]
    } else {
        Vec::new()
    };
    let save_draft = ConnectionSaveDraft {
        connections,
        default_model,
        credential_updates,
    };
    let persisted = persisted_root_config(&root_config);
    let config_path = state.config_path.clone();
    let replace_invalid = state.existing_config_repair_required();
    let startup_config = state.startup_config.clone();
    let outcome = std::thread::Builder::new()
        .name("sigil-setup-save".to_owned())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .context("failed to initialize credential save runtime")?;
            #[cfg(not(test))]
            let credential_store =
                ConfiguredProviderCredentialStore::from_root_config(&root_config);
            #[cfg(test)]
            let credential_store = TestSetupCredentialStore::default();
            let outcome = if replace_invalid {
                let startup_config = startup_config
                    .as_ref()
                    .context("invalid config source snapshot is unavailable")?;
                runtime.block_on(save_connection_config_replacing_invalid(
                    &persisted,
                    startup_config,
                    &config_path,
                    save_draft,
                    &credential_store,
                    &RootConfigPublisher,
                ))
            } else if let Some(startup_config) = startup_config.as_ref() {
                runtime.block_on(save_connection_config_with_base_from_snapshot(
                    startup_config,
                    &persisted,
                    &config_path,
                    save_draft,
                    &credential_store,
                    &RootConfigPublisher,
                ))
            } else {
                runtime.block_on(save_connection_config(
                    &persisted,
                    &config_path,
                    save_draft,
                    &credential_store,
                    &RootConfigPublisher,
                ))
            };
            outcome
                .map_err(anyhow::Error::new)
                .context("failed to save provider connection")
        })
        .context("failed to spawn setup save thread")?
        .join()
        .map_err(|_| anyhow::anyhow!("setup save thread panicked"))??;
    if outcome.publish_outcome == ConfigPublishOutcome::PublishedDurabilityUncertain {
        tracing::warn!("setup config was published but directory durability is uncertain");
    }
    Ok((
        outcome.root_config,
        outcome.publish_outcome,
        outcome.old_credential_cleanup_warning,
    ))
}

#[cfg(test)]
#[derive(Default)]
struct TestSetupCredentialStore {
    records: std::sync::Mutex<
        std::collections::BTreeMap<
            sigil_runtime::provider_connections::CredentialId,
            sigil_runtime::provider_connections::ProviderCredentialRecord,
        >,
    >,
}

#[cfg(test)]
#[async_trait::async_trait]
impl sigil_runtime::provider_connections::ProviderCredentialStore for TestSetupCredentialStore {
    async fn load(
        &self,
        credential_id: &sigil_runtime::provider_connections::CredentialId,
    ) -> std::result::Result<
        Option<sigil_runtime::provider_connections::ProviderCredentialRecord>,
        sigil_runtime::provider_connections::ProviderCredentialError,
    > {
        Ok(self
            .records
            .lock()
            .expect("test setup credential store lock")
            .get(credential_id)
            .cloned())
    }

    async fn store(
        &self,
        record: &sigil_runtime::provider_connections::ProviderCredentialRecord,
    ) -> std::result::Result<(), sigil_runtime::provider_connections::ProviderCredentialError> {
        self.records
            .lock()
            .expect("test setup credential store lock")
            .insert(record.credential_id.clone(), record.clone());
        Ok(())
    }

    async fn delete(
        &self,
        credential_id: &sigil_runtime::provider_connections::CredentialId,
    ) -> std::result::Result<bool, sigil_runtime::provider_connections::ProviderCredentialError>
    {
        Ok(self
            .records
            .lock()
            .expect("test setup credential store lock")
            .remove(credential_id)
            .is_some())
    }
}
