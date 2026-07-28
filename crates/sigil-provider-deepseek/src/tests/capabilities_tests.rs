use super::*;
use sigil_kernel::CacheMode;

#[test]
fn official_deepseek_route_exposes_implicit_cache_without_explicit_breakpoints() {
    let capabilities = deepseek_context_capabilities(true);

    capabilities.validate().expect("capability must validate");
    assert_eq!(capabilities.cache_mode, CacheMode::ImplicitPrefix);
    assert_eq!(capabilities.explicit_breakpoint_limit, None);
    assert!(capabilities.cache_usage_fields.read_tokens);
    assert!(capabilities.cache_usage_fields.miss_tokens);
    assert!(!capabilities.cache_usage_fields.write_tokens);
}

#[test]
fn deepseek_compatible_proxy_does_not_inherit_vendor_cache_semantics() {
    let capabilities = deepseek_context_capabilities(false);

    capabilities.validate().expect("capability must validate");
    assert_eq!(capabilities.cache_mode, CacheMode::ObservedImplicitOrNone);
    assert!(capabilities.native_compaction.is_none());
}
