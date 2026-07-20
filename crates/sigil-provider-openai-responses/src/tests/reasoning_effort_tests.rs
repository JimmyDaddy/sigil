use sigil_kernel::ReasoningEffort;

use crate::openai_responses_reasoning_efforts;

#[test]
fn reasoning_models_expose_only_the_kernel_supported_subset() {
    assert_eq!(
        openai_responses_reasoning_efforts("gpt-5.1"),
        vec![
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
        ]
    );
    assert_eq!(
        openai_responses_reasoning_efforts("o3-2025-04-16"),
        openai_responses_reasoning_efforts("gpt-5.1")
    );
    assert_eq!(
        openai_responses_reasoning_efforts("gpt-5-pro"),
        vec![ReasoningEffort::High]
    );
}

#[test]
fn non_reasoning_and_unknown_models_fail_closed() {
    assert!(openai_responses_reasoning_efforts("gpt-4.1").is_empty());
    assert!(openai_responses_reasoning_efforts("gpt-5-unknown").is_empty());
    assert!(openai_responses_reasoning_efforts("o3-2025-4-16").is_empty());
}
