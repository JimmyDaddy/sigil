use serde_json::json;
use sigil_kernel::{AgentConfig, ModelRequestConfig, RootConfig};

use super::{
    ANTHROPIC_PROVIDER_KEY, DEEPSEEK_PROVIDER_KEY, DeepSeekProviderConfigFields,
    GEMINI_PROVIDER_KEY, OPENAI_COMPAT_PROVIDER_KEY, ProviderConfigFields, ProviderStrictToolsMode,
    deepseek_provider_config_fields, default_provider_config_fields, normalize_provider_name,
    provider_api_key_env_name, provider_config_fields, provider_status_config_from_fields,
    set_provider_config_fields,
};

fn test_root_config() -> RootConfig {
    RootConfig {
        workspace: Default::default(),
        storage: Default::default(),
        session: Default::default(),
        agent: AgentConfig {
            provider: DEEPSEEK_PROVIDER_KEY.to_owned(),
            model: "deepseek-v4-flash".to_owned(),
            max_turns: None,
            tool_timeout_secs: 30,
        },
        permission: Default::default(),
        model_request: Default::default(),
        memory: Default::default(),
        skills: Default::default(),
        compaction: Default::default(),
        code_intelligence: Default::default(),
        terminal: Default::default(),
        execution: Default::default(),
        verification: Default::default(),
        appearance: Default::default(),
        task: Default::default(),
        providers: Default::default(),
        mcp_servers: Vec::new(),
    }
}

#[test]
fn provider_helpers_normalize_aliases_and_env_labels() {
    assert_eq!(
        normalize_provider_name("openai-compatible"),
        OPENAI_COMPAT_PROVIDER_KEY
    );
    assert_eq!(normalize_provider_name("claude"), ANTHROPIC_PROVIDER_KEY);
    assert_eq!(
        normalize_provider_name("google-gemini"),
        GEMINI_PROVIDER_KEY
    );
    assert_eq!(normalize_provider_name("unknown"), DEEPSEEK_PROVIDER_KEY);

    assert_eq!(provider_api_key_env_name("deepseek"), "SIGIL_API_KEY");
    assert_eq!(
        provider_api_key_env_name("openai_compatible"),
        "SIGIL_OPENAI_COMPATIBLE_API_KEY"
    );
    assert_eq!(
        provider_api_key_env_name("claude"),
        "SIGIL_ANTHROPIC_API_KEY"
    );
    assert_eq!(provider_api_key_env_name("google"), "SIGIL_GEMINI_API_KEY");
}

#[test]
fn provider_config_fields_read_defaults_and_update_provider_blocks() -> anyhow::Result<()> {
    let mut config = test_root_config();
    config.agent.provider = "claude".to_owned();
    config.providers.insert(
        ANTHROPIC_PROVIDER_KEY.to_owned(),
        json!({
            "base_url": "https://anthropic.example.com",
            "model": "claude-old",
            "api_key": "old-key",
            "anthropic_version": "2023-06-01",
            "max_tokens": 2048
        }),
    );

    let draft = provider_config_fields(&config, "claude", "fallback");
    assert_eq!(draft.model, "claude-old");
    assert_eq!(draft.api_key, "old-key");

    let fields = ProviderConfigFields {
        model: " claude-new ".to_owned(),
        api_key: " new-key ".to_owned(),
        base_url: " https://anthropic-proxy.example.com ".to_owned(),
    };
    set_provider_config_fields(&mut config, "claude", &fields, None)?;

    let provider = config.providers[ANTHROPIC_PROVIDER_KEY]
        .as_object()
        .expect("provider should serialize as object");
    assert_eq!(config.agent.provider, ANTHROPIC_PROVIDER_KEY);
    assert_eq!(config.agent.model, "claude-new");
    assert_eq!(provider["model"], "claude-new");
    assert_eq!(provider["api_key"], "new-key");
    assert_eq!(provider["base_url"], "https://anthropic-proxy.example.com");
    assert_eq!(provider["anthropic_version"], "2023-06-01");
    assert_eq!(provider["max_tokens"], 2048);
    assert!(provider.get("request_timeout_secs").is_none());
    Ok(())
}

#[test]
fn deepseek_config_fields_update_provider_specific_surface() -> anyhow::Result<()> {
    let mut config = test_root_config();
    let fields = ProviderConfigFields {
        model: "deepseek-v4-pro".to_owned(),
        api_key: String::new(),
        base_url: "https://deepseek-proxy.example.com".to_owned(),
    };
    let deepseek_fields = DeepSeekProviderConfigFields {
        beta_base_url: "https://deepseek-proxy.example.com/beta".to_owned(),
        anthropic_base_url: "https://deepseek-proxy.example.com/anthropic".to_owned(),
        user_id_strategy: " ".to_owned(),
        strict_tools_mode: ProviderStrictToolsMode::Always,
        fim_model: "deepseek-v4-pro".to_owned(),
    };

    set_provider_config_fields(
        &mut config,
        DEEPSEEK_PROVIDER_KEY,
        &fields,
        Some(&deepseek_fields),
    )?;

    let provider = config.providers[DEEPSEEK_PROVIDER_KEY]
        .as_object()
        .expect("provider should serialize as object");
    assert_eq!(provider["model"], "deepseek-v4-pro");
    assert!(provider.get("api_key").is_none());
    assert!(provider.get("user_id_strategy").is_none());
    assert_eq!(provider["strict_tools_mode"], "always");
    assert_eq!(provider["fim_model"], "deepseek-v4-pro");

    let round_tripped = deepseek_provider_config_fields(&config, "fallback");
    assert_eq!(
        round_tripped.strict_tools_mode,
        ProviderStrictToolsMode::Always
    );
    assert_eq!(round_tripped.user_id_strategy, "stable_per_end_user");
    Ok(())
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
