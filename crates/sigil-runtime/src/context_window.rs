use anyhow::{Context, Result};
use sigil_kernel::{AdaptiveTailPolicyV3, CompactionConfig, Session, V2CompactionPreview};
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

/// Builds the current complete-turn adaptive fold preview when the route has an exact-fit budget.
pub fn compaction_preview_for_strategy(
    session: &Session,
    effective: &CompactionConfig,
) -> Result<Option<V2CompactionPreview>> {
    let Some(target_output) = crate::portable_compaction_target_output_tokens(
        session.provider_name(),
        session.model_name(),
    ) else {
        return Ok(None);
    };
    let Some(context_window) = effective.context_window_tokens else {
        return Ok(None);
    };
    let exact_fit_limit_tokens = u64::from(context_window)
        .checked_sub(u64::from(target_output))
        .and_then(|tokens| tokens.checked_sub(8_192))
        .filter(|tokens| *tokens > 0)
        .context("adaptive compaction reservations exhaust the context window")?;
    session.adaptive_compaction_preview(AdaptiveTailPolicyV3::default(), exact_fit_limit_tokens)
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
