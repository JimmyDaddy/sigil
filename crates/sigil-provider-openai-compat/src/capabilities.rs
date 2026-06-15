use sigil_kernel::ProviderCapabilities;

pub fn openai_compatible_capabilities() -> ProviderCapabilities {
    ProviderCapabilities {
        exact_prefix_cache: false,
        reports_cache_tokens: true,
        supports_reasoning_stream: true,
        supports_tool_stream: true,
        supports_background_tasks: false,
        supports_response_handles: false,
        supports_reasoning_artifacts: false,
        supports_structured_output: false,
        supports_assistant_prefix_seed: false,
        supports_schema_constrained_tools: false,
        supports_infill_completion: false,
        supports_system_fingerprint: true,
        tool_name_max_chars: 64,
    }
}

#[cfg(test)]
#[path = "tests/capabilities_tests.rs"]
mod tests;
