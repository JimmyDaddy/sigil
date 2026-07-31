use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::ModelPricingSnapshotV1;

/// Schema version for forecast, cache-aware cost and admission evidence introduced by RFC-0057.
pub const COMPACTION_ECONOMICS_V2_SCHEMA_VERSION: u16 = 2;
pub const DEFAULT_COMPACTION_ECONOMICS_HORIZON_TURNS: u32 = 3;
pub const DEFAULT_COMPACTION_MIN_SAVINGS_RATIO_PPM: u32 = 50_000;
pub const DEFAULT_COMPACTION_MIN_SAVINGS_TOKENS_EQUIVALENT: u64 = 4_096;
pub const DEFAULT_COMPACTION_MAX_BREAK_EVEN_TURNS: u32 = 3;
pub const DEFAULT_COMPACTION_OBSERVE_RATIO_PPM: u32 = 500_000;
pub const DEFAULT_COMPACTION_PREPARE_RATIO_PPM: u32 = 700_000;
pub const DEFAULT_COMPACTION_EMERGENCY_RATIO_PPM: u32 = 900_000;
const NANO_USD_PER_USD: f64 = 1_000_000_000.0;

/// Confidence of a bounded remaining-turn forecast.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum CompactionForecastConfidenceV1 {
    Low,
    Medium,
    High,
}

/// Durable source category for the expected remaining-turn count.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CompactionForecastSourceV1 {
    AcceptedTaskPlan,
    AcceptedIntentCriteria,
    ActiveToolOrQueuedInput,
    SessionTurnShape,
    ConservativeFallback,
}

/// Bounded forecast of how many real requests remain in the active objective.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ExpectedRemainingTurnsV1 {
    pub turns: u32,
    pub source: CompactionForecastSourceV1,
    pub confidence: CompactionForecastConfidenceV1,
    /// Exact durable event references supporting structured forecasts. Heuristic fallback may be
    /// empty, but never claims structured confidence.
    pub source_event_ids: Vec<crate::EventId>,
}

impl ExpectedRemainingTurnsV1 {
    fn validate(&self) -> Result<()> {
        if self.turns == 0 || self.turns > 1_000 || self.source_event_ids.len() > 128 {
            bail!("expected remaining-turn forecast is out of bounds");
        }
        if self.source == CompactionForecastSourceV1::ConservativeFallback
            && (self.confidence != CompactionForecastConfidenceV1::Low
                || !self.source_event_ids.is_empty())
        {
            bail!("fallback remaining-turn forecast must stay low-confidence and ungrounded");
        }
        if matches!(
            self.source,
            CompactionForecastSourceV1::AcceptedTaskPlan
                | CompactionForecastSourceV1::AcceptedIntentCriteria
                | CompactionForecastSourceV1::ActiveToolOrQueuedInput
        ) && self.source_event_ids.is_empty()
        {
            bail!("structured remaining-turn forecast requires durable source events");
        }
        if self
            .source_event_ids
            .iter()
            .any(|event_id| event_id.trim().is_empty())
        {
            bail!("remaining-turn forecast contains an empty source event");
        }
        Ok(())
    }
}

/// Pressure state produced by a conservative projected-next-input forecast.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum CompactionPressureStateV1 {
    BelowObserve,
    Observe,
    Prepare,
    Admit,
    Emergency,
}

/// Input to the provider-neutral fit forecast.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct CompactionFitForecastInputV1 {
    pub context_window_tokens: u64,
    pub current_input_tokens: u64,
    pub next_turn_p95_tokens: u64,
    pub reserved_output_tokens: u64,
    pub tool_growth_p95_tokens: u64,
    pub provider_state_tokens: u64,
    pub safety_buffer_tokens: u64,
    pub bulky_shrink_candidate_tokens: u64,
    pub overflow_observed: bool,
    pub expected_remaining_turns: ExpectedRemainingTurnsV1,
}

/// Durable, integer-only evidence for fit and trigger-state decisions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct CompactionFitForecastV1 {
    pub schema_version: u16,
    pub input: CompactionFitForecastInputV1,
    pub usable_context_tokens: u64,
    pub projected_next_input_tokens: u64,
    pub pressure_state: CompactionPressureStateV1,
    pub fit_required: bool,
}

impl CompactionFitForecastV1 {
    /// Builds a checked fit forecast without estimating from raw characters or bytes.
    ///
    /// # Errors
    ///
    /// Returns an error when reservations exhaust the context window or any checked sum
    /// overflows.
    pub fn from_input(input: CompactionFitForecastInputV1) -> Result<Self> {
        input.expected_remaining_turns.validate()?;
        if input.context_window_tokens == 0
            || input.current_input_tokens == 0
            || input.next_turn_p95_tokens == 0
        {
            bail!("compaction fit forecast requires non-zero window and token evidence");
        }
        let reserved = input
            .reserved_output_tokens
            .checked_add(input.tool_growth_p95_tokens)
            .and_then(|value| value.checked_add(input.provider_state_tokens))
            .and_then(|value| value.checked_add(input.safety_buffer_tokens))
            .context("compaction fit reservation overflowed")?;
        let usable_context_tokens = input
            .context_window_tokens
            .checked_sub(reserved)
            .filter(|usable| *usable > 0)
            .context("compaction fit reservations exhaust the context window")?;
        let projected_next_input_tokens = input
            .current_input_tokens
            .checked_add(input.next_turn_p95_tokens)
            .context("compaction projected next input overflowed")?;
        let fit_required = projected_next_input_tokens > usable_context_tokens;
        let pressure_state = if input.overflow_observed
            || ratio_at_least(
                input.current_input_tokens,
                input.context_window_tokens,
                DEFAULT_COMPACTION_EMERGENCY_RATIO_PPM,
            )? {
            CompactionPressureStateV1::Emergency
        } else if fit_required {
            CompactionPressureStateV1::Admit
        } else if ratio_at_least(
            projected_next_input_tokens,
            usable_context_tokens,
            DEFAULT_COMPACTION_PREPARE_RATIO_PPM,
        )? || input.bulky_shrink_candidate_tokens
            >= DEFAULT_COMPACTION_MIN_SAVINGS_TOKENS_EQUIVALENT
        {
            CompactionPressureStateV1::Prepare
        } else if ratio_at_least(
            input.current_input_tokens,
            input.context_window_tokens,
            DEFAULT_COMPACTION_OBSERVE_RATIO_PPM,
        )? {
            CompactionPressureStateV1::Observe
        } else {
            CompactionPressureStateV1::BelowObserve
        };
        Ok(Self {
            schema_version: COMPACTION_ECONOMICS_V2_SCHEMA_VERSION,
            input,
            usable_context_tokens,
            projected_next_input_tokens,
            pressure_state,
            fit_required,
        })
    }

    pub(crate) fn validate(&self) -> Result<()> {
        if self.schema_version != COMPACTION_ECONOMICS_V2_SCHEMA_VERSION {
            bail!("unsupported compaction fit forecast schema version");
        }
        let rebuilt = Self::from_input(self.input.clone())?;
        if &rebuilt != self {
            bail!("compaction fit forecast does not match its input evidence");
        }
        Ok(())
    }
}

/// Integer price evidence derived from a trusted provider/model snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct TrustedCompactionPricingV1 {
    pub schema_version: u16,
    pub snapshot_id: String,
    pub unit_tokens: u64,
    pub cache_read_nano_usd_per_unit: u64,
    pub cache_write_nano_usd_per_unit: Option<u64>,
    pub uncached_input_nano_usd_per_unit: u64,
    pub output_nano_usd_per_unit: u64,
    pub source: String,
    pub verified_at: String,
}

impl TrustedCompactionPricingV1 {
    /// Converts trusted floating-point provider evidence once into replay-stable nano-USD units.
    ///
    /// # Errors
    ///
    /// Returns an error when the source snapshot is invalid or a price cannot be represented.
    pub fn from_model_snapshot(snapshot: &ModelPricingSnapshotV1) -> Result<Self> {
        snapshot.validate()?;
        let pricing = Self {
            schema_version: COMPACTION_ECONOMICS_V2_SCHEMA_VERSION,
            snapshot_id: snapshot.snapshot_id.clone(),
            unit_tokens: snapshot.unit_tokens,
            cache_read_nano_usd_per_unit: price_to_nano_usd(snapshot.cache_read_per_unit)?,
            cache_write_nano_usd_per_unit: snapshot
                .cache_write_per_unit
                .map(price_to_nano_usd)
                .transpose()?,
            uncached_input_nano_usd_per_unit: price_to_nano_usd(snapshot.uncached_input_per_unit)?,
            output_nano_usd_per_unit: price_to_nano_usd(snapshot.output_per_unit)?,
            source: snapshot.source.clone(),
            verified_at: snapshot.verified_at.clone(),
        };
        pricing.validate()?;
        Ok(pricing)
    }

    fn validate(&self) -> Result<()> {
        if self.schema_version != COMPACTION_ECONOMICS_V2_SCHEMA_VERSION
            || self.snapshot_id.trim().is_empty()
            || self.unit_tokens == 0
            || self.source.trim().is_empty()
            || self.verified_at.trim().is_empty()
            || self.uncached_input_nano_usd_per_unit == 0
            || self.output_nano_usd_per_unit == 0
        {
            bail!("trusted compaction pricing evidence is invalid");
        }
        Ok(())
    }

    fn new_input_price(&self) -> u64 {
        self.cache_write_nano_usd_per_unit
            .unwrap_or(self.uncached_input_nano_usd_per_unit)
    }
}

/// Initial RFC-0057 cost/admission policy.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct CompactionEconomicsPolicyV1 {
    pub horizon_turns: u32,
    pub minimum_savings_ratio_ppm: u32,
    pub minimum_savings_tokens_equivalent: u64,
    pub max_break_even_turns: u32,
}

impl Default for CompactionEconomicsPolicyV1 {
    fn default() -> Self {
        Self {
            horizon_turns: DEFAULT_COMPACTION_ECONOMICS_HORIZON_TURNS,
            minimum_savings_ratio_ppm: DEFAULT_COMPACTION_MIN_SAVINGS_RATIO_PPM,
            minimum_savings_tokens_equivalent: DEFAULT_COMPACTION_MIN_SAVINGS_TOKENS_EQUIVALENT,
            max_break_even_turns: DEFAULT_COMPACTION_MAX_BREAK_EVEN_TURNS,
        }
    }
}

impl CompactionEconomicsPolicyV1 {
    fn validate(&self) -> Result<()> {
        if self.horizon_turns > 64
            || self.minimum_savings_ratio_ppm > 1_000_000
            || self.minimum_savings_tokens_equivalent == 0
            || self.max_break_even_turns > self.horizon_turns
        {
            bail!("compaction economics policy is invalid");
        }
        Ok(())
    }
}

/// Expected future cache survival used by cost simulations and trusted admissions.
///
/// TTLs are expressed as future real-request turns because wall-clock expiry cannot be predicted
/// without a request-interval forecast. `None` means no expiry is assumed inside the bounded
/// horizon; `Some(0)` means the cache is already expired.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct CompactionCacheScenarioV1 {
    pub current_epoch_hit_ratio_ppm: u32,
    pub rotated_epoch_hit_ratio_ppm: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_epoch_ttl_turns: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotated_epoch_ttl_turns: Option<u32>,
}

impl CompactionCacheScenarioV1 {
    fn validate(&self) -> Result<()> {
        if self.current_epoch_hit_ratio_ppm > 1_000_000
            || self.rotated_epoch_hit_ratio_ppm > 1_000_000
            || self
                .current_epoch_ttl_turns
                .is_some_and(|turns| turns > 1_000)
            || self
                .rotated_epoch_ttl_turns
                .is_some_and(|turns| turns > 1_000)
        {
            bail!("compaction cache scenario is invalid");
        }
        Ok(())
    }

    fn current_hit_ratio(&self, future_turn: u32) -> u32 {
        if self
            .current_epoch_ttl_turns
            .is_some_and(|ttl| future_turn > ttl)
        {
            0
        } else {
            self.current_epoch_hit_ratio_ppm
        }
    }

    fn rotated_hit_ratio(&self, turns_after_epoch_write: u32) -> u32 {
        if self
            .rotated_epoch_ttl_turns
            .is_some_and(|ttl| turns_after_epoch_write > ttl)
        {
            0
        } else {
            self.rotated_epoch_hit_ratio_ppm
        }
    }
}

/// Token-shape inputs for comparing keep-current-epoch and rotate-now costs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct CompactionCostModelInputV1 {
    pub current_cache_read_tokens: u64,
    pub current_uncached_input_tokens: u64,
    pub post_rotation_input_tokens: u64,
    pub next_turn_p95_tokens: u64,
    pub compactor_cache_read_tokens: u64,
    pub compactor_uncached_input_tokens: u64,
    pub compactor_output_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_scenario: Option<CompactionCacheScenarioV1>,
}

/// Replay-stable, cache-aware cost projection over a bounded number of real requests.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct CompactionCostProjectionV1 {
    pub schema_version: u16,
    pub pricing: TrustedCompactionPricingV1,
    pub input: CompactionCostModelInputV1,
    pub horizon_turns: u32,
    pub minimum_savings_nano_usd: u64,
    pub keep_cost_nano_usd: u64,
    pub rotate_compactor_cost_nano_usd: u64,
    pub rotate_first_epoch_cost_nano_usd: u64,
    pub rotate_followup_cost_nano_usd: u64,
    pub rotate_cost_nano_usd: u64,
    pub savings_nano_usd: u64,
    pub savings_ratio_ppm: u32,
    pub break_even_turns: Option<u32>,
    pub qualifies: bool,
}

impl CompactionCostProjectionV1 {
    /// Computes a bounded keep/rotate comparison using explicit read/write/miss/output prices.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid pricing/policy or checked arithmetic overflow.
    pub fn project(
        pricing: TrustedCompactionPricingV1,
        input: CompactionCostModelInputV1,
        policy: &CompactionEconomicsPolicyV1,
    ) -> Result<Self> {
        pricing.validate()?;
        policy.validate()?;
        if input.post_rotation_input_tokens == 0 || input.next_turn_p95_tokens == 0 {
            bail!("compaction cost model requires non-zero rotated input and turn growth");
        }
        if let Some(scenario) = &input.cache_scenario {
            scenario.validate()?;
        }
        let current_input_tokens = input
            .current_cache_read_tokens
            .checked_add(input.current_uncached_input_tokens)
            .context("compaction current input token total overflowed")?;
        if current_input_tokens == 0 {
            bail!("compaction cost model current input is empty");
        }
        let minimum_savings_nano_usd = charge(
            policy.minimum_savings_tokens_equivalent,
            pricing.uncached_input_nano_usd_per_unit,
            pricing.unit_tokens,
        )?;
        let compactor_cache_read_cost = charge(
            input.compactor_cache_read_tokens,
            pricing.cache_read_nano_usd_per_unit,
            pricing.unit_tokens,
        )?;
        let compactor_uncached_input_cost = charge(
            input.compactor_uncached_input_tokens,
            pricing.uncached_input_nano_usd_per_unit,
            pricing.unit_tokens,
        )?;
        let compactor_output_cost = charge(
            input.compactor_output_tokens,
            pricing.output_nano_usd_per_unit,
            pricing.unit_tokens,
        )?;
        let rotate_compactor_cost_nano_usd = compactor_cache_read_cost
            .checked_add(compactor_uncached_input_cost)
            .and_then(|value| value.checked_add(compactor_output_cost))
            .context("compactor cost overflowed")?;

        let mut keep_cost_nano_usd = 0_u64;
        let mut rotate_first_epoch_cost_nano_usd = 0_u64;
        let mut rotate_followup_cost_nano_usd = 0_u64;
        let mut break_even_turns = None;
        for turn in 1..=policy.horizon_turns {
            let growth_before_turn = input
                .next_turn_p95_tokens
                .checked_mul(u64::from(turn.saturating_sub(1)))
                .context("keep-cost turn growth overflowed")?;
            let keep_read_tokens = current_input_tokens
                .checked_add(growth_before_turn)
                .context("keep-cost cached prefix overflowed")?;
            let keep_read_cost = cache_blended_charge(
                keep_read_tokens,
                input
                    .cache_scenario
                    .as_ref()
                    .map_or(1_000_000, |scenario| scenario.current_hit_ratio(turn)),
                &pricing,
            )?;
            let keep_growth_cost = charge(
                input.next_turn_p95_tokens,
                pricing.new_input_price(),
                pricing.unit_tokens,
            )?;
            keep_cost_nano_usd = keep_cost_nano_usd
                .checked_add(keep_read_cost)
                .and_then(|value| value.checked_add(keep_growth_cost))
                .context("keep horizon cost overflowed")?;

            if turn == 1 {
                let first_epoch_tokens = input
                    .post_rotation_input_tokens
                    .checked_add(input.next_turn_p95_tokens)
                    .context("first rotated epoch input overflowed")?;
                rotate_first_epoch_cost_nano_usd = charge(
                    first_epoch_tokens,
                    pricing.new_input_price(),
                    pricing.unit_tokens,
                )?;
            } else {
                let rotate_read_tokens = input
                    .post_rotation_input_tokens
                    .checked_add(
                        input
                            .next_turn_p95_tokens
                            .checked_mul(u64::from(turn - 1))
                            .context("rotated follow-up growth overflowed")?,
                    )
                    .context("rotated follow-up prefix overflowed")?;
                let rotate_read_cost = cache_blended_charge(
                    rotate_read_tokens,
                    input
                        .cache_scenario
                        .as_ref()
                        .map_or(1_000_000, |scenario| scenario.rotated_hit_ratio(turn - 1)),
                    &pricing,
                )?;
                let rotate_growth_cost = charge(
                    input.next_turn_p95_tokens,
                    pricing.new_input_price(),
                    pricing.unit_tokens,
                )?;
                rotate_followup_cost_nano_usd = rotate_followup_cost_nano_usd
                    .checked_add(rotate_read_cost)
                    .and_then(|value| value.checked_add(rotate_growth_cost))
                    .context("rotated follow-up cost overflowed")?;
            }
            let rotate_so_far = rotate_compactor_cost_nano_usd
                .checked_add(rotate_first_epoch_cost_nano_usd)
                .and_then(|value| value.checked_add(rotate_followup_cost_nano_usd))
                .context("rotate cumulative cost overflowed")?;
            if break_even_turns.is_none()
                && savings_meets_policy(
                    keep_cost_nano_usd,
                    rotate_so_far,
                    minimum_savings_nano_usd,
                    policy.minimum_savings_ratio_ppm,
                )?
            {
                break_even_turns = Some(turn);
            }
        }
        let rotate_cost_nano_usd = rotate_compactor_cost_nano_usd
            .checked_add(rotate_first_epoch_cost_nano_usd)
            .and_then(|value| value.checked_add(rotate_followup_cost_nano_usd))
            .context("rotate horizon cost overflowed")?;
        let savings_nano_usd = keep_cost_nano_usd.saturating_sub(rotate_cost_nano_usd);
        let savings_ratio_ppm = ratio_ppm(savings_nano_usd, keep_cost_nano_usd)?;
        let qualifies = savings_meets_policy(
            keep_cost_nano_usd,
            rotate_cost_nano_usd,
            minimum_savings_nano_usd,
            policy.minimum_savings_ratio_ppm,
        )? && break_even_turns
            .is_some_and(|turns| turns <= policy.max_break_even_turns);
        Ok(Self {
            schema_version: COMPACTION_ECONOMICS_V2_SCHEMA_VERSION,
            pricing,
            input,
            horizon_turns: policy.horizon_turns,
            minimum_savings_nano_usd,
            keep_cost_nano_usd,
            rotate_compactor_cost_nano_usd,
            rotate_first_epoch_cost_nano_usd,
            rotate_followup_cost_nano_usd,
            rotate_cost_nano_usd,
            savings_nano_usd,
            savings_ratio_ppm,
            break_even_turns,
            qualifies,
        })
    }

    fn validate(&self, policy: &CompactionEconomicsPolicyV1) -> Result<()> {
        if self.schema_version != COMPACTION_ECONOMICS_V2_SCHEMA_VERSION {
            bail!("unsupported compaction cost projection schema version");
        }
        let rebuilt = Self::project(self.pricing.clone(), self.input.clone(), policy)?;
        if &rebuilt != self {
            bail!("compaction cost projection does not match its evidence");
        }
        Ok(())
    }
}

/// Rollout gate applied after deterministic fit and cost evaluation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CompactionRolloutModeV1 {
    Shadow,
    Preview,
    Automatic,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CompactionAdmissionDecisionV2 {
    Shadow,
    Admit,
    Preview,
    Reject,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CompactionAdmissionReasonV2 {
    EmergencyFit,
    ProjectedFitRequired,
    QualifiedCostSavings,
    PricingUnavailable,
    LowForecastConfidence,
    ExpectedTurnsBeforeBreakEven,
    InsufficientSavings,
}

/// Durable V3 admission decision and its rollout guards.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct CompactionAdmissionV2 {
    pub decision: CompactionAdmissionDecisionV2,
    pub reason: CompactionAdmissionReasonV2,
    pub rollout_mode: CompactionRolloutModeV1,
    pub user_confirmed: bool,
    pub automatic_allowed: bool,
    pub user_confirmation_required: bool,
    pub v3_would_admit: bool,
}

/// V2 extension carried by the existing portable economics proof.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct CompactionEconomicsV2 {
    pub schema_version: u16,
    pub policy: CompactionEconomicsPolicyV1,
    pub forecast: CompactionFitForecastV1,
    pub cost_projection: Option<CompactionCostProjectionV1>,
    pub token_savings: u64,
    pub token_savings_ratio_ppm: u32,
    pub admission: CompactionAdmissionV2,
}

/// Inputs that select shadow/preview/automatic behavior after forecast and cost evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactionAdmissionOptionsV2 {
    pub rollout_mode: CompactionRolloutModeV1,
    pub user_confirmed: bool,
}

impl CompactionEconomicsV2 {
    /// Evaluates fit first, then trusted-price cost, then manual-only token heuristics.
    ///
    /// Cost-only candidates always require confirmation in the initial rollout. Low-confidence or
    /// unpriced candidates never become automatic. Fit-required/emergency candidates can become
    /// automatic only when the rollout mode explicitly enables it.
    pub fn evaluate(
        forecast: CompactionFitForecastV1,
        policy: CompactionEconomicsPolicyV1,
        pricing_and_input: Option<(TrustedCompactionPricingV1, CompactionCostModelInputV1)>,
        token_savings: u64,
        token_savings_ratio_ppm: u32,
        options: CompactionAdmissionOptionsV2,
    ) -> Result<Self> {
        forecast.validate()?;
        policy.validate()?;
        if token_savings_ratio_ppm > 1_000_000 {
            bail!("compaction token savings ratio exceeds one million ppm");
        }
        let cost_projection = pricing_and_input
            .map(|(pricing, input)| CompactionCostProjectionV1::project(pricing, input, &policy))
            .transpose()?;
        let admission = derive_admission(
            &forecast,
            &policy,
            cost_projection.as_ref(),
            token_savings,
            token_savings_ratio_ppm,
            options,
        );
        let result = Self {
            schema_version: COMPACTION_ECONOMICS_V2_SCHEMA_VERSION,
            policy,
            forecast,
            cost_projection,
            token_savings,
            token_savings_ratio_ppm,
            admission,
        };
        result.validate()?;
        Ok(result)
    }

    pub(crate) fn validate(&self) -> Result<()> {
        if self.schema_version != COMPACTION_ECONOMICS_V2_SCHEMA_VERSION {
            bail!("unsupported compaction economics V2 schema version");
        }
        self.policy.validate()?;
        self.forecast.validate()?;
        if let Some(cost) = &self.cost_projection {
            cost.validate(&self.policy)?;
        }
        if self.token_savings_ratio_ppm > 1_000_000 {
            bail!("compaction economics V2 token savings ratio is invalid");
        }
        let rebuilt = derive_admission(
            &self.forecast,
            &self.policy,
            self.cost_projection.as_ref(),
            self.token_savings,
            self.token_savings_ratio_ppm,
            CompactionAdmissionOptionsV2 {
                rollout_mode: self.admission.rollout_mode,
                user_confirmed: self.admission.user_confirmed,
            },
        );
        if rebuilt != self.admission {
            bail!("compaction economics V2 admission does not match its evidence");
        }
        Ok(())
    }
}

fn derive_admission(
    forecast: &CompactionFitForecastV1,
    policy: &CompactionEconomicsPolicyV1,
    cost_projection: Option<&CompactionCostProjectionV1>,
    token_savings: u64,
    token_savings_ratio_ppm: u32,
    options: CompactionAdmissionOptionsV2,
) -> CompactionAdmissionV2 {
    let emergency = forecast.pressure_state == CompactionPressureStateV1::Emergency;
    let fit = emergency || forecast.fit_required;
    let cost_qualified = cost_projection.is_some_and(|projection| {
        projection.qualifies
            && projection
                .break_even_turns
                .is_some_and(|turns| turns <= forecast.input.expected_remaining_turns.turns)
    });
    let confidence_ok = forecast.input.expected_remaining_turns.confidence
        >= CompactionForecastConfidenceV1::Medium;
    let unpriced_token_candidate = cost_projection.is_none()
        && token_savings >= policy.minimum_savings_tokens_equivalent
        && token_savings_ratio_ppm >= policy.minimum_savings_ratio_ppm;
    let v3_would_admit = fit || (cost_qualified && confidence_ok);
    let reason = if emergency {
        CompactionAdmissionReasonV2::EmergencyFit
    } else if forecast.fit_required {
        CompactionAdmissionReasonV2::ProjectedFitRequired
    } else if cost_qualified && !confidence_ok {
        CompactionAdmissionReasonV2::LowForecastConfidence
    } else if cost_qualified {
        CompactionAdmissionReasonV2::QualifiedCostSavings
    } else if cost_projection.is_some_and(|projection| {
        projection
            .break_even_turns
            .is_some_and(|turns| turns > forecast.input.expected_remaining_turns.turns)
    }) {
        CompactionAdmissionReasonV2::ExpectedTurnsBeforeBreakEven
    } else if unpriced_token_candidate {
        CompactionAdmissionReasonV2::PricingUnavailable
    } else {
        CompactionAdmissionReasonV2::InsufficientSavings
    };
    let (decision, automatic_allowed, user_confirmation_required) = match options.rollout_mode {
        CompactionRolloutModeV1::Shadow => (CompactionAdmissionDecisionV2::Shadow, false, false),
        CompactionRolloutModeV1::Preview
            if options.user_confirmed && (fit || cost_qualified || unpriced_token_candidate) =>
        {
            (CompactionAdmissionDecisionV2::Admit, false, false)
        }
        CompactionRolloutModeV1::Preview if fit || cost_qualified || unpriced_token_candidate => {
            (CompactionAdmissionDecisionV2::Preview, false, true)
        }
        CompactionRolloutModeV1::Automatic if fit => {
            (CompactionAdmissionDecisionV2::Admit, true, false)
        }
        CompactionRolloutModeV1::Automatic if cost_qualified || unpriced_token_candidate => {
            // Initial V3 rollout never turns a cost-only forecast into an unattended epoch
            // rotation. Automatic mode may surface the candidate, but only fit/emergency
            // evidence can authorize mutation without an explicit confirmation.
            (CompactionAdmissionDecisionV2::Preview, false, true)
        }
        CompactionRolloutModeV1::Preview | CompactionRolloutModeV1::Automatic => {
            (CompactionAdmissionDecisionV2::Reject, false, false)
        }
    };
    CompactionAdmissionV2 {
        decision,
        reason,
        rollout_mode: options.rollout_mode,
        user_confirmed: options.user_confirmed,
        automatic_allowed,
        user_confirmation_required,
        v3_would_admit,
    }
}

fn price_to_nano_usd(price: f64) -> Result<u64> {
    if !price.is_finite() || price < 0.0 {
        bail!("compaction price is not finite and non-negative");
    }
    let scaled = price * NANO_USD_PER_USD;
    if scaled > u64::MAX as f64 {
        bail!("compaction price exceeds nano-USD representation");
    }
    Ok(scaled.round() as u64)
}

fn charge(tokens: u64, nano_usd_per_unit: u64, unit_tokens: u64) -> Result<u64> {
    if tokens == 0 || nano_usd_per_unit == 0 {
        return Ok(0);
    }
    let numerator = u128::from(tokens)
        .checked_mul(u128::from(nano_usd_per_unit))
        .context("compaction cost numerator overflowed")?;
    let cost = numerator.div_ceil(u128::from(unit_tokens));
    u64::try_from(cost).context("compaction cost exceeds u64")
}

fn cache_blended_charge(
    tokens: u64,
    hit_ratio_ppm: u32,
    pricing: &TrustedCompactionPricingV1,
) -> Result<u64> {
    if hit_ratio_ppm > 1_000_000 {
        bail!("cache hit ratio exceeds one million ppm");
    }
    let hit_tokens = u64::try_from(
        u128::from(tokens)
            .checked_mul(u128::from(hit_ratio_ppm))
            .context("cache-hit token projection overflowed")?
            / 1_000_000_u128,
    )
    .context("cache-hit token projection exceeds u64")?;
    let miss_tokens = tokens.saturating_sub(hit_tokens);
    charge(
        hit_tokens,
        pricing.cache_read_nano_usd_per_unit,
        pricing.unit_tokens,
    )?
    .checked_add(charge(
        miss_tokens,
        pricing.new_input_price(),
        pricing.unit_tokens,
    )?)
    .context("cache-blended input cost overflowed")
}

fn savings_meets_policy(
    keep_cost: u64,
    rotate_cost: u64,
    minimum_savings: u64,
    minimum_ratio_ppm: u32,
) -> Result<bool> {
    let savings = keep_cost.saturating_sub(rotate_cost);
    Ok(savings >= minimum_savings && ratio_ppm(savings, keep_cost)? >= minimum_ratio_ppm)
}

fn ratio_ppm(numerator: u64, denominator: u64) -> Result<u32> {
    if denominator == 0 {
        return Ok(0);
    }
    let ratio = u128::from(numerator)
        .checked_mul(1_000_000)
        .context("compaction ratio overflowed")?
        / u128::from(denominator);
    u32::try_from(ratio.min(1_000_000)).context("compaction ratio exceeds u32")
}

fn ratio_at_least(value: u64, total: u64, threshold_ppm: u32) -> Result<bool> {
    let lhs = u128::from(value)
        .checked_mul(1_000_000)
        .context("compaction pressure ratio overflowed")?;
    let rhs = u128::from(total)
        .checked_mul(u128::from(threshold_ppm))
        .context("compaction pressure threshold overflowed")?;
    Ok(lhs >= rhs)
}

#[cfg(test)]
#[path = "tests/compaction_economics_v2_tests.rs"]
mod tests;
