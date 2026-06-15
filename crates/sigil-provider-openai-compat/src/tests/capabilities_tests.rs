use super::openai_compatible_capabilities;
use sigil_kernel::ReasoningStreamSupport;

#[test]
fn capabilities_are_generic_chat_completion_defaults() {
    let capabilities = openai_compatible_capabilities();

    assert!(capabilities.supports_tool_stream);
    assert!(capabilities.reports_cache_tokens);
    assert!(capabilities.supports_system_fingerprint);
    assert_eq!(
        capabilities.reasoning_stream,
        ReasoningStreamSupport::Passthrough
    );
    assert!(capabilities.can_surface_reasoning_stream());
    assert!(!capabilities.supports_reasoning_effort);
    assert!(!capabilities.supports_infill_completion);
    assert_eq!(capabilities.tool_name_max_chars, 64);
}
