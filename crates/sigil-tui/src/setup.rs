use std::{collections::BTreeMap, env, fmt, path::PathBuf};

use sigil_kernel::SecretString;
use sigil_runtime::{
    DEFAULT_SETUP_PROVIDER_KEY, NewInstallOrchestrationRolloutDecision, default_provider_model,
    new_install_orchestration_rollout_decision, provider_api_key_env_name,
    provider_connections::ProviderProtocol,
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
    Save,
}

impl SetupField {
    const STANDARD_ORDER: [Self; 4] = [Self::Provider, Self::ApiKey, Self::Model, Self::Save];
    const CUSTOM_ORDER: [Self; 6] = [
        Self::Provider,
        Self::Protocol,
        Self::Endpoint,
        Self::ApiKey,
        Self::Model,
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
    pub(crate) credential_source: SetupCredentialSource,
    pub(crate) api_key: SecretString,
    pub(crate) draft_revision: u64,
    pub(crate) startup_error: Option<String>,
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
            .field("credential_source", &self.credential_source)
            .field("api_key", &"[redacted]")
            .field("draft_revision", &self.draft_revision)
            .field("startup_error", &self.startup_error)
            .field("orchestration_rollout", &self.orchestration_rollout)
            .field("provider_draft_count", &self.provider_drafts.len())
            .finish()
    }
}

impl SetupState {
    pub(crate) fn new(config_path: PathBuf, startup_error: Option<String>) -> Self {
        let provider_name = DEFAULT_SETUP_PROVIDER_KEY.to_owned();
        let protocol = ProviderProtocol::DeepSeek;
        let base_url = default_endpoint(&provider_name, protocol).to_owned();
        let credential_source = default_credential_source(&provider_name);
        let model = default_provider_model(&provider_name)
            .expect("default setup provider must have a default model");
        let orchestration_rollout =
            new_install_orchestration_rollout_decision(&provider_name, &model);
        Self {
            config_path,
            selected_field: SetupField::Provider,
            model,
            api_key: SecretString::default(),
            draft_revision: 0,
            provider_name,
            protocol,
            base_url,
            credential_source,
            startup_error,
            orchestration_rollout,
            provider_drafts: BTreeMap::new(),
        }
    }

    pub(crate) fn is_custom(&self) -> bool {
        self.provider_name == "openai_compat"
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
            self.api_key = draft.api_key;
            self.base_url = draft.base_url;
            self.credential_source = draft.credential_source;
            self.protocol = draft.protocol;
        } else {
            self.protocol = default_protocol(&provider_name);
            self.base_url = default_endpoint(&provider_name, self.protocol).to_owned();
            self.model =
                default_provider_model(&provider_name).unwrap_or_else(|| "gpt-4.1".to_owned());
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
            return match provider_api_key_env_name(provider_name) {
                Some(name)
                    if env::var(name)
                        .ok()
                        .is_some_and(|value| !value.trim().is_empty()) =>
                {
                    format!("{name} detected · loopback no-auth available")
                }
                Some(name) => format!("API key ({name}) or loopback no-auth"),
                None => "API key or loopback no-auth".to_owned(),
            };
        }
        match provider_api_key_env_name(provider_name) {
            Some(name)
                if env::var(name)
                    .ok()
                    .is_some_and(|value| !value.trim().is_empty()) =>
            {
                format!("{name} detected")
            }
            Some(name) => format!("API key · {name} not set"),
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
        self.credential_source = default_credential_source_for_env(self.api_key_env_name());
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

    pub(crate) fn api_key_env_name(&self) -> Option<&'static str> {
        match self.protocol {
            ProviderProtocol::OpenAiResponses if self.is_custom() => {
                Some("SIGIL_OPENAI_RESPONSES_API_KEY")
            }
            ProviderProtocol::OpenAiChatCompletions => Some("SIGIL_OPENAI_COMPATIBLE_API_KEY"),
            _ => provider_api_key_env_name(&self.provider_name),
        }
    }

    pub(crate) fn environment_detected(&self) -> bool {
        self.api_key_env_name()
            .and_then(|name| env::var(name).ok())
            .is_some_and(|value| !value.trim().is_empty())
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
            SetupCredentialSource::Environment => match self.api_key_env_name() {
                Some(name) if self.environment_detected() => format!("environment {name} detected"),
                Some(name) => format!("environment {name} missing"),
                None => "environment unavailable".to_owned(),
            },
            SetupCredentialSource::SecureStore
                if self.api_key.expose_secret().trim().is_empty() =>
            {
                "protected store · key required".to_owned()
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

    fn capture_current_provider_draft(&mut self) {
        self.provider_drafts.insert(
            self.provider_name.clone(),
            SetupProviderDraft {
                model: self.model.clone(),
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
        self.orchestration_rollout =
            new_install_orchestration_rollout_decision(&self.provider_name, &self.model);
    }

    pub(crate) fn clear_staged_secrets(&mut self) {
        self.api_key.clear();
        for draft in self.provider_drafts.values_mut() {
            draft.api_key.clear();
        }
    }

    pub(crate) fn existing_config_repair_required(&self) -> bool {
        self.startup_error.is_some()
    }
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
    default_credential_source_for_env(provider_api_key_env_name(provider_name))
}

fn default_credential_source_for_env(env_name: Option<&str>) -> SetupCredentialSource {
    if env_name
        .and_then(|name| env::var(name).ok())
        .is_some_and(|value| !value.trim().is_empty())
    {
        SetupCredentialSource::Environment
    } else {
        SetupCredentialSource::SecureStore
    }
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/setup_tests.rs"]
mod tests;
