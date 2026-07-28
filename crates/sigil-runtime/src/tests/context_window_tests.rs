use anyhow::Result;
use sigil_kernel::{
    CompactionConfig, CompactionStrategy, JsonlSessionStore, ModelMessage, Session,
};

use super::{
    ContextWindowSource, compaction_preview_for_strategy, effective_compaction_config,
    resolve_context_window_tokens,
};

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
        strategy: Default::default(),
        enabled: true,
        native_carrier_enabled: false,
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

#[test]
fn cache_aware_strategy_uses_adaptive_whole_turn_preview_in_production_helper() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("adaptive-preview.jsonl"))?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(store);
    for index in 0..4 {
        session.append_user_message(ModelMessage::user(format!(
            "user turn {index}: {}",
            "u".repeat(20_000)
        )))?;
        session.append_assistant_message(ModelMessage::assistant(
            Some(format!("assistant turn {index}: {}", "a".repeat(20_000))),
            Vec::new(),
        ))?;
    }
    let effective = effective_compaction_config(
        session.provider_name(),
        session.model_name(),
        &CompactionConfig::default(),
    );
    let preview = compaction_preview_for_strategy(&session, &effective)?
        .expect("older whole turns are foldable");
    assert!(preview.plan.adaptive_tail.is_some());

    let legacy = CompactionConfig {
        strategy: CompactionStrategy::LegacyV2,
        ..effective
    };
    let legacy_preview =
        compaction_preview_for_strategy(&session, &legacy)?.expect("legacy history is foldable");
    assert!(legacy_preview.plan.adaptive_tail.is_none());
    Ok(())
}
