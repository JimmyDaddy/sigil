use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use sigil_kernel::{
    Agent, AgentRunDisposition, AgentRunInput, AgentRunOptions, AgentRunPurpose,
    AgentRunTerminalReason, ControlEntry, ConversationRoute, ConversationRouteDecisionProjection,
    ConversationTurnRef, EventHandler, IntentAcceptanceAuthorityV1, IntentAdmissionContextV1,
    IntentStackId, ModelMessage, PlanApprovalExpiry, PlanApprovalPermission, PlanApprovalScope,
    PlanDecision, PlanDecisionActor, PlanDecisionRecordedEntry, PlanDraftCreatedEntry, PlanId,
    PlanPermissionGrantedEntry, PlanReviewAttemptEntry, PlanReviewAttemptId,
    PlanReviewAttemptStatus, PlanReviewId, PlanReviewProjection, PlanReviewSource,
    PlanReviewTerminalReason, PlanSourceRef, PlanTaskStartMode, ProviderPhysicalAttemptOutcome,
    RunEvent, Session, SessionLogEntry, SessionRef, StartPlanReviewAction,
    TaskCreatedFromPlanEntry, TaskId, TaskPlanEntry, TaskPlanStatus, TaskRunEntry, TaskRunStatus,
    admit_suggested_decomposition, append_task_intent_plan_admission, bind_task_plan_intents,
    build_workspace_snapshot, plan_review_attempt_id_for_review,
    plan_review_attempt_id_for_revision_ordinal, plan_review_child_session_ref,
    plan_review_finalizer_session_ref, plan_review_id_for_explicit_command,
    plan_review_no_draft_retry_contract_material, plan_review_plan_id_for_attempt,
    plan_review_system_prompt_contract_material, plan_task_input_from_draft, safe_persistence_text,
    stable_event_uuid, stable_workspace_id, task_id_from_plan_draft, task_plan_from_plan_draft,
};

use sigil_kernel::ApprovalHandler;

use crate::{RootConfig, attach_session_url_capability_store};

const PLAN_REVIEW_RESEARCH_MAX_MODEL_TURNS: usize = 4;
const PLAN_REVIEW_FINALIZATION_MAX_MODEL_TURNS: usize = 1;

/// Host-owned outcome of one plan review run.
#[derive(Debug, Clone)]
pub enum PlanReviewRunOutcome {
    DraftReady {
        draft: Box<PlanDraftCreatedEntry>,
    },
    AwaitingUserInput {
        request: Box<sigil_kernel::PublicUserInputRequestV1>,
    },
    CompletedWithoutDraft,
    Cancelled,
    Interrupted(String),
    Failed(String),
    SubmitOnlyProtocolViolation(String),
}

/// Host-bound request describing one read-only plan review run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanReviewRunRequest {
    pub plan_review_id: PlanReviewId,
    pub attempt_id: PlanReviewAttemptId,
    pub plan_id: PlanId,
    pub source: PlanReviewSource,
    pub source_turn: ConversationTurnRef,
    pub route_decision_id: Option<sigil_kernel::ConversationRouteDecisionId>,
    pub child_session_ref: SessionRef,
    pub finalizer_session_ref: SessionRef,
    pub revision_request_id: Option<sigil_kernel::UserInputRequestId>,
    pub attempt_ordinal: u32,
    pub base_plan_id: Option<PlanId>,
    pub base_plan_hash: Option<String>,
    pub objective: String,
    /// Exact workspace snapshot the draft will be bound to; direct promotion requires the
    /// workspace to be unchanged between review and `Run plan`.
    pub workspace_snapshot_id: Option<String>,
}

impl PlanReviewRunRequest {
    /// Builds the durable source reference bound into the plan artifact.
    #[must_use]
    pub fn plan_source_ref(&self) -> PlanSourceRef {
        PlanSourceRef {
            source_turn: Some(self.source_turn.clone()),
            route_decision_id: self.route_decision_id.clone(),
            plan_review_id: Some(self.plan_review_id.clone()),
            ..PlanSourceRef::default()
        }
    }

    /// Derives the retry-stable logical run id for the plan review child run.
    #[must_use]
    pub fn child_logical_run_id(&self) -> String {
        format!(
            "plan-review-{}-{}",
            self.plan_review_id.as_str(),
            self.attempt_id.as_str()
        )
    }
}

/// Shared application service for the read-only PlanReview lifecycle.
///
/// Explicit `/plan`, automatic `PlanReview` route decisions, and revisions all enter through this
/// coordinator. It owns the durable attempt lifecycle, the retry-stable child session, the typed
/// `submit_plan_draft` draft commit, and the RFC-0018 Plan-to-Task decision commands.
#[derive(Debug, Clone, Default)]
pub struct PlanReviewCoordinator;

/// Typed plan decision command shared by TUI, HTTP, and Desktop surfaces.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct PlanDecisionCommand {
    pub plan_id: String,
    pub expected_plan_hash: String,
    pub decision: PlanDecision,
}

/// Typed create-task-from-plan command shared by TUI, HTTP, and Desktop surfaces.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct CreateTaskFromPlanRequest {
    pub plan_id: String,
    pub expected_plan_hash: String,
    pub start_mode: PlanTaskStartMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_grant: Option<PlanApprovalPermission>,
}

/// Result of creating a durable task from an accepted plan.
#[derive(Debug, Clone)]
pub struct CreatedTaskFromPlan {
    pub task_id: TaskId,
    pub task_id_value: String,
    pub objective: String,
    pub entry: TaskCreatedFromPlanEntry,
    pub start_mode: PlanTaskStartMode,
    pub entries: Vec<SessionLogEntry>,
}

/// Result of recording a plan rejection.
#[derive(Debug, Clone)]
pub struct RejectedPlan {
    pub entry: PlanDecisionRecordedEntry,
    pub entries: Vec<SessionLogEntry>,
}

impl PlanReviewCoordinator {
    /// Prepares the plan review run for an accepted automatic `PlanReview` route decision.
    ///
    /// Validates the durable route decision, appends the idempotent `Started` attempt, and returns
    /// the host-bound run request. The model never supplies identity, timestamps, or authority.
    ///
    /// # Errors
    ///
    /// Returns an error when the decision is missing or conflicts, when the review is already
    /// terminal, or when the source turn is missing from the durable session.
    pub fn prepare_automatic_plan_review(
        session: &mut Session,
        action: &StartPlanReviewAction,
        workspace_snapshot_id: Option<String>,
        now_ms: u64,
    ) -> Result<PlanReviewRunRequest> {
        let decision_projection =
            ConversationRouteDecisionProjection::from_entries(session.entries());
        if decision_projection.has_conflicts() {
            bail!("conversation route decision projection contains conflicts");
        }
        let decision = decision_projection
            .decision(&action.decision_id)
            .ok_or_else(|| {
                anyhow!(
                    "plan review decision {} is not durable in this session",
                    action.decision_id.as_str()
                )
            })?;
        if decision.route != ConversationRoute::PlanReview {
            bail!(
                "route decision {} is not a plan review decision",
                action.decision_id.as_str()
            );
        }
        if decision.source_turn != action.source_turn {
            bail!("plan review action source turn conflicts with its route decision");
        }
        let objective = source_turn_objective(session, &action.source_turn).ok_or_else(|| {
            anyhow!(
                "plan review source user turn {} is not present",
                action.source_turn.message_id
            )
        })?;
        let review_projection = PlanReviewProjection::from_entries(session.entries());
        if review_projection.has_conflicts() {
            bail!("plan review projection contains conflicts");
        }
        if review_projection.is_terminal(&action.plan_review_id) {
            bail!(
                "plan review {} is already terminal",
                action.plan_review_id.as_str()
            );
        }
        let attempt_id = plan_review_attempt_id_for_review(&action.plan_review_id);
        let child_session_ref = plan_review_child_session_ref(&action.plan_review_id, &attempt_id);
        let finalizer_session_ref =
            plan_review_finalizer_session_ref(&action.plan_review_id, &attempt_id, 1);
        let request = PlanReviewRunRequest {
            plan_review_id: action.plan_review_id.clone(),
            attempt_id,
            plan_id: action.plan_id.clone(),
            source: PlanReviewSource::AutomaticConversationRoute,
            source_turn: action.source_turn.clone(),
            route_decision_id: Some(action.decision_id.clone()),
            workspace_snapshot_id,
            child_session_ref,
            finalizer_session_ref,
            revision_request_id: None,
            attempt_ordinal: 1,
            base_plan_id: None,
            base_plan_hash: None,
            objective,
        };
        Self::ensure_attempt_started(session, &request, now_ms)?;
        Ok(request)
    }

    /// Prepares the plan review run for an explicit `/plan` command.
    ///
    /// Explicit plan commands have no persisted provider-visible user turn, so the source turn is
    /// a host-derived identity bound to the session and the root logical run.
    ///
    /// # Errors
    ///
    /// Returns an error when the review is already terminal or the objective is empty.
    pub fn prepare_explicit_plan_review(
        session: &mut Session,
        prompt: &str,
        root_logical_run_id: &str,
        workspace_snapshot_id: Option<String>,
        now_ms: u64,
    ) -> Result<PlanReviewRunRequest> {
        let objective = safe_persistence_text(prompt);
        if objective.trim().is_empty() {
            bail!("explicit plan review objective is empty");
        }
        let plan_review_id =
            plan_review_id_for_explicit_command(session.session_scope_id(), root_logical_run_id);
        let attempt_id = plan_review_attempt_id_for_review(&plan_review_id);
        let plan_id = plan_review_plan_id_for_attempt(&plan_review_id, &attempt_id);
        let source_turn = ConversationTurnRef::new(
            session.session_scope_id(),
            format!("plan-review:{}", plan_review_id.as_str()),
            root_logical_run_id,
        )?;
        let review_projection = PlanReviewProjection::from_entries(session.entries());
        if review_projection.has_conflicts() {
            bail!("plan review projection contains conflicts");
        }
        if review_projection.is_terminal(&plan_review_id) {
            bail!(
                "plan review {} is already terminal",
                plan_review_id.as_str()
            );
        }
        let request = PlanReviewRunRequest {
            plan_review_id: plan_review_id.clone(),
            attempt_id: attempt_id.clone(),
            plan_id: plan_id.clone(),
            source: PlanReviewSource::ExplicitPlanCommand,
            source_turn,
            route_decision_id: None,
            child_session_ref: plan_review_child_session_ref(&plan_review_id, &attempt_id),
            finalizer_session_ref: plan_review_finalizer_session_ref(
                &plan_review_id,
                &attempt_id,
                1,
            ),
            revision_request_id: None,
            attempt_ordinal: 1,
            base_plan_id: None,
            base_plan_hash: None,
            objective,
            workspace_snapshot_id,
        };
        Self::ensure_attempt_started(session, &request, now_ms)?;
        Ok(request)
    }

    /// Runs the read-only plan review child session.
    ///
    /// The run uses the read-only tool registry, never writes to the parent session, and closes
    /// with a validated typed draft. Research is bounded independently from an ordinary agent run.
    /// If research reaches that bound, finishes without a draft, or loses a provider stream at a
    /// typed recoverable boundary, the host starts one submit-only finalization turn. A draft-less
    /// finalization closes with `CompletedWithoutDraft`.
    #[allow(clippy::too_many_arguments)]
    pub async fn run_plan_review<H, A>(
        parent_session: &mut Session,
        request: &PlanReviewRunRequest,
        agent: &Agent<impl sigil_kernel::Provider>,
        options: AgentRunOptions,
        tool_registry: sigil_kernel::ToolRegistry,
        handler: &mut H,
        approval_handler: &mut A,
        cancellation: sigil_kernel::RunCancellationHandle,
    ) -> Result<PlanReviewRunOutcome>
    where
        H: EventHandler + Send,
        A: ApprovalHandler + Send,
    {
        // Keep the coordinator API safe for every caller, including recovery and tests. Product
        // drivers also call this before registering their supervised run; the append is
        // idempotent for the exact same attempt binding.
        Self::ensure_attempt_started(parent_session, request, now_ms())?;
        // The host owns plan acceptance authority: the plan review run is always read-only,
        // regardless of the enclosing run's permission mode.
        let mut options = options;
        options.permission_config.mode = sigil_kernel::PermissionMode::ReadOnly;
        let mut child_session = build_plan_review_child_session(parent_session, request)?;
        let draft_context = sigil_kernel::PlanReviewDraftContext {
            plan_review_id: request.plan_review_id.clone(),
            attempt_id: request.attempt_id.clone(),
            plan_id: request.plan_id.clone(),
            source: request.plan_source_ref(),
            workspace_snapshot_id: request.workspace_snapshot_id.clone(),
        };
        if cancellation.is_cancel_requested() {
            return Ok(PlanReviewRunOutcome::Cancelled);
        }

        let existing_research_input = {
            let projection = child_session.user_input_projection()?;
            projection
                .public_requests()
                .into_iter()
                .filter(|candidate| {
                    matches!(
                        &candidate.source,
                        sigil_kernel::UserInputSourceV1::PlanReviewResearch {
                            plan_review_id,
                            attempt_id,
                        } if plan_review_id == &request.plan_review_id
                            && attempt_id == &request.attempt_id
                    )
                })
                .max_by_key(|candidate| candidate.requested_at_unix_ms)
                .and_then(|candidate| projection.request(&candidate.identity).cloned())
        };
        if let Some(state) = existing_research_input.as_ref()
            && state.status == sigil_kernel::UserInputStatusV1::Requested
        {
            return complete_plan_review_run(
                &cancellation,
                PlanReviewRunOutcome::AwaitingUserInput {
                    request: Box::new(state.public_view()),
                },
            );
        }
        if existing_research_input.as_ref().is_some_and(|state| {
            matches!(
                state.resolution.as_ref().map(|entry| &entry.resolution),
                Some(sigil_kernel::UserInputResolutionV1::RunCancelled)
            )
        }) {
            return complete_plan_review_run(&cancellation, PlanReviewRunOutcome::Cancelled);
        }

        let configured_max_turns = options.max_turns;
        let host_imposed_research_cap = configured_max_turns
            .is_none_or(|max_turns| max_turns > PLAN_REVIEW_RESEARCH_MAX_MODEL_TURNS);
        let mut research_options = options.clone();
        research_options.max_turns = Some(
            configured_max_turns
                .unwrap_or(PLAN_REVIEW_RESEARCH_MAX_MODEL_TURNS)
                .min(PLAN_REVIEW_RESEARCH_MAX_MODEL_TURNS),
        );
        let mut continuation_resolution = None;
        let mut research_logical_run_id = request.child_logical_run_id();
        let skip_research_cause = existing_research_input.as_ref().and_then(|state| {
            match state.resolution.as_ref().map(|entry| &entry.resolution) {
                Some(sigil_kernel::UserInputResolutionV1::Declined) => {
                    Some("the user declined a plan-research clarification".to_owned())
                }
                Some(sigil_kernel::UserInputResolutionV1::Consumed) => Some(
                    "a recovered plan-research continuation completed without a committed draft"
                        .to_owned(),
                ),
                Some(sigil_kernel::UserInputResolutionV1::Failed { failure_class, .. }) => Some(
                    format!("the prior plan-research continuation failed: {failure_class}"),
                ),
                _ => None,
            }
        });
        if skip_research_cause.is_some()
            && let Some(draft) = child_session
                .plan_artifact_projection()
                .plans
                .get(&request.plan_id)
                .cloned()
        {
            return complete_plan_review_run(
                &cancellation,
                PlanReviewRunOutcome::DraftReady {
                    draft: Box::new(draft),
                },
            );
        }
        let research = if skip_research_cause.is_some() {
            None
        } else {
            let research_input = if let Some(state) = existing_research_input.as_ref()
                && matches!(
                    state.status,
                    sigil_kernel::UserInputStatusV1::DecisionAccepted
                        | sigil_kernel::UserInputStatusV1::ContinuationClaimed
                        | sigil_kernel::UserInputStatusV1::ContinuationStarted
                ) {
                let physical_attempt_id = sigil_kernel::new_provider_physical_attempt_id();
                let prepared = sigil_kernel::prepare_user_input_continuation(
                    &mut child_session,
                    &state.requested.request.identity,
                    &state.requested.request_hash,
                    "plan-review-supervisor-v1",
                    &physical_attempt_id,
                    now_ms(),
                )?;
                research_logical_run_id = prepared
                    .continuation
                    .continuation_logical_run_id
                    .as_str()
                    .to_owned();
                continuation_resolution = Some((
                    state.requested.request.identity.clone(),
                    state.requested.request_hash.clone(),
                ));
                plan_review_continuation_input(
                    request,
                    &draft_context,
                    &cancellation,
                    &prepared.continuation,
                )
            } else {
                plan_review_run_input(request, &draft_context, &cancellation, None, 0)
            };
            Some(
                agent
                    .run_with_approval_input_and_tool_registry(
                        &mut child_session,
                        research_input,
                        research_options,
                        tool_registry,
                        handler,
                        approval_handler,
                    )
                    .await,
            )
        };
        if let Some((identity, request_hash)) = continuation_resolution.as_ref() {
            if research.as_ref().is_some_and(Result::is_ok) {
                child_session.append_user_input_lifecycle(vec![
                    sigil_kernel::UserInputLifecycleEntryV1::Resolved(
                        sigil_kernel::UserInputResolvedV1 {
                            schema_version: sigil_kernel::USER_INPUT_SCHEMA_VERSION,
                            identity: identity.clone(),
                            request_hash: request_hash.clone(),
                            resolution: sigil_kernel::UserInputResolutionV1::Consumed,
                            resolved_at_unix_ms: now_ms(),
                        },
                    ),
                ])?;
            } else {
                sigil_kernel::reconcile_user_input_continuation_after_failed_run(
                    &mut child_session,
                    identity,
                    request_hash,
                    now_ms(),
                )?;
            }
        }
        let recovery_cause = match research {
            None => skip_research_cause,
            Some(research) => {
                match research {
                    Ok(_) if cancellation.is_cancel_requested() => {
                        return Ok(PlanReviewRunOutcome::Cancelled);
                    }
                    Ok(output) => match output.disposition {
                        AgentRunDisposition::PlanReviewDraftSubmitted(action) => {
                            let outcome =
                                plan_review_draft_ready_outcome(&child_session, &action.plan_id)?;
                            return complete_plan_review_run(&cancellation, outcome);
                        }
                        AgentRunDisposition::FinalAnswer => None,
                        AgentRunDisposition::AwaitingUserInput(request_ref) => {
                            let request = child_session
                        .user_input_projection()?
                        .request(&request_ref.identity)
                        .filter(|state| {
                            state.requested.request_hash == request_ref.request_hash
                                && matches!(
                                    state.requested.request.source,
                                    sigil_kernel::UserInputSourceV1::PlanReviewResearch {
                                        ref plan_review_id,
                                        ref attempt_id,
                                    } if plan_review_id == &request.plan_review_id
                                        && attempt_id == &request.attempt_id
                                )
                        })
                        .map(sigil_kernel::UserInputRequestStateV1::public_view)
                        .context("plan review research suspension lost its exact durable request")?;
                            return complete_plan_review_run(
                                &cancellation,
                                PlanReviewRunOutcome::AwaitingUserInput {
                                    request: Box::new(request),
                                },
                            );
                        }
                        AgentRunDisposition::Interrupted
                            if host_imposed_research_cap
                                && output.outcome.terminal_reason
                                    == AgentRunTerminalReason::MaxTurns =>
                        {
                            handler.handle(RunEvent::Notice(format!(
                        "Plan review research reached its {PLAN_REVIEW_RESEARCH_MAX_MODEL_TURNS}-turn bound; continuing with one submit-only finalization turn."
                    )))?;
                            None
                        }
                        AgentRunDisposition::Interrupted => {
                            return complete_plan_review_run(
                                &cancellation,
                                PlanReviewRunOutcome::Interrupted(
                                    "plan review run was interrupted before a draft".to_owned(),
                                ),
                            );
                        }
                        AgentRunDisposition::Blocked => {
                            return complete_plan_review_run(
                                &cancellation,
                                PlanReviewRunOutcome::Failed(
                                    "plan review run was blocked before a draft".to_owned(),
                                ),
                            );
                        }
                        AgentRunDisposition::StartDurableTask(_)
                        | AgentRunDisposition::ContinueDurableTask(_)
                        | AgentRunDisposition::TaskPlanAccepted => {
                            return complete_plan_review_run(
                                &cancellation,
                                PlanReviewRunOutcome::Failed(
                                    "plan review run attempted an out-of-scope handoff".to_owned(),
                                ),
                            );
                        }
                        AgentRunDisposition::StartPlanReview(_) => {
                            return complete_plan_review_run(
                                &cancellation,
                                PlanReviewRunOutcome::Failed(
                                    "plan review run requested a nested plan review".to_owned(),
                                ),
                            );
                        }
                    },
                    Err(error) => {
                        if cancellation.is_cancel_requested() {
                            return Ok(PlanReviewRunOutcome::Cancelled);
                        }
                        let recovery_allowed =
                            match plan_review_provider_terminal_allows_submit_only_recovery(
                                &child_session,
                                &research_logical_run_id,
                            ) {
                                Ok(recovery_allowed) => recovery_allowed,
                                Err(projection_error) => {
                                    return Err(error.context(format!(
                        "plan review provider failure could not be classified from durable physical-attempt evidence: {projection_error:#}"
                    )));
                                }
                            };
                        if !recovery_allowed {
                            return Err(error);
                        }
                        handler.handle(RunEvent::Notice(
                    "Plan review provider stream ended before a durable result; continuing with one submit-only finalization turn from the recorded read-only evidence."
                        .to_owned(),
                ))?;
                        Some(format!("{error:#}"))
                    }
                }
            }
        };

        if cancellation.is_cancel_requested() {
            return Ok(PlanReviewRunOutcome::Cancelled);
        }
        append_attempt_status(
            parent_session,
            request,
            PlanReviewAttemptStatus::Finalizing,
            None,
            now_ms(),
        )?;
        let evidence = plan_review_finalizer_evidence_bundle(request, &child_session);
        let mut last_violation = None;
        for corrective_ordinal in 1..=2 {
            if cancellation.is_cancel_requested() {
                return Ok(PlanReviewRunOutcome::Cancelled);
            }
            let mut finalizer_session =
                build_plan_review_finalizer_session(parent_session, request, corrective_ordinal)?;
            if let Some(draft) = finalizer_session
                .plan_artifact_projection()
                .plans
                .get(&request.plan_id)
                .cloned()
            {
                return complete_plan_review_run(
                    &cancellation,
                    PlanReviewRunOutcome::DraftReady {
                        draft: Box::new(draft),
                    },
                );
            }
            let mut finalization_options = options.clone();
            finalization_options.max_turns = Some(PLAN_REVIEW_FINALIZATION_MAX_MODEL_TURNS);
            let finalization_input = plan_review_run_input(
                request,
                &draft_context,
                &cancellation,
                Some(&evidence),
                corrective_ordinal,
            );
            let finalization = agent
                .run_with_approval_input_and_tool_registry(
                    &mut finalizer_session,
                    finalization_input,
                    finalization_options,
                    sigil_kernel::ToolRegistry::new(),
                    handler,
                    approval_handler,
                )
                .await;
            let output = match finalization {
                Ok(_) if cancellation.is_cancel_requested() => {
                    return Ok(PlanReviewRunOutcome::Cancelled);
                }
                Ok(output) => output,
                Err(_) if cancellation.is_cancel_requested() => {
                    return Ok(PlanReviewRunOutcome::Cancelled);
                }
                Err(error) => {
                    let context = recovery_cause.as_deref().map_or_else(
                        || "plan review submit-only finalization failed".to_owned(),
                        |cause| {
                            format!(
                                "plan review submit-only recovery failed after research stream ended early ({cause})"
                            )
                        },
                    );
                    return Err(error.context(context));
                }
            };
            if finalizer_session.entries().iter().any(|entry| {
                matches!(
                    entry,
                    SessionLogEntry::Assistant(message)
                        if message.tool_calls.iter().any(|call| {
                            call.name != sigil_kernel::SUBMIT_PLAN_DRAFT_TOOL_NAME
                        })
                )
            }) {
                let reason = "submit-only finalizer attempted a non-submit tool".to_owned();
                last_violation = Some(reason.clone());
                if corrective_ordinal == 1 {
                    handler.handle(RunEvent::Notice(
                        "Plan finalization attempted an unavailable research tool; retrying once in a fresh submit-only context."
                            .to_owned(),
                    ))?;
                    continue;
                }
                return complete_plan_review_run(
                    &cancellation,
                    PlanReviewRunOutcome::SubmitOnlyProtocolViolation(reason),
                );
            }
            if let Some(reason) = invalid_plan_draft_submission_reason(&finalizer_session) {
                last_violation = Some(reason.clone());
                if corrective_ordinal == 1 {
                    handler.handle(RunEvent::Notice(
                        "Plan finalization produced an invalid typed draft; retrying once in a fresh submit-only context."
                            .to_owned(),
                    ))?;
                    continue;
                }
                return complete_plan_review_run(
                    &cancellation,
                    PlanReviewRunOutcome::SubmitOnlyProtocolViolation(reason),
                );
            }
            let outcome = match output.disposition {
                AgentRunDisposition::PlanReviewDraftSubmitted(action) => {
                    plan_review_draft_ready_outcome(&finalizer_session, &action.plan_id)?
                }
                AgentRunDisposition::FinalAnswer => PlanReviewRunOutcome::CompletedWithoutDraft,
                AgentRunDisposition::AwaitingUserInput(_) => PlanReviewRunOutcome::Failed(
                    "plan review finalization exposed an unsupported user-input suspension"
                        .to_owned(),
                ),
                AgentRunDisposition::Interrupted => PlanReviewRunOutcome::Interrupted(
                    "plan review finalization was interrupted before a draft".to_owned(),
                ),
                AgentRunDisposition::Blocked => PlanReviewRunOutcome::Failed(
                    "plan review finalization was blocked before a draft".to_owned(),
                ),
                AgentRunDisposition::StartDurableTask(_)
                | AgentRunDisposition::ContinueDurableTask(_)
                | AgentRunDisposition::TaskPlanAccepted => PlanReviewRunOutcome::Failed(
                    "plan review finalization attempted an out-of-scope handoff".to_owned(),
                ),
                AgentRunDisposition::StartPlanReview(_) => PlanReviewRunOutcome::Failed(
                    "plan review finalization requested a nested plan review".to_owned(),
                ),
            };
            return complete_plan_review_run(&cancellation, outcome);
        }
        complete_plan_review_run(
            &cancellation,
            PlanReviewRunOutcome::SubmitOnlyProtocolViolation(
                last_violation.unwrap_or_else(|| "submit-only finalizer failed".to_owned()),
            ),
        )
    }

    /// Commits a validated draft from the plan review child session into the parent session.
    ///
    /// The parent append is idempotent: identical drafts are skipped, conflicting durable facts
    /// fail closed. The attempt transitions to `DraftReady`.
    ///
    /// # Errors
    ///
    /// Returns an error when the draft conflicts with durable facts or the attempt transition is
    /// invalid.
    pub fn commit_draft_from_child(
        parent: &mut Session,
        draft: &PlanDraftCreatedEntry,
        request: &PlanReviewRunRequest,
        now_ms: u64,
    ) -> Result<()> {
        validate_plan_review_child_draft(draft, request)?;
        let projection = parent.plan_artifact_projection();
        let draft_missing = match projection.plans.get(&request.plan_id) {
            Some(existing) if existing == draft => false,
            Some(_) => {
                bail!(
                    "plan {} already has conflicting durable facts",
                    request.plan_id.as_str()
                );
            }
            None => true,
        };
        let attempt_entry = plan_review_attempt_status_entry(
            parent,
            request,
            PlanReviewAttemptStatus::DraftReady,
            None,
            now_ms,
        )?;
        let mut controls = Vec::new();
        if draft_missing {
            controls.push(ControlEntry::PlanDraftCreated(draft.clone()));
        }
        if let Some(attempt_entry) = attempt_entry {
            controls.push(ControlEntry::PlanReviewAttempt(attempt_entry));
        }
        if let (Some(base_plan_id), Some(base_plan_hash)) = (
            request.base_plan_id.as_ref(),
            request.base_plan_hash.as_ref(),
        ) {
            let latest = projection.latest_decision(base_plan_id);
            match latest.map(|entry| entry.decision) {
                Some(PlanDecision::RevisionSucceeded) => {}
                Some(PlanDecision::RevisionRequested) => {
                    controls.push(ControlEntry::PlanDecisionRecorded(
                        PlanDecisionRecordedEntry {
                            plan_id: base_plan_id.clone(),
                            plan_hash: base_plan_hash.clone(),
                            decision: PlanDecision::RevisionSucceeded,
                            decided_by: PlanDecisionActor::System,
                            decided_at_ms: now_ms,
                            reason: Some(format!(
                                "superseded by revised plan {}",
                                request.plan_id.as_str()
                            )),
                        },
                    ));
                }
                other => bail!("revision base plan is not awaiting success: {:?}", other),
            }
        }
        if controls.is_empty() {
            Ok(())
        } else {
            parent.append_controls(controls)
        }
    }

    /// Durably closes a plan review run that terminated without a committed draft.
    ///
    /// `Cancelled` and `Failed` outcomes append the exact terminal attempt status instead of
    /// leaving a dangling `Started` attempt that recovery would later guess as `Interrupted`.
    ///
    /// # Errors
    ///
    /// Returns an error when the attempt transition is invalid or conflicts with durable facts.
    pub fn close_plan_review_run(
        session: &mut Session,
        request: &PlanReviewRunRequest,
        outcome: &PlanReviewRunOutcome,
        now_ms: u64,
    ) -> Result<()> {
        let closed = match outcome {
            PlanReviewRunOutcome::DraftReady { .. }
            | PlanReviewRunOutcome::CompletedWithoutDraft => Ok(()),
            PlanReviewRunOutcome::AwaitingUserInput { request: pending } => {
                append_attempt_status_with_pending_input(
                    session,
                    request,
                    PlanReviewAttemptStatus::WaitingForInput,
                    Some((**pending).clone()),
                    now_ms,
                )
            }
            PlanReviewRunOutcome::Cancelled => append_attempt_status(
                session,
                request,
                PlanReviewAttemptStatus::Cancelled,
                Some(PlanReviewTerminalReason::UserCancelled),
                now_ms,
            ),
            PlanReviewRunOutcome::Interrupted(_) => append_attempt_status(
                session,
                request,
                PlanReviewAttemptStatus::Interrupted,
                Some(PlanReviewTerminalReason::RunInterrupted),
                now_ms,
            ),
            PlanReviewRunOutcome::Failed(_) => append_attempt_status(
                session,
                request,
                PlanReviewAttemptStatus::Failed,
                Some(PlanReviewTerminalReason::RunFailed),
                now_ms,
            ),
            PlanReviewRunOutcome::SubmitOnlyProtocolViolation(_) => append_attempt_status(
                session,
                request,
                PlanReviewAttemptStatus::Failed,
                Some(PlanReviewTerminalReason::SubmitOnlyProtocolViolation),
                now_ms,
            ),
        };
        closed?;
        if let (Some(base_plan_id), Some(base_plan_hash)) = (
            request.base_plan_id.as_ref(),
            request.base_plan_hash.as_ref(),
        ) && !matches!(
            outcome,
            PlanReviewRunOutcome::DraftReady { .. }
                | PlanReviewRunOutcome::AwaitingUserInput { .. }
        ) {
            let reason = match outcome {
                PlanReviewRunOutcome::CompletedWithoutDraft => "revision completed without draft",
                PlanReviewRunOutcome::Cancelled => "revision cancelled",
                PlanReviewRunOutcome::Interrupted(_) => "revision interrupted",
                PlanReviewRunOutcome::Failed(_) => "revision failed",
                PlanReviewRunOutcome::SubmitOnlyProtocolViolation(_) => {
                    "revision submit-only protocol violation"
                }
                PlanReviewRunOutcome::AwaitingUserInput { .. } => unreachable!(),
                PlanReviewRunOutcome::DraftReady { .. } => unreachable!(),
            };
            Self::record_revision_failure(session, base_plan_id, base_plan_hash, reason, now_ms)?;
        }
        Ok(())
    }

    /// Closes a plan review run unless the attempt is already terminal or was never started.
    ///
    /// Used by executors on the error path of a run that failed before producing an outcome:
    /// a dangling `Started` attempt would otherwise be misread by recovery as a crash. When the
    /// attempt already carries a terminal status (e.g. a concurrent closer won), this is a no-op.
    ///
    /// # Errors
    ///
    /// Returns an error when the attempt is open and its terminal transition fails.
    pub fn close_plan_review_run_if_open(
        session: &mut Session,
        request: &PlanReviewRunRequest,
        outcome: &PlanReviewRunOutcome,
        now_ms: u64,
    ) -> Result<()> {
        let projection = PlanReviewProjection::from_entries(session.entries());
        let Some(existing) = projection.latest_attempt(&request.plan_review_id) else {
            return Ok(());
        };
        if existing.attempt_id == request.attempt_id && existing.status.is_terminal() {
            if let (Some(base_plan_id), Some(base_plan_hash)) = (
                request.base_plan_id.as_ref(),
                request.base_plan_hash.as_ref(),
            ) {
                Self::record_revision_failure(
                    session,
                    base_plan_id,
                    base_plan_hash,
                    "revision terminal closure recovered",
                    now_ms,
                )?;
            }
            return Ok(());
        }
        Self::close_plan_review_run(session, request, outcome, now_ms)
    }

    /// Closes an automatic plan review that produced no draft.
    ///
    /// # Errors
    ///
    /// Returns an error when the attempt transition is invalid.
    pub fn complete_without_draft(
        session: &mut Session,
        request: &PlanReviewRunRequest,
        now_ms: u64,
    ) -> Result<()> {
        append_attempt_status(
            session,
            request,
            PlanReviewAttemptStatus::CompletedWithoutDraft,
            Some(PlanReviewTerminalReason::NoDraftAfterRetry),
            now_ms,
        )?;
        if let (Some(base_plan_id), Some(base_plan_hash)) = (
            request.base_plan_id.as_ref(),
            request.base_plan_hash.as_ref(),
        ) {
            Self::record_revision_failure(
                session,
                base_plan_id,
                base_plan_hash,
                "revision completed without draft",
                now_ms,
            )?;
        }
        Ok(())
    }

    /// Prepares a revision attempt for an existing plan review lifecycle.
    ///
    /// Creates or replays the host-owned question that must precede every first revision run.
    pub fn request_plan_revision_guidance(
        session: &mut Session,
        plan_id: &PlanId,
        expected_plan_hash: &str,
        now_ms: u64,
    ) -> Result<sigil_kernel::UserInputRequestedV1> {
        let projection = session.plan_artifact_projection();
        let draft =
            projection.plans.get(plan_id).cloned().ok_or_else(|| {
                anyhow!("plan {} is not present in this session", plan_id.as_str())
            })?;
        if draft.plan_hash != expected_plan_hash {
            bail!(
                "plan {} is stale: expected {}, current {}",
                plan_id.as_str(),
                expected_plan_hash,
                draft.plan_hash
            );
        }
        let request_id = sigil_kernel::UserInputRequestId::new(stable_event_uuid(
            "sigil-plan-revision-request-v1",
            &format!("{}|{}", plan_id.as_str(), expected_plan_hash),
        ))?;
        let user_input = session.user_input_projection()?;
        if let Some(existing) = user_input
            .public_requests()
            .into_iter()
            .filter(|request| request.identity.request_id == request_id)
            .max_by_key(|request| request.identity.generation)
            && existing.status == sigil_kernel::UserInputStatusV1::Requested
        {
            return session
                .entries()
                .iter()
                .rev()
                .find_map(|entry| match entry {
                    SessionLogEntry::Control(ControlEntry::UserInputRequested(requested))
                        if requested.request.identity == existing.identity =>
                    {
                        Some((**requested).clone())
                    }
                    _ => None,
                })
                .context("pending revision guidance lost its durable request");
        }
        ensure_plan_action_allowed(
            session,
            plan_id,
            expected_plan_hash,
            sigil_kernel::PublicPlanAction::Revise,
        )?;
        if projection.plan_is_rejected(plan_id) {
            bail!("plan {} was rejected", plan_id.as_str());
        }
        if projection.task_created_for_plan(plan_id) {
            bail!("plan {} already created a task", plan_id.as_str());
        }
        if let Some(existing) = projection.latest_decision(plan_id)
            && !matches!(
                existing.decision,
                PlanDecision::RevisionFailed | PlanDecision::SavedOnly
            )
        {
            bail!(
                "plan {} already has decision {}",
                plan_id.as_str(),
                existing.decision.as_str()
            );
        }
        let review_projection = PlanReviewProjection::from_entries(session.entries());
        if review_projection.has_conflicts() {
            bail!("plan review projection contains conflicts");
        }
        let previous = review_projection
            .attempt_for_plan(plan_id)
            .cloned()
            .ok_or_else(|| {
                anyhow!(
                    "plan {} is not bound to a plan review lifecycle",
                    plan_id.as_str()
                )
            })?;
        let generation = user_input
            .public_requests()
            .into_iter()
            .filter(|request| request.identity.request_id == request_id)
            .map(|request| request.identity.generation)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        let root_logical_run_id = sigil_kernel::LogicalRunId::new(stable_event_uuid(
            "sigil-plan-revision-root-run-v1",
            previous.plan_review_id.as_str(),
        ))?;
        let source_thread_id = sigil_kernel::AgentThreadId::new("main")?;
        let source_binding_hash = sigil_kernel::stable_event_hash(format!(
            "{}|{}|{}|{}",
            session.session_scope_id(),
            plan_id.as_str(),
            expected_plan_hash,
            previous.attempt_id.as_str()
        ));
        let requested =
            sigil_kernel::UserInputRequestedV1::new(sigil_kernel::UserInputRequestV1 {
                schema_version: sigil_kernel::USER_INPUT_SCHEMA_VERSION,
                identity: sigil_kernel::UserInputIdentityV1 {
                    session_scope_id: sigil_kernel::SessionScopeId::new(
                        session.session_scope_id(),
                    )?,
                    root_logical_run_id,
                    source_thread_id,
                    request_id,
                    generation,
                    source_binding_hash,
                },
                source: sigil_kernel::UserInputSourceV1::PlanRevision {
                    base_plan_id: plan_id.clone(),
                    base_plan_hash: expected_plan_hash.to_owned(),
                },
                purpose: sigil_kernel::UserInputPurposeV1::RevisionGuidance,
                prompt: "What should change in this plan?".to_owned(),
                questions: vec![sigil_kernel::UserInputQuestionV1 {
                    id: "revision_guidance".to_owned(),
                    header: "Revision guidance".to_owned(),
                    question: "Describe the changes you want before a new plan is prepared."
                        .to_owned(),
                    description: Some(
                        "The original plan remains available until a revised draft succeeds."
                            .to_owned(),
                    ),
                    required: true,
                    field: sigil_kernel::UserInputFieldKindV1::Text {
                        multiline: true,
                        max_chars: 2_000,
                    },
                }],
                allowed_actions: vec![
                    sigil_kernel::UserInputActionV1::Submit,
                    sigil_kernel::UserInputActionV1::Decline,
                ],
                requested_at_unix_ms: now_ms,
                continuation: None,
            })?;
        session.append_user_input_lifecycle(vec![
            sigil_kernel::UserInputLifecycleEntryV1::Requested(Box::new(requested.clone())),
        ])?;
        Ok(requested)
    }

    fn plan_review_revision_request(
        session: &Session,
        base_plan_id: &PlanId,
        base_plan_hash: &str,
        revision_request_id: sigil_kernel::UserInputRequestId,
        guidance: &str,
        workspace_snapshot_id: Option<String>,
    ) -> Result<PlanReviewRunRequest> {
        let review_projection = PlanReviewProjection::from_entries(session.entries());
        if review_projection.has_conflicts() {
            bail!("plan review projection contains conflicts");
        }
        let base = review_projection
            .attempt_for_plan(base_plan_id)
            .cloned()
            .context("revision base plan is not bound to a plan review lifecycle")?;
        let latest = review_projection
            .latest_attempt(&base.plan_review_id)
            .cloned()
            .context("revision lifecycle has no attempt")?;
        let ordinal = if latest.revision_request_id.as_ref() == Some(&revision_request_id) {
            latest.attempt_ordinal.saturating_add(1)
        } else {
            1
        };
        let attempt_id = plan_review_attempt_id_for_revision_ordinal(
            &base.plan_review_id,
            &revision_request_id,
            ordinal,
        );
        let next_plan_id = plan_review_plan_id_for_attempt(&base.plan_review_id, &attempt_id);
        let original_objective = source_turn_objective(session, &base.source_turn)
            .filter(|objective| !objective.trim().is_empty())
            .unwrap_or_else(|| "Revise the active plan".to_owned());
        let objective = format!(
            "{original_objective}\n\nUser revision guidance:\n{}",
            safe_persistence_text(guidance)
        );
        Ok(PlanReviewRunRequest {
            plan_review_id: base.plan_review_id.clone(),
            attempt_id: attempt_id.clone(),
            plan_id: next_plan_id,
            source: base.source,
            source_turn: base.source_turn,
            route_decision_id: base.route_decision_id,
            child_session_ref: plan_review_child_session_ref(&base.plan_review_id, &attempt_id),
            finalizer_session_ref: plan_review_finalizer_session_ref(
                &base.plan_review_id,
                &attempt_id,
                1,
            ),
            revision_request_id: Some(revision_request_id),
            attempt_ordinal: ordinal,
            base_plan_id: Some(base_plan_id.clone()),
            base_plan_hash: Some(base_plan_hash.to_owned()),
            objective,
            workspace_snapshot_id,
        })
    }

    /// Atomically accepts host-owned revision guidance and records the matching base-plan
    /// revision decision before returning any executable run authority.
    pub fn accept_plan_revision_guidance(
        session: &mut Session,
        command: sigil_kernel::UserInputDecisionCommandV1,
        workspace_snapshot_id: Option<String>,
        now_ms: u64,
    ) -> Result<(
        sigil_kernel::UserInputDecisionReceiptV1,
        Option<PlanReviewRunRequest>,
    )> {
        let projection = session.user_input_projection()?;
        if let Some(previous) = projection.request_for_command(&command.command_id).cloned() {
            let receipt = sigil_kernel::accept_user_input_decision(session, command, now_ms)?;
            let revision_request = Self::recover_unstarted_revision_request(
                session,
                &previous,
                workspace_snapshot_id,
            )?;
            return Ok((receipt, revision_request));
        }
        let state = projection
            .request(&command.identity)
            .cloned()
            .context("revision guidance decision references an unknown request")?;
        if state.requested.request_hash != command.request_hash {
            bail!("revision guidance decision does not bind the exact request hash");
        }
        let (base_plan_id, base_plan_hash) = match &state.requested.request.source {
            sigil_kernel::UserInputSourceV1::PlanRevision {
                base_plan_id,
                base_plan_hash,
            } => (base_plan_id.clone(), base_plan_hash.clone()),
            _ => bail!("user-input request is not plan revision guidance"),
        };
        let accepted = sigil_kernel::UserInputDecisionAcceptedV1::new(
            &state.requested,
            command.command_id,
            command.decision,
            now_ms,
        )?;
        let (resolution, revision_request, plan_decision) = match &accepted.decision {
            sigil_kernel::UserInputDurableDecisionV1::Submitted {
                answers: Some(answers),
                ..
            } => {
                let guidance = answers
                    .iter()
                    .find(|answer| answer.question_id == "revision_guidance")
                    .and_then(|answer| match &answer.value {
                        sigil_kernel::UserInputAnswerValueV1::Text { value } => {
                            Some(value.as_str())
                        }
                        _ => None,
                    })
                    .context("revision guidance answer is missing its text value")?;
                let plan_projection = session.plan_artifact_projection();
                let draft = plan_projection
                    .plans
                    .get(&base_plan_id)
                    .context("revision guidance base plan is missing")?;
                if draft.plan_hash != base_plan_hash {
                    bail!("revision guidance base plan hash is stale");
                }
                if let Some(existing) = plan_projection.latest_decision(&base_plan_id)
                    && !matches!(
                        existing.decision,
                        PlanDecision::RevisionFailed | PlanDecision::SavedOnly
                    )
                {
                    bail!(
                        "revision guidance base plan already has decision {}",
                        existing.decision.as_str()
                    );
                }
                let revision_request = Self::plan_review_revision_request(
                    session,
                    &base_plan_id,
                    &base_plan_hash,
                    state.requested.request.identity.request_id.clone(),
                    guidance,
                    workspace_snapshot_id,
                )?;
                (
                    sigil_kernel::UserInputResolutionV1::Consumed,
                    Some(revision_request),
                    Some(PlanDecisionRecordedEntry {
                        plan_id: base_plan_id.clone(),
                        plan_hash: base_plan_hash.clone(),
                        decision: PlanDecision::RevisionRequested,
                        decided_by: PlanDecisionActor::User,
                        decided_at_ms: now_ms,
                        reason: Some(safe_persistence_text(guidance)),
                    }),
                )
            }
            sigil_kernel::UserInputDurableDecisionV1::Submitted { answers: None, .. } => {
                bail!("revision guidance answer values must be persisted")
            }
            sigil_kernel::UserInputDurableDecisionV1::Declined => {
                (sigil_kernel::UserInputResolutionV1::Declined, None, None)
            }
            sigil_kernel::UserInputDurableDecisionV1::RunCancelled => (
                sigil_kernel::UserInputResolutionV1::RunCancelled,
                None,
                None,
            ),
        };
        let resolved = sigil_kernel::UserInputResolvedV1 {
            schema_version: sigil_kernel::USER_INPUT_SCHEMA_VERSION,
            identity: state.requested.request.identity.clone(),
            request_hash: state.requested.request_hash.clone(),
            resolution,
            resolved_at_unix_ms: now_ms,
        };
        let mut controls = vec![
            sigil_kernel::UserInputLifecycleEntryV1::DecisionAccepted(Box::new(accepted))
                .into_control(),
        ];
        if let Some(plan_decision) = plan_decision {
            controls.push(ControlEntry::PlanDecisionRecorded(plan_decision));
        }
        controls.push(sigil_kernel::UserInputLifecycleEntryV1::Resolved(resolved).into_control());
        session.append_controls(controls)?;
        let current = session
            .user_input_projection()?
            .request(&state.requested.request.identity)
            .cloned()
            .context("accepted revision guidance lost its durable request")?;
        Ok((
            sigil_kernel::UserInputDecisionReceiptV1 {
                request: current.public_view(),
                idempotent_replay: false,
                continuation_required: false,
            },
            revision_request,
        ))
    }

    fn recover_unstarted_revision_request(
        session: &Session,
        state: &sigil_kernel::UserInputRequestStateV1,
        workspace_snapshot_id: Option<String>,
    ) -> Result<Option<PlanReviewRunRequest>> {
        let (base_plan_id, base_plan_hash) = match &state.requested.request.source {
            sigil_kernel::UserInputSourceV1::PlanRevision {
                base_plan_id,
                base_plan_hash,
            } => (base_plan_id, base_plan_hash),
            _ => return Ok(None),
        };
        if !session
            .plan_artifact_projection()
            .latest_decision(base_plan_id)
            .is_some_and(|decision| {
                decision.decision == PlanDecision::RevisionRequested
                    && decision.plan_hash == *base_plan_hash
            })
        {
            return Ok(None);
        }
        let request_id = &state.requested.request.identity.request_id;
        let review_projection = PlanReviewProjection::from_entries(session.entries());
        let base_attempt = review_projection
            .attempt_for_plan(base_plan_id)
            .context("revision replay lost its base review attempt")?;
        if review_projection
            .review(&base_attempt.plan_review_id)
            .is_some_and(|review| {
                review
                    .attempts
                    .iter()
                    .any(|attempt| attempt.revision_request_id.as_ref() == Some(request_id))
            })
        {
            return Ok(None);
        }
        let guidance = state
            .decision
            .as_ref()
            .and_then(|decision| match &decision.decision {
                sigil_kernel::UserInputDurableDecisionV1::Submitted {
                    answers: Some(answers),
                    ..
                } => answers.iter().find_map(|answer| {
                    (answer.question_id == "revision_guidance")
                        .then_some(&answer.value)
                        .and_then(|value| match value {
                            sigil_kernel::UserInputAnswerValueV1::Text { value } => {
                                Some(value.as_str())
                            }
                            _ => None,
                        })
                }),
                _ => None,
            })
            .context("revision replay lost its accepted guidance")?;
        Self::plan_review_revision_request(
            session,
            base_plan_id,
            base_plan_hash,
            request_id.clone(),
            guidance,
            workspace_snapshot_id,
        )
        .map(Some)
    }

    /// Reuses already accepted revision guidance while allocating a fresh physical attempt.
    pub fn retry_plan_revision(
        session: &mut Session,
        base_plan_id: &PlanId,
        base_plan_hash: &str,
        workspace_snapshot_id: Option<String>,
        now_ms: u64,
    ) -> Result<Option<PlanReviewRunRequest>> {
        let plan_projection = session.plan_artifact_projection();
        if !plan_projection
            .latest_decision(base_plan_id)
            .is_some_and(|decision| {
                decision.decision == PlanDecision::RevisionFailed
                    && decision.plan_hash == base_plan_hash
            })
        {
            return Ok(None);
        }
        let latest = session
            .user_input_projection()?
            .public_requests()
            .into_iter()
            .filter(|request| {
                matches!(
                    &request.source,
                    sigil_kernel::UserInputSourceV1::PlanRevision {
                        base_plan_id: candidate_id,
                        base_plan_hash: candidate_hash,
                    } if candidate_id == base_plan_id && candidate_hash == base_plan_hash
                )
            })
            .max_by_key(|request| request.identity.generation);
        let Some(latest) = latest else {
            return Ok(None);
        };
        let accepted = session
            .entries()
            .iter()
            .rev()
            .find_map(|entry| match entry {
                SessionLogEntry::Control(ControlEntry::UserInputDecisionAccepted(accepted))
                    if accepted.identity == latest.identity =>
                {
                    Some((**accepted).clone())
                }
                _ => None,
            });
        let Some(accepted) = accepted else {
            return Ok(None);
        };
        let guidance = match accepted.decision {
            sigil_kernel::UserInputDurableDecisionV1::Submitted {
                answers: Some(answers),
                ..
            } => answers.into_iter().find_map(|answer| {
                (answer.question_id == "revision_guidance")
                    .then_some(answer.value)
                    .and_then(|value| match value {
                        sigil_kernel::UserInputAnswerValueV1::Text { value } => Some(value),
                        _ => None,
                    })
            }),
            _ => None,
        };
        let Some(guidance) = guidance else {
            return Ok(None);
        };
        let request = Self::plan_review_revision_request(
            session,
            base_plan_id,
            base_plan_hash,
            latest.identity.request_id,
            &guidance,
            workspace_snapshot_id,
        )?;
        session.append_control(ControlEntry::PlanDecisionRecorded(
            PlanDecisionRecordedEntry {
                plan_id: base_plan_id.clone(),
                plan_hash: base_plan_hash.to_owned(),
                decision: PlanDecision::RevisionRequested,
                decided_by: PlanDecisionActor::User,
                decided_at_ms: now_ms,
                reason: Some(safe_persistence_text(&guidance)),
            },
        ))?;
        Ok(Some(request))
    }

    /// Accepts a user-input decision owned by the read-only research child and returns the exact
    /// plan-review attempt that must be resumed under a supervisor.
    ///
    /// The parent only carries a public-safe mirror while suspended. The authoritative request,
    /// answer, tool settlement, and continuation claim remain in the child session, so a caller
    /// cannot redirect an answer to a different session or attempt.
    pub fn accept_plan_review_research_input(
        parent: &mut Session,
        command: sigil_kernel::UserInputDecisionCommandV1,
        now_ms: u64,
    ) -> Result<(
        sigil_kernel::UserInputDecisionReceiptV1,
        Option<PlanReviewRunRequest>,
    )> {
        let projection = PlanReviewProjection::from_entries(parent.entries());
        if projection.has_conflicts() {
            bail!("plan review projection contains conflicts");
        }
        let attempt = projection
            .attempt_for_pending_user_input(&command.identity, &command.request_hash)
            .cloned()
            .context("plan-review input decision does not bind a suspended attempt")?;
        let request = plan_review_request_from_attempt(parent, &attempt)?;
        let mut child = build_plan_review_child_session(parent, &request)?;
        if child.session_scope_id() != command.identity.session_scope_id.as_str() {
            bail!("plan-review input belongs to a different child session");
        }
        let receipt = sigil_kernel::accept_user_input_decision(&mut child, command, now_ms)?;
        if matches!(
            receipt.request.resolution,
            Some(sigil_kernel::UserInputResolutionV1::RunCancelled)
        ) {
            Self::close_plan_review_run(
                parent,
                &request,
                &PlanReviewRunOutcome::Cancelled,
                now_ms,
            )?;
            return Ok((receipt, None));
        }
        Ok((receipt, Some(request)))
    }

    /// Records a typed user decision for one plan artifact.
    ///
    /// Decisions bind the exact plan id and hash; stale hashes, duplicate acceptance, and
    /// post-task decisions fail closed.
    ///
    /// # Errors
    ///
    /// Returns an error for a stale hash, a missing plan, or a conflicting decision.
    pub fn record_plan_decision(
        session: &mut Session,
        command: &PlanDecisionCommand,
        now_ms: u64,
    ) -> Result<PlanDecisionRecordedEntry> {
        let plan_id = PlanId::new(command.plan_id.clone())
            .map_err(|error| anyhow!("invalid plan id for decision: {error}"))?;
        let projection = session.plan_artifact_projection();
        let draft =
            projection.plans.get(&plan_id).cloned().ok_or_else(|| {
                anyhow!("plan {} is not present in this session", plan_id.as_str())
            })?;
        if draft.plan_hash != command.expected_plan_hash {
            bail!(
                "plan {} is stale: expected {}, current {}",
                plan_id.as_str(),
                command.expected_plan_hash,
                draft.plan_hash
            );
        }
        if let Some(existing) = projection.latest_decision(&plan_id)
            && existing.decision == command.decision
            && existing.plan_hash == draft.plan_hash
        {
            return Ok(existing.clone());
        }
        if command.decision == PlanDecision::SavedOnly {
            ensure_plan_action_allowed(
                session,
                &plan_id,
                &command.expected_plan_hash,
                sigil_kernel::PublicPlanAction::Save,
            )?;
        }
        if projection.plan_is_rejected(&plan_id) {
            bail!("plan {} was rejected", plan_id.as_str());
        }
        if command.decision == PlanDecision::Accepted && projection.task_created_for_plan(&plan_id)
        {
            bail!("plan {} already created a task", plan_id.as_str());
        }
        if let Some(existing) = projection.latest_decision(&plan_id) {
            if existing.decision == PlanDecision::RevisionFailed {
                // The revision never started; the original plan remains actionable.
            } else {
                bail!(
                    "plan {} already has decision {}",
                    plan_id.as_str(),
                    existing.decision.as_str()
                );
            }
        }
        let entry = PlanDecisionRecordedEntry {
            plan_id,
            plan_hash: draft.plan_hash,
            decision: command.decision,
            decided_by: PlanDecisionActor::User,
            decided_at_ms: now_ms,
            reason: None,
        };
        session.append_control(ControlEntry::PlanDecisionRecorded(entry.clone()))?;
        Ok(entry)
    }

    /// Records a durable host-side failure for a revision that could not start.
    ///
    /// The `RevisionRequested` decision is persisted before the driver registers the supervised
    /// run; if that registration fails, this fact makes the failure recoverable: the original
    /// plan stays actionable (run/save/reject/revise again) instead of being stuck behind a
    /// decision that can never complete.
    ///
    /// # Errors
    ///
    /// Returns an error for a stale hash, a missing plan, or a conflicting decision.
    pub fn record_revision_failure(
        session: &mut Session,
        plan_id: &PlanId,
        expected_plan_hash: &str,
        reason: &str,
        now_ms: u64,
    ) -> Result<PlanDecisionRecordedEntry> {
        let projection = session.plan_artifact_projection();
        let draft =
            projection.plans.get(plan_id).cloned().ok_or_else(|| {
                anyhow!("plan {} is not present in this session", plan_id.as_str())
            })?;
        if draft.plan_hash != expected_plan_hash {
            bail!(
                "plan {} is stale: expected {}, current {}",
                plan_id.as_str(),
                expected_plan_hash,
                draft.plan_hash
            );
        }
        if let Some(existing) = projection.latest_decision(plan_id) {
            match existing.decision {
                PlanDecision::RevisionFailed => return Ok(existing.clone()),
                PlanDecision::RevisionRequested => {}
                _ => bail!(
                    "plan {} already has decision {}",
                    plan_id.as_str(),
                    existing.decision.as_str()
                ),
            }
        }
        let entry = PlanDecisionRecordedEntry {
            plan_id: plan_id.clone(),
            plan_hash: draft.plan_hash,
            decision: PlanDecision::RevisionFailed,
            decided_by: PlanDecisionActor::System,
            decided_at_ms: now_ms,
            reason: Some(reason.to_owned()),
        };
        session.append_control(ControlEntry::PlanDecisionRecorded(entry.clone()))?;
        Ok(entry)
    }

    /// Creates a durable task from an accepted plan through the shared RFC-0018 handoff.
    ///
    /// This is the single Plan-to-Task promotion path used by TUI, HTTP, and Desktop. The function
    /// is idempotent: retries reconcile the deterministic prefix; conflicting facts fail closed.
    ///
    /// # Errors
    ///
    /// Returns an error for a stale hash, missing/rejected plan, step-limit violation, unsafe
    /// promotion, or conflicting durable prefix facts.
    pub fn create_task_from_plan(
        session: &mut Session,
        root_config: &RootConfig,
        workspace_root: &Path,
        parent_session_ref: SessionRef,
        request: &CreateTaskFromPlanRequest,
    ) -> Result<CreatedTaskFromPlan> {
        let plan_id = PlanId::new(request.plan_id.clone())
            .map_err(|error| anyhow!("invalid plan id for task creation: {error}"))?;
        let projection = session.plan_artifact_projection();
        let draft =
            projection.plans.get(&plan_id).cloned().ok_or_else(|| {
                anyhow!("plan {} is not present in this session", plan_id.as_str())
            })?;
        if draft.plan_hash != request.expected_plan_hash {
            bail!(
                "plan {} is stale: expected {}, current {}",
                plan_id.as_str(),
                request.expected_plan_hash,
                draft.plan_hash
            );
        }
        let exact_prefix_exists = projection
            .latest_decision(&plan_id)
            .is_some_and(|decision| {
                decision.decision == PlanDecision::Accepted
                    && decision.plan_hash == request.expected_plan_hash
            })
            || projection
                .tasks_created
                .get(&plan_id)
                .and_then(|entries| entries.last())
                .is_some_and(|created| created.plan_hash == request.expected_plan_hash);
        if !exact_prefix_exists {
            ensure_plan_action_allowed(
                session,
                &plan_id,
                &request.expected_plan_hash,
                sigil_kernel::PublicPlanAction::Run,
            )?;
        }
        if projection.plan_is_rejected(&plan_id) {
            bail!("plan {} was rejected", plan_id.as_str());
        }
        let current_workspace_snapshot_id =
            plan_handoff_workspace_snapshot_id(root_config, workspace_root)?;
        let stale_reason = plan_handoff_stale_reason(
            draft.workspace_snapshot_id.as_deref(),
            current_workspace_snapshot_id.as_deref(),
        );
        let task_id = task_id_from_plan_draft(&draft)?;
        let task_id_value = task_id.as_str().to_owned();
        let objective = plan_task_input_from_draft(&draft);
        let decision = PlanDecisionRecordedEntry {
            plan_id: plan_id.clone(),
            plan_hash: draft.plan_hash.clone(),
            decision: PlanDecision::Accepted,
            decided_by: PlanDecisionActor::User,
            decided_at_ms: now_ms(),
            reason: Some("created task from plan".to_owned()),
        };
        if draft.steps.len() > root_config.task.max_plan_steps {
            bail!(
                "plan {} has {} steps, exceeding task.max_plan_steps={}",
                plan_id.as_str(),
                draft.steps.len(),
                root_config.task.max_plan_steps
            );
        }
        let promoted = if stale_reason.is_none() {
            task_plan_from_plan_draft(&draft, task_id.clone(), 1)?
        } else {
            None
        };
        let (task_plan, step_mapping, intent_admission) = match promoted {
            Some(promotion) => {
                let mut task_plan = promotion.task_plan;
                let intent_admission = match draft.intent_proposal.as_ref() {
                    Some(proposal) => {
                        let workspace_id = stable_workspace_id(workspace_root)
                            .map_err(|error| anyhow!("failed to scope IntentPlan: {error}"))?;
                        let stack_id = IntentStackId::new(stable_event_uuid(
                            "sigil-plan-intent-stack-v1",
                            &format!("{}:{}", draft.plan_id.as_str(), draft.plan_hash),
                        ))?;
                        let context = IntentAdmissionContextV1::initial(
                            stack_id,
                            workspace_id,
                            session.session_scope_id().to_owned(),
                        )?;
                        let authority_event_id = stable_event_uuid(
                            "sigil-plan-intent-acceptance-v1",
                            &format!(
                                "{}:{}:{}",
                                draft.plan_id.as_str(),
                                draft.plan_hash,
                                task_id.as_str()
                            ),
                        );
                        let authority = IntentAcceptanceAuthorityV1::explicit_user_confirmation(
                            proposal.source_turn_id.clone(),
                            authority_event_id,
                            proposal.proposal_digest.clone(),
                        )?;
                        let admission =
                            admit_suggested_decomposition(&context, proposal, &authority)?;
                        task_plan = bind_task_plan_intents(
                            &admission,
                            task_plan,
                            &promotion.intent_alias_bindings,
                        )?;
                        Some(admission)
                    }
                    None => {
                        if !promotion.intent_alias_bindings.is_empty() {
                            bail!(
                                "plan {} carries intent aliases without a digest-bound proposal",
                                plan_id.as_str()
                            );
                        }
                        None
                    }
                };
                (Some(task_plan), promotion.step_mapping, intent_admission)
            }
            None => (None, Vec::new(), None),
        };
        let existing_accepted_plan = session
            .task_state_projection()
            .tasks
            .get(&task_id)
            .and_then(|task| {
                task.plans
                    .values()
                    .find(|plan| plan.status == TaskPlanStatus::Accepted)
            })
            .cloned();
        if task_plan.is_none()
            && let Some(existing_plan) = existing_accepted_plan
        {
            session.append_control(ControlEntry::TaskPlan(TaskPlanEntry {
                task_id: task_id.clone(),
                plan_version: existing_plan.plan_version,
                status: TaskPlanStatus::Superseded,
                steps: existing_plan.steps,
                reason: Some(
                    "workspace drift invalidated a crash-interrupted plan promotion".to_owned(),
                ),
            }))?;
            let existing_task = session
                .task_state_projection()
                .tasks
                .get(&task_id)
                .cloned()
                .ok_or_else(|| anyhow!("stale promoted task prefix is missing its task run"))?;
            session.append_control(ControlEntry::TaskRun(TaskRunEntry {
                task_id: task_id.clone(),
                parent_session_ref: existing_task.parent_session_ref,
                objective: existing_task.objective,
                title: None,
                status: TaskRunStatus::Cancelled,
                reason: Some(
                    "plan creation cancelled because the workspace changed before commit"
                        .to_owned(),
                ),
            }))?;
            bail!(
                "plan {} creation prefix conflicts with current workspace drift; refusing to execute an earlier promoted task plan",
                plan_id.as_str()
            );
        }
        let task_created = TaskCreatedFromPlanEntry {
            plan_id: plan_id.clone(),
            plan_hash: draft.plan_hash.clone(),
            task_id: task_id.clone(),
            task_plan_version: task_plan.as_ref().map_or(0, |plan| plan.plan_version),
            step_mapping: step_mapping.clone(),
            stale_reason,
            created_at_ms: now_ms(),
        };
        let permission_grant = match request.permission_grant {
            Some(permission) => {
                if draft.target_paths.is_empty() {
                    bail!(
                        "plan {} has no concrete target paths for scoped edits",
                        plan_id.as_str()
                    );
                }
                Some(PlanPermissionGrantedEntry {
                    plan_id: plan_id.clone(),
                    plan_hash: draft.plan_hash.clone(),
                    task_id: task_id.clone(),
                    workspace_snapshot_id: current_workspace_snapshot_id,
                    permission,
                    scope: PlanApprovalScope {
                        summary: format!("scoped edits for task {}", task_id.as_str()),
                        workspace_paths: draft.target_paths.clone(),
                    },
                    expires: PlanApprovalExpiry::Session,
                    granted_at_ms: now_ms(),
                })
            }
            None => None,
        };

        let desired_task_status = if request.start_mode == PlanTaskStartMode::CreatePaused {
            TaskRunStatus::Paused
        } else {
            TaskRunStatus::Started
        };
        let safe_objective = safe_persistence_text(&objective);
        let existing_task = session.task_state_projection().tasks.get(&task_id).cloned();
        match existing_task {
            Some(existing)
                if existing.parent_session_ref == parent_session_ref
                    && existing.objective == safe_objective
                    && existing.status == desired_task_status => {}
            Some(existing)
                if existing.parent_session_ref == parent_session_ref
                    && existing.objective == safe_objective
                    && existing.status == TaskRunStatus::Paused
                    && desired_task_status == TaskRunStatus::Started
                    && existing.participant_attempts.is_empty()
                    && existing.steps.is_empty() =>
            {
                session.append_control(ControlEntry::TaskRun(TaskRunEntry {
                    task_id: task_id.clone(),
                    parent_session_ref: parent_session_ref.clone(),
                    objective: safe_objective.clone(),
                    title: Some(sigil_kernel::task_semantic_title(&draft.summary)),
                    status: TaskRunStatus::Started,
                    reason: Some(format!(
                        "resumed crash-interrupted creation from plan {}",
                        plan_id.as_str()
                    )),
                }))?;
            }
            Some(_) => {
                bail!(
                    "plan {} task prefix conflicts with the requested task facts",
                    plan_id.as_str()
                );
            }
            None => session.append_control(ControlEntry::TaskRun(TaskRunEntry {
                task_id: task_id.clone(),
                parent_session_ref: parent_session_ref.clone(),
                objective: safe_objective,
                title: Some(sigil_kernel::task_semantic_title(&draft.summary)),
                status: desired_task_status,
                reason: Some(format!("created from plan {}", plan_id.as_str())),
            }))?,
        }

        if let Some(task_plan) = task_plan {
            let existing_plan = session
                .task_state_projection()
                .tasks
                .get(&task_id)
                .and_then(|task| task.plans.get(&task_plan.plan_version))
                .cloned();
            match existing_plan {
                Some(existing)
                    if existing.plan_version == task_plan.plan_version
                        && existing.status == task_plan.status
                        && existing.steps == task_plan.steps
                        && existing.reason == task_plan.reason => {}
                Some(_) => {
                    bail!(
                        "plan {} task-plan prefix conflicts with direct promotion",
                        plan_id.as_str()
                    );
                }
                None if intent_admission.is_none() => {
                    session.append_control(ControlEntry::TaskPlan(task_plan.clone()))?
                }
                None => {}
            }
            if let Some(admission) = intent_admission.as_ref() {
                append_task_intent_plan_admission(session, admission, task_plan)?;
            }
        }

        if let Some(grant) = permission_grant {
            let existing_grants = session
                .plan_artifact_projection()
                .permission_grants
                .get(&plan_id)
                .cloned()
                .unwrap_or_default();
            if existing_grants.iter().any(|existing| {
                existing.plan_hash == grant.plan_hash
                    && existing.task_id == grant.task_id
                    && existing.workspace_snapshot_id == grant.workspace_snapshot_id
                    && existing.permission == grant.permission
                    && existing.scope == grant.scope
                    && existing.expires == grant.expires
            }) {
                // The crash-prefix retry already persisted this exact grant.
            } else if existing_grants
                .iter()
                .any(|existing| existing.task_id == task_id)
            {
                bail!(
                    "plan {} already has a conflicting permission grant for this task",
                    plan_id.as_str()
                );
            } else {
                session.append_control(ControlEntry::PlanPermissionGranted(grant))?;
            }
        }

        let existing_created = session
            .plan_artifact_projection()
            .tasks_created
            .get(&plan_id)
            .and_then(|entries| entries.last())
            .cloned();
        match existing_created {
            Some(existing)
                if existing.plan_id == task_created.plan_id
                    && existing.plan_hash == task_created.plan_hash
                    && existing.task_id == task_created.task_id
                    && existing.task_plan_version == task_created.task_plan_version
                    && existing.step_mapping == task_created.step_mapping
                    && existing.stale_reason == task_created.stale_reason => {}
            Some(_) => {
                bail!(
                    "plan {} already has a conflicting task-created anchor",
                    plan_id.as_str()
                );
            }
            None => {
                session.append_control(ControlEntry::TaskCreatedFromPlan(task_created.clone()))?
            }
        }

        let existing_decision = session
            .plan_artifact_projection()
            .latest_decision(&plan_id)
            .cloned();
        match existing_decision {
            Some(existing)
                if existing.decision == PlanDecision::Accepted
                    && existing.plan_hash == draft.plan_hash => {}
            Some(existing) if existing.decision == PlanDecision::Accepted => {
                bail!(
                    "plan {} already has an accepted decision for another hash",
                    plan_id.as_str()
                );
            }
            _ => session.append_control(ControlEntry::PlanDecisionRecorded(decision))?,
        }

        let entries = session.entries().to_vec();
        Ok(CreatedTaskFromPlan {
            task_id,
            task_id_value,
            objective,
            entry: task_created,
            start_mode: request.start_mode,
            entries,
        })
    }

    /// Discards a plan durably.
    ///
    /// # Errors
    ///
    /// Returns an error for a stale hash, a missing plan, an existing task, or an existing
    /// decision.
    pub fn reject_plan(session: &mut Session, request: &RejectPlanRequest) -> Result<RejectedPlan> {
        let plan_id = PlanId::new(request.plan_id.clone())
            .map_err(|error| anyhow!("invalid plan id for rejection: {error}"))?;
        let projection = session.plan_artifact_projection();
        let draft = projection
            .plans
            .get(&plan_id)
            .ok_or_else(|| anyhow!("plan {} is not present in this session", plan_id.as_str()))?;
        if draft.plan_hash != request.expected_plan_hash {
            bail!(
                "plan {} is stale: expected {}, current {}",
                plan_id.as_str(),
                request.expected_plan_hash,
                draft.plan_hash
            );
        }
        if let Some(decision) = projection.latest_decision(&plan_id)
            && decision.decision == PlanDecision::Rejected
            && decision.plan_hash == draft.plan_hash
        {
            return Ok(RejectedPlan {
                entry: decision.clone(),
                entries: session.entries().to_vec(),
            });
        }
        ensure_plan_action_allowed(
            session,
            &plan_id,
            &request.expected_plan_hash,
            sigil_kernel::PublicPlanAction::Reject,
        )?;
        if projection.task_created_for_plan(&plan_id) {
            bail!("plan {} already created a task", plan_id.as_str());
        }
        if let Some(decision) = projection.latest_decision(&plan_id) {
            match decision.decision {
                PlanDecision::SavedOnly | PlanDecision::RevisionFailed => {}
                _ => bail!(
                    "plan {} already has decision {}",
                    plan_id.as_str(),
                    decision.decision.as_str()
                ),
            }
        }
        let entry = PlanDecisionRecordedEntry {
            plan_id,
            plan_hash: draft.plan_hash.clone(),
            decision: PlanDecision::Rejected,
            decided_by: PlanDecisionActor::User,
            decided_at_ms: now_ms(),
            reason: Some("discarded plan".to_owned()),
        };
        session.append_control(ControlEntry::PlanDecisionRecorded(entry.clone()))?;
        let entries = session.entries().to_vec();
        Ok(RejectedPlan { entry, entries })
    }
}

fn invalid_plan_draft_submission_reason(session: &Session) -> Option<String> {
    session.entries().iter().rev().find_map(|entry| {
        let SessionLogEntry::ToolResultV3(result) = entry else {
            return None;
        };
        if result.tool_name != sigil_kernel::SUBMIT_PLAN_DRAFT_TOOL_NAME
            || result.facts.status != "error"
        {
            return None;
        }
        let detail = result
            .facts
            .error
            .as_ref()
            .map(|error| error.message.as_str())
            .unwrap_or("typed draft validation failed");
        Some(format!(
            "submit-only finalizer produced an invalid draft: {detail}"
        ))
    })
}

/// Typed plan rejection command shared by TUI, HTTP, and Desktop surfaces.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct RejectPlanRequest {
    pub plan_id: String,
    pub expected_plan_hash: String,
}

impl PlanReviewCoordinator {
    /// Ensures the attempt `Started` record exists in the parent session.
    ///
    /// The record is owned by the run executor, not by the prepare step: a persisted `Started`
    /// without an in-process run would be misread as a crashed run by recovery (which closes it
    /// as `Interrupted`) when the executor reloads the session across a process boundary.
    ///
    /// # Errors
    ///
    /// Returns an error when the attempt already carries a different status or the transition is
    /// invalid.
    pub fn ensure_attempt_started(
        session: &mut Session,
        request: &PlanReviewRunRequest,
        now_ms: u64,
    ) -> Result<()> {
        let projection = PlanReviewProjection::from_entries(session.entries());
        if let Some(existing) = projection.latest_attempt(&request.plan_review_id) {
            if existing.attempt_id == request.attempt_id
                && existing.status == PlanReviewAttemptStatus::Started
            {
                return Ok(());
            }
            if existing.attempt_id == request.attempt_id
                && existing.status != PlanReviewAttemptStatus::WaitingForInput
            {
                bail!(
                    "plan review attempt {} already has status {}",
                    request.attempt_id.as_str(),
                    existing.status.as_str()
                );
            }
        }
        let entry = PlanReviewAttemptEntry {
            plan_review_id: request.plan_review_id.clone(),
            attempt_id: request.attempt_id.clone(),
            plan_id: request.plan_id.clone(),
            source: request.source,
            source_turn: request.source_turn.clone(),
            route_decision_id: request.route_decision_id.clone(),
            child_session_ref: request.child_session_ref.clone(),
            finalizer_session_ref: Some(request.finalizer_session_ref.clone()),
            revision_request_id: request.revision_request_id.clone(),
            attempt_ordinal: request.attempt_ordinal,
            base_plan_id: request.base_plan_id.clone(),
            base_plan_hash: request.base_plan_hash.clone(),
            workspace_snapshot_id: request.workspace_snapshot_id.clone(),
            pending_user_input: None,
            status: PlanReviewAttemptStatus::Started,
            terminal_reason: None,
            recorded_at_ms: now_ms,
        };
        projection.validate_append(&entry)?;
        session.append_control(ControlEntry::PlanReviewAttempt(entry))?;
        Ok(())
    }
}

fn append_attempt_status(
    session: &mut Session,
    request: &PlanReviewRunRequest,
    status: PlanReviewAttemptStatus,
    terminal_reason: Option<PlanReviewTerminalReason>,
    now_ms: u64,
) -> Result<()> {
    if let Some(entry) =
        plan_review_attempt_status_entry(session, request, status, terminal_reason, now_ms)?
    {
        let projection = PlanReviewProjection::from_entries(session.entries());
        projection.validate_append(&entry)?;
        session.append_control(ControlEntry::PlanReviewAttempt(entry))?;
    }
    Ok(())
}

fn append_attempt_status_with_pending_input(
    session: &mut Session,
    request: &PlanReviewRunRequest,
    status: PlanReviewAttemptStatus,
    pending_user_input: Option<sigil_kernel::PublicUserInputRequestV1>,
    now_ms: u64,
) -> Result<()> {
    let mut entry = plan_review_attempt_status_entry(session, request, status, None, now_ms)?
        .context("plan review pending-input transition was already recorded")?;
    entry.pending_user_input = pending_user_input.map(Box::new);
    let projection = PlanReviewProjection::from_entries(session.entries());
    projection.validate_append(&entry)?;
    session.append_control(ControlEntry::PlanReviewAttempt(entry))
}

fn plan_review_attempt_status_entry(
    session: &Session,
    request: &PlanReviewRunRequest,
    status: PlanReviewAttemptStatus,
    terminal_reason: Option<PlanReviewTerminalReason>,
    now_ms: u64,
) -> Result<Option<PlanReviewAttemptEntry>> {
    let projection = PlanReviewProjection::from_entries(session.entries());
    if let Some(existing) = projection.latest_attempt(&request.plan_review_id) {
        if existing.attempt_id == request.attempt_id && existing.status == status {
            return Ok(None);
        }
        if existing.attempt_id != request.attempt_id {
            bail!(
                "plan review {} has a different active attempt {}",
                request.plan_review_id.as_str(),
                existing.attempt_id.as_str()
            );
        }
    }
    let entry = PlanReviewAttemptEntry {
        plan_review_id: request.plan_review_id.clone(),
        attempt_id: request.attempt_id.clone(),
        plan_id: request.plan_id.clone(),
        source: request.source,
        source_turn: request.source_turn.clone(),
        route_decision_id: request.route_decision_id.clone(),
        child_session_ref: request.child_session_ref.clone(),
        finalizer_session_ref: Some(request.finalizer_session_ref.clone()),
        revision_request_id: request.revision_request_id.clone(),
        attempt_ordinal: request.attempt_ordinal,
        base_plan_id: request.base_plan_id.clone(),
        base_plan_hash: request.base_plan_hash.clone(),
        workspace_snapshot_id: request.workspace_snapshot_id.clone(),
        pending_user_input: None,
        status,
        terminal_reason,
        recorded_at_ms: now_ms,
    };
    Ok(Some(entry))
}

fn plan_review_run_input(
    request: &PlanReviewRunRequest,
    draft_context: &sigil_kernel::PlanReviewDraftContext,
    cancellation: &sigil_kernel::RunCancellationHandle,
    finalizer_evidence: Option<&str>,
    finalizer_ordinal: u32,
) -> AgentRunInput {
    let mut transient = vec![ModelMessage::system(
        plan_review_system_prompt_contract_material(),
    )];
    if let Some(evidence) = finalizer_evidence {
        transient.push(ModelMessage::system(
            plan_review_no_draft_retry_contract_material(),
        ));
        transient.push(ModelMessage::user(format!(
            "Bounded host evidence bundle (do not perform more research):\n{evidence}"
        )));
    }
    transient.push(ModelMessage::user(request.objective.clone()));
    let input = AgentRunInput::without_persisted_user_message(transient)
        .with_logical_run_id(if finalizer_evidence.is_some() {
            format!(
                "{}-finalizer-{finalizer_ordinal}",
                request.child_logical_run_id()
            )
        } else {
            request.child_logical_run_id()
        })
        .with_child_cancellation(cancellation.clone())
        .with_run_purpose(AgentRunPurpose::PlanReview(
            sigil_kernel::PlanReviewPurposeContext {
                plan_review_id: request.plan_review_id.clone(),
                attempt_id: request.attempt_id.clone(),
                plan_id: request.plan_id.clone(),
                source_turn: request.source_turn.clone(),
                route_decision_id: request.route_decision_id.clone(),
            },
        ))
        .with_plan_review_draft(draft_context.clone());
    if finalizer_evidence.is_some() {
        input.with_plan_review_submit_only()
    } else {
        input
    }
}

fn plan_review_continuation_input(
    request: &PlanReviewRunRequest,
    draft_context: &sigil_kernel::PlanReviewDraftContext,
    cancellation: &sigil_kernel::RunCancellationHandle,
    continuation: &sigil_kernel::UserInputContinuationStartedV1,
) -> AgentRunInput {
    AgentRunInput::without_persisted_user_message(vec![ModelMessage::system(
        plan_review_system_prompt_contract_material(),
    )])
    .with_logical_run_id(continuation.continuation_logical_run_id.as_str())
    .with_user_input_continuation_context(
        continuation.identity.root_logical_run_id.as_str(),
        continuation.identity.source_thread_id.clone(),
    )
    .with_initial_provider_physical_attempt_id(continuation.physical_attempt_id.clone())
    .with_child_cancellation(cancellation.clone())
    .with_run_purpose(AgentRunPurpose::PlanReview(
        sigil_kernel::PlanReviewPurposeContext {
            plan_review_id: request.plan_review_id.clone(),
            attempt_id: request.attempt_id.clone(),
            plan_id: request.plan_id.clone(),
            source_turn: request.source_turn.clone(),
            route_decision_id: request.route_decision_id.clone(),
        },
    ))
    .with_plan_review_draft(draft_context.clone())
}

fn plan_review_draft_ready_outcome(
    child_session: &Session,
    plan_id: &PlanId,
) -> Result<PlanReviewRunOutcome> {
    let draft = child_session
        .plan_artifact_projection()
        .plans
        .get(plan_id)
        .cloned()
        .ok_or_else(|| {
            anyhow!(
                "plan review draft {} is missing from its child session",
                plan_id.as_str()
            )
        })?;
    Ok(PlanReviewRunOutcome::DraftReady {
        draft: Box::new(draft),
    })
}

fn complete_plan_review_run(
    cancellation: &sigil_kernel::RunCancellationHandle,
    outcome: PlanReviewRunOutcome,
) -> Result<PlanReviewRunOutcome> {
    if !cancellation.is_naturally_finalized() && !cancellation.try_finalize_naturally() {
        if cancellation.is_cancel_requested() {
            return Ok(PlanReviewRunOutcome::Cancelled);
        }
        bail!("run cancellation won before plan review completion");
    }
    Ok(outcome)
}

fn plan_review_provider_terminal_allows_submit_only_recovery(
    child_session: &Session,
    logical_run_id: &str,
) -> Result<bool> {
    let projection = child_session.provider_physical_attempt_projection()?;
    let outcome = projection
        .attempts_for_logical_run_id(logical_run_id)
        .last()
        .and_then(|attempt| attempt.terminal.as_ref())
        .map(|terminal| terminal.outcome);
    Ok(matches!(
        outcome,
        Some(ProviderPhysicalAttemptOutcome::ProtocolRejectedAfterOutput)
    ))
}

fn build_plan_review_child_session(
    parent_session: &Session,
    request: &PlanReviewRunRequest,
) -> Result<Session> {
    if let Some(parent_path) = parent_session.store_path() {
        let parent_dir = parent_path.parent().unwrap_or_else(|| Path::new("."));
        let store =
            sigil_kernel::JsonlSessionStore::new(request.child_session_ref.resolve(parent_dir))?;
        let mut session = Session::load_from_store(
            parent_session.provider_name(),
            parent_session.model_name(),
            store,
        )?;
        attach_session_url_capability_store(&mut session)?;
        return Ok(session);
    }
    let mut session = Session::new(parent_session.provider_name(), parent_session.model_name());
    attach_session_url_capability_store(&mut session)?;
    Ok(session)
}

fn plan_review_request_from_attempt(
    parent_session: &Session,
    attempt: &PlanReviewAttemptEntry,
) -> Result<PlanReviewRunRequest> {
    let mut objective = source_turn_objective(parent_session, &attempt.source_turn)
        .filter(|value| !value.trim().is_empty())
        .context("plan review attempt lost its durable source objective")?;
    if attempt.revision_request_id.is_some() {
        let base_plan_id = attempt
            .base_plan_id
            .as_ref()
            .context("revision attempt lost its base plan identity")?;
        let guidance = parent_session
            .plan_artifact_projection()
            .latest_decision(base_plan_id)
            .filter(|decision| decision.decision == PlanDecision::RevisionRequested)
            .and_then(|decision| decision.reason.clone())
            .context("revision attempt lost its accepted guidance")?;
        objective.push_str("\n\nUser revision guidance:\n");
        objective.push_str(&guidance);
    }
    Ok(PlanReviewRunRequest {
        plan_review_id: attempt.plan_review_id.clone(),
        attempt_id: attempt.attempt_id.clone(),
        plan_id: attempt.plan_id.clone(),
        source: attempt.source,
        source_turn: attempt.source_turn.clone(),
        route_decision_id: attempt.route_decision_id.clone(),
        child_session_ref: attempt.child_session_ref.clone(),
        finalizer_session_ref: attempt.finalizer_session_ref.clone().unwrap_or_else(|| {
            plan_review_finalizer_session_ref(&attempt.plan_review_id, &attempt.attempt_id, 1)
        }),
        revision_request_id: attempt.revision_request_id.clone(),
        attempt_ordinal: attempt.attempt_ordinal,
        base_plan_id: attempt.base_plan_id.clone(),
        base_plan_hash: attempt.base_plan_hash.clone(),
        objective,
        workspace_snapshot_id: attempt.workspace_snapshot_id.clone(),
    })
}

fn build_plan_review_finalizer_session(
    parent_session: &Session,
    request: &PlanReviewRunRequest,
    corrective_ordinal: u32,
) -> Result<Session> {
    if let Some(parent_path) = parent_session.store_path() {
        let parent_dir = parent_path.parent().unwrap_or_else(|| Path::new("."));
        let child_ref = if corrective_ordinal == 1 {
            request.finalizer_session_ref.clone()
        } else {
            plan_review_finalizer_session_ref(
                &request.plan_review_id,
                &request.attempt_id,
                corrective_ordinal,
            )
        };
        let store = sigil_kernel::JsonlSessionStore::new(child_ref.resolve(parent_dir))?;
        let mut session = Session::load_from_store(
            parent_session.provider_name(),
            parent_session.model_name(),
            store,
        )?;
        attach_session_url_capability_store(&mut session)?;
        return Ok(session);
    }
    let mut session = Session::new(parent_session.provider_name(), parent_session.model_name());
    attach_session_url_capability_store(&mut session)?;
    Ok(session)
}

fn plan_review_finalizer_evidence_bundle(
    request: &PlanReviewRunRequest,
    research_session: &Session,
) -> String {
    const MAX_EVIDENCE_BYTES: usize = 24 * 1024;
    const MAX_RESULTS: usize = 12;
    let mut lines = vec![
        format!("plan_review_id: {}", request.plan_review_id.as_str()),
        format!("attempt_id: {}", request.attempt_id.as_str()),
        format!("workspace_snapshot: {:?}", request.workspace_snapshot_id),
    ];
    if let (Some(base_id), Some(base_hash)) = (
        request.base_plan_id.as_ref(),
        request.base_plan_hash.as_ref(),
    ) {
        lines.push(format!("base_plan: {} @ {}", base_id.as_str(), base_hash));
    }
    let results = research_session
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::ToolResultV3(result) => Some(result),
            _ => None,
        })
        .rev()
        .take(MAX_RESULTS)
        .collect::<Vec<_>>();
    for result in results.into_iter().rev() {
        let artifact = result
            .initial_model_view
            .artifact_ref
            .as_ref()
            .map(|reference| format!(" artifact={}", reference.artifact_id))
            .unwrap_or_default();
        lines.push(format!(
            "tool {} call={} hash={}{}\n{}",
            result.tool_name,
            result.call_id,
            result.artifact_hash,
            artifact,
            result.initial_model_view.preview
        ));
    }
    let mut evidence = lines.join("\n\n");
    if evidence.len() > MAX_EVIDENCE_BYTES {
        let mut end = MAX_EVIDENCE_BYTES.saturating_sub("\n...[evidence truncated]".len());
        while !evidence.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        evidence.truncate(end);
        evidence.push_str("\n...[evidence truncated]");
    }
    evidence
}

fn source_turn_objective(session: &Session, source_turn: &ConversationTurnRef) -> Option<String> {
    session.entries().iter().find_map(|entry| match entry {
        SessionLogEntry::User(message) if message.id == source_turn.message_id => {
            Some(message.content.clone().unwrap_or_default())
        }
        SessionLogEntry::Control(ControlEntry::ConversationInputPromoted(promoted))
            if promoted.durable_user_message.id == source_turn.message_id =>
        {
            Some(
                promoted
                    .durable_user_message
                    .content
                    .clone()
                    .unwrap_or_default(),
            )
        }
        _ => None,
    })
}

fn validate_plan_review_child_draft(
    draft: &PlanDraftCreatedEntry,
    request: &PlanReviewRunRequest,
) -> Result<()> {
    if draft.plan_id != request.plan_id
        || draft.source.source_turn.as_ref() != Some(&request.source_turn)
        || draft.source.route_decision_id != request.route_decision_id
        || draft.source.plan_review_id.as_ref() != Some(&request.plan_review_id)
        || draft.workspace_snapshot_id != request.workspace_snapshot_id
    {
        bail!(
            "plan review child draft {} does not match its bound attempt lineage \
             (plan_id_match={}, source_turn_match={}, route_decision_match={}, \
             plan_review_id_match={}, workspace_snapshot_match={})",
            draft.plan_id.as_str(),
            draft.plan_id == request.plan_id,
            draft.source.source_turn.as_ref() == Some(&request.source_turn),
            draft.source.route_decision_id == request.route_decision_id,
            draft.source.plan_review_id.as_ref() == Some(&request.plan_review_id),
            draft.workspace_snapshot_id == request.workspace_snapshot_id,
        );
    }
    Ok(())
}

fn ensure_plan_action_allowed(
    session: &Session,
    plan_id: &PlanId,
    expected_plan_hash: &str,
    action: sigil_kernel::PublicPlanAction,
) -> Result<()> {
    let review =
        crate::conversation_display::public_plan_review_from_entries(session.entries(), None)
            .context("plan action has no canonical review projection")?;
    if review.plan_id != plan_id.as_str()
        || review.plan_hash.as_deref() != Some(expected_plan_hash)
        || review.status != sigil_kernel::PublicPlanReviewStatus::DraftReady
        || !review.allowed_actions.contains(&action)
    {
        bail!(
            "plan {} action {} is unavailable in the current review state \
             (projected_plan={}, projected_hash={}, status={:?}, allowed_actions={:?})",
            plan_id.as_str(),
            match action {
                sigil_kernel::PublicPlanAction::Run => "run",
                sigil_kernel::PublicPlanAction::Save => "save",
                sigil_kernel::PublicPlanAction::Revise => "revise",
                sigil_kernel::PublicPlanAction::Reject => "reject",
            },
            review.plan_id,
            review.plan_hash.as_deref().unwrap_or("none"),
            review.status,
            review.allowed_actions,
        );
    }
    Ok(())
}

/// Builds the current workspace snapshot id bound to plan handoff artifacts.
pub fn plan_handoff_workspace_snapshot_id(
    root_config: &RootConfig,
    workspace_root: &Path,
) -> Result<Option<String>> {
    let workspace_id = stable_workspace_id(workspace_root)?;
    let scope = root_config
        .verification
        .scope_for_hash(sigil_kernel::DEFAULT_TASK_VERIFICATION_SCOPE_HASH);
    let snapshot = build_workspace_snapshot(workspace_root, workspace_id, &scope, 0)?;
    Ok(snapshot.workspace_snapshot_id)
}

pub fn plan_handoff_stale_reason(
    base_workspace_snapshot_id: Option<&str>,
    current_workspace_snapshot_id: Option<&str>,
) -> Option<String> {
    match (base_workspace_snapshot_id, current_workspace_snapshot_id) {
        (Some(base), Some(current)) => (base != current).then(|| {
            format!(
                "plan may be stale: workspace changed since plan was created (base={}, current={})",
                truncate_plan_snapshot_id(base),
                truncate_plan_snapshot_id(current)
            )
        }),
        (Some(base), None) => Some(format!(
            "plan may be stale: current workspace snapshot is unavailable (base={})",
            truncate_plan_snapshot_id(base)
        )),
        (None, _) => Some(
            "plan cannot be direct-promoted: its base workspace snapshot is unavailable".to_owned(),
        ),
    }
}

fn truncate_plan_snapshot_id(snapshot_id: &str) -> String {
    snapshot_id.chars().take(24).collect()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Typed plan decision command for one application surface.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ApplicationPlanDecisionCommand {
    pub plan_id: String,
    pub expected_plan_hash: String,
    pub action: ApplicationPlanAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_grant: Option<PlanApprovalPermission>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApplicationPlanAction {
    Run,
    Save,
    Revise,
    Reject,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ApplicationPlanDecisionReceipt {
    pub plan_id: String,
    pub plan_hash: String,
    pub action: ApplicationPlanAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// Durable host-owned guidance request created by a first `Revise` action.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_input_request: Option<sigil_kernel::PublicUserInputRequestV1>,
    /// Prepared revision run the caller must execute so `Revise` does not leave a dangling
    /// `Started` attempt. `None` for every other action.
    #[serde(skip)]
    pub revision_request: Option<PlanReviewRunRequest>,
}

/// Applies one typed plan decision on an application surface session.
///
/// `Run` creates the durable RFC-0018 task prefix and returns its stable task id; execution
/// continues through the existing application Task path. `Save`, `Revise`, and `Reject` record
/// durable decisions only.
/// Records a durable revision-start failure for the exact plan hash.
///
/// Called by HTTP/Desktop drivers when the supervised revision run cannot be registered after
/// `application_plan_decision` already persisted `RevisionRequested`. Appending `RevisionFailed`
/// keeps the original plan actionable (run/save/reject/revise again) instead of leaving an
/// unrecoverable decision.
///
/// # Errors
///
/// Returns an error for a stale hash, a missing plan, or a conflicting decision.
pub fn application_record_revision_failure(
    root_config: &RootConfig,
    session_log_path: &Path,
    expected_scope: &str,
    plan_id: &str,
    expected_plan_hash: &str,
    reason: &str,
) -> Result<PlanDecisionRecordedEntry> {
    let store = sigil_kernel::JsonlSessionStore::new(session_log_path)?;
    let (_, fallback_route) =
        crate::provider_connections::resolve_default_model_route(root_config)?;
    let mut session =
        crate::provider_connections::load_session_for_route_resume_with_directive_and_attachment(
            root_config,
            &fallback_route,
            store,
            None,
            None,
            None,
        )?;
    if session.session_scope_id() != expected_scope {
        bail!("plan decision session scope mismatch");
    }
    let plan_id = PlanId::new(plan_id.to_owned())
        .map_err(|error| anyhow!("invalid plan id for revision failure: {error}"))?;
    PlanReviewCoordinator::record_revision_failure(
        &mut session,
        &plan_id,
        expected_plan_hash,
        reason,
        now_ms(),
    )
}

pub fn application_plan_decision(
    root_config: &RootConfig,
    workspace_root: &Path,
    session_log_path: &Path,
    expected_scope: &str,
    command: &ApplicationPlanDecisionCommand,
) -> Result<ApplicationPlanDecisionReceipt> {
    let store = sigil_kernel::JsonlSessionStore::new(session_log_path)?;
    let (_, fallback_route) =
        crate::provider_connections::resolve_default_model_route(root_config)?;
    let mut session =
        crate::provider_connections::load_session_for_route_resume_with_directive_and_attachment(
            root_config,
            &fallback_route,
            store,
            None,
            None,
            None,
        )?;
    if session.session_scope_id() != expected_scope {
        bail!("plan decision session scope mismatch");
    }
    let plan_id = PlanId::new(command.plan_id.clone())
        .map_err(|error| anyhow!("invalid plan id for decision: {error}"))?;
    let draft = session
        .plan_artifact_projection()
        .plans
        .get(&plan_id)
        .cloned()
        .ok_or_else(|| anyhow!("plan {} is not present in this session", plan_id.as_str()))?;
    if draft.plan_hash != command.expected_plan_hash {
        bail!(
            "plan {} is stale: expected {}, current {}",
            plan_id.as_str(),
            command.expected_plan_hash,
            draft.plan_hash
        );
    }
    let parent_session_ref = session_ref_for_log_path(session_log_path)?;
    let receipt = match command.action {
        ApplicationPlanAction::Run => {
            let created = PlanReviewCoordinator::create_task_from_plan(
                &mut session,
                root_config,
                workspace_root,
                parent_session_ref,
                &CreateTaskFromPlanRequest {
                    plan_id: command.plan_id.clone(),
                    expected_plan_hash: command.expected_plan_hash.clone(),
                    start_mode: PlanTaskStartMode::CreateAndRun,
                    permission_grant: command.permission_grant,
                },
            )?;
            ApplicationPlanDecisionReceipt {
                plan_id: command.plan_id.clone(),
                plan_hash: draft.plan_hash,
                action: ApplicationPlanAction::Run,
                task_id: Some(created.task_id_value),
                user_input_request: None,
                revision_request: None,
            }
        }
        ApplicationPlanAction::Save => {
            PlanReviewCoordinator::record_plan_decision(
                &mut session,
                &PlanDecisionCommand {
                    plan_id: command.plan_id.clone(),
                    expected_plan_hash: command.expected_plan_hash.clone(),
                    decision: PlanDecision::SavedOnly,
                },
                now_ms(),
            )?;
            ApplicationPlanDecisionReceipt {
                plan_id: command.plan_id.clone(),
                plan_hash: draft.plan_hash,
                action: ApplicationPlanAction::Save,
                task_id: None,
                user_input_request: None,
                revision_request: None,
            }
        }
        ApplicationPlanAction::Revise => {
            let workspace_snapshot_id =
                plan_handoff_workspace_snapshot_id(root_config, workspace_root)
                    .ok()
                    .flatten();
            let revision_request = PlanReviewCoordinator::retry_plan_revision(
                &mut session,
                &plan_id,
                &command.expected_plan_hash,
                workspace_snapshot_id,
                now_ms(),
            )?;
            let user_input_request = if revision_request.is_none() {
                let requested = PlanReviewCoordinator::request_plan_revision_guidance(
                    &mut session,
                    &plan_id,
                    &command.expected_plan_hash,
                    now_ms(),
                )?;
                Some(
                    session
                        .user_input_projection()?
                        .request(&requested.request.identity)
                        .map(sigil_kernel::UserInputRequestStateV1::public_view)
                        .context("revision guidance request was not projected")?,
                )
            } else {
                None
            };
            ApplicationPlanDecisionReceipt {
                plan_id: command.plan_id.clone(),
                plan_hash: draft.plan_hash,
                action: ApplicationPlanAction::Revise,
                task_id: None,
                user_input_request,
                revision_request,
            }
        }
        ApplicationPlanAction::Reject => {
            PlanReviewCoordinator::reject_plan(
                &mut session,
                &RejectPlanRequest {
                    plan_id: command.plan_id.clone(),
                    expected_plan_hash: command.expected_plan_hash.clone(),
                },
            )?;
            ApplicationPlanDecisionReceipt {
                plan_id: command.plan_id.clone(),
                plan_hash: draft.plan_hash,
                action: ApplicationPlanAction::Reject,
                task_id: None,
                user_input_request: None,
                revision_request: None,
            }
        }
    };
    Ok(receipt)
}

/// Accepts one exact host-owned plan-revision guidance decision and prepares the supervised
/// revision attempt. The answer, terminal user-input resolution, and `RevisionRequested` fact
/// are written in one durable control batch before the request is returned.
pub fn application_plan_revision_guidance_decision(
    root_config: &RootConfig,
    workspace_root: &Path,
    session_log_path: &Path,
    expected_scope: &str,
    command: sigil_kernel::UserInputDecisionCommandV1,
) -> Result<(
    sigil_kernel::UserInputDecisionReceiptV1,
    Option<PlanReviewRunRequest>,
)> {
    let store = sigil_kernel::JsonlSessionStore::new(session_log_path)?;
    let (_, fallback_route) =
        crate::provider_connections::resolve_default_model_route(root_config)?;
    let mut session =
        crate::provider_connections::load_session_for_route_resume_with_directive_and_attachment(
            root_config,
            &fallback_route,
            store,
            None,
            None,
            None,
        )?;
    if session.session_scope_id() != expected_scope {
        bail!("plan revision guidance session scope mismatch");
    }
    PlanReviewCoordinator::accept_plan_revision_guidance(
        &mut session,
        command,
        plan_handoff_workspace_snapshot_id(root_config, workspace_root)
            .ok()
            .flatten(),
        now_ms(),
    )
}

/// Accepts one exact child-owned plan-research question through its bound parent session.
pub fn application_plan_review_research_input_decision(
    root_config: &RootConfig,
    session_log_path: &Path,
    expected_scope: &str,
    command: sigil_kernel::UserInputDecisionCommandV1,
) -> Result<(
    sigil_kernel::UserInputDecisionReceiptV1,
    Option<PlanReviewRunRequest>,
)> {
    let store = sigil_kernel::JsonlSessionStore::new(session_log_path)?;
    let (_, fallback_route) =
        crate::provider_connections::resolve_default_model_route(root_config)?;
    let mut session =
        crate::provider_connections::load_session_for_route_resume_with_directive_and_attachment(
            root_config,
            &fallback_route,
            store,
            None,
            None,
            None,
        )?;
    if session.session_scope_id() != expected_scope {
        bail!("plan-review research input parent scope mismatch");
    }
    PlanReviewCoordinator::accept_plan_review_research_input(&mut session, command, now_ms())
}

fn session_ref_for_log_path(path: &Path) -> Result<SessionRef> {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("session.jsonl");
    SessionRef::new_relative(file_name)
        .map_err(|error| anyhow!("failed to build parent session ref: {error}"))
}
