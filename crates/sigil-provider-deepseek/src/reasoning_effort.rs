use sigil_kernel::ReasoningEffort;

/// Returns the reasoning-effort values proven for one exact DeepSeek model.
///
/// Unknown/custom model identifiers remain unsupported until their wire contract is reviewed.
#[must_use]
pub fn deepseek_reasoning_efforts(model_name: &str) -> Vec<ReasoningEffort> {
    match model_name.trim().to_ascii_lowercase().as_str() {
        "deepseek-v4-flash" | "deepseek-v4-pro" | "deepseek-chat" | "deepseek-reasoner" => {
            vec![
                ReasoningEffort::Low,
                ReasoningEffort::Medium,
                ReasoningEffort::High,
                ReasoningEffort::Max,
            ]
        }
        _ => Vec::new(),
    }
}
