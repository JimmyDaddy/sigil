use super::{SessionStats, UsageStats};

#[test]
fn session_stats_track_latest_prompt_tokens_separately_from_totals() {
    let mut stats = SessionStats::default();
    stats.apply_usage(&UsageStats {
        prompt_tokens: 120,
        completion_tokens: 10,
        cache_hit_tokens: 80,
        cache_miss_tokens: 40,
        input_cost: 0.0,
        output_cost: 0.0,
        cache_savings: 0.0,
        system_fingerprint: None,
    });
    stats.apply_usage(&UsageStats {
        prompt_tokens: 42,
        completion_tokens: 5,
        cache_hit_tokens: 21,
        cache_miss_tokens: 21,
        input_cost: 0.0,
        output_cost: 0.0,
        cache_savings: 0.0,
        system_fingerprint: None,
    });

    assert_eq!(stats.prompt_tokens, 162);
    assert_eq!(stats.last_prompt_tokens, 42);
}
