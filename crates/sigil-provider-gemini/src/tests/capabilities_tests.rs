use super::*;

#[test]
fn gemini_capabilities_are_provider_neutral() {
    let capabilities = gemini_capabilities();

    assert!(capabilities.supports_schema_constrained_tools);
    assert!(capabilities.supports_structured_output);
    assert!(capabilities.reports_cache_tokens);
    assert!(!capabilities.supports_tool_stream);
    assert!(!capabilities.supports_reasoning_effort);
    assert_eq!(capabilities.reasoning_stream.as_str(), "unsupported");
    assert_eq!(capabilities.tool_name_max_chars, 64);
}

#[test]
fn official_gemini_generate_content_route_exposes_only_implicit_cache() {
    let capabilities = gemini_context_capabilities(true);

    capabilities.validate().expect("capability must validate");
    assert_eq!(
        capabilities.cache_mode,
        sigil_kernel::CacheMode::ImplicitPrefix
    );
    assert!(capabilities.cache_usage_fields.read_tokens);
    assert!(capabilities.stateful_continuation.is_none());
}

#[test]
fn custom_gemini_endpoint_fails_closed() {
    let capabilities = gemini_context_capabilities(false);

    capabilities.validate().expect("capability must validate");
    assert_eq!(capabilities.cache_mode, sigil_kernel::CacheMode::Unknown);
    assert!(capabilities.stateful_continuation.is_none());
}
