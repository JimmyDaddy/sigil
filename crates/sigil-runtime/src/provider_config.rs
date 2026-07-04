use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;
use serde_json::Value;
use sigil_kernel::{ModelRequestConfig, RootConfig};
use sigil_provider_anthropic::{AnthropicProviderConfig, SIGIL_ANTHROPIC_API_KEY_ENV};
use sigil_provider_deepseek::{DeepSeekProviderConfig, SIGIL_API_KEY_ENV, StrictToolsMode};
use sigil_provider_gemini::{GeminiProviderConfig, SIGIL_GEMINI_API_KEY_ENV};
use sigil_provider_openai_compat::{OPENAI_COMPATIBLE_API_KEY_ENV, OpenAiCompatibleProviderConfig};

use crate::{
    load_anthropic_config, load_deepseek_config, load_gemini_config, load_openai_compat_config,
    provider_config_key,
};

pub const DEEPSEEK_PROVIDER_KEY: &str = "deepseek";
pub const OPENAI_COMPAT_PROVIDER_KEY: &str = "openai_compat";
pub const ANTHROPIC_PROVIDER_KEY: &str = "anthropic";
pub const GEMINI_PROVIDER_KEY: &str = "gemini";

pub const DEFAULT_SETUP_PROVIDER_KEY: &str = DEEPSEEK_PROVIDER_KEY;
pub const DEFAULT_SETUP_API_KEY_ENV: &str = SIGIL_API_KEY_ENV;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderConfigFields {
    pub model: String,
    pub api_key: String,
    pub base_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRequestConfigFields {
    pub request_timeout_secs: String,
    pub stream_idle_timeout_secs: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeepSeekProviderConfigFields {
    pub beta_base_url: String,
    pub anthropic_base_url: String,
    pub user_id_strategy: String,
    pub strict_tools_mode: ProviderStrictToolsMode,
    pub fim_model: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ProviderStrictToolsMode {
    Off,
    #[default]
    Auto,
    Always,
}

impl ProviderStrictToolsMode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Auto => "auto",
            Self::Always => "always",
        }
    }
}

impl From<StrictToolsMode> for ProviderStrictToolsMode {
    fn from(value: StrictToolsMode) -> Self {
        match value {
            StrictToolsMode::Off => Self::Off,
            StrictToolsMode::Auto => Self::Auto,
            StrictToolsMode::Always => Self::Always,
        }
    }
}

impl From<ProviderStrictToolsMode> for StrictToolsMode {
    fn from(value: ProviderStrictToolsMode) -> Self {
        match value {
            ProviderStrictToolsMode::Off => Self::Off,
            ProviderStrictToolsMode::Auto => Self::Auto,
            ProviderStrictToolsMode::Always => Self::Always,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderStatusConfig {
    pub api_key: Option<String>,
    pub base_url: String,
    pub request_timeout_secs: u64,
}

#[must_use]
pub fn normalize_provider_name(provider: &str) -> &str {
    provider_config_key(provider)
}

pub fn supported_provider_name(provider: &str) -> Result<&str> {
    let provider = normalize_provider_name(provider);
    match provider {
        DEEPSEEK_PROVIDER_KEY
        | OPENAI_COMPAT_PROVIDER_KEY
        | ANTHROPIC_PROVIDER_KEY
        | GEMINI_PROVIDER_KEY => Ok(provider),
        other => bail!(
            "unsupported provider {other}; expected one of deepseek, openai_compat, anthropic, or gemini"
        ),
    }
}

#[must_use]
pub fn provider_api_key_env_name(provider: &str) -> Option<&'static str> {
    match normalize_provider_name(provider) {
        DEEPSEEK_PROVIDER_KEY => Some(SIGIL_API_KEY_ENV),
        OPENAI_COMPAT_PROVIDER_KEY => Some(OPENAI_COMPATIBLE_API_KEY_ENV),
        ANTHROPIC_PROVIDER_KEY => Some(SIGIL_ANTHROPIC_API_KEY_ENV),
        GEMINI_PROVIDER_KEY => Some(SIGIL_GEMINI_API_KEY_ENV),
        _ => None,
    }
}

#[must_use]
pub fn default_setup_provider_model() -> String {
    DeepSeekProviderConfig::default().model
}

#[must_use]
pub fn provider_config_fields(
    root_config: &RootConfig,
    provider_name: &str,
    fallback_model: &str,
) -> ProviderConfigFields {
    match normalize_provider_name(provider_name) {
        OPENAI_COMPAT_PROVIDER_KEY => load_openai_compat_config(root_config)
            .map(provider_config_fields_from_openai_compat)
            .unwrap_or_else(|_| default_provider_config_fields(provider_name, fallback_model)),
        ANTHROPIC_PROVIDER_KEY => load_anthropic_config(root_config)
            .map(provider_config_fields_from_anthropic)
            .unwrap_or_else(|_| default_provider_config_fields(provider_name, fallback_model)),
        GEMINI_PROVIDER_KEY => load_gemini_config(root_config)
            .map(provider_config_fields_from_gemini)
            .unwrap_or_else(|_| default_provider_config_fields(provider_name, fallback_model)),
        DEEPSEEK_PROVIDER_KEY => load_deepseek_config(root_config)
            .map(provider_config_fields_from_deepseek)
            .unwrap_or_else(|_| default_provider_config_fields(provider_name, fallback_model)),
        _ => ProviderConfigFields {
            model: fallback_model.to_owned(),
            api_key: String::new(),
            base_url: String::new(),
        },
    }
}

#[must_use]
pub fn default_provider_config_fields(provider_name: &str, model: &str) -> ProviderConfigFields {
    match normalize_provider_name(provider_name) {
        OPENAI_COMPAT_PROVIDER_KEY => provider_config_fields_from_openai_compat(
            OpenAiCompatibleProviderConfig::default_for_model(model),
        ),
        ANTHROPIC_PROVIDER_KEY => {
            provider_config_fields_from_anthropic(AnthropicProviderConfig::default_for_model(model))
        }
        GEMINI_PROVIDER_KEY => {
            provider_config_fields_from_gemini(GeminiProviderConfig::default_for_model(model))
        }
        DEEPSEEK_PROVIDER_KEY => {
            provider_config_fields_from_deepseek(DeepSeekProviderConfig::default_for_model(model))
        }
        _ => ProviderConfigFields {
            model: model.to_owned(),
            api_key: String::new(),
            base_url: String::new(),
        },
    }
}

#[must_use]
pub fn deepseek_provider_config_fields(
    root_config: &RootConfig,
    fallback_model: &str,
) -> DeepSeekProviderConfigFields {
    let config = load_deepseek_config(root_config)
        .unwrap_or_else(|_| DeepSeekProviderConfig::default_for_model(fallback_model));
    DeepSeekProviderConfigFields {
        beta_base_url: config.beta_base_url,
        anthropic_base_url: config.anthropic_base_url,
        user_id_strategy: config.user_id_strategy.unwrap_or_default(),
        strict_tools_mode: config.strict_tools_mode.into(),
        fim_model: config.fim_model,
    }
}

pub fn set_provider_config_fields(
    root_config: &mut RootConfig,
    provider_name: &str,
    fields: &ProviderConfigFields,
    deepseek_fields: Option<&DeepSeekProviderConfigFields>,
) -> Result<()> {
    let provider_name = supported_provider_name(provider_name)?;
    let model = fields.model.trim();
    if model.is_empty() {
        bail!("model cannot be empty");
    }
    let base_url = fields.base_url.trim();
    if base_url.is_empty() {
        bail!("base_url cannot be empty");
    }
    let api_key = optional_trimmed_string(&fields.api_key);

    root_config.agent.provider = provider_name.to_owned();
    root_config.agent.model = model.to_owned();

    match provider_name {
        OPENAI_COMPAT_PROVIDER_KEY => {
            let mut config = load_openai_compat_config(root_config)
                .unwrap_or_else(|_| OpenAiCompatibleProviderConfig::default_for_model(model));
            config.model = model.to_owned();
            config.api_key = api_key;
            config.base_url = base_url.to_owned();
            root_config.providers.insert(
                OPENAI_COMPAT_PROVIDER_KEY.to_owned(),
                serialize_provider_config("openai_compat", &config)?,
            );
        }
        ANTHROPIC_PROVIDER_KEY => {
            let mut config = load_anthropic_config(root_config)
                .unwrap_or_else(|_| AnthropicProviderConfig::default_for_model(model));
            config.model = model.to_owned();
            config.api_key = api_key;
            config.base_url = base_url.to_owned();
            root_config.providers.insert(
                ANTHROPIC_PROVIDER_KEY.to_owned(),
                serialize_provider_config("anthropic", &config)?,
            );
        }
        GEMINI_PROVIDER_KEY => {
            let mut config = load_gemini_config(root_config)
                .unwrap_or_else(|_| GeminiProviderConfig::default_for_model(model));
            config.model = model.to_owned();
            config.api_key = api_key;
            config.base_url = base_url.to_owned();
            root_config.providers.insert(
                GEMINI_PROVIDER_KEY.to_owned(),
                serialize_provider_config("gemini", &config)?,
            );
        }
        DEEPSEEK_PROVIDER_KEY => {
            let extras = deepseek_fields
                .cloned()
                .unwrap_or_else(|| deepseek_provider_config_fields(root_config, model));
            let beta_base_url = extras.beta_base_url.trim();
            if beta_base_url.is_empty() {
                bail!("beta_base_url cannot be empty");
            }
            let anthropic_base_url = extras.anthropic_base_url.trim();
            if anthropic_base_url.is_empty() {
                bail!("anthropic_base_url cannot be empty");
            }
            let fim_model = extras.fim_model.trim();
            if fim_model.is_empty() {
                bail!("fim_model cannot be empty");
            }

            let mut config = load_deepseek_config(root_config)
                .unwrap_or_else(|_| DeepSeekProviderConfig::default_for_model(model));
            config.model = model.to_owned();
            config.api_key = api_key;
            config.base_url = base_url.to_owned();
            config.beta_base_url = beta_base_url.to_owned();
            config.anthropic_base_url = anthropic_base_url.to_owned();
            config.user_id_strategy = optional_trimmed_string(&extras.user_id_strategy);
            config.strict_tools_mode = extras.strict_tools_mode.into();
            config.fim_model = fim_model.to_owned();
            root_config.providers.insert(
                DEEPSEEK_PROVIDER_KEY.to_owned(),
                serialize_provider_config("deepseek", &config)?,
            );
        }
        _ => unreachable!("supported_provider_name returned an unsupported provider"),
    }

    Ok(())
}

#[must_use]
pub fn model_request_config_fields(root_config: &RootConfig) -> ModelRequestConfigFields {
    ModelRequestConfigFields {
        request_timeout_secs: root_config.model_request.request_timeout_secs.to_string(),
        stream_idle_timeout_secs: root_config
            .model_request
            .stream_idle_timeout_secs
            .to_string(),
    }
}

pub fn set_model_request_config_fields(
    root_config: &mut RootConfig,
    fields: &ModelRequestConfigFields,
) -> Result<()> {
    let request_timeout_secs = parse_model_request_timeout_secs(
        "model_request.request_timeout_secs",
        &fields.request_timeout_secs,
    )?;
    let stream_idle_timeout_secs = parse_model_request_timeout_secs(
        "model_request.stream_idle_timeout_secs",
        &fields.stream_idle_timeout_secs,
    )?;
    root_config.model_request = ModelRequestConfig {
        request_timeout_secs,
        stream_idle_timeout_secs,
        stream_total_timeout_secs: root_config.model_request.stream_total_timeout_secs,
    };
    root_config.model_request.to_timeouts()?;
    Ok(())
}

pub fn set_active_provider_model(root_config: &mut RootConfig, model: &str) -> Result<()> {
    let provider_name = normalize_provider_name(&root_config.agent.provider).to_owned();
    let mut fields = provider_config_fields(root_config, &provider_name, model);
    fields.model = model.to_owned();
    let deepseek_fields = deepseek_provider_config_fields(root_config, model);
    set_provider_config_fields(root_config, &provider_name, &fields, Some(&deepseek_fields))
}

pub fn deepseek_provider_value_for_setup(model: &str, api_key: Option<&str>) -> Result<Value> {
    let mut config = DeepSeekProviderConfig::default_for_model(model);
    config.api_key = api_key.map(str::to_owned);
    serialize_provider_config("deepseek", &config)
}

pub fn provider_status_config_from_fields(
    fields: &ProviderConfigFields,
    model_request: &ModelRequestConfig,
) -> Result<ProviderStatusConfig> {
    model_request.to_timeouts()?;
    let base_url = fields.base_url.trim();
    if base_url.is_empty() {
        bail!("base_url cannot be empty");
    }
    Ok(ProviderStatusConfig {
        api_key: optional_trimmed_string(&fields.api_key),
        base_url: base_url.to_owned(),
        request_timeout_secs: model_request.request_timeout_secs,
    })
}

pub fn deepseek_provider_status_config(root_config: &RootConfig) -> Result<ProviderStatusConfig> {
    let config = crate::resolve_deepseek_config(root_config).or_else(|_| {
        DeepSeekProviderConfig::default_for_model(&root_config.agent.model).resolved()
    })?;
    Ok(ProviderStatusConfig {
        api_key: crate::resolve_deepseek_api_key(&config).map(|secret| secret.value),
        base_url: config.base_url,
        request_timeout_secs: root_config.model_request.request_timeout_secs,
    })
}

fn provider_config_fields_from_deepseek(config: DeepSeekProviderConfig) -> ProviderConfigFields {
    ProviderConfigFields {
        model: config.model,
        api_key: config.api_key.unwrap_or_default(),
        base_url: config.base_url,
    }
}

fn provider_config_fields_from_openai_compat(
    config: OpenAiCompatibleProviderConfig,
) -> ProviderConfigFields {
    ProviderConfigFields {
        model: config.model,
        api_key: config.api_key.unwrap_or_default(),
        base_url: config.base_url,
    }
}

fn provider_config_fields_from_anthropic(config: AnthropicProviderConfig) -> ProviderConfigFields {
    ProviderConfigFields {
        model: config.model,
        api_key: config.api_key.unwrap_or_default(),
        base_url: config.base_url,
    }
}

fn provider_config_fields_from_gemini(config: GeminiProviderConfig) -> ProviderConfigFields {
    ProviderConfigFields {
        model: config.model,
        api_key: config.api_key.unwrap_or_default(),
        base_url: config.base_url,
    }
}

fn parse_model_request_timeout_secs(label: &str, raw: &str) -> Result<u64> {
    let parsed = raw
        .trim()
        .parse::<u64>()
        .map_err(|error| anyhow!("{label} must be a positive integer: {error}"))?;
    if parsed == 0 {
        bail!("{label} must be greater than 0");
    }
    Ok(parsed)
}

fn optional_trimmed_string(raw: &str) -> Option<String> {
    let value = raw.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn serialize_provider_config<T>(label: &str, provider_config: &T) -> Result<Value>
where
    T: Serialize,
{
    let mut value = serde_json::to_value(provider_config)
        .with_context(|| format!("failed to serialize {label} provider config"))?;
    if let Some(object) = value.as_object_mut() {
        object.retain(|_, entry| !entry.is_null());
    }
    Ok(value)
}

#[cfg(test)]
#[path = "tests/provider_config_tests.rs"]
mod tests;
