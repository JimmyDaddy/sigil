use std::collections::BTreeSet;

use anyhow::{Result, anyhow, bail};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    AgentRunInput, AgentRunPurpose, AutomaticRouteCapability, ContinueDurableTaskAction,
    ControlEntry, ConversationPurposeContext, ConversationTurnRef, MessageRole, ModelMessage,
    PendingPlanHandoffBinding, PlanReviewAttemptStatus, PlanReviewHandoffBinding,
    RecoverableTaskGuidanceReviewAuthority, Session, SessionLogEntry, SessionRef,
    StartDurableTaskAction, TaskAdmissionTrigger, TaskContinuationControl,
    TaskContinuationHandoffBinding, TaskHandoffDecision, TaskHandoffId, TaskHandoffRequestedEntry,
    TaskHandoffResolvedEntry, TaskId, TaskParticipantAttemptStatus, TaskParticipantPurpose,
    TaskPlanStatus, TaskPlanningHandoffBinding, TaskRoutingPolicy, TaskRunEntry, TaskRunStatus,
    TaskStepEntry, TaskStepStatus, WriteLeaseReleaseStatus, WriteLeaseReleased,
    conversation_route_contract_fingerprint, conversation_route_decision_id_for_source,
    conversation_route_routing_contract_material, durable_task_cancellation_requested,
    plan_review_attempt_id_for_review, plan_review_id_for_source, plan_review_plan_id_for_attempt,
    plan_review_policy_snapshot_hash, reconcile_task_final_answer_prefix,
    reconcile_task_step_projections, recoverable_task_guidance_review,
    route_surface_tool_specs_for_bound_context, route_surface_tool_specs_with_memory,
    safe_persistence_text, task_planner_logical_run_id,
};

const TASK_HANDOFF_ID_DOMAIN: &str = "sigil-task-handoff-v1";
const TASK_ID_DOMAIN: &str = "sigil-task-v1";
const TASK_ROUTING_POLICY_DOMAIN: &str = "sigil-task-routing-policy-v1";
const EXPLICIT_TASK_POLICY_DOMAIN: &str = "sigil-explicit-task-policy-v1";
const TASK_CONTINUATION_POLICY_DOMAIN: &str = "sigil-task-continuation-policy-v1";

#[derive(Debug, Clone)]
struct TaskContinuationCandidate {
    task_id: TaskId,
    plan_version: Option<u32>,
    task_status: TaskRunStatus,
    plan_status: Option<TaskPlanStatus>,
}

/// Host-owned evidence used to derive the automatic route capability tier.
///
/// The model cannot modify this evidence. `provider_supports_routing_tools` reflects the
/// effective provider/tool capability; `route_qualified` reflects exact-route qualification
/// evidence from the release manifest. The default is the RFC-0063 baseline: routing enabled at
/// `ReviewFirst`, never `DirectTask` without exact qualification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteCapabilityEvidence {
    pub provider_supports_routing_tools: bool,
    pub route_qualified: bool,
}

impl Default for RouteCapabilityEvidence {
    fn default() -> Self {
        Self {
            provider_supports_routing_tools: true,
            route_qualified: false,
        }
    }
}

/// Explicit source binding for already-persisted direct or queued user input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationSourceTurn {
    pub message_id: String,
    pub objective: String,
}

/// Runtime-owned admission service for one conversation-to-route transition.
#[derive(Debug, Clone)]
pub struct ConversationCoordinator {
    task_enabled: bool,
    routing_policy: TaskRoutingPolicy,
    orchestration_route_guard: Option<crate::OrchestrationRouteGuard>,
    route_capability_evidence: RouteCapabilityEvidence,
    writable_memory_routing: bool,
}

impl ConversationCoordinator {
    #[must_use]
    pub fn new(task_enabled: bool, routing_policy: TaskRoutingPolicy) -> Self {
        Self {
            task_enabled,
            routing_policy,
            orchestration_route_guard: None,
            route_capability_evidence: RouteCapabilityEvidence::default(),
            writable_memory_routing: false,
        }
    }

    #[must_use]
    pub fn with_orchestration_route_guard(
        mut self,
        orchestration_route_guard: crate::OrchestrationRouteGuard,
    ) -> Self {
        self.orchestration_route_guard = Some(orchestration_route_guard);
        self
    }

    /// Binds the exact provider/tool and release-qualification evidence used to derive the
    /// automatic route capability tier.
    #[must_use]
    pub fn with_route_capability_evidence(mut self, evidence: RouteCapabilityEvidence) -> Self {
        self.route_capability_evidence = evidence;
        self
    }

    /// Allows previewed writable-memory calls to accompany an automatic route decision.
    #[must_use]
    pub fn with_writable_memory_routing(mut self, enabled: bool) -> Self {
        self.writable_memory_routing = enabled;
        self
    }

    /// Returns the exact model-visible tool contracts for one routing microturn.
    #[must_use]
    pub fn route_tool_specs(
        &self,
        capability: AutomaticRouteCapability,
    ) -> Vec<sigil_kernel::ToolSpec> {
        route_surface_tool_specs_with_memory(capability, self.writable_memory_routing)
    }

    /// Returns the exact routing surface for the current durable session focus.
    #[must_use]
    pub fn route_tool_specs_for_session(
        &self,
        session: &Session,
        capability: AutomaticRouteCapability,
    ) -> Vec<sigil_kernel::ToolSpec> {
        route_surface_tool_specs_for_bound_context(
            capability,
            self.writable_memory_routing,
            task_continuation_candidate(session, None).is_some(),
            draft_ready_pending_plan(session).is_some(),
        )
    }

    /// Persists a route-local kill switch when durable facts expose a hard invariant.
    ///
    /// # Errors
    ///
    /// Returns an error when the disablement cannot be validated or durably appended.
    pub fn enforce_orchestration_route_kill_switch(
        &self,
        session: &mut Session,
        now_ms: u64,
    ) -> Result<Option<sigil_kernel::OrchestrationRouteDisabledEntry>> {
        let Some(guard) = &self.orchestration_route_guard else {
            return Ok(None);
        };
        guard.enforce(session, now_ms)
    }

    /// Returns the effective automatic route capability for the current session.
    ///
    /// `Manual` configuration, a disabled task mode, or a provider that cannot stream tool calls
    /// all resolve to `Unsupported`. Without exact-route qualification evidence the capability
    /// resolves to `ReviewFirst`, never `DirectTask`. A route-local kill switch (hard invariant)
    /// degrades `DirectTask` to the `ReviewFirst` baseline but keeps the safe, reviewable
    /// automatic plan review handoff.
    #[must_use]
    pub fn resolve_route_capability(&self, session: &Session) -> AutomaticRouteCapability {
        if !self.task_enabled {
            return AutomaticRouteCapability::Unsupported;
        }
        if self.effective_routing_policy(session) != TaskRoutingPolicy::Auto {
            return AutomaticRouteCapability::Unsupported;
        }
        if !self
            .route_capability_evidence
            .provider_supports_routing_tools
        {
            return AutomaticRouteCapability::Unsupported;
        }
        if self.route_capability_evidence.route_qualified
            && !self
                .orchestration_route_guard
                .as_ref()
                .is_some_and(|guard| guard.direct_task_blocked(session))
        {
            AutomaticRouteCapability::DirectTask
        } else {
            AutomaticRouteCapability::ReviewFirst
        }
    }

    fn effective_routing_policy(&self, session: &Session) -> TaskRoutingPolicy {
        self.orchestration_route_guard
            .as_ref()
            .map_or(self.routing_policy, |guard| {
                guard.effective_policy(session, self.routing_policy)
            })
    }

    /// Computes the deterministic route-contract fingerprint for one capability tier.
    ///
    /// The fingerprint binds the routing contract, the exact tool surface, the effective
    /// capability, and host route facts (provider/model/build/route fingerprint), and is recorded
    /// with every durable route decision.
    fn route_contract_fingerprint(
        &self,
        session: &Session,
        capability: AutomaticRouteCapability,
        continuation: Option<&TaskContinuationCandidate>,
        pending_plan: Option<&PendingPlanHandoffBinding>,
    ) -> String {
        let mut host_facts = self
            .orchestration_route_guard
            .as_ref()
            .map(|guard| {
                vec![
                    ("provider", session.provider_name()),
                    ("model", session.model_name()),
                    ("build", guard.sigil_build()),
                    ("route", guard.route_fingerprint()),
                ]
            })
            .unwrap_or_else(|| {
                vec![
                    ("provider", session.provider_name()),
                    ("model", session.model_name()),
                ]
            });
        let continuation_plan_version = continuation.map(|continuation| {
            continuation
                .plan_version
                .map(|version| version.to_string())
                .unwrap_or_else(|| "none".to_owned())
        });
        if let Some(continuation) = continuation {
            host_facts.extend([
                ("continuation_task", continuation.task_id.as_str()),
                (
                    "continuation_plan_version",
                    continuation_plan_version.as_deref().unwrap_or("none"),
                ),
                (
                    "continuation_status",
                    task_run_status_name(continuation.task_status),
                ),
                (
                    "continuation_plan_status",
                    continuation
                        .plan_status
                        .map(task_plan_status_name)
                        .unwrap_or("none"),
                ),
            ]);
        }
        if let Some(pending_plan) = pending_plan {
            host_facts.extend([
                ("pending_plan_id", pending_plan.plan_id.as_str()),
                ("pending_plan_hash", pending_plan.plan_hash.as_str()),
            ]);
        }
        conversation_route_contract_fingerprint(
            conversation_route_routing_contract_material(),
            &route_surface_tool_specs_for_bound_context(
                capability,
                self.writable_memory_routing,
                continuation.is_some(),
                pending_plan.is_some(),
            ),
            capability,
            &host_facts,
        )
    }

    /// Binds a root conversation run to its exact user turn and optional automatic route.
    ///
    /// The model only receives typed routing decision tools when the effective capability routes
    /// automatically. Stable identities and the safe objective are frozen before provider
    /// dispatch.
    ///
    /// # Errors
    ///
    /// Returns an error when source identity is missing, the source conflicts with durable state,
    /// or existing handoff/route facts disagree with the deterministic binding.
    #[allow(clippy::too_many_arguments)]
    pub fn bind_conversation_input(
        &self,
        session: &Session,
        input: AgentRunInput,
        parent_session_ref: SessionRef,
        root_logical_run_id: impl Into<String>,
        source_override: Option<ConversationSourceTurn>,
        now_ms: u64,
    ) -> Result<AgentRunInput> {
        let root_logical_run_id = root_logical_run_id.into();
        if root_logical_run_id.trim().is_empty() {
            bail!("conversation root logical run id is empty");
        }
        let source = match source_override {
            Some(source) => {
                validate_existing_source_turn(session, &source)?;
                source
            }
            None => source_from_direct_input(&input)?,
        };
        let source_turn = ConversationTurnRef::new(
            session.session_scope_id(),
            source.message_id,
            root_logical_run_id.clone(),
        )?;
        let exact_source_prompt = input
            .exact_user_prompt_for_source(&source_turn.message_id)
            .map(ToOwned::to_owned);
        let capability = self.resolve_route_capability(session);
        let routes_automatically = capability.routes_automatically();
        let effective_policy = if routes_automatically {
            TaskRoutingPolicy::Auto
        } else {
            TaskRoutingPolicy::Manual
        };
        let task_continuation_candidate = routes_automatically
            .then(|| task_continuation_candidate(session, Some(&source_turn.message_id)))
            .flatten();
        let pending_plan = routes_automatically
            .then(|| draft_ready_pending_plan(session))
            .flatten();
        let route_contract_fingerprint = if routes_automatically {
            Some(self.route_contract_fingerprint(
                session,
                capability,
                task_continuation_candidate.as_ref(),
                pending_plan.as_ref(),
            ))
        } else {
            None
        };
        let task_continuation = task_continuation_candidate
            .map(|candidate| {
                let exact_guidance = exact_source_prompt.as_deref().ok_or_else(|| {
                    anyhow!(
                        "current Task continuation requires exact process-local source prompt material"
                    )
                })?;
                let prompt = sigil_kernel::project_conversation_prompt_for_persistence(exact_guidance);
                Ok::<TaskContinuationHandoffBinding, anyhow::Error>(
                    TaskContinuationHandoffBinding {
                        task_id: candidate.task_id,
                        source_turn: source_turn.clone(),
                        plan_version: candidate.plan_version,
                        task_status: candidate.task_status,
                        plan_status: candidate.plan_status,
                        effective_capability: capability,
                        policy_snapshot_hash: task_continuation_policy_snapshot_hash(),
                        route_contract_fingerprint: route_contract_fingerprint
                            .clone()
                            .unwrap_or_default(),
                        decided_at_ms: now_ms,
                        exact_guidance: sigil_kernel::SecretString::new(exact_guidance),
                        prompt_hash: prompt.prompt_hash,
                        exact_prompt_required: prompt.exact_prompt_required,
                        safe_guidance: prompt.safe_prompt,
                    },
                )
            })
            .transpose()?;
        let task_handoff = if capability.allows_direct_task() {
            Some(self.binding_for_source(
                session,
                source_turn.clone(),
                parent_session_ref,
                source.objective.clone(),
                now_ms,
                route_contract_fingerprint.clone().unwrap_or_default(),
            )?)
        } else {
            None
        };
        let plan_review = if routes_automatically {
            Some(self.plan_review_binding_for_source(
                session,
                source_turn.clone(),
                source.objective,
                now_ms,
                route_contract_fingerprint.unwrap_or_default(),
            )?)
        } else {
            None
        };
        Ok(input
            .with_logical_run_id(root_logical_run_id.clone())
            .with_run_purpose(AgentRunPurpose::Conversation(Box::new(
                ConversationPurposeContext {
                    root_run_id: root_logical_run_id,
                    source_turn,
                    routing_policy: effective_policy,
                    route_capability: capability,
                    writable_memory_routing: routes_automatically && self.writable_memory_routing,
                    task_handoff,
                    plan_review,
                    task_continuation,
                },
            ))))
    }

    /// Persists and admits an explicit user task through the same durable handoff protocol.
    ///
    /// # Errors
    ///
    /// Returns an error when task mode is disabled, the source is not a user message, or durable
    /// source/handoff/task facts conflict with the deterministic admission.
    pub fn admit_explicit_task(
        &self,
        session: &mut Session,
        mut user_message: ModelMessage,
        parent_session_ref: SessionRef,
        root_logical_run_id: impl Into<String>,
        now_ms: u64,
    ) -> Result<StartDurableTaskAction> {
        if !self.task_enabled {
            bail!("task planning is disabled in config");
        }
        if user_message.role != MessageRole::User {
            bail!("explicit task admission requires a user message");
        }
        let root_logical_run_id = root_logical_run_id.into();
        if root_logical_run_id.trim().is_empty() {
            bail!("explicit task root logical run id is empty");
        }
        let objective = safe_persistence_text(user_message.content.as_deref().unwrap_or_default());
        if objective.trim().is_empty() {
            bail!("explicit task objective is empty");
        }
        user_message.content = Some(objective.clone());
        let source_message_exists = match session.entries().iter().find_map(|entry| match entry {
            SessionLogEntry::User(existing) if existing.id == user_message.id => Some(existing),
            _ => None,
        }) {
            Some(existing)
                if existing.role != MessageRole::User
                    || existing.content != user_message.content
                    || !existing.tool_calls.is_empty()
                    || existing.tool_call_id.is_some()
                    || existing.assistant_kind.is_some()
                    || !existing.image_attachments.is_empty() =>
            {
                bail!("explicit task source message id conflicts with durable content");
            }
            Some(_) => true,
            None => false,
        };

        let source_turn = ConversationTurnRef::new(
            session.session_scope_id(),
            user_message.id.clone(),
            root_logical_run_id,
        )?;
        let handoff_id = handoff_id_for_source(&source_turn)?;
        let task_id = task_id_for_handoff(&handoff_id)?;
        let projection = session.task_handoff_projection();
        if projection.has_conflicts() {
            bail!("task handoff projection contains conflicting durable facts");
        }
        let existing = projection.handoffs.get(&handoff_id);
        let requested = TaskHandoffRequestedEntry {
            handoff_id: handoff_id.clone(),
            source_turn: source_turn.clone(),
            trigger: TaskAdmissionTrigger::ExplicitTaskCommand,
            reason_codes: Vec::new(),
            recovery_objective: Some(objective.clone()),
            policy_snapshot_hash: explicit_task_policy_snapshot_hash(),
            requested_at_ms: existing
                .and_then(|state| state.request.as_ref())
                .map_or(now_ms, |entry| entry.requested_at_ms),
        };
        let resolved = TaskHandoffResolvedEntry {
            handoff_id: handoff_id.clone(),
            decision: TaskHandoffDecision::Accepted,
            task_id: Some(task_id.clone()),
            decided_at_ms: existing
                .and_then(|state| state.resolution.as_ref())
                .map_or(now_ms, |entry| entry.decided_at_ms),
        };
        if let Some(state) = projection.handoffs.get(&handoff_id)
            && (state
                .request
                .as_ref()
                .is_some_and(|entry| entry != &requested)
                || state
                    .resolution
                    .as_ref()
                    .is_some_and(|entry| entry != &resolved))
        {
            bail!("explicit task admission conflicts with durable handoff facts");
        }
        let request_exists = projection
            .handoffs
            .get(&handoff_id)
            .and_then(|state| state.request.as_ref())
            .is_some();
        if !request_exists {
            // Requested is the single recovery-critical admission anchor. It carries the safe
            // explicit objective so reconciliation can reconstruct the User entry if the process
            // exits before the following append.
            session.append_control(ControlEntry::TaskHandoffRequested(requested.clone()))?;
        }
        if !source_message_exists {
            session.append_user_message(user_message.clone())?;
        }
        if projection
            .handoffs
            .get(&handoff_id)
            .and_then(|state| state.resolution.as_ref())
            .is_none()
        {
            session.append_control(ControlEntry::TaskHandoffResolved(resolved))?;
        }
        ensure_task_started(
            session,
            &task_id,
            &parent_session_ref,
            &objective,
            "admitted by explicit task command",
        )?;
        Ok(StartDurableTaskAction {
            handoff_id,
            task_id,
            source_turn,
        })
    }

    /// Repairs local crash gaps without replaying a provider request.
    ///
    /// Requested handoffs are resolved from their durable policy snapshot, and accepted handoffs
    /// missing a task run receive the same deterministic `TaskRun::Started` fact. Repeated calls
    /// append nothing after the projection is complete.
    ///
    /// # Errors
    ///
    /// Returns an error for conflicting handoff facts, unsupported policy snapshots, missing
    /// source turns, or an existing task whose facts disagree with the handoff.
    pub fn reconcile(
        &self,
        session: &mut Session,
        parent_session_ref: &SessionRef,
        now_ms: u64,
    ) -> Result<Vec<StartDurableTaskAction>> {
        let projection = session.task_handoff_projection();
        if projection.has_conflicts() {
            bail!("task handoff projection contains conflicting durable facts");
        }
        reconcile_result_backed_participant_attempts(session)?;
        interrupt_durably_cancelled_active_tasks(session)?;
        let states = projection.handoffs.into_iter().collect::<Vec<_>>();
        let mut actions = Vec::new();
        for (handoff_id, state) in states {
            let request = state.request.ok_or_else(|| {
                anyhow!(
                    "task handoff {} has a resolution without a request",
                    handoff_id.as_str()
                )
            })?;
            validate_supported_request(&request)?;
            let expected_handoff_id = handoff_id_for_source(&request.source_turn)?;
            if handoff_id != expected_handoff_id {
                bail!("task handoff id does not match its durable source turn");
            }
            let task_id = task_id_for_handoff(&handoff_id)?;
            let resolution = match state.resolution {
                Some(resolution) => resolution,
                None => {
                    let resolution = TaskHandoffResolvedEntry {
                        handoff_id: handoff_id.clone(),
                        decision: TaskHandoffDecision::Accepted,
                        task_id: Some(task_id.clone()),
                        decided_at_ms: now_ms,
                    };
                    session
                        .append_control(ControlEntry::TaskHandoffResolved(resolution.clone()))?;
                    resolution
                }
            };
            if resolution.decision != TaskHandoffDecision::Accepted
                || resolution.task_id.as_ref() != Some(&task_id)
            {
                bail!("task handoff resolution conflicts with deterministic admission");
            }
            let objective = match source_turn_objective(session, &request.source_turn) {
                Some(objective) => {
                    if request
                        .recovery_objective
                        .as_ref()
                        .is_some_and(|recovery| recovery != &objective)
                    {
                        bail!(
                            "task handoff recovery objective conflicts with its source user turn"
                        );
                    }
                    objective
                }
                None => recover_explicit_source_turn(session, &request)?,
            };
            let task_was_created = ensure_task_started(
                session,
                &task_id,
                parent_session_ref,
                &objective,
                "reconciled accepted conversation handoff",
            )?;
            if durable_task_cancellation_requested(session, task_id.as_str())? {
                interrupt_task_after_durable_cancellation(session, &task_id)?;
                continue;
            }
            reconcile_committed_planner_attempts(session, &task_id)?;
            let task = session
                .task_state_projection()
                .tasks
                .get(&task_id)
                .cloned()
                .ok_or_else(|| anyhow!("reconciled task is missing from task projection"))?;
            let safe_to_resume = if task_was_created {
                true
            } else if task.status == TaskRunStatus::Started {
                let has_uncertain_participant = task
                    .participant_attempts
                    .values()
                    .any(|attempt| attempt.status == TaskParticipantAttemptStatus::Started)
                    || task
                        .steps
                        .values()
                        .any(|step| step.status == TaskStepStatus::Running);
                let accepted_plan = task.latest_plan_version.is_some_and(|version| {
                    task.plans
                        .get(&version)
                        .is_some_and(|plan| plan.status == TaskPlanStatus::Accepted)
                });
                !has_uncertain_participant
                    && (accepted_plan || !task_planner_dispatch_seen(session, &task_id)?)
            } else {
                false
            };
            if safe_to_resume {
                actions.push(StartDurableTaskAction {
                    handoff_id,
                    task_id,
                    source_turn: request.source_turn,
                });
            } else if matches!(task.status, TaskRunStatus::Started | TaskRunStatus::Running) {
                pause_uncertain_task(session, &task_id, parent_session_ref, &objective)?;
            }
        }

        let repairable_task_ids = session
            .task_state_projection()
            .tasks
            .values()
            .filter(|task| matches!(task.status, TaskRunStatus::Started | TaskRunStatus::Running))
            .filter(|task| {
                task.final_answer.is_some()
                    || task.participant_attempts.values().any(|attempt| {
                        attempt.purpose == TaskParticipantPurpose::Synthesis
                            && attempt.status == TaskParticipantAttemptStatus::Completed
                            && task.participant_results.contains_key(&attempt.attempt_id)
                    })
            })
            .map(|task| task.task_id.clone())
            .collect::<Vec<_>>();
        // Reproject stale blocked step facts before deciding whether a continuation can resume.
        // This is an append-only repair through the kernel task writer, not a JSONL rewrite.
        let reprojection_task_ids = session
            .task_state_projection()
            .tasks
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for task_id in reprojection_task_ids {
            reconcile_task_step_projections(session, &task_id)?;
        }
        for task_id in repairable_task_ids {
            reconcile_task_final_answer_prefix(session, &task_id)?;
        }
        let repaired_projection = session.task_state_projection();
        actions.retain(|action| {
            repaired_projection
                .tasks
                .get(&action.task_id)
                .is_some_and(|task| {
                    matches!(task.status, TaskRunStatus::Started | TaskRunStatus::Running)
                })
        });

        let resumable_task_ids = actions
            .iter()
            .map(|action| action.task_id.clone())
            .collect::<BTreeSet<_>>();
        let uncertain_tasks = session
            .task_state_projection()
            .tasks
            .values()
            .filter(|task| matches!(task.status, TaskRunStatus::Started | TaskRunStatus::Running))
            .filter(|task| !resumable_task_ids.contains(&task.task_id))
            .map(|task| {
                (
                    task.task_id.clone(),
                    task.parent_session_ref.clone(),
                    task.objective.clone(),
                )
            })
            .collect::<Vec<_>>();
        for (task_id, parent_session_ref, objective) in uncertain_tasks {
            pause_uncertain_task(session, &task_id, &parent_session_ref, &objective)?;
        }
        Ok(actions)
    }

    fn binding_for_source(
        &self,
        session: &Session,
        source_turn: ConversationTurnRef,
        parent_session_ref: SessionRef,
        objective: String,
        now_ms: u64,
        route_contract_fingerprint: String,
    ) -> Result<TaskPlanningHandoffBinding> {
        let expected_handoff_id = handoff_id_for_source(&source_turn)?;
        let expected_task_id = task_id_for_handoff(&expected_handoff_id)?;
        let projection = session.task_handoff_projection();
        if projection.has_conflicts() {
            bail!("task handoff projection contains conflicting durable facts");
        }
        let existing = projection.handoff_for_source(&source_turn);
        if let Some(existing_request) = existing.and_then(|state| state.request.as_ref())
            && existing_request.handoff_id != expected_handoff_id
        {
            bail!("source turn is bound to a non-deterministic task handoff id");
        }
        if let Some(existing_resolution) = existing.and_then(|state| state.resolution.as_ref())
            && (existing_resolution.decision != TaskHandoffDecision::Accepted
                || existing_resolution.task_id.as_ref() != Some(&expected_task_id))
        {
            bail!("source turn has a conflicting task handoff resolution");
        }
        Ok(TaskPlanningHandoffBinding {
            handoff_id: expected_handoff_id,
            task_id: expected_task_id,
            source_turn,
            parent_session_ref,
            objective,
            policy_snapshot_hash: automatic_policy_snapshot_hash(),
            route_contract_fingerprint,
            requested_at_ms: existing
                .and_then(|state| state.request.as_ref())
                .map_or(now_ms, |request| request.requested_at_ms),
            decided_at_ms: existing
                .and_then(|state| state.resolution.as_ref())
                .map_or(now_ms, |resolution| resolution.decided_at_ms),
        })
    }

    /// Builds the host-bound PlanReview handoff identity for one source turn.
    ///
    /// All identities derive deterministically from the exact source turn with distinct domain
    /// separators, so retries and crash recovery never mint a second conflicting decision.
    fn plan_review_binding_for_source(
        &self,
        session: &Session,
        source_turn: ConversationTurnRef,
        objective: String,
        now_ms: u64,
        route_contract_fingerprint: String,
    ) -> Result<PlanReviewHandoffBinding> {
        let decision_id = conversation_route_decision_id_for_source(&source_turn);
        let plan_review_id = plan_review_id_for_source(&source_turn);
        let attempt_id = plan_review_attempt_id_for_review(&plan_review_id);
        let plan_id = plan_review_plan_id_for_attempt(&plan_review_id, &attempt_id);
        let projection =
            sigil_kernel::ConversationRouteDecisionProjection::from_entries(session.entries());
        if projection.has_conflicts() {
            bail!("conversation route decision projection contains conflicting durable facts");
        }
        if let Some(existing) = projection.decision_id_for_source(&source_turn)
            && existing != &decision_id
        {
            bail!("source turn is bound to a different route decision");
        }
        let existing = projection.decision(&decision_id);
        Ok(PlanReviewHandoffBinding {
            decision_id,
            plan_review_id,
            attempt_id,
            plan_id,
            source_turn,
            objective,
            policy_snapshot_hash: plan_review_policy_snapshot_hash(),
            route_contract_fingerprint,
            pending_plan: draft_ready_pending_plan(session),
            requested_at_ms: existing.map_or(now_ms, |decision| decision.decided_at_ms),
            decided_at_ms: existing.map_or(now_ms, |decision| decision.decided_at_ms),
        })
    }
}

/// Revalidates a semantic Task continuation at the adapter dispatch boundary.
///
/// This is deliberately stricter than resolving a Task by id: the exact source turn, route
/// fingerprint, Task status, plan version, and plan status must still match the frozen route.
pub fn validate_task_continuation_action(
    session: &Session,
    action: &ContinueDurableTaskAction,
) -> Result<crate::agent_supervisor::task_execution::ResolvedTaskContinuation> {
    if action.source_turn.session_scope_id != session.session_scope_id() {
        bail!("Task continuation action belongs to another session");
    }
    let selected = session
        .entries()
        .iter()
        .rev()
        .find_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::TaskContinuationSelected(selected))
                if selected.source_turn == action.source_turn =>
            {
                Some(selected)
            }
            _ => None,
        });
    let selected = selected.ok_or_else(|| {
        anyhow!("Task continuation action is missing its durable selection receipt")
    })?;
    let legacy_receipt_upgrade =
        selected.control == sigil_kernel::TaskContinuationControlKind::LegacyUnspecified && {
            let mut upgraded = selected.clone();
            upgraded.control = action.guidance_receipt.control;
            upgraded == action.guidance_receipt
        };
    if (selected != &action.guidance_receipt && !legacy_receipt_upgrade)
        || selected.task_id != action.task_id
        || selected.plan_version != action.plan_version
        || selected.task_status != action.task_status
        || selected.plan_status != action.plan_status
        || selected.route_contract_fingerprint != action.route_contract_fingerprint
    {
        bail!("Task continuation action conflicts with its durable selection receipt");
    }
    let action_is_resume = matches!(action.control(), TaskContinuationControl::ResumeTask);
    let receipt_is_resume =
        action.guidance_receipt.control == sigil_kernel::TaskContinuationControlKind::ResumeTask;
    if action_is_resume != receipt_is_resume {
        bail!("Task continuation control conflicts with its durable selection receipt");
    }
    let prompt =
        sigil_kernel::project_conversation_prompt_for_persistence(action.guidance.expose_secret());
    if !action_is_resume
        && (prompt.prompt_hash != selected.prompt_hash
            || prompt.safe_prompt != selected.guidance
            || prompt.exact_prompt_required != selected.exact_prompt_required)
    {
        bail!("Task continuation exact guidance no longer matches its durable receipt");
    }
    let route = sigil_kernel::ConversationRouteDecisionProjection::from_entries(session.entries());
    if route.has_conflicts() {
        bail!("Task continuation route projection contains conflicting durable facts");
    }
    let decision = route
        .decision_for_source(&action.source_turn)
        .ok_or_else(|| anyhow!("Task continuation action is missing its durable route decision"))?;
    if decision.route != sigil_kernel::ConversationRoute::Task
        || decision.route_contract_fingerprint != action.route_contract_fingerprint
    {
        bail!("Task continuation route changed after the model decision");
    }
    let projection = session.task_state_projection();
    let pending_selection_matches = recoverable_task_guidance_review(
        session,
        &action.task_id,
        (!action_is_resume).then_some(action.guidance.expose_secret()),
    )?
    .is_some_and(|review| {
        matches!(
            review.authority,
            RecoverableTaskGuidanceReviewAuthority::ContinuationSelected(recorded)
                if recorded.as_ref() == selected
        )
    });
    let focus_matches = match projection.current_task_id.as_ref() {
        Some(current_task_id) => current_task_id == &action.task_id,
        None => pending_selection_matches,
    };
    if projection.focus_conflicts != 0 || !focus_matches {
        bail!("Task continuation is no longer the current durable run target");
    }
    let task = projection
        .tasks
        .get(&action.task_id)
        .ok_or_else(|| anyhow!("Task continuation target is no longer present"))?;
    let plan_status = action
        .plan_version
        .and_then(|version| task.plans.get(&version).map(|plan| plan.status));
    if (task.status != action.task_status && !pending_selection_matches)
        || task.latest_plan_version != action.plan_version
        || plan_status != action.plan_status
    {
        bail!("Task continuation target changed before adapter dispatch");
    }
    crate::agent_supervisor::task_execution::resolve_task_continuation(
        session,
        Some(action.task_id.as_str()),
    )
}

fn interrupt_durably_cancelled_active_tasks(session: &mut Session) -> Result<()> {
    let active_task_ids = session
        .task_state_projection()
        .tasks
        .values()
        .filter(|task| {
            matches!(
                task.status,
                TaskRunStatus::Started | TaskRunStatus::Running | TaskRunStatus::Paused
            )
        })
        .map(|task| task.task_id.clone())
        .collect::<Vec<_>>();
    for task_id in active_task_ids {
        if durable_task_cancellation_requested(session, task_id.as_str())? {
            interrupt_task_after_durable_cancellation(session, &task_id)?;
        }
    }
    Ok(())
}

fn reconcile_result_backed_participant_attempts(session: &mut Session) -> Result<()> {
    let result_backed_attempts = session
        .task_state_projection()
        .tasks
        .values()
        .filter(|task| {
            matches!(
                task.status,
                TaskRunStatus::Started | TaskRunStatus::Running | TaskRunStatus::Paused
            )
        })
        .flat_map(|task| {
            task.participant_attempts
                .values()
                .filter_map(|attempt| {
                    task.participant_results
                        .get(&attempt.attempt_id)
                        .map(|result| {
                            (
                                task.parent_session_ref.clone(),
                                task.objective.clone(),
                                attempt.clone(),
                                result.clone(),
                                attempt.step_id.as_ref().and_then(|step_id| {
                                    attempt.plan_version.and_then(|plan_version| {
                                        task.steps.get(&(plan_version, step_id.clone())).cloned()
                                    })
                                }),
                            )
                        })
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    for (parent_session_ref, objective, mut attempt, result, step) in result_backed_attempts {
        if attempt.status == TaskParticipantAttemptStatus::Started {
            let terminal_status = result
                .terminal_status
                .or_else(|| {
                    (attempt.purpose == TaskParticipantPurpose::Synthesis
                        && result.final_answer_ref.is_some())
                    .then_some(TaskParticipantAttemptStatus::Completed)
                })
                .or_else(|| {
                    (attempt.purpose == TaskParticipantPurpose::Step)
                        .then_some(TaskParticipantAttemptStatus::Interrupted)
                });
            if let Some(terminal_status) = terminal_status {
                attempt.status = terminal_status;
                attempt.reason = Some(
                    "reconciled participant result persisted before its terminal marker".to_owned(),
                );
                session.append_control(ControlEntry::TaskParticipantAttempt(attempt.clone()))?;
            }
        }

        if attempt.purpose != TaskParticipantPurpose::Step {
            continue;
        }
        let Some(step) = step else {
            continue;
        };
        if step.status.is_terminal() {
            continue;
        }
        session.append_control(ControlEntry::TaskStep(TaskStepEntry {
            task_id: attempt.task_id.clone(),
            plan_version: step.plan_version,
            step_id: step.step_id,
            role: step.role,
            status: TaskStepStatus::Blocked,
            title: step.title,
            summary: Some(result.summary),
            reason: Some(
                "participant result was committed before readiness and step status; manual review is required before replanning"
                    .to_owned(),
            ),
        }))?;
        release_active_task_write_leases(session, &attempt.task_id)?;
        let task_status = session
            .task_state_projection()
            .tasks
            .get(&attempt.task_id)
            .map(|task| task.status);
        if task_status
            .is_some_and(|status| matches!(status, TaskRunStatus::Started | TaskRunStatus::Running))
        {
            session.append_control(ControlEntry::TaskRun(TaskRunEntry {
                task_id: attempt.task_id.clone(),
                parent_session_ref,
                objective: objective.clone(),
                title: Some(sigil_kernel::task_semantic_title(&objective)),
                status: TaskRunStatus::Paused,
                reason: Some(
                    "step result recovery stopped before readiness commit; manual review or replan is required"
                        .to_owned(),
                ),
            }))?;
        }
    }
    Ok(())
}

fn interrupt_task_after_durable_cancellation(
    session: &mut Session,
    task_id: &TaskId,
) -> Result<()> {
    let task = session
        .task_state_projection()
        .tasks
        .get(task_id)
        .cloned()
        .ok_or_else(|| anyhow!("cancelled task is missing from task projection"))?;
    if !matches!(
        task.status,
        TaskRunStatus::Started | TaskRunStatus::Running | TaskRunStatus::Paused
    ) {
        return Ok(());
    }
    if matches!(task.status, TaskRunStatus::Started | TaskRunStatus::Running) {
        pause_uncertain_task(
            session,
            &task.task_id,
            &task.parent_session_ref,
            &task.objective,
        )?;
    }
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task.task_id,
        parent_session_ref: task.parent_session_ref,
        objective: task.objective,
        title: None,
        status: TaskRunStatus::Interrupted,
        reason: Some(
            "durable cancellation won before crash recovery; final answer repair is suppressed"
                .to_owned(),
        ),
    }))?;
    Ok(())
}

fn source_from_direct_input(input: &AgentRunInput) -> Result<ConversationSourceTurn> {
    let durable_message = input
        .durable_user_message_projection()?
        .ok_or_else(|| anyhow!("coordinated direct input is missing its user message"))?;
    let objective = durable_message.content.unwrap_or_default();
    Ok(ConversationSourceTurn {
        message_id: durable_message.id,
        objective,
    })
}

fn draft_ready_pending_plan(session: &Session) -> Option<PendingPlanHandoffBinding> {
    let artifacts = session.plan_artifact_projection();
    let draft = artifacts.latest_pending_plan()?;
    let reviews = sigil_kernel::PlanReviewProjection::from_entries(session.entries());
    if !reviews.conflicts.is_empty()
        || reviews
            .attempt_for_plan(&draft.plan_id)
            .is_none_or(|attempt| attempt.status != PlanReviewAttemptStatus::DraftReady)
    {
        return None;
    }
    Some(PendingPlanHandoffBinding {
        plan_id: draft.plan_id.clone(),
        plan_hash: draft.plan_hash.clone(),
    })
}

fn task_continuation_candidate(
    session: &Session,
    source_message_id: Option<&str>,
) -> Option<TaskContinuationCandidate> {
    let entries = session.entries();
    let prefix = source_message_id
        .and_then(|message_id| {
            entries.iter().position(|entry| match entry {
                SessionLogEntry::User(message) => message.id == message_id,
                SessionLogEntry::Control(ControlEntry::ConversationInputPromoted(promoted)) => {
                    promoted.durable_user_message.id == message_id
                }
                _ => false,
            })
        })
        .map_or(entries, |index| &entries[..index]);
    let projection = sigil_kernel::TaskStateProjection::from_entries(prefix);
    if projection.focus_conflicts != 0 {
        return None;
    }
    let task = projection.current_task()?;
    if !matches!(
        task.status,
        TaskRunStatus::Started
            | TaskRunStatus::Paused
            | TaskRunStatus::Failed
            | TaskRunStatus::Interrupted
    ) {
        return None;
    }
    let plan_status = task
        .latest_plan_version
        .and_then(|version| task.plans.get(&version).map(|plan| plan.status));
    if plan_status != Some(TaskPlanStatus::Accepted) {
        return None;
    }
    Some(TaskContinuationCandidate {
        task_id: task.task_id.clone(),
        plan_version: task.latest_plan_version,
        task_status: task.status,
        plan_status,
    })
}

fn task_run_status_name(status: TaskRunStatus) -> &'static str {
    match status {
        TaskRunStatus::Started => "started",
        TaskRunStatus::Running => "running",
        TaskRunStatus::Paused => "paused",
        TaskRunStatus::Completed => "completed",
        TaskRunStatus::Failed => "failed",
        TaskRunStatus::Cancelled => "cancelled",
        TaskRunStatus::Interrupted => "interrupted",
    }
}

fn task_plan_status_name(status: TaskPlanStatus) -> &'static str {
    match status {
        TaskPlanStatus::Proposed => "proposed",
        TaskPlanStatus::Accepted => "accepted",
        TaskPlanStatus::Superseded => "superseded",
        TaskPlanStatus::Rejected => "rejected",
    }
}

fn validate_existing_source_turn(session: &Session, source: &ConversationSourceTurn) -> Result<()> {
    let durable_objective = source_turn_objective_by_id(session, &source.message_id)
        .ok_or_else(|| anyhow!("coordinated source user turn is not present in the session"))?;
    if durable_objective != source.objective {
        bail!("coordinated source objective conflicts with the durable user turn");
    }
    Ok(())
}

fn validate_supported_request(request: &TaskHandoffRequestedEntry) -> Result<()> {
    let expected_policy = match request.trigger {
        TaskAdmissionTrigger::ModelRequested => automatic_policy_snapshot_hash(),
        TaskAdmissionTrigger::ExplicitTaskCommand => explicit_task_policy_snapshot_hash(),
        TaskAdmissionTrigger::ApprovedPlan | TaskAdmissionTrigger::ExplicitUserDelegation => {
            bail!("reconciliation does not support this task admission trigger yet");
        }
    };
    if request.policy_snapshot_hash != expected_policy {
        bail!("task handoff uses an unsupported durable policy snapshot");
    }
    Ok(())
}

fn ensure_task_started(
    session: &mut Session,
    task_id: &TaskId,
    parent_session_ref: &SessionRef,
    objective: &str,
    reason: &str,
) -> Result<bool> {
    if let Some(task) = session.task_state_projection().tasks.get(task_id) {
        if &task.parent_session_ref != parent_session_ref || task.objective != objective {
            bail!("task handoff target already exists with conflicting task facts");
        }
        return Ok(false);
    }
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref: parent_session_ref.clone(),
        objective: objective.to_owned(),
        title: None,
        status: TaskRunStatus::Started,
        reason: Some(reason.to_owned()),
    }))?;
    Ok(true)
}

fn task_planner_dispatch_seen(session: &Session, task_id: &TaskId) -> Result<bool> {
    if session.store_path().is_none() {
        return Ok(false);
    }
    let attempts = session.provider_physical_attempt_projection()?;
    Ok(!attempts
        .attempts_for_logical_run_id(&task_planner_logical_run_id(task_id))
        .is_empty())
}

/// Closes a planner attempt left `Started` when the parent plan commit survived but the child
/// completion record did not. The plan batch is the authoritative recovery boundary: it is
/// written before the runtime records child completion, so a matching accepted plan proves the
/// planner result is already durable and can be resumed without re-running the planner.
fn reconcile_committed_planner_attempts(session: &mut Session, task_id: &TaskId) -> Result<usize> {
    let projection = session.task_state_projection();
    let Some(task) = projection.tasks.get(task_id) else {
        return Ok(0);
    };
    let Some(plan_version) = task.latest_plan_version else {
        return Ok(0);
    };
    let Some(plan) = task.plans.get(&plan_version) else {
        return Ok(0);
    };
    let plan_is_recoverable = plan.status == TaskPlanStatus::Accepted
        && (plan.step_contracts.is_empty() || plan.contract_set_committed_v2);
    if !plan_is_recoverable {
        return Ok(0);
    }
    let attempts = task
        .participant_attempts
        .values()
        .filter(|attempt| {
            attempt.purpose == TaskParticipantPurpose::Planner
                && attempt.status == TaskParticipantAttemptStatus::Started
        })
        .cloned()
        .collect::<Vec<_>>();
    for mut attempt in attempts.iter().cloned() {
        attempt.status = TaskParticipantAttemptStatus::Completed;
        attempt.reason = Some(format!(
            "recovered accepted task plan v{plan_version} after parent plan commit"
        ));
        session.append_control(ControlEntry::TaskParticipantAttempt(attempt))?;
    }
    Ok(attempts.len())
}

fn pause_uncertain_task(
    session: &mut Session,
    task_id: &TaskId,
    parent_session_ref: &SessionRef,
    objective: &str,
) -> Result<()> {
    let task = session
        .task_state_projection()
        .tasks
        .get(task_id)
        .cloned()
        .ok_or_else(|| anyhow!("uncertain task is missing from task projection"))?;
    for attempt in task
        .participant_attempts
        .values()
        .filter(|attempt| attempt.status == TaskParticipantAttemptStatus::Started)
    {
        let mut interrupted = attempt.clone();
        interrupted.status = TaskParticipantAttemptStatus::Interrupted;
        interrupted.reason = Some(
            "interrupted during crash recovery; explicit task continue is required".to_owned(),
        );
        session.append_control(ControlEntry::TaskParticipantAttempt(interrupted))?;
    }
    for step in task
        .steps
        .values()
        .filter(|step| step.status == TaskStepStatus::Running)
    {
        session.append_control(ControlEntry::TaskStep(TaskStepEntry {
            task_id: task_id.clone(),
            plan_version: step.plan_version,
            step_id: step.step_id.clone(),
            role: step.role,
            status: TaskStepStatus::Interrupted,
            title: step.title.clone(),
            summary: step.summary.clone(),
            reason: Some(
                "interrupted during crash recovery; explicit task continue is required".to_owned(),
            ),
        }))?;
    }

    release_active_task_write_leases(session, task_id)?;
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref: parent_session_ref.clone(),
        objective: objective.to_owned(),
        title: Some(sigil_kernel::task_semantic_title(objective)),
        status: TaskRunStatus::Paused,
        reason: Some(
            "recovery found uncertain planner or participant execution; explicit continue required"
                .to_owned(),
        ),
    }))
}

fn release_active_task_write_leases(session: &mut Session, task_id: &TaskId) -> Result<()> {
    let owner_prefix = format!("task:{}:", task_id.as_str());
    let stale_task_leases = session
        .write_isolation_projection()
        .leases
        .values()
        .filter(|state| state.is_active())
        .filter_map(|state| state.acquired.as_ref().map(|entry| (state, entry)))
        .filter(|(_, entry)| entry.owner_agent_id.as_str().starts_with(&owner_prefix))
        .map(|(state, _)| WriteLeaseReleased {
            lease_id: state.lease_id.clone(),
            status: WriteLeaseReleaseStatus::Interrupted,
        })
        .collect::<Vec<_>>();
    for release in stale_task_leases {
        session.append_control(ControlEntry::WriteLeaseReleased(release))?;
    }
    Ok(())
}

fn source_turn_objective(session: &Session, source_turn: &ConversationTurnRef) -> Option<String> {
    if source_turn.session_scope_id != session.session_scope_id() {
        return None;
    }
    source_turn_objective_by_id(session, &source_turn.message_id)
}

fn recover_explicit_source_turn(
    session: &mut Session,
    request: &TaskHandoffRequestedEntry,
) -> Result<String> {
    if request.trigger != TaskAdmissionTrigger::ExplicitTaskCommand {
        bail!(
            "task handoff source user turn {} is not present",
            request.source_turn.message_id
        );
    }
    let objective = request
        .recovery_objective
        .as_deref()
        .map(safe_persistence_text)
        .filter(|objective| !objective.trim().is_empty())
        .ok_or_else(|| anyhow!("explicit task handoff is missing its recovery objective"))?;
    let mut user_message = ModelMessage::user(objective.clone());
    user_message.id = request.source_turn.message_id.clone();
    session.append_user_message(user_message)?;
    Ok(objective)
}

fn source_turn_objective_by_id(session: &Session, message_id: &str) -> Option<String> {
    session.entries().iter().find_map(|entry| match entry {
        SessionLogEntry::User(message) if message.id == message_id => {
            Some(message.content.clone().unwrap_or_default())
        }
        SessionLogEntry::Control(ControlEntry::ConversationInputPromoted(promoted))
            if promoted.durable_user_message.id == message_id =>
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

fn handoff_id_for_source(source_turn: &ConversationTurnRef) -> Result<TaskHandoffId> {
    TaskHandoffId::new(format!(
        "handoff-{}",
        domain_hash(
            TASK_HANDOFF_ID_DOMAIN,
            &[
                &source_turn.session_scope_id,
                &source_turn.message_id,
                &source_turn.logical_run_id,
            ],
        )
    ))
}

fn task_id_for_handoff(handoff_id: &TaskHandoffId) -> Result<TaskId> {
    TaskId::new(format!(
        "task-{}",
        domain_hash(TASK_ID_DOMAIN, &[handoff_id.as_str()])
    ))
}

fn automatic_policy_snapshot_hash() -> String {
    format!(
        "sha256:{}",
        domain_hash(
            TASK_ROUTING_POLICY_DOMAIN,
            &["enabled=true", "routing=auto"]
        )
    )
}

fn task_continuation_policy_snapshot_hash() -> String {
    format!(
        "sha256:{}",
        domain_hash(
            TASK_CONTINUATION_POLICY_DOMAIN,
            &["enabled=true", "routing=auto", "target=current_resumable"]
        )
    )
}

fn explicit_task_policy_snapshot_hash() -> String {
    format!(
        "sha256:{}",
        domain_hash(
            EXPLICIT_TASK_POLICY_DOMAIN,
            &["trigger=explicit_task_command"]
        )
    )
}

fn domain_hash(domain: &str, parts: &[&str]) -> String {
    let mut digest = Sha256::new();
    digest.update(domain.as_bytes());
    for part in parts {
        digest.update([0]);
        digest.update(part.as_bytes());
    }
    format!("{:x}", digest.finalize())
}

#[cfg(test)]
#[path = "tests/conversation_coordinator_tests.rs"]
mod tests;
