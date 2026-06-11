use sigil_kernel::ProviderCapabilities;

pub fn deepseek_capabilities() -> ProviderCapabilities {
    ProviderCapabilities {
        exact_prefix_cache: true,
        reports_cache_tokens: true,
        supports_reasoning_stream: true,
        supports_tool_stream: true,
        supports_background_tasks: false,
        supports_response_handles: false,
        supports_reasoning_artifacts: false,
        supports_structured_output: true,
        supports_assistant_prefix_seed: true,
        supports_schema_constrained_tools: true,
        supports_infill_completion: true,
        supports_system_fingerprint: true,
        tool_name_max_chars: 64,
    }
}
