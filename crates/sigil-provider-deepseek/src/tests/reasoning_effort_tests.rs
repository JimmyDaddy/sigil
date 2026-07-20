use sigil_kernel::ReasoningEffort;

use crate::deepseek_reasoning_efforts;

#[test]
fn known_deepseek_models_expose_the_exact_four_value_contract() {
    assert_eq!(
        deepseek_reasoning_efforts("deepseek-v4-flash"),
        vec![
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
            ReasoningEffort::Max,
        ]
    );
    assert_eq!(
        deepseek_reasoning_efforts("deepseek-v4-pro"),
        deepseek_reasoning_efforts("deepseek-v4-flash")
    );
}

#[test]
fn custom_deepseek_models_fail_closed() {
    assert!(deepseek_reasoning_efforts("deepseek-v4-custom").is_empty());
}
