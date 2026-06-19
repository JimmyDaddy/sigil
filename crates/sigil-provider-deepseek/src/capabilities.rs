use sigil_kernel::{ProviderCapabilities, ReasoningStreamSupport};

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
