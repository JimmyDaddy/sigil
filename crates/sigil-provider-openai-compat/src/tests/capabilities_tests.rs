use super::{openai_compatible_capabilities, openai_compatible_context_capabilities};
use sigil_kernel::{CacheMode, ReasoningStreamSupport};

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

#[test]
fn compatible_route_is_observation_only_even_for_vendor_named_models() {
    let capabilities = openai_compatible_context_capabilities();

    capabilities.validate().expect("capability must validate");
    assert_eq!(capabilities.cache_mode, CacheMode::ObservedImplicitOrNone);
    assert_eq!(capabilities.explicit_breakpoint_limit, None);
    assert!(capabilities.native_compaction.is_none());
    assert!(capabilities.stateful_continuation.is_none());
}
