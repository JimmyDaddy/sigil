use std::env;

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

pub const SIGIL_ANTHROPIC_API_KEY_ENV: &str = "SIGIL_ANTHROPIC_API_KEY";
pub const ANTHROPIC_API_KEY_ENV: &str = "ANTHROPIC_API_KEY";
pub const SIGIL_ANTHROPIC_MODEL_ENV: &str = "SIGIL_ANTHROPIC_MODEL";
pub const SIGIL_ANTHROPIC_BASE_URL_ENV: &str = "SIGIL_ANTHROPIC_BASE_URL";
pub const SIGIL_ANTHROPIC_VERSION_ENV: &str = "SIGIL_ANTHROPIC_VERSION";
pub const SIGIL_ANTHROPIC_MAX_TOKENS_ENV: &str = "SIGIL_ANTHROPIC_MAX_TOKENS";
pub const SIGIL_ANTHROPIC_REQUEST_TIMEOUT_SECS_ENV: &str = "SIGIL_ANTHROPIC_REQUEST_TIMEOUT_SECS";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicProviderConfig {
    #[serde(default = "default_base_url")]
    pub base_url: String,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default = "default_anthropic_version")]
    pub anthropic_version: String,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    #[serde(default)]
    pub beta_headers: Vec<String>,
    #[serde(default = "default_request_timeout_secs")]
    pub request_timeout_secs: u64,
}

impl AnthropicProviderConfig {
    pub fn default_for_model(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            ..Self::default()
        }
    }

    pub fn resolved(self) -> Result<Self> {
        let mut resolved = self;

        if let Some(value) = read_env_string(SIGIL_ANTHROPIC_MODEL_ENV) {
            resolved.model = value;
        }
        if let Some(value) = read_env_string(SIGIL_ANTHROPIC_BASE_URL_ENV) {
            resolved.base_url = value;
        }
        if let Some(value) = read_env_string(SIGIL_ANTHROPIC_VERSION_ENV) {
            resolved.anthropic_version = value;
        }
        if let Some(value) = read_env_u32(SIGIL_ANTHROPIC_MAX_TOKENS_ENV)? {
            resolved.max_tokens = value;
        }
        if let Some(value) = read_env_u64(SIGIL_ANTHROPIC_REQUEST_TIMEOUT_SECS_ENV)? {
            resolved.request_timeout_secs = value;
        }
        if let Some(value) = read_env_string(SIGIL_ANTHROPIC_API_KEY_ENV)
            .or_else(|| read_env_string(ANTHROPIC_API_KEY_ENV))
        {
            resolved.api_key = Some(value);
        }

        Ok(resolved)
    }
}

impl Default for AnthropicProviderConfig {
    fn default() -> Self {
        Self {
            base_url: default_base_url(),
            model: default_model(),
            api_key: None,
            anthropic_version: default_anthropic_version(),
            max_tokens: default_max_tokens(),
            beta_headers: Vec::new(),
            request_timeout_secs: default_request_timeout_secs(),
        }
    }
}

fn default_base_url() -> String {
    "https://api.anthropic.com".to_owned()
}

fn default_model() -> String {
    "claude-sonnet-4-5".to_owned()
}

fn default_anthropic_version() -> String {
    "2023-06-01".to_owned()
}

fn default_max_tokens() -> u32 {
    4096
}

fn default_request_timeout_secs() -> u64 {
    120
}

fn read_env_string(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn read_env_u32(name: &str) -> Result<Option<u32>> {
    let Some(value) = read_env_string(name) else {
        return Ok(None);
    };
    let parsed = value
        .parse::<u32>()
        .map_err(|error| anyhow!("invalid {name}: {error}"))?;
    if parsed == 0 {
        return Err(anyhow!("{name} must be greater than 0"));
    }
    Ok(Some(parsed))
}

fn read_env_u64(name: &str) -> Result<Option<u64>> {
    let Some(value) = read_env_string(name) else {
        return Ok(None);
    };
    let parsed = value
        .parse::<u64>()
        .map_err(|error| anyhow!("invalid {name}: {error}"))?;
    if parsed == 0 {
        return Err(anyhow!("{name} must be greater than 0"));
    }
    Ok(Some(parsed))
}

#[cfg(test)]
#[path = "tests/config_tests.rs"]
mod tests;
