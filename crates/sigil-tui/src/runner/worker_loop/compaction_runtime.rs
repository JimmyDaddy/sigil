use anyhow::{Context, Result, bail};
use sigil_kernel::{
    CompactionInitiation, CompactionLifecycleProjection, CompactionThresholdStatus,
    ContinuationModelOutputV1, FrozenProviderRequestMaterial, PortableSemanticCompactionOutcome,
    PortableSemanticCompactionPreflight, PortableSemanticCompactionRequest,
    PortableTargetRequestMaterial, ProviderNonGeneratingAttempt, ProviderPhysicalAttemptOutcome,
    ProviderPhysicalAttemptPurpose, ProviderRequestRejection, RuntimeContextCandidates,
    ToolOutputProjectionPolicy, V2CompactionPreview,
};

use super::{
    AdmittedQueuedConversationCandidate, AgentRunOptions, DEFAULT_TASK_VERIFICATION_SCOPE_HASH,
    ExactConversationPromptStore, JsonlSessionStore, PreparedQueuedConversationCandidate,
    QueuedConversationPressureAdmission, RootConfig, Session, build_workspace_snapshot,
    current_unix_time_ms, stable_event_uuid, stable_workspace_id,
};
use crate::runner::protocol::{V2CompactionAdmission, V2CompactionReview};

const IDLE_AUTO_COMPACTION_COOLDOWN_MS: u64 = 60_000;

/// User-visible explanation for the temporary V2 activation freeze.
///
/// The fold preview stays available so users can inspect their durable history, but no path may
/// change the active boundary until the correctness blockers in RFC-0025 are resolved.
pub(in crate::runner) const V2_COMPACTION_APPLY_FREEZE_REASON: &str =
    "V2 context compaction apply is temporarily frozen while correctness fixes are in progress";

fn v2_compaction_apply_is_frozen(initiation: &CompactionInitiation) -> bool {
    !matches!(
        initiation,
        CompactionInitiation::Manual
            | CompactionInitiation::IdleAutomatic { .. }
            | CompactionInitiation::PreTurnPressure { .. }
    )
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

/// Result of checking the idle-only automatic compaction policy after a completed chat run.
pub(in crate::runner) enum IdleAutoCompactionPreparation {
    NotRequested,
    NotHardThreshold,
    NoFoldableHistory,
    FailureLatched,
    CoolingDown { retry_after_unix_ms: u64 },
    AdmissionUnavailable { reason: String },
    Ready(Box<PendingV2Compaction>),
}

/// Process-local admission state kept between a confirmed `/compact` review and its apply.
///
/// It intentionally retains the frozen request only in memory. The durable checkpoint receives
/// just the session-bound fingerprint and proof through the K25.9 executor.
pub(in crate::runner) struct PendingV2Compaction {
    request_id: u64,
    session_scope_id: String,
    initiation: CompactionInitiation,
    idle_auto_scope_fingerprint: Option<String>,
    preflight: PortableSemanticCompactionPreflight,
    target_material: PortableTargetRequestMaterial,
    folded_event_count: usize,
}

/// Exact process-local material prepared for a portable V2 activation before its target proof is
/// handed to the durable executor. The frozen request stays private to the worker and is never
/// rendered or persisted.
struct PreparedPortableV2Compaction {
    request_id: u64,
    session_scope_id: String,
    initiation: CompactionInitiation,
    idle_auto_scope_fingerprint: Option<String>,
    cache_root: std::path::PathBuf,
    preflight: PortableSemanticCompactionPreflight,
    frozen_before_request: FrozenProviderRequestMaterial,
    frozen_target_request: FrozenProviderRequestMaterial,
    folded_event_count: usize,
}

impl PreparedPortableV2Compaction {
    fn into_pending(self) -> Result<PendingV2Compaction> {
        let target_material =
            sigil_runtime::deepseek_v4_flash_portable_target_material_with_economics(
                &self.cache_root,
                &self.frozen_before_request,
                self.frozen_target_request,
            )?;
        Ok(PendingV2Compaction {
            request_id: self.request_id,
            session_scope_id: self.session_scope_id,
            initiation: self.initiation,
            idle_auto_scope_fingerprint: self.idle_auto_scope_fingerprint,
            preflight: self.preflight,
            target_material,
            folded_event_count: self.folded_event_count,
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
        let logical_run_id =
            format!("overflow-input-token-measurement:{source_physical_attempt_id}");
        let mut measurement = ProviderNonGeneratingAttempt::start(
            session,
            &logical_run_id,
            &self.frozen_target_request,
            ProviderPhysicalAttemptPurpose::InputTokenMeasurement,
        )
        .await?;
        let target_material = match provider
            .prove_portable_compaction_target(self.frozen_target_request)
            .await
        {
            Ok(target_material) => {
                measurement
                    .finish(session, ProviderPhysicalAttemptOutcome::Completed)
                    .await?;
                let receipt = measurement
                    .completed_receipt()
                    .cloned()
                    .context("portable overflow input-token measurement has no durable receipt")?;
                self.preflight.admit_completed_input_token_measurement(
                    receipt,
                    target_material.frozen_request().fingerprint(),
                )?;
                target_material
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
                        "portable overflow input-token measurement failed after its durable start: {error:#}"
                    )));
                }
                return Err(error.context("portable overflow input-token measurement failed"));
            }
        };
        Ok(PendingV2Compaction {
            request_id: self.request_id,
            session_scope_id: self.session_scope_id,
            initiation: self.initiation,
            idle_auto_scope_fingerprint: self.idle_auto_scope_fingerprint,
            preflight: self.preflight,
            target_material,
            folded_event_count: self.folded_event_count,
        })
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
    pub(in crate::runner) fn apply_compaction(
        self,
        session: &Session,
        session_log_path: &std::path::Path,
    ) -> Result<(
        PreparedQueuedConversationCandidate,
        PortableSemanticCompactionOutcome,
    )> {
        let outcome = self.pending_compaction.apply(session, session_log_path)?;
        Ok((self.candidate, outcome))
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
            Self::Blocked { queue_id, reason } => formatter
                .debug_struct("QueuedConversationPreTurnAdmission::Blocked")
                .field("queue_id", queue_id)
                .field("reason", reason)
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

    pub(in crate::runner) fn idle_auto_scope_fingerprint(&self) -> Option<&str> {
        self.idle_auto_scope_fingerprint.as_deref()
    }

    pub(in crate::runner) fn frozen_target_request(&self) -> FrozenProviderRequestMaterial {
        self.target_material.frozen_request().clone()
    }

    pub(in crate::runner) fn apply(
        self,
        session: &Session,
        session_log_path: &std::path::Path,
    ) -> Result<PortableSemanticCompactionOutcome> {
        if v2_compaction_apply_is_frozen(&self.initiation) {
            bail!(V2_COMPACTION_APPLY_FREEZE_REASON);
        }
        if session.session_scope_id() != self.session_scope_id {
            bail!("reviewed V2 compaction belongs to a different session scope");
        }
        JsonlSessionStore::new(session_log_path)?
            .execute_portable_semantic_compaction(self.preflight, self.target_material)
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
    session: &Session,
    options: &AgentRunOptions,
    tools: Vec<sigil_kernel::ToolSpec>,
    source_physical_attempt_id: String,
    provider: &P,
) -> Result<PendingV2Compaction>
where
    P: sigil_kernel::Provider,
{
    let initiation = CompactionInitiation::OverflowRecovery {
        source_physical_attempt_id: source_physical_attempt_id.clone(),
    };
    if v2_compaction_apply_is_frozen(&initiation) {
        bail!(V2_COMPACTION_APPLY_FREEZE_REASON);
    }
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
    let effective_config = sigil_runtime::effective_compaction_config(
        session.provider_name(),
        session.model_name(),
        &options.compaction_config,
    );
    if !effective_config.enabled {
        bail!("overflow recovery requires enabled compaction");
    }
    let preview = session
        .v2_compaction_preview(effective_config.tail_messages)?
        .context("overflow recovery has no foldable V2 history")?;
    let target_input = PortableV2TargetRequestInput {
        tools,
        reasoning_effort: options.reasoning_effort.clone(),
        previous_response_handle: session.latest_response_handle(session.provider_name()),
        traffic_partition_key: options.traffic_partition_key.clone(),
        transient_messages: Vec::new(),
        runtime_context: RuntimeContextCandidates::default(),
    };
    prepare_portable_v2_compaction(
        request_id,
        initiation,
        root_config,
        workspace_root,
        session_log_path,
        session,
        &options.memory_config,
        target_input,
        preview,
    )?
    .into_server_count_pending(provider, session, &source_physical_attempt_id)
    .await
}

/// Prepares a read-only V2 review plus the exact process-local material needed for confirmation.
///
/// The returned review is always safe to render. An unavailable local tokenizer or exact proof
/// produces an unavailable admission instead of a durable lifecycle write.
pub(in crate::runner) fn prepare_v2_compaction_review(
    request_id: u64,
    root_config: &RootConfig,
    workspace_root: &std::path::Path,
    session_log_path: &std::path::Path,
    session: &Session,
    options: &AgentRunOptions,
    tools: Vec<sigil_kernel::ToolSpec>,
    preview: V2CompactionPreview,
) -> Result<(V2CompactionReview, Option<PendingV2Compaction>)> {
    prepare_v2_compaction(
        request_id,
        CompactionInitiation::Manual,
        root_config,
        workspace_root,
        session_log_path,
        session,
        options,
        tools,
        preview,
    )
}

/// Prepares the automatic K25.11 path without creating a modal or a durable attempt.
///
/// This is invoked only by the scheduler after a successful chat run and after it has proven
/// that no active run, queue item, or agent-result continuation remains. It never performs
/// provider I/O; the same local target admission as manual `/compact` remains mandatory.
#[allow(clippy::too_many_arguments)]
pub(in crate::runner) fn prepare_idle_auto_compaction(
    state: &mut IdleAutoCompactionState,
    root_config: &RootConfig,
    workspace_root: &std::path::Path,
    session_log_path: &std::path::Path,
    session: &Session,
    options: &AgentRunOptions,
    tools: Vec<sigil_kernel::ToolSpec>,
) -> Result<IdleAutoCompactionPreparation> {
    if !state.is_requested() {
        return Ok(IdleAutoCompactionPreparation::NotRequested);
    }

    let effective_config = sigil_runtime::effective_compaction_config(
        session.provider_name(),
        session.model_name(),
        &options.compaction_config,
    );
    if effective_config.threshold_status(session.stats().last_prompt_tokens)
        != CompactionThresholdStatus::Hard
    {
        state.consume_request();
        return Ok(IdleAutoCompactionPreparation::NotHardThreshold);
    }

    let Some(preview) = session.v2_compaction_preview(effective_config.tail_messages)? else {
        state.consume_request();
        return Ok(IdleAutoCompactionPreparation::NoFoldableHistory);
    };
    let scope_fingerprint = idle_auto_scope_fingerprint(session, &preview, &effective_config)?;
    let now = current_unix_time_ms();
    if let Some(retry_after_unix_ms) = state.retry_after(&scope_fingerprint)
        && now < retry_after_unix_ms
    {
        state.consume_request();
        return Ok(IdleAutoCompactionPreparation::CoolingDown {
            retry_after_unix_ms,
        });
    }

    if has_failed_idle_automatic_scope(session_log_path, &scope_fingerprint)? {
        state.consume_request();
        return Ok(IdleAutoCompactionPreparation::FailureLatched);
    }

    let (review, pending) = prepare_v2_compaction(
        0,
        CompactionInitiation::IdleAutomatic {
            scope_fingerprint: scope_fingerprint.clone(),
        },
        root_config,
        workspace_root,
        session_log_path,
        session,
        options,
        tools,
        preview,
    )?;
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
fn prepare_v2_compaction(
    request_id: u64,
    initiation: CompactionInitiation,
    root_config: &RootConfig,
    workspace_root: &std::path::Path,
    session_log_path: &std::path::Path,
    session: &Session,
    options: &AgentRunOptions,
    tools: Vec<sigil_kernel::ToolSpec>,
    preview: V2CompactionPreview,
) -> Result<(V2CompactionReview, Option<PendingV2Compaction>)> {
    let review = |admission| V2CompactionReview {
        request_id,
        preview: preview.clone(),
        admission,
    };
    if v2_compaction_apply_is_frozen(&initiation) {
        return Ok((
            review(V2CompactionAdmission::Unavailable {
                reason: V2_COMPACTION_APPLY_FREEZE_REASON.to_owned(),
            }),
            None,
        ));
    }
    let target_input = PortableV2TargetRequestInput {
        tools,
        reasoning_effort: options.reasoning_effort.clone(),
        previous_response_handle: session.latest_response_handle(session.provider_name()),
        traffic_partition_key: options.traffic_partition_key.clone(),
        transient_messages: Vec::new(),
        runtime_context: RuntimeContextCandidates::default(),
    };
    match prepare_portable_v2_compaction(
        request_id,
        initiation,
        root_config,
        workspace_root,
        session_log_path,
        session,
        &options.memory_config,
        target_input,
        preview.clone(),
    )
    .and_then(PreparedPortableV2Compaction::into_pending)
    {
        Ok(pending) => {
            let budget = &pending.target_material.proof().budget;
            let economics = pending
                .target_material
                .portable_economics()
                .context("portable target material has no before/after economics proof")?;
            match &pending.target_material.proof().input {
                sigil_kernel::InputTokenEvidence::Exact { tokens, .. } => Ok((
                    review(V2CompactionAdmission::Ready {
                        before_input_tokens: economics.before_input.admission_tokens(),
                        input_tokens: *tokens,
                        context_window_tokens: budget.context_window_tokens,
                        output_tokens: budget.requested_output_tokens,
                        safety_buffer_tokens: budget.safety_buffer_tokens,
                        savings_tokens: economics.savings_tokens,
                        savings_ratio_ppm: economics.savings_ratio_ppm,
                        minimum_savings_tokens: economics.minimum_savings_tokens,
                        minimum_savings_ratio_ppm: economics.minimum_savings_ratio_ppm,
                    }),
                    Some(pending),
                )),
                sigil_kernel::InputTokenEvidence::ConservativeUpperBound { .. } => Ok((
                    review(V2CompactionAdmission::Unavailable {
                        reason: "local exact target proof is unavailable".to_owned(),
                    }),
                    None,
                )),
            }
        }
        Err(error) => Ok((
            review(V2CompactionAdmission::Unavailable {
                reason: format!("local exact target proof is unavailable: {error:#}"),
            }),
            None,
        )),
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

#[allow(clippy::too_many_arguments)]
fn prepare_portable_v2_compaction(
    request_id: u64,
    initiation: CompactionInitiation,
    root_config: &RootConfig,
    workspace_root: &std::path::Path,
    session_log_path: &std::path::Path,
    session: &Session,
    memory_config: &sigil_kernel::MemoryConfig,
    target_input: PortableV2TargetRequestInput,
    preview: V2CompactionPreview,
) -> Result<PreparedPortableV2Compaction> {
    if sigil_runtime::is_deepseek_v4_flash_portable_target_profile(
        session.provider_name(),
        session.model_name(),
    ) {
        sigil_runtime::require_default_deepseek_v4_flash_portable_transport(root_config)?;
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
        CompactionInitiation::IdleAutomatic { scope_fingerprint } => {
            format!(
                "{}:idle-auto:{scope_fingerprint}",
                session.session_scope_id()
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
    let request = PortableSemanticCompactionRequest {
        attempt_id,
        compaction_id,
        initiation: initiation.clone(),
        base_projection_revision: "portable-v2-admission-r1".to_owned(),
        branch_id: None,
        valid_for_snapshot,
        objective: None,
        language: "en".to_owned(),
        plan: preview.plan.clone(),
        // K25.10B intentionally activates only deterministic task-memory and user-constraint
        // extraction. Semantic compressor I/O remains a later admitted stage.
        model_output: ContinuationModelOutputV1 {
            in_progress: Vec::new(),
            pending_actions: Vec::new(),
            provider_continuity: Vec::new(),
            model_notes: Vec::new(),
        },
        tool_output_projection_policy: ToolOutputProjectionPolicy::default(),
        started_at_unix_ms: now,
        completed_at_unix_ms: now,
    };
    let store = JsonlSessionStore::new(session_log_path)?;
    let preflight = store.prepare_portable_semantic_compaction(request)?;
    let target_max_tokens = sigil_runtime::portable_compaction_target_output_tokens(
        session.provider_name(),
        session.model_name(),
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
    Ok(PreparedPortableV2Compaction {
        request_id,
        session_scope_id: session.session_scope_id().to_owned(),
        initiation: initiation.clone(),
        idle_auto_scope_fingerprint: match initiation {
            CompactionInitiation::IdleAutomatic { scope_fingerprint } => Some(scope_fingerprint),
            CompactionInitiation::Manual
            | CompactionInitiation::PreTurnPressure { .. }
            | CompactionInitiation::OverflowRecovery { .. } => None,
        },
        cache_root: paths.cache_root,
        preflight,
        frozen_before_request,
        frozen_target_request,
        folded_event_count: preview.plan.folded_event_ids.len(),
    })
}

/// Completes the no-write pre-turn admission for the next queued conversation input.
///
/// Exact fit returns the frozen direct candidate. When the direct target exceeds the only
/// admitted local budget, this prepares and proves a second frozen request based on a portable
/// compaction preflight whose fold source is the current durable stream before queue promotion.
/// Neither branch appends a queue promotion, compaction lifecycle, capability registration, or
/// provider request.
#[allow(clippy::too_many_arguments)]
pub(in crate::runner) fn prepare_next_queued_conversation_pre_turn_admission(
    root_config: &RootConfig,
    workspace_root: &std::path::Path,
    session_log_path: &std::path::Path,
    session: &Session,
    exact_prompts: &ExactConversationPromptStore,
    memory_config: &sigil_kernel::MemoryConfig,
    tools: Vec<sigil_kernel::ToolSpec>,
    default_reasoning_effort: Option<sigil_kernel::ReasoningEffort>,
    traffic_partition_key: Option<String>,
) -> Result<QueuedConversationPreTurnAdmission> {
    let paths = sigil_runtime::resolve_sigil_paths(
        &root_config.storage,
        &root_config.session,
        workspace_root,
    );
    match super::prepare_next_queued_conversation_pressure_admission(
        session,
        exact_prompts,
        workspace_root,
        memory_config,
        tools,
        default_reasoning_effort,
        traffic_partition_key,
        &paths.cache_root,
    )
    .map_err(anyhow::Error::msg)?
    {
        QueuedConversationPressureAdmission::NoQueuedInput => {
            Ok(QueuedConversationPreTurnAdmission::NoQueuedInput)
        }
        QueuedConversationPressureAdmission::ExactFit(candidate) => {
            Ok(QueuedConversationPreTurnAdmission::ExactFit(candidate))
        }
        QueuedConversationPressureAdmission::Blocked { queue_id, reason } => {
            Ok(QueuedConversationPreTurnAdmission::Blocked { queue_id, reason })
        }
        QueuedConversationPressureAdmission::PortablePreflightRequired { candidate, .. } => {
            let queue_id = candidate.promotion.queue_id.clone();
            if v2_compaction_apply_is_frozen(&CompactionInitiation::PreTurnPressure {
                queue_id: queue_id.clone(),
            }) {
                return Ok(QueuedConversationPreTurnAdmission::Blocked {
                    queue_id,
                    reason: V2_COMPACTION_APPLY_FREEZE_REASON.to_owned(),
                });
            }
            let effective_config = sigil_runtime::effective_compaction_config(
                session.provider_name(),
                session.model_name(),
                &root_config.compaction,
            );
            if !effective_config.enabled {
                return Ok(QueuedConversationPreTurnAdmission::Blocked {
                    queue_id,
                    reason: "queued pre-turn portable compaction is disabled".to_owned(),
                });
            }
            match prepare_queued_portable_preflight(
                root_config,
                workspace_root,
                session_log_path,
                session,
                memory_config,
                *candidate,
            ) {
                Ok(Some(pending)) => Ok(
                    QueuedConversationPreTurnAdmission::PortablePreflightReady(Box::new(pending)),
                ),
                Ok(None) => Ok(QueuedConversationPreTurnAdmission::Blocked {
                    queue_id,
                    reason: "queued pre-turn portable compaction has no foldable prior history"
                        .to_owned(),
                }),
                Err(_) => Ok(QueuedConversationPreTurnAdmission::Blocked {
                    queue_id,
                    reason:
                        "queued pre-turn portable compaction is unavailable from the local target profile"
                            .to_owned(),
                }),
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn prepare_queued_portable_preflight(
    root_config: &RootConfig,
    workspace_root: &std::path::Path,
    session_log_path: &std::path::Path,
    session: &Session,
    memory_config: &sigil_kernel::MemoryConfig,
    mut candidate: PreparedQueuedConversationCandidate,
) -> Result<Option<PendingQueuedConversationPortablePreflight>> {
    let effective_config = sigil_runtime::effective_compaction_config(
        session.provider_name(),
        session.model_name(),
        &root_config.compaction,
    );
    let Some(preview) = session.v2_compaction_preview(effective_config.tail_messages)? else {
        return Ok(None);
    };
    if v2_compaction_apply_is_frozen(&CompactionInitiation::PreTurnPressure {
        queue_id: candidate.promotion.queue_id.clone(),
    }) {
        bail!(V2_COMPACTION_APPLY_FREEZE_REASON);
    }

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

    let runtime_context =
        sigil_runtime::context_candidates_from_safe_sources(workspace_root, exact_prompt, None)
            .unwrap_or_default();
    let direct_request = candidate.frozen_request.request();
    let mut transient_messages = vec![exact_user_message];
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
        session,
        memory_config,
        target_input,
        preview,
    )?;
    let post_compaction_frozen_request = prepared.frozen_target_request.clone();
    let pending_compaction = prepared.into_pending()?;
    candidate.frozen_request = post_compaction_frozen_request;
    Ok(Some(PendingQueuedConversationPortablePreflight {
        candidate,
        pending_compaction,
    }))
}

pub(in crate::runner) fn has_failed_idle_automatic_scope(
    session_log_path: &std::path::Path,
    scope_fingerprint: &str,
) -> Result<bool> {
    let records = JsonlSessionStore::read_event_records(session_log_path)?;
    Ok(CompactionLifecycleProjection::from_records(&records)?
        .has_failed_idle_automatic_scope(scope_fingerprint))
}

fn idle_auto_scope_fingerprint(
    session: &Session,
    preview: &V2CompactionPreview,
    effective_config: &sigil_kernel::CompactionConfig,
) -> Result<String> {
    let material = serde_json::json!({
        "schema": "sigil-idle-auto-compaction-scope-v1",
        "session_scope_id": session.session_scope_id(),
        "provider_name": session.provider_name(),
        "model_name": session.model_name(),
        "context_window_tokens": effective_config.context_window_tokens,
        "hard_threshold_ratio_bits": effective_config.hard_threshold_ratio.to_bits(),
        "tail_messages": effective_config.tail_messages,
        "target_output_tokens": sigil_runtime::deepseek_v4_flash_portable_target_output_tokens(),
        "target_policy_revision": 1,
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

#[cfg(test)]
#[path = "../tests/compaction_runtime_tests.rs"]
mod tests;
