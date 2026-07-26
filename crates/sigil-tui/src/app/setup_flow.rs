use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use sigil_kernel::{ConfigUpdateLockGuard, ConnectionId, ModelRef, RootConfig, SecretString};
#[cfg(not(test))]
use sigil_runtime::provider_connections::ConfiguredProviderCredentialStore;
use sigil_runtime::provider_connections::{
    ConfigPublishOutcome, ConnectionCredentialUpdate, ConnectionSaveDraft, CredentialRefConfig,
    PreparedCredential, ProviderConfigPublisher, ProviderConnectionConfig, ProviderFamily,
    ProviderProtocol, RootConfigPublisher, default_setup_root_config, load_provider_connections,
    materialize_v2_root_config, provider_connection_template, save_connection_config,
};

use super::{
    AppAction, AppState, SetupField, SetupState,
    formatting::persisted_root_config,
    modal_flow::{ModelPickerTarget, SecretInputTarget, TextInputTarget},
};
use crate::setup::{SETUP_PROVIDER_ORDER, SetupCredentialSource};

impl AppState {
    pub(crate) fn setup_field_line_indices(&self) -> Vec<usize> {
        match self.setup_state.as_ref() {
            Some(state) if state.selected_field == SetupField::Provider => {
                (3..3 + SETUP_PROVIDER_ORDER.len()).collect()
            }
            Some(state) if state.is_custom() => (3..=8).collect(),
            Some(_) => (3..=6).collect(),
            None => Vec::new(),
        }
    }

    pub fn setup_lines(&self) -> Vec<String> {
        let Some(state) = &self.setup_state else {
            return Vec::new();
        };

        if state.selected_field == SetupField::Provider {
            let mut lines = vec![
                "Set up a model connection".to_owned(),
                "Step 1 of 4 · Choose the provider or endpoint you want to connect.".to_owned(),
                String::new(),
            ];
            for (index, provider_name) in SETUP_PROVIDER_ORDER.iter().enumerate() {
                let marker = if index == state.provider_index() {
                    ">"
                } else {
                    " "
                };
                lines.push(format!(
                    "{marker} {:<20} {}",
                    SetupState::provider_choice_label(provider_name),
                    SetupState::provider_choice_auth_summary(provider_name)
                ));
            }
            lines.extend([
                String::new(),
                "Up/Down choose · Enter continue · Ctrl-C quit".to_owned(),
            ]);
            if let Some(error) = &state.startup_error {
                lines.extend([String::new(), format!("load failed: {error}")]);
            }
            return lines;
        }

        let mut lines = vec![
            "Set up a model connection".to_owned(),
            "Steps 2–4 · Authenticate, choose a model, then review and start.".to_owned(),
            String::new(),
            render_setup_value_row(
                SetupField::Provider,
                state.selected_field,
                "provider",
                state.provider_label(),
                Some("Enter change"),
            ),
        ];
        if state.is_custom() {
            lines.push(render_setup_value_row(
                SetupField::Protocol,
                state.selected_field,
                "protocol",
                state.protocol.label(),
                Some("Left/Right switch"),
            ));
            lines.push(render_setup_value_row(
                SetupField::Endpoint,
                state.selected_field,
                "endpoint",
                &state.base_url,
                Some("Enter edit"),
            ));
        }
        lines.extend([
            render_setup_value_row(
                SetupField::ApiKey,
                state.selected_field,
                "authentication",
                state.credential_source.label(),
                Some("Left/Right choose · Enter continue"),
            ),
            render_setup_value_row(
                SetupField::Model,
                state.selected_field,
                "model",
                &state.model,
                Some("Enter choose"),
            ),
            render_setup_action_row(
                SetupField::Save,
                state.selected_field,
                "review, trust folder, save and start",
            ),
            String::new(),
            "[review]".to_owned(),
            format!(
                "connection: {} · {}",
                state.provider_label(),
                state.protocol.label()
            ),
            format!("authentication: {}", state.auth_summary()),
            format!("model: {}", state.model),
            "current session: starts with this route · saved default: this route".to_owned(),
        ]);

        if state.credential_source == SetupCredentialSource::SecureStore {
            lines.push(format!("staged credential: {}", state.masked_api_key()));
        }
        if let Some(error) = &state.startup_error {
            lines.push(String::new());
            lines.push(format!("load failed: {error}"));
        }

        lines.push(String::new());
        lines.push(
            "Up/Down move · Enter continue · Left/Right change option · Ctrl-S save · Ctrl-C quit"
                .to_owned(),
        );
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

        let Some((selected_field, custom)) = self
            .setup_state
            .as_ref()
            .map(|state| (state.selected_field, state.is_custom()))
        else {
            return Ok(None);
        };

        match key.code {
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return self.complete_setup();
            }
            KeyCode::Tab => {
                let state = self
                    .setup_state
                    .as_mut()
                    .expect("setup state was checked before setup key handling");
                state.selected_field = state.selected_field.next(custom);
                self.last_notice = Some(format!("setup field {}", state.selected_field.label()));
                return Ok(None);
            }
            KeyCode::BackTab => {
                let state = self
                    .setup_state
                    .as_mut()
                    .expect("setup state was checked before setup key handling");
                state.selected_field = state.selected_field.previous(custom);
                self.last_notice = Some(format!("setup field {}", state.selected_field.label()));
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
                state.selected_field = if state.is_custom() {
                    SetupField::Protocol
                } else {
                    SetupField::ApiKey
                };
                self.last_notice = Some(format!("provider selected: {}", state.provider_label()));
                return Ok(None);
            }
            KeyCode::Down => {
                let state = self
                    .setup_state
                    .as_mut()
                    .expect("setup state was checked before setup key handling");
                state.selected_field = state.selected_field.next(custom);
                self.last_notice = Some(format!("setup field {}", state.selected_field.label()));
                return Ok(None);
            }
            KeyCode::Up => {
                let state = self
                    .setup_state
                    .as_mut()
                    .expect("setup state was checked before setup key handling");
                state.selected_field = state.selected_field.previous(custom);
                self.last_notice = Some(format!("setup field {}", state.selected_field.label()));
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
            KeyCode::Enter if selected_field == SetupField::ApiKey => {
                let Some(state) = self.setup_state.as_ref() else {
                    return Ok(None);
                };
                if state.credential_source == SetupCredentialSource::SecureStore {
                    let current = state.api_key.clone();
                    self.open_secret_input(SecretInputTarget::SetupApiKey, &current);
                } else {
                    let custom = state.is_custom();
                    let state = self
                        .setup_state
                        .as_mut()
                        .expect("setup state was checked before setup key handling");
                    state.selected_field = state.selected_field.next(custom);
                    self.last_notice = Some(format!(
                        "authentication confirmed: {}",
                        state.credential_source.label()
                    ));
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
        let value = text
            .chars()
            .filter(|character| !character.is_control())
            .collect::<String>();
        if value.is_empty() {
            return;
        }
        match state.selected_field {
            SetupField::Model => {
                if state.admit_manual_model(&value) {
                    state.model = value.clone();
                    self.last_notice = Some(format!("updated model {value}"));
                } else {
                    self.last_notice = Some(
                        "manual model entry is unavailable for the current catalog state"
                            .to_owned(),
                    );
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
                self.last_notice = Some("staged API key for secure credential store".to_owned());
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

        if let Some(error) = validate_setup_state(state) {
            self.last_notice = Some(error.clone());
            self.push_event("setup:error", error);
            return Ok(None);
        }

        let (root_config, publish_outcome) = match save_setup_state(state) {
            Ok(root_config) => root_config,
            Err(error) => {
                let message = format!("{error:#}");
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
            self.last_notice = Some(message.clone());
            self.push_event("setup:error", message);
            return Ok(None);
        }
        state.clear_staged_secrets();
        self.last_notice = Some(
            if publish_outcome == ConfigPublishOutcome::PublishedDurabilityUncertain {
                format!(
                    "saved config to {}; filesystem durability is uncertain",
                    state.config_path.display()
                )
            } else {
                format!("saved config to {}", state.config_path.display())
            },
        );
        Ok(Some(AppAction::SetupCompleted {
            config_path: state.config_path.clone(),
            root_config: Box::new(root_config),
        }))
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
    if state.existing_config_repair_required() {
        return Some(
            "existing config is invalid and remains unchanged; run `sigil doctor`, repair the file, then reopen setup"
                .to_owned(),
        );
    }
    if state.model.trim().is_empty() {
        return Some("model cannot be empty".to_owned());
    }
    if state.is_custom()
        && let Err(error) = url::Url::parse(state.base_url.trim())
    {
        return Some(format!("custom endpoint is invalid: {error}"));
    }
    if let Err(error) = setup_connection_identity(state) {
        return Some(format!("{error:#}"));
    }
    match state.credential_source {
        SetupCredentialSource::Environment if !state.environment_detected() => Some(format!(
            "selected environment variable {} is not set",
            state.api_key_env_name().unwrap_or("for this provider")
        )),
        SetupCredentialSource::SecureStore if state.api_key.expose_secret().trim().is_empty() => {
            Some("enter an API key to save in the secure credential store".to_owned())
        }
        SetupCredentialSource::NoAuthentication if !state.no_authentication_allowed() => Some(
            "no authentication is only allowed for an explicit loopback custom endpoint".to_owned(),
        ),
        _ => {
            let admission = state.catalog_admission.as_ref().filter(|admission| {
                admission.draft_revision == state.draft_revision
                    && (admission.available_models.contains(state.model.trim())
                        || admission.manual_model.as_deref() == Some(state.model.trim()))
            });
            if admission.is_none() {
                return Some(
                    "verify this connection in the model picker before saving; rejected, missing, or offline credentials cannot start a session"
                        .to_owned(),
                );
            }
            build_setup_root_config(state)
                .err()
                .map(|error| format!("{error:#}"))
        }
    }
}

pub(super) fn build_setup_root_config(state: &SetupState) -> Result<RootConfig> {
    let (base, connections, default_model) = build_setup_draft(state)?;
    materialize_v2_root_config(&base, &connections, &default_model)
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
        SetupCredentialSource::Environment if !state.environment_detected() => bail!(
            "provide api_key or export {}",
            state
                .api_key_env_name()
                .unwrap_or("the provider credential environment variable")
        ),
        SetupCredentialSource::SecureStore if state.api_key.expose_secret().trim().is_empty() => {
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
    let (mut connection, _) =
        provider_connection_template(family, protocol, connection_id.clone(), label)?;
    connection.base_url = state.base_url.trim().to_owned();
    connection.credential = match state.credential_source {
        SetupCredentialSource::Environment => CredentialRefConfig::Environment {
            name: state
                .api_key_env_name()
                .context("provider does not declare an environment credential")?
                .to_owned(),
        },
        // The copy-on-write save replaces this non-secret placeholder before publish.
        SetupCredentialSource::SecureStore => connection.credential,
        SetupCredentialSource::NoAuthentication => CredentialRefConfig::None,
    };
    connection.validate()?;
    let default_model = ModelRef::new(connection_id.clone(), model)?;
    let base = default_setup_root_config();
    Ok((
        base,
        BTreeMap::from([(connection_id, connection)]),
        default_model,
    ))
}

fn setup_connection_identity(
    state: &SetupState,
) -> Result<(ProviderFamily, ProviderProtocol, ConnectionId, &'static str)> {
    let (family, protocol, id, label) = match state.provider_name.as_str() {
        "deepseek" => (
            ProviderFamily::DeepSeek,
            ProviderProtocol::DeepSeek,
            "deepseek-default",
            "DeepSeek",
        ),
        "openai_responses" => (
            ProviderFamily::OpenAi,
            ProviderProtocol::OpenAiResponses,
            "openai-default",
            "OpenAI",
        ),
        "anthropic" => (
            ProviderFamily::Anthropic,
            ProviderProtocol::AnthropicMessages,
            "anthropic-default",
            "Anthropic",
        ),
        "gemini" => (
            ProviderFamily::Gemini,
            ProviderProtocol::GeminiGenerateContent,
            "gemini-default",
            "Google Gemini",
        ),
        "openai_compat" => (
            ProviderFamily::Custom,
            state.protocol,
            "custom-default",
            "Custom endpoint",
        ),
        _ => bail!("unsupported setup provider"),
    };
    Ok((family, protocol, ConnectionId::new(id)?, label))
}

fn save_setup_state(state: &SetupState) -> Result<(RootConfig, ConfigPublishOutcome)> {
    let root_config = build_setup_root_config(state)?;
    if state.credential_source != SetupCredentialSource::SecureStore {
        let lock = ConfigUpdateLockGuard::acquire(&state.config_path)?;
        let publish_outcome = RootConfigPublisher.publish(
            &state.config_path,
            &persisted_root_config(&root_config),
            &lock,
        )?;
        return Ok((root_config, publish_outcome));
    }

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
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to initialize credential save runtime")?;
    #[cfg(not(test))]
    let credential_store = ConfiguredProviderCredentialStore::from_root_config(&root_config);
    #[cfg(test)]
    let credential_store = TestSetupCredentialStore::default();
    let save = async {
        save_connection_config(
            &root_config,
            &state.config_path,
            ConnectionSaveDraft {
                connections,
                default_model,
                credential_updates: vec![ConnectionCredentialUpdate {
                    connection_id,
                    prepared: PreparedCredential::api_key(
                        family,
                        state.api_key.expose_secret().trim().to_owned(),
                    ),
                }],
                confirmed_legacy_environment: Default::default(),
            },
            &credential_store,
            &RootConfigPublisher,
        )
        .await
    };
    let outcome = runtime
        .block_on(save)
        .map_err(anyhow::Error::new)
        .context("failed to save provider connection")?;
    if outcome.publish_outcome == ConfigPublishOutcome::PublishedDurabilityUncertain {
        tracing::warn!("setup config was published but directory durability is uncertain");
    }
    Ok((outcome.root_config, outcome.publish_outcome))
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
