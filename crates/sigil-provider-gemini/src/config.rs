use std::env;

use anyhow::Result;
use serde::{Deserialize, Serialize};

pub const SIGIL_GEMINI_API_KEY_ENV: &str = "SIGIL_GEMINI_API_KEY";
pub const SIGIL_GEMINI_BASE_URL_ENV: &str = "SIGIL_GEMINI_BASE_URL";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeminiProviderConfig {
    #[serde(default = "default_base_url")]
    pub base_url: String,
    #[serde(
        rename = "__runtime_model",
        skip_serializing,
        skip_deserializing,
        default = "default_model"
    )]
    pub model: String,
    #[serde(default)]
    pub api_key: Option<String>,
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

        if let Some(value) = read_env_string(SIGIL_GEMINI_BASE_URL_ENV) {
            resolved.base_url = value;
        }
        if let Some(value) = read_env_string(SIGIL_GEMINI_API_KEY_ENV) {
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
        }
    }
}

fn default_base_url() -> String {
    "https://generativelanguage.googleapis.com/v1beta".to_owned()
}

fn default_model() -> String {
    "gemini-2.5-pro".to_owned()
}

fn read_env_string(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
#[path = "tests/config_tests.rs"]
mod tests;
