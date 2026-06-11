use super::{DeepSeekReasoningReplayPayload, REASONING_REPLAY_STATE_KIND};

#[test]
fn replay_payload_into_state_preserves_reasoning_content() {
    let state = DeepSeekReasoningReplayPayload {
        reasoning_content: "step by step".to_owned(),
    }
    .into_state();

    assert_eq!(state.provider_name, "deepseek");
    assert_eq!(state.state_kind, REASONING_REPLAY_STATE_KIND);
    assert!(state.message_id.is_none());
    assert_eq!(state.opaque_blob["reasoning_content"], "step by step");
}
