use std::{env, ffi::OsString};

use super::*;
use crate::test_env;

struct EnvScope {
    previous: Vec<(&'static str, Option<OsString>)>,
}

impl EnvScope {
    fn set_many(values: &[(&'static str, &str)]) -> Self {
        let names = [
            SIGIL_GEMINI_API_KEY_ENV,
            GEMINI_API_KEY_ENV,
            GOOGLE_API_KEY_ENV,
            SIGIL_GEMINI_MODEL_ENV,
            SIGIL_GEMINI_BASE_URL_ENV,
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
fn default_config_has_stable_endpoint_and_model() {
    let config = GeminiProviderConfig::default();

    assert_eq!(
        config.base_url,
        "https://generativelanguage.googleapis.com/v1beta"
    );
    assert_eq!(config.model, "gemini-2.5-pro");
}

#[test]
fn resolved_config_prefers_sigil_env_then_gemini_then_google() -> anyhow::Result<()> {
    let _guard = test_env::lock();
    let base = GeminiProviderConfig::default();

    {
        let _scope = EnvScope::set_many(&[
            (SIGIL_GEMINI_API_KEY_ENV, "sigil-key"),
            (GEMINI_API_KEY_ENV, "gemini-key"),
            (GOOGLE_API_KEY_ENV, "google-key"),
            (SIGIL_GEMINI_MODEL_ENV, "gemini-test"),
            (
                SIGIL_GEMINI_BASE_URL_ENV,
                "https://gemini.example.com/v1beta",
            ),
        ]);
        let resolved = base.clone().resolved()?;
        assert_eq!(resolved.api_key.as_deref(), Some("sigil-key"));
        assert_eq!(resolved.model, "gemini-test");
        assert_eq!(resolved.base_url, "https://gemini.example.com/v1beta");
    }

    {
        let _scope = EnvScope::set_many(&[
            (SIGIL_GEMINI_API_KEY_ENV, " "),
            (GEMINI_API_KEY_ENV, "gemini-key"),
            (GOOGLE_API_KEY_ENV, "google-key"),
        ]);
        let resolved = base.clone().resolved()?;
        assert_eq!(resolved.api_key.as_deref(), Some("gemini-key"));
    }

    {
        let _scope = EnvScope::set_many(&[
            (SIGIL_GEMINI_API_KEY_ENV, " "),
            (GEMINI_API_KEY_ENV, " "),
            (GOOGLE_API_KEY_ENV, "google-key"),
        ]);
        let resolved = base.resolved()?;
        assert_eq!(resolved.api_key.as_deref(), Some("google-key"));
    }
    Ok(())
}

#[test]
fn config_rejects_legacy_provider_timeout_field() {
    let error = serde_json::from_value::<GeminiProviderConfig>(serde_json::json!({
        "request_timeout_secs": 43
    }))
    .expect_err("provider timeout field should be rejected");

    assert!(error.to_string().contains("request_timeout_secs"));
}
