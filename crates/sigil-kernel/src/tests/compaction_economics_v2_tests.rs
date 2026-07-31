use anyhow::Result;

use super::*;

fn remaining_turns(confidence: CompactionForecastConfidenceV1) -> ExpectedRemainingTurnsV1 {
    ExpectedRemainingTurnsV1 {
        turns: 3,
        source: CompactionForecastSourceV1::SessionTurnShape,
        confidence,
        source_event_ids: Vec::new(),
    }
}

fn fit_forecast(
    current_input_tokens: u64,
    next_turn_p95_tokens: u64,
    overflow_observed: bool,
    confidence: CompactionForecastConfidenceV1,
) -> Result<CompactionFitForecastV1> {
    CompactionFitForecastV1::from_input(CompactionFitForecastInputV1 {
        context_window_tokens: 100_000,
        current_input_tokens,
        next_turn_p95_tokens,
        reserved_output_tokens: 8_000,
        tool_growth_p95_tokens: 4_000,
        provider_state_tokens: 2_000,
        safety_buffer_tokens: 6_000,
        bulky_shrink_candidate_tokens: 0,
        overflow_observed,
        expected_remaining_turns: remaining_turns(confidence),
    })
}

fn pricing(
    id: &str,
    cache_read: f64,
    cache_write: Option<f64>,
    uncached_input: f64,
    output: f64,
) -> Result<TrustedCompactionPricingV1> {
    TrustedCompactionPricingV1::from_model_snapshot(&ModelPricingSnapshotV1 {
        schema_version: ModelPricingSnapshotV1::SCHEMA_VERSION,
        snapshot_id: id.to_owned(),
        currency: "USD".to_owned(),
        unit_tokens: 1_000_000,
        cache_read_per_unit: cache_read,
        cache_write_per_unit: cache_write,
        uncached_input_per_unit: uncached_input,
        output_per_unit: output,
        source: "provider-owned-test-catalog".to_owned(),
        verified_at: "2026-07-28".to_owned(),
    })
}

fn cost_input() -> CompactionCostModelInputV1 {
    CompactionCostModelInputV1 {
        current_cache_read_tokens: 400_000,
        current_uncached_input_tokens: 10_000,
        post_rotation_input_tokens: 60_000,
        next_turn_p95_tokens: 10_000,
        compactor_cache_read_tokens: 400_000,
        compactor_uncached_input_tokens: 0,
        compactor_output_tokens: 10_000,
        cache_scenario: None,
    }
}

fn admission_options(rollout_mode: CompactionRolloutModeV1) -> CompactionAdmissionOptionsV2 {
    CompactionAdmissionOptionsV2 {
        rollout_mode,
        user_confirmed: false,
    }
}

#[test]
fn fit_forecast_reserves_every_non_input_budget_and_records_pressure() -> Result<()> {
    let forecast = fit_forecast(74_000, 7_000, false, CompactionForecastConfidenceV1::Medium)?;

    assert_eq!(forecast.usable_context_tokens, 80_000);
    assert_eq!(forecast.projected_next_input_tokens, 81_000);
    assert!(forecast.fit_required);
    assert_eq!(forecast.pressure_state, CompactionPressureStateV1::Admit);
    assert_eq!(
        forecast.input.expected_remaining_turns.source,
        CompactionForecastSourceV1::SessionTurnShape
    );
    Ok(())
}

#[test]
fn fallback_forecast_cannot_claim_structured_confidence() {
    let error = CompactionFitForecastV1::from_input(CompactionFitForecastInputV1 {
        context_window_tokens: 100_000,
        current_input_tokens: 10_000,
        next_turn_p95_tokens: 1_000,
        reserved_output_tokens: 1_000,
        tool_growth_p95_tokens: 1_000,
        provider_state_tokens: 1_000,
        safety_buffer_tokens: 1_000,
        bulky_shrink_candidate_tokens: 0,
        overflow_observed: false,
        expected_remaining_turns: ExpectedRemainingTurnsV1 {
            turns: 3,
            source: CompactionForecastSourceV1::ConservativeFallback,
            confidence: CompactionForecastConfidenceV1::High,
            source_event_ids: Vec::new(),
        },
    })
    .expect_err("fallback evidence must not impersonate a structured forecast");
    assert!(error.to_string().contains("fallback"));
}

#[test]
fn fit_required_is_automatic_even_when_rotation_costs_more() -> Result<()> {
    let economics = CompactionEconomicsV2::evaluate(
        fit_forecast(75_000, 10_000, false, CompactionForecastConfidenceV1::Low)?,
        CompactionEconomicsPolicyV1::default(),
        Some((
            pricing("anthropic-write-premium", 0.10, Some(1.25), 1.00, 5.00)?,
            cost_input(),
        )),
        20_000,
        200_000,
        admission_options(CompactionRolloutModeV1::Automatic),
    )?;

    assert_eq!(
        economics.admission.decision,
        CompactionAdmissionDecisionV2::Admit
    );
    assert!(economics.admission.automatic_allowed);
    assert_eq!(
        economics.admission.reason,
        CompactionAdmissionReasonV2::ProjectedFitRequired
    );
    assert!(
        !economics
            .cost_projection
            .as_ref()
            .expect("priced projection")
            .qualifies
    );
    Ok(())
}

#[test]
fn unpriced_cost_only_candidate_is_preview_only() -> Result<()> {
    let forecast = fit_forecast(40_000, 5_000, false, CompactionForecastConfidenceV1::High)?;
    let economics = CompactionEconomicsV2::evaluate(
        forecast.clone(),
        CompactionEconomicsPolicyV1::default(),
        None,
        20_000,
        200_000,
        admission_options(CompactionRolloutModeV1::Automatic),
    )?;

    assert_eq!(
        economics.admission.decision,
        CompactionAdmissionDecisionV2::Preview
    );
    assert!(!economics.admission.automatic_allowed);
    assert!(economics.admission.user_confirmation_required);
    assert_eq!(
        economics.admission.reason,
        CompactionAdmissionReasonV2::PricingUnavailable
    );
    let confirmed = CompactionEconomicsV2::evaluate(
        forecast,
        CompactionEconomicsPolicyV1::default(),
        None,
        20_000,
        200_000,
        CompactionAdmissionOptionsV2 {
            rollout_mode: CompactionRolloutModeV1::Preview,
            user_confirmed: true,
        },
    )?;
    assert_eq!(
        confirmed.admission.decision,
        CompactionAdmissionDecisionV2::Admit
    );
    assert!(confirmed.admission.user_confirmed);
    assert!(!confirmed.admission.automatic_allowed);
    Ok(())
}

#[test]
fn trusted_cost_savings_with_measured_turn_shape_requires_confirmation() -> Result<()> {
    let economics = CompactionEconomicsV2::evaluate(
        fit_forecast(40_000, 5_000, false, CompactionForecastConfidenceV1::Medium)?,
        CompactionEconomicsPolicyV1::default(),
        Some((
            pricing("openai-moderate-discount", 0.25, None, 1.25, 2.00)?,
            CompactionCostModelInputV1 {
                compactor_output_tokens: 0,
                ..cost_input()
            },
        )),
        20_000,
        200_000,
        admission_options(CompactionRolloutModeV1::Automatic),
    )?;

    assert_eq!(
        economics.admission.decision,
        CompactionAdmissionDecisionV2::Preview
    );
    assert!(!economics.admission.automatic_allowed);
    assert!(economics.admission.user_confirmation_required);
    assert_eq!(
        economics.admission.reason,
        CompactionAdmissionReasonV2::QualifiedCostSavings
    );
    Ok(())
}

#[test]
fn cache_price_shapes_never_admit_a_cost_increasing_reset() -> Result<()> {
    let policy = CompactionEconomicsPolicyV1::default();
    let deepseek = CompactionCostProjectionV1::project(
        pricing("deepseek-cheap-read", 0.028, None, 0.28, 0.42)?,
        cost_input(),
        &policy,
    )?;
    let anthropic = CompactionCostProjectionV1::project(
        pricing("anthropic-write-premium", 0.10, Some(1.25), 1.00, 5.00)?,
        cost_input(),
        &policy,
    )?;
    assert!(deepseek.rotate_cost_nano_usd >= deepseek.keep_cost_nano_usd);
    assert!(!deepseek.qualifies);
    assert!(anthropic.rotate_cost_nano_usd >= anthropic.keep_cost_nano_usd);
    assert!(!anthropic.qualifies);

    let openai = CompactionCostProjectionV1::project(
        pricing("openai-moderate-discount", 0.25, None, 1.25, 2.00)?,
        CompactionCostModelInputV1 {
            compactor_output_tokens: 0,
            ..cost_input()
        },
        &policy,
    )?;
    assert!(openai.rotate_cost_nano_usd < openai.keep_cost_nano_usd);
    assert!(openai.qualifies);
    assert!(openai.break_even_turns.is_some_and(|turns| turns <= 3));
    Ok(())
}

#[test]
fn shadow_mode_records_v3_decision_without_activating() -> Result<()> {
    let economics = CompactionEconomicsV2::evaluate(
        fit_forecast(75_000, 10_000, false, CompactionForecastConfidenceV1::High)?,
        CompactionEconomicsPolicyV1::default(),
        None,
        20_000,
        200_000,
        CompactionAdmissionOptionsV2 {
            rollout_mode: CompactionRolloutModeV1::Shadow,
            user_confirmed: false,
        },
    )?;

    assert_eq!(
        economics.admission.decision,
        CompactionAdmissionDecisionV2::Shadow
    );
    assert!(economics.admission.v3_would_admit);
    assert!(!economics.admission.automatic_allowed);
    Ok(())
}

#[test]
fn persisted_admission_is_rederived_instead_of_trusting_mutable_flags() -> Result<()> {
    let mut economics = CompactionEconomicsV2::evaluate(
        fit_forecast(75_000, 10_000, false, CompactionForecastConfidenceV1::High)?,
        CompactionEconomicsPolicyV1::default(),
        None,
        20_000,
        200_000,
        admission_options(CompactionRolloutModeV1::Automatic),
    )?;
    economics.admission.v3_would_admit = false;

    assert!(
        economics
            .validate()
            .expect_err("tampered admission must not replay")
            .to_string()
            .contains("does not match")
    );
    Ok(())
}

#[test]
fn economics_grid_covers_turns_hits_ttl_summary_cache_plan_and_pressure_shapes() -> Result<()> {
    #[derive(Clone, Copy)]
    enum Plan {
        ShrinkOnly,
        Portable,
        Native,
    }

    let price_shapes = [
        pricing("deepseek-grid", 0.028, None, 0.28, 0.42)?,
        pricing("openai-grid", 0.25, None, 1.25, 2.00)?,
        pricing("anthropic-grid", 0.10, Some(1.25), 1.00, 5.00)?,
    ];
    let future_turns = [0, 1, 2, 3, 10];
    let hit_ratios = [0, 500_000, 900_000, 990_000];
    // With the bounded request-interval forecast used by this simulation, one surviving turn
    // represents a 5-minute cache and ten surviving turns represent a 1-hour cache.
    let ttl_turns = [1, 10];
    let summary_cache_hits = [false, true];
    let plans = [Plan::ShrinkOnly, Plan::Portable, Plan::Native];
    let pressure_inputs = [
        (40_000, 5_000, 0),       // currently fits
        (75_000, 10_000, 0),      // projected next request does not fit
        (65_000, 5_000, 100_000), // one tool-heavy turn makes local prepare useful
    ];
    let mut cases = 0_usize;

    for pricing in price_shapes {
        for horizon_turns in future_turns {
            for hit_ratio in hit_ratios {
                for ttl in ttl_turns {
                    for summary_cache_hit in summary_cache_hits {
                        for plan in plans {
                            for (current_input, next_turn, bulky_shrink) in pressure_inputs {
                                let policy = CompactionEconomicsPolicyV1 {
                                    horizon_turns,
                                    max_break_even_turns: horizon_turns.min(3),
                                    ..CompactionEconomicsPolicyV1::default()
                                };
                                let (
                                    post_rotation_input_tokens,
                                    mut compactor_read,
                                    mut compactor_miss,
                                    compactor_output,
                                ) = match plan {
                                    Plan::ShrinkOnly => (250_000, 0, 0, 0),
                                    Plan::Portable => (60_000, 400_000, 0, 10_000),
                                    Plan::Native => (40_000, 400_000, 0, 20_000),
                                };
                                if !summary_cache_hit && !matches!(plan, Plan::ShrinkOnly) {
                                    compactor_miss = compactor_read;
                                    compactor_read = 0;
                                }
                                let projection = CompactionCostProjectionV1::project(
                                    pricing.clone(),
                                    CompactionCostModelInputV1 {
                                        current_cache_read_tokens: 400_000,
                                        current_uncached_input_tokens: 10_000,
                                        post_rotation_input_tokens,
                                        next_turn_p95_tokens: 10_000,
                                        compactor_cache_read_tokens: compactor_read,
                                        compactor_uncached_input_tokens: compactor_miss,
                                        compactor_output_tokens: compactor_output,
                                        cache_scenario: Some(CompactionCacheScenarioV1 {
                                            current_epoch_hit_ratio_ppm: hit_ratio,
                                            rotated_epoch_hit_ratio_ppm: hit_ratio,
                                            current_epoch_ttl_turns: Some(ttl),
                                            rotated_epoch_ttl_turns: Some(ttl),
                                        }),
                                    },
                                    &policy,
                                )?;
                                if projection.rotate_cost_nano_usd >= projection.keep_cost_nano_usd
                                {
                                    assert!(!projection.qualifies);
                                }
                                if horizon_turns == 0 {
                                    assert_eq!(projection.keep_cost_nano_usd, 0);
                                    assert!(!projection.qualifies);
                                    assert_eq!(projection.break_even_turns, None);
                                }

                                let forecast = CompactionFitForecastV1::from_input(
                                    CompactionFitForecastInputV1 {
                                        context_window_tokens: 100_000,
                                        current_input_tokens: current_input,
                                        next_turn_p95_tokens: next_turn,
                                        reserved_output_tokens: 8_000,
                                        tool_growth_p95_tokens: 4_000,
                                        provider_state_tokens: 2_000,
                                        safety_buffer_tokens: 6_000,
                                        bulky_shrink_candidate_tokens: bulky_shrink,
                                        overflow_observed: false,
                                        expected_remaining_turns: remaining_turns(
                                            CompactionForecastConfidenceV1::High,
                                        ),
                                    },
                                )?;
                                if bulky_shrink > 0 && !forecast.fit_required {
                                    assert!(matches!(
                                        forecast.pressure_state,
                                        CompactionPressureStateV1::Prepare
                                            | CompactionPressureStateV1::Observe
                                    ));
                                }
                                cases += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    assert_eq!(cases, 3 * 5 * 4 * 2 * 2 * 3 * 3);
    Ok(())
}
