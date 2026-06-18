use std::{env, ffi::OsString};

use super::*;
use crate::test_env;

struct EnvScope {
    previous: Vec<(&'static str, Option<OsString>)>,
}

impl EnvScope {
    fn set_many(values: &[(&'static str, &str)]) -> Self {
        let names = [
            SIGIL_ANTHROPIC_API_KEY_ENV,
            ANTHROPIC_API_KEY_ENV,
            SIGIL_ANTHROPIC_MODEL_ENV,
            SIGIL_ANTHROPIC_BASE_URL_ENV,
            SIGIL_ANTHROPIC_VERSION_ENV,
            SIGIL_ANTHROPIC_MAX_TOKENS_ENV,
            SIGIL_ANTHROPIC_REQUEST_TIMEOUT_SECS_ENV,
        ];
        let previous = names
            .into_iter()
            .map(|name| (name, env::var_os(name)))
            .collect::<Vec<_>>();
        for name in names {
            unsafe {
                env::remove_var(name);
            }
        }
        for (name, value) in values {
            unsafe {
                env::set_var(name, value);
            }
        }
        Self { previous }
    }
}

impl Drop for EnvScope {
    fn drop(&mut self) {
        for (name, value) in self.previous.drain(..) {
            unsafe {
                if let Some(value) = value {
                    env::set_var(name, value);
                } else {
                    env::remove_var(name);
                }
            }
        }
    }
}

#[test]
fn default_config_has_stable_endpoint_model_and_limits() {
    let config = AnthropicProviderConfig::default();

    assert_eq!(config.base_url, "https://api.anthropic.com");
    assert_eq!(config.model, "claude-sonnet-4-5");
    assert_eq!(config.anthropic_version, "2023-06-01");
    assert_eq!(config.max_tokens, 4096);
    assert_eq!(config.request_timeout_secs, 120);
}

#[test]
fn resolved_config_prefers_sigil_env_over_provider_env() -> anyhow::Result<()> {
    let _guard = test_env::lock();
    let _scope = EnvScope::set_many(&[
        (ANTHROPIC_API_KEY_ENV, "provider-key"),
        (SIGIL_ANTHROPIC_API_KEY_ENV, "sigil-key"),
        (SIGIL_ANTHROPIC_MODEL_ENV, "claude-test"),
        (
            SIGIL_ANTHROPIC_BASE_URL_ENV,
            "https://anthropic.example.com",
        ),
        (SIGIL_ANTHROPIC_VERSION_ENV, "2024-01-01"),
        (SIGIL_ANTHROPIC_MAX_TOKENS_ENV, "1234"),
        (SIGIL_ANTHROPIC_REQUEST_TIMEOUT_SECS_ENV, "42"),
    ]);

    let resolved = AnthropicProviderConfig::default().resolved()?;

    assert_eq!(resolved.api_key.as_deref(), Some("sigil-key"));
    assert_eq!(resolved.model, "claude-test");
    assert_eq!(resolved.base_url, "https://anthropic.example.com");
    assert_eq!(resolved.anthropic_version, "2024-01-01");
    assert_eq!(resolved.max_tokens, 1234);
    assert_eq!(resolved.request_timeout_secs, 42);
    Ok(())
}

#[test]
fn resolved_config_rejects_zero_numeric_env() {
    let _guard = test_env::lock();
    let _scope = EnvScope::set_many(&[(SIGIL_ANTHROPIC_MAX_TOKENS_ENV, "0")]);

    let error = AnthropicProviderConfig::default()
        .resolved()
        .expect_err("zero max tokens should fail");

    assert!(
        error
            .to_string()
            .contains("SIGIL_ANTHROPIC_MAX_TOKENS must be greater than 0")
    );
}
