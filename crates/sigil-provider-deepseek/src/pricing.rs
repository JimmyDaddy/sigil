use sigil_kernel::ModelPricingSnapshotV1;
#[cfg(test)]
use sigil_kernel::UsageStats;

const V4_CONTEXT_WINDOW_TOKENS: u32 = 1_000_000;
const DEEPSEEK_PRICING_SOURCE: &str = "https://api-docs.deepseek.com/quick_start/pricing/";
const DEEPSEEK_PRICING_VERIFIED_AT: &str = "2026-07-28";

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

#[cfg(test)]
pub fn enrich_usage_costs(model: &str, usage: UsageStats) -> UsageStats {
    let Some(snapshot) = pricing_snapshot(model) else {
        return usage;
    };
    snapshot
        .apply_to_usage(usage)
        .expect("bundled DeepSeek pricing snapshot must remain valid")
}

pub(crate) fn pricing_snapshot(model: &str) -> Option<ModelPricingSnapshotV1> {
    let (snapshot_id, pricing) = match model {
        "deepseek-v4-flash" | "deepseek-chat" | "deepseek-reasoner" => (
            "deepseek-v4-flash-usd-2026-07-28",
            ModelPricing {
                input_cache_hit_per_million: 0.0028,
                input_cache_miss_per_million: 0.14,
                output_per_million: 0.28,
            },
        ),
        "deepseek-v4-pro" => (
            "deepseek-v4-pro-usd-2026-07-28",
            ModelPricing {
                input_cache_hit_per_million: 0.003625,
                input_cache_miss_per_million: 0.435,
                output_per_million: 0.87,
            },
        ),
        _ => return None,
    };
    Some(ModelPricingSnapshotV1 {
        schema_version: ModelPricingSnapshotV1::SCHEMA_VERSION,
        snapshot_id: snapshot_id.to_owned(),
        currency: "USD".to_owned(),
        unit_tokens: 1_000_000,
        cache_read_per_unit: pricing.input_cache_hit_per_million,
        cache_write_per_unit: None,
        uncached_input_per_unit: pricing.input_cache_miss_per_million,
        output_per_unit: pricing.output_per_million,
        source: DEEPSEEK_PRICING_SOURCE.to_owned(),
        verified_at: DEEPSEEK_PRICING_VERIFIED_AT.to_owned(),
    })
}

#[cfg(test)]
#[path = "tests/pricing_tests.rs"]
mod tests;
