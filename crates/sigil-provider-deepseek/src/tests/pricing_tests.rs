use sigil_kernel::{CacheTokenCountV1, CacheUsageV1, UsageStats};

use super::{context_window_tokens, enrich_usage_costs};

fn reported_usage(
    prompt_tokens: u64,
    completion_tokens: u64,
    cache_hit_tokens: u64,
    cache_miss_tokens: u64,
) -> UsageStats {
    UsageStats {
        prompt_tokens,
        completion_tokens,
        cache_hit_tokens,
        cache_miss_tokens,
        input_cost: 0.0,
        output_cost: 0.0,
        cache_savings: 0.0,
        system_fingerprint: None,
        cache_usage: Some(CacheUsageV1 {
            schema_version: CacheUsageV1::SCHEMA_VERSION,
            read: Some(CacheTokenCountV1::provider_reported(cache_hit_tokens)),
            write: None,
            uncached: Some(CacheTokenCountV1::provider_reported(cache_miss_tokens)),
            local_layout_mutation: None,
            provider_miss_without_local_mutation: false,
        }),
        pricing_snapshot: None,
    }
}

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
    let usage = enrich_usage_costs("deepseek-v4-flash", reported_usage(100, 40, 80, 20));

    assert!(usage.input_cost > 0.0);
    assert!(usage.output_cost > 0.0);
    assert!(usage.cache_savings > 0.0);
}

#[test]
fn enrich_usage_costs_uses_pro_rates_and_preserves_unknown_models() {
    let pro = enrich_usage_costs(
        "deepseek-v4-pro",
        UsageStats {
            system_fingerprint: Some("fp-pro".to_owned()),
            ..reported_usage(100, 25, 50, 50)
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
        cache_usage: None,
        pricing_snapshot: None,
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

#[test]
fn enrich_usage_costs_rounds_serialized_cost_estimates() {
    let usage = enrich_usage_costs("deepseek-v4-pro", reported_usage(20621, 1015, 7296, 13325));

    assert_eq!(usage.input_cost, 0.005822823);
    assert_eq!(usage.output_cost, 0.00088305);
    assert_eq!(usage.cache_savings, 0.003147312);

    let serialized = serde_json::to_string(&usage).expect("usage should serialize");
    assert!(serialized.contains(r#""input_cost":0.005822823"#));
    assert!(serialized.contains(r#""output_cost":0.00088305"#));
    assert!(serialized.contains(r#""cache_savings":0.003147312"#));
    assert!(!serialized.contains("999999999"));
}
