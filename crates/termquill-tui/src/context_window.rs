use termquill_kernel::CompactionConfig;
use termquill_provider_deepseek::deepseek_context_window_tokens;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContextWindowSource {
    Provider,
    Config,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ResolvedContextWindow {
    pub(crate) tokens: Option<u32>,
    pub(crate) source: ContextWindowSource,
}

pub(crate) fn resolve_context_window_tokens(
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

pub(crate) fn effective_compaction_config(
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
    match provider_name {
        "deepseek" => deepseek_context_window_tokens(model_name),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use termquill_kernel::CompactionConfig;

    use super::{ContextWindowSource, effective_compaction_config, resolve_context_window_tokens};

    #[test]
    fn provider_window_overrides_compaction_config_window() {
        let resolved = resolve_context_window_tokens("deepseek", "deepseek-v4-pro", Some(128_000));

        assert_eq!(resolved.tokens, Some(1_000_000));
        assert_eq!(resolved.source, ContextWindowSource::Provider);
    }

    #[test]
    fn configured_window_is_used_when_provider_window_is_unknown() {
        let resolved = resolve_context_window_tokens("custom", "custom-model", Some(128_000));

        assert_eq!(resolved.tokens, Some(128_000));
        assert_eq!(resolved.source, ContextWindowSource::Config);
    }

    #[test]
    fn effective_compaction_config_preserves_thresholds_and_tail() {
        let config = CompactionConfig {
            enabled: true,
            soft_threshold_ratio: 0.5,
            hard_threshold_ratio: 0.8,
            context_window_tokens: Some(128_000),
            tail_messages: 6,
        };

        let effective = effective_compaction_config("deepseek", "deepseek-v4-pro", &config);

        assert_eq!(effective.context_window_tokens, Some(1_000_000));
        assert_eq!(effective.soft_threshold_ratio, 0.5);
        assert_eq!(effective.hard_threshold_ratio, 0.8);
        assert_eq!(effective.tail_messages, 6);
    }
}
