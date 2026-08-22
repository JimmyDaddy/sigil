use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::*;
use crate::{
    EventId, FrozenProviderRequestMaterial, ProviderFailureClassV1, ProviderFailureObservationV1,
    ProviderRequestReconstructionDispositionV1, ProviderRequestSourceFrontierV1,
    ProviderTransportFallbackCandidateV1, projection_apply_decision,
};

/// Schema version for durable provider-turn recovery facts.
pub const PROVIDER_TURN_RECOVERY_SCHEMA_VERSION: u16 = 1;
/// Projection schema version for provider-turn recovery facts.
pub const PROVIDER_TURN_RECOVERY_PROJECTION_SCHEMA_VERSION: u16 = 1;
/// Normal-profile retry cap for a single logical provider turn.
pub const DEFAULT_PROVIDER_TURN_MAX_TRANSPORT_RETRIES: u32 = 2;
/// Normal-profile retry cap after a stream produced only discardable partial output.
pub const DEFAULT_PROVIDER_TURN_MAX_PARTIAL_OUTPUT_RETRIES: u32 = 1;
/// Normal-profile first retry delay.
pub const DEFAULT_PROVIDER_TURN_INITIAL_DELAY_MS: u64 = 500;
/// Normal-profile maximum retry delay.
pub const DEFAULT_PROVIDER_TURN_MAX_DELAY_MS: u64 = 10_000;
/// Normal-profile symmetric jitter (10%) applied to locally-derived retry delays.
///
/// The value is expressed in millionths instead of a float so a recovery-policy fingerprint is
/// stable across platforms and the policy remains exactly comparable in durable tests.
pub const DEFAULT_PROVIDER_TURN_JITTER_RATIO_MILLIONTHS: u32 = 100_000;
/// Normal-profile total delay cap.
pub const DEFAULT_PROVIDER_TURN_MAX_CUMULATIVE_DELAY_MS: u64 = 120_000;

/// Durable output settlement visible to the recovery policy.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ProviderOutputStateV1 {
    None,
    TransientOnly,
    DurableSurfaceCommitted,
}

/// Exact settlement boundary for a local or hosted effect.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum EffectSettlementStateV1 {
    None,
    Settled,
    OutcomeUncertain,
}

/// Exact request material available to the live recovery owner.
///
/// Durable reconstruction is sufficient across process loss. Exact frozen material is equally
/// safe for a bounded retry while the current owner still holds and verifies it, but it must never
/// be treated as restart-safe authority.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ProviderTurnRequestMaterialAvailabilityV1 {
    DurableFrontierAndRuntimeInputs,
    ExactFrozenInCurrentProcess,
    Unavailable,
}

/// Recovery evidence derived from a terminal physical attempt; callers cannot invent booleans.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ProviderTurnRecoveryEvidenceV1 {
    pub logical_run_id: String,
    pub failed_physical_attempt_id: ProviderPhysicalAttemptId,
    pub request_material_fingerprint: String,
    pub request_envelope_digest: String,
    pub source_frontier: Option<ProviderRequestSourceFrontierV1>,
    pub failure: ProviderFailureObservationV1,
    pub output_state: ProviderOutputStateV1,
    pub local_tool_effect_state: EffectSettlementStateV1,
    pub hosted_effect_state: EffectSettlementStateV1,
    pub request_reconstruction: ProviderRequestReconstructionDispositionV1,
    pub request_material_availability: ProviderTurnRequestMaterialAvailabilityV1,
    /// A partial streamed tool request is never retried automatically, even before a local tool
    /// executor received it. The model may otherwise produce a distinct second proposal whose
    /// authority cannot be tied to the interrupted turn.
    pub partial_output_has_tool_calls: bool,
}

impl ProviderTurnRecoveryEvidenceV1 {
    /// Derives evidence from the exact terminal attempt and the same frozen request retained by
    /// the current process. Provider-hosted reads remain bounded provider work, while a hosted
    /// capability that may mutate external state fails closed after dispatch becomes uncertain.
    pub fn from_terminal_attempt(
        attempt: &ProviderPhysicalAttemptState,
        failure: ProviderFailureObservationV1,
        frozen_request: &FrozenProviderRequestMaterial,
    ) -> Result<Self> {
        let terminal = attempt
            .terminal
            .as_ref()
            .context("provider-turn recovery requires a terminal physical attempt")?;
        let envelope = attempt
            .entry
            .request_envelope
            .as_ref()
            .context("provider-turn recovery requires a request envelope")?;
        anyhow::ensure!(
            attempt.entry.request_material_fingerprint == frozen_request.fingerprint(),
            "provider-turn recovery frozen request does not match the terminal attempt"
        );
        envelope
            .verify_exact_process_local_request(frozen_request)
            .context("provider-turn recovery frozen request does not match its durable envelope")?;
        let output_state = if terminal.durable_output_event_ids.is_empty() {
            ProviderOutputStateV1::None
        } else {
            ProviderOutputStateV1::DurableSurfaceCommitted
        };
        // Merely enabling a provider-hosted read does not create a write-side effect. Only a
        // capability whose contract permits external mutation requires effect reconciliation once
        // request bytes may have crossed the provider boundary.
        let hosted_effect_state = if frozen_request
            .request()
            .hosted_tools
            .iter()
            .any(|tool| tool.kind.may_mutate_external_state())
            && failure.wire_state != crate::ProviderWireStateV1::NoBytesSent
        {
            EffectSettlementStateV1::OutcomeUncertain
        } else {
            EffectSettlementStateV1::None
        };
        Ok(Self {
            logical_run_id: attempt.entry.logical_run_id.clone(),
            failed_physical_attempt_id: attempt.entry.physical_attempt_id.clone(),
            request_material_fingerprint: attempt.entry.request_material_fingerprint.clone(),
            request_envelope_digest: envelope.canonical_request_hash.clone(),
            source_frontier: envelope.source_frontier.clone(),
            failure,
            output_state,
            local_tool_effect_state: if terminal.durable_side_effect_event_ids.is_empty() {
                EffectSettlementStateV1::None
            } else {
                EffectSettlementStateV1::Settled
            },
            hosted_effect_state,
            request_reconstruction: envelope.reconstruction_disposition,
            request_material_availability: match envelope.reconstruction_disposition {
                ProviderRequestReconstructionDispositionV1::DurableFrontierAndRuntimeInputs => {
                    ProviderTurnRequestMaterialAvailabilityV1::DurableFrontierAndRuntimeInputs
                }
                ProviderRequestReconstructionDispositionV1::InMemoryOnly
                | ProviderRequestReconstructionDispositionV1::ProcessLocalOverlayRequired => {
                    ProviderTurnRequestMaterialAvailabilityV1::ExactFrozenInCurrentProcess
                }
            },
            partial_output_has_tool_calls: false,
        })
    }

    #[must_use]
    pub fn is_zero_effect_retry_boundary(&self) -> bool {
        self.output_state == ProviderOutputStateV1::None
            && self.local_tool_effect_state == EffectSettlementStateV1::None
            && self.hosted_effect_state == EffectSettlementStateV1::None
    }
}

/// Durable accounting for a logical provider-turn retry budget.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RecoveryBudgetProjectionV1 {
    pub retry_count: u32,
    pub max_transport_retries: u32,
    pub partial_output_retry_count: u32,
    pub max_partial_output_retries: u32,
    pub cumulative_delay_ms: u64,
    pub max_cumulative_delay_ms: u64,
}

impl Default for RecoveryBudgetProjectionV1 {
    fn default() -> Self {
        Self {
            retry_count: 0,
            max_transport_retries: DEFAULT_PROVIDER_TURN_MAX_TRANSPORT_RETRIES,
            partial_output_retry_count: 0,
            max_partial_output_retries: DEFAULT_PROVIDER_TURN_MAX_PARTIAL_OUTPUT_RETRIES,
            cumulative_delay_ms: 0,
            max_cumulative_delay_ms: DEFAULT_PROVIDER_TURN_MAX_CUMULATIVE_DELAY_MS,
        }
    }
}

/// The pure, bounded provider-turn recovery policy. It performs no I/O and owns no writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderTurnRecoveryPolicyV1 {
    pub max_transport_retries: u32,
    pub max_partial_output_retries: u32,
    pub initial_delay_ms: u64,
    pub max_delay_ms: u64,
    /// Symmetric local-delay jitter expressed in millionths (`100_000` means 10%).
    pub jitter_ratio_millionths: u32,
    pub max_cumulative_delay_ms: u64,
}

impl Default for ProviderTurnRecoveryPolicyV1 {
    fn default() -> Self {
        Self {
            max_transport_retries: DEFAULT_PROVIDER_TURN_MAX_TRANSPORT_RETRIES,
            max_partial_output_retries: DEFAULT_PROVIDER_TURN_MAX_PARTIAL_OUTPUT_RETRIES,
            initial_delay_ms: DEFAULT_PROVIDER_TURN_INITIAL_DELAY_MS,
            max_delay_ms: DEFAULT_PROVIDER_TURN_MAX_DELAY_MS,
            jitter_ratio_millionths: DEFAULT_PROVIDER_TURN_JITTER_RATIO_MILLIONTHS,
            max_cumulative_delay_ms: DEFAULT_PROVIDER_TURN_MAX_CUMULATIVE_DELAY_MS,
        }
    }
}

/// Non-I/O policy result consumed by the existing provider-turn owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryDispositionV1 {
    RetryProviderTurn { retry_after_ms: u64 },
    Block { reason_code: &'static str },
    Pause { reason_code: &'static str },
    Irrecoverable { reason_code: &'static str },
    Cancelled,
}

impl ProviderTurnRecoveryPolicyV1 {
    /// Validates policy relationships before an owner can use this policy to mint a new durable
    /// schedule. The configuration layer additionally applies product hard caps; this method
    /// deliberately permits a zero delay for deterministic fault-injection fixtures.
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            self.max_delay_ms >= self.initial_delay_ms,
            "provider recovery max_delay_ms must be at least initial_delay_ms"
        );
        anyhow::ensure!(
            self.jitter_ratio_millionths <= 1_000_000,
            "provider recovery jitter_ratio must be between 0.0 and 1.0"
        );
        if self.max_transport_retries > 0 || self.max_partial_output_retries > 0 {
            anyhow::ensure!(
                self.max_cumulative_delay_ms >= self.initial_delay_ms,
                "provider recovery max_cumulative_delay_ms must be at least initial_delay_ms when retries are enabled"
            );
        }
        Ok(())
    }

    #[must_use]
    pub fn fingerprint(&self) -> String {
        format!(
            "provider-turn-recovery-v1:{}:{}:{}:{}:{}:{}",
            self.max_transport_retries,
            self.max_partial_output_retries,
            self.initial_delay_ms,
            self.max_delay_ms,
            self.jitter_ratio_millionths,
            self.max_cumulative_delay_ms
        )
    }

    #[must_use]
    pub fn decide(
        &self,
        evidence: &ProviderTurnRecoveryEvidenceV1,
        budget: RecoveryBudgetProjectionV1,
        cancelled: bool,
    ) -> RecoveryDispositionV1 {
        if cancelled || evidence.failure.class == ProviderFailureClassV1::Cancelled {
            return RecoveryDispositionV1::Cancelled;
        }
        if evidence.hosted_effect_state == EffectSettlementStateV1::OutcomeUncertain
            || evidence.local_tool_effect_state == EffectSettlementStateV1::OutcomeUncertain
        {
            return RecoveryDispositionV1::Block {
                reason_code: "effect_reconciliation_required",
            };
        }
        if !evidence.is_zero_effect_retry_boundary() {
            return RecoveryDispositionV1::Block {
                reason_code: "provider_output_or_effect_committed",
            };
        }
        let request_material_available = match evidence.request_material_availability {
            ProviderTurnRequestMaterialAvailabilityV1::DurableFrontierAndRuntimeInputs => {
                evidence.request_reconstruction
                    == ProviderRequestReconstructionDispositionV1::DurableFrontierAndRuntimeInputs
            }
            ProviderTurnRequestMaterialAvailabilityV1::ExactFrozenInCurrentProcess => true,
            ProviderTurnRequestMaterialAvailabilityV1::Unavailable => false,
        };
        if !request_material_available {
            return RecoveryDispositionV1::Block {
                reason_code: "recovery_material_unavailable",
            };
        }
        let partial_output =
            evidence.failure.wire_state == crate::ProviderWireStateV1::ResponseStarted;
        if partial_output && evidence.partial_output_has_tool_calls {
            return RecoveryDispositionV1::Block {
                reason_code: "partial_provider_tool_request_requires_review",
            };
        }
        match evidence.failure.class {
            ProviderFailureClassV1::Authentication
            | ProviderFailureClassV1::BillingOrQuota
            | ProviderFailureClassV1::RouteUnavailable
            | ProviderFailureClassV1::ContextCapacity => RecoveryDispositionV1::Block {
                reason_code: "provider_configuration_or_capacity_required",
            },
            ProviderFailureClassV1::ProtocolViolation
            | ProviderFailureClassV1::PermanentRequest => RecoveryDispositionV1::Block {
                reason_code: "provider_request_requires_attention",
            },
            ProviderFailureClassV1::RejectedBeforeDispatch
            | ProviderFailureClassV1::RateLimited
            | ProviderFailureClassV1::TransientServer
            | ProviderFailureClassV1::TransportInterrupted
            | ProviderFailureClassV1::StreamEndedUnexpectedly => {
                if partial_output
                    && budget.partial_output_retry_count >= self.max_partial_output_retries
                {
                    return RecoveryDispositionV1::Pause {
                        reason_code: "provider_partial_output_retry_budget_exhausted",
                    };
                }
                if !partial_output && budget.retry_count >= self.max_transport_retries {
                    return RecoveryDispositionV1::Pause {
                        reason_code: "provider_retry_budget_exhausted",
                    };
                }
                let exponent = budget.retry_count.min(20);
                let local_delay = self
                    .initial_delay_ms
                    .saturating_mul(1_u64 << exponent)
                    .min(self.max_delay_ms);
                let local_delay = self.jittered_local_delay(local_delay, evidence);
                let delay = evidence
                    .failure
                    .retry_after_ms
                    .unwrap_or(local_delay)
                    .min(self.max_delay_ms);
                if budget.cumulative_delay_ms.saturating_add(delay) > self.max_cumulative_delay_ms {
                    return RecoveryDispositionV1::Pause {
                        reason_code: "provider_retry_delay_budget_exhausted",
                    };
                }
                RecoveryDispositionV1::RetryProviderTurn {
                    retry_after_ms: delay,
                }
            }
            ProviderFailureClassV1::Cancelled => RecoveryDispositionV1::Cancelled,
        }
    }

    /// Derives the jitter from durable recovery inputs rather than process-local entropy. The
    /// resulting delay is persisted in the schedule, but deterministic derivation also makes
    /// policy-only tests and independently reconstructed schedules auditable.
    fn jittered_local_delay(
        &self,
        delay_ms: u64,
        evidence: &ProviderTurnRecoveryEvidenceV1,
    ) -> u64 {
        if delay_ms == 0 || self.jitter_ratio_millionths == 0 {
            return delay_ms;
        }
        let window = delay_ms.saturating_mul(u64::from(self.jitter_ratio_millionths)) / 1_000_000;
        if window == 0 {
            return delay_ms;
        }
        let mut hasher = Sha256::new();
        hasher.update(b"sigil-provider-turn-recovery-jitter-v1\\0");
        hasher.update(evidence.logical_run_id.as_bytes());
        hasher.update(b"\\0");
        hasher.update(evidence.failed_physical_attempt_id.as_bytes());
        hasher.update(b"\\0");
        hasher.update(evidence.request_envelope_digest.as_bytes());
        let digest = hasher.finalize();
        let sample = u64::from_be_bytes(
            digest[..8]
                .try_into()
                .expect("sha256 prefix always has eight bytes"),
        );
        let offset = sample % window.saturating_mul(2).saturating_add(1);
        delay_ms.saturating_sub(window).saturating_add(offset)
    }
}

/// Direct durable authority that permits exactly one subsequent physical attempt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ProviderTurnRecoveryScheduledEntry {
    pub schema_version: u16,
    pub recovery_id: String,
    pub logical_run_id: String,
    pub failed_physical_attempt_id: ProviderPhysicalAttemptId,
    pub next_physical_attempt_ordinal: u32,
    pub request_envelope_digest: String,
    pub source_frontier: Option<ProviderRequestSourceFrontierV1>,
    pub failure_class: ProviderFailureClassV1,
    pub retry_kind: ProviderTurnRecoveryRetryKindV1,
    pub not_before_unix_ms: u64,
    pub retry_after_ms: u64,
    pub budget_snapshot: RecoveryBudgetProjectionV1,
    pub recovery_policy_fingerprint: String,
}

/// Secret-free sidecar proving that live partial output from a failed physical attempt was
/// deliberately excluded from the next request and replaced on product surfaces.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ProviderTurnPartialOutputDiscardedEntryV1 {
    pub schema_version: u16,
    pub logical_run_id: String,
    pub physical_attempt_id: ProviderPhysicalAttemptId,
    pub text_bytes: u32,
    pub reasoning_bytes: u32,
    pub streamed_tool_call_count: u16,
}

/// Product-safe replacement signal for one failed streaming attempt.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct PublicProviderTurnPartialOutputDiscardedViewV1 {
    pub text_discarded: bool,
    pub reasoning_discarded: bool,
    pub tool_request_discarded: bool,
}

impl From<&ProviderTurnPartialOutputDiscardedEntryV1>
    for PublicProviderTurnPartialOutputDiscardedViewV1
{
    fn from(value: &ProviderTurnPartialOutputDiscardedEntryV1) -> Self {
        Self {
            text_discarded: value.text_bytes > 0,
            reasoning_discarded: value.reasoning_bytes > 0,
            tool_request_discarded: value.streamed_tool_call_count > 0,
        }
    }
}

/// Distinguishes an ordinary pre-output retry from a bounded retry that first discarded a live
/// partial stream. Both retain the same physical-attempt and request-frontier authority.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ProviderTurnRecoveryRetryKindV1 {
    Transport,
    PartialOutput,
}

/// Durable consumption of one scheduled recovery before its matching physical attempt starts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ProviderTurnRecoveryStartedEntry {
    pub schema_version: u16,
    pub recovery_id: String,
    pub logical_run_id: String,
    pub physical_attempt_id: ProviderPhysicalAttemptId,
    pub started_at_unix_ms: u64,
}

/// Durable authority selecting a provider-owned, semantically equivalent transport before the
/// recovery schedule can dispatch. It intentionally carries opaque fingerprints rather than a
/// URL, tenant, request body, or provider diagnostic.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ProviderTurnTransportFallbackSelectedEntryV1 {
    pub schema_version: u16,
    pub recovery_id: String,
    pub logical_run_id: String,
    pub failed_physical_attempt_id: ProviderPhysicalAttemptId,
    pub request_envelope_digest: String,
    pub candidate: ProviderTransportFallbackCandidateV1,
    pub selected_at_unix_ms: u64,
}

/// Durable terminal when a logical provider turn cannot automatically continue.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ProviderTurnRecoveryExhaustedEntry {
    pub schema_version: u16,
    pub logical_run_id: String,
    pub last_physical_attempt_id: ProviderPhysicalAttemptId,
    pub reason_code: String,
    pub budget_snapshot: RecoveryBudgetProjectionV1,
    pub terminal_disposition: ProviderTurnRecoveryTerminalDispositionV1,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ProviderTurnRecoveryTerminalDispositionV1 {
    Blocked,
    Paused,
    Irrecoverable,
    Cancelled,
}

/// Product-safe lifecycle phase for one logical provider turn. It intentionally omits physical
/// attempt ids, request digests, and provider diagnostics.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum PublicProviderTurnRecoveryPhaseV1 {
    Waiting,
    Recovering,
    Blocked,
    Paused,
}

/// Typed user action advertised by a recovery projection. Dispatch remains an application-owned
/// command and must revalidate the durable schedule/effect boundary before doing I/O.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum PublicProviderTurnRecoveryActionV1 {
    RetryNow,
    UpdateConnection,
    ReviewEffect,
    Cancel,
}

/// Bounded recovery state shared by all product surfaces.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct PublicProviderTurnRecoveryViewV1 {
    pub phase: PublicProviderTurnRecoveryPhaseV1,
    /// Count for the currently active retry class. This keeps the user-visible denominator
    /// correct for a partial-output replacement without exposing the policy matrix.
    pub active_retry_count: u32,
    /// Bound for the currently active retry class.
    pub active_max_retries: u32,
    pub retry_count: u32,
    pub max_transport_retries: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_retry_unix_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    pub available_actions: Vec<PublicProviderTurnRecoveryActionV1>,
    pub user_attention_required: bool,
}

impl PublicProviderTurnRecoveryViewV1 {
    #[must_use]
    pub fn waiting(schedule: &ProviderTurnRecoveryScheduledEntry) -> Self {
        let (active_retry_count, active_max_retries) = active_retry_budget(schedule);
        Self {
            phase: PublicProviderTurnRecoveryPhaseV1::Waiting,
            active_retry_count,
            active_max_retries,
            retry_count: schedule.budget_snapshot.retry_count,
            max_transport_retries: schedule.budget_snapshot.max_transport_retries,
            next_retry_unix_ms: Some(schedule.not_before_unix_ms),
            reason_code: None,
            available_actions: vec![
                PublicProviderTurnRecoveryActionV1::RetryNow,
                PublicProviderTurnRecoveryActionV1::Cancel,
            ],
            user_attention_required: false,
        }
    }

    #[must_use]
    pub fn recovering(schedule: &ProviderTurnRecoveryScheduledEntry) -> Self {
        let (active_retry_count, active_max_retries) = active_retry_budget(schedule);
        Self {
            phase: PublicProviderTurnRecoveryPhaseV1::Recovering,
            active_retry_count,
            active_max_retries,
            retry_count: schedule.budget_snapshot.retry_count,
            max_transport_retries: schedule.budget_snapshot.max_transport_retries,
            next_retry_unix_ms: None,
            reason_code: None,
            available_actions: vec![PublicProviderTurnRecoveryActionV1::Cancel],
            user_attention_required: false,
        }
    }

    #[must_use]
    pub fn terminal(entry: &ProviderTurnRecoveryExhaustedEntry) -> Self {
        let phase = match entry.terminal_disposition {
            ProviderTurnRecoveryTerminalDispositionV1::Paused
            | ProviderTurnRecoveryTerminalDispositionV1::Cancelled => {
                PublicProviderTurnRecoveryPhaseV1::Paused
            }
            ProviderTurnRecoveryTerminalDispositionV1::Blocked
            | ProviderTurnRecoveryTerminalDispositionV1::Irrecoverable => {
                PublicProviderTurnRecoveryPhaseV1::Blocked
            }
        };
        let mut available_actions = vec![
            PublicProviderTurnRecoveryActionV1::RetryNow,
            PublicProviderTurnRecoveryActionV1::Cancel,
        ];
        if entry.reason_code == "effect_reconciliation_required" {
            available_actions.insert(0, PublicProviderTurnRecoveryActionV1::ReviewEffect);
        }
        if matches!(
            entry.reason_code.as_str(),
            "provider_configuration_or_capacity_required" | "provider_request_requires_attention"
        ) {
            available_actions.insert(0, PublicProviderTurnRecoveryActionV1::UpdateConnection);
        }
        Self {
            phase,
            active_retry_count: entry.budget_snapshot.retry_count,
            active_max_retries: entry.budget_snapshot.max_transport_retries,
            retry_count: entry.budget_snapshot.retry_count,
            max_transport_retries: entry.budget_snapshot.max_transport_retries,
            next_retry_unix_ms: None,
            reason_code: Some(entry.reason_code.clone()),
            available_actions,
            user_attention_required: true,
        }
    }
}

fn active_retry_budget(schedule: &ProviderTurnRecoveryScheduledEntry) -> (u32, u32) {
    match schedule.retry_kind {
        ProviderTurnRecoveryRetryKindV1::Transport => (
            schedule.budget_snapshot.retry_count,
            schedule.budget_snapshot.max_transport_retries,
        ),
        ProviderTurnRecoveryRetryKindV1::PartialOutput => (
            schedule.budget_snapshot.partial_output_retry_count,
            schedule.budget_snapshot.max_partial_output_retries,
        ),
    }
}

/// Typed boundary returned to the current run owner after recovery reaches an actionable stop.
/// It deliberately exposes only a safe code and disposition, never provider diagnostics.
#[derive(Debug, thiserror::Error)]
#[error("provider-turn recovery {disposition:?}: {reason_code}")]
pub struct ProviderTurnRecoveryTerminalError {
    pub disposition: ProviderTurnRecoveryTerminalDispositionV1,
    pub reason_code: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderTurnRecoveryState {
    pub schedule: ProviderTurnRecoveryScheduledEntry,
    pub schedule_event_id: EventId,
    pub transport_fallback: Option<ProviderTurnTransportFallbackSelectedEntryV1>,
    pub started: Option<ProviderTurnRecoveryStartedEntry>,
    pub exhausted: Option<ProviderTurnRecoveryExhaustedEntry>,
}

/// Replayable provider-turn recovery projection. It is intentionally separate from task state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderTurnRecoveryProjection {
    cursor: Option<ProjectionCursor>,
    recoveries: BTreeMap<String, ProviderTurnRecoveryState>,
    terminals: BTreeMap<String, ProviderTurnRecoveryExhaustedEntry>,
    discarded_partials:
        BTreeMap<ProviderPhysicalAttemptId, ProviderTurnPartialOutputDiscardedEntryV1>,
}

impl ProviderTurnRecoveryProjection {
    pub fn from_records(records: &[SessionStreamRecord]) -> Result<Self> {
        let mut projection = Self::default();
        for record in records {
            projection.apply_record(record)?;
        }
        Ok(projection)
    }

    #[must_use]
    pub fn recovery(&self, recovery_id: &str) -> Option<&ProviderTurnRecoveryState> {
        self.recoveries.get(recovery_id)
    }

    #[must_use]
    pub fn recoveries_for_logical_run_id(
        &self,
        logical_run_id: &str,
    ) -> Vec<&ProviderTurnRecoveryState> {
        self.recoveries
            .values()
            .filter(|state| state.schedule.logical_run_id == logical_run_id)
            .collect()
    }

    /// Returns the durable logical-turn terminal, including a direct terminal that did not need
    /// a preceding retry schedule.
    #[must_use]
    pub fn terminal_for_logical_run_id(
        &self,
        logical_run_id: &str,
    ) -> Option<&ProviderTurnRecoveryExhaustedEntry> {
        self.terminals.get(logical_run_id)
    }

    #[must_use]
    pub fn discarded_partial_for_physical_attempt(
        &self,
        physical_attempt_id: &ProviderPhysicalAttemptId,
    ) -> Option<&ProviderTurnPartialOutputDiscardedEntryV1> {
        self.discarded_partials.get(physical_attempt_id)
    }

    /// Returns scheduled recovery that can be claimed after the supplied absolute time. A
    /// schedule that already has a durable `Started` fact is deliberately not claimable: a
    /// restart must repair it as uncertain rather than send the request again.
    #[must_use]
    pub fn claimable_schedules_at(&self, now_unix_ms: u64) -> Vec<&ProviderTurnRecoveryState> {
        self.recoveries
            .values()
            .filter(|state| {
                state.started.is_none()
                    && state.exhausted.is_none()
                    && state.schedule.not_before_unix_ms <= now_unix_ms
            })
            .collect()
    }

    #[must_use]
    pub fn budget_for_logical_run_id(&self, logical_run_id: &str) -> RecoveryBudgetProjectionV1 {
        self.recoveries
            .values()
            .filter(|state| state.schedule.logical_run_id == logical_run_id)
            .max_by_key(|state| {
                (
                    state.schedule.budget_snapshot.retry_count,
                    state.schedule.not_before_unix_ms,
                )
            })
            .map(|state| state.schedule.budget_snapshot)
            .unwrap_or_default()
    }

    fn apply_record(&mut self, record: &SessionStreamRecord) -> Result<()> {
        let event = record.stored_event();
        if projection_apply_decision(self.cursor.as_ref(), event)?
            == ProjectionApplyDecision::IgnoreAlreadyApplied
        {
            return Ok(());
        }
        match event.event_kind() {
            Some(DurableEventType::ProviderTurnRecoveryScheduled) => {
                let entry: ProviderTurnRecoveryScheduledEntry = decode_recovery_payload(event)?;
                validate_schedule(&entry)?;
                if self.recoveries.contains_key(&entry.recovery_id) {
                    bail!("provider-turn recovery was scheduled more than once");
                }
                self.recoveries.insert(
                    entry.recovery_id.clone(),
                    ProviderTurnRecoveryState {
                        schedule: entry,
                        schedule_event_id: event.event_id.clone(),
                        transport_fallback: None,
                        started: None,
                        exhausted: None,
                    },
                );
            }
            Some(DurableEventType::ProviderTurnTransportFallbackSelected) => {
                let entry: ProviderTurnTransportFallbackSelectedEntryV1 =
                    decode_recovery_payload(event)?;
                validate_transport_fallback_selected(&entry)?;
                let state = self
                    .recoveries
                    .get_mut(&entry.recovery_id)
                    .context("provider transport fallback references an unknown recovery")?;
                if state.schedule.logical_run_id != entry.logical_run_id
                    || state.schedule.failed_physical_attempt_id != entry.failed_physical_attempt_id
                    || state.schedule.request_envelope_digest != entry.request_envelope_digest
                    || state.started.is_some()
                    || state.exhausted.is_some()
                    || state.transport_fallback.is_some()
                {
                    bail!("provider transport fallback selection is inconsistent or duplicated");
                }
                state.transport_fallback = Some(entry);
            }
            Some(DurableEventType::ProviderTurnRecoveryStarted) => {
                let entry: ProviderTurnRecoveryStartedEntry = decode_recovery_payload(event)?;
                validate_started(&entry)?;
                let state = self
                    .recoveries
                    .get_mut(&entry.recovery_id)
                    .context("provider-turn recovery start references an unknown schedule")?;
                if state.schedule.logical_run_id != entry.logical_run_id || state.started.is_some()
                {
                    bail!("provider-turn recovery start is inconsistent or duplicated");
                }
                state.started = Some(entry);
            }
            Some(DurableEventType::ProviderTurnRecoveryExhausted) => {
                let entry: ProviderTurnRecoveryExhaustedEntry = decode_recovery_payload(event)?;
                validate_exhausted(&entry)?;
                if self
                    .terminals
                    .insert(entry.logical_run_id.clone(), entry.clone())
                    .is_some()
                {
                    bail!("provider-turn recovery logical turn was terminal more than once");
                }
                let matching = self
                    .recoveries
                    .values_mut()
                    .filter(|state| {
                        state.schedule.logical_run_id == entry.logical_run_id
                            && state.schedule.failed_physical_attempt_id
                                == entry.last_physical_attempt_id
                    })
                    .last();
                if let Some(state) = matching
                    && state.exhausted.replace(entry).is_some()
                {
                    bail!("provider-turn recovery exhausted more than once");
                }
            }
            Some(DurableEventType::ProviderTurnPartialOutputDiscarded) => {
                let entry: ProviderTurnPartialOutputDiscardedEntryV1 =
                    decode_recovery_payload(event)?;
                validate_partial_output_discarded(&entry)?;
                if self
                    .discarded_partials
                    .insert(entry.physical_attempt_id.clone(), entry)
                    .is_some()
                {
                    bail!("provider partial output was discarded more than once");
                }
            }
            Some(_) | None => {}
        }
        self.cursor =
            Some(record.projection_cursor(PROVIDER_TURN_RECOVERY_PROJECTION_SCHEMA_VERSION));
        Ok(())
    }
}

/// Appends recovery authority under the session writer lock. The physical-attempt owner remains
/// responsible for dispatch; this type never owns a provider client or a task writer.
pub(crate) struct ProviderTurnRecoveryAudit;

impl ProviderTurnRecoveryAudit {
    pub(crate) async fn schedule(
        session: &Session,
        evidence: &ProviderTurnRecoveryEvidenceV1,
        budget_before: RecoveryBudgetProjectionV1,
        retry_after_ms: u64,
        policy: ProviderTurnRecoveryPolicyV1,
    ) -> Result<ProviderTurnRecoveryScheduledEntry> {
        let recovery_id = format!("provider-recovery-{}", Uuid::new_v4());
        let scheduled_at = unix_time_ms();
        let retry_kind =
            if evidence.failure.wire_state == crate::ProviderWireStateV1::ResponseStarted {
                ProviderTurnRecoveryRetryKindV1::PartialOutput
            } else {
                ProviderTurnRecoveryRetryKindV1::Transport
            };
        let entry = ProviderTurnRecoveryScheduledEntry {
            schema_version: PROVIDER_TURN_RECOVERY_SCHEMA_VERSION,
            recovery_id,
            logical_run_id: evidence.logical_run_id.clone(),
            failed_physical_attempt_id: evidence.failed_physical_attempt_id.clone(),
            next_physical_attempt_ordinal: budget_before.retry_count.saturating_add(2),
            request_envelope_digest: evidence.request_envelope_digest.clone(),
            source_frontier: evidence.source_frontier.clone(),
            failure_class: evidence.failure.class,
            retry_kind,
            not_before_unix_ms: scheduled_at.saturating_add(retry_after_ms),
            retry_after_ms,
            budget_snapshot: RecoveryBudgetProjectionV1 {
                retry_count: budget_before.retry_count.saturating_add(1),
                max_transport_retries: policy.max_transport_retries,
                partial_output_retry_count: budget_before
                    .partial_output_retry_count
                    .saturating_add(u32::from(matches!(
                        retry_kind,
                        ProviderTurnRecoveryRetryKindV1::PartialOutput
                    ))),
                max_partial_output_retries: policy.max_partial_output_retries,
                cumulative_delay_ms: budget_before
                    .cumulative_delay_ms
                    .saturating_add(retry_after_ms),
                max_cumulative_delay_ms: policy.max_cumulative_delay_ms,
            },
            recovery_policy_fingerprint: policy.fingerprint(),
        };
        validate_schedule(&entry)?;
        let guard_entry = entry.clone();
        append_recovery_event(
            session,
            DurableEventType::ProviderTurnRecoveryScheduled,
            &entry,
            move |records| {
                let attempts = ProviderPhysicalAttemptProjection::from_records(records)?;
                let attempt = attempts
                    .attempt(&guard_entry.failed_physical_attempt_id)
                    .context("provider-turn recovery schedule references no physical attempt")?;
                let terminal = attempt.terminal.as_ref().context(
                    "provider-turn recovery schedule requires a terminal physical attempt",
                )?;
                if attempt.entry.logical_run_id != guard_entry.logical_run_id
                    || terminal.outcome == ProviderPhysicalAttemptOutcome::Completed
                {
                    bail!(
                        "provider-turn recovery schedule does not match a failed physical attempt"
                    );
                }
                let projection = ProviderTurnRecoveryProjection::from_records(records)?;
                if projection.recovery(&guard_entry.recovery_id).is_some()
                    || projection
                        .recoveries_for_logical_run_id(&guard_entry.logical_run_id)
                        .iter()
                        .any(|state| {
                            state.schedule.failed_physical_attempt_id
                                == guard_entry.failed_physical_attempt_id
                                && state.started.is_none()
                                && state.exhausted.is_none()
                        })
                {
                    bail!("provider-turn recovery schedule already exists for this failed attempt");
                }
                Ok(true)
            },
        )
        .await?;
        Ok(entry)
    }

    pub(crate) async fn start(
        session: &Session,
        schedule: &ProviderTurnRecoveryScheduledEntry,
    ) -> Result<ProviderTurnRecoveryStartedEntry> {
        let entry = ProviderTurnRecoveryStartedEntry {
            schema_version: PROVIDER_TURN_RECOVERY_SCHEMA_VERSION,
            recovery_id: schedule.recovery_id.clone(),
            logical_run_id: schedule.logical_run_id.clone(),
            physical_attempt_id: new_provider_physical_attempt_id(),
            started_at_unix_ms: unix_time_ms(),
        };
        validate_started(&entry)?;
        let guard_entry = entry.clone();
        let expected_schedule = schedule.clone();
        append_recovery_event(
            session,
            DurableEventType::ProviderTurnRecoveryStarted,
            &entry,
            move |records| {
                let projection = ProviderTurnRecoveryProjection::from_records(records)?;
                let state = projection
                    .recovery(&guard_entry.recovery_id)
                    .context("provider-turn recovery start references no schedule")?;
                if state.schedule != expected_schedule
                    || state.started.is_some()
                    || state.exhausted.is_some()
                {
                    bail!("provider-turn recovery start does not own its schedule");
                }
                if unix_time_ms() < state.schedule.not_before_unix_ms {
                    bail!("provider-turn recovery start is before its durable backoff deadline");
                }
                if ProviderPhysicalAttemptProjection::from_records(records)?
                    .attempt(&guard_entry.physical_attempt_id)
                    .is_some()
                {
                    bail!("provider-turn recovery physical attempt id already exists");
                }
                Ok(true)
            },
        )
        .await?;
        Ok(entry)
    }

    /// Selects one provider-owned equivalent transport under the recovery schedule writer lock.
    /// Provider activation happens later, after this append succeeds, so a process loss can never
    /// produce a hidden alternate-transport dispatch.
    pub(crate) async fn select_transport_fallback(
        session: &Session,
        schedule: &ProviderTurnRecoveryScheduledEntry,
        candidate: ProviderTransportFallbackCandidateV1,
    ) -> Result<ProviderTurnTransportFallbackSelectedEntryV1> {
        let entry = ProviderTurnTransportFallbackSelectedEntryV1 {
            schema_version: PROVIDER_TURN_RECOVERY_SCHEMA_VERSION,
            recovery_id: schedule.recovery_id.clone(),
            logical_run_id: schedule.logical_run_id.clone(),
            failed_physical_attempt_id: schedule.failed_physical_attempt_id.clone(),
            request_envelope_digest: schedule.request_envelope_digest.clone(),
            candidate,
            selected_at_unix_ms: unix_time_ms(),
        };
        validate_transport_fallback_selected(&entry)?;
        let expected_schedule = schedule.clone();
        let guard_entry = entry.clone();
        append_recovery_event(
            session,
            DurableEventType::ProviderTurnTransportFallbackSelected,
            &entry,
            move |records| {
                let projection = ProviderTurnRecoveryProjection::from_records(records)?;
                let state = projection
                    .recovery(&guard_entry.recovery_id)
                    .context("provider transport fallback references no recovery schedule")?;
                if state.schedule != expected_schedule
                    || state.transport_fallback.is_some()
                    || state.started.is_some()
                    || state.exhausted.is_some()
                {
                    bail!("provider transport fallback does not own its recovery schedule");
                }
                let attempts = ProviderPhysicalAttemptProjection::from_records(records)?;
                let attempt = attempts
                    .attempt(&guard_entry.failed_physical_attempt_id)
                    .context("provider transport fallback references no failed physical attempt")?;
                if attempt.entry.logical_run_id != guard_entry.logical_run_id
                    || attempt.terminal.is_none()
                {
                    bail!("provider transport fallback does not match a terminal physical attempt");
                }
                Ok(true)
            },
        )
        .await?;
        Ok(entry)
    }

    pub(crate) async fn exhaust(
        session: &Session,
        evidence: &ProviderTurnRecoveryEvidenceV1,
        budget: RecoveryBudgetProjectionV1,
        disposition: ProviderTurnRecoveryTerminalDispositionV1,
        reason_code: &'static str,
    ) -> Result<ProviderTurnRecoveryExhaustedEntry> {
        let entry = ProviderTurnRecoveryExhaustedEntry {
            schema_version: PROVIDER_TURN_RECOVERY_SCHEMA_VERSION,
            logical_run_id: evidence.logical_run_id.clone(),
            last_physical_attempt_id: evidence.failed_physical_attempt_id.clone(),
            reason_code: reason_code.to_owned(),
            budget_snapshot: budget,
            terminal_disposition: disposition,
        };
        validate_exhausted(&entry)?;
        let guard_entry = entry.clone();
        append_recovery_event(
            session,
            DurableEventType::ProviderTurnRecoveryExhausted,
            &entry,
            move |records| {
                let attempts = ProviderPhysicalAttemptProjection::from_records(records)?;
                let attempt = attempts
                    .attempt(&guard_entry.last_physical_attempt_id)
                    .context("provider-turn recovery terminal references no physical attempt")?;
                if attempt.entry.logical_run_id != guard_entry.logical_run_id
                    || attempt.terminal.is_none()
                {
                    bail!(
                        "provider-turn recovery terminal does not match a terminal physical attempt"
                    );
                }
                if ProviderTurnRecoveryProjection::from_records(records)?
                    .terminal_for_logical_run_id(&guard_entry.logical_run_id)
                    .is_some()
                {
                    bail!("provider-turn recovery logical turn is already terminal");
                }
                Ok(true)
            },
        )
        .await?;
        Ok(entry)
    }

    /// Closes an already-scheduled recovery when restart repair proves that it cannot safely be
    /// dispatched. This is deliberately separate from `exhaust`: after process loss the original
    /// typed error object is no longer available, while the schedule itself remains the durable
    /// authority and budget source.
    pub(crate) async fn exhaust_scheduled(
        session: &Session,
        schedule: &ProviderTurnRecoveryScheduledEntry,
        require_started: bool,
        disposition: ProviderTurnRecoveryTerminalDispositionV1,
        reason_code: &'static str,
    ) -> Result<ProviderTurnRecoveryExhaustedEntry> {
        let entry = ProviderTurnRecoveryExhaustedEntry {
            schema_version: PROVIDER_TURN_RECOVERY_SCHEMA_VERSION,
            logical_run_id: schedule.logical_run_id.clone(),
            last_physical_attempt_id: schedule.failed_physical_attempt_id.clone(),
            reason_code: reason_code.to_owned(),
            budget_snapshot: schedule.budget_snapshot,
            terminal_disposition: disposition,
        };
        validate_exhausted(&entry)?;
        let guard_schedule = schedule.clone();
        let guard_entry = entry.clone();
        append_recovery_event(
            session,
            DurableEventType::ProviderTurnRecoveryExhausted,
            &entry,
            move |records| {
                let projection = ProviderTurnRecoveryProjection::from_records(records)?;
                let state = projection
                    .recovery(&guard_schedule.recovery_id)
                    .context("provider-turn recovery repair references no schedule")?;
                if state.schedule != guard_schedule
                    || state.exhausted.is_some()
                    || state.started.is_some() != require_started
                    || projection
                        .terminal_for_logical_run_id(&guard_entry.logical_run_id)
                        .is_some()
                {
                    bail!("provider-turn recovery repair no longer owns its schedule");
                }
                let attempts = ProviderPhysicalAttemptProjection::from_records(records)?;
                let predecessor = attempts
                    .attempt(&guard_entry.last_physical_attempt_id)
                    .context("provider-turn recovery repair predecessor is missing")?;
                if predecessor.entry.logical_run_id != guard_entry.logical_run_id
                    || predecessor.terminal.is_none()
                {
                    bail!("provider-turn recovery repair predecessor is not terminal");
                }
                if let Some(started) = &state.started
                    && let Some(successor) = attempts.attempt(&started.physical_attempt_id)
                    && successor.terminal.is_none()
                {
                    bail!("provider-turn recovery repair must close an unfinished successor first");
                }
                Ok(true)
            },
        )
        .await?;
        Ok(entry)
    }

    /// Persists only the shape of discarded live stream data after its physical attempt has
    /// reached a durable failure terminal. An in-memory session still receives the replacement
    /// signal, but cannot claim durable cross-restart sidecar authority.
    pub(crate) async fn discard_partial_output(
        session: &Session,
        logical_run_id: &str,
        physical_attempt_id: &ProviderPhysicalAttemptId,
        text_bytes: usize,
        reasoning_bytes: usize,
        streamed_tool_call_count: usize,
    ) -> Result<Option<ProviderTurnPartialOutputDiscardedEntryV1>> {
        if text_bytes == 0 && reasoning_bytes == 0 && streamed_tool_call_count == 0 {
            return Ok(None);
        }
        if session.durable_store().is_none() {
            return Ok(None);
        }
        let entry = ProviderTurnPartialOutputDiscardedEntryV1 {
            schema_version: PROVIDER_TURN_RECOVERY_SCHEMA_VERSION,
            logical_run_id: logical_run_id.to_owned(),
            physical_attempt_id: physical_attempt_id.clone(),
            text_bytes: text_bytes.try_into().unwrap_or(u32::MAX),
            reasoning_bytes: reasoning_bytes.try_into().unwrap_or(u32::MAX),
            streamed_tool_call_count: streamed_tool_call_count.try_into().unwrap_or(u16::MAX),
        };
        validate_partial_output_discarded(&entry)?;
        let guard_entry = entry.clone();
        append_recovery_event(
            session,
            DurableEventType::ProviderTurnPartialOutputDiscarded,
            &entry,
            move |records| {
                let attempts = ProviderPhysicalAttemptProjection::from_records(records)?;
                let attempt = attempts
                    .attempt(&guard_entry.physical_attempt_id)
                    .context("provider partial-output sidecar references no physical attempt")?;
                if attempt.entry.logical_run_id != guard_entry.logical_run_id
                    || attempt.terminal.as_ref().is_none_or(|terminal| {
                        terminal.outcome == ProviderPhysicalAttemptOutcome::Completed
                    })
                {
                    bail!("provider partial-output sidecar requires a failed terminal attempt");
                }
                if ProviderTurnRecoveryProjection::from_records(records)?
                    .discarded_partial_for_physical_attempt(&guard_entry.physical_attempt_id)
                    .is_some()
                {
                    bail!(
                        "provider partial-output sidecar already exists for this physical attempt"
                    );
                }
                Ok(true)
            },
        )
        .await?;
        Ok(Some(entry))
    }
}

async fn append_recovery_event<T, F>(
    session: &Session,
    event_type: DurableEventType,
    entry: &T,
    guard: F,
) -> Result<()>
where
    T: Serialize,
    F: FnOnce(&[SessionStreamRecord]) -> Result<bool> + Send + 'static,
{
    let Some(store) = session.durable_store() else {
        bail!("provider-turn recovery requires a durable session store");
    };
    let payload =
        serde_json::to_value(entry).context("failed to encode provider-turn recovery event")?;
    let event_id = Uuid::new_v4().to_string();
    tokio::task::spawn_blocking(move || {
        store
            .append_event_if_with_identity(event_type, payload, event_id, None, None, guard)
            .and_then(|event| {
                event.context("provider-turn recovery durable append was not attempted")
            })
            .map(|_| ())
    })
    .await
    .context("provider-turn recovery durable append task failed")?
}

fn decode_recovery_payload<T>(event: &StoredEvent) -> Result<T>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_value(event.payload.clone()).with_context(|| {
        format!(
            "failed to decode {} provider-turn recovery payload",
            event.event_type
        )
    })
}

pub(crate) fn validate_schedule(entry: &ProviderTurnRecoveryScheduledEntry) -> Result<()> {
    if entry.schema_version != PROVIDER_TURN_RECOVERY_SCHEMA_VERSION
        || entry.recovery_id.trim().is_empty()
        || entry.logical_run_id.trim().is_empty()
        || entry.failed_physical_attempt_id.trim().is_empty()
        || !is_sha256_digest(&entry.request_envelope_digest)
        || entry.recovery_policy_fingerprint.is_empty()
        || entry.next_physical_attempt_ordinal < 2
        || entry.retry_after_ms > entry.budget_snapshot.max_cumulative_delay_ms
    {
        bail!("provider-turn recovery schedule is malformed");
    }
    Ok(())
}

fn is_sha256_digest(value: &str) -> bool {
    value.len() == "sha256:".len() + 64
        && value.starts_with("sha256:")
        && value["sha256:".len()..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
}

pub(crate) fn validate_started(entry: &ProviderTurnRecoveryStartedEntry) -> Result<()> {
    if entry.schema_version != PROVIDER_TURN_RECOVERY_SCHEMA_VERSION
        || entry.recovery_id.trim().is_empty()
        || entry.logical_run_id.trim().is_empty()
        || entry.physical_attempt_id.trim().is_empty()
    {
        bail!("provider-turn recovery start is malformed");
    }
    Ok(())
}

pub(crate) fn validate_transport_fallback_selected(
    entry: &ProviderTurnTransportFallbackSelectedEntryV1,
) -> Result<()> {
    if entry.schema_version != PROVIDER_TURN_RECOVERY_SCHEMA_VERSION
        || entry.recovery_id.trim().is_empty()
        || entry.logical_run_id.trim().is_empty()
        || entry.failed_physical_attempt_id.trim().is_empty()
        || !is_sha256_digest(&entry.request_envelope_digest)
    {
        bail!("provider transport fallback selection is malformed");
    }
    entry.candidate.validate()
}

pub(crate) fn validate_exhausted(entry: &ProviderTurnRecoveryExhaustedEntry) -> Result<()> {
    if entry.schema_version != PROVIDER_TURN_RECOVERY_SCHEMA_VERSION
        || entry.logical_run_id.trim().is_empty()
        || entry.last_physical_attempt_id.trim().is_empty()
        || entry.reason_code.trim().is_empty()
    {
        bail!("provider-turn recovery terminal is malformed");
    }
    Ok(())
}

pub(crate) fn validate_partial_output_discarded(
    entry: &ProviderTurnPartialOutputDiscardedEntryV1,
) -> Result<()> {
    if entry.schema_version != PROVIDER_TURN_RECOVERY_SCHEMA_VERSION
        || entry.logical_run_id.trim().is_empty()
        || entry.physical_attempt_id.trim().is_empty()
        || (entry.text_bytes == 0
            && entry.reasoning_bytes == 0
            && entry.streamed_tool_call_count == 0)
    {
        bail!("provider partial-output sidecar is malformed");
    }
    Ok(())
}

fn unix_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
#[path = "tests/provider_turn_recovery_tests.rs"]
mod tests;
