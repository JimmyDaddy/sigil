use anyhow::{Context, Result};
use sigil_kernel::{
    AdaptiveTailPolicyV3, CompactionConfig, ModelRef, Provider, RootConfig, Session,
    V2CompactionPreview,
};
use sigil_provider_deepseek::deepseek_context_window_tokens;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextWindowSource {
    Connection,
    Provider,
    Config,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedContextWindow {
    pub tokens: Option<u32>,
    pub source: ContextWindowSource,
}

/// Tokens kept available for the request envelope and provider-side framing when an explicit
/// output cap is configured. A request with `input + max_output == context` is not actually
/// sendable: the provider still needs room for message framing, tool metadata, and tokenizer
/// rounding. Keep this shared by every setup/runtime adapter so validation cannot drift.
pub const REQUEST_INPUT_SAFETY_BUFFER_TOKENS: u32 = 8_192;

/// Validates an explicit output cap against the resolved model context window.
///
/// An unknown context remains valid here because custom providers may only expose it at request
/// time. Once a context is known, the output cap must leave a non-empty input budget plus the
/// shared safety buffer. This catches configurations that would otherwise save successfully and
/// fail on the first prompt.
pub fn validate_output_token_budget(
    context_window_tokens: Option<u32>,
    max_output_tokens: Option<u32>,
) -> Result<()> {
    let (Some(context_window_tokens), Some(max_output_tokens)) =
        (context_window_tokens, max_output_tokens)
    else {
        return Ok(());
    };
    let required_context = max_output_tokens
        .checked_add(REQUEST_INPUT_SAFETY_BUFFER_TOKENS)
        .context("output token reservation overflowed")?;
    let minimum_context = required_context
        .checked_add(1)
        .context("context window minimum overflowed")?;
    if required_context >= context_window_tokens {
        anyhow::bail!(
            "max output tokens ({max_output_tokens}) leave insufficient input budget in the effective context window ({context_window_tokens}); configure at least {minimum_context} context tokens"
        );
    }
    Ok(())
}

/// Rejects a configured output cap that exceeds the exact provider model's known hard limit.
/// Unknown limits remain allowed so compatible/custom providers are not incorrectly constrained.
pub fn validate_provider_output_token_budget(
    provider: &dyn Provider,
    model_name: &str,
    max_output_tokens: Option<u32>,
) -> Result<()> {
    let (Some(max_output_tokens), Some(provider_limit)) = (
        max_output_tokens,
        provider.maximum_output_tokens(model_name),
    ) else {
        return Ok(());
    };
    anyhow::ensure!(
        max_output_tokens <= provider_limit,
        "max output tokens ({max_output_tokens}) exceed the provider limit ({provider_limit}) for model {model_name}"
    );
    Ok(())
}

/// Chooses an automatic provider default that still leaves the shared request safety buffer.
/// Explicit user values are validated separately; this helper is for the blank/automatic field so
/// a provider default cannot make a small configured context fail on the first request.
pub fn resolve_automatic_output_token_budget(
    context_window_tokens: Option<u32>,
    provider_default_tokens: Option<u32>,
) -> Result<Option<u32>> {
    let Some(provider_default_tokens) = provider_default_tokens else {
        return Ok(None);
    };
    let Some(context_window_tokens) = context_window_tokens else {
        return Ok(Some(provider_default_tokens));
    };
    let safe_output_limit = context_window_tokens
        .checked_sub(REQUEST_INPUT_SAFETY_BUFFER_TOKENS)
        .context("effective context window leaves no input budget")?;
    anyhow::ensure!(
        safe_output_limit > 0,
        "effective context window ({context_window_tokens}) leaves no input budget after the request safety buffer"
    );
    Ok(Some(provider_default_tokens.min(safe_output_limit)))
}

#[must_use]
pub fn resolve_context_window_tokens(
    provider_name: &str,
    model_name: &str,
    configured_tokens: Option<u32>,
) -> ResolvedContextWindow {
    resolve_context_window_tokens_with_override(provider_name, model_name, None, configured_tokens)
}

#[must_use]
pub fn resolve_context_window_tokens_with_override(
    provider_name: &str,
    model_name: &str,
    model_configured_tokens: Option<u32>,
    fallback_tokens: Option<u32>,
) -> ResolvedContextWindow {
    if let Some(tokens) = model_configured_tokens {
        return ResolvedContextWindow {
            tokens: Some(tokens),
            source: ContextWindowSource::Connection,
        };
    }

    if let Some(tokens) = provider_context_window_tokens(provider_name, model_name) {
        return ResolvedContextWindow {
            tokens: Some(tokens),
            source: ContextWindowSource::Provider,
        };
    }

    if let Some(tokens) = fallback_tokens {
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

#[must_use]
pub fn effective_compaction_config_with_override(
    provider_name: &str,
    model_name: &str,
    model_configured_tokens: Option<u32>,
    base: &CompactionConfig,
) -> CompactionConfig {
    let mut effective = base.clone();
    effective.context_window_tokens = resolve_context_window_tokens_with_override(
        provider_name,
        model_name,
        model_configured_tokens,
        base.context_window_tokens,
    )
    .tokens;
    effective
}

#[must_use]
pub fn configured_model_context_window_tokens(
    root_config: &RootConfig,
    model_ref: &ModelRef,
) -> Option<u32> {
    crate::provider_connections::load_provider_connections(root_config)
        .connections
        .get(&model_ref.connection_id)
        .and_then(|connection| {
            connection
                .config
                .model_context_windows
                .get(&model_ref.model_id)
                .copied()
        })
}

#[must_use]
pub fn configured_runtime_model_context_window_tokens(
    root_config: &RootConfig,
    model_name: &str,
) -> Option<u32> {
    let connection_id = root_config.agent.connection.clone()?;
    let model_ref = ModelRef::new(connection_id, model_name.to_owned()).ok()?;
    configured_model_context_window_tokens(root_config, &model_ref)
}

#[must_use]
pub fn resolve_model_context_window_tokens(
    root_config: &RootConfig,
    model_ref: &ModelRef,
    provider_name: &str,
) -> ResolvedContextWindow {
    resolve_context_window_tokens_with_override(
        provider_name,
        &model_ref.model_id,
        configured_model_context_window_tokens(root_config, model_ref),
        root_config.compaction.context_window_tokens,
    )
}

#[must_use]
pub fn effective_compaction_config_for_model_ref(
    root_config: &RootConfig,
    model_ref: &ModelRef,
    provider_name: &str,
) -> CompactionConfig {
    effective_compaction_config_with_override(
        provider_name,
        &model_ref.model_id,
        configured_model_context_window_tokens(root_config, model_ref),
        &root_config.compaction,
    )
}

#[must_use]
pub fn effective_compaction_config_for_runtime_model(
    root_config: &RootConfig,
    provider_name: &str,
    model_name: &str,
) -> CompactionConfig {
    effective_compaction_config_with_override(
        provider_name,
        model_name,
        configured_runtime_model_context_window_tokens(root_config, model_name),
        &root_config.compaction,
    )
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
        .and_then(|tokens| tokens.checked_sub(u64::from(REQUEST_INPUT_SAFETY_BUFFER_TOKENS)))
        .filter(|tokens| *tokens > 0)
        .context("adaptive compaction reservations exhaust the context window")?;
    session.adaptive_compaction_preview(AdaptiveTailPolicyV3::default(), exact_fit_limit_tokens)
}

pub fn provider_context_window_tokens(provider_name: &str, model_name: &str) -> Option<u32> {
    match crate::provider_config_key(provider_name) {
        "deepseek" => deepseek_context_window_tokens(model_name),
        _ => None,
    }
}

#[cfg(test)]
#[path = "tests/context_window_tests.rs"]
mod tests;
