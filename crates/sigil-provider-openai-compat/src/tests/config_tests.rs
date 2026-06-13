use anyhow::Result;

use crate::{
    OPENAI_API_KEY_ENV, OPENAI_COMPATIBLE_API_KEY_ENV, OPENAI_COMPATIBLE_BASE_URL_ENV,
    OPENAI_COMPATIBLE_MODEL_ENV, OPENAI_COMPATIBLE_REQUEST_TIMEOUT_SECS_ENV,
    OpenAiCompatibleProviderConfig,
};

#[test]
fn default_config_uses_openai_v1_base_and_default_model() {
    let config = OpenAiCompatibleProviderConfig::default();

    assert_eq!(config.base_url, "https://api.openai.com/v1");
    assert_eq!(config.model, "gpt-4.1");
    assert_eq!(config.request_timeout_secs, 120);
    assert_eq!(config.api_key, None);
}

#[test]
fn default_for_model_overrides_only_model() {
    let config = OpenAiCompatibleProviderConfig::default_for_model("custom-model");

    assert_eq!(config.model, "custom-model");
    assert_eq!(config.base_url, "https://api.openai.com/v1");
}

#[test]
fn resolved_config_prefers_sigil_specific_env_over_openai_key() -> Result<()> {
    let _guard = crate::test_env::lock();
    let _scope = EnvScope::set_many(&[
        (OPENAI_COMPATIBLE_MODEL_ENV, "env-model"),
        (
            OPENAI_COMPATIBLE_BASE_URL_ENV,
            "https://proxy.example.test/v1",
        ),
        (OPENAI_COMPATIBLE_REQUEST_TIMEOUT_SECS_ENV, "30"),
        (OPENAI_COMPATIBLE_API_KEY_ENV, "sigil-key"),
        (OPENAI_API_KEY_ENV, "openai-key"),
    ]);

    let resolved = OpenAiCompatibleProviderConfig {
        model: "config-model".to_owned(),
        base_url: "https://config.example.test/v1".to_owned(),
        api_key: Some("config-key".to_owned()),
        request_timeout_secs: 10,
        ..OpenAiCompatibleProviderConfig::default()
    }
    .resolved()?;

    assert_eq!(resolved.model, "env-model");
    assert_eq!(resolved.base_url, "https://proxy.example.test/v1");
    assert_eq!(resolved.request_timeout_secs, 30);
    assert_eq!(resolved.api_key.as_deref(), Some("sigil-key"));
    Ok(())
}

#[test]
fn resolved_config_uses_openai_api_key_fallback_and_skips_blank_env() -> Result<()> {
    let _guard = crate::test_env::lock();
    let _scope = EnvScope::set_many(&[
        (OPENAI_COMPATIBLE_API_KEY_ENV, "   "),
        (OPENAI_API_KEY_ENV, "openai-key"),
        (OPENAI_COMPATIBLE_MODEL_ENV, "   "),
    ]);

    let resolved = OpenAiCompatibleProviderConfig::default_for_model("config-model").resolved()?;

    assert_eq!(resolved.model, "config-model");
    assert_eq!(resolved.api_key.as_deref(), Some("openai-key"));
    Ok(())
}

#[test]
fn resolved_config_rejects_invalid_timeout_env() {
    let _guard = crate::test_env::lock();
    let _scope = EnvScope::set_many(&[(OPENAI_COMPATIBLE_REQUEST_TIMEOUT_SECS_ENV, "0")]);

    let error = OpenAiCompatibleProviderConfig::default()
        .resolved()
        .expect_err("zero timeout should fail");

    assert!(
        error
            .to_string()
            .contains("SIGIL_OPENAI_COMPATIBLE_REQUEST_TIMEOUT_SECS must be greater than 0")
    );
}

struct EnvScope {
    saved: Vec<(&'static str, Option<String>)>,
}

impl EnvScope {
    fn set_many(values: &[(&'static str, &'static str)]) -> Self {
        let saved = values
            .iter()
            .map(|(name, _)| (*name, std::env::var(name).ok()))
            .collect::<Vec<_>>();
        for (name, value) in values {
            unsafe {
                std::env::set_var(name, value);
            }
        }
        Self { saved }
    }
}

impl Drop for EnvScope {
    fn drop(&mut self) {
        for (name, value) in self.saved.drain(..) {
            unsafe {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }
}
