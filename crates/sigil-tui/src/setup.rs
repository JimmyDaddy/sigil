use std::{collections::BTreeMap, env, fmt, path::PathBuf};

use sigil_kernel::{PublicRouteRecoveryCode, SecretString};
use sigil_runtime::{
    DEFAULT_SETUP_PROVIDER_KEY, NewInstallOrchestrationRolloutDecision, default_provider_model,
    new_install_orchestration_rollout_decision,
    new_install_orchestration_rollout_decision_for_config, provider_api_key_env_names,
    provider_connections::{
        CredentialRefConfig, PersistedConfigSnapshot, ProviderFamily, ProviderProtocol,
        configured_environment_credential_available, load_provider_connections,
    },
};

pub(crate) const SETUP_PROVIDER_ORDER: [&str; 5] = [
    "deepseek",
    "openai_responses",
    "anthropic",
    "gemini",
    "openai_compat",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SetupField {
    Provider,
    Protocol,
    Endpoint,
    ApiKey,
    Model,
    ContextWindow,
    MaxOutputTokens,
    Save,
}

impl SetupField {
    const STANDARD_ORDER: [Self; 6] = [
        Self::Provider,
        Self::ApiKey,
        Self::Model,
        Self::ContextWindow,
        Self::MaxOutputTokens,
        Self::Save,
    ];
    const CUSTOM_ORDER: [Self; 8] = [
        Self::Provider,
        Self::Protocol,
        Self::Endpoint,
        Self::ApiKey,
        Self::Model,
        Self::ContextWindow,
        Self::MaxOutputTokens,
        Self::Save,
    ];

    fn order(custom: bool) -> &'static [Self] {
        if custom {
            &Self::CUSTOM_ORDER
        } else {
            &Self::STANDARD_ORDER
        }
    }

    pub(crate) fn next(self, custom: bool) -> Self {
        let order = Self::order(custom);
        let index = order
            .iter()
            .position(|field| *field == self)
            .unwrap_or_default();
        order[(index + 1) % order.len()]
    }

    pub(crate) fn previous(self, custom: bool) -> Self {
        let order = Self::order(custom);
        let index = order
            .iter()
            .position(|field| *field == self)
            .unwrap_or_default();
        if index == 0 {
            *order.last().expect("setup fields are non-empty")
        } else {
            order[index - 1]
        }
    }

    pub(crate) fn from_index(index: usize, custom: bool) -> Option<Self> {
        Self::order(custom).get(index).copied()
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Provider => "provider",
            Self::Protocol => "protocol",
            Self::Endpoint => "endpoint",
            Self::ApiKey => "authentication",
            Self::Model => "model",
            Self::ContextWindow => "context window",
            Self::MaxOutputTokens => "max output tokens",
            Self::Save => "review",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SetupCredentialSource {
    Environment,
    SecureStore,
    NoAuthentication,
}

impl SetupCredentialSource {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Environment => "detected environment",
            Self::SecureStore => "protected credential store",
            Self::NoAuthentication => "no authentication",
        }
    }
}

#[derive(Debug, Clone)]
struct SetupProviderDraft {
    model: String,
    context_window_tokens: String,
    api_key: SecretString,
    base_url: String,
    credential_source: SetupCredentialSource,
    protocol: ProviderProtocol,
}

#[derive(Clone)]
pub(crate) struct SetupState {
    pub(crate) config_path: PathBuf,
    pub(crate) selected_field: SetupField,
    pub(crate) provider_name: String,
    pub(crate) protocol: ProviderProtocol,
    pub(crate) base_url: String,
    pub(crate) model: String,
    pub(crate) context_window_tokens: String,
    pub(crate) max_output_tokens: String,
    pub(crate) credential_source: SetupCredentialSource,
    pub(crate) api_key: SecretString,
    pub(crate) draft_revision: u64,
    pub(crate) startup_error: Option<String>,
    pub(crate) startup_recovery_code: Option<PublicRouteRecoveryCode>,
    pub(crate) save_error: Option<String>,
    /// The exact source observed before setup opened. Its parsed state distinguishes malformed
    /// config from a valid-config boot failure, and its redacted bytes form the save-time CAS.
    pub(crate) startup_config: Option<PersistedConfigSnapshot>,
    pub(crate) orchestration_rollout: NewInstallOrchestrationRolloutDecision,
    provider_drafts: BTreeMap<String, SetupProviderDraft>,
}

impl fmt::Debug for SetupState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SetupState")
            .field("config_path", &self.config_path)
            .field("selected_field", &self.selected_field)
            .field("provider_name", &self.provider_name)
            .field("protocol", &self.protocol)
            .field("base_url", &"[redacted endpoint]")
            .field("model", &self.model)
            .field("context_window_tokens", &self.context_window_tokens)
            .field("max_output_tokens", &self.max_output_tokens)
            .field("credential_source", &self.credential_source)
            .field("api_key", &"[redacted]")
            .field("draft_revision", &self.draft_revision)
            .field("startup_error", &self.startup_error)
            .field("startup_recovery_code", &self.startup_recovery_code)
            .field("save_error_present", &self.save_error.is_some())
            .field("startup_config_present", &self.startup_config.is_some())
            .field("orchestration_rollout", &self.orchestration_rollout)
            .field("provider_draft_count", &self.provider_drafts.len())
            .finish()
    }
}

impl SetupState {
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn new(config_path: PathBuf, startup_error: Option<String>) -> Self {
        Self::new_with_recovery(config_path, startup_error, None)
    }

    pub(crate) fn new_with_recovery(
        config_path: PathBuf,
        startup_error: Option<String>,
        startup_recovery_code: Option<PublicRouteRecoveryCode>,
    ) -> Self {
        let startup_config = startup_error
            .as_ref()
            .and_then(|_| PersistedConfigSnapshot::load(&config_path).ok());
        let mut provider_name = DEFAULT_SETUP_PROVIDER_KEY.to_owned();
        let mut protocol = ProviderProtocol::DeepSeek;
        let mut base_url = default_endpoint(&provider_name, protocol).to_owned();
        let mut credential_source = default_credential_source(&provider_name);
        let mut model = default_provider_model(&provider_name)
            .expect("default setup provider must have a default model");
        let mut context_window_tokens = String::new();
        let mut max_output_tokens = String::new();
        let mut orchestration_rollout =
            new_install_orchestration_rollout_decision(&provider_name, &model);
        if let Some(root_config) = startup_config
            .as_ref()
            .and_then(PersistedConfigSnapshot::parsed)
            && let Some(existing) = existing_setup_values(root_config)
        {
            provider_name = existing.provider_name;
            protocol = existing.protocol;
            base_url = existing.base_url;
            credential_source = existing.credential_source;
            model = existing.model;
            context_window_tokens = existing.context_window_tokens;
            max_output_tokens = root_config
                .model_request
                .max_output_tokens
                .map_or_else(String::new, |tokens| tokens.to_string());
            orchestration_rollout =
                new_install_orchestration_rollout_decision_for_config(root_config);
        }
        Self {
            config_path,
            selected_field: SetupField::Provider,
            model,
            context_window_tokens,
            max_output_tokens,
            api_key: SecretString::default(),
            draft_revision: 0,
            provider_name,
            protocol,
            base_url,
            credential_source,
            startup_error,
            startup_recovery_code,
            save_error: None,
            startup_config,
            orchestration_rollout,
            provider_drafts: BTreeMap::new(),
        }
    }

    pub(crate) fn is_custom(&self) -> bool {
        self.provider_name == "openai_compat"
    }

    /// Safe mode is only valid when the persisted config parsed successfully and a later boot
    /// phase failed. It must never mask malformed or missing configuration.
    pub(crate) fn provider_only_safe_mode_config(&self) -> Option<sigil_kernel::RootConfig> {
        if !self.valid_config_boot_retry_required()
            || !matches!(
                self.startup_recovery_code,
                Some(
                    PublicRouteRecoveryCode::AuthorityUnavailable
                        | PublicRouteRecoveryCode::AuthorityJournalCorrupted
                )
            )
            || env::var_os("SSL_CERT_FILE").is_some()
        {
            return None;
        }
        let root_config = self.startup_config.as_ref()?.parsed()?.clone();
        let loaded = load_provider_connections(&root_config);
        let model_ref = loaded.default_model.as_ref()?;
        let connection = loaded.connections.get(&model_ref.connection_id)?;
        match &connection.config.credential {
            CredentialRefConfig::Environment { name } => {
                configured_environment_credential_available(&connection.config, name)
                    .then_some(root_config)
            }
            CredentialRefConfig::None => Some(root_config),
            CredentialRefConfig::Stored { .. } => None,
        }
    }

    pub(crate) fn cycle_provider(&mut self) {
        let index = SETUP_PROVIDER_ORDER
            .iter()
            .position(|provider| *provider == self.provider_name)
            .unwrap_or_default();
        self.select_provider_index((index + 1) % SETUP_PROVIDER_ORDER.len());
    }

    pub(crate) fn cycle_provider_previous(&mut self) {
        let index = SETUP_PROVIDER_ORDER
            .iter()
            .position(|provider| *provider == self.provider_name)
            .unwrap_or_default();
        self.select_provider_index(
            index
                .checked_sub(1)
                .unwrap_or(SETUP_PROVIDER_ORDER.len() - 1),
        );
    }

    pub(crate) fn select_provider_index(&mut self, index: usize) {
        let Some(provider_name) = SETUP_PROVIDER_ORDER.get(index) else {
            return;
        };
        if self.provider_name == *provider_name {
            return;
        }
        self.capture_current_provider_draft();
        self.provider_name = (*provider_name).to_owned();
        let provider_name = self.provider_name.clone();
        if let Some(draft) = self.provider_drafts.get(&provider_name).cloned() {
            self.model = draft.model;
            self.context_window_tokens = draft.context_window_tokens;
            self.api_key = draft.api_key;
            self.base_url = draft.base_url;
            self.credential_source = draft.credential_source;
            self.protocol = draft.protocol;
        } else {
            self.protocol = default_protocol(&provider_name);
            self.base_url = default_endpoint(&provider_name, self.protocol).to_owned();
            self.model =
                default_provider_model(&provider_name).unwrap_or_else(|| "gpt-4.1".to_owned());
            self.context_window_tokens.clear();
            self.api_key.clear();
            self.credential_source = default_credential_source(&provider_name);
        }
        self.selected_field = SetupField::Provider;
        self.bump_revision();
        self.refresh_orchestration_rollout();
    }

    #[must_use]
    pub(crate) fn provider_index(&self) -> usize {
        SETUP_PROVIDER_ORDER
            .iter()
            .position(|provider| *provider == self.provider_name)
            .unwrap_or_default()
    }

    #[must_use]
    pub(crate) fn provider_choice_label(provider_name: &str) -> &'static str {
        match provider_name {
            "deepseek" => "DeepSeek",
            "openai_responses" => "OpenAI",
            "anthropic" => "Anthropic",
            "gemini" => "Google Gemini",
            "openai_compat" => "Custom endpoint",
            _ => "Unknown",
        }
    }

    #[must_use]
    pub(crate) fn provider_choice_auth_summary(provider_name: &str) -> String {
        if provider_name == "openai_compat" {
            return match provider_api_key_env_names(provider_name) {
                Some(names) if detected_environment_name(names).is_some() => format!(
                    "{} detected · loopback no-auth available",
                    detected_environment_name(names).expect("detected environment name")
                ),
                Some(names) => format!(
                    "API key ({}) or loopback no-auth",
                    format_environment_names(names)
                ),
                None => "API key or loopback no-auth".to_owned(),
            };
        }
        match provider_api_key_env_names(provider_name) {
            Some(names) if detected_environment_name(names).is_some() => format!(
                "{} detected",
                detected_environment_name(names).expect("detected environment name")
            ),
            Some(names) => format!("API key · {}", missing_environment_names(names)),
            None => "authentication required".to_owned(),
        }
    }

    pub(crate) fn cycle_protocol(&mut self) {
        if !self.is_custom() {
            return;
        }
        self.protocol = match self.protocol {
            ProviderProtocol::OpenAiResponses => ProviderProtocol::OpenAiChatCompletions,
            _ => ProviderProtocol::OpenAiResponses,
        };
        self.base_url = default_endpoint(&self.provider_name, self.protocol).to_owned();
        self.api_key.clear();
        self.credential_source = default_credential_source_for_env(self.api_key_env_names());
        self.bump_revision();
    }

    pub(crate) fn cycle_credential_source(&mut self) {
        self.credential_source = match self.credential_source {
            SetupCredentialSource::Environment => SetupCredentialSource::SecureStore,
            SetupCredentialSource::SecureStore if self.no_authentication_allowed() => {
                SetupCredentialSource::NoAuthentication
            }
            SetupCredentialSource::SecureStore | SetupCredentialSource::NoAuthentication => {
                SetupCredentialSource::Environment
            }
        };
        if self.credential_source != SetupCredentialSource::SecureStore {
            self.api_key.clear();
        }
        self.bump_revision();
    }

    pub(crate) fn api_key_env_names(&self) -> &'static [&'static str] {
        match self.protocol {
            ProviderProtocol::OpenAiResponses if self.is_custom() => {
                provider_api_key_env_names("openai_responses").unwrap_or(&[])
            }
            ProviderProtocol::OpenAiChatCompletions => {
                provider_api_key_env_names("openai_compat").unwrap_or(&[])
            }
            _ => provider_api_key_env_names(&self.provider_name).unwrap_or(&[]),
        }
    }

    pub(crate) fn api_key_env_name(&self) -> Option<&'static str> {
        self.api_key_env_names().first().copied()
    }

    pub(crate) fn environment_detected(&self) -> bool {
        self.detected_api_key_env_name().is_some()
    }

    pub(crate) fn detected_api_key_env_name(&self) -> Option<String> {
        if let Some(CredentialRefConfig::Environment { name }) = self.reusable_existing_credential()
        {
            if self.api_key_env_name() == Some(name.as_str()) {
                return detected_environment_name(self.api_key_env_names()).map(str::to_owned);
            }
            return env::var(&name)
                .ok()
                .filter(|value| !value.trim().is_empty())
                .map(|_| name);
        }
        detected_environment_name(self.api_key_env_names()).map(str::to_owned)
    }

    pub(crate) fn environment_credential_name(&self) -> Option<String> {
        match self.reusable_existing_credential() {
            Some(CredentialRefConfig::Environment { name }) => Some(name),
            // Persist the exact detected source. A setup that was validated through
            // `DEEPSEEK_API_KEY` must not silently write `SIGIL_API_KEY` and only fail on the
            // first provider request.
            _ => self
                .detected_api_key_env_name()
                .or_else(|| self.api_key_env_name().map(str::to_owned)),
        }
    }

    pub(crate) fn can_reuse_stored_credential(&self) -> bool {
        matches!(
            self.reusable_existing_credential(),
            Some(CredentialRefConfig::Stored { .. })
        )
    }

    pub(crate) fn no_authentication_allowed(&self) -> bool {
        self.is_custom()
            && url::Url::parse(&self.base_url).is_ok_and(|url| {
                matches!(url.scheme(), "http" | "https")
                    && url
                        .host_str()
                        .is_some_and(|host| matches!(host, "localhost" | "127.0.0.1" | "::1"))
            })
    }

    pub(crate) fn masked_api_key(&self) -> String {
        if self.api_key.is_empty() {
            "<not staged>".to_owned()
        } else {
            "*".repeat(self.api_key.char_count().max(8))
        }
    }

    pub(crate) fn auth_summary(&self) -> String {
        match self.credential_source {
            SetupCredentialSource::Environment => {
                if let Some(name) = self.detected_api_key_env_name() {
                    format!("environment {name} detected")
                } else if self.api_key_env_name().is_some() {
                    format!(
                        "environment {}",
                        missing_environment_names(self.api_key_env_names())
                    )
                } else {
                    "environment unavailable".to_owned()
                }
            }
            SetupCredentialSource::SecureStore
                if self.api_key.expose_secret().trim().is_empty() =>
            {
                if self.can_reuse_stored_credential() {
                    "protected store · existing credential reference".to_owned()
                } else {
                    "protected store · key required".to_owned()
                }
            }
            SetupCredentialSource::SecureStore => {
                "protected store · credential staged in memory".to_owned()
            }
            SetupCredentialSource::NoAuthentication => {
                "no authentication · local endpoint only".to_owned()
            }
        }
    }

    pub(crate) fn provider_label(&self) -> &'static str {
        Self::provider_choice_label(&self.provider_name)
    }

    pub(crate) fn set_model(&mut self, model: String) -> bool {
        let model = model.trim().to_owned();
        if self.model == model {
            return false;
        }
        self.model = model;
        self.context_window_tokens.clear();
        self.bump_revision();
        self.refresh_orchestration_rollout();
        true
    }

    fn capture_current_provider_draft(&mut self) {
        self.provider_drafts.insert(
            self.provider_name.clone(),
            SetupProviderDraft {
                model: self.model.clone(),
                context_window_tokens: self.context_window_tokens.clone(),
                api_key: self.api_key.clone(),
                base_url: self.base_url.clone(),
                credential_source: self.credential_source,
                protocol: self.protocol,
            },
        );
    }

    pub(crate) fn bump_revision(&mut self) {
        self.draft_revision = self.draft_revision.saturating_add(1);
    }

    pub(crate) fn refresh_orchestration_rollout(&mut self) {
        self.orchestration_rollout = self
            .startup_config
            .as_ref()
            .and_then(PersistedConfigSnapshot::parsed)
            .map(new_install_orchestration_rollout_decision_for_config)
            .unwrap_or_else(|| {
                new_install_orchestration_rollout_decision(&self.provider_name, &self.model)
            });
    }

    pub(crate) fn clear_staged_secrets(&mut self) {
        self.api_key.clear();
        for draft in self.provider_drafts.values_mut() {
            draft.api_key.clear();
        }
    }

    pub(crate) fn existing_config_repair_required(&self) -> bool {
        self.startup_error.is_some()
            && self
                .startup_config
                .as_ref()
                .is_some_and(PersistedConfigSnapshot::is_invalid)
    }

    pub(crate) fn valid_config_boot_retry_required(&self) -> bool {
        self.startup_error.is_some()
            && self
                .startup_config
                .as_ref()
                .is_some_and(|snapshot| snapshot.parsed().is_some())
    }

    pub(crate) fn startup_config_snapshot_missing(&self) -> bool {
        self.startup_error.is_some() && self.startup_config.is_none()
    }

    fn reusable_existing_credential(&self) -> Option<CredentialRefConfig> {
        let root_config = self.startup_config.as_ref()?.parsed()?;
        let loaded = load_provider_connections(root_config);
        let model_ref = loaded.default_model.as_ref()?;
        let connection = loaded.connections.get(&model_ref.connection_id)?;
        setup_identity_matches(
            &self.provider_name,
            self.protocol,
            connection.config.provider,
            connection.config.protocol,
        )
        .then(|| connection.config.credential.clone())
    }
}

struct ExistingSetupValues {
    provider_name: String,
    protocol: ProviderProtocol,
    base_url: String,
    model: String,
    context_window_tokens: String,
    credential_source: SetupCredentialSource,
}

fn existing_setup_values(root_config: &sigil_kernel::RootConfig) -> Option<ExistingSetupValues> {
    let loaded = load_provider_connections(root_config);
    let model_ref = loaded.default_model.as_ref()?;
    let connection = loaded.connections.get(&model_ref.connection_id)?;
    let provider_name =
        setup_provider_name(connection.config.provider, connection.config.protocol)?;
    let credential_source = match &connection.config.credential {
        CredentialRefConfig::Environment { .. } => SetupCredentialSource::Environment,
        CredentialRefConfig::Stored { .. } => SetupCredentialSource::SecureStore,
        CredentialRefConfig::None => SetupCredentialSource::NoAuthentication,
    };
    Some(ExistingSetupValues {
        provider_name: provider_name.to_owned(),
        protocol: connection.config.protocol,
        base_url: connection.config.base_url.clone(),
        model: model_ref.model_id.clone(),
        context_window_tokens: connection
            .config
            .model_context_windows
            .get(&model_ref.model_id)
            .map_or_else(String::new, u32::to_string),
        credential_source,
    })
}

fn setup_provider_name(family: ProviderFamily, protocol: ProviderProtocol) -> Option<&'static str> {
    match (family, protocol) {
        (ProviderFamily::DeepSeek, ProviderProtocol::DeepSeek) => Some("deepseek"),
        (ProviderFamily::OpenAi, ProviderProtocol::OpenAiResponses) => Some("openai_responses"),
        (ProviderFamily::Anthropic, ProviderProtocol::AnthropicMessages) => Some("anthropic"),
        (ProviderFamily::Gemini, ProviderProtocol::GeminiGenerateContent) => Some("gemini"),
        (
            ProviderFamily::Custom | ProviderFamily::OpenAi,
            ProviderProtocol::OpenAiChatCompletions,
        )
        | (ProviderFamily::Custom, ProviderProtocol::OpenAiResponses) => Some("openai_compat"),
        _ => None,
    }
}

fn setup_identity_matches(
    provider_name: &str,
    protocol: ProviderProtocol,
    family: ProviderFamily,
    existing_protocol: ProviderProtocol,
) -> bool {
    setup_provider_name(family, existing_protocol) == Some(provider_name)
        && protocol == existing_protocol
}

fn default_protocol(provider_name: &str) -> ProviderProtocol {
    match provider_name {
        "deepseek" => ProviderProtocol::DeepSeek,
        "openai_responses" => ProviderProtocol::OpenAiResponses,
        "anthropic" => ProviderProtocol::AnthropicMessages,
        "gemini" => ProviderProtocol::GeminiGenerateContent,
        "openai_compat" => ProviderProtocol::OpenAiChatCompletions,
        _ => ProviderProtocol::OpenAiResponses,
    }
}

fn default_endpoint(provider_name: &str, protocol: ProviderProtocol) -> &'static str {
    match (provider_name, protocol) {
        ("deepseek", ProviderProtocol::DeepSeek) => "https://api.deepseek.com",
        ("openai_responses", ProviderProtocol::OpenAiResponses) => "https://api.openai.com/v1",
        ("anthropic", ProviderProtocol::AnthropicMessages) => "https://api.anthropic.com",
        ("gemini", ProviderProtocol::GeminiGenerateContent) => {
            "https://generativelanguage.googleapis.com/v1beta"
        }
        ("openai_compat", ProviderProtocol::OpenAiResponses)
        | ("openai_compat", ProviderProtocol::OpenAiChatCompletions) => "http://127.0.0.1:8000/v1",
        _ => "https://api.openai.com/v1",
    }
}

fn default_credential_source(provider_name: &str) -> SetupCredentialSource {
    default_credential_source_for_env(provider_api_key_env_names(provider_name).unwrap_or(&[]))
}

fn default_credential_source_for_env(env_names: &'static [&'static str]) -> SetupCredentialSource {
    if detected_environment_name(env_names).is_some() {
        SetupCredentialSource::Environment
    } else {
        SetupCredentialSource::SecureStore
    }
}

fn detected_environment_name(names: &'static [&'static str]) -> Option<&'static str> {
    names.iter().copied().find(|name| {
        env::var(name)
            .ok()
            .is_some_and(|value| !value.trim().is_empty())
    })
}

fn format_environment_names(names: &[&str]) -> String {
    names.join(" or ")
}

fn missing_environment_names(names: &[&str]) -> String {
    let Some((canonical, aliases)) = names.split_first() else {
        return "unavailable".to_owned();
    };
    if aliases.is_empty() {
        format!("{canonical} not set")
    } else {
        format!(
            "{canonical} not set (aliases: {})",
            format_environment_names(aliases)
        )
    }
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/setup_tests.rs"]
mod tests;
