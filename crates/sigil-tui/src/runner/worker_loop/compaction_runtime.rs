use anyhow::{Context, Result, bail};
use sigil_kernel::session::StableCompactionSnapshot;
use sigil_kernel::{
    CompactionCircuitBreakerDecisionV1, CompactionCircuitBreakerInputV1, CompactionCircuitScopeV1,
    CompactionConfig, CompactionEmergencyBlockingLayerV1, CompactionForecastConfidenceV1,
    CompactionForecastSourceV1, CompactionInitiation, CompactionRolloutModeV1, CompactionStrategy,
    CompactionThresholdStatus, ControlEntry, ExpectedRemainingTurnsV1,
    FrozenProviderRequestMaterial, PortableSemanticCompactionOutcome,
    PortableSemanticCompactionPreflight, PortableSemanticCompactionRequest,
    PortableTargetRequestMaterial, ProviderContextCapabilities, ProviderNonGeneratingAttempt,
    ProviderNonGeneratingAttemptReceipt, ProviderPhysicalAttemptOutcome,
    ProviderPhysicalAttemptPurpose, ProviderRequestRejection, RuntimeContextCandidates,
    ToolOutputProjectionPolicy, V2CompactionPreview,
};

use super::{
    AdmittedQueuedConversationCandidate, AgentRunOptions, DEFAULT_TASK_VERIFICATION_SCOPE_HASH,
    ExactConversationPromptStore, JsonlSessionStore, PreparedQueuedConversationCandidate,
    QueuedConversationPressureAdmission, RootConfig, Session, build_workspace_snapshot,
    current_unix_time_ms, stable_event_uuid, stable_workspace_id,
};
use crate::runner::protocol::{
    ToolOutputShrinkPreview, V2CompactionAdmission, V2CompactionReview, V2ConstraintPreview,
    V2ContinuityPreview,
};

const IDLE_AUTO_COMPACTION_COOLDOWN_MS: u64 = 60_000;
const IDLE_AUTO_COMPACTION_PREFLIGHT_EVIDENCE_SCHEMA_VERSION: u32 = 1;

/// Scheduler-owned state required to decide whether idle automatic compaction may be considered.
///
/// This snapshot deliberately contains no path, transcript, prompt, store or task-manager handle.
/// It lets the worker reject an ineligible request before it creates a background preparation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::runner) struct IdleAutoCompactionSchedulerEligibility {
    pub(in crate::runner) run_active: bool,
    pub(in crate::runner) conversation_queue_idle: bool,
    pub(in crate::runner) pending_agent_result_continuation: bool,
    pub(in crate::runner) pending_compaction: bool,
    pub(in crate::runner) preparation_active: bool,
}

impl IdleAutoCompactionSchedulerEligibility {
    #[cfg(test)]
    fn idle() -> Self {
        Self {
            run_active: false,
            conversation_queue_idle: true,
            pending_agent_result_continuation: false,
            pending_compaction: false,
            preparation_active: false,
        }
    }

    fn blocked_reason(self) -> Option<IdleAutoCompactionSchedulerBlockReason> {
        if self.run_active {
            Some(IdleAutoCompactionSchedulerBlockReason::ActiveRun)
        } else if !self.conversation_queue_idle {
            Some(IdleAutoCompactionSchedulerBlockReason::ConversationQueue)
        } else if self.pending_agent_result_continuation {
            Some(IdleAutoCompactionSchedulerBlockReason::AgentResultContinuation)
        } else if self.pending_compaction {
            Some(IdleAutoCompactionSchedulerBlockReason::PendingCompaction)
        } else if self.preparation_active {
            Some(IdleAutoCompactionSchedulerBlockReason::PreparationActive)
        } else {
            None
        }
    }
}

/// Why the worker cannot yet evaluate one requested idle automatic compaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::runner) enum IdleAutoCompactionSchedulerBlockReason {
    ActiveRun,
    ConversationQueue,
    AgentResultContinuation,
    PendingCompaction,
    PreparationActive,
    SessionUnavailable,
}

/// A pure-memory reason that consumes the current post-run request without starting preparation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::runner) enum IdleAutoCompactionNotEligibleReason {
    CompactionDisabled,
    ContextWindowUnavailable,
    ProviderCapabilityUnavailable,
    NotFitRequired,
}

/// Cheap decision made before any path load or background preparation is created.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::runner) enum IdleAutoCompactionPreflightDecision {
    NotRequested,
    SchedulerBlocked(IdleAutoCompactionSchedulerBlockReason),
    NotEligible(IdleAutoCompactionNotEligibleReason),
    ProceedToDetailedPreparation {
        effective_strategy: CompactionStrategy,
    },
}

/// Per-evaluation evidence delta for cheap idle-compaction preflight.
///
/// These counters describe only this pure decision. They intentionally do not claim durable reads,
/// file-lock attempts, worker wakes, projection rebuilds or preparation starts, because those
/// operations are outside this function and require their own integration instrumentation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::runner) struct IdleAutoCompactionPreflightEvidenceV1 {
    pub(in crate::runner) schema_version: u32,
    pub(in crate::runner) evaluation_count: u64,
    pub(in crate::runner) not_requested_count: u64,
    pub(in crate::runner) scheduler_blocked_count: u64,
    pub(in crate::runner) not_eligible_count: u64,
    pub(in crate::runner) detailed_preparation_candidate_count: u64,
    pub(in crate::runner) prompt_tokens: Option<u64>,
    pub(in crate::runner) context_window_tokens: Option<u64>,
    pub(in crate::runner) threshold_status: Option<CompactionThresholdStatus>,
    pub(in crate::runner) configured_strategy: CompactionStrategy,
    pub(in crate::runner) effective_strategy: Option<CompactionStrategy>,
    pub(in crate::runner) cache_aware_v3_supported: Option<bool>,
}

/// Pure-memory result returned before idle automatic compaction can load or spawn anything.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::runner) struct IdleAutoCompactionPreflight {
    pub(in crate::runner) decision: IdleAutoCompactionPreflightDecision,
    pub(in crate::runner) evidence: IdleAutoCompactionPreflightEvidenceV1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct IdleAutoCompactionDurableAdmission {
    failure_latched: bool,
    circuit_decision: CompactionCircuitBreakerDecisionV1,
}

/// Captures the live session projection for idle compaction only when it still exactly matches
/// the current durable session-entry frontier.
///
/// The first active-projection read selects the frontier; `stable_compaction_snapshot` performs
/// the compare-and-snapshot check against that exact frontier. Durable lifecycle-only records may
/// advance later, but an externally appended session entry invalidates this snapshot.
pub(in crate::runner) fn capture_stable_idle_compaction_snapshot(
    session: &Session,
) -> Result<Option<StableCompactionSnapshot>> {
    let Some(active) = session.active_projection_snapshot()? else {
        return Ok(None);
    };
    session.stable_compaction_snapshot(active.frontier())
}

fn idle_auto_compaction_durable_admission(
    session: &Session,
    scope_fingerprint: &str,
    circuit_scope: CompactionCircuitScopeV1,
    emergency: bool,
    emergency_blocking_layer: CompactionEmergencyBlockingLayerV1,
) -> Result<IdleAutoCompactionDurableAdmission> {
    let active = session
        .active_projection_snapshot()?
        .context("eligible automatic compaction requires a durable active projection")?;
    let compaction = active.compaction();
    let post_activation_emergency_layer = (compaction.latest_applied_stream_sequence().is_some()
        && compaction.completed_real_turns_since_latest_applied() == 1
        && emergency)
        .then_some(emergency_blocking_layer);
    let circuit_decision =
        compaction.circuit_breaker_decision(&CompactionCircuitBreakerInputV1 {
            scope: circuit_scope,
            latest_completed_real_turn_sequence: compaction.latest_completed_real_turn_sequence(),
            emergency,
            post_activation_emergency_layer,
            manual_retry: false,
        })?;
    Ok(IdleAutoCompactionDurableAdmission {
        failure_latched: compaction.has_failed_idle_automatic_scope(scope_fingerprint),
        circuit_decision,
    })
}

/// Process-local post-run policy state for the deliberately narrow K25.11 automation path.
///
/// The only durable suppression is a failed initiated lifecycle keyed by its scope fingerprint.
/// This short cooldown is intentionally local: no admission means no compaction attempt and no
/// session mutation. A future successful provider turn may retry after the cooldown expires.
#[derive(Clone, Debug, Default)]
pub(in crate::runner) struct IdleAutoCompactionState {
    requested_after_run: bool,
    cooldown: Option<IdleAutoCompactionCooldown>,
}

#[derive(Clone, Debug)]
struct IdleAutoCompactionCooldown {
    scope_fingerprint: String,
    retry_after_unix_ms: u64,
}

impl IdleAutoCompactionState {
    pub(in crate::runner) fn request_after_successful_chat_run(&mut self) {
        self.requested_after_run = true;
    }

    pub(in crate::runner) fn cancel_requested_run(&mut self) {
        self.requested_after_run = false;
    }

    pub(in crate::runner) fn is_requested(&self) -> bool {
        self.requested_after_run
    }

    fn consume_request(&mut self) {
        self.requested_after_run = false;
    }

    fn retry_after(&self, scope_fingerprint: &str) -> Option<u64> {
        self.cooldown.as_ref().and_then(|cooldown| {
            (cooldown.scope_fingerprint == scope_fingerprint)
                .then_some(cooldown.retry_after_unix_ms)
        })
    }

    fn set_cooldown(&mut self, scope_fingerprint: String, now_unix_ms: u64) {
        self.cooldown = Some(IdleAutoCompactionCooldown {
            scope_fingerprint,
            retry_after_unix_ms: now_unix_ms.saturating_add(IDLE_AUTO_COMPACTION_COOLDOWN_MS),
        });
    }
}

/// Performs the first idle automatic-compaction gate using process-local state only.
///
/// The result is not final compaction admission. `ProceedToDetailedPreparation` means only that
/// the caller may create the existing detailed preparation, which still owns foldability,
/// fit/economics, circuit-breaker and exact-target validation. Every `NotEligible` result consumes
/// the current post-run request at the caller and must not load the session path or start a task.
#[must_use]
pub(in crate::runner) fn idle_auto_compaction_preflight(
    state: &IdleAutoCompactionState,
    session: Option<&Session>,
    configured_compaction: &CompactionConfig,
    provider_context_capabilities: &ProviderContextCapabilities,
    scheduler: IdleAutoCompactionSchedulerEligibility,
) -> IdleAutoCompactionPreflight {
    let mut evidence = IdleAutoCompactionPreflightEvidenceV1 {
        schema_version: IDLE_AUTO_COMPACTION_PREFLIGHT_EVIDENCE_SCHEMA_VERSION,
        evaluation_count: 1,
        not_requested_count: 0,
        scheduler_blocked_count: 0,
        not_eligible_count: 0,
        detailed_preparation_candidate_count: 0,
        prompt_tokens: None,
        context_window_tokens: None,
        threshold_status: None,
        configured_strategy: configured_compaction.strategy,
        effective_strategy: None,
        cache_aware_v3_supported: None,
    };

    if !state.is_requested() {
        return idle_auto_compaction_preflight_result(
            IdleAutoCompactionPreflightDecision::NotRequested,
            evidence,
        );
    }
    if let Some(reason) = scheduler.blocked_reason() {
        return idle_auto_compaction_preflight_result(
            IdleAutoCompactionPreflightDecision::SchedulerBlocked(reason),
            evidence,
        );
    }
    let Some(session) = session else {
        return idle_auto_compaction_preflight_result(
            IdleAutoCompactionPreflightDecision::SchedulerBlocked(
                IdleAutoCompactionSchedulerBlockReason::SessionUnavailable,
            ),
            evidence,
        );
    };

    let cache_aware_v3_supported = Some(sigil_runtime::cache_aware_v3_automatic_supported(
        session.provider_name(),
        session.model_name(),
        provider_context_capabilities,
    ));
    let effective_compaction = configured_compaction.clone();
    let prompt_tokens = session.stats().last_prompt_tokens;
    let threshold_status = effective_compaction.threshold_status(prompt_tokens);
    evidence.prompt_tokens = Some(prompt_tokens);
    evidence.context_window_tokens = effective_compaction.context_window_tokens.map(u64::from);
    evidence.threshold_status = Some(threshold_status);
    evidence.effective_strategy = Some(effective_compaction.strategy);
    evidence.cache_aware_v3_supported = cache_aware_v3_supported;

    let decision = if cache_aware_v3_supported == Some(false) {
        IdleAutoCompactionPreflightDecision::NotEligible(
            IdleAutoCompactionNotEligibleReason::ProviderCapabilityUnavailable,
        )
    } else {
        match threshold_status {
            CompactionThresholdStatus::Off => IdleAutoCompactionPreflightDecision::NotEligible(
                IdleAutoCompactionNotEligibleReason::CompactionDisabled,
            ),
            CompactionThresholdStatus::NotAvailable => {
                IdleAutoCompactionPreflightDecision::NotEligible(
                    IdleAutoCompactionNotEligibleReason::ContextWindowUnavailable,
                )
            }
            CompactionThresholdStatus::Ready => IdleAutoCompactionPreflightDecision::NotEligible(
                IdleAutoCompactionNotEligibleReason::NotFitRequired,
            ),
            CompactionThresholdStatus::Soft | CompactionThresholdStatus::Hard => {
                IdleAutoCompactionPreflightDecision::ProceedToDetailedPreparation {
                    effective_strategy: effective_compaction.strategy,
                }
            }
        }
    };
    idle_auto_compaction_preflight_result(decision, evidence)
}

fn idle_auto_compaction_preflight_result(
    decision: IdleAutoCompactionPreflightDecision,
    mut evidence: IdleAutoCompactionPreflightEvidenceV1,
) -> IdleAutoCompactionPreflight {
    match decision {
        IdleAutoCompactionPreflightDecision::NotRequested => {
            evidence.not_requested_count = 1;
        }
        IdleAutoCompactionPreflightDecision::SchedulerBlocked(_) => {
            evidence.scheduler_blocked_count = 1;
        }
        IdleAutoCompactionPreflightDecision::NotEligible(_) => {
            evidence.not_eligible_count = 1;
        }
        IdleAutoCompactionPreflightDecision::ProceedToDetailedPreparation { .. } => {
            evidence.detailed_preparation_candidate_count = 1;
        }
    }
    IdleAutoCompactionPreflight { decision, evidence }
}

/// Result of checking the idle-only automatic compaction policy after a completed chat run.
pub(in crate::runner) enum IdleAutoCompactionPreparation {
    NotRequested,
    NotHardThreshold,
    NoFoldableHistory,
    FailureLatched,
    CircuitOpen {
        decision: CompactionCircuitBreakerDecisionV1,
    },
    CoolingDown {
        retry_after_unix_ms: u64,
    },
    AdmissionUnavailable {
        reason: String,
    },
    Ready(Box<PendingV2Compaction>),
}

/// Process-local admission state kept between a confirmed `/compact` review and its apply.
///
/// It intentionally retains the frozen request only in memory. The durable checkpoint receives
/// just the session-bound fingerprint and proof through the K25.9 executor.
pub(in crate::runner) struct PendingV2Compaction {
    request_id: u64,
    session_scope_id: String,
    idle_auto_scope_fingerprint: Option<String>,
    deterministic_emergency_fallback: bool,
    source_preview: V2CompactionPreview,
    preflight: PortableSemanticCompactionPreflight,
    target_material: PortableTargetRequestMaterial,
    economics_v2_input: sigil_runtime::PortableCompactionEconomicsV2Input,
    folded_event_count: usize,
    native_carrier: PendingNativeCarrier,
}

/// Exact zero-provider-I/O plan retained between `/compact` and the user's choice.
///
/// It binds the durable cursor and session scope but carries no generated summary. Confirming a
/// full compaction consumes this plan and starts the semantic-summary stage; choosing standalone
/// shrink applies only its deterministic large-tool projection.
pub(in crate::runner) struct PendingLocalV2Compaction {
    request_id: u64,
    session_scope_id: String,
    preview: V2CompactionPreview,
}

impl PendingLocalV2Compaction {
    pub(in crate::runner) fn request_id(&self) -> u64 {
        self.request_id
    }

    pub(in crate::runner) fn session_scope_id(&self) -> &str {
        &self.session_scope_id
    }

    pub(in crate::runner) fn preview(&self) -> &V2CompactionPreview {
        &self.preview
    }
}

struct PendingNativeCarrier {
    frozen_request: FrozenProviderRequestMaterial,
    covers_through: sigil_kernel::CompactionCursor,
    portable_compaction_id: sigil_kernel::CompactionId,
}

/// Exact process-local material prepared for a portable V2 activation before its target proof is
/// handed to the durable executor. The frozen request stays private to the worker and is never
/// rendered or persisted.
struct PreparedPortableV2Compaction {
    request_id: u64,
    session_scope_id: String,
    idle_auto_scope_fingerprint: Option<String>,
    deterministic_emergency_fallback: bool,
    source_preview: V2CompactionPreview,
    cache_root: std::path::PathBuf,
    preflight: PortableSemanticCompactionPreflight,
    frozen_before_request: FrozenProviderRequestMaterial,
    frozen_target_request: FrozenProviderRequestMaterial,
    native_covers_through: sigil_kernel::CompactionCursor,
    native_portable_compaction_id: sigil_kernel::CompactionId,
    economics_v2_input: sigil_runtime::PortableCompactionEconomicsV2Input,
    folded_event_count: usize,
}

impl PreparedPortableV2Compaction {
    fn into_pending(self) -> Result<PendingV2Compaction> {
        let target_material =
            sigil_runtime::deepseek_v4_flash_portable_target_material_with_economics_v2_candidate(
                &self.cache_root,
                &self.frozen_before_request,
                self.frozen_target_request,
            )?;
        let target_material = sigil_runtime::attach_portable_compaction_economics_v2(
            target_material,
            self.economics_v2_input.clone(),
        )?;
        require_prepared_v2_admission(&target_material, self.economics_v2_input.rollout_mode)?;
        Ok(PendingV2Compaction {
            request_id: self.request_id,
            session_scope_id: self.session_scope_id,
            idle_auto_scope_fingerprint: self.idle_auto_scope_fingerprint,
            deterministic_emergency_fallback: self.deterministic_emergency_fallback,
            source_preview: self.source_preview,
            preflight: self.preflight,
            target_material,
            economics_v2_input: self.economics_v2_input,
            folded_event_count: self.folded_event_count,
            native_carrier: PendingNativeCarrier {
                frozen_request: self.frozen_before_request,
                covers_through: self.native_covers_through,
                portable_compaction_id: self.native_portable_compaction_id,
            },
        })
    }

    async fn into_server_count_pending<P>(
        mut self,
        provider: &P,
        session: &Session,
        source_physical_attempt_id: &str,
    ) -> Result<PendingV2Compaction>
    where
        P: sigil_kernel::Provider,
    {
        let (before_material, before_receipt) = measure_portable_request_input(
            provider,
            session,
            format!("overflow-before-input-token-measurement:{source_physical_attempt_id}"),
            self.frozen_before_request.clone(),
            "pre-compaction overflow request",
        )
        .await?;
        let before_input = before_material.proof().input.clone();
        self.preflight.admit_completed_input_token_measurement(
            before_receipt,
            before_material.frozen_request().fingerprint(),
        )?;

        let (target_material, target_receipt) = measure_portable_request_input(
            provider,
            session,
            format!("overflow-target-input-token-measurement:{source_physical_attempt_id}"),
            self.frozen_target_request,
            "post-compaction overflow target",
        )
        .await?;
        self.preflight.admit_completed_input_token_measurement(
            target_receipt,
            target_material.frozen_request().fingerprint(),
        )?;
        let target_material = target_material
            .with_portable_economics_v2_candidate(&self.frozen_before_request, before_input)?;
        let target_material = sigil_runtime::attach_portable_compaction_economics_v2(
            target_material,
            self.economics_v2_input.clone(),
        )?;
        require_prepared_v2_admission(&target_material, self.economics_v2_input.rollout_mode)?;
        Ok(PendingV2Compaction {
            request_id: self.request_id,
            session_scope_id: self.session_scope_id,
            idle_auto_scope_fingerprint: self.idle_auto_scope_fingerprint,
            deterministic_emergency_fallback: self.deterministic_emergency_fallback,
            source_preview: self.source_preview,
            preflight: self.preflight,
            target_material,
            economics_v2_input: self.economics_v2_input,
            folded_event_count: self.folded_event_count,
            native_carrier: PendingNativeCarrier {
                frozen_request: self.frozen_before_request,
                covers_through: self.native_covers_through,
                portable_compaction_id: self.native_portable_compaction_id,
            },
        })
    }
}

async fn measure_portable_request_input<P>(
    provider: &P,
    session: &Session,
    logical_run_id: String,
    frozen_request: FrozenProviderRequestMaterial,
    description: &str,
) -> Result<(
    PortableTargetRequestMaterial,
    ProviderNonGeneratingAttemptReceipt,
)>
where
    P: sigil_kernel::Provider,
{
    let mut measurement = ProviderNonGeneratingAttempt::start(
        session,
        &logical_run_id,
        &frozen_request,
        ProviderPhysicalAttemptPurpose::InputTokenMeasurement,
    )
    .await?;
    match provider
        .prove_portable_compaction_target(frozen_request)
        .await
    {
        Ok(target_material) => {
            measurement
                .finish(session, ProviderPhysicalAttemptOutcome::Completed)
                .await?;
            let receipt = measurement
                .completed_receipt()
                .cloned()
                .with_context(|| format!("{description} measurement has no durable receipt"))?;
            Ok((target_material, receipt))
        }
        Err(error) => {
            if let Err(terminal_error) = measurement
                .finish(
                    session,
                    ProviderPhysicalAttemptOutcome::TransportOutcomeUncertain,
                )
                .await
            {
                return Err(terminal_error.context(format!(
                    "{description} measurement failed after its durable start: {error:#}"
                )));
            }
            Err(error).with_context(|| format!("{description} measurement failed"))
        }
    }
}

/// A fully admitted pre-turn portable path whose post-compaction request is frozen in memory.
///
/// The contained queue promotion is still uncommitted. The scheduler must apply the independent
/// compaction CAS, append the promotion CAS, and commit capabilities before it can send this
/// exact request.
pub(in crate::runner) struct PendingQueuedConversationPortablePreflight {
    pub(in crate::runner) candidate: PreparedQueuedConversationCandidate,
    pending_compaction: PendingV2Compaction,
}

impl std::fmt::Debug for PendingQueuedConversationPortablePreflight {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PendingQueuedConversationPortablePreflight")
            .field("candidate", &self.candidate)
            .field(
                "folded_event_count",
                &self.pending_compaction.folded_event_count(),
            )
            .finish()
    }
}

impl PendingQueuedConversationPortablePreflight {
    /// Applies the independently reviewed portable lifecycle before queue promotion.
    ///
    /// A failure leaves the queue unpromoted. The caller must reload durable state before it can
    /// attempt the separate queue-revision CAS and before it ever hands the retained request to
    /// the provider path.
    pub(in crate::runner) fn apply_compaction<P>(
        self,
        session: &Session,
        session_log_path: &std::path::Path,
        provider: &P,
        runtime: &tokio::runtime::Runtime,
        native_carrier_enabled: bool,
    ) -> Result<(
        PreparedQueuedConversationCandidate,
        PortableSemanticCompactionOutcome,
        Option<String>,
    )>
    where
        P: sigil_kernel::Provider,
    {
        let (outcome, native_notice) = self.pending_compaction.apply_with_optional_native(
            session,
            session_log_path,
            provider,
            runtime,
            native_carrier_enabled,
        )?;
        Ok((self.candidate, outcome, native_notice))
    }

    pub(in crate::runner) fn folded_event_count(&self) -> usize {
        self.pending_compaction.folded_event_count()
    }
}

/// Complete no-write admission result for the next queued conversation input.
pub(in crate::runner) enum QueuedConversationPreTurnAdmission {
    NoQueuedInput,
    ExactFit(Box<AdmittedQueuedConversationCandidate>),
    PortablePreflightReady(Box<PendingQueuedConversationPortablePreflight>),
    Blocked {
        queue_id: sigil_kernel::ConversationInputQueueId,
        reason: String,
        candidate: Option<Box<PreparedQueuedConversationCandidate>>,
    },
}

impl std::fmt::Debug for QueuedConversationPreTurnAdmission {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoQueuedInput => {
                formatter.write_str("QueuedConversationPreTurnAdmission::NoQueuedInput")
            }
            Self::ExactFit(candidate) => formatter
                .debug_tuple("QueuedConversationPreTurnAdmission::ExactFit")
                .field(candidate)
                .finish(),
            Self::PortablePreflightReady(pending) => formatter
                .debug_tuple("QueuedConversationPreTurnAdmission::PortablePreflightReady")
                .field(pending)
                .finish(),
            Self::Blocked {
                queue_id,
                reason,
                candidate,
            } => formatter
                .debug_struct("QueuedConversationPreTurnAdmission::Blocked")
                .field("queue_id", queue_id)
                .field("reason", reason)
                .field("has_frozen_candidate", &candidate.is_some())
                .finish(),
        }
    }
}

impl PendingV2Compaction {
    pub(in crate::runner) fn request_id(&self) -> u64 {
        self.request_id
    }

    pub(in crate::runner) fn folded_event_count(&self) -> usize {
        self.folded_event_count
    }

    pub(in crate::runner) fn source_preview(&self) -> &V2CompactionPreview {
        &self.source_preview
    }

    pub(in crate::runner) fn idle_auto_scope_fingerprint(&self) -> Option<&str> {
        self.idle_auto_scope_fingerprint.as_deref()
    }

    pub(in crate::runner) fn frozen_target_request(&self) -> FrozenProviderRequestMaterial {
        self.target_material.frozen_request().clone()
    }

    pub(in crate::runner) fn apply_with_optional_native<P>(
        self,
        session: &Session,
        session_log_path: &std::path::Path,
        provider: &P,
        runtime: &tokio::runtime::Runtime,
        native_carrier_enabled: bool,
    ) -> Result<(PortableSemanticCompactionOutcome, Option<String>)>
    where
        P: sigil_kernel::Provider,
    {
        let emergency_fallback_notice = self.deterministic_emergency_fallback.then(|| {
            "semantic summary unavailable; compaction continued with the audited deterministic emergency continuity floor".to_owned()
        });
        let (outcome, native_carrier) = self.apply_portable(session, session_log_path)?;
        if !native_carrier_enabled
            || !sigil_runtime::application_compaction::NATIVE_COMPACTION_RESUME_ENABLED
        {
            return Ok((outcome, emergency_fallback_notice));
        }
        let logical_run_id = format!("native-carrier:{}", outcome.compaction_id);
        let native_result = runtime.block_on(sigil_runtime::materialize_native_compaction_carrier(
            provider,
            session,
            logical_run_id,
            native_carrier.frozen_request,
            native_carrier.covers_through,
            native_carrier.portable_compaction_id,
        ));
        let notice = match native_result {
            Ok(Some(_materialized)) => {
                Some("portable compaction applied; native carrier materialized".to_owned())
            }
            Ok(None) => Some(
                "portable compaction applied; provider-native threshold did not produce a carrier"
                    .to_owned(),
            ),
            Err(_error) => {
                Some("portable compaction applied; optional native carrier unavailable".to_owned())
            }
        };
        let notice = match (emergency_fallback_notice, notice) {
            (Some(emergency), Some(native)) => Some(format!("{emergency}; {native}")),
            (Some(emergency), None) => Some(emergency),
            (None, native) => native,
        };
        Ok((outcome, notice))
    }

    fn apply_portable(
        mut self,
        session: &Session,
        session_log_path: &std::path::Path,
    ) -> Result<(PortableSemanticCompactionOutcome, PendingNativeCarrier)> {
        if session.session_scope_id() != self.session_scope_id {
            bail!("reviewed V2 compaction belongs to a different session scope");
        }
        if self.economics_v2_input.rollout_mode == CompactionRolloutModeV1::Preview {
            self.economics_v2_input.user_confirmed = true;
            self.target_material = sigil_runtime::attach_portable_compaction_economics_v2(
                self.target_material,
                self.economics_v2_input.clone(),
            )?;
        }
        require_activation_v2_admission(
            &self.target_material,
            self.economics_v2_input.rollout_mode,
        )?;
        let outcome = JsonlSessionStore::new(session_log_path)?
            .execute_portable_semantic_compaction(self.preflight, self.target_material)?;
        Ok((outcome, self.native_carrier))
    }
}

/// Returns the source physical attempt only when this just-finished logical run contains one
/// exact, output-free context-window rejection.
///
/// A preceding tool/model turn makes the result ineligible even if a later request was rejected:
/// the recovery contract never attempts to replay a run that has already exposed output or side
/// effects.
pub(in crate::runner) fn exact_context_window_rejection_source(
    session: &Session,
    logical_run_id: &str,
) -> Result<Option<String>> {
    let projection = session.provider_physical_attempt_projection()?;
    let attempts = projection.attempts_for_logical_run_id(logical_run_id);
    if attempts.len() != 1 {
        return Ok(None);
    }
    let attempt = attempts[0];
    let Some(terminal) = attempt.terminal.as_ref() else {
        return Ok(None);
    };
    if attempt.entry.purpose != ProviderPhysicalAttemptPurpose::ConversationGeneration
        || attempt.entry.provider_name != session.provider_name()
        || attempt.entry.model_name != session.model_name()
        || terminal.outcome != ProviderPhysicalAttemptOutcome::ConfirmedNoModelConsumption
        || terminal.rejection != Some(ProviderRequestRejection::ContextWindowExceeded)
        || !terminal.durable_output_event_ids.is_empty()
        || !terminal.durable_side_effect_event_ids.is_empty()
    {
        return Ok(None);
    }
    Ok(Some(attempt.entry.physical_attempt_id.clone()))
}

/// Builds and measures one portable target only after an exact durable overflow rejection.
///
/// This path is intentionally not used by manual or idle compaction. Its remote count is bounded
/// by a dedicated physical-attempt lifecycle and returns process-local material only; the caller
/// must still apply the portable lifecycle and hand the retained frozen request to one new run.
#[allow(clippy::too_many_arguments)]
pub(in crate::runner) async fn prepare_overflow_recovery_compaction<P>(
    request_id: u64,
    root_config: &RootConfig,
    workspace_root: &std::path::Path,
    session_log_path: &std::path::Path,
    session: &mut Session,
    options: &AgentRunOptions,
    tools: Vec<sigil_kernel::ToolSpec>,
    source_physical_attempt_id: String,
    provider: &P,
    context_resolver: &sigil_runtime::RequestContextResolver,
) -> Result<PendingV2Compaction>
where
    P: sigil_kernel::Provider,
{
    let initiation = CompactionInitiation::OverflowRecovery {
        source_physical_attempt_id: source_physical_attempt_id.clone(),
    };
    if !sigil_runtime::is_openai_responses_portable_target_profile(
        session.provider_name(),
        session.model_name(),
    ) {
        bail!(
            "overflow recovery is unavailable outside the pinned official OpenAI Responses target profile"
        );
    }
    if provider.name() != session.provider_name() {
        bail!("overflow recovery provider does not match the durable session provider");
    }
    let effective_config = options.compaction_config.clone();
    if !effective_config.enabled {
        bail!("overflow recovery requires enabled compaction");
    }
    let preview =
        sigil_runtime::context_window::compaction_preview_for_strategy(session, &effective_config)?
            .context("overflow recovery has no foldable history")?;
    let runtime_context = resolve_session_request_context(session, context_resolver).await;
    let target_input = PortableV2TargetRequestInput {
        tools,
        reasoning_effort: options.reasoning_effort.clone(),
        previous_response_handle: session.latest_response_handle(session.provider_name()),
        traffic_partition_key: options.traffic_partition_key.clone(),
        transient_messages: Vec::new(),
        runtime_context,
    };
    prepare_portable_v2_compaction(
        request_id,
        initiation,
        root_config,
        workspace_root,
        session_log_path,
        provider,
        session,
        &options.memory_config,
        target_input,
        preview,
    )
    .await?
    .into_server_count_pending(provider, session, &source_physical_attempt_id)
    .await
}

/// Builds the zero-provider-I/O manual preview required before a billed semantic summary.
///
/// The deterministic checkpoint baseline is constructed with empty model-owned sections solely
/// to render authority, continuity, whole-turn tail and shrink evidence. It is never activated;
/// the full-compaction choice builds a fresh checkpoint from the validated model response.
pub(in crate::runner) fn prepare_v2_compaction_review(
    request_id: u64,
    root_config: &RootConfig,
    workspace_root: &std::path::Path,
    session_log_path: &std::path::Path,
    session: &Session,
    preview: V2CompactionPreview,
) -> Result<(V2CompactionReview, PendingLocalV2Compaction)> {
    let workspace_id = stable_workspace_id(workspace_root)?;
    let scope = root_config
        .verification
        .scope_for_hash(DEFAULT_TASK_VERIFICATION_SCOPE_HASH);
    let snapshot = build_workspace_snapshot(workspace_root, workspace_id, &scope, 0)?;
    let valid_for_snapshot = snapshot
        .workspace_snapshot_id
        .context("portable compaction requires a complete workspace snapshot")?;
    let now = current_unix_time_ms();
    let source_key = format!(
        "{}:{}:local-preview:{request_id}",
        session.session_scope_id(),
        preview.plan.base_stream_cursor.last_applied_event_id,
    );
    let store = JsonlSessionStore::new(session_log_path)?;
    let preflight =
        store.prepare_portable_semantic_compaction(PortableSemanticCompactionRequest {
            attempt_id: format!(
                "local-preview-{}",
                stable_event_uuid("sigil-local-compaction-preview-attempt", &source_key)
            ),
            compaction_id: format!(
                "local-preview-{}",
                stable_event_uuid("sigil-local-compaction-preview", &source_key)
            ),
            initiation: CompactionInitiation::Manual,
            base_projection_revision: "portable-v3-local-preview-r1".to_owned(),
            branch_id: None,
            valid_for_snapshot,
            objective: None,
            language: "en".to_owned(),
            plan: preview.plan.clone(),
            model_output: sigil_kernel::ContinuationModelOutputV1 {
                in_progress: Vec::new(),
                pending_actions: Vec::new(),
                provider_continuity: Vec::new(),
                model_notes: Vec::new(),
            },
            tool_output_projection_policy: ToolOutputProjectionPolicy::default(),
            started_at_unix_ms: now,
            completed_at_unix_ms: now,
        })?;
    let tool_output_shrink_candidates = tool_output_aging_previews(
        session,
        &sigil_runtime::secret_redactor_for_root_config(root_config),
    )?;
    let standalone_tool_output_shrink_available = !tool_output_shrink_candidates.is_empty();
    let review = V2CompactionReview {
        request_id,
        strategy: root_config.compaction.strategy,
        preview: preview.clone(),
        admission: V2CompactionAdmission::Prepared {
            standalone_tool_output_shrink_available,
        },
        tool_output_shrink_candidates,
        continuity: Some(continuity_preview(&preflight)),
        native_carrier_requested: root_config.compaction.native_carrier_enabled
            && sigil_runtime::application_compaction::NATIVE_COMPACTION_RESUME_ENABLED,
    };
    Ok((
        review,
        PendingLocalV2Compaction {
            request_id,
            session_scope_id: session.session_scope_id().to_owned(),
            preview,
        },
    ))
}

#[allow(clippy::too_many_arguments)]
pub(in crate::runner) fn prepare_v2_compaction_summary_review(
    request_id: u64,
    root_config: &RootConfig,
    workspace_root: &std::path::Path,
    session_log_path: &std::path::Path,
    provider: &dyn sigil_kernel::Provider,
    session: &mut Session,
    options: &AgentRunOptions,
    tools: Vec<sigil_kernel::ToolSpec>,
    context_resolver: &sigil_runtime::RequestContextResolver,
    runtime_handle: &tokio::runtime::Handle,
    preview: V2CompactionPreview,
) -> Result<(V2CompactionReview, Option<PendingV2Compaction>)> {
    let runtime_context =
        runtime_handle.block_on(resolve_session_request_context(session, context_resolver));
    runtime_handle.block_on(prepare_v2_compaction(
        request_id,
        CompactionInitiation::Manual,
        root_config,
        workspace_root,
        session_log_path,
        provider,
        session,
        options,
        tools,
        runtime_context,
        preview,
    ))
}

/// Prepares the automatic K25.11 path without creating a modal.
///
/// This is invoked only by the scheduler after a successful chat run and after it has proven
/// that no active run, queue item, or agent-result continuation remains. Cache-aware V3 performs
/// one audited semantic-summary request; the same exact target admission as manual `/compact`
/// remains mandatory.
#[allow(clippy::too_many_arguments)]
pub(in crate::runner) fn prepare_idle_auto_compaction(
    state: &mut IdleAutoCompactionState,
    root_config: &RootConfig,
    workspace_root: &std::path::Path,
    session_log_path: &std::path::Path,
    provider: &dyn sigil_kernel::Provider,
    session: &mut Session,
    options: &AgentRunOptions,
    tools: Vec<sigil_kernel::ToolSpec>,
    context_resolver: &sigil_runtime::RequestContextResolver,
    runtime_handle: &tokio::runtime::Handle,
) -> Result<IdleAutoCompactionPreparation> {
    if !state.is_requested() {
        return Ok(IdleAutoCompactionPreparation::NotRequested);
    }

    let effective_config = options.compaction_config.clone();
    let threshold_status = effective_config.threshold_status(session.stats().last_prompt_tokens);
    let threshold_allows_preparation = !matches!(
        threshold_status,
        CompactionThresholdStatus::Off | CompactionThresholdStatus::NotAvailable
    );
    if !threshold_allows_preparation {
        state.consume_request();
        return Ok(IdleAutoCompactionPreparation::NotHardThreshold);
    }

    let Some(preview) =
        sigil_runtime::context_window::compaction_preview_for_strategy(session, &effective_config)?
    else {
        state.consume_request();
        return Ok(IdleAutoCompactionPreparation::NoFoldableHistory);
    };
    let next_turn_p95_tokens = preview
        .plan
        .adaptive_tail
        .recent_complete_turn_p95_tokens
        .max(1);
    let output_reserve = sigil_runtime::portable_compaction_target_output_tokens(
        session.provider_name(),
        session.model_name(),
    )
    .map_or(4_096, u64::from);
    let fit_required = effective_config
        .context_window_tokens
        .is_some_and(|context_window| {
            session
                .stats()
                .last_prompt_tokens
                .saturating_add(next_turn_p95_tokens)
                .saturating_add(output_reserve)
                .saturating_add(8_192)
                >= u64::from(context_window)
        });
    if !fit_required {
        // Cost-only automatic compaction must eventually pass a pre-call upper-bound
        // economics gate. Until that exact gate is available, do not spend a summary request
        // merely to discover that rotation is uneconomic.
        state.consume_request();
        return Ok(IdleAutoCompactionPreparation::NotHardThreshold);
    }
    let circuit_scope = idle_auto_circuit_scope(session, &preview)?;
    let scope_fingerprint =
        idle_auto_scope_fingerprint(session, &preview, &effective_config, &circuit_scope)?;
    let now = current_unix_time_ms();
    if let Some(retry_after_unix_ms) = state.retry_after(&scope_fingerprint)
        && now < retry_after_unix_ms
    {
        state.consume_request();
        return Ok(IdleAutoCompactionPreparation::CoolingDown {
            retry_after_unix_ms,
        });
    }

    let emergency = effective_config
        .context_window_tokens
        .is_some_and(|context_window| {
            session.stats().last_prompt_tokens.saturating_mul(10)
                >= u64::from(context_window).saturating_mul(9)
        });
    let durable_admission = idle_auto_compaction_durable_admission(
        session,
        &scope_fingerprint,
        circuit_scope.clone(),
        emergency,
        emergency_blocking_layer(&preview, session.stats().last_prompt_tokens),
    )?;
    if durable_admission.failure_latched {
        state.consume_request();
        return Ok(IdleAutoCompactionPreparation::FailureLatched);
    }
    if durable_admission.circuit_decision != CompactionCircuitBreakerDecisionV1::Allowed {
        state.consume_request();
        return Ok(IdleAutoCompactionPreparation::CircuitOpen {
            decision: durable_admission.circuit_decision,
        });
    }

    let runtime_context =
        runtime_handle.block_on(resolve_session_request_context(session, context_resolver));
    let (review, pending) = runtime_handle.block_on(prepare_v2_compaction(
        0,
        CompactionInitiation::IdleAutomatic {
            scope_fingerprint: scope_fingerprint.clone(),
            circuit_scope: Some(circuit_scope),
        },
        root_config,
        workspace_root,
        session_log_path,
        provider,
        session,
        options,
        tools,
        runtime_context,
        preview,
    ))?;
    state.consume_request();
    match pending {
        Some(pending) => Ok(IdleAutoCompactionPreparation::Ready(Box::new(pending))),
        None => {
            let V2CompactionAdmission::Unavailable { reason } = review.admission else {
                bail!("V2 compaction admission lost its pending apply material");
            };
            state.set_cooldown(scope_fingerprint, now);
            Ok(IdleAutoCompactionPreparation::AdmissionUnavailable { reason })
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn prepare_v2_compaction(
    request_id: u64,
    initiation: CompactionInitiation,
    root_config: &RootConfig,
    workspace_root: &std::path::Path,
    session_log_path: &std::path::Path,
    provider: &dyn sigil_kernel::Provider,
    session: &mut Session,
    options: &AgentRunOptions,
    tools: Vec<sigil_kernel::ToolSpec>,
    runtime_context: RuntimeContextCandidates,
    preview: V2CompactionPreview,
) -> Result<(V2CompactionReview, Option<PendingV2Compaction>)> {
    let review = |source_preview: &V2CompactionPreview,
                  admission,
                  tool_output_shrink_candidates,
                  continuity| V2CompactionReview {
        request_id,
        strategy: root_config.compaction.strategy,
        preview: source_preview.clone(),
        admission,
        tool_output_shrink_candidates,
        continuity,
        native_carrier_requested: root_config.compaction.native_carrier_enabled
            && sigil_runtime::application_compaction::NATIVE_COMPACTION_RESUME_ENABLED,
    };
    let target_input = PortableV2TargetRequestInput {
        tools,
        reasoning_effort: options.reasoning_effort.clone(),
        previous_response_handle: session.latest_response_handle(session.provider_name()),
        traffic_partition_key: options.traffic_partition_key.clone(),
        transient_messages: Vec::new(),
        runtime_context,
    };
    match prepare_portable_v2_compaction(
        request_id,
        initiation,
        root_config,
        workspace_root,
        session_log_path,
        provider,
        session,
        &options.memory_config,
        target_input,
        preview.clone(),
    )
    .await
    .and_then(PreparedPortableV2Compaction::into_pending)
    {
        Ok(pending) => {
            let source_preview = pending.source_preview().clone();
            let continuity = Some(continuity_preview(&pending.preflight));
            let tool_output_shrink_candidates = tool_output_aging_previews(
                session,
                &sigil_runtime::secret_redactor_for_root_config(root_config),
            )?;
            let budget = &pending.target_material.proof().budget;
            let economics = pending
                .target_material
                .portable_economics()
                .context("portable target material has no before/after economics proof")?;
            match &pending.target_material.proof().input {
                sigil_kernel::InputTokenEvidence::Exact { tokens, .. } => Ok((
                    review(
                        &source_preview,
                        V2CompactionAdmission::Ready {
                            before_input_tokens: economics.before_input.admission_tokens(),
                            input_tokens: *tokens,
                            context_window_tokens: budget.context_window_tokens,
                            output_tokens: budget.requested_output_tokens,
                            safety_buffer_tokens: budget.safety_buffer_tokens,
                            savings_tokens: economics.savings_tokens,
                            savings_ratio_ppm: economics.savings_ratio_ppm,
                            minimum_savings_tokens: economics.minimum_savings_tokens,
                            minimum_savings_ratio_ppm: economics.minimum_savings_ratio_ppm,
                            summary_usage_observed: pending
                                .economics_v2_input
                                .compactor_usage_observed,
                            deterministic_emergency_fallback: pending
                                .deterministic_emergency_fallback,
                            summary_cache_read_tokens: pending
                                .economics_v2_input
                                .compactor_cache_read_tokens,
                            summary_uncached_input_tokens: pending
                                .economics_v2_input
                                .compactor_uncached_input_tokens,
                            summary_output_tokens: pending
                                .economics_v2_input
                                .compactor_output_tokens,
                            summary_cost_nano_usd: economics
                                .v2_economics
                                .as_ref()
                                .and_then(|economics| economics.cost_projection.as_ref())
                                .map(|cost| cost.rotate_compactor_cost_nano_usd),
                            economics_v2: economics.v2_economics.clone().map(Box::new),
                        },
                        tool_output_shrink_candidates,
                        continuity,
                    ),
                    Some(pending),
                )),
                sigil_kernel::InputTokenEvidence::ConservativeUpperBound { .. } => Ok((
                    review(
                        &source_preview,
                        V2CompactionAdmission::Unavailable {
                            reason: "local exact target proof is unavailable".to_owned(),
                        },
                        tool_output_shrink_candidates,
                        continuity,
                    ),
                    None,
                )),
            }
        }
        Err(error) => Ok((
            review(
                &preview,
                V2CompactionAdmission::Unavailable {
                    reason: format!("local exact target proof is unavailable: {error:#}"),
                },
                Vec::new(),
                None,
            ),
            None,
        )),
    }
}

fn tool_output_aging_previews(
    session: &Session,
    redactor: &sigil_kernel::SecretRedactor,
) -> Result<Vec<ToolOutputShrinkPreview>> {
    let Some(active) = session.active_projection_snapshot()? else {
        return Ok(Vec::new());
    };
    let pressure = active.tool_output_pressure();
    let Some(batch) = sigil_kernel::ToolOutputAgingBatchV1::select(
        &pressure,
        sigil_kernel::ToolOutputAgingReasonV1::Manual,
    )?
    else {
        return Ok(Vec::new());
    };
    let selected = batch
        .source_event_ids
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    Ok(pressure
        .items
        .iter()
        .filter(|item| selected.contains(&item.source_event_id))
        .map(|item| ToolOutputShrinkPreview {
            tool_name: item.tool_name.clone(),
            tool_call_id: item.call_id.clone(),
            status: item.facts.status.clone(),
            original_content_bytes: item.observed_bytes,
            original_content_token_upper_bound: item.initial_model_tokens,
            head_excerpt: redactor.redact_text(&item.preview_excerpt),
            tail_excerpt: String::new(),
            content_sha256: item.artifact_sha256.clone().unwrap_or_default(),
            artifact_ref: item.artifact_ref.as_ref().map_or_else(
                || "artifact unavailable".to_owned(),
                |artifact_ref| artifact_ref.artifact_id.clone(),
            ),
            reason: "large completed historical result".to_owned(),
            recovery_instruction: item.artifact_ref.as_ref().map_or_else(
                || "raw artifact is unavailable; use the preserved facts and preview".to_owned(),
                |artifact_ref| {
                    format!(
                        "use read_tool_artifact with opaque ref {} for bounded retrieval",
                        artifact_ref.artifact_id
                    )
                },
            ),
        })
        .collect())
}

fn continuity_preview(preflight: &PortableSemanticCompactionPreflight) -> V2ContinuityPreview {
    let checkpoint = preflight.checkpoint();
    let anchor = checkpoint
        .session_anchor
        .as_ref()
        .expect("admitted V3 checkpoint has a session anchor");
    let continuity = checkpoint
        .continuity_v2
        .as_ref()
        .expect("admitted V3 checkpoint has source-bound continuity");
    let anchored_source_refs = 1
        + usize::from(anchor.active_subgoal.is_some())
        + anchor.constraints.len()
        + anchor.authorization_boundary.len()
        + anchor.attachment_refs.len();
    let grounded_source_refs = [
        &continuity.decisions,
        &continuity.progress,
        &continuity.pending_work,
        &continuity.files_and_artifacts,
        &continuity.commands,
        &continuity.verification,
        &continuity.failures_and_dead_ends,
        &continuity.risks,
        &continuity.unresolved_questions,
    ]
    .into_iter()
    .flatten()
    .map(|item| item.source_refs.len())
    .sum::<usize>();
    V2ContinuityPreview {
        root_objective: bounded_preview_text(&anchor.root_objective.exact_text, 192),
        active_constraints: anchor
            .constraints
            .iter()
            .chain(&anchor.authorization_boundary)
            .filter(|constraint| constraint.status == sigil_kernel::ConstraintStatusV1::Active)
            .map(|constraint| V2ConstraintPreview {
                text: bounded_preview_text(&constraint.exact_text, 192),
                source_event_id: constraint.source.event_id.clone(),
                source_field_path: constraint.source.field_path.clone(),
            })
            .collect(),
        active_constraint_count: anchor
            .constraints
            .iter()
            .filter(|constraint| constraint.status == sigil_kernel::ConstraintStatusV1::Active)
            .count(),
        authorization_boundary_count: anchor.authorization_boundary.len(),
        recoverable_attachment_count: anchor.attachment_refs.len(),
        pending_work_count: continuity.pending_work.len(),
        unresolved_question_count: continuity.unresolved_questions.len(),
        source_ref_count: anchored_source_refs.saturating_add(grounded_source_refs),
    }
}

fn bounded_preview_text(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let prefix = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}

struct PortableV2TargetRequestInput {
    tools: Vec<sigil_kernel::ToolSpec>,
    reasoning_effort: Option<sigil_kernel::ReasoningEffort>,
    previous_response_handle: Option<sigil_kernel::ResponseHandle>,
    traffic_partition_key: Option<String>,
    transient_messages: Vec<sigil_kernel::ModelMessage>,
    runtime_context: RuntimeContextCandidates,
}

fn require_deepseek_portable_transport(root_config: &RootConfig, session: &Session) -> Result<()> {
    match session.resolved_model_route() {
        Some(route) => sigil_runtime::require_deepseek_v4_flash_portable_transport_for_model_ref(
            root_config,
            &route.model_ref,
        ),
        None => sigil_runtime::require_default_deepseek_v4_flash_portable_transport(root_config),
    }
}

#[allow(clippy::too_many_arguments)]
async fn prepare_portable_v2_compaction(
    request_id: u64,
    initiation: CompactionInitiation,
    root_config: &RootConfig,
    workspace_root: &std::path::Path,
    session_log_path: &std::path::Path,
    provider: &dyn sigil_kernel::Provider,
    session: &mut Session,
    memory_config: &sigil_kernel::MemoryConfig,
    target_input: PortableV2TargetRequestInput,
    preview: V2CompactionPreview,
) -> Result<PreparedPortableV2Compaction> {
    let local_target_profile = sigil_runtime::is_deepseek_v4_flash_portable_target_profile(
        session.provider_name(),
        session.model_name(),
    );
    let overflow_server_count_profile =
        matches!(&initiation, CompactionInitiation::OverflowRecovery { .. })
            && sigil_runtime::is_openai_responses_portable_target_profile(
                session.provider_name(),
                session.model_name(),
            );
    if !local_target_profile && !overflow_server_count_profile {
        bail!("route has no admitted portable target profile for this compaction initiation");
    }
    if sigil_runtime::is_deepseek_v4_flash_portable_target_profile(
        session.provider_name(),
        session.model_name(),
    ) {
        require_deepseek_portable_transport(root_config, session)?;
    }
    let workspace_id = stable_workspace_id(workspace_root)?;
    let scope = root_config
        .verification
        .scope_for_hash(DEFAULT_TASK_VERIFICATION_SCOPE_HASH);
    let snapshot = build_workspace_snapshot(workspace_root, workspace_id, &scope, 0)?;
    let valid_for_snapshot = snapshot
        .workspace_snapshot_id
        .context("portable compaction requires a complete workspace snapshot")?;
    let now = current_unix_time_ms();
    let source_key = match &initiation {
        CompactionInitiation::Manual => format!(
            "{}:{}:manual:{request_id}",
            session.session_scope_id(),
            preview.plan.base_stream_cursor.last_applied_event_id,
        ),
        CompactionInitiation::IdleAutomatic {
            scope_fingerprint, ..
        } => {
            format!(
                "{}:idle-auto:{scope_fingerprint}:request:{request_id}",
                session.session_scope_id(),
            )
        }
        CompactionInitiation::PreTurnPressure { queue_id } => format!(
            "{}:{}:pre-turn:{}",
            session.session_scope_id(),
            preview.plan.base_stream_cursor.last_applied_event_id,
            queue_id.as_str(),
        ),
        CompactionInitiation::OverflowRecovery {
            source_physical_attempt_id,
        } => format!(
            "{}:{}:overflow-recovery:{source_physical_attempt_id}",
            session.session_scope_id(),
            preview.plan.base_stream_cursor.last_applied_event_id,
        ),
    };
    let attempt_id = format!(
        "portable-{}",
        stable_event_uuid("sigil-portable-compaction-attempt", &source_key)
    );
    let compaction_id = format!(
        "portable-{}",
        stable_event_uuid("sigil-portable-compaction-activation", &source_key)
    );
    let store = JsonlSessionStore::new(session_log_path)?;
    let target_max_tokens = Some(
        sigil_runtime::portable_compaction_target_output_tokens(
            session.provider_name(),
            session.model_name(),
        )
        .context(
            "local exact target proof is unavailable: route has no admitted portable target profile",
        )?,
    );
    let before_request = session.build_pre_turn_candidate_request(
        workspace_root,
        memory_config,
        target_input.tools.clone(),
        target_max_tokens,
        target_input.reasoning_effort.clone(),
        target_input.previous_response_handle.clone(),
        target_input.traffic_partition_key.clone(),
        &target_input.transient_messages,
        target_input.runtime_context.clone(),
        &[],
    )?;
    let frozen_before_request =
        FrozenProviderRequestMaterial::freeze(session.session_scope_id(), before_request)?;
    let fallback_policy = if matches!(
        &initiation,
        CompactionInitiation::PreTurnPressure { .. }
            | CompactionInitiation::OverflowRecovery { .. }
    ) {
        sigil_runtime::SemanticCompactionFallbackPolicy::DeterministicEmergency
    } else {
        sigil_runtime::SemanticCompactionFallbackPolicy::Forbid
    };
    let summary_result = sigil_runtime::generate_portable_compaction_summary(
        provider,
        session,
        &store,
        &attempt_id,
        &frozen_before_request,
        &preview.plan,
        fallback_policy,
    )
    .await;
    let summary = match summary_result {
        Ok(summary) => summary,
        Err(error) => {
            if fallback_policy == sigil_runtime::SemanticCompactionFallbackPolicy::Forbid {
                sigil_runtime::record_semantic_compaction_failure(
                    &store,
                    &attempt_id,
                    initiation.clone(),
                    now,
                    &error,
                )
                .context("failed to record semantic compaction failure")?;
            }
            return Err(error).context("semantic compaction summary request failed");
        }
    };
    let sigil_runtime::PortableCompactionSummary {
        model_output,
        usage: summary_usage,
        rebased_plan,
        deterministic_emergency_fallback,
    } = summary;
    let source_preview = V2CompactionPreview {
        plan: rebased_plan.clone(),
        active_compaction_id: preview.active_compaction_id.clone(),
    };
    let native_portable_compaction_id = compaction_id.clone();
    let native_covers_through = rebased_plan
        .folded_through
        .clone()
        .context("portable compaction plan has no folded-through cursor")?;
    let request = PortableSemanticCompactionRequest {
        attempt_id,
        compaction_id,
        initiation: initiation.clone(),
        base_projection_revision: "portable-v3-hybrid-summary-r1".to_owned(),
        branch_id: None,
        valid_for_snapshot,
        objective: None,
        language: "en".to_owned(),
        plan: rebased_plan,
        model_output,
        tool_output_projection_policy: ToolOutputProjectionPolicy::default(),
        started_at_unix_ms: now,
        completed_at_unix_ms: current_unix_time_ms(),
    };
    let preflight = store.prepare_portable_semantic_compaction(request)?;
    let target_request = session.build_portable_compaction_candidate_request(
        workspace_root,
        memory_config,
        preflight.checkpoint(),
        preflight.task_memory(),
        preflight.candidate_messages().to_vec(),
        target_input.tools,
        target_max_tokens,
        target_input.reasoning_effort,
        target_input.previous_response_handle,
        target_input.traffic_partition_key,
        &target_input.transient_messages,
        target_input.runtime_context,
        &[],
    )?;
    let frozen_target_request =
        FrozenProviderRequestMaterial::freeze(session.session_scope_id(), target_request)?;
    let paths = sigil_runtime::resolve_sigil_paths(
        &root_config.storage,
        &root_config.session,
        workspace_root,
    );
    let economics_v2_input = portable_economics_v2_input(
        session,
        &preview,
        &preflight,
        &initiation,
        summary_usage.as_ref(),
    );
    Ok(PreparedPortableV2Compaction {
        request_id,
        session_scope_id: session.session_scope_id().to_owned(),
        idle_auto_scope_fingerprint: match initiation {
            CompactionInitiation::IdleAutomatic {
                scope_fingerprint, ..
            } => Some(scope_fingerprint),
            CompactionInitiation::Manual
            | CompactionInitiation::PreTurnPressure { .. }
            | CompactionInitiation::OverflowRecovery { .. } => None,
        },
        deterministic_emergency_fallback,
        source_preview,
        cache_root: paths.cache_root,
        preflight,
        frozen_before_request,
        frozen_target_request,
        native_covers_through,
        native_portable_compaction_id,
        economics_v2_input,
        folded_event_count: preview.plan.folded_event_ids.len(),
    })
}

fn portable_economics_v2_input(
    session: &Session,
    preview: &V2CompactionPreview,
    preflight: &PortableSemanticCompactionPreflight,
    initiation: &CompactionInitiation,
    summary_usage: Option<&sigil_kernel::UsageStats>,
) -> sigil_runtime::PortableCompactionEconomicsV2Input {
    let latest_usage = session.entries().iter().rev().find_map(|entry| {
        let sigil_kernel::SessionLogEntry::Control(ControlEntry::UsageSnapshot(usage)) = entry
        else {
            return None;
        };
        Some(usage)
    });
    let cache_usage = latest_usage.and_then(|usage| usage.cache_usage.as_ref());
    let summary_cache_usage = summary_usage.and_then(|usage| usage.cache_usage.as_ref());
    let next_turn_p95_tokens = preview
        .plan
        .adaptive_tail
        .recent_complete_turn_p95_tokens
        .max(1);
    let bulky_shrink_candidate_tokens = preflight
        .tool_output_shrink_candidates()
        .iter()
        .fold(0_u64, |total, candidate| {
            total.saturating_add(candidate.original_content_token_upper_bound)
        });
    let (rollout_mode, user_confirmed, overflow_observed) = match initiation {
        CompactionInitiation::Manual => (CompactionRolloutModeV1::Preview, false, false),
        CompactionInitiation::IdleAutomatic { .. } => {
            (CompactionRolloutModeV1::Automatic, false, false)
        }
        CompactionInitiation::PreTurnPressure { .. } => {
            (CompactionRolloutModeV1::Automatic, false, false)
        }
        CompactionInitiation::OverflowRecovery { .. } => {
            (CompactionRolloutModeV1::Automatic, false, true)
        }
    };
    sigil_runtime::PortableCompactionEconomicsV2Input {
        next_turn_p95_tokens,
        tool_growth_p95_tokens: 4_096,
        provider_state_tokens: 0,
        bulky_shrink_candidate_tokens,
        overflow_observed,
        expected_remaining_turns: ExpectedRemainingTurnsV1 {
            turns: 3,
            source: CompactionForecastSourceV1::ConservativeFallback,
            confidence: CompactionForecastConfidenceV1::Low,
            source_event_ids: Vec::new(),
        },
        observed_current_cache_read_tokens: cache_usage
            .and_then(|usage| usage.read.as_ref())
            .map(|count| count.tokens),
        observed_current_uncached_tokens: cache_usage
            .and_then(|usage| usage.uncached.as_ref())
            .map(|count| count.tokens),
        pricing_snapshot: summary_usage
            .filter(|usage| usage.prompt_tokens > 0 && usage.completion_tokens > 0)
            .and_then(|usage| usage.pricing_snapshot.clone())
            .or_else(|| {
                summary_usage
                    .is_some_and(|usage| usage.prompt_tokens > 0 && usage.completion_tokens > 0)
                    .then(|| latest_usage.and_then(|usage| usage.pricing_snapshot.clone()))
                    .flatten()
            }),
        compactor_usage_observed: summary_usage
            .is_some_and(|usage| usage.prompt_tokens > 0 && usage.completion_tokens > 0),
        compactor_cache_read_tokens: summary_cache_usage
            .and_then(|usage| usage.read.as_ref())
            .map_or_else(
                || summary_usage.map_or(0, |usage| usage.cache_hit_tokens),
                |count| count.tokens,
            ),
        compactor_uncached_input_tokens: summary_cache_usage
            .and_then(|usage| usage.uncached.as_ref())
            .map_or_else(
                || summary_usage.map_or(0, |usage| usage.cache_miss_tokens),
                |count| count.tokens,
            ),
        compactor_output_tokens: summary_usage.map_or(0, |usage| usage.completion_tokens),
        rollout_mode,
        user_confirmed,
    }
}

fn economics_v2_admission(
    target_material: &PortableTargetRequestMaterial,
) -> Result<&sigil_kernel::CompactionAdmissionV2> {
    target_material
        .portable_economics()
        .and_then(|economics| economics.v2_economics.as_ref())
        .map(|economics| &economics.admission)
        .context("portable target material has no RFC-0057 admission")
}

fn require_prepared_v2_admission(
    target_material: &PortableTargetRequestMaterial,
    rollout_mode: CompactionRolloutModeV1,
) -> Result<()> {
    let admission = economics_v2_admission(target_material)?;
    match rollout_mode {
        CompactionRolloutModeV1::Shadow => Ok(()),
        CompactionRolloutModeV1::Preview
            if admission.decision == sigil_kernel::CompactionAdmissionDecisionV2::Preview =>
        {
            Ok(())
        }
        CompactionRolloutModeV1::Automatic
            if admission.decision == sigil_kernel::CompactionAdmissionDecisionV2::Admit
                && admission.automatic_allowed =>
        {
            Ok(())
        }
        CompactionRolloutModeV1::Preview | CompactionRolloutModeV1::Automatic => bail!(
            "RFC-0057 compaction preparation is not admitted: {:?} ({:?})",
            admission.decision,
            admission.reason
        ),
    }
}

fn require_activation_v2_admission(
    target_material: &PortableTargetRequestMaterial,
    rollout_mode: CompactionRolloutModeV1,
) -> Result<()> {
    let admission = economics_v2_admission(target_material)?;
    match rollout_mode {
        CompactionRolloutModeV1::Shadow => Ok(()),
        CompactionRolloutModeV1::Preview
            if admission.decision == sigil_kernel::CompactionAdmissionDecisionV2::Admit
                && admission.user_confirmed
                && !admission.automatic_allowed =>
        {
            Ok(())
        }
        CompactionRolloutModeV1::Automatic
            if admission.decision == sigil_kernel::CompactionAdmissionDecisionV2::Admit
                && admission.automatic_allowed =>
        {
            Ok(())
        }
        CompactionRolloutModeV1::Preview | CompactionRolloutModeV1::Automatic => bail!(
            "RFC-0057 compaction activation is not admitted: {:?} ({:?})",
            admission.decision,
            admission.reason
        ),
    }
}

/// Completes the no-write pre-turn admission for the next queued conversation input.
///
/// Exact fit returns the frozen direct candidate. When the direct target exceeds the only
/// admitted local budget, this prepares and proves a second frozen request based on a portable
/// compaction preflight whose fold source is the current durable stream before queue promotion.
/// Neither branch appends a queue promotion, compaction activation, or capability registration.
/// The pressure branch may append one audited semantic-summary physical attempt before target
/// admission; failure leaves the queued input unpromoted.
#[allow(clippy::too_many_arguments)]
pub(in crate::runner) fn prepare_next_queued_conversation_pre_turn_admission(
    root_config: &RootConfig,
    workspace_root: &std::path::Path,
    session_log_path: &std::path::Path,
    provider: &dyn sigil_kernel::Provider,
    session: &mut Session,
    exact_prompts: &ExactConversationPromptStore,
    memory_config: &sigil_kernel::MemoryConfig,
    tools: Vec<sigil_kernel::ToolSpec>,
    default_reasoning_effort: Option<sigil_kernel::ReasoningEffort>,
    traffic_partition_key: Option<String>,
    context_resolver: &sigil_runtime::RequestContextResolver,
    runtime_handle: &tokio::runtime::Handle,
) -> Result<QueuedConversationPreTurnAdmission> {
    let paths = sigil_runtime::resolve_sigil_paths(
        &root_config.storage,
        &root_config.session,
        workspace_root,
    );
    let mut fit_required_aging_attempted = false;
    loop {
        match super::prepare_next_queued_conversation_pressure_admission_with_resolver(
            session,
            exact_prompts,
            workspace_root,
            memory_config,
            tools.clone(),
            default_reasoning_effort.clone(),
            traffic_partition_key.clone(),
            &paths.cache_root,
            context_resolver,
            runtime_handle,
        )
        .map_err(anyhow::Error::msg)?
        {
            QueuedConversationPressureAdmission::NoQueuedInput => {
                return Ok(QueuedConversationPreTurnAdmission::NoQueuedInput);
            }
            QueuedConversationPressureAdmission::ExactFit(candidate) => {
                return Ok(QueuedConversationPreTurnAdmission::ExactFit(candidate));
            }
            QueuedConversationPressureAdmission::Blocked {
                queue_id,
                reason,
                candidate,
            } => {
                return Ok(QueuedConversationPreTurnAdmission::Blocked {
                    queue_id,
                    reason,
                    candidate,
                });
            }
            QueuedConversationPressureAdmission::PortablePreflightRequired {
                candidate, ..
            } => {
                if !fit_required_aging_attempted {
                    fit_required_aging_attempted = true;
                    if let Some(active) = session.active_projection_snapshot()? {
                        let pressure = active.tool_output_pressure();
                        if let Some(batch) = sigil_kernel::ToolOutputAgingBatchV1::select(
                            &pressure,
                            sigil_kernel::ToolOutputAgingReasonV1::FitRequired,
                        )? {
                            let activation = sigil_kernel::ToolOutputAgingActivatedV1::prepare(
                                &pressure, &batch,
                            )?;
                            if session
                                .append_tool_output_aging_activation(active.frontier(), activation)?
                                .is_some()
                            {
                                // Re-freeze the queued request from the newly activated epoch before
                                // considering semantic compaction. No raw JSONL replay is needed by
                                // the event-driven pressure selector or activation CAS.
                                continue;
                            }
                        }
                    }
                }
                let queue_id = candidate.promotion.queue_id.clone();
                let effective_config = sigil_runtime::effective_compaction_config_for_runtime_model(
                    root_config,
                    session.provider_name(),
                    session.model_name(),
                );
                if !effective_config.enabled {
                    return Ok(QueuedConversationPreTurnAdmission::Blocked {
                        queue_id,
                        reason: "queued pre-turn portable compaction is disabled".to_owned(),
                        candidate: Some(candidate),
                    });
                }
                let fallback_candidate = candidate.clone();
                return match runtime_handle.block_on(prepare_queued_portable_preflight(
                    root_config,
                    workspace_root,
                    session_log_path,
                    provider,
                    session,
                    memory_config,
                    *candidate,
                )) {
                    Ok(Some(pending)) => Ok(
                        QueuedConversationPreTurnAdmission::PortablePreflightReady(Box::new(
                            pending,
                        )),
                    ),
                    Ok(None) => Ok(QueuedConversationPreTurnAdmission::Blocked {
                        queue_id,
                        reason: "queued pre-turn portable compaction has no foldable prior history"
                            .to_owned(),
                        candidate: Some(fallback_candidate),
                    }),
                    Err(_) => Ok(QueuedConversationPreTurnAdmission::Blocked {
                        queue_id,
                        reason:
                            "queued pre-turn portable compaction is unavailable from the local target profile"
                                .to_owned(),
                        candidate: Some(fallback_candidate),
                    }),
                };
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn prepare_queued_portable_preflight(
    root_config: &RootConfig,
    workspace_root: &std::path::Path,
    session_log_path: &std::path::Path,
    provider: &dyn sigil_kernel::Provider,
    session: &mut Session,
    memory_config: &sigil_kernel::MemoryConfig,
    mut candidate: PreparedQueuedConversationCandidate,
) -> Result<Option<PendingQueuedConversationPortablePreflight>> {
    let effective_config = sigil_runtime::effective_compaction_config_for_runtime_model(
        root_config,
        session.provider_name(),
        session.model_name(),
    );
    let Some(preview) =
        sigil_runtime::context_window::compaction_preview_for_strategy(session, &effective_config)?
    else {
        return Ok(None);
    };

    let durable_user_message_id = &candidate.promotion.durable_user_message.id;
    let exact_user_message = candidate
        .frozen_request
        .request()
        .messages
        .iter()
        .find(|message| message.id == *durable_user_message_id)
        .cloned()
        .context("queued pre-turn candidate lost its exact user message")?;
    let exact_prompt = exact_user_message
        .content
        .as_deref()
        .context("queued pre-turn candidate user message has no text")?;
    let prompt_projection = sigil_kernel::project_conversation_prompt_for_persistence(exact_prompt);
    if prompt_projection.prompt_hash != candidate.promotion.prompt_hash
        || prompt_projection.safe_prompt
            != candidate
                .promotion
                .durable_user_message
                .content
                .as_deref()
                .unwrap_or_default()
        || prompt_projection.exact_prompt_required != candidate.promotion.exact_prompt_required
    {
        bail!("queued pre-turn candidate exact material no longer matches its promotion bind");
    }

    let runtime_context = candidate.runtime_context.clone();
    let direct_request = candidate.frozen_request.request();
    let mut transient_messages = vec![exact_user_message];
    if direct_request.messages.iter().any(|message| {
        message.role == sigil_kernel::MessageRole::System
            && message.content.as_deref()
                == Some(sigil_kernel::conversation_route_routing_contract_material())
    }) {
        transient_messages.insert(
            0,
            sigil_kernel::ModelMessage::system(
                sigil_kernel::conversation_route_routing_contract_material(),
            ),
        );
    }
    transient_messages.extend(candidate.background_ready_context.clone());
    let target_input = PortableV2TargetRequestInput {
        tools: direct_request.tools.clone(),
        reasoning_effort: direct_request.reasoning_effort.clone(),
        previous_response_handle: direct_request.previous_response_handle.clone(),
        traffic_partition_key: direct_request.traffic_partition_key.clone(),
        transient_messages,
        runtime_context,
    };
    let prepared = prepare_portable_v2_compaction(
        0,
        CompactionInitiation::PreTurnPressure {
            queue_id: candidate.promotion.queue_id.clone(),
        },
        root_config,
        workspace_root,
        session_log_path,
        provider,
        session,
        memory_config,
        target_input,
        preview,
    )
    .await?;
    let post_compaction_frozen_request = prepared.frozen_target_request.clone();
    let pending_compaction = prepared.into_pending()?;
    candidate.frozen_request = post_compaction_frozen_request;
    Ok(Some(PendingQueuedConversationPortablePreflight {
        candidate,
        pending_compaction,
    }))
}

async fn resolve_session_request_context(
    session: &Session,
    context_resolver: &sigil_runtime::RequestContextResolver,
) -> RuntimeContextCandidates {
    let query = session.messages().into_iter().rev().find_map(|message| {
        matches!(message.role, sigil_kernel::MessageRole::User)
            .then_some(message.content)
            .flatten()
            .filter(|content| !content.trim().is_empty())
    });
    match query {
        Some(query) => context_resolver.resolve(&query).await.unwrap_or_default(),
        None => RuntimeContextCandidates::default(),
    }
}

pub(in crate::runner) fn has_failed_idle_automatic_scope(
    session_log_path: &std::path::Path,
    scope_fingerprint: &str,
) -> Result<bool> {
    let active = JsonlSessionStore::new(session_log_path)?.active_projection_snapshot()?;
    Ok(active
        .compaction()
        .has_failed_idle_automatic_scope(scope_fingerprint))
}

fn idle_auto_scope_fingerprint(
    session: &Session,
    preview: &V2CompactionPreview,
    effective_config: &sigil_kernel::CompactionConfig,
    circuit_scope: &CompactionCircuitScopeV1,
) -> Result<String> {
    let material = serde_json::json!({
        "schema": "sigil-idle-auto-compaction-scope-v1",
        "session_scope_id": session.session_scope_id(),
        "provider_name": session.provider_name(),
        "model_name": session.model_name(),
        "context_window_tokens": effective_config.context_window_tokens,
        "strategy": effective_config.strategy.as_str(),
        "preparation_ratio_bits": sigil_kernel::COMPACTION_PREPARATION_RATIO.to_bits(),
        "emergency_ratio_bits": sigil_kernel::COMPACTION_EMERGENCY_RATIO.to_bits(),
        "adaptive_tail_policy": sigil_kernel::AdaptiveTailPolicyV3::default(),
        "target_output_tokens": sigil_runtime::deepseek_v4_flash_portable_target_output_tokens(),
        "target_policy_revision": 1,
        "circuit_scope": circuit_scope,
        "active_compaction_id": &preview.active_compaction_id,
        "prior_folded_through": &preview.plan.prior_folded_through,
        "folded_event_ids": &preview.plan.folded_event_ids,
        "retained_event_ids": &preview.plan.retained_event_ids,
    });
    let serialized = serde_json::to_string(&material)
        .context("failed to canonicalize idle automatic compaction scope")?;
    Ok(stable_event_uuid(
        "sigil-idle-auto-compaction-scope",
        &serialized,
    ))
}

fn idle_auto_circuit_scope(
    session: &Session,
    preview: &V2CompactionPreview,
) -> Result<CompactionCircuitScopeV1> {
    let layout_material = serde_json::json!({
        "schema": "sigil-compaction-circuit-layout-v1",
        "active_compaction_id": &preview.active_compaction_id,
        "prior_folded_through": &preview.plan.prior_folded_through,
        "folded_event_ids": &preview.plan.folded_event_ids,
        "retained_event_ids": &preview.plan.retained_event_ids,
        "adaptive_tail": &preview.plan.adaptive_tail,
    });
    let layout_serialized = serde_json::to_string(&layout_material)
        .context("failed to canonicalize automatic compaction circuit layout")?;
    Ok(CompactionCircuitScopeV1 {
        source_cursor_event_id: preview
            .plan
            .base_stream_cursor
            .last_applied_event_id
            .clone(),
        layout_hash: stable_event_uuid("sigil-compaction-circuit-layout", &layout_serialized),
        route_fingerprint: stable_event_uuid(
            "sigil-compaction-circuit-route",
            &format!("{}:{}", session.provider_name(), session.model_name()),
        ),
    })
}

fn emergency_blocking_layer(
    preview: &V2CompactionPreview,
    current_input_tokens: u64,
) -> CompactionEmergencyBlockingLayerV1 {
    let adaptive_tail = &preview.plan.adaptive_tail;
    if adaptive_tail.active_turn_extended {
        CompactionEmergencyBlockingLayerV1::ActiveTurn
    } else if adaptive_tail.retained_token_upper_bound.saturating_mul(2) >= current_input_tokens {
        CompactionEmergencyBlockingLayerV1::RetainedConversation
    } else {
        CompactionEmergencyBlockingLayerV1::StableSystemAndTools
    }
}

#[cfg(test)]
#[path = "../tests/compaction_runtime_tests.rs"]
mod tests;
