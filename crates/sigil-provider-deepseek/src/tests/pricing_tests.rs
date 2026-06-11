use sigil_kernel::UsageStats;

use super::{context_window_tokens, enrich_usage_costs};

#[test]
fn context_window_tokens_returns_v4_budget_for_known_models() {
    assert_eq!(context_window_tokens("deepseek-v4-flash"), Some(1_000_000));
    assert_eq!(context_window_tokens("deepseek-v4-pro"), Some(1_000_000));
    assert_eq!(context_window_tokens("deepseek-chat"), Some(1_000_000));
    assert_eq!(context_window_tokens("deepseek-reasoner"), Some(1_000_000));
    assert_eq!(context_window_tokens("custom-model"), None);
}

#[test]
fn enrich_usage_costs_populates_cost_fields_for_flash() {
    let usage = enrich_usage_costs(
        "deepseek-v4-flash",
        UsageStats {
            prompt_tokens: 100,
            completion_tokens: 40,
            cache_hit_tokens: 80,
            cache_miss_tokens: 20,
            input_cost: 0.0,
            output_cost: 0.0,
            cache_savings: 0.0,
            system_fingerprint: None,
        },
    );

    assert!(usage.input_cost > 0.0);
    assert!(usage.output_cost > 0.0);
    assert!(usage.cache_savings > 0.0);
}

#[test]
fn enrich_usage_costs_uses_pro_rates_and_preserves_unknown_models() {
    let pro = enrich_usage_costs(
        "deepseek-v4-pro",
        UsageStats {
            prompt_tokens: 100,
            completion_tokens: 25,
            cache_hit_tokens: 50,
            cache_miss_tokens: 50,
            input_cost: 0.0,
            output_cost: 0.0,
            cache_savings: 0.0,
            system_fingerprint: Some("fp-pro".to_owned()),
        },
    );
    assert!(pro.input_cost > 0.0);
    assert!(pro.output_cost > 0.0);
    assert_eq!(pro.system_fingerprint.as_deref(), Some("fp-pro"));

    let original = UsageStats {
        prompt_tokens: 1,
        completion_tokens: 2,
        cache_hit_tokens: 3,
        cache_miss_tokens: 4,
        input_cost: 7.0,
        output_cost: 8.0,
        cache_savings: 9.0,
        system_fingerprint: None,
    };
    let unchanged = enrich_usage_costs("unknown-model", original);
    assert_eq!(unchanged.prompt_tokens, 1);
    assert_eq!(unchanged.completion_tokens, 2);
    assert_eq!(unchanged.cache_hit_tokens, 3);
    assert_eq!(unchanged.cache_miss_tokens, 4);
    assert_eq!(unchanged.input_cost, 7.0);
    assert_eq!(unchanged.output_cost, 8.0);
    assert_eq!(unchanged.cache_savings, 9.0);
}
