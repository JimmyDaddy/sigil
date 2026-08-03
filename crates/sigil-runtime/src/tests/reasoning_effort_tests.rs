use sigil_kernel::ReasoningEffort;

use crate::reasoning_effort::{
    admitted_reasoning_effort, reasoning_effort_binding, supported_reasoning_efforts,
};

#[test]
fn exact_provider_model_support_is_projected_without_guessing() {
    assert_eq!(
        supported_reasoning_efforts("deepseek", "deepseek-v4-flash"),
        vec![
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
            ReasoningEffort::Max,
        ]
    );
    assert!(supported_reasoning_efforts("openai_responses", "gpt-4.1").is_empty());
    assert!(supported_reasoning_efforts("openai_compat", "gpt-5").is_empty());
}

#[test]
fn request_effort_is_omitted_for_unknown_or_unsupported_models() {
    assert_eq!(
        admitted_reasoning_effort("deepseek", "deepseek-v4-flash", Some(ReasoningEffort::Max)),
        Some(ReasoningEffort::Max)
    );
    assert_eq!(
        admitted_reasoning_effort("openai_responses", "gpt-4.1", Some(ReasoningEffort::High)),
        None
    );
    assert_eq!(
        admitted_reasoning_effort("openai_compat", "private", Some(ReasoningEffort::Max)),
        None
    );
}

#[test]
fn binding_is_support_and_model_bound() {
    let supported = supported_reasoning_efforts("deepseek", "deepseek-v4-flash");
    let flash = reasoning_effort_binding("deepseek", "deepseek-v4-flash", &supported);
    let pro = reasoning_effort_binding("deepseek", "deepseek-v4-pro", &supported);
    assert!(flash.is_some());
    assert_ne!(flash, pro);
    assert_eq!(reasoning_effort_binding("anthropic", "claude", &[]), None);
}
