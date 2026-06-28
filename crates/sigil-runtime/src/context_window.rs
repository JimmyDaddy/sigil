use sigil_kernel::CompactionConfig;
use sigil_provider_deepseek::deepseek_context_window_tokens;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextWindowSource {
    Provider,
    Config,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedContextWindow {
    pub tokens: Option<u32>,
    pub source: ContextWindowSource,
}

#[must_use]
pub fn resolve_context_window_tokens(
    provider_name: &str,
    model_name: &str,
    configured_tokens: Option<u32>,
) -> ResolvedContextWindow {
    if let Some(tokens) = provider_context_window_tokens(provider_name, model_name) {
        return ResolvedContextWindow {
            tokens: Some(tokens),
            source: ContextWindowSource::Provider,
        };
    }

    if let Some(tokens) = configured_tokens {
        return ResolvedContextWindow {
            tokens: Some(tokens),
            source: ContextWindowSource::Config,
        };
    }

    ResolvedContextWindow {
        tokens: None,
        source: ContextWindowSource::None,
    }
}

#[must_use]
pub fn effective_compaction_config(
    provider_name: &str,
    model_name: &str,
    base: &CompactionConfig,
) -> CompactionConfig {
    let mut effective = base.clone();
    effective.context_window_tokens =
        resolve_context_window_tokens(provider_name, model_name, base.context_window_tokens).tokens;
    effective
}

fn provider_context_window_tokens(provider_name: &str, model_name: &str) -> Option<u32> {
    match crate::provider_config_key(provider_name) {
        "deepseek" => deepseek_context_window_tokens(model_name),
        _ => None,
    }
}

#[cfg(test)]
#[path = "tests/context_window_tests.rs"]
mod tests;
