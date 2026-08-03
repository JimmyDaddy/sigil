use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value};
use sigil_kernel::{ConnectionId, ModelRef, RootConfig};
use sigil_runtime::{
    ANTHROPIC_PROVIDER_KEY, DEEPSEEK_PROVIDER_KEY, GEMINI_PROVIDER_KEY, OPENAI_COMPAT_PROVIDER_KEY,
    OPENAI_RESPONSES_PROVIDER_KEY, default_provider_model, normalize_provider_name,
    provider_connections::{
        ConnectionCredentialUpdate, ConnectionSaveDraft, CredentialRefConfig, PreparedCredential,
        ProviderConnectionConfig, ProviderFamily, ProviderProtocol, load_provider_connections,
        provider_connection_template,
    },
};

use super::ConfigDraft;

#[derive(Debug, Clone)]
pub(super) struct ProviderConnectionDraft {
    pub(super) config: ProviderConnectionConfig,
    pub(super) model: String,
    pub(super) staged_credential: Option<PreparedCredential>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConnectionPickerChoiceKind {
    Existing(ConnectionId),
    AddProvider(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConnectionPickerChoice {
    pub(crate) kind: ConnectionPickerChoiceKind,
    pub(crate) label: String,
    pub(crate) detail: String,
}

pub(super) fn connection_drafts_from_root_config(
    root_config: &RootConfig,
) -> Result<(
    BTreeMap<ConnectionId, ProviderConnectionDraft>,
    ModelRef,
    ConnectionId,
)> {
    let loaded = load_provider_connections(root_config);
    anyhow::ensure!(
        loaded.issues.is_empty(),
        "provider connection configuration is invalid"
    );
    let loaded_default_model = loaded
        .default_model
        .clone()
        .context("model_route_not_configured")?;
    let mut drafts = BTreeMap::new();
    for (id, loaded_connection) in loaded.connections {
        let model = if id == loaded_default_model.connection_id {
            loaded_default_model.model_id.clone()
        } else {
            default_provider_model(provider_key_for_connection(&loaded_connection.config))
                .context("provider-owned default model is unavailable")?
        };
        drafts.insert(
            id,
            ProviderConnectionDraft {
                config: loaded_connection.config,
                model,
                staged_credential: None,
            },
        );
    }
    anyhow::ensure!(!drafts.is_empty(), "connection registry is empty");
    let default_model = loaded_default_model;
    let selected_connection_id = default_model.connection_id.clone();
    Ok((drafts, default_model, selected_connection_id))
}

impl ConfigDraft {
    pub(super) fn selected_connection(&self) -> Option<&ProviderConnectionDraft> {
        self.connection_drafts.get(&self.selected_connection_id)
    }

    pub(crate) fn selected_connection_summary(&self) -> String {
        self.selected_connection()
            .map(|draft| {
                format!(
                    "{} ({}/{})",
                    draft.config.label,
                    self.connection_position(),
                    self.connection_drafts.len()
                )
            })
            .unwrap_or_else(|| "unavailable".to_owned())
    }

    pub(crate) fn selected_credential_summary(&self) -> String {
        let Some(draft) = self.selected_connection() else {
            return "unavailable".to_owned();
        };
        if !self.provider_api_key.is_empty() || draft.staged_credential.is_some() {
            return "protected store · staged in memory".to_owned();
        }
        match &draft.config.credential {
            CredentialRefConfig::Environment { name } => format!("environment · {name}"),
            CredentialRefConfig::Stored { .. } => "protected store · referenced".to_owned(),
            CredentialRefConfig::None => "no authentication".to_owned(),
        }
    }

    pub(crate) fn selected_prepared_credential(&self) -> Option<PreparedCredential> {
        if !self.provider_api_key.expose_secret().trim().is_empty() {
            return self.selected_connection().map(|draft| {
                PreparedCredential::api_key(
                    draft.config.provider,
                    self.provider_api_key.expose_secret().trim().to_owned(),
                )
            });
        }
        self.selected_connection()
            .and_then(|draft| draft.staged_credential.clone())
    }

    pub(crate) fn capture_selected_connection(&mut self) -> Result<()> {
        self.capture_selected_model_context_window()?;
        let selected = self
            .connection_drafts
            .get(&self.selected_connection_id)
            .cloned()
            .context("selected connection is unavailable")?;
        let (family, protocol) = if selected.config.provider == ProviderFamily::Custom
            && selected.config.protocol == ProviderProtocol::OpenAiResponses
            && normalize_provider_name(&self.provider_name) == OPENAI_RESPONSES_PROVIDER_KEY
        {
            // The simple surface presents the supported Responses protocol without collapsing a
            // saved custom connection into the first-party OpenAI family on an unrelated save.
            (ProviderFamily::Custom, ProviderProtocol::OpenAiResponses)
        } else {
            provider_identity(&self.provider_name)?
        };
        let mut next = if selected.config.provider == family && selected.config.protocol == protocol
        {
            selected
        } else {
            let (template, model) = provider_connection_template(
                family,
                protocol,
                self.selected_connection_id.clone(),
                selected.config.label,
            )?;
            ProviderConnectionDraft {
                config: template,
                model,
                staged_credential: None,
            }
        };
        next.config.base_url = self.provider_base_url.trim().to_owned();
        next.model = self.provider_model.trim().to_owned();
        if next.model.is_empty() {
            bail!("model cannot be empty");
        }
        apply_deepseek_options(self, &mut next.config)?;
        next.config.validate()?;
        ModelRef::new(next.config.id.clone(), next.model.clone())?;
        if !self.provider_api_key.expose_secret().trim().is_empty() {
            next.staged_credential = Some(PreparedCredential::api_key(
                next.config.provider,
                self.provider_api_key.expose_secret().trim().to_owned(),
            ));
        }
        if self.default_model.connection_id == self.selected_connection_id {
            self.default_model =
                ModelRef::new(self.selected_connection_id.clone(), next.model.clone())?;
        }
        self.connection_drafts
            .insert(self.selected_connection_id.clone(), next);
        Ok(())
    }

    pub(crate) fn set_provider_model(&mut self, model: String) -> Result<bool> {
        let model = model.trim().to_owned();
        anyhow::ensure!(!model.is_empty(), "model cannot be empty");
        if self.provider_model == model {
            return Ok(false);
        }
        self.capture_selected_model_context_window()?;
        self.provider_model = model;
        self.load_selected_model_context_window()?;
        Ok(true)
    }

    fn capture_selected_model_context_window(&mut self) -> Result<()> {
        let model = self.provider_model.trim().to_owned();
        ModelRef::new(self.selected_connection_id.clone(), model.clone())?;
        let tokens = parse_model_context_window_tokens(&self.provider_context_window_tokens)?;
        let selected = self
            .connection_drafts
            .get_mut(&self.selected_connection_id)
            .context("selected connection is unavailable")?;
        if let Some(tokens) = tokens {
            selected.config.model_context_windows.insert(model, tokens);
        } else {
            selected.config.model_context_windows.remove(&model);
        }
        Ok(())
    }

    fn load_selected_model_context_window(&mut self) -> Result<()> {
        let selected = self
            .connection_drafts
            .get(&self.selected_connection_id)
            .context("selected connection is unavailable")?;
        self.provider_context_window_tokens = selected
            .config
            .model_context_windows
            .get(self.provider_model.trim())
            .map(u32::to_string)
            .unwrap_or_default();
        Ok(())
    }

    pub(crate) fn cycle_connection(&mut self, forward: bool) -> Result<()> {
        self.capture_selected_connection()?;
        let ids = self.connection_drafts.keys().cloned().collect::<Vec<_>>();
        let current = ids
            .iter()
            .position(|id| id == &self.selected_connection_id)
            .unwrap_or_default();
        let next = if forward {
            (current + 1) % ids.len()
        } else if current == 0 {
            ids.len() - 1
        } else {
            current - 1
        };
        self.selected_connection_id = ids[next].clone();
        self.load_selected_connection()
    }

    pub(crate) fn select_connection(&mut self, connection_id: &ConnectionId) -> Result<()> {
        anyhow::ensure!(
            self.connection_drafts.contains_key(connection_id),
            "selected connection is unavailable"
        );
        self.capture_selected_connection()?;
        self.selected_connection_id = connection_id.clone();
        self.load_selected_connection()
    }

    pub(crate) fn add_connection_for_provider(&mut self, provider_name: &str) -> Result<()> {
        self.capture_selected_connection()?;
        let (family, protocol) = provider_identity(provider_name)?;
        let base = match protocol {
            ProviderProtocol::DeepSeek => "deepseek",
            ProviderProtocol::OpenAiResponses => "openai",
            ProviderProtocol::OpenAiChatCompletions => "openai-compatible",
            ProviderProtocol::AnthropicMessages => "anthropic",
            ProviderProtocol::GeminiGenerateContent => "gemini",
        };
        let mut suffix = 1_u32;
        let id = loop {
            let candidate = ConnectionId::new(format!("{base}-{suffix}"))?;
            if !self.connection_drafts.contains_key(&candidate) {
                break candidate;
            }
            suffix = suffix.saturating_add(1);
        };
        let label = format!("{} {suffix}", family.label());
        let (config, model) = provider_connection_template(family, protocol, id.clone(), label)?;
        self.connection_drafts.insert(
            id.clone(),
            ProviderConnectionDraft {
                config,
                model,
                staged_credential: None,
            },
        );
        self.selected_connection_id = id;
        self.load_selected_connection()
    }

    pub(crate) fn connection_picker_choices(&self) -> Vec<ConnectionPickerChoice> {
        let mut choices = self
            .connection_drafts
            .iter()
            .map(|(id, draft)| {
                let mut tags = vec![
                    provider_key_for_connection(&draft.config).to_owned(),
                    draft.model.clone(),
                    credential_label(draft).to_owned(),
                ];
                if id == &self.default_model.connection_id {
                    tags.push("saved default".to_owned());
                }
                ConnectionPickerChoice {
                    kind: ConnectionPickerChoiceKind::Existing(id.clone()),
                    label: draft.config.label.clone(),
                    detail: tags.join(" · "),
                }
            })
            .collect::<Vec<_>>();
        choices.extend([
            add_provider_choice(DEEPSEEK_PROVIDER_KEY, "DeepSeek"),
            add_provider_choice(OPENAI_RESPONSES_PROVIDER_KEY, "OpenAI"),
            add_provider_choice(ANTHROPIC_PROVIDER_KEY, "Anthropic"),
            add_provider_choice(GEMINI_PROVIDER_KEY, "Google Gemini"),
            add_provider_choice(OPENAI_COMPAT_PROVIDER_KEY, "OpenAI-compatible"),
        ]);
        choices
    }

    pub(crate) fn delete_selected_connection(
        &mut self,
        current_session: Option<&ModelRef>,
    ) -> Result<()> {
        self.validate_selected_connection_deletion(current_session)?;
        let removed = self.selected_connection_id.clone();
        let removed_default = removed == self.default_model.connection_id;
        self.connection_drafts.remove(&removed);
        self.selected_connection_id = self
            .connection_drafts
            .keys()
            .next()
            .cloned()
            .context("connection registry became empty")?;
        self.load_selected_connection()?;
        if removed_default {
            self.set_selected_as_default()?;
        }
        Ok(())
    }

    pub(crate) fn validate_selected_connection_deletion(
        &mut self,
        current_session: Option<&ModelRef>,
    ) -> Result<()> {
        self.capture_selected_connection()?;
        anyhow::ensure!(
            self.connection_drafts.len() > 1,
            "at least one connection must remain"
        );
        anyhow::ensure!(
            current_session
                .is_none_or(|model| { model.connection_id != self.selected_connection_id }),
            "the current session connection cannot be deleted"
        );
        anyhow::ensure!(
            !role_references_connection(&self.base_root_config, &self.selected_connection_id),
            "an agent role still references this connection"
        );
        Ok(())
    }

    pub(crate) fn set_selected_as_default(&mut self) -> Result<()> {
        self.capture_selected_connection()?;
        let model = self
            .connection_drafts
            .get(&self.selected_connection_id)
            .context("selected connection is unavailable")?
            .model
            .clone();
        self.default_model = ModelRef::new(self.selected_connection_id.clone(), model)?;
        Ok(())
    }

    pub(crate) fn connection_save_draft(&self) -> Result<ConnectionSaveDraft> {
        let mut snapshot = self.clone();
        snapshot.capture_selected_connection()?;
        let connections = snapshot
            .connection_drafts
            .iter()
            .map(|(id, draft)| (id.clone(), draft.config.clone()))
            .collect();
        let credential_updates = snapshot
            .connection_drafts
            .iter()
            .filter_map(|(id, draft)| {
                draft
                    .staged_credential
                    .clone()
                    .map(|prepared| ConnectionCredentialUpdate {
                        connection_id: id.clone(),
                        prepared,
                    })
            })
            .collect();
        Ok(ConnectionSaveDraft {
            connections,
            default_model: snapshot.default_model,
            credential_updates,
        })
    }

    pub(crate) fn connection_rows(&self) -> Vec<String> {
        self.connection_drafts
            .iter()
            .map(|(id, draft)| {
                let selected = if id == &self.selected_connection_id {
                    ">"
                } else {
                    " "
                };
                let default = if id == &self.default_model.connection_id {
                    " · default"
                } else {
                    ""
                };
                let credential = credential_label(draft);
                format!(
                    "{selected} {}  {} · {credential}{default}",
                    draft.config.label, draft.model
                )
            })
            .collect()
    }

    fn connection_position(&self) -> usize {
        self.connection_drafts
            .keys()
            .position(|id| id == &self.selected_connection_id)
            .map_or(0, |index| index + 1)
    }

    pub(super) fn load_selected_connection(&mut self) -> Result<()> {
        let selected = self
            .selected_connection()
            .cloned()
            .context("selected connection is unavailable")?;
        self.provider_name = provider_key_for_connection(&selected.config).to_owned();
        self.provider_model = selected.model;
        self.provider_context_window_tokens = selected
            .config
            .model_context_windows
            .get(&self.provider_model)
            .map(u32::to_string)
            .unwrap_or_default();
        self.provider_base_url = selected.config.base_url;
        self.provider_api_key.clear();
        load_deepseek_options(self, &selected.config.options);
        Ok(())
    }
}

fn parse_model_context_window_tokens(value: &str) -> Result<Option<u32>> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    let tokens = value
        .parse::<u32>()
        .context("model context_window_tokens must be a positive integer")?;
    anyhow::ensure!(
        tokens > 0,
        "model context_window_tokens must be greater than 0"
    );
    Ok(Some(tokens))
}

fn add_provider_choice(provider_name: &str, label: &str) -> ConnectionPickerChoice {
    ConnectionPickerChoice {
        kind: ConnectionPickerChoiceKind::AddProvider(provider_name.to_owned()),
        label: format!("Add {label}"),
        detail: "create an unsaved connection from this provider template".to_owned(),
    }
}

fn credential_label(draft: &ProviderConnectionDraft) -> &'static str {
    match draft.config.credential {
        CredentialRefConfig::Environment { .. } => "environment",
        CredentialRefConfig::Stored { .. } => "protected store",
        CredentialRefConfig::None => "no auth",
    }
}

pub(super) fn provider_key_for_connection(connection: &ProviderConnectionConfig) -> &'static str {
    match (connection.provider, connection.protocol) {
        (ProviderFamily::DeepSeek, ProviderProtocol::DeepSeek) => DEEPSEEK_PROVIDER_KEY,
        (ProviderFamily::OpenAi, ProviderProtocol::OpenAiResponses) => {
            OPENAI_RESPONSES_PROVIDER_KEY
        }
        (ProviderFamily::Anthropic, ProviderProtocol::AnthropicMessages) => ANTHROPIC_PROVIDER_KEY,
        (ProviderFamily::Gemini, ProviderProtocol::GeminiGenerateContent) => GEMINI_PROVIDER_KEY,
        (ProviderFamily::Custom, ProviderProtocol::OpenAiResponses) => {
            OPENAI_RESPONSES_PROVIDER_KEY
        }
        (ProviderFamily::Custom, ProviderProtocol::OpenAiChatCompletions) => {
            OPENAI_COMPAT_PROVIDER_KEY
        }
        _ => OPENAI_COMPAT_PROVIDER_KEY,
    }
}

pub(super) fn provider_identity(provider_name: &str) -> Result<(ProviderFamily, ProviderProtocol)> {
    match normalize_provider_name(provider_name) {
        DEEPSEEK_PROVIDER_KEY => Ok((ProviderFamily::DeepSeek, ProviderProtocol::DeepSeek)),
        OPENAI_RESPONSES_PROVIDER_KEY => {
            Ok((ProviderFamily::OpenAi, ProviderProtocol::OpenAiResponses))
        }
        OPENAI_COMPAT_PROVIDER_KEY => Ok((
            ProviderFamily::Custom,
            ProviderProtocol::OpenAiChatCompletions,
        )),
        ANTHROPIC_PROVIDER_KEY => Ok((
            ProviderFamily::Anthropic,
            ProviderProtocol::AnthropicMessages,
        )),
        GEMINI_PROVIDER_KEY => Ok((
            ProviderFamily::Gemini,
            ProviderProtocol::GeminiGenerateContent,
        )),
        other => bail!("unsupported provider {other}"),
    }
}

fn apply_deepseek_options(
    draft: &ConfigDraft,
    connection: &mut ProviderConnectionConfig,
) -> Result<()> {
    if connection.provider != ProviderFamily::DeepSeek {
        return Ok(());
    }
    let options = connection
        .options
        .as_object_mut()
        .context("DeepSeek connection options must be an object")?;
    options.insert(
        "beta_base_url".to_owned(),
        Value::String(draft.provider_beta_base_url.trim().to_owned()),
    );
    options.insert(
        "anthropic_base_url".to_owned(),
        Value::String(draft.provider_anthropic_base_url.trim().to_owned()),
    );
    if draft.provider_user_id_strategy.trim().is_empty() {
        options.remove("user_id_strategy");
    } else {
        options.insert(
            "user_id_strategy".to_owned(),
            Value::String(draft.provider_user_id_strategy.trim().to_owned()),
        );
    }
    options.insert(
        "strict_tools_mode".to_owned(),
        Value::String(draft.provider_strict_tools_mode.as_str().to_owned()),
    );
    options.insert(
        "fim_model".to_owned(),
        Value::String(draft.provider_fim_model.trim().to_owned()),
    );
    Ok(())
}

fn load_deepseek_options(draft: &mut ConfigDraft, value: &Value) {
    let Some(options) = value.as_object() else {
        return;
    };
    if let Some(value) = option_string(options, "beta_base_url") {
        draft.provider_beta_base_url = value.to_owned();
    }
    if let Some(value) = option_string(options, "anthropic_base_url") {
        draft.provider_anthropic_base_url = value.to_owned();
    }
    draft.provider_user_id_strategy = option_string(options, "user_id_strategy")
        .unwrap_or_default()
        .to_owned();
    if let Some(value) = option_string(options, "fim_model") {
        draft.provider_fim_model = value.to_owned();
    }
    if let Some(value) = option_string(options, "strict_tools_mode") {
        draft.provider_strict_tools_mode = match value {
            "off" => sigil_runtime::ProviderStrictToolsMode::Off,
            "always" => sigil_runtime::ProviderStrictToolsMode::Always,
            _ => sigil_runtime::ProviderStrictToolsMode::Auto,
        };
    }
}

fn option_string<'a>(options: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    options.get(key).and_then(Value::as_str)
}

fn role_references_connection(root: &RootConfig, connection_id: &ConnectionId) -> bool {
    [
        &root.task.planner,
        &root.task.executor,
        &root.task.subagent_read,
        &root.task.subagent_write,
    ]
    .into_iter()
    .any(|role| role.connection.as_ref() == Some(connection_id))
}
