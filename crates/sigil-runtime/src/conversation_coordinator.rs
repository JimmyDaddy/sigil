use anyhow::{Result, anyhow, bail};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    AgentRunInput, AgentRunPurpose, AutomaticRouteCapability, ContinueDurableTaskAction,
    ControlEntry, ConversationPurposeContext, ConversationTurnRef, MessageRole, ModelMessage,
    PendingPlanHandoffBinding, PlanReviewAttemptStatus, PlanReviewHandoffBinding, Session,
    SessionLogEntry, SessionRef, StartDurableTaskAction, TaskAdmissionTrigger,
    TaskContinuationControl, TaskContinuationHandoffBinding, TaskExecutionAttemptStatus,
    TaskHandoffDecision, TaskHandoffId, TaskHandoffRequestedEntry, TaskHandoffResolvedEntry,
    TaskId, TaskRoutingPolicy, TaskRunEntry, TaskRunStatus, TaskStartHandoffBinding,
    conversation_auto_execution_contract_material, conversation_route_contract_fingerprint,
    conversation_route_decision_id_for_source, conversation_tool_specs_for_bound_context,
    durable_task_cancellation_requested, plan_review_attempt_id_for_review,
    plan_review_id_for_source, plan_review_plan_id_for_attempt, plan_review_policy_snapshot_hash,
    route_surface_tool_specs_for_bound_context, route_surface_tool_specs_with_memory,
    safe_persistence_text,
};

const TASK_HANDOFF_ID_DOMAIN: &str = "sigil-task-handoff-v1";
const TASK_ID_DOMAIN: &str = "sigil-task-v1";
const TASK_ROUTING_POLICY_DOMAIN: &str = "sigil-task-routing-policy-v1";
const EXPLICIT_TASK_POLICY_DOMAIN: &str = "sigil-explicit-task-policy-v1";
const TASK_CONTINUATION_POLICY_DOMAIN: &str = "sigil-task-continuation-policy-v1";

#[derive(Debug, Clone)]
struct TaskContinuationCandidate {
    task_id: TaskId,
    task_status: TaskRunStatus,
}

/// Host-owned evidence used to derive the automatic route capability tier.
///
/// The model cannot modify this evidence. `provider_supports_routing_tools` reflects the
/// effective provider/tool capability; `task_executor_available` reflects an attached executor.
/// Release evaluation evidence informs installation defaults, not execution permission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteCapabilityEvidence {
    pub provider_supports_routing_tools: bool,
    pub task_executor_available: bool,
}

impl Default for RouteCapabilityEvidence {
    fn default() -> Self {
        Self {
            provider_supports_routing_tools: true,
            task_executor_available: false,
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

    /// Binds the actual provider/tool and executor availability used to derive the
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

    /// Returns optional handoff and memory tool contracts for one automatic capability.
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

    /// Returns the complete first-turn tool surface, preserving ordinary tools outside bound routing.
    #[must_use]
    pub fn conversation_tool_specs_for_session(
        &self,
        session: &Session,
        capability: AutomaticRouteCapability,
        ordinary_tools: Vec<sigil_kernel::ToolSpec>,
    ) -> Vec<sigil_kernel::ToolSpec> {
        conversation_tool_specs_for_bound_context(
            ordinary_tools,
            capability,
            self.writable_memory_routing,
            task_continuation_candidate(session, None).is_some(),
            draft_ready_pending_plan(session).is_some(),
        )
    }

    /// Returns the model contract selected by capability and durable Plan/Task bindings.
    #[must_use]
    pub fn conversation_contract_for_session(
        &self,
        _session: &Session,
        capability: AutomaticRouteCapability,
    ) -> Option<&'static str> {
        capability
            .routes_automatically()
            .then_some(conversation_auto_execution_contract_material())
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
    /// all resolve to `Unsupported`. Without an attached executor the capability resolves to
    /// `ReviewFirst`. A route-local kill switch (hard invariant)
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
        if self.route_capability_evidence.task_executor_available
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
    /// The fingerprint binds the conversation contract, internal control and memory tools, the effective
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
        if let Some(continuation) = continuation {
            host_facts.extend([
                ("continuation_task", continuation.task_id.as_str()),
                ("continuation_status", continuation.task_status.as_str()),
            ]);
        }
        if let Some(pending_plan) = pending_plan {
            host_facts.extend([
                ("pending_plan_id", pending_plan.plan_id.as_str()),
                ("pending_plan_hash", pending_plan.plan_hash.as_str()),
            ]);
        }
        conversation_route_contract_fingerprint(
            conversation_auto_execution_contract_material(),
            &conversation_tool_specs_for_bound_context(
                if self.writable_memory_routing {
                    sigil_kernel::writable_memory_route_tool_specs()
                } else {
                    Vec::new()
                },
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
                        task_status: candidate.task_status,
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
            bail!("durable Task execution is disabled in config");
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
            title: None,
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
            None,
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
        session.recover_unfinished_provider_physical_attempts(now_ms)?;
        let projection = session.task_handoff_projection();
        if projection.has_conflicts() {
            bail!("task handoff projection contains conflicting durable facts");
        }
        let mut actions = Vec::new();
        for (handoff_id, state) in projection.handoffs {
            let request = state.request.ok_or_else(|| {
                anyhow!(
                    "task handoff {} has a resolution without a request",
                    handoff_id.as_str()
                )
            })?;
            validate_supported_request(&request)?;
            if handoff_id != handoff_id_for_source(&request.source_turn)? {
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
            let objective = source_turn_objective(session, &request.source_turn)
                .or_else(|| request.recovery_objective.clone())
                .ok_or_else(|| anyhow!("accepted Task handoff is missing its source objective"))?;
            ensure_task_started(
                session,
                &task_id,
                parent_session_ref,
                &objective,
                request.title.as_deref(),
                "admitted conversation Task",
            )?;
            if durable_task_cancellation_requested(session, task_id.as_str())? {
                interrupt_task_after_durable_cancellation(session, &task_id)?;
                continue;
            }
            let task = session
                .task_state_projection()
                .tasks
                .get(&task_id)
                .cloned()
                .ok_or_else(|| anyhow!("admitted Task is missing from task projection"))?;
            if matches!(task.status, TaskRunStatus::Started | TaskRunStatus::Running) {
                actions.push(StartDurableTaskAction {
                    handoff_id,
                    task_id,
                    source_turn: request.source_turn,
                });
            }
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
    ) -> Result<TaskStartHandoffBinding> {
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
        Ok(TaskStartHandoffBinding {
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
    if selected != &action.guidance_receipt
        || selected.task_id != action.task_id
        || selected.task_status != action.task_status
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
    let focus_matches = match projection.current_task_id.as_ref() {
        Some(current_task_id) => current_task_id == &action.task_id,
        None => false,
    };
    if projection.focus_conflicts != 0 || !focus_matches {
        bail!("Task continuation is no longer the current durable run target");
    }
    let task = projection
        .tasks
        .get(&action.task_id)
        .ok_or_else(|| anyhow!("Task continuation target is no longer present"))?;
    if task.status != action.task_status || task.latest_plan_version.is_some() {
        bail!("Task continuation target changed before adapter dispatch");
    }
    if task.direct_execution_admission.is_none() {
        bail!("Task continuation target has no direct execution admission");
    }
    crate::agent_supervisor::task_execution::resolve_task_continuation(
        session,
        Some(action.task_id.as_str()),
    )
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
    for mut attempt in task
        .direct_execution_attempts
        .values()
        .filter(|attempt| attempt.status == TaskExecutionAttemptStatus::Started)
        .cloned()
    {
        attempt.status = TaskExecutionAttemptStatus::Cancelled;
        attempt.reason =
            Some("durable cancellation won before direct Task execution completed".to_owned());
        session.append_control(ControlEntry::TaskDirectExecutionAttemptV1(attempt))?;
    }
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task.task_id,
        parent_session_ref: task.parent_session_ref,
        objective: task.objective,
        title: None,
        status: TaskRunStatus::Interrupted,
        reason: Some("durable cancellation won before direct Task execution completed".to_owned()),
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
        || !artifacts.plan_is_ready(&draft.plan_id)
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
    let task = projection
        .current_task()
        .or_else(|| projection.latest_unfinished_task())?;
    if !matches!(
        task.status,
        TaskRunStatus::Started
            | TaskRunStatus::Paused
            | TaskRunStatus::Failed
            | TaskRunStatus::Interrupted
    ) {
        return None;
    }
    if task.latest_plan_version.is_some() || task.direct_execution_admission.is_none() {
        return None;
    }
    Some(TaskContinuationCandidate {
        task_id: task.task_id.clone(),
        task_status: task.status,
    })
}

fn validate_existing_source_turn(session: &Session, source: &ConversationSourceTurn) -> Result<()> {
    let durable_objective = session
        .source_user_message(&source.message_id)
        .map(|message| message.content.clone().unwrap_or_default())
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
    title: Option<&str>,
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
        title: Some(title.map_or_else(
            || sigil_kernel::task_semantic_title(objective),
            str::to_owned,
        )),
        status: TaskRunStatus::Started,
        reason: Some(reason.to_owned()),
    }))?;
    Ok(true)
}

fn source_turn_objective(session: &Session, source_turn: &ConversationTurnRef) -> Option<String> {
    if source_turn.session_scope_id != session.session_scope_id() {
        return None;
    }
    session
        .source_user_message(&source_turn.message_id)
        .map(|message| message.content.clone().unwrap_or_default())
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
