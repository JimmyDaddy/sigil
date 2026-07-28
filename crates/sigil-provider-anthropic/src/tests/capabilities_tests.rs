use super::*;

#[test]
fn anthropic_capabilities_are_provider_neutral() {
    let capabilities = anthropic_capabilities();

    assert!(capabilities.supports_tool_stream);
    assert!(capabilities.supports_schema_constrained_tools);
    assert!(capabilities.reports_cache_tokens);
    assert!(!capabilities.exact_prefix_cache);
    assert!(!capabilities.supports_reasoning_effort);
    assert_eq!(capabilities.reasoning_stream.as_str(), "native");
    assert!(capabilities.can_surface_reasoning_stream());
    assert_eq!(capabilities.tool_name_max_chars, 64);
}

#[test]
fn official_anthropic_route_reserves_four_explicit_breakpoints_and_two_ttls() {
    let capabilities = anthropic_context_capabilities(true, true);

    capabilities.validate().expect("capability must validate");
    assert_eq!(
        capabilities.cache_mode,
        sigil_kernel::CacheMode::ExplicitBreakpoints
    );
    assert_eq!(capabilities.explicit_breakpoint_limit, Some(4));
    assert_eq!(
        capabilities
            .cache_ttls
            .iter()
            .map(|ttl| (ttl.seconds, ttl.is_default))
            .collect::<Vec<_>>(),
        vec![(300, true), (3_600, false)]
    );
    assert!(capabilities.cache_usage_fields.read_tokens);
    assert!(capabilities.cache_usage_fields.write_tokens);
    assert!(capabilities.native_compaction.is_some());
}

#[test]
fn custom_anthropic_endpoint_fails_closed() {
    let capabilities = anthropic_context_capabilities(false, true);

    capabilities.validate().expect("capability must validate");
    assert_eq!(capabilities.cache_mode, sigil_kernel::CacheMode::Unknown);
    assert!(capabilities.native_compaction.is_none());
}
