use std::env;

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

pub const SIGIL_GEMINI_API_KEY_ENV: &str = "SIGIL_GEMINI_API_KEY";
pub const GEMINI_API_KEY_ENV: &str = "GEMINI_API_KEY";
pub const GOOGLE_API_KEY_ENV: &str = "GOOGLE_API_KEY";
pub const SIGIL_GEMINI_MODEL_ENV: &str = "SIGIL_GEMINI_MODEL";
pub const SIGIL_GEMINI_BASE_URL_ENV: &str = "SIGIL_GEMINI_BASE_URL";
pub const SIGIL_GEMINI_REQUEST_TIMEOUT_SECS_ENV: &str = "SIGIL_GEMINI_REQUEST_TIMEOUT_SECS";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiProviderConfig {
    #[serde(default = "default_base_url")]
    pub base_url: String,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default = "default_request_timeout_secs")]
    pub request_timeout_secs: u64,
}

impl GeminiProviderConfig {
    pub fn default_for_model(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            ..Self::default()
        }
    }

    pub fn resolved(self) -> Result<Self> {
        let mut resolved = self;

        if let Some(value) = read_env_string(SIGIL_GEMINI_MODEL_ENV) {
            resolved.model = value;
        }
        if let Some(value) = read_env_string(SIGIL_GEMINI_BASE_URL_ENV) {
            resolved.base_url = value;
        }
        if let Some(value) = read_env_u64(SIGIL_GEMINI_REQUEST_TIMEOUT_SECS_ENV)? {
            resolved.request_timeout_secs = value;
        }
        if let Some(value) = read_env_string(SIGIL_GEMINI_API_KEY_ENV)
            .or_else(|| read_env_string(GEMINI_API_KEY_ENV))
            .or_else(|| read_env_string(GOOGLE_API_KEY_ENV))
        {
            resolved.api_key = Some(value);
        }

        Ok(resolved)
    }
}

impl Default for GeminiProviderConfig {
    fn default() -> Self {
        Self {
            base_url: default_base_url(),
            model: default_model(),
            api_key: None,
            request_timeout_secs: default_request_timeout_secs(),
        }
    }
}

fn default_base_url() -> String {
    "https://generativelanguage.googleapis.com/v1beta".to_owned()
}

fn default_model() -> String {
    "gemini-2.5-pro".to_owned()
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
