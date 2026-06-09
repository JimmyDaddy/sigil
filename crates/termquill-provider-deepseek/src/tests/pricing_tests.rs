use termquill_kernel::UsageStats;

use super::{context_window_tokens, enrich_usage_costs};

#[test]
fn context_window_tokens_returns_v4_budget_for_known_models() {
    assert_eq!(context_window_tokens("deepseek-v4-flash"), Some(1_000_000));
    assert_eq!(context_window_tokens("deepseek-v4-pro"), Some(1_000_000));
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
