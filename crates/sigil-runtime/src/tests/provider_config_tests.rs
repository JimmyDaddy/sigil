use std::env;

use serde_json::json;
use sigil_kernel::{ConnectionId, ModelRef, ModelRequestConfig, RootConfig};

use super::{
    ANTHROPIC_PROVIDER_KEY, DEEPSEEK_PROVIDER_KEY, GEMINI_PROVIDER_KEY, OPENAI_COMPAT_PROVIDER_KEY,
    OPENAI_RESPONSES_PROVIDER_KEY, ProviderConfigFields, bundled_provider_models,
    default_provider_config_fields, default_provider_model, next_provider_name,
    normalize_provider_model_alias, normalize_provider_name, provider_api_key_env_name,
    provider_balance_status_config, provider_model_status_config,
    provider_model_status_config_from_fields, provider_status_config_from_fields,
};

fn test_root_config() -> RootConfig {
    test_root_config_for(
        crate::provider_connections::ProviderFamily::DeepSeek,
        crate::provider_connections::ProviderProtocol::DeepSeek,
        "deepseek-default",
        "deepseek-v4-flash",
        "https://api.deepseek.com",
    )
}

fn test_root_config_for(
    family: crate::provider_connections::ProviderFamily,
    protocol: crate::provider_connections::ProviderProtocol,
    connection_id: &str,
    model: &str,
    base_url: &str,
) -> RootConfig {
    let connection_id = ConnectionId::new(connection_id).expect("connection id");
    let mut connection = crate::provider_connections::provider_connection_template(
        family,
        protocol,
        connection_id.clone(),
        "Test connection",
    )
    .expect("connection template")
    .0;
    connection.base_url = base_url.to_owned();
    let runtime_provider =
        crate::provider_connections::runtime_provider_name(&connection).to_owned();
    let base: RootConfig = toml::from_str(
        "config_version = 2\n[agent]\nconnection = \"bootstrap\"\nmodel = \"bootstrap\"\n",
    )
    .expect("base root config");
    let mut root = crate::provider_connections::materialize_root_config(
        &base,
        &std::collections::BTreeMap::from([(connection_id.clone(), connection)]),
        &ModelRef::new(connection_id, model).expect("model ref"),
    )
    .expect("current root config");
    root.agent.runtime_provider = runtime_provider;
    root
}

#[test]
fn provider_helpers_use_only_canonical_names_and_env_labels() {
    assert_eq!(normalize_provider_name("deepseek"), DEEPSEEK_PROVIDER_KEY);
    assert_eq!(
        normalize_provider_name("openai_compat"),
        OPENAI_COMPAT_PROVIDER_KEY
    );
    assert_eq!(
        normalize_provider_name("openai_responses"),
        OPENAI_RESPONSES_PROVIDER_KEY
    );
    assert_eq!(normalize_provider_name("anthropic"), ANTHROPIC_PROVIDER_KEY);
    assert_eq!(normalize_provider_name("gemini"), GEMINI_PROVIDER_KEY);
    assert_eq!(normalize_provider_name("unknown"), "unknown");
    assert_eq!(
        normalize_provider_name("openai-compatible"),
        "openai-compatible"
    );

    assert_eq!(provider_api_key_env_name("deepseek"), Some("SIGIL_API_KEY"));
    assert_eq!(
        provider_api_key_env_name("openai_compat"),
        Some("SIGIL_OPENAI_COMPATIBLE_API_KEY")
    );
    assert_eq!(
        provider_api_key_env_name("openai_responses"),
        Some("SIGIL_OPENAI_RESPONSES_API_KEY")
    );
    assert_eq!(
        provider_api_key_env_name("anthropic"),
        Some("SIGIL_ANTHROPIC_API_KEY")
    );
    assert_eq!(
        provider_api_key_env_name("gemini"),
        Some("SIGIL_GEMINI_API_KEY")
    );
    assert_eq!(provider_api_key_env_name("claude"), None);
}

#[test]
fn provider_model_alias_normalization_is_provider_aware() {
    assert_eq!(
        normalize_provider_model_alias("deepseek", "  flash "),
        Some("deepseek-v4-flash".to_owned())
    );
    assert_eq!(
        normalize_provider_model_alias("deepseek", "v4-pro"),
        Some("deepseek-v4-pro".to_owned())
    );
    assert_eq!(
        normalize_provider_model_alias("openai_compat", "  flash "),
        Some("flash".to_owned())
    );
    assert_eq!(normalize_provider_model_alias("deepseek", "   "), None);
}

#[test]
fn provider_cycling_is_runtime_owned() {
    assert_eq!(next_provider_name("deepseek"), "openai_compat");
    assert_eq!(next_provider_name("openai_compat"), "openai_responses");
    assert_eq!(next_provider_name("openai_responses"), "anthropic");
    assert_eq!(next_provider_name("anthropic"), "gemini");
    assert_eq!(next_provider_name("gemini"), "deepseek");
    assert_eq!(next_provider_name("unknown"), "deepseek");
}

#[test]
fn provider_defaults_are_available_to_provider_neutral_setup_flows() {
    assert_eq!(
        default_provider_model("deepseek").as_deref(),
        Some("deepseek-v4-flash")
    );
    assert_eq!(
        default_provider_model("openai_compat").as_deref(),
        Some("gpt-4.1")
    );
    assert_eq!(
        default_provider_model("openai_responses").as_deref(),
        Some("gpt-4.1")
    );
    assert_eq!(
        default_provider_model("anthropic").as_deref(),
        Some("claude-sonnet-4-5")
    );
    assert_eq!(
        default_provider_model("gemini").as_deref(),
        Some("gemini-2.5-pro")
    );
    assert_eq!(default_provider_model("unknown"), None);

    assert_eq!(
        bundled_provider_models("deepseek"),
        vec!["deepseek-v4-flash", "deepseek-v4-pro"]
    );
    for (provider, expected) in [
        ("openai_compat", "gpt-4.1"),
        ("openai_responses", "gpt-4.1"),
        ("anthropic", "claude-sonnet-4-5"),
        ("gemini", "gemini-2.5-pro"),
    ] {
        let bundled = bundled_provider_models(provider);
        assert_eq!(bundled, vec![expected]);
        assert!(bundled.iter().all(|model| !model.starts_with("deepseek-")));
    }
}

#[test]
fn provider_status_config_from_fields_validates_common_status_surface() {
    let defaults = default_provider_config_fields(DEEPSEEK_PROVIDER_KEY, "deepseek-v4-flash");
    let model_request = ModelRequestConfig {
        request_timeout_secs: 5,
        ..Default::default()
    };
    let status = provider_status_config_from_fields(
        &ProviderConfigFields {
            api_key: " secret ".to_owned(),
            ..defaults.clone()
        },
        &model_request,
    )
    .expect("status config should parse");
    assert_eq!(status.api_key.as_deref(), Some("secret"));
    assert_eq!(status.request_timeout_secs, 5);
    assert!(!status.base_url.is_empty());

    let invalid_model_request = ModelRequestConfig {
        request_timeout_secs: 0,
        ..Default::default()
    };
    let error = provider_status_config_from_fields(
        &ProviderConfigFields { ..defaults },
        &invalid_model_request,
    )
    .expect_err("zero timeout should fail");
    assert_eq!(
        error.to_string(),
        "model_request.request_timeout_secs must be greater than 0"
    );
}

#[test]
fn provider_status_helpers_expose_supported_status_surfaces() {
    let _env_lock = crate::test_env::lock();
    let _base_url = EnvironmentGuard::set("SIGIL_BASE_URL", "https://api.deepseek.com");
    let config = test_root_config();
    let balance = provider_balance_status_config(&config)
        .expect("balance status should resolve")
        .expect("deepseek exposes balance status");
    assert_eq!(balance.base_url, "https://api.deepseek.com");

    let config = test_root_config_for(
        crate::provider_connections::ProviderFamily::Custom,
        crate::provider_connections::ProviderProtocol::OpenAiChatCompletions,
        "custom-default",
        "gpt-test",
        "https://openai.example.com/v1",
    );
    assert!(
        provider_balance_status_config(&config)
            .expect("openai-compatible balance status should resolve")
            .is_none()
    );
    let models = provider_model_status_config(&config)
        .expect("openai-compatible model status should resolve")
        .expect("openai-compatible exposes model listing");
    assert_eq!(models.base_url, "https://openai.example.com/v1");

    let fields = ProviderConfigFields {
        model: "claude".to_owned(),
        api_key: "anthropic-key".to_owned(),
        base_url: "https://anthropic.example.com".to_owned(),
    };
    assert!(
        provider_model_status_config_from_fields(
            ANTHROPIC_PROVIDER_KEY,
            &fields,
            &ModelRequestConfig::default(),
        )
        .expect("anthropic model status should resolve")
        .is_none()
    );
}

#[test]
fn model_status_discovery_uses_the_current_provider_environment_credential() {
    let _env_lock = crate::test_env::lock();
    for (provider_name, model, api_key_env, base_url_env) in [
        (
            DEEPSEEK_PROVIDER_KEY,
            "deepseek-v4-flash",
            "SIGIL_API_KEY",
            "SIGIL_BASE_URL",
        ),
        (
            OPENAI_COMPAT_PROVIDER_KEY,
            "gpt-4.1",
            "SIGIL_OPENAI_COMPATIBLE_API_KEY",
            "SIGIL_OPENAI_COMPATIBLE_BASE_URL",
        ),
        (
            OPENAI_RESPONSES_PROVIDER_KEY,
            "gpt-4.1",
            "SIGIL_OPENAI_RESPONSES_API_KEY",
            "SIGIL_OPENAI_RESPONSES_BASE_URL",
        ),
    ] {
        let expected_base_url = format!("https://{provider_name}.models.example/v1");
        let _api_key = EnvironmentGuard::set(api_key_env, "environment-model-list-secret");
        let _base_url = EnvironmentGuard::set(base_url_env, &expected_base_url);
        let fields = ProviderConfigFields {
            model: model.to_owned(),
            api_key: "inline-secret".to_owned(),
            base_url: "https://default.invalid/v1".to_owned(),
        };

        let status = provider_model_status_config_from_fields(
            provider_name,
            &fields,
            &ModelRequestConfig::default(),
        )
        .expect("model status should resolve")
        .expect("provider should support model discovery");

        assert_eq!(
            status.api_key.as_deref(),
            Some("environment-model-list-secret")
        );
        assert_eq!(status.base_url, expected_base_url);
        let debug = format!("{status:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("environment-model-list-secret"));
    }
}

#[test]
fn root_model_status_discovery_fails_closed_on_malformed_provider_config() {
    let _env_lock = crate::test_env::lock();
    for (provider_name, model, api_key_env, base_url_env) in [
        (
            DEEPSEEK_PROVIDER_KEY,
            "deepseek-v4-flash",
            "SIGIL_API_KEY",
            "SIGIL_BASE_URL",
        ),
        (
            OPENAI_COMPAT_PROVIDER_KEY,
            "gpt-4.1",
            "SIGIL_OPENAI_COMPATIBLE_API_KEY",
            "SIGIL_OPENAI_COMPATIBLE_BASE_URL",
        ),
        (
            OPENAI_RESPONSES_PROVIDER_KEY,
            "gpt-4.1",
            "SIGIL_OPENAI_RESPONSES_API_KEY",
            "SIGIL_OPENAI_RESPONSES_BASE_URL",
        ),
    ] {
        let _api_key = EnvironmentGuard::set(api_key_env, "must-not-be-routed");
        let _base_url = EnvironmentGuard::set(base_url_env, "https://environment.example/v1");
        let (family, protocol) = match provider_name {
            DEEPSEEK_PROVIDER_KEY => (
                crate::provider_connections::ProviderFamily::DeepSeek,
                crate::provider_connections::ProviderProtocol::DeepSeek,
            ),
            OPENAI_COMPAT_PROVIDER_KEY => (
                crate::provider_connections::ProviderFamily::Custom,
                crate::provider_connections::ProviderProtocol::OpenAiChatCompletions,
            ),
            OPENAI_RESPONSES_PROVIDER_KEY => (
                crate::provider_connections::ProviderFamily::OpenAi,
                crate::provider_connections::ProviderProtocol::OpenAiResponses,
            ),
            _ => unreachable!("fixture covers known providers"),
        };
        let mut root_config = test_root_config_for(
            family,
            protocol,
            &format!("{provider_name}-default"),
            model,
            "https://custom-gateway.example/v1",
        );
        root_config.connections.insert(
            format!("{provider_name}-default"),
            json!({
                "label": "Malformed",
                "provider": provider_name,
                "protocol": "invalid",
                "base_url": "https://custom-gateway.example/v1",
                "credential": {"source": "none"},
                "unknown_field": true
            }),
        );

        let error = provider_model_status_config(&root_config)
            .expect_err("malformed provider config must fail before model discovery");
        assert!(format!("{error:#}").contains("invalid"));
        assert!(!format!("{error:#}").contains("must-not-be-routed"));
    }
}

struct EnvironmentGuard {
    name: &'static str,
    previous: Option<String>,
}

impl EnvironmentGuard {
    fn set(name: &'static str, value: &str) -> Self {
        let previous = env::var(name).ok();
        // SAFETY: this test holds the crate-wide environment lock for the guard lifetime.
        unsafe { env::set_var(name, value) };
        Self { name, previous }
    }
}

impl Drop for EnvironmentGuard {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.as_ref() {
            // SAFETY: this test holds the crate-wide environment lock for the guard lifetime.
            unsafe { env::set_var(self.name, previous) };
        } else {
            // SAFETY: this test holds the crate-wide environment lock for the guard lifetime.
            unsafe { env::remove_var(self.name) };
        }
    }
}
