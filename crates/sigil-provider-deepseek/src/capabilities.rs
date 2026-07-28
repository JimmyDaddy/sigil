use sigil_kernel::{
    CacheMode, CacheUsageCapabilities, NativeCarrierPortability, ProviderCapabilities,
    ProviderContextCapabilities, ReasoningStreamSupport,
};

pub fn deepseek_capabilities() -> ProviderCapabilities {
    ProviderCapabilities {
        exact_prefix_cache: true,
        reports_cache_tokens: true,
        reasoning_stream: ReasoningStreamSupport::Native,
        supports_reasoning_effort: true,
        supports_tool_stream: true,
        supports_background_tasks: false,
        supports_response_handles: false,
        supports_reasoning_artifacts: false,
        supports_structured_output: true,
        supports_assistant_prefix_seed: true,
        supports_schema_constrained_tools: true,
        supports_agent_background_resume: false,
        supports_agent_thread_usage: false,
        supports_agent_result_replay: false,
        supports_infill_completion: true,
        supports_system_fingerprint: true,
        tool_name_max_chars: 64,
    }
}

pub fn deepseek_context_capabilities(trusted_official_route: bool) -> ProviderContextCapabilities {
    if !trusted_official_route {
        return ProviderContextCapabilities::observed_implicit_or_none(CacheUsageCapabilities {
            read_tokens: true,
            write_tokens: false,
            miss_tokens: true,
        });
    }
    ProviderContextCapabilities {
        cache_mode: CacheMode::ImplicitPrefix,
        explicit_breakpoint_limit: None,
        cache_ttls: Vec::new(),
        cache_usage_fields: CacheUsageCapabilities {
            read_tokens: true,
            write_tokens: false,
            miss_tokens: true,
        },
        stateful_continuation: None,
        native_compaction: None,
        native_carrier_portability: NativeCarrierPortability::Unavailable,
    }
}

#[cfg(test)]
#[path = "tests/capabilities_tests.rs"]
mod tests;
