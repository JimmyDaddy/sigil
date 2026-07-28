use sigil_kernel::{
    CacheMode, CacheUsageCapabilities, NativeCarrierPortability, ProviderCapabilities,
    ProviderContextCapabilities, ReasoningStreamSupport,
};

pub fn gemini_capabilities() -> ProviderCapabilities {
    ProviderCapabilities {
        exact_prefix_cache: false,
        reports_cache_tokens: true,
        reasoning_stream: ReasoningStreamSupport::Unsupported,
        supports_reasoning_effort: false,
        supports_tool_stream: false,
        supports_background_tasks: false,
        supports_response_handles: false,
        supports_reasoning_artifacts: false,
        supports_structured_output: true,
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

pub fn gemini_context_capabilities(trusted_official_route: bool) -> ProviderContextCapabilities {
    if !trusted_official_route {
        return ProviderContextCapabilities::unknown();
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
        // This adapter currently uses GenerateContent. Interactions API stateful continuation
        // must remain fail-closed until the transport round-trips previous_interaction_id.
        stateful_continuation: None,
        native_compaction: None,
        native_carrier_portability: NativeCarrierPortability::Unavailable,
    }
}

#[cfg(test)]
#[path = "tests/capabilities_tests.rs"]
mod tests;
