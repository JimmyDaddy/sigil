use std::{env, fmt};

use anyhow::Result;
use serde::{Deserialize, Serialize};

pub const OPENAI_RESPONSES_API_KEY_ENV: &str = "SIGIL_OPENAI_RESPONSES_API_KEY";
pub const OPENAI_API_KEY_ENV: &str = "OPENAI_API_KEY";
pub const OPENAI_RESPONSES_API_KEY_ENV_NAMES: &[&str] =
    &[OPENAI_RESPONSES_API_KEY_ENV, OPENAI_API_KEY_ENV];
pub const OPENAI_RESPONSES_BASE_URL_ENV: &str = "SIGIL_OPENAI_RESPONSES_BASE_URL";

/// Authentication selected by the validated host connection, never by provider JSON options.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OpenAiResponsesAuthentication {
    /// A missing key remains an error, including on a local endpoint.
    #[default]
    Bearer,
    /// The host explicitly selected a no-credential custom loopback connection.
    UnauthenticatedLoopback,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct OpenAiResponsesProviderConfig {
    /// Runtime-only exact authentication choice; persisted provider options cannot enable it.
    #[serde(skip)]
    pub authentication: OpenAiResponsesAuthentication,
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
            .field("authentication", &self.authentication)
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
        // This connection intentionally has no credentials; ambient key/endpoint overrides must
        // not turn it into a different authenticated or remote route.
        if resolved.authentication == OpenAiResponsesAuthentication::UnauthenticatedLoopback {
            return Ok(resolved);
        }
        if let Some(value) = read_env_string(OPENAI_RESPONSES_BASE_URL_ENV) {
            resolved.base_url = value;
        }
        if let Some(value) = read_env_string_from(OPENAI_RESPONSES_API_KEY_ENV_NAMES) {
            resolved.api_key = Some(value);
        }
        Ok(resolved)
    }
}

impl Default for OpenAiResponsesProviderConfig {
    fn default() -> Self {
        Self {
            authentication: OpenAiResponsesAuthentication::Bearer,
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

fn read_env_string_from(names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| read_env_string(name))
}

#[cfg(test)]
#[path = "tests/config_tests.rs"]
mod tests;
