use sigil_kernel::ReasoningEffort;

/// Returns the subset of reasoning-effort values implemented and proven by Sigil for one exact
/// official OpenAI Responses model.
///
/// The kernel does not yet model `none`, `minimal`, or `xhigh`; those values are intentionally not
/// projected. Non-reasoning and unknown model identifiers fail closed.
#[must_use]
pub fn openai_responses_reasoning_efforts(model_name: &str) -> Vec<ReasoningEffort> {
    let model = model_name.trim().to_ascii_lowercase();
    if ["gpt-5-pro"]
        .iter()
        .any(|alias| exact_or_dated_snapshot(&model, alias))
    {
        return vec![ReasoningEffort::High];
    }
    if [
        "gpt-5.6",
        "gpt-5.6-sol",
        "gpt-5.6-terra",
        "gpt-5.6-luna",
        "gpt-5.5",
        "gpt-5.4",
        "gpt-5.4-mini",
        "gpt-5.4-nano",
        "gpt-5.3-codex",
        "gpt-5.2",
        "gpt-5.1",
        "gpt-5.1-codex",
        "gpt-5.1-codex-mini",
        "gpt-5.1-codex-max",
        "gpt-5",
        "gpt-5-mini",
        "gpt-5-nano",
        "gpt-5-codex",
        "o1",
        "o3",
        "o4-mini",
    ]
    .iter()
    .any(|alias| exact_or_dated_snapshot(&model, alias))
    {
        return vec![
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
        ];
    }
    Vec::new()
}

fn exact_or_dated_snapshot(model: &str, alias: &str) -> bool {
    if model == alias {
        return true;
    }
    let Some(date) = model
        .strip_prefix(alias)
        .and_then(|suffix| suffix.strip_prefix('-'))
    else {
        return false;
    };
    date.len() == 10
        && date.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 4 | 7) {
                byte == b'-'
            } else {
                byte.is_ascii_digit()
            }
        })
}
