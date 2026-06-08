use termquill_kernel::UsageStats;

const V4_CONTEXT_WINDOW_TOKENS: u32 = 1_000_000;

#[derive(Debug, Clone, Copy)]
struct ModelPricing {
    input_cache_hit_per_million: f64,
    input_cache_miss_per_million: f64,
    output_per_million: f64,
}

pub fn context_window_tokens(model: &str) -> Option<u32> {
    match model {
        "deepseek-v4-flash" | "deepseek-v4-pro" | "deepseek-chat" | "deepseek-reasoner" => {
            Some(V4_CONTEXT_WINDOW_TOKENS)
        }
        _ => None,
    }
}

pub fn enrich_usage_costs(model: &str, usage: UsageStats) -> UsageStats {
    let Some(pricing) = pricing_for(model) else {
        return usage;
    };
    let input_cost = ((usage.cache_hit_tokens as f64 * pricing.input_cache_hit_per_million)
        + (usage.cache_miss_tokens as f64 * pricing.input_cache_miss_per_million))
        / 1_000_000.0;
    let output_cost = (usage.completion_tokens as f64 * pricing.output_per_million) / 1_000_000.0;
    let cache_savings = (usage.cache_hit_tokens as f64
        * (pricing.input_cache_miss_per_million - pricing.input_cache_hit_per_million))
        / 1_000_000.0;

    UsageStats {
        input_cost,
        output_cost,
        cache_savings,
        ..usage
    }
}

fn pricing_for(model: &str) -> Option<ModelPricing> {
    match model {
        "deepseek-v4-flash" | "deepseek-chat" | "deepseek-reasoner" => Some(ModelPricing {
            input_cache_hit_per_million: 0.0028,
            input_cache_miss_per_million: 0.14,
            output_per_million: 0.28,
        }),
        "deepseek-v4-pro" => Some(ModelPricing {
            input_cache_hit_per_million: 0.003625,
            input_cache_miss_per_million: 0.435,
            output_per_million: 0.87,
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
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
}
