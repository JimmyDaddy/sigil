use super::*;

#[test]
fn anthropic_capabilities_are_provider_neutral() {
    let capabilities = anthropic_capabilities();

    assert!(capabilities.supports_tool_stream);
    assert!(capabilities.supports_schema_constrained_tools);
    assert!(capabilities.reports_cache_tokens);
    assert!(!capabilities.exact_prefix_cache);
    assert!(!capabilities.supports_reasoning_effort);
    assert_eq!(capabilities.reasoning_stream.as_str(), "unsupported");
    assert_eq!(capabilities.tool_name_max_chars, 64);
}
