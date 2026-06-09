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
#[path = "tests/pricing_tests.rs"]
mod tests;
