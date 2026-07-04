use std::env;

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

use sigil_kernel::ReasoningEffort;

pub const SIGIL_API_KEY_ENV: &str = "SIGIL_API_KEY";
pub const SIGIL_BASE_URL_ENV: &str = "SIGIL_BASE_URL";
pub const SIGIL_BETA_BASE_URL_ENV: &str = "SIGIL_BETA_BASE_URL";
pub const SIGIL_ANTHROPIC_BASE_URL_ENV: &str = "SIGIL_ANTHROPIC_BASE_URL";
pub const SIGIL_USER_ID_STRATEGY_ENV: &str = "SIGIL_USER_ID_STRATEGY";
pub const SIGIL_FIM_MODEL_ENV: &str = "SIGIL_FIM_MODEL";
pub const SIGIL_STRICT_TOOLS_MODE_ENV: &str = "SIGIL_STRICT_TOOLS_MODE";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeepSeekProviderConfig {
    #[serde(default = "default_primary_base_url")]
    pub base_url: String,
    #[serde(default = "default_beta_base_url")]
    pub beta_base_url: String,
    #[serde(default = "default_anthropic_base_url")]
    pub anthropic_base_url: String,
    #[serde(
        rename = "__runtime_model",
        skip_serializing,
        skip_deserializing,
        default = "default_model"
    )]
    pub model: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default = "default_user_id_strategy")]
    pub user_id_strategy: Option<String>,
    #[serde(default)]
    pub strict_tools_mode: StrictToolsMode,
    #[serde(default = "default_fim_model")]
    pub fim_model: String,
}

impl DeepSeekProviderConfig {
    pub fn default_for_model(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            ..Self::default()
        }
    }

    pub fn resolved(self) -> Result<Self> {
        let mut resolved = self;

        if let Some(value) = read_env_string(SIGIL_BASE_URL_ENV) {
            resolved.base_url = value;
        }
        if let Some(value) = read_env_string(SIGIL_BETA_BASE_URL_ENV) {
            resolved.beta_base_url = value;
        }
        if let Some(value) = read_env_string(SIGIL_ANTHROPIC_BASE_URL_ENV) {
            resolved.anthropic_base_url = value;
        }
        if let Some(value) = read_env_string(SIGIL_USER_ID_STRATEGY_ENV) {
            resolved.user_id_strategy = Some(value);
        }
        if let Some(value) = read_env_string(SIGIL_FIM_MODEL_ENV) {
            resolved.fim_model = value;
        }
        if let Some(value) = read_env_strict_tools_mode(SIGIL_STRICT_TOOLS_MODE_ENV)? {
            resolved.strict_tools_mode = value;
        }
        if let Some(value) = read_env_string(SIGIL_API_KEY_ENV) {
            resolved.api_key = Some(value);
        }

        Ok(resolved)
    }

    pub fn profile(&self) -> DeepSeekProviderProfile {
        DeepSeekProviderProfile {
            primary_base_url: self.base_url.clone(),
            beta_base_url: self.beta_base_url.clone(),
            anthropic_base_url: self.anthropic_base_url.clone(),
            default_model: self.model.clone(),
            default_fim_model: self.fim_model.clone(),
            default_thinking: true,
            default_reasoning_effort: ReasoningEffort::Max,
            quirks: DeepSeekProviderQuirkProfile::default(),
        }
    }
}

impl Default for DeepSeekProviderConfig {
    fn default() -> Self {
        Self {
            base_url: default_primary_base_url(),
            beta_base_url: default_beta_base_url(),
            anthropic_base_url: default_anthropic_base_url(),
            model: default_model(),
            api_key: None,
            user_id_strategy: default_user_id_strategy(),
            strict_tools_mode: StrictToolsMode::default(),
            fim_model: default_fim_model(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct DeepSeekProviderProfile {
    pub primary_base_url: String,
    pub beta_base_url: String,
    pub anthropic_base_url: String,
    pub default_model: String,
    pub default_fim_model: String,
    pub default_thinking: bool,
    pub default_reasoning_effort: ReasoningEffort,
    pub quirks: DeepSeekProviderQuirkProfile,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StrictToolsMode {
    Off,
    #[default]
    Auto,
    Always,
}

#[derive(Debug, Clone)]
pub struct DeepSeekProviderQuirkProfile {
    pub requires_reasoning_replay_after_tool_call: bool,
    pub ignores_sampling_params_in_thinking_mode: bool,
    pub strict_tools_requires_beta_endpoint: bool,
    pub prefix_completion_requires_beta_endpoint: bool,
    pub fim_requires_non_thinking_mode: bool,
    pub keep_alive_uses_blank_lines: bool,
    pub streaming_keep_alive_uses_sse_comments: bool,
}

impl Default for DeepSeekProviderQuirkProfile {
    fn default() -> Self {
        Self {
            requires_reasoning_replay_after_tool_call: true,
            ignores_sampling_params_in_thinking_mode: true,
            strict_tools_requires_beta_endpoint: true,
            prefix_completion_requires_beta_endpoint: true,
            fim_requires_non_thinking_mode: true,
            keep_alive_uses_blank_lines: true,
            streaming_keep_alive_uses_sse_comments: true,
        }
    }
}

fn default_primary_base_url() -> String {
    "https://api.deepseek.com".to_owned()
}

fn default_beta_base_url() -> String {
    "https://api.deepseek.com/beta".to_owned()
}

fn default_anthropic_base_url() -> String {
    "https://api.deepseek.com/anthropic".to_owned()
}

fn default_model() -> String {
    "deepseek-v4-flash".to_owned()
}

fn default_fim_model() -> String {
    "deepseek-v4-pro".to_owned()
}

fn default_user_id_strategy() -> Option<String> {
    Some("stable_per_end_user".to_owned())
}

fn read_env_string(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn read_env_strict_tools_mode(name: &str) -> Result<Option<StrictToolsMode>> {
    let Some(value) = read_env_string(name) else {
        return Ok(None);
    };
    match value.as_str() {
        "off" => Ok(Some(StrictToolsMode::Off)),
        "auto" => Ok(Some(StrictToolsMode::Auto)),
        "always" => Ok(Some(StrictToolsMode::Always)),
        _ => Err(anyhow!("invalid {name}: expected one of off, auto, always")),
    }
}

#[cfg(test)]
#[path = "tests/config_tests.rs"]
mod tests;
