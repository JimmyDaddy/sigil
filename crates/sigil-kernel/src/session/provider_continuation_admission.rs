use anyhow::{Context, Result, bail};

use super::*;

/// Read-only result of evaluating a frozen provider-observed resolution plan.
///
/// These values never authorize provider I/O by themselves. In particular, the hybrid result
/// only proves the persisted plan has a valid semantic-checkpoint stage; a later provider slice
/// must still write its own physical-attempt start barrier before it can send anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderObservedResolutionAdmission {
    /// The source provider attempt has not reached a durable terminal, so resolution must wait.
    AwaitingSourceAttemptTerminal {
        resolution_plan_id: ProviderObservedResolutionPlanId,
        candidate_id: ProviderContinuationCandidateId,
        physical_attempt_id: ProviderPhysicalAttemptId,
    },
    /// The frozen NativeOnly before/after proofs fit and meet all durable savings thresholds.
    NativeOnlyReady {
        resolution_plan_id: ProviderObservedResolutionPlanId,
        candidate_id: ProviderContinuationCandidateId,
        guaranteed_savings_tokens: u64,
    },
    /// The frozen hybrid plan may later start a semantic checkpoint physical attempt.
    HybridSemanticCheckpointAuthorized {
        resolution_plan_id: ProviderObservedResolutionPlanId,
        candidate_id: ProviderContinuationCandidateId,
    },
    /// A durable source terminal or frozen economics proof makes this plan ineligible.
    Rejected {
        resolution_plan_id: ProviderObservedResolutionPlanId,
        candidate_id: ProviderContinuationCandidateId,
        reason: ProviderObservedResolutionAdmissionRejection,
    },
}

/// Why a frozen resolution plan cannot progress at its current durable frontier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderObservedResolutionAdmissionRejection {
    SourceAttemptOutcome(ProviderPhysicalAttemptOutcome),
    NativeAfterInputDoesNotFit,
    NoGuaranteedSavings,
    SavingsBelowMinimumTokens {
        guaranteed_savings_tokens: u64,
        minimum_savings_tokens: u64,
    },
    SavingsBelowMinimumRatio {
        guaranteed_savings_tokens: u64,
        before_tokens: u64,
        minimum_savings_ratio_ppm: u32,
    },
}

/// Deterministic evaluator for C1-frozen provider-observed resolution plans.
///
/// It reads only the validated V2 stream. It never loads continuation payload bytes, writes a
/// direct event, creates a provider request, or changes the active compaction boundary.
pub struct ProviderObservedResolutionAdmissionEvaluator;

impl ProviderObservedResolutionAdmissionEvaluator {
    /// Evaluates every frozen provider-observed resolution plan in deterministic plan-id order.
    ///
    /// # Errors
    ///
    /// Returns an error when the V2 stream, plan provenance, terminal order, or frozen arithmetic
    /// cannot be proven. Callers must not substitute current configuration or estimates.
    pub fn from_records(
        records: &[SessionStreamRecord],
    ) -> Result<Vec<ProviderObservedResolutionAdmission>> {
        let continuations = ProviderContinuationProjection::from_records(records)?;
        let physical_attempts = ProviderPhysicalAttemptProjection::from_records(records)?;
        continuations
            .resolution_plans()
            .map(|plan| evaluate_plan(plan, &continuations, &physical_attempts))
            .collect()
    }
}

impl JsonlSessionStore {
    /// Reads frozen provider-observed resolution admissions without mutation.
    ///
    /// # Errors
    ///
    /// Returns an error when the current V2 stream cannot prove a safe frozen-plan decision.
    pub fn provider_observed_resolution_admissions(
        &self,
    ) -> Result<Vec<ProviderObservedResolutionAdmission>> {
        let records = Self::read_event_records(self.path())?;
        ProviderObservedResolutionAdmissionEvaluator::from_records(&records)
    }
}

fn evaluate_plan(
    plan: &ProviderObservedResolutionPlanState,
    continuations: &ProviderContinuationProjection,
    physical_attempts: &ProviderPhysicalAttemptProjection,
) -> Result<ProviderObservedResolutionAdmission> {
    let observation = continuations
        .observation(&plan.entry.observation_id)
        .context("provider observed resolution plan lost its observation")?;
    let candidate = continuations
        .candidate(&plan.entry.candidate_id)
        .context("provider observed resolution plan lost its candidate")?;
    let attempt = physical_attempts
        .attempt(&observation.entry.physical_attempt_id)
        .context("provider observed resolution plan lost its source physical attempt")?;

    let awaiting = || ProviderObservedResolutionAdmission::AwaitingSourceAttemptTerminal {
        resolution_plan_id: plan.entry.resolution_plan_id.clone(),
        candidate_id: candidate.entry.candidate_id.clone(),
        physical_attempt_id: observation.entry.physical_attempt_id.clone(),
    };
    let Some(terminal) = &attempt.terminal else {
        return Ok(awaiting());
    };
    let terminal_sequence = attempt
        .terminal_stream_sequence
        .context("provider physical terminal is missing its stream sequence")?;
    if terminal_sequence >= plan.stream_sequence {
        bail!("provider observed resolution plan precedes its source physical terminal")
    }
    if terminal.outcome != ProviderPhysicalAttemptOutcome::Completed {
        return Ok(ProviderObservedResolutionAdmission::Rejected {
            resolution_plan_id: plan.entry.resolution_plan_id.clone(),
            candidate_id: candidate.entry.candidate_id.clone(),
            reason: ProviderObservedResolutionAdmissionRejection::SourceAttemptOutcome(
                terminal.outcome,
            ),
        });
    }

    match plan.entry.resolution_mode {
        ProviderContinuationResolutionMode::NativeOnly => evaluate_native_only(plan, candidate),
        ProviderContinuationResolutionMode::NativePlusPortableModelCheckpoint => Ok(
            ProviderObservedResolutionAdmission::HybridSemanticCheckpointAuthorized {
                resolution_plan_id: plan.entry.resolution_plan_id.clone(),
                candidate_id: candidate.entry.candidate_id.clone(),
            },
        ),
    }
}

fn evaluate_native_only(
    plan: &ProviderObservedResolutionPlanState,
    candidate: &ProviderContinuationCandidateState,
) -> Result<ProviderObservedResolutionAdmission> {
    let after = plan
        .entry
        .native_after_input
        .as_ref()
        .context("native-only resolution plan is missing native-after evidence")?;
    let budget = &plan.entry.target_budget;
    let target = &budget.target_request;
    let reserved = target
        .requested_output_tokens
        .checked_add(target.safety_buffer_tokens)
        .context("provider observed resolution target reservation overflowed")?;
    let post_request_tokens = after
        .guaranteed_tokens()
        .checked_add(reserved)
        .context("provider observed resolution native-after fit overflowed")?;
    if post_request_tokens > target.context_window_tokens {
        return Ok(ProviderObservedResolutionAdmission::Rejected {
            resolution_plan_id: plan.entry.resolution_plan_id.clone(),
            candidate_id: candidate.entry.candidate_id.clone(),
            reason: ProviderObservedResolutionAdmissionRejection::NativeAfterInputDoesNotFit,
        });
    }

    let before_tokens = plan.entry.before_input.guaranteed_tokens();
    let guaranteed_savings_tokens = before_tokens.saturating_sub(after.guaranteed_tokens());
    if guaranteed_savings_tokens == 0 {
        return Ok(ProviderObservedResolutionAdmission::Rejected {
            resolution_plan_id: plan.entry.resolution_plan_id.clone(),
            candidate_id: candidate.entry.candidate_id.clone(),
            reason: ProviderObservedResolutionAdmissionRejection::NoGuaranteedSavings,
        });
    }
    if guaranteed_savings_tokens < budget.minimum_savings_tokens {
        return Ok(ProviderObservedResolutionAdmission::Rejected {
            resolution_plan_id: plan.entry.resolution_plan_id.clone(),
            candidate_id: candidate.entry.candidate_id.clone(),
            reason: ProviderObservedResolutionAdmissionRejection::SavingsBelowMinimumTokens {
                guaranteed_savings_tokens,
                minimum_savings_tokens: budget.minimum_savings_tokens,
            },
        });
    }
    let actual_ppm = guaranteed_savings_tokens
        .checked_mul(1_000_000)
        .context("provider observed resolution savings ratio overflowed")?;
    let required_ppm = before_tokens
        .checked_mul(u64::from(budget.minimum_savings_ratio_ppm))
        .context("provider observed resolution minimum ratio overflowed")?;
    if actual_ppm < required_ppm {
        return Ok(ProviderObservedResolutionAdmission::Rejected {
            resolution_plan_id: plan.entry.resolution_plan_id.clone(),
            candidate_id: candidate.entry.candidate_id.clone(),
            reason: ProviderObservedResolutionAdmissionRejection::SavingsBelowMinimumRatio {
                guaranteed_savings_tokens,
                before_tokens,
                minimum_savings_ratio_ppm: budget.minimum_savings_ratio_ppm,
            },
        });
    }
    Ok(ProviderObservedResolutionAdmission::NativeOnlyReady {
        resolution_plan_id: plan.entry.resolution_plan_id.clone(),
        candidate_id: candidate.entry.candidate_id.clone(),
        guaranteed_savings_tokens,
    })
}
