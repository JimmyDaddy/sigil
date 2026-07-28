use super::{openai_responses_capabilities, openai_responses_context_capabilities};
use sigil_kernel::{CacheMode, NativeCarrierPortability};

#[test]
fn responses_capabilities_advertise_streamed_reasoning_and_tool_calls_without_remote_resume() {
    let capabilities = openai_responses_capabilities();

    assert!(capabilities.supports_tool_stream);
    assert!(capabilities.supports_reasoning_effort);
    assert!(capabilities.can_surface_reasoning_stream());
    assert!(!capabilities.supports_response_handles);
    assert!(!capabilities.supports_background_tasks);
}

#[test]
fn official_responses_route_exposes_logical_breakpoints_and_bound_native_carrier() {
    let capabilities = openai_responses_context_capabilities(true);

    capabilities.validate().expect("capability must validate");
    assert_eq!(
        capabilities.cache_mode,
        CacheMode::ImplicitPrefixWithLogicalBreakpoints
    );
    assert_eq!(capabilities.explicit_breakpoint_limit, Some(2));
    assert!(capabilities.cache_usage_fields.read_tokens);
    assert_eq!(
        capabilities.native_carrier_portability,
        NativeCarrierPortability::ConnectionModelProtocolBound
    );
}

#[test]
fn custom_responses_endpoint_fails_closed() {
    let capabilities = openai_responses_context_capabilities(false);

    capabilities.validate().expect("capability must validate");
    assert_eq!(capabilities.cache_mode, CacheMode::Unknown);
    assert!(capabilities.native_compaction.is_none());
}
