use std::{collections::BTreeMap, fmt, str::FromStr};

use anyhow::{Context, Result};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use sigil_kernel::ConnectionId;
use url::Url;
use uuid::Uuid;

const CONNECTION_LABEL_MAX_BYTES: usize = 80;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderFamily {
    #[serde(rename = "deepseek")]
    DeepSeek,
    #[serde(rename = "openai")]
    OpenAi,
    Anthropic,
    Gemini,
    Custom,
}

impl ProviderFamily {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DeepSeek => "deepseek",
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
            Self::Gemini => "gemini",
            Self::Custom => "custom",
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::DeepSeek => "DeepSeek",
            Self::OpenAi => "OpenAI",
            Self::Anthropic => "Anthropic",
            Self::Gemini => "Google Gemini",
            Self::Custom => "Custom",
        }
    }
}

impl fmt::Display for ProviderFamily {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ProviderProtocol {
    #[serde(rename = "deepseek")]
    DeepSeek,
    #[serde(rename = "responses")]
    OpenAiResponses,
    #[serde(rename = "chat_completions")]
    OpenAiChatCompletions,
    #[serde(rename = "anthropic_messages")]
    AnthropicMessages,
    #[serde(rename = "generate_content")]
    GeminiGenerateContent,
}

impl ProviderProtocol {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DeepSeek => "deepseek",
            Self::OpenAiResponses => "responses",
            Self::OpenAiChatCompletions => "chat_completions",
            Self::AnthropicMessages => "anthropic_messages",
            Self::GeminiGenerateContent => "generate_content",
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::DeepSeek => "DeepSeek",
            Self::OpenAiResponses => "Responses",
            Self::OpenAiChatCompletions => "Chat Completions",
            Self::AnthropicMessages => "Messages",
            Self::GeminiGenerateContent => "GenerateContent",
        }
    }
}

impl fmt::Display for ProviderProtocol {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct CredentialId(Uuid);

impl CredentialId {
    #[must_use]
    pub fn random() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn parse(value: &str) -> Result<Self> {
        let parsed = Uuid::parse_str(value).context("credential id must be a UUID")?;
        anyhow::ensure!(
            parsed.get_version_num() == 4,
            "credential id must be a random UUIDv4"
        );
        Ok(Self(parsed))
    }

    #[must_use]
    pub fn as_uuid(&self) -> Uuid {
        self.0
    }
}

impl fmt::Display for CredentialId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FromStr for CredentialId {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl<'de> Deserialize<'de> for CredentialId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum CredentialRefConfig {
    Environment { name: String },
    Stored { id: CredentialId },
    None,
}

impl fmt::Debug for CredentialRefConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Environment { name } => formatter
                .debug_struct("Environment")
                .field("name", name)
                .finish(),
            Self::Stored { .. } => formatter.write_str("Stored([redacted])"),
            Self::None => formatter.write_str("None"),
        }
    }
}

#[derive(Clone, PartialEq, Serialize)]
pub struct ProviderConnectionConfig {
    #[serde(skip)]
    pub id: ConnectionId,
    pub label: String,
    pub provider: ProviderFamily,
    pub protocol: ProviderProtocol,
    pub base_url: String,
    pub credential: CredentialRefConfig,
    /// Optional exact context-window limits keyed by provider model id.
    pub model_context_windows: BTreeMap<String, u32>,
    pub options: Value,
}

impl fmt::Debug for ProviderConnectionConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderConnectionConfig")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("provider", &self.provider)
            .field("protocol", &self.protocol)
            .field("base_url", &"[redacted endpoint]")
            .field("credential", &credential_debug_label(&self.credential))
            .field("model_context_windows", &self.model_context_windows)
            .field("options", &"[redacted provider options]")
            .finish()
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderConnectionWire {
    label: String,
    provider: ProviderFamily,
    protocol: ProviderProtocol,
    base_url: String,
    credential: CredentialRefConfig,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    model_context_windows: BTreeMap<String, u32>,
    #[serde(default = "empty_options")]
    options: Value,
}

fn empty_options() -> Value {
    Value::Object(Default::default())
}

impl ProviderConnectionConfig {
    pub fn from_raw(id: ConnectionId, value: Value) -> Result<Self> {
        let wire: ProviderConnectionWire =
            serde_json::from_value(value).context("invalid provider connection")?;
        let config = Self {
            id,
            label: wire.label,
            provider: wire.provider,
            protocol: wire.protocol,
            base_url: wire.base_url,
            credential: wire.credential,
            model_context_windows: wire.model_context_windows,
            options: wire.options,
        };
        config.validate()?;
        Ok(config)
    }

    pub fn to_raw(&self) -> Result<Value> {
        self.validate()?;
        serde_json::to_value(ProviderConnectionWire {
            label: self.label.clone(),
            provider: self.provider,
            protocol: self.protocol,
            base_url: self.base_url.clone(),
            credential: self.credential.clone(),
            model_context_windows: self.model_context_windows.clone(),
            options: self.options.clone(),
        })
        .context("failed to serialize provider connection")
    }

    pub fn validate(&self) -> Result<()> {
        validate_connection_label(&self.label)?;
        validate_family_protocol(self.provider, self.protocol)?;
        validate_credential_ref(self.provider, self.protocol, &self.credential)?;
        validate_endpoint(self.provider, &self.credential, &self.base_url)?;
        for (model_id, context_window_tokens) in &self.model_context_windows {
            sigil_kernel::ModelRef::new(self.id.clone(), model_id.clone())?;
            anyhow::ensure!(
                *context_window_tokens > 0,
                "model context window must be greater than zero"
            );
        }
        anyhow::ensure!(
            self.options.is_object(),
            "provider connection options must be an object"
        );
        validate_provider_option_tree(&self.options, "options")?;
        validate_provider_option_endpoints(
            self.provider,
            self.protocol,
            &self.credential,
            &self.options,
        )?;
        crate::provider_factory::validate_connection_provider_options(self)?;
        Ok(())
    }
}

fn credential_debug_label(credential: &CredentialRefConfig) -> &'static str {
    match credential {
        CredentialRefConfig::Environment { .. } => "environment",
        CredentialRefConfig::Stored { .. } => "stored",
        CredentialRefConfig::None => "none",
    }
}

fn provider_option_key_is_reserved_or_sensitive(key: &str) -> bool {
    let normalized = key.trim().to_ascii_lowercase().replace('-', "_");
    let compact = normalized
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>();
    matches!(
        normalized.as_str(),
        "id" | "label"
            | "provider"
            | "protocol"
            | "base_url"
            | "model"
            | "credential"
            | "__runtime_model"
            | "authorization"
            | "auth"
            | "token"
            | "secret"
            | "password"
    ) || normalized == "api_key"
        || normalized.ends_with("_api_key")
        || normalized.ends_with("_token")
        || normalized.ends_with("_secret")
        || normalized.ends_with("_password")
        || normalized.ends_with("_credential")
        || matches!(
            compact.as_str(),
            "apikey"
                | "authorization"
                | "authentication"
                | "auth"
                | "token"
                | "secret"
                | "password"
                | "credential"
                | "credentials"
        )
        || compact.ends_with("apikey")
        || compact.ends_with("token")
        || compact.ends_with("secret")
        || compact.ends_with("password")
        || compact.ends_with("credential")
        || compact.ends_with("credentials")
}

fn validate_provider_option_tree(value: &Value, path: &str) -> Result<()> {
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                anyhow::ensure!(
                    !provider_option_key_is_reserved_or_sensitive(key),
                    "provider connection option {path}.{key} is reserved or credential-like"
                );
                validate_provider_option_tree(child, &format!("{path}.{key}"))?;
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                validate_provider_option_tree(child, &format!("{path}[{index}]"))?;
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
    Ok(())
}

pub fn provider_connection_template(
    family: ProviderFamily,
    protocol: ProviderProtocol,
    id: ConnectionId,
    label: impl Into<String>,
) -> Result<(ProviderConnectionConfig, String)> {
    validate_family_protocol(family, protocol)?;
    let (base_url, environment, model) = match (family, protocol) {
        (ProviderFamily::DeepSeek, ProviderProtocol::DeepSeek) => (
            "https://api.deepseek.com",
            "SIGIL_API_KEY",
            "deepseek-v4-flash",
        ),
        (ProviderFamily::OpenAi, ProviderProtocol::OpenAiResponses) => (
            "https://api.openai.com/v1",
            "SIGIL_OPENAI_RESPONSES_API_KEY",
            "gpt-4.1",
        ),
        (ProviderFamily::Anthropic, ProviderProtocol::AnthropicMessages) => (
            "https://api.anthropic.com",
            "SIGIL_ANTHROPIC_API_KEY",
            "claude-sonnet-4-5",
        ),
        (ProviderFamily::Gemini, ProviderProtocol::GeminiGenerateContent) => (
            "https://generativelanguage.googleapis.com/v1beta",
            "SIGIL_GEMINI_API_KEY",
            "gemini-2.5-pro",
        ),
        (ProviderFamily::Custom, ProviderProtocol::OpenAiResponses) => (
            "https://api.openai.com/v1",
            "SIGIL_OPENAI_RESPONSES_API_KEY",
            "gpt-4.1",
        ),
        (ProviderFamily::Custom, ProviderProtocol::OpenAiChatCompletions) => (
            "https://api.openai.com/v1",
            "SIGIL_OPENAI_COMPATIBLE_API_KEY",
            "gpt-4.1",
        ),
        _ => anyhow::bail!("unsupported provider family and protocol combination"),
    };
    let options = match (family, protocol) {
        (ProviderFamily::DeepSeek, ProviderProtocol::DeepSeek) => serde_json::json!({
            "beta_base_url": "https://api.deepseek.com/beta",
            "anthropic_base_url": "https://api.deepseek.com/anthropic",
            "user_id_strategy": "stable_per_end_user",
            "strict_tools_mode": "auto",
            "fim_model": "deepseek-v4-pro"
        }),
        (ProviderFamily::Anthropic, ProviderProtocol::AnthropicMessages) => serde_json::json!({
            "anthropic_version": "2023-06-01",
            "max_tokens": 4096,
            "beta_headers": []
        }),
        _ => Value::Object(Default::default()),
    };
    let config = ProviderConnectionConfig {
        id,
        label: label.into(),
        provider: family,
        protocol,
        base_url: base_url.to_owned(),
        credential: CredentialRefConfig::Environment {
            name: environment.to_owned(),
        },
        model_context_windows: BTreeMap::new(),
        options,
    };
    config.validate()?;
    Ok((config, model.to_owned()))
}

pub(crate) fn allowed_environment_names(
    family: ProviderFamily,
    protocol: ProviderProtocol,
) -> &'static [&'static str] {
    match (family, protocol) {
        (ProviderFamily::DeepSeek, ProviderProtocol::DeepSeek) => &["SIGIL_API_KEY"],
        (ProviderFamily::OpenAi, ProviderProtocol::OpenAiResponses)
        | (ProviderFamily::Custom, ProviderProtocol::OpenAiResponses) => {
            &["SIGIL_OPENAI_RESPONSES_API_KEY"]
        }
        (ProviderFamily::Custom, ProviderProtocol::OpenAiChatCompletions) => {
            &["SIGIL_OPENAI_COMPATIBLE_API_KEY"]
        }
        (ProviderFamily::Anthropic, ProviderProtocol::AnthropicMessages) => {
            &["SIGIL_ANTHROPIC_API_KEY"]
        }
        (ProviderFamily::Gemini, ProviderProtocol::GeminiGenerateContent) => {
            &["SIGIL_GEMINI_API_KEY"]
        }
        _ => &[],
    }
}

fn validate_family_protocol(family: ProviderFamily, protocol: ProviderProtocol) -> Result<()> {
    let valid = matches!(
        (family, protocol),
        (ProviderFamily::DeepSeek, ProviderProtocol::DeepSeek)
            | (ProviderFamily::OpenAi, ProviderProtocol::OpenAiResponses)
            | (
                ProviderFamily::Anthropic,
                ProviderProtocol::AnthropicMessages
            )
            | (
                ProviderFamily::Gemini,
                ProviderProtocol::GeminiGenerateContent
            )
            | (
                ProviderFamily::Custom,
                ProviderProtocol::OpenAiResponses | ProviderProtocol::OpenAiChatCompletions
            )
    );
    anyhow::ensure!(valid, "provider family and protocol do not match");
    Ok(())
}

fn validate_credential_ref(
    family: ProviderFamily,
    protocol: ProviderProtocol,
    credential: &CredentialRefConfig,
) -> Result<()> {
    match credential {
        CredentialRefConfig::Environment { name } => {
            anyhow::ensure!(
                allowed_environment_names(family, protocol).contains(&name.as_str()),
                "credential environment name is not allowed for this provider connection"
            );
        }
        CredentialRefConfig::Stored { .. } => {}
        CredentialRefConfig::None => {
            anyhow::ensure!(
                family == ProviderFamily::Custom,
                "unauthenticated credentials are only allowed for custom connections"
            );
        }
    }
    Ok(())
}

fn validate_endpoint(
    family: ProviderFamily,
    credential: &CredentialRefConfig,
    raw: &str,
) -> Result<()> {
    let parsed = Url::parse(raw).context("provider connection base_url is invalid")?;
    anyhow::ensure!(
        parsed.username().is_empty() && parsed.password().is_none(),
        "provider connection base_url must not contain userinfo"
    );
    anyhow::ensure!(
        parsed.query().is_none() && parsed.fragment().is_none(),
        "provider connection base_url must not contain query or fragment"
    );
    anyhow::ensure!(
        matches!(parsed.scheme(), "https" | "http"),
        "provider connection base_url must use http or https"
    );
    let loopback = parsed
        .host_str()
        .is_some_and(|host| matches!(host, "localhost" | "127.0.0.1" | "::1"));
    if matches!(credential, CredentialRefConfig::None) {
        anyhow::ensure!(
            family == ProviderFamily::Custom && loopback,
            "unauthenticated provider connections are limited to explicit loopback endpoints"
        );
    }
    if parsed.scheme() == "http" {
        anyhow::ensure!(
            family == ProviderFamily::Custom
                && matches!(credential, CredentialRefConfig::None)
                && loopback,
            "credentialed or remote provider connections require https"
        );
    }
    Ok(())
}

fn validate_provider_option_endpoints(
    family: ProviderFamily,
    protocol: ProviderProtocol,
    credential: &CredentialRefConfig,
    options: &Value,
) -> Result<()> {
    if (family, protocol) != (ProviderFamily::DeepSeek, ProviderProtocol::DeepSeek) {
        return Ok(());
    }
    let object = options
        .as_object()
        .expect("provider options object was checked before endpoint validation");
    for key in ["beta_base_url", "anthropic_base_url"] {
        let Some(value) = object.get(key) else {
            continue;
        };
        let raw = value
            .as_str()
            .with_context(|| format!("provider connection option {key} must be a URL string"))?;
        validate_endpoint(family, credential, raw)
            .with_context(|| format!("provider connection option {key} is unsafe"))?;
    }
    Ok(())
}

fn validate_connection_label(label: &str) -> Result<()> {
    let label = label.trim();
    anyhow::ensure!(
        !label.is_empty() && label.len() <= CONNECTION_LABEL_MAX_BYTES,
        "connection label must contain 1..={CONNECTION_LABEL_MAX_BYTES} bytes"
    );
    anyhow::ensure!(
        !label.chars().any(|character| {
            character.is_control()
                || matches!(
                    character,
                    '\u{009b}'
                        | '\u{009d}'
                        | '\u{202a}'..='\u{202e}'
                        | '\u{2066}'..='\u{2069}'
                )
        }),
        "connection label contains an unsafe character"
    );
    Ok(())
}
