use std::env;

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

pub const OPENAI_COMPATIBLE_API_KEY_ENV: &str = "SIGIL_OPENAI_COMPATIBLE_API_KEY";
pub const OPENAI_API_KEY_ENV: &str = "OPENAI_API_KEY";
pub const OPENAI_COMPATIBLE_MODEL_ENV: &str = "SIGIL_OPENAI_COMPATIBLE_MODEL";
pub const OPENAI_COMPATIBLE_BASE_URL_ENV: &str = "SIGIL_OPENAI_COMPATIBLE_BASE_URL";
pub const OPENAI_COMPATIBLE_REQUEST_TIMEOUT_SECS_ENV: &str =
    "SIGIL_OPENAI_COMPATIBLE_REQUEST_TIMEOUT_SECS";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiCompatibleProviderConfig {
    #[serde(default = "default_base_url")]
    pub base_url: String,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub organization: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default = "default_request_timeout_secs")]
    pub request_timeout_secs: u64,
}

impl OpenAiCompatibleProviderConfig {
    pub fn default_for_model(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            ..Self::default()
        }
    }

    pub fn resolved(self) -> Result<Self> {
        let mut resolved = self;

        if let Some(value) = read_env_string(OPENAI_COMPATIBLE_MODEL_ENV) {
            resolved.model = value;
        }
        if let Some(value) = read_env_string(OPENAI_COMPATIBLE_BASE_URL_ENV) {
            resolved.base_url = value;
        }
        if let Some(value) = read_env_u64(OPENAI_COMPATIBLE_REQUEST_TIMEOUT_SECS_ENV)? {
            resolved.request_timeout_secs = value;
        }
        if let Some(value) = read_env_string(OPENAI_COMPATIBLE_API_KEY_ENV)
            .or_else(|| read_env_string(OPENAI_API_KEY_ENV))
        {
            resolved.api_key = Some(value);
        }

        Ok(resolved)
    }
}

impl Default for OpenAiCompatibleProviderConfig {
    fn default() -> Self {
        Self {
            base_url: default_base_url(),
            model: default_model(),
            api_key: None,
            organization: None,
            project: None,
            request_timeout_secs: default_request_timeout_secs(),
        }
    }
}

fn default_base_url() -> String {
    "https://api.openai.com/v1".to_owned()
}

fn default_model() -> String {
    "gpt-4.1".to_owned()
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
