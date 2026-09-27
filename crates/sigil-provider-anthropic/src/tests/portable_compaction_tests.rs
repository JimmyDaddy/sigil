use super::*;
use crate::AnthropicProviderConfig;
use anyhow::Result;
use sigil_kernel::{CompletionRequest, ModelMessage, ModelRequestTimeouts};

fn provider(base_url: &str) -> Result<AnthropicProvider> {
    let _guard = crate::test_env::lock();
    AnthropicProvider::new_exact(
        AnthropicProviderConfig {
            base_url: base_url.to_owned(),
            model: DEEPSEEK_ANTHROPIC_PORTABLE_TARGET_MODEL.to_owned(),
            ..AnthropicProviderConfig::default()
        },
        ModelRequestTimeouts::default(),
    )
}
#[test]
fn deepseek_messages_profile_binds_endpoint_model_headers_and_cache_semantics() -> Result<()> {
    let mut official = provider("https://api.deepseek.com/anthropic")?;
    assert!(official.deepseek_portable_profile("deepseek-flash"));
    assert!(!official.deepseek_portable_profile("deepseek-v4-flash"));
    assert!(
        !provider("https://api.deepseek.com.attacker.invalid/anthropic")?
            .deepseek_portable_profile("deepseek-flash")
    );
    assert!(!provider("https://api.anthropic.com")?.deepseek_portable_profile("deepseek-flash"));
    official
        .config
        .beta_headers
        .push("changed-shaping".to_owned());
    assert!(!official.deepseek_portable_profile("deepseek-flash"));
    let capabilities = deepseek_context_capabilities();
    capabilities.validate()?;
    assert_eq!(capabilities.cache_mode, CacheMode::ImplicitPrefix);
    assert_eq!(capabilities.explicit_breakpoint_limit, None);
    Ok(())
}
#[tokio::test]
async fn deepseek_messages_count_requires_explicit_output_before_network() -> Result<()> {
    let request = CompletionRequest {
        provider_name: "anthropic".to_owned(),
        model_name: "deepseek-flash".to_owned(),
        messages: vec![ModelMessage::user("input")],
        tools: Vec::new(),
        temperature: None,
        max_tokens: None,
        reasoning_effort: None,
        previous_response_handle: None,
        continuation_states: Vec::new(),
        traffic_partition_key: None,
        background: false,
        store: false,
        deterministic_materialization: true,
        hosted_tools: Vec::new(),
    };
    let error = provider("https://api.deepseek.com/anthropic")?
        .deepseek_portable_target(
            FrozenProviderRequestMaterial::freeze("test", request)?,
            PortableCompactionRequestRole::Target,
        )
        .await
        .expect_err("missing output reservation");
    assert!(error.to_string().contains("explicit portable output"));
    Ok(())
}
#[test]
fn deepseek_messages_overflow_requires_exact_envelope_and_token_algebra() {
    let message = "This model's maximum context length is 1048576 tokens. However, you requested 1082799 tokens (1050031 in the messages, 32768 in the completion). Please reduce the length of the messages or completion. (request_id: fixture-1)";
    let body=serde_json::json!({"error":{"type":"invalid_request_error","code":"invalid_request_error","message":message}}).to_string();
    assert!(context_window_rejection(&body, 32768));
    assert!(!context_window_rejection(&body, 1024));
    assert!(!context_window_rejection(
        &body.replace("1082799", "1082798"),
        32768
    ));
    assert!(!context_window_rejection(
        &body.replace("1048576", "2000000"),
        32768
    ));
    assert!(!context_window_rejection(
        &body.replace("invalid_request_error", "context_error"),
        32768
    ));
    assert!(!context_window_rejection("context length exceeded", 32768));
}
