use std::{env, fmt};

use anyhow::Result;
use serde::{Deserialize, Serialize};

pub const OPENAI_RESPONSES_API_KEY_ENV: &str = "SIGIL_OPENAI_RESPONSES_API_KEY";
pub const OPENAI_RESPONSES_BASE_URL_ENV: &str = "SIGIL_OPENAI_RESPONSES_BASE_URL";

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenAiResponsesProviderConfig {
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
    #[serde(default)]
    pub organization: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
}

impl fmt::Debug for OpenAiResponsesProviderConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenAiResponsesProviderConfig")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("api_key", &self.api_key.as_ref().map(|_| "[REDACTED]"))
            .field("organization", &self.organization)
            .field("project", &self.project)
            .finish()
    }
}

impl OpenAiResponsesProviderConfig {
    pub fn default_for_model(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            ..Self::default()
        }
    }

    pub fn resolved(self) -> Result<Self> {
        let mut resolved = self;
        if let Some(value) = read_env_string(OPENAI_RESPONSES_BASE_URL_ENV) {
            resolved.base_url = value;
        }
        if let Some(value) = read_env_string(OPENAI_RESPONSES_API_KEY_ENV) {
            resolved.api_key = Some(value);
        }
        Ok(resolved)
    }
}

impl Default for OpenAiResponsesProviderConfig {
    fn default() -> Self {
        Self {
            base_url: default_base_url(),
            model: default_model(),
            api_key: None,
            organization: None,
            project: None,
        }
    }
}

fn default_base_url() -> String {
    "https://api.openai.com/v1".to_owned()
}

fn default_model() -> String {
    "gpt-4.1".to_owned()
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
