use sigil_kernel::ProviderContinuationState;

pub const REASONING_REPLAY_STATE_KIND: &str = "deepseek.reasoning_replay";

#[derive(Debug, Clone)]
pub struct DeepSeekReasoningReplayPayload {
    pub reasoning_content: String,
}

impl DeepSeekReasoningReplayPayload {
    pub fn into_state(self) -> ProviderContinuationState {
        ProviderContinuationState {
            provider_name: "deepseek".to_owned(),
            state_kind: REASONING_REPLAY_STATE_KIND.to_owned(),
            message_id: None,
            opaque_blob: serde_json::json!({
                "reasoning_content": self.reasoning_content,
            }),
        }
    }
}

#[cfg(test)]
#[path = "tests/reasoning_tests.rs"]
mod tests;
