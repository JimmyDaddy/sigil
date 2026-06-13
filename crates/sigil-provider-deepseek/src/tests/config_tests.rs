use std::{env, ffi::OsString};

use anyhow::Result;

use super::{
    DeepSeekProviderConfig, LEGACY_DEEPSEEK_API_KEY_ENV, SIGIL_ANTHROPIC_BASE_URL_ENV,
    SIGIL_API_KEY_ENV, SIGIL_BASE_URL_ENV, SIGIL_BETA_BASE_URL_ENV, SIGIL_FIM_MODEL_ENV,
    SIGIL_MODEL_ENV, SIGIL_REQUEST_TIMEOUT_SECS_ENV, SIGIL_STRICT_TOOLS_MODE_ENV,
    SIGIL_USER_ID_STRATEGY_ENV, StrictToolsMode,
};

#[test]
fn default_config_deserializes_all_provider_defaults() -> Result<()> {
    let config: DeepSeekProviderConfig = serde_json::from_value(serde_json::json!({}))?;

    assert_eq!(config.base_url, "https://api.deepseek.com");
    assert_eq!(config.beta_base_url, "https://api.deepseek.com/beta");
    assert_eq!(
        config.anthropic_base_url,
        "https://api.deepseek.com/anthropic"
    );
    assert_eq!(config.model, "deepseek-v4-flash");
    assert_eq!(config.fim_model, "deepseek-v4-pro");
    assert_eq!(
        config.user_id_strategy.as_deref(),
        Some("stable_per_end_user")
    );
    assert_eq!(config.request_timeout_secs, 120);
    assert_eq!(config.strict_tools_mode, StrictToolsMode::Auto);
    Ok(())
}

#[test]
fn resolved_applies_sigil_env_overrides() -> Result<()> {
    let _guard = crate::test_env::lock();
    let _scope = EnvScope::set_many(&[
        (SIGIL_API_KEY_ENV, "env-key"),
        (SIGIL_MODEL_ENV, "env-model"),
        (SIGIL_BASE_URL_ENV, "https://example.invalid/openai"),
        (SIGIL_BETA_BASE_URL_ENV, "https://example.invalid/beta"),
        (
            SIGIL_ANTHROPIC_BASE_URL_ENV,
            "https://example.invalid/anthropic",
        ),
        (SIGIL_USER_ID_STRATEGY_ENV, "stable_per_workspace"),
        (SIGIL_FIM_MODEL_ENV, "env-fim"),
        (SIGIL_REQUEST_TIMEOUT_SECS_ENV, "9"),
        (SIGIL_STRICT_TOOLS_MODE_ENV, "always"),
    ]);

    let resolved = DeepSeekProviderConfig {
        base_url: "https://api.deepseek.com".to_owned(),
        beta_base_url: "https://api.deepseek.com/beta".to_owned(),
        anthropic_base_url: "https://api.deepseek.com/anthropic".to_owned(),
        model: "file-model".to_owned(),
        api_key: None,
        user_id_strategy: None,
        strict_tools_mode: StrictToolsMode::Auto,
        fim_model: "file-fim".to_owned(),
        request_timeout_secs: 120,
    }
    .resolved()?;

    assert_eq!(resolved.api_key.as_deref(), Some("env-key"));
    assert_eq!(resolved.model, "env-model");
    assert_eq!(resolved.base_url, "https://example.invalid/openai");
    assert_eq!(resolved.beta_base_url, "https://example.invalid/beta");
    assert_eq!(
        resolved.anthropic_base_url,
        "https://example.invalid/anthropic"
    );
    assert_eq!(
        resolved.user_id_strategy.as_deref(),
        Some("stable_per_workspace")
    );
    assert_eq!(resolved.fim_model, "env-fim");
    assert_eq!(resolved.request_timeout_secs, 9);
    assert_eq!(resolved.strict_tools_mode, StrictToolsMode::Always);
    Ok(())
}

#[test]
fn resolved_uses_legacy_api_key_when_sigil_api_key_is_missing() -> Result<()> {
    let _guard = crate::test_env::lock();
    let _scope = EnvScope::set_many(&[(LEGACY_DEEPSEEK_API_KEY_ENV, "legacy-key")]);

    let resolved = file_config().resolved()?;

    assert_eq!(resolved.api_key.as_deref(), Some("legacy-key"));
    Ok(())
}

#[test]
fn resolved_api_key_env_overrides_file_value() -> Result<()> {
    let _guard = crate::test_env::lock();
    let _scope = EnvScope::set_many(&[(SIGIL_API_KEY_ENV, "env-key")]);

    let resolved = DeepSeekProviderConfig {
        api_key: Some("file-key".to_owned()),
        ..file_config()
    }
    .resolved()?;

    assert_eq!(resolved.api_key.as_deref(), Some("env-key"));
    Ok(())
}

#[test]
fn resolved_rejects_invalid_timeout_override() {
    let _guard = crate::test_env::lock();
    let _scope = EnvScope::set_many(&[(SIGIL_REQUEST_TIMEOUT_SECS_ENV, "0")]);

    let error = file_config()
        .resolved()
        .expect_err("timeout=0 should be rejected");

    assert!(error.to_string().contains(SIGIL_REQUEST_TIMEOUT_SECS_ENV));
}

#[test]
fn resolved_rejects_non_numeric_timeout_override() {
    let _guard = crate::test_env::lock();
    let _scope = EnvScope::set_many(&[(SIGIL_REQUEST_TIMEOUT_SECS_ENV, "soon")]);

    let error = file_config()
        .resolved()
        .expect_err("non-numeric timeout should be rejected");

    assert!(
        error
            .to_string()
            .contains("invalid SIGIL_REQUEST_TIMEOUT_SECS")
    );
}

#[test]
fn resolved_ignores_blank_string_env_overrides() -> Result<()> {
    let _guard = crate::test_env::lock();
    let _scope = EnvScope::set_many(&[
        (SIGIL_API_KEY_ENV, "   "),
        (SIGIL_MODEL_ENV, "   "),
        (SIGIL_BASE_URL_ENV, "   "),
    ]);

    let resolved = DeepSeekProviderConfig {
        api_key: Some("file-key".to_owned()),
        ..file_config()
    }
    .resolved()?;

    assert_eq!(resolved.api_key.as_deref(), Some("file-key"));
    assert_eq!(resolved.model, "file-model");
    assert_eq!(resolved.base_url, "https://api.deepseek.com");
    Ok(())
}

#[test]
fn resolved_rejects_invalid_strict_tools_mode_override() {
    let _guard = crate::test_env::lock();
    let _scope = EnvScope::set_many(&[(SIGIL_STRICT_TOOLS_MODE_ENV, "strict")]);

    let error = file_config()
        .resolved()
        .expect_err("unknown strict tools mode should be rejected");

    assert!(error.to_string().contains(SIGIL_STRICT_TOOLS_MODE_ENV));
}

#[test]
fn profile_exposes_deepseek_defaults_and_quirks() {
    let profile = file_config().profile();

    assert_eq!(profile.primary_base_url, "https://api.deepseek.com");
    assert_eq!(profile.beta_base_url, "https://api.deepseek.com/beta");
    assert_eq!(
        profile.anthropic_base_url,
        "https://api.deepseek.com/anthropic"
    );
    assert_eq!(profile.default_model, "file-model");
    assert_eq!(profile.default_fim_model, "file-fim");
    assert!(profile.default_thinking);
    assert_eq!(
        profile.default_reasoning_effort,
        sigil_kernel::ReasoningEffort::Max
    );
    assert!(profile.quirks.requires_reasoning_replay_after_tool_call);
    assert!(profile.quirks.ignores_sampling_params_in_thinking_mode);
    assert!(profile.quirks.strict_tools_requires_beta_endpoint);
    assert!(profile.quirks.prefix_completion_requires_beta_endpoint);
    assert!(profile.quirks.fim_requires_non_thinking_mode);
    assert!(profile.quirks.keep_alive_uses_blank_lines);
    assert!(profile.quirks.streaming_keep_alive_uses_sse_comments);
}

fn file_config() -> DeepSeekProviderConfig {
    DeepSeekProviderConfig {
        base_url: "https://api.deepseek.com".to_owned(),
        beta_base_url: "https://api.deepseek.com/beta".to_owned(),
        anthropic_base_url: "https://api.deepseek.com/anthropic".to_owned(),
        model: "file-model".to_owned(),
        api_key: None,
        user_id_strategy: None,
        strict_tools_mode: StrictToolsMode::Auto,
        fim_model: "file-fim".to_owned(),
        request_timeout_secs: 120,
    }
}

struct EnvScope {
    saved: Vec<(&'static str, Option<OsString>)>,
}

impl EnvScope {
    fn set_many(values: &[(&'static str, &'static str)]) -> Self {
        let mut saved = Vec::with_capacity(values.len());
        for (name, value) in values {
            saved.push((*name, env::var_os(name)));
            // SAFETY: tests serialize process-wide env mutation with ENV_LOCK.
            unsafe { env::set_var(name, value) };
        }
        Self { saved }
    }
}

impl Drop for EnvScope {
    fn drop(&mut self) {
        for (name, value) in self.saved.drain(..).rev() {
            match value {
                Some(value) => {
                    // SAFETY: tests serialize process-wide env mutation with ENV_LOCK.
                    unsafe { env::set_var(name, value) };
                }
                None => {
                    // SAFETY: tests serialize process-wide env mutation with ENV_LOCK.
                    unsafe { env::remove_var(name) };
                }
            }
        }
    }
}
