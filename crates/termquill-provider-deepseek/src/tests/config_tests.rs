use std::{
    env,
    ffi::OsString,
    sync::{Mutex, OnceLock},
};

use anyhow::Result;

use super::{
    DeepSeekProviderConfig, StrictToolsMode, TERMQUILL_API_KEY_ENV, TERMQUILL_BASE_URL_ENV,
    TERMQUILL_BETA_BASE_URL_ENV, TERMQUILL_FIM_MODEL_ENV, TERMQUILL_MODEL_ENV,
    TERMQUILL_REQUEST_TIMEOUT_SECS_ENV, TERMQUILL_STRICT_TOOLS_MODE_ENV,
    TERMQUILL_USER_ID_STRATEGY_ENV,
};

static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[test]
fn resolved_applies_termquill_env_overrides() -> Result<()> {
    let _guard = ENV_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("env lock poisoned");
    let _scope = EnvScope::set_many(&[
        (TERMQUILL_API_KEY_ENV, "env-key"),
        (TERMQUILL_MODEL_ENV, "env-model"),
        (TERMQUILL_BASE_URL_ENV, "https://example.invalid/openai"),
        (TERMQUILL_BETA_BASE_URL_ENV, "https://example.invalid/beta"),
        (TERMQUILL_USER_ID_STRATEGY_ENV, "stable_per_workspace"),
        (TERMQUILL_FIM_MODEL_ENV, "env-fim"),
        (TERMQUILL_REQUEST_TIMEOUT_SECS_ENV, "9"),
        (TERMQUILL_STRICT_TOOLS_MODE_ENV, "always"),
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
        resolved.user_id_strategy.as_deref(),
        Some("stable_per_workspace")
    );
    assert_eq!(resolved.fim_model, "env-fim");
    assert_eq!(resolved.request_timeout_secs, 9);
    assert_eq!(resolved.strict_tools_mode, StrictToolsMode::Always);
    Ok(())
}

#[test]
fn resolved_api_key_env_overrides_file_value() -> Result<()> {
    let _guard = ENV_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("env lock poisoned");
    let _scope = EnvScope::set_many(&[(TERMQUILL_API_KEY_ENV, "env-key")]);

    let resolved = DeepSeekProviderConfig {
        base_url: "https://api.deepseek.com".to_owned(),
        beta_base_url: "https://api.deepseek.com/beta".to_owned(),
        anthropic_base_url: "https://api.deepseek.com/anthropic".to_owned(),
        model: "file-model".to_owned(),
        api_key: Some("file-key".to_owned()),
        user_id_strategy: None,
        strict_tools_mode: StrictToolsMode::Auto,
        fim_model: "file-fim".to_owned(),
        request_timeout_secs: 120,
    }
    .resolved()?;

    assert_eq!(resolved.api_key.as_deref(), Some("env-key"));
    Ok(())
}

#[test]
fn resolved_rejects_invalid_timeout_override() {
    let _guard = ENV_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("env lock poisoned");
    let _scope = EnvScope::set_many(&[(TERMQUILL_REQUEST_TIMEOUT_SECS_ENV, "0")]);

    let error = DeepSeekProviderConfig {
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
    .resolved()
    .expect_err("timeout=0 should be rejected");

    assert!(
        error
            .to_string()
            .contains(TERMQUILL_REQUEST_TIMEOUT_SECS_ENV)
    );
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
