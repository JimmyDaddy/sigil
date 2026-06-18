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
