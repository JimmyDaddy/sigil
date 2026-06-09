use std::env;

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

use termquill_kernel::ReasoningEffort;

pub const TERMQUILL_API_KEY_ENV: &str = "TERMQUILL_API_KEY";
pub const LEGACY_TERMQUILL_DEEPSEEK_API_KEY_ENV: &str = "TERMQUILL_DEEPSEEK_API_KEY";
pub const LEGACY_DEEPSEEK_API_KEY_ENV: &str = "DEEPSEEK_API_KEY";
pub const TERMQUILL_MODEL_ENV: &str = "TERMQUILL_MODEL";
pub const TERMQUILL_BASE_URL_ENV: &str = "TERMQUILL_BASE_URL";
pub const TERMQUILL_BETA_BASE_URL_ENV: &str = "TERMQUILL_BETA_BASE_URL";
pub const TERMQUILL_ANTHROPIC_BASE_URL_ENV: &str = "TERMQUILL_ANTHROPIC_BASE_URL";
pub const TERMQUILL_USER_ID_STRATEGY_ENV: &str = "TERMQUILL_USER_ID_STRATEGY";
pub const TERMQUILL_FIM_MODEL_ENV: &str = "TERMQUILL_FIM_MODEL";
pub const TERMQUILL_REQUEST_TIMEOUT_SECS_ENV: &str = "TERMQUILL_REQUEST_TIMEOUT_SECS";
pub const TERMQUILL_STRICT_TOOLS_MODE_ENV: &str = "TERMQUILL_STRICT_TOOLS_MODE";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeepSeekProviderConfig {
    #[serde(default = "default_primary_base_url")]
    pub base_url: String,
    #[serde(default = "default_beta_base_url")]
    pub beta_base_url: String,
    #[serde(default = "default_anthropic_base_url")]
    pub anthropic_base_url: String,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default = "default_user_id_strategy")]
    pub user_id_strategy: Option<String>,
    #[serde(default)]
    pub strict_tools_mode: StrictToolsMode,
    #[serde(default = "default_fim_model")]
    pub fim_model: String,
    #[serde(default = "default_request_timeout_secs")]
    pub request_timeout_secs: u64,
}

impl DeepSeekProviderConfig {
    pub fn resolved(self) -> Result<Self> {
        let mut resolved = self;

        if let Some(value) = read_env_string(TERMQUILL_MODEL_ENV) {
            resolved.model = value;
        }
        if let Some(value) = read_env_string(TERMQUILL_BASE_URL_ENV) {
            resolved.base_url = value;
        }
        if let Some(value) = read_env_string(TERMQUILL_BETA_BASE_URL_ENV) {
            resolved.beta_base_url = value;
        }
        if let Some(value) = read_env_string(TERMQUILL_ANTHROPIC_BASE_URL_ENV) {
            resolved.anthropic_base_url = value;
        }
        if let Some(value) = read_env_string(TERMQUILL_USER_ID_STRATEGY_ENV) {
            resolved.user_id_strategy = Some(value);
        }
        if let Some(value) = read_env_string(TERMQUILL_FIM_MODEL_ENV) {
            resolved.fim_model = value;
        }
        if let Some(value) = read_env_u64(TERMQUILL_REQUEST_TIMEOUT_SECS_ENV)? {
            resolved.request_timeout_secs = value;
        }
        if let Some(value) = read_env_strict_tools_mode(TERMQUILL_STRICT_TOOLS_MODE_ENV)? {
            resolved.strict_tools_mode = value;
        }
        if let Some(value) = read_env_string(TERMQUILL_API_KEY_ENV)
            .or_else(|| read_env_string(LEGACY_TERMQUILL_DEEPSEEK_API_KEY_ENV))
            .or_else(|| read_env_string(LEGACY_DEEPSEEK_API_KEY_ENV))
        {
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
