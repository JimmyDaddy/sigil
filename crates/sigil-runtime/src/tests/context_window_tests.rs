use anyhow::Result;
use sigil_kernel::{CompactionConfig, JsonlSessionStore, ModelMessage, Session};

use super::{
    ContextWindowSource, compaction_preview_for_strategy, effective_compaction_config,
    effective_compaction_config_with_override, resolve_automatic_output_token_budget,
    resolve_context_window_tokens, resolve_context_window_tokens_with_override,
    validate_output_token_budget,
};

#[test]
fn exact_connection_model_window_overrides_provider_metadata() {
    let resolved = resolve_context_window_tokens_with_override(
        "deepseek",
        "deepseek-v4-pro",
        Some(256_000),
        Some(128_000),
    );

    assert_eq!(resolved.tokens, Some(256_000));
    assert_eq!(resolved.source, ContextWindowSource::Connection);

    let effective = effective_compaction_config_with_override(
        "deepseek",
        "deepseek-v4-pro",
        Some(256_000),
        &CompactionConfig::default(),
    );
    assert_eq!(effective.context_window_tokens, Some(256_000));
}

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
fn output_budget_preserves_input_capacity_without_estimated_envelope_admission() {
    assert!(validate_output_token_budget(Some(1_000_000), Some(256_000)).is_ok());
    assert!(validate_output_token_budget(Some(256_000), Some(256_000)).is_err());
    assert!(validate_output_token_budget(Some(64_000), Some(60_000)).is_ok());
    assert!(validate_output_token_budget(None, Some(256_000)).is_ok());
    assert!(validate_output_token_budget(None, Some(0)).is_err());
    for context in [4_096, 8_192] {
        assert!(validate_output_token_budget(Some(context), Some(512)).is_ok());
        assert!(validate_output_token_budget(Some(context), Some(context)).is_err());
        assert!(validate_output_token_budget(Some(context), Some(context + 1)).is_err());
        assert!(validate_output_token_budget(Some(context), Some(0)).is_err());
    }
    assert!(validate_output_token_budget(Some(u32::MAX), Some(u32::MAX - 1)).is_ok());
}

#[test]
fn automatic_provider_default_is_capped_for_small_configured_windows() -> Result<()> {
    assert_eq!(
        resolve_automatic_output_token_budget(Some(256_000), Some(256_000))?,
        Some(247_808)
    );
    assert_eq!(
        resolve_automatic_output_token_budget(Some(1_000_000), Some(256_000))?,
        Some(256_000)
    );
    for context in [4_096, 8_192] {
        assert_eq!(
            resolve_automatic_output_token_budget(Some(context), Some(4_096))?,
            Some(context / 2)
        );
        assert_eq!(
            resolve_automatic_output_token_budget(Some(context), Some(512))?,
            Some(512)
        );
        assert_eq!(
            resolve_automatic_output_token_budget(Some(context), None)?,
            None
        );
    }
    assert!(resolve_automatic_output_token_budget(Some(1), Some(1)).is_err());
    Ok(())
}

#[test]
fn effective_compaction_config_preserves_current_strategy_settings() {
    let config = CompactionConfig {
        strategy: Default::default(),
        enabled: true,
        native_carrier_enabled: false,
        context_window_tokens: Some(128_000),
    };

    let effective = effective_compaction_config("deepseek", "deepseek-v4-pro", &config);

    assert_eq!(effective.context_window_tokens, Some(1_000_000));
    assert!(effective.enabled);
    assert!(!effective.native_carrier_enabled);
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
    assert!(preview.plan.adaptive_tail.folded_complete_turns > 0);
    assert!(preview.plan.adaptive_tail.retained_complete_turns >= 2);
    Ok(())
}
