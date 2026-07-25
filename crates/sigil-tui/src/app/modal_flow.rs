use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use sigil_kernel::{ModelRequestConfig, SecretString};
use sigil_runtime::{
    DEFAULT_SETUP_PROVIDER_KEY, McpElicitationRequest, McpElicitationResponse,
    ProviderConfigFields, ProviderStatusConfig, default_provider_config_fields,
    normalize_provider_name, provider_api_key_env_name,
    provider_connections::{
        ModelCatalogEntry as ConnectionModelCatalogEntry,
        ModelCatalogRequest as ConnectionModelCatalogRequest,
        ModelCatalogResult as ConnectionModelCatalogResult,
        ModelCatalogState as ConnectionModelCatalogState, PreparedCredential,
        connection_semantic_fingerprint, load_provider_connections,
    },
    provider_model_status_config, provider_model_status_config_from_fields,
};

use super::{
    AppState, PaneFocus, TimelineRole,
    formatting::{ProviderModelIdentity, build_model_picker_options, non_empty_or},
};
use crate::commands::{keyboard_help_lines, metadata_slash_commands, metadata_slash_help_lines};
use crate::config_panel::{ConfigField, config_field_accepts_char};
use crate::runner::WorkerCommand;
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
            Self::Setup | Self::Provider => "Choose a provider-scoped model.",
            Self::ProviderFim => "Choose a provider-scoped FIM model.",
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct ModelPickerState {
    pub(super) target: ModelPickerTarget,
    pub(super) connection_id: Option<sigil_kernel::ConnectionId>,
    pub(super) provider_name: String,
    pub(super) current: String,
    pub(super) route_base_url: Option<String>,
    pub(super) catalog_state: ModelCatalogState,
    pub(super) catalog_entries: Vec<ConnectionModelCatalogEntry>,
    pub(super) manual_entry_allowed: bool,
    pub(super) options: Vec<ProviderModelIdentity>,
    pub(super) selected: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ModelCatalogState {
    Bundled,
    Loading,
    Remote,
    CacheFresh,
    CacheStale,
    Empty,
    AuthRejected,
    Offline,
    Unsupported,
    Malformed,
    TlsRejected,
    ProtocolMismatch,
    RateLimited(Option<u64>),
    CredentialUnavailable,
    Error(String),
}

impl ModelCatalogState {
    fn summary(&self) -> String {
        match self {
            Self::Bundled => "catalog: bundled provider models".to_owned(),
            Self::Loading => "catalog: loading remote provider models".to_owned(),
            Self::Remote => "catalog: remote provider models".to_owned(),
            Self::CacheFresh => "catalog: exact connection cache · fresh".to_owned(),
            Self::CacheStale => "catalog: exact connection cache · stale reference".to_owned(),
            Self::Empty => {
                "catalog: provider returned no models; press M to enter a model id".to_owned()
            }
            Self::Unsupported => {
                "catalog: remote discovery unsupported; bundled models only".to_owned()
            }
            Self::AuthRejected => {
                "catalog: credential rejected; update authentication or retry".to_owned()
            }
            Self::Offline => {
                "catalog: endpoint offline; exact cache/bundled rows are marked by source"
                    .to_owned()
            }
            Self::Malformed => {
                "catalog: provider returned a malformed or oversized response".to_owned()
            }
            Self::TlsRejected => "catalog: TLS validation rejected the endpoint".to_owned(),
            Self::ProtocolMismatch => {
                "catalog: endpoint does not match the selected provider protocol".to_owned()
            }
            Self::RateLimited(retry_after) => retry_after.map_or_else(
                || "catalog: provider rate limited discovery".to_owned(),
                |seconds| format!("catalog: rate limited; retry in at most {seconds}s"),
            ),
            Self::CredentialUnavailable => {
                "catalog: selected credential source is unavailable".to_owned()
            }
            Self::Error(error) => {
                format!("catalog: remote refresh failed; bundled models remain ({error})")
            }
        }
    }
}

#[derive(Debug)]
pub(super) struct ModelPickerRefresh {
    pub(super) target: ModelPickerTarget,
    pub(super) provider_name: String,
    pub(super) current: String,
    pub(super) base_url: String,
    pub(super) result: Result<Vec<String>, String>,
}

#[derive(Debug, Clone)]
pub(super) struct PendingModelPickerRefresh {
    pub(super) request_id: u64,
    pub(super) target: ModelPickerTarget,
    pub(super) provider_name: String,
    pub(super) current: String,
    pub(super) base_url: String,
    pub(super) connection_id: Option<sigil_kernel::ConnectionId>,
    pub(super) draft_revision: u64,
    pub(super) connection_fingerprint: Option<String>,
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

    fn summary(self, env_name: &str) -> String {
        match self {
            Self::SetupApiKey => {
                format!(
                    "Saved to the secure credential store. The value never enters sigil.toml; {env_name} is a separate selectable source."
                )
            }
            Self::ConfigProviderApiKey => {
                format!(
                    "Staged in memory for secure-store save. The value never enters sigil.toml; {env_name} is a separate source."
                )
            }
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct SecretInputState {
    pub(super) target: SecretInputTarget,
    pub(super) buffer: SecretString,
    pub(super) summary: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TextInputTarget {
    SetupModel,
    SetupEndpoint,
    ConfigManualModel,
    ConfigField(ConfigField),
    SkillArguments,
}

impl TextInputTarget {
    fn title(self) -> &'static str {
        match self {
            Self::SetupModel => "Model ID",
            Self::SetupEndpoint => "Custom Endpoint",
            Self::ConfigManualModel => "Model ID",
            Self::ConfigField(field) => field.display_label(),
            Self::SkillArguments => "Use Skill",
        }
    }

    fn summary(self) -> &'static str {
        match self {
            Self::SetupModel => "Custom model id.",
            Self::SetupEndpoint => {
                "HTTPS is required except for an explicit loopback development endpoint."
            }
            Self::ConfigManualModel => {
                "Custom model id admitted by the verified connection catalog."
            }
            Self::ConfigField(field) => field.help_text(),
            Self::SkillArguments => "Optional instructions for how to use the selected skill.",
        }
    }

    fn prompt_label(self) -> &'static str {
        match self {
            Self::SetupModel | Self::ConfigManualModel => "model",
            Self::SetupEndpoint => "endpoint",
            Self::ConfigField(_) => "value",
            Self::SkillArguments => "instructions",
        }
    }

    fn config_key(self) -> Option<&'static str> {
        match self {
            Self::SetupModel | Self::SetupEndpoint => None,
            Self::ConfigManualModel => Some(ConfigField::ProviderModel.label()),
            Self::ConfigField(field) => Some(field.label()),
            Self::SkillArguments => None,
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
    McpOAuth(super::mcp_oauth_flow::McpOAuthModalState),
    CheckpointRestore(super::checkpoint_flow::CheckpointRestoreModalState),
    V2CompactionPreview(Box<super::compaction_flow::V2CompactionPreviewModalState>),
    SessionActions(Box<super::session_lifecycle_flow::SessionActionsModalState>),
    SessionRetention(Box<super::session_lifecycle_flow::SessionRetentionModalState>),
    Feedback(Box<super::feedback_flow::FeedbackModalState>),
    KeyboardHelp,
}

#[derive(Debug, Clone)]
pub(super) enum ModalOutcome {
    None,
    Dismissed(String),
    ModelSelected {
        target: ModelPickerTarget,
        connection_id: Option<sigil_kernel::ConnectionId>,
        provider_name: String,
        value: String,
    },
    ManualModelRequested {
        target: ModelPickerTarget,
        current: String,
    },
    SecretSubmitted {
        target: SecretInputTarget,
        value: SecretString,
    },
    TextSubmitted {
        target: TextInputTarget,
        value: String,
    },
    V2CompactionConfirmed {
        request_id: u64,
    },
    V2CompactionDismissed {
        request_id: u64,
    },
}

impl AppState {
    pub fn modal_title(&self) -> Option<&'static str> {
        match self.modal_state.as_ref()? {
            ModalState::ModelPicker(state) => Some(state.target.title()),
            ModalState::SecretInput(state) => Some(state.target.title()),
            ModalState::TextInput(state) => Some(state.target.title()),
            ModalState::McpElicitation(_) => Some("MCP Elicitation"),
            ModalState::McpOAuth(_) => Some("MCP Authentication"),
            ModalState::CheckpointRestore(_) => Some("Restore Checkpoint"),
            ModalState::V2CompactionPreview(_) => Some("Context Compaction"),
            ModalState::SessionActions(_) => Some("Session Actions"),
            ModalState::SessionRetention(_) => Some("Storage Maintenance"),
            ModalState::Feedback(_) => Some("Feedback Report"),
            ModalState::KeyboardHelp => Some("Keyboard Help"),
        }
    }

    pub fn modal_lines(&self) -> Vec<String> {
        match self.modal_state.as_ref() {
            Some(ModalState::ModelPicker(state)) => {
                let actions = if state.manual_entry_allowed {
                    "Up/Down choose  Enter apply  M manual model id  Esc cancel"
                } else {
                    "Up/Down choose  Enter apply  Esc cancel"
                };
                let mut lines = vec![
                    state.target.summary().to_owned(),
                    format!("provider: {}", state.provider_name),
                    state.catalog_state.summary(),
                    actions.to_owned(),
                    String::new(),
                ];
                if !state.current.trim().is_empty()
                    && !state
                        .options
                        .iter()
                        .any(|option| option.model_id == state.current)
                {
                    let remediation = if state.manual_entry_allowed {
                        format!("not listed for {}; use M to edit", state.provider_name)
                    } else {
                        "not selectable in the current catalog state; repair or retry".to_owned()
                    };
                    lines.push(format!("configured: {}  [{remediation}]", state.current));
                    lines.push(String::new());
                }
                for (index, option) in state.options.iter().enumerate() {
                    let marker = if index == state.selected { ">" } else { " " };
                    let metadata = state.catalog_entries.iter().find(|entry| {
                        entry.model_ref.connection_id.as_str()
                            == option
                                .connection_id
                                .as_ref()
                                .map(sigil_kernel::ConnectionId::as_str)
                                .unwrap_or_default()
                            && entry.model_ref.model_id == option.model_id
                    });
                    let mut tags = Vec::new();
                    if option.model_id == state.current {
                        tags.push("current");
                    }
                    if metadata.is_some_and(|entry| {
                        entry.recommendation
                            == sigil_runtime::provider_connections::ModelRecommendation::Recommended
                    }) {
                        tags.push("recommended");
                    }
                    if metadata.is_some_and(|entry| {
                        entry.availability
                            == sigil_runtime::provider_connections::ModelAvailability::Unverified
                    }) {
                        tags.push("unverified");
                    }
                    if metadata.is_some_and(|entry| {
                        entry.availability
                            == sigil_runtime::provider_connections::ModelAvailability::ConfiguredUnavailable
                    }) {
                        tags.push("configured · not returned");
                    }
                    let suffix = if tags.is_empty() {
                        String::new()
                    } else {
                        format!("  [{}]", tags.join(" · "))
                    };
                    lines.push(format!("{marker} {}{suffix}", option.model_id));
                }
                lines.push(String::new());
                if state.manual_entry_allowed {
                    lines.push("  Enter model ID manually…  [M]".to_owned());
                }
                lines
            }
            Some(ModalState::SecretInput(state)) => vec![
                state.summary.clone(),
                "key: api_key".to_owned(),
                "Enter apply  F2 save  F3 save+close  Esc cancel".to_owned(),
                String::new(),
                format!("api_key: {}|", "*".repeat(state.buffer.char_count())),
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
            Some(ModalState::McpOAuth(state)) => super::mcp_oauth_flow::modal_lines(state),
            Some(ModalState::CheckpointRestore(_)) => Vec::new(),
            Some(ModalState::V2CompactionPreview(state)) => state.lines(),
            Some(ModalState::SessionActions(state)) => state.lines(),
            Some(ModalState::SessionRetention(state)) => state.lines(),
            Some(ModalState::Feedback(state)) => state.lines(self.terminal_height),
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
                Some(("api_key".to_owned(), state.buffer.char_count(), 4))
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
            ModalState::McpOAuth(state) => state.manual_callback.as_ref().map(|buffer| {
                (
                    "callback URL".to_owned(),
                    buffer.chars().count(),
                    super::mcp_oauth_flow::modal_lines(state)
                        .len()
                        .saturating_sub(2),
                )
            }),
            ModalState::ModelPicker(_) => None,
            ModalState::CheckpointRestore(_) => None,
            ModalState::V2CompactionPreview(_) => None,
            ModalState::SessionActions(_) | ModalState::SessionRetention(_) => None,
            ModalState::Feedback(_) => None,
            ModalState::KeyboardHelp => None,
        }
    }

    pub(super) fn open_model_picker(&mut self, target: ModelPickerTarget, current: &str) {
        let provider_name = self.provider_name_for_model_picker();
        let options = if matches!(
            target,
            ModelPickerTarget::Setup | ModelPickerTarget::Provider
        ) {
            Vec::new()
        } else {
            build_model_picker_options(&provider_name, current, None)
        };
        let selected = options
            .iter()
            .position(|option| option.model_id == current)
            .unwrap_or(0);
        self.modal_state = Some(ModalState::ModelPicker(ModelPickerState {
            target,
            connection_id: None,
            provider_name: provider_name.clone(),
            current: current.to_owned(),
            route_base_url: None,
            catalog_state: ModelCatalogState::Bundled,
            catalog_entries: Vec::new(),
            manual_entry_allowed: target == ModelPickerTarget::ProviderFim,
            options,
            selected,
        }));
        if matches!(
            target,
            ModelPickerTarget::Setup | ModelPickerTarget::Provider
        ) {
            if let Some(notice) = self.schedule_connection_model_refresh(target, current) {
                self.last_notice = Some(notice);
                return;
            }
            if let Some(ModalState::ModelPicker(state)) = self.modal_state.as_mut() {
                state.catalog_state = ModelCatalogState::Error(
                    "exact connection draft is unavailable; repair provider settings".to_owned(),
                );
                state.manual_entry_allowed = false;
            }
            self.last_notice =
                Some("model list unavailable: repair the exact connection settings".to_owned());
            return;
        }
        let notice = self.schedule_model_picker_refresh(target, &provider_name, current);
        self.last_notice = Some(notice);
    }

    fn schedule_connection_model_refresh(
        &mut self,
        target: ModelPickerTarget,
        current: &str,
    ) -> Option<String> {
        self.cancel_model_picker_refresh();
        let (root_config, connection_id, draft_revision, provider_name, prepared_credential) =
            match target {
                ModelPickerTarget::Setup => {
                    let state = self.setup_state.as_ref()?;
                    let root_config = super::setup_flow::build_setup_root_config(state).ok()?;
                    let loaded = load_provider_connections(&root_config);
                    let model_ref = loaded.default_model?;
                    let connection = loaded.connections.get(&model_ref.connection_id)?;
                    let prepared_credential = (state.credential_source
                        == crate::setup::SetupCredentialSource::SecureStore
                        && !state.api_key.expose_secret().trim().is_empty())
                    .then(|| {
                        PreparedCredential::api_key(
                            connection.config.provider,
                            state.api_key.expose_secret().trim().to_owned(),
                        )
                    });
                    (
                        root_config,
                        connection.config.id.clone(),
                        state.draft_revision,
                        state.provider_name.clone(),
                        prepared_credential,
                    )
                }
                ModelPickerTarget::Provider => {
                    if let Some(state) = self.config_state.as_ref() {
                        let root_config = state.draft.to_root_config().ok()?;
                        (
                            root_config,
                            state.draft.selected_connection_id.clone(),
                            state.draft_revision,
                            state.draft.provider_name.clone(),
                            state.draft.selected_prepared_credential(),
                        )
                    } else {
                        let root_config = self.config_snapshot.as_ref()?.clone();
                        let loaded = load_provider_connections(&root_config);
                        let model_ref = self
                            .runtime
                            .model_route
                            .as_ref()
                            .map(|route| route.model_ref.clone())
                            .or(loaded.default_model)?;
                        let connection = loaded.connections.get(&model_ref.connection_id)?;
                        (
                            root_config,
                            connection.config.id.clone(),
                            0,
                            self.runtime.provider_name.clone(),
                            None,
                        )
                    }
                }
                ModelPickerTarget::ProviderFim => return None,
            };
        let loaded = load_provider_connections(&root_config);
        let connection = loaded.connections.get(&connection_id)?;
        let fingerprint = connection_semantic_fingerprint(&connection.config);
        if let Some(ModalState::ModelPicker(picker)) = self.modal_state.as_mut() {
            picker.connection_id = Some(connection_id.clone());
            for option in &mut picker.options {
                option.connection_id = Some(connection_id.clone());
            }
            picker.catalog_state = ModelCatalogState::Loading;
            picker.manual_entry_allowed = false;
        }
        let request_id = self.next_background_request_id();
        self.runtime.active_model_picker_refresh = Some(PendingModelPickerRefresh {
            request_id,
            target,
            provider_name,
            current: current.to_owned(),
            base_url: String::new(),
            connection_id: Some(connection_id.clone()),
            draft_revision,
            connection_fingerprint: Some(fingerprint.clone()),
        });
        let request = ConnectionModelCatalogRequest {
            request_id,
            connection_id,
            draft_revision,
            connection_fingerprint: fingerprint,
            explicit_refresh: true,
        };
        if target == ModelPickerTarget::Setup {
            let (sender, receiver) = std::sync::mpsc::sync_channel(1);
            let cache_root = self.sigil_paths.cache_root.clone();
            let _ = std::thread::Builder::new()
                .name("sigil-setup-model-catalog".to_owned())
                .spawn(move || {
                    let result = (|| -> anyhow::Result<ConnectionModelCatalogResult> {
                        let runtime = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()?;
                        let service = sigil_runtime::provider_connections::ProviderModelCatalogService::new(
                            cache_root,
                            std::sync::Arc::new(
                                sigil_runtime::provider_connections::ConfiguredProviderCredentialStore::from_root_config(
                                    &root_config,
                                ),
                            ),
                            std::sync::Arc::new(
                                sigil_runtime::provider_connections::ProcessCredentialEnvironment,
                            ),
                        )?;
                        Ok(runtime.block_on(service.models_with_prepared_credential(
                            &root_config,
                            request,
                            prepared_credential.as_ref(),
                        )))
                    })();
                    let _ = sender.send(
                        result.map_err(|_| "model catalog worker unavailable".to_owned()),
                    );
                });
            self.runtime.setup_model_catalog_rx = Some(receiver);
        } else {
            self.enqueue_worker_command(WorkerCommand::RefreshConnectionModels {
                cache_root: self.sigil_paths.cache_root.clone(),
                root_config: Box::new(root_config),
                request,
                prepared_credential,
            });
        }
        Some("loading models for the exact connection".to_owned())
    }

    pub(super) fn apply_connection_model_catalog(
        &mut self,
        result: ConnectionModelCatalogResult,
    ) -> bool {
        let Some(ModalState::ModelPicker(state)) = self.modal_state.as_mut() else {
            return false;
        };
        if !matches!(
            state.target,
            ModelPickerTarget::Setup | ModelPickerTarget::Provider
        ) || state.connection_id.as_ref() != Some(&result.connection_id)
        {
            return false;
        }
        let selected = state.options.get(state.selected).cloned();
        state.options = result
            .entries
            .iter()
            .filter(|entry| entry.model_ref.connection_id == result.connection_id)
            .map(|entry| ProviderModelIdentity {
                connection_id: Some(entry.model_ref.connection_id.clone()),
                provider_name: state.provider_name.clone(),
                model_id: entry.model_ref.model_id.clone(),
            })
            .collect();
        state.catalog_entries = result.entries.clone();
        state.manual_entry_allowed = result.manual_entry_allowed;
        state.selected = selected
            .and_then(|selected| state.options.iter().position(|entry| entry == &selected))
            .or_else(|| {
                state
                    .options
                    .iter()
                    .position(|entry| entry.model_id == state.current)
            })
            .unwrap_or_default();
        state.catalog_state = match result.state {
            ConnectionModelCatalogState::Remote => ModelCatalogState::Remote,
            ConnectionModelCatalogState::CacheFresh => ModelCatalogState::CacheFresh,
            ConnectionModelCatalogState::CacheStale => ModelCatalogState::CacheStale,
            ConnectionModelCatalogState::Bundled => ModelCatalogState::Bundled,
            ConnectionModelCatalogState::Empty => ModelCatalogState::Empty,
            ConnectionModelCatalogState::AuthRejected => ModelCatalogState::AuthRejected,
            ConnectionModelCatalogState::Offline => ModelCatalogState::Offline,
            ConnectionModelCatalogState::Unsupported => ModelCatalogState::Unsupported,
            ConnectionModelCatalogState::Malformed => ModelCatalogState::Malformed,
            ConnectionModelCatalogState::TlsRejected => ModelCatalogState::TlsRejected,
            ConnectionModelCatalogState::ProtocolMismatch => ModelCatalogState::ProtocolMismatch,
            ConnectionModelCatalogState::RateLimited => {
                ModelCatalogState::RateLimited(result.retry_after_secs)
            }
            ConnectionModelCatalogState::CredentialUnavailable => {
                ModelCatalogState::CredentialUnavailable
            }
        };
        if state.target == ModelPickerTarget::Setup
            && let Some(setup) = self.setup_state.as_mut()
        {
            setup.catalog_admission = Some(crate::setup::SetupCatalogAdmission {
                draft_revision: result.draft_revision,
                available_models: result
                    .entries
                    .iter()
                    .filter(|entry| {
                        entry.availability
                            == sigil_runtime::provider_connections::ModelAvailability::Available
                    })
                    .map(|entry| entry.model_ref.model_id.clone())
                    .collect(),
                manual_entry_allowed: result.manual_entry_allowed,
                manual_model: None,
            });
        }
        let notice = format!(
            "model catalog {} for {}",
            result.state.code(),
            result.connection_id
        );
        self.last_notice = Some(notice.clone());
        self.push_event("model_list", notice);
        true
    }

    #[cfg(test)]
    pub(crate) fn pending_connection_model_refresh_for_test(
        &self,
    ) -> Option<(u64, sigil_kernel::ConnectionId, u64, String)> {
        let pending = self.runtime.active_model_picker_refresh.as_ref()?;
        Some((
            pending.request_id,
            pending.connection_id.clone()?,
            pending.draft_revision,
            pending.connection_fingerprint.clone()?,
        ))
    }

    fn provider_name_for_model_picker(&self) -> String {
        let provider_name = self
            .config_state
            .as_ref()
            .map(|state| state.draft.provider_name.as_str())
            .or_else(|| {
                self.setup_state
                    .as_ref()
                    .map(|state| state.provider_name.as_str())
            })
            .or_else(|| {
                self.config_snapshot
                    .as_ref()
                    .map(|config| config.agent.provider.as_str())
            })
            .unwrap_or(DEFAULT_SETUP_PROVIDER_KEY);
        normalize_provider_name(provider_name).to_owned()
    }

    fn schedule_model_picker_refresh(
        &mut self,
        target: ModelPickerTarget,
        provider_name: &str,
        current: &str,
    ) -> String {
        self.cancel_model_picker_refresh();
        let provider_config = match self.provider_status_config_for_model_picker(target, current) {
            Ok(Some(config)) => config,
            Ok(None) => {
                if let Some(ModalState::ModelPicker(state)) = self.modal_state.as_mut()
                    && state.target == target
                    && state.provider_name == provider_name
                    && state.current == current
                {
                    state.catalog_state = ModelCatalogState::Unsupported;
                }
                return format!("remote model discovery unsupported for {provider_name}");
            }
            Err(error) => {
                if let Some(ModalState::ModelPicker(state)) = self.modal_state.as_mut()
                    && state.target == target
                    && state.provider_name == provider_name
                    && state.current == current
                {
                    state.catalog_state = ModelCatalogState::Error(error.to_string());
                }
                return format!("model list unavailable for {provider_name}: {error}");
            }
        };
        let base_url = provider_config.base_url.clone();
        if let Some(ModalState::ModelPicker(state)) = self.modal_state.as_mut()
            && state.target == target
            && state.provider_name == provider_name
            && state.current == current
        {
            state.route_base_url = Some(base_url.clone());
            state.catalog_state = ModelCatalogState::Loading;
        }
        let request_id = self.next_background_request_id();
        self.runtime.active_model_picker_refresh = Some(PendingModelPickerRefresh {
            request_id,
            target,
            provider_name: provider_name.to_owned(),
            current: current.to_owned(),
            base_url: base_url.clone(),
            connection_id: None,
            draft_revision: 0,
            connection_fingerprint: None,
        });
        self.enqueue_worker_command(WorkerCommand::RefreshProviderModels {
            request_id,
            provider_config,
        });
        format!("loading provider model list ({base_url})")
    }

    pub(super) fn apply_model_picker_refresh(&mut self, refresh: ModelPickerRefresh) -> bool {
        let mut notice = None;
        if let Some(ModalState::ModelPicker(state)) = self.modal_state.as_mut() {
            if state.target != refresh.target
                || state.provider_name != refresh.provider_name
                || state.current != refresh.current
                || state.route_base_url.as_deref() != Some(refresh.base_url.as_str())
            {
                return false;
            }
            match refresh.result {
                Ok(remote) if !remote.is_empty() => {
                    let selected_value =
                        state
                            .options
                            .get(state.selected)
                            .cloned()
                            .unwrap_or_else(|| ProviderModelIdentity {
                                connection_id: state.connection_id.clone(),
                                provider_name: state.provider_name.clone(),
                                model_id: state.current.clone(),
                            });
                    state.options = build_model_picker_options(
                        &state.provider_name,
                        &state.current,
                        Some(remote),
                    );
                    state.selected = state
                        .options
                        .iter()
                        .position(|option| option == &selected_value)
                        .or_else(|| {
                            state
                                .options
                                .iter()
                                .position(|option| option.model_id == state.current)
                        })
                        .unwrap_or(0);
                    state.catalog_state = ModelCatalogState::Remote;
                    notice = Some(format!("loaded provider model list ({})", refresh.base_url));
                }
                Ok(_) => {
                    state.catalog_state = ModelCatalogState::Empty;
                    state.options.clear();
                    state.selected = 0;
                    notice = Some("provider returned an empty model list".to_owned());
                }
                Err(error) => {
                    state.catalog_state = ModelCatalogState::Error(error.clone());
                    notice = Some(format!("provider model list failed: {error}"));
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
    fn provider_status_config_for_model_picker(
        &self,
        target: ModelPickerTarget,
        current: &str,
    ) -> Result<Option<ProviderStatusConfig>, anyhow::Error> {
        if let Some(state) = &self.config_state {
            let fallback_model = match target {
                ModelPickerTarget::ProviderFim => state.draft.provider_model.trim(),
                _ => current.trim(),
            };
            let provider_name = state.draft.provider_name.as_str();
            let defaults = default_provider_config_fields(provider_name, fallback_model);
            let model_request = model_request_config_from_draft_or_default(
                &state.draft.model_request_timeout_secs,
                &state.draft.model_request_stream_idle_timeout_secs,
            );
            let fields = ProviderConfigFields {
                model: fallback_model.to_owned(),
                api_key: state
                    .draft
                    .provider_api_key
                    .expose_secret()
                    .trim()
                    .to_owned(),
                base_url: non_empty_or(&state.draft.provider_base_url, &defaults.base_url),
            };
            return provider_model_status_config_from_fields(
                provider_name,
                &fields,
                &model_request,
            );
        }

        if let Some(state) = &self.setup_state {
            let provider_name = state.provider_name.as_str();
            let defaults = default_provider_config_fields(provider_name, current.trim());
            let fields = ProviderConfigFields {
                model: current.trim().to_owned(),
                api_key: state.api_key.expose_secret().trim().to_owned(),
                base_url: defaults.base_url,
            };
            return provider_model_status_config_from_fields(
                provider_name,
                &fields,
                &ModelRequestConfig::default(),
            );
        }

        if let Some(root_config) = self.config_snapshot.as_ref() {
            return provider_model_status_config(root_config);
        }

        provider_model_status_config_from_fields(
            DEFAULT_SETUP_PROVIDER_KEY,
            &default_provider_config_fields(DEFAULT_SETUP_PROVIDER_KEY, current.trim()),
            &ModelRequestConfig::default(),
        )
    }

    pub(super) fn open_secret_input(
        &mut self,
        target: SecretInputTarget,
        current: impl Into<SecretString>,
    ) {
        let summary = self.secret_input_summary(target);
        self.modal_state = Some(ModalState::SecretInput(SecretInputState {
            target,
            buffer: current.into(),
            summary,
        }));
        self.last_notice = Some(format!("editing {}", target.title().to_lowercase()));
    }

    pub(super) fn open_secret_input_with_char(
        &mut self,
        target: SecretInputTarget,
        character: char,
    ) {
        let summary = self.secret_input_summary(target);
        self.modal_state = Some(ModalState::SecretInput(SecretInputState {
            target,
            buffer: SecretString::new(character.to_string()),
            summary,
        }));
        self.last_notice = Some(format!("editing {}", target.title().to_lowercase()));
    }

    fn secret_input_summary(&self, target: SecretInputTarget) -> String {
        let provider_name = match target {
            SecretInputTarget::SetupApiKey => self
                .setup_state
                .as_ref()
                .map(|state| state.provider_name.as_str()),
            SecretInputTarget::ConfigProviderApiKey => self
                .config_state
                .as_ref()
                .map(|state| state.draft.provider_name.as_str())
                .or_else(|| {
                    self.config_snapshot
                        .as_ref()
                        .map(|config| config.agent.provider.as_str())
                }),
        }
        .unwrap_or(DEFAULT_SETUP_PROVIDER_KEY);
        let env_name = provider_api_key_env_name(provider_name).unwrap_or("provider API key env");
        target.summary(env_name)
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
            ModalState::ModelPicker(state) => {
                match key.code {
                    KeyCode::Esc => {
                        self.cancel_model_picker_refresh();
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
                                .map(|option| option.model_id.clone())
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
                                .map(|option| option.model_id.clone())
                                .unwrap_or_default()
                        ));
                        ModalOutcome::None
                    }
                    KeyCode::Enter => {
                        let Some(identity) = state.options.get(state.selected).cloned() else {
                            self.last_notice = Some(if state.manual_entry_allowed {
                                "no verified model is selectable; retry or press M".to_owned()
                            } else {
                                "no verified model is selectable; repair connection or retry"
                                    .to_owned()
                            });
                            return ModalOutcome::None;
                        };
                        if matches!(
                            state.target,
                            ModelPickerTarget::Setup | ModelPickerTarget::Provider
                        ) && !state.catalog_entries.iter().any(|entry| {
                            identity.connection_id.as_ref().is_some_and(|connection_id| {
                            entry.model_ref.connection_id == *connection_id
                        })
                            && entry.model_ref.model_id == identity.model_id
                            && entry.availability
                                == sigil_runtime::provider_connections::ModelAvailability::Available
                        }) {
                            self.last_notice = Some(
                                "that model is an unverified reference; retry discovery or press M"
                                    .to_owned(),
                            );
                            return ModalOutcome::None;
                        }
                        let target = state.target;
                        self.cancel_model_picker_refresh();
                        self.modal_state = None;
                        ModalOutcome::ModelSelected {
                            target,
                            connection_id: identity.connection_id,
                            provider_name: identity.provider_name,
                            value: identity.model_id,
                        }
                    }
                    KeyCode::Char('m' | 'M') if state.manual_entry_allowed => {
                        let target = state.target;
                        let current = state.current.clone();
                        self.cancel_model_picker_refresh();
                        self.modal_state = None;
                        ModalOutcome::ManualModelRequested { target, current }
                    }
                    KeyCode::Char('m' | 'M') => {
                        self.last_notice = Some(
                            "manual model entry is unavailable until this connection is verified"
                                .to_owned(),
                        );
                        ModalOutcome::None
                    }
                    _ => ModalOutcome::None,
                }
            }
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
                    let value = std::mem::take(&mut state.buffer);
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
            ModalState::McpOAuth(_) => ModalOutcome::None,
            ModalState::CheckpointRestore(_) => ModalOutcome::None,
            ModalState::V2CompactionPreview(state) => match key.code {
                KeyCode::Esc => {
                    let request_id = state.request_id();
                    self.modal_state = None;
                    ModalOutcome::V2CompactionDismissed { request_id }
                }
                KeyCode::Enter if state.is_admitted() => {
                    let request_id = state.request_id();
                    self.modal_state = None;
                    ModalOutcome::V2CompactionConfirmed { request_id }
                }
                KeyCode::Enter => {
                    let request_id = state.request_id();
                    self.modal_state = None;
                    ModalOutcome::V2CompactionDismissed { request_id }
                }
                _ => ModalOutcome::None,
            },
            ModalState::SessionActions(_) | ModalState::SessionRetention(_) => ModalOutcome::None,
            ModalState::Feedback(_) => ModalOutcome::None,
            ModalState::KeyboardHelp => match key.code {
                KeyCode::Esc | KeyCode::Enter => {
                    self.modal_state = None;
                    ModalOutcome::Dismissed("closed keyboard help".to_owned())
                }
                _ => ModalOutcome::None,
            },
        }
    }

    pub(super) fn handle_modal_paste_text(&mut self, text: &str) -> ModalOutcome {
        let Some(modal_state) = self.modal_state.as_mut() else {
            return ModalOutcome::None;
        };

        match modal_state {
            ModalState::SecretInput(state) => {
                let accepted = text.chars().filter(|character| !character.is_control());
                let mut count = 0usize;
                for character in accepted {
                    state.buffer.push(character);
                    count += 1;
                }
                if count > 0 {
                    self.last_notice = Some("editing api key".to_owned());
                }
                ModalOutcome::None
            }
            ModalState::TextInput(state) => {
                let mut count = 0usize;
                for character in text.chars() {
                    if character.is_control()
                        || !text_input_target_accepts_char(state.target, character)
                    {
                        continue;
                    }
                    state.buffer.push(character);
                    count += 1;
                }
                if count > 0 {
                    self.last_notice = Some(format!("editing {}", state.target.prompt_label()));
                }
                ModalOutcome::None
            }
            ModalState::McpOAuth(state) => {
                if let Some(buffer) = state.manual_callback.as_mut() {
                    for character in text.chars().filter(|character| !character.is_control()) {
                        if buffer.len() >= 8 * 1024 {
                            break;
                        }
                        buffer.push(character);
                    }
                }
                ModalOutcome::None
            }
            ModalState::ModelPicker(_)
            | ModalState::McpElicitation(_)
            | ModalState::CheckpointRestore(_)
            | ModalState::V2CompactionPreview(_)
            | ModalState::SessionActions(_)
            | ModalState::SessionRetention(_)
            | ModalState::Feedback(_)
            | ModalState::KeyboardHelp => ModalOutcome::None,
        }
    }

    pub(super) fn submit_modal(&mut self) -> ModalOutcome {
        if matches!(self.modal_state, Some(ModalState::SecretInput(_))) {
            let Some(ModalState::SecretInput(state)) = self.modal_state.take() else {
                unreachable!("secret input was matched before taking modal state");
            };
            return ModalOutcome::SecretSubmitted {
                target: state.target,
                value: state.buffer,
            };
        }
        let Some(modal_state) = self.modal_state.as_ref() else {
            return ModalOutcome::None;
        };

        match modal_state {
            ModalState::ModelPicker(state) => {
                let Some(identity) = state.options.get(state.selected).cloned() else {
                    self.last_notice = Some(if state.manual_entry_allowed {
                        "no verified model is selectable; retry or press M".to_owned()
                    } else {
                        "no verified model is selectable; repair connection or retry".to_owned()
                    });
                    return ModalOutcome::None;
                };
                if matches!(
                    state.target,
                    ModelPickerTarget::Setup | ModelPickerTarget::Provider
                ) && !state.catalog_entries.iter().any(|entry| {
                    identity
                        .connection_id
                        .as_ref()
                        .is_some_and(|connection_id| {
                            entry.model_ref.connection_id == *connection_id
                        })
                        && entry.model_ref.model_id == identity.model_id
                        && entry.availability
                            == sigil_runtime::provider_connections::ModelAvailability::Available
                }) {
                    self.last_notice = Some(
                        "that model is an unverified reference; retry discovery or press M"
                            .to_owned(),
                    );
                    return ModalOutcome::None;
                };
                let target = state.target;
                self.cancel_model_picker_refresh();
                self.modal_state = None;
                ModalOutcome::ModelSelected {
                    target,
                    connection_id: identity.connection_id,
                    provider_name: identity.provider_name,
                    value: identity.model_id,
                }
            }
            ModalState::SecretInput(_) => {
                unreachable!("secret input is handled before borrowing modal state")
            }
            ModalState::TextInput(state) => {
                let target = state.target;
                let value = state.buffer.clone();
                self.modal_state = None;
                ModalOutcome::TextSubmitted { target, value }
            }
            ModalState::McpElicitation(_) => self.accept_mcp_elicitation(),
            ModalState::McpOAuth(_) => ModalOutcome::None,
            ModalState::CheckpointRestore(_) => ModalOutcome::None,
            ModalState::V2CompactionPreview(state) => {
                if state.is_admitted() {
                    let request_id = state.request_id();
                    self.modal_state = None;
                    ModalOutcome::V2CompactionConfirmed { request_id }
                } else {
                    let request_id = state.request_id();
                    self.modal_state = None;
                    ModalOutcome::V2CompactionDismissed { request_id }
                }
            }
            ModalState::SessionActions(_) | ModalState::SessionRetention(_) => ModalOutcome::None,
            ModalState::Feedback(_) => ModalOutcome::None,
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
            ModalOutcome::ModelSelected {
                target,
                connection_id,
                provider_name,
                value,
            } => {
                let active_provider = self.provider_name_for_model_picker();
                if active_provider != provider_name {
                    self.last_notice = Some(format!(
                        "ignored stale model selection for {provider_name}; active provider is {active_provider}"
                    ));
                    return;
                }
                if target == ModelPickerTarget::Setup
                    && let Some(expected_connection) = connection_id.as_ref()
                {
                    let current_connection = self
                        .setup_state
                        .as_ref()
                        .and_then(|state| super::setup_flow::build_setup_root_config(state).ok())
                        .and_then(|root| {
                            sigil_runtime::provider_connections::load_provider_connections(&root)
                                .default_model
                        })
                        .map(|model| model.connection_id);
                    if current_connection.as_ref() != Some(expected_connection) {
                        self.last_notice =
                            Some("ignored stale model selection for another connection".to_owned());
                        return;
                    }
                }
                match target {
                    ModelPickerTarget::Setup => {
                        if let Some(state) = self.setup_state.as_mut() {
                            state.model = value.clone();
                        }
                        self.last_notice = Some(format!("selected model {value}"));
                    }
                    ModelPickerTarget::Provider => {
                        if let Some(state) = self.config_state.as_mut() {
                            state.draft.provider_model = value.clone();
                            state.mark_dirty();
                        }
                        self.last_notice = Some(format!("selected model {value}"));
                    }
                    ModelPickerTarget::ProviderFim => {
                        if let Some(state) = self.config_state.as_mut() {
                            state.draft.provider_fim_model = value.clone();
                            state.mark_dirty();
                        }
                        self.last_notice = Some(format!("selected fim model {value}"));
                    }
                }
            }
            ModalOutcome::ManualModelRequested { target, current } => {
                let target = match target {
                    ModelPickerTarget::Setup => TextInputTarget::SetupModel,
                    ModelPickerTarget::Provider => TextInputTarget::ConfigManualModel,
                    ModelPickerTarget::ProviderFim => {
                        TextInputTarget::ConfigField(ConfigField::ProviderFimModel)
                    }
                };
                self.open_text_input(target, &current);
            }
            ModalOutcome::SecretSubmitted { target, value } => match target {
                SecretInputTarget::SetupApiKey => {
                    if let Some(state) = self.setup_state.as_mut() {
                        state.api_key = value;
                        state.bump_revision();
                    }
                    self.last_notice = Some("updated api key".to_owned());
                }
                SecretInputTarget::ConfigProviderApiKey => {
                    if let Some(state) = self.config_state.as_mut() {
                        state.draft.provider_api_key = value;
                        state.mark_dirty();
                    }
                    self.last_notice = Some("updated api key".to_owned());
                }
            },
            ModalOutcome::TextSubmitted { target, value } => match target {
                TextInputTarget::SetupModel => {
                    if let Some(state) = self.setup_state.as_mut() {
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
                }
                TextInputTarget::SetupEndpoint => {
                    if let Some(state) = self.setup_state.as_mut() {
                        state.base_url = value;
                        state.bump_revision();
                    }
                    self.last_notice = Some("updated custom endpoint".to_owned());
                }
                TextInputTarget::ConfigManualModel => {
                    if let Some(state) = self.config_state.as_mut() {
                        let changed = state.draft.provider_model != value;
                        state.draft.provider_model = value.clone();
                        if changed {
                            state.mark_dirty();
                        }
                    }
                    self.last_notice =
                        Some(format!("updated {}", ConfigField::ProviderModel.label()));
                }
                TextInputTarget::ConfigField(field) => {
                    if let Some(state) = self.config_state.as_mut() {
                        if field == ConfigField::AppearanceColorOverride {
                            match state
                                .draft
                                .set_selected_appearance_color_override(value.clone())
                            {
                                Ok(changed) => {
                                    if changed {
                                        state.dirty = true;
                                    }
                                }
                                Err(error) => {
                                    self.last_notice =
                                        Some(format!("invalid color override: {error}"));
                                    return;
                                }
                            }
                        } else if let Some(target) = state.field_text_value_mut(field) {
                            let changed = *target != value;
                            *target = value.clone();
                            if changed {
                                state.dirty = true;
                            }
                        }
                    }
                    self.last_notice = Some(format!("updated {}", field.label()));
                }
                TextInputTarget::SkillArguments => {
                    self.last_notice = Some("skill arguments submitted".to_owned());
                }
            },
            ModalOutcome::V2CompactionConfirmed { .. } => {
                self.last_notice = Some("applying V2 compaction".to_owned());
            }
            ModalOutcome::V2CompactionDismissed { .. } => {
                self.last_notice = Some("closed V2 compaction preview".to_owned());
            }
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

fn model_request_config_from_draft_or_default(
    request_timeout_secs: &str,
    stream_idle_timeout_secs: &str,
) -> ModelRequestConfig {
    let mut config = ModelRequestConfig::default();
    if let Ok(value) = request_timeout_secs.trim().parse::<u64>()
        && value > 0
    {
        config.request_timeout_secs = value;
    }
    if let Ok(value) = stream_idle_timeout_secs.trim().parse::<u64>()
        && value > 0
    {
        config.stream_idle_timeout_secs = value;
    }
    config
}

fn text_input_target_accepts_char(target: TextInputTarget, character: char) -> bool {
    match target {
        TextInputTarget::SetupModel
        | TextInputTarget::SetupEndpoint
        | TextInputTarget::ConfigManualModel => !character.is_control(),
        TextInputTarget::ConfigField(field) => config_field_accepts_char(field, character),
        TextInputTarget::SkillArguments => !character.is_control(),
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

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/modal_flow_detail_tests.rs"]
mod tests;
