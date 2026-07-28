use sigil_kernel::{
    CacheMode, CacheTtl, CacheUsageCapabilities, NativeCarrierPortability,
    NativeCompactionCapability, ProviderCapabilities, ProviderContextCapabilities,
    ReasoningStreamSupport,
};

pub fn anthropic_capabilities() -> ProviderCapabilities {
    ProviderCapabilities {
        exact_prefix_cache: false,
        reports_cache_tokens: true,
        reasoning_stream: ReasoningStreamSupport::Native,
        supports_reasoning_effort: false,
        supports_tool_stream: true,
        supports_background_tasks: false,
        supports_response_handles: false,
        supports_reasoning_artifacts: false,
        supports_structured_output: false,
        supports_assistant_prefix_seed: false,
        supports_schema_constrained_tools: true,
        supports_agent_background_resume: false,
        supports_agent_thread_usage: false,
        supports_agent_result_replay: false,
        supports_infill_completion: false,
        supports_system_fingerprint: false,
        tool_name_max_chars: 64,
    }
}

pub fn anthropic_context_capabilities(
    trusted_official_route: bool,
    native_compaction_enabled: bool,
) -> ProviderContextCapabilities {
    if !trusted_official_route {
        return ProviderContextCapabilities::unknown();
    }
    ProviderContextCapabilities {
        cache_mode: CacheMode::ExplicitBreakpoints,
        explicit_breakpoint_limit: Some(4),
        cache_ttls: vec![
            CacheTtl {
                seconds: 300,
                is_default: true,
            },
            CacheTtl {
                seconds: 3_600,
                is_default: false,
            },
        ],
        cache_usage_fields: CacheUsageCapabilities {
            read_tokens: true,
            write_tokens: true,
            miss_tokens: true,
        },
        stateful_continuation: None,
        native_compaction: native_compaction_enabled.then_some(NativeCompactionCapability {
            requires_exact_route_binding: true,
            supports_portable_fallback: true,
        }),
        native_carrier_portability: if native_compaction_enabled {
            NativeCarrierPortability::ConnectionModelProtocolBound
        } else {
            NativeCarrierPortability::Unavailable
        },
    }
}

#[cfg(test)]
#[path = "tests/capabilities_tests.rs"]
mod tests;
