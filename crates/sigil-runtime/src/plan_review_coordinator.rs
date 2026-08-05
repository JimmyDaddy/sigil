use std::path::Path;

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use sigil_kernel::{
    Agent, AgentRunDisposition, AgentRunInput, AgentRunOptions, AgentRunPurpose, ControlEntry,
    ConversationRoute, ConversationRouteDecisionProjection, ConversationTurnRef, EventHandler,
    IntentAcceptanceAuthorityV1, IntentAdmissionContextV1, IntentStackId, ModelMessage,
    PlanApprovalExpiry, PlanApprovalPermission, PlanApprovalScope, PlanDecision, PlanDecisionActor,
    PlanDecisionRecordedEntry, PlanDraftCreatedEntry, PlanId, PlanPermissionGrantedEntry,
    PlanReviewAttemptEntry, PlanReviewAttemptId, PlanReviewAttemptStatus, PlanReviewId,
    PlanReviewProjection, PlanReviewSource, PlanReviewTerminalReason, PlanSourceRef,
    PlanTaskStartMode, Session, SessionLogEntry, SessionRef, StartPlanReviewAction,
    TaskCreatedFromPlanEntry, TaskId, TaskPlanEntry, TaskPlanStatus, TaskRunEntry, TaskRunStatus,
    admit_suggested_decomposition, append_task_intent_plan_admission, bind_task_plan_intents,
    build_workspace_snapshot, plan_review_attempt_id_for_review,
    plan_review_attempt_id_for_revision, plan_review_child_session_ref,
    plan_review_id_for_explicit_command, plan_review_no_draft_retry_contract_material,
    plan_review_plan_id_for_attempt, plan_review_system_prompt_contract_material,
    plan_task_input_from_draft, safe_persistence_text, stable_event_uuid, stable_workspace_id,
    task_id_from_plan_draft, task_plan_from_plan_draft,
};

use sigil_kernel::ApprovalHandler;

use crate::{RootConfig, attach_session_url_capability_store};

/// Host-owned outcome of one plan review run.
#[derive(Debug, Clone)]
pub enum PlanReviewRunOutcome {
    DraftReady { draft: Box<PlanDraftCreatedEntry> },
    CompletedWithoutDraft,
    Cancelled,
    Interrupted(String),
    Failed(String),
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
        let request = PlanReviewRunRequest {
            plan_review_id: action.plan_review_id.clone(),
            attempt_id,
            plan_id: action.plan_id.clone(),
            source: PlanReviewSource::AutomaticConversationRoute,
            source_turn: action.source_turn.clone(),
            route_decision_id: Some(action.decision_id.clone()),
            workspace_snapshot_id,
            child_session_ref,
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
            objective,
            workspace_snapshot_id,
        };
        Self::ensure_attempt_started(session, &request, now_ms)?;
        Ok(request)
    }

    /// Runs the read-only plan review child session.
    ///
    /// The run uses the read-only tool registry, never writes to the parent session, and closes
    /// with a validated typed draft. When the model finishes without a draft, the host injects one
    /// bounded retry contract; a second draft-less finish closes with `CompletedWithoutDraft`.
    #[allow(clippy::too_many_arguments)]
    pub async fn run_plan_review<H, A>(
        parent_session: &Session,
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
        for attempt in 0..=1 {
            if cancellation.is_cancel_requested() {
                return Ok(PlanReviewRunOutcome::Cancelled);
            }
            let mut transient = vec![ModelMessage::system(
                plan_review_system_prompt_contract_material(),
            )];
            if attempt == 1 {
                transient.push(ModelMessage::system(
                    plan_review_no_draft_retry_contract_material(),
                ));
            }
            transient.push(ModelMessage::user(request.objective.clone()));
            let input = AgentRunInput::without_persisted_user_message(transient)
                .with_logical_run_id(request.child_logical_run_id())
                .with_cancellation(cancellation.clone())
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
            let output = agent
                .run_with_approval_input_and_tool_registry(
                    &mut child_session,
                    input,
                    options.clone(),
                    tool_registry.clone(),
                    handler,
                    approval_handler,
                )
                .await?;
            match output.disposition {
                AgentRunDisposition::PlanReviewDraftSubmitted(action) => {
                    let draft = child_session
                        .plan_artifact_projection()
                        .plans
                        .get(&action.plan_id)
                        .cloned()
                        .ok_or_else(|| {
                            anyhow!(
                                "plan review draft {} is missing from its child session",
                                action.plan_id.as_str()
                            )
                        })?;
                    return Ok(PlanReviewRunOutcome::DraftReady {
                        draft: Box::new(draft),
                    });
                }
                AgentRunDisposition::FinalAnswer => {
                    if attempt == 0 {
                        continue;
                    }
                    return Ok(PlanReviewRunOutcome::CompletedWithoutDraft);
                }
                AgentRunDisposition::Interrupted => {
                    return Ok(PlanReviewRunOutcome::Interrupted(
                        "plan review run was interrupted before a draft".to_owned(),
                    ));
                }
                AgentRunDisposition::Blocked => {
                    return Ok(PlanReviewRunOutcome::Failed(
                        "plan review run was blocked before a draft".to_owned(),
                    ));
                }
                AgentRunDisposition::StartDurableTask(_)
                | AgentRunDisposition::TaskPlanAccepted => {
                    return Ok(PlanReviewRunOutcome::Failed(
                        "plan review run attempted an out-of-scope handoff".to_owned(),
                    ));
                }
                AgentRunDisposition::StartPlanReview(_) => {
                    return Ok(PlanReviewRunOutcome::Failed(
                        "plan review run requested a nested plan review".to_owned(),
                    ));
                }
            }
        }
        Ok(PlanReviewRunOutcome::CompletedWithoutDraft)
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
        if draft.plan_id != request.plan_id {
            bail!(
                "plan review child draft {} does not match its bound plan {}",
                draft.plan_id.as_str(),
                request.plan_id.as_str()
            );
        }
        let projection = parent.plan_artifact_projection();
        match projection.plans.get(&request.plan_id) {
            Some(existing) if existing == draft => {}
            Some(_) => {
                bail!(
                    "plan {} already has conflicting durable facts",
                    request.plan_id.as_str()
                );
            }
            None => parent.append_control(ControlEntry::PlanDraftCreated(draft.clone()))?,
        }
        append_attempt_status(
            parent,
            request,
            PlanReviewAttemptStatus::DraftReady,
            None,
            now_ms,
        )
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
        match outcome {
            PlanReviewRunOutcome::DraftReady { .. }
            | PlanReviewRunOutcome::CompletedWithoutDraft => Ok(()),
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
        }
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
        )
    }

    /// Prepares a revision attempt for an existing plan review lifecycle.
    ///
    /// Records `RevisionRequested`, derives the next retry-stable attempt identity, and returns
    /// the host-bound run request for the new read-only plan review run. The attempt `Started`
    /// record is appended by the run executor via [`PlanReviewCoordinator::ensure_attempt_started`]
    /// right before the run
    /// starts: persisting `Started` here would be misread as a crashed run when the executor
    /// reloads the session across a process boundary. The objective is recovered from the
    /// original source turn when available; otherwise the current draft summary is used as the
    /// revision prompt (never as an authority).
    ///
    /// # Errors
    ///
    /// Returns an error for a stale hash, a missing draft, or a conflicting decision.
    pub fn prepare_plan_review_revision(
        session: &mut Session,
        plan_id: &PlanId,
        expected_plan_hash: &str,
        workspace_snapshot_id: Option<String>,
        now_ms: u64,
    ) -> Result<PlanReviewRunRequest> {
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
        if projection.plan_is_rejected(plan_id) {
            bail!("plan {} was rejected", plan_id.as_str());
        }
        if projection.task_created_for_plan(plan_id) {
            bail!("plan {} already created a task", plan_id.as_str());
        }
        if let Some(existing) = projection.latest_decision(plan_id)
            && existing.decision != PlanDecision::RevisionFailed
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
        let plan_review_id = previous.plan_review_id.clone();
        let attempt_id = plan_review_attempt_id_for_revision(&plan_review_id, &previous.attempt_id);
        let next_plan_id = plan_review_plan_id_for_attempt(&plan_review_id, &attempt_id);
        let objective = source_turn_objective(session, &previous.source_turn)
            .filter(|objective| !objective.trim().is_empty())
            .unwrap_or_else(|| draft.summary.clone());
        let request = PlanReviewRunRequest {
            plan_review_id: plan_review_id.clone(),
            attempt_id: attempt_id.clone(),
            plan_id: next_plan_id,
            source: previous.source,
            source_turn: previous.source_turn,
            route_decision_id: previous.route_decision_id,
            child_session_ref: plan_review_child_session_ref(&plan_review_id, &attempt_id),
            objective,
            workspace_snapshot_id,
        };
        session.append_control(ControlEntry::PlanDecisionRecorded(
            PlanDecisionRecordedEntry {
                plan_id: plan_id.clone(),
                plan_hash: draft.plan_hash,
                decision: PlanDecision::RevisionRequested,
                decided_by: PlanDecisionActor::User,
                decided_at_ms: now_ms,
                reason: Some("revise plan".to_owned()),
            },
        ))?;
        Ok(request)
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
        if projection.plan_is_rejected(&plan_id) {
            bail!("plan {} was rejected", plan_id.as_str());
        }
        if command.decision == PlanDecision::Accepted && projection.task_created_for_plan(&plan_id)
        {
            bail!("plan {} already created a task", plan_id.as_str());
        }
        if let Some(existing) = projection.latest_decision(&plan_id) {
            if existing.decision == command.decision && existing.plan_hash == draft.plan_hash {
                return Ok(existing.clone());
            }
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
        if projection.task_created_for_plan(&plan_id) {
            bail!("plan {} already created a task", plan_id.as_str());
        }
        if let Some(decision) = projection.latest_decision(&plan_id) {
            bail!(
                "plan {} already has decision {}",
                plan_id.as_str(),
                decision.decision.as_str()
            );
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
            if existing.attempt_id == request.attempt_id {
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
    let projection = PlanReviewProjection::from_entries(session.entries());
    if let Some(existing) = projection.latest_attempt(&request.plan_review_id) {
        if existing.attempt_id == request.attempt_id && existing.status == status {
            return Ok(());
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
        status,
        terminal_reason,
        recorded_at_ms: now_ms,
    };
    projection.validate_append(&entry)?;
    session.append_control(ControlEntry::PlanReviewAttempt(entry))?;
    Ok(())
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
                revision_request: None,
            }
        }
        ApplicationPlanAction::Revise => {
            let revision_request = PlanReviewCoordinator::prepare_plan_review_revision(
                &mut session,
                &plan_id,
                &command.expected_plan_hash,
                plan_handoff_workspace_snapshot_id(root_config, workspace_root)
                    .ok()
                    .flatten(),
                now_ms(),
            )?;
            ApplicationPlanDecisionReceipt {
                plan_id: command.plan_id.clone(),
                plan_hash: draft.plan_hash,
                action: ApplicationPlanAction::Revise,
                task_id: None,
                revision_request: Some(revision_request),
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
                revision_request: None,
            }
        }
    };
    Ok(receipt)
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
