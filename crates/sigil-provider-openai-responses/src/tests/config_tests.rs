use anyhow::Result;

use crate::{
    OPENAI_RESPONSES_API_KEY_ENV, OPENAI_RESPONSES_BASE_URL_ENV, OpenAiResponsesProviderConfig,
};

#[test]
fn default_config_uses_openai_v1_base_and_default_model() {
    let config = OpenAiResponsesProviderConfig::default();

    assert_eq!(config.base_url, "https://api.openai.com/v1");
    assert_eq!(config.model, "gpt-4.1");
    assert_eq!(config.api_key, None);
}

#[test]
fn resolved_config_uses_responses_specific_environment_overrides() -> Result<()> {
    let _guard = crate::test_env::lock();
    let _scope = EnvScope::set_many(&[
        (
            OPENAI_RESPONSES_BASE_URL_ENV,
            "https://proxy.example.test/v1",
        ),
        (OPENAI_RESPONSES_API_KEY_ENV, "responses-key"),
        ("OPENAI_API_KEY", "unrelated-key"),
    ]);

    let resolved = OpenAiResponsesProviderConfig::default_for_model("gpt-test").resolved()?;

    assert_eq!(resolved.base_url, "https://proxy.example.test/v1");
    assert_eq!(resolved.api_key.as_deref(), Some("responses-key"));
    Ok(())
}

#[test]
fn config_rejects_provider_model_field() {
    let error = serde_json::from_value::<OpenAiResponsesProviderConfig>(serde_json::json!({
        "model": "gpt-test"
    }))
    .expect_err("model belongs to [agent]");

    assert!(error.to_string().contains("model"));
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
