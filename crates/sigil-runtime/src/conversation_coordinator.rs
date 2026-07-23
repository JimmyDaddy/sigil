use std::collections::BTreeSet;

use anyhow::{Result, anyhow, bail};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    AgentRunInput, AgentRunPurpose, ControlEntry, ConversationPurposeContext, ConversationTurnRef,
    MessageRole, ModelMessage, Session, SessionLogEntry, SessionRef, StartDurableTaskAction,
    TaskAdmissionTrigger, TaskHandoffDecision, TaskHandoffId, TaskHandoffRequestedEntry,
    TaskHandoffResolvedEntry, TaskId, TaskParticipantAttemptStatus, TaskParticipantPurpose,
    TaskPlanStatus, TaskPlanningHandoffBinding, TaskRoutingPolicy, TaskRunEntry, TaskRunStatus,
    TaskStepEntry, TaskStepStatus, WriteLeaseReleaseStatus, WriteLeaseReleased,
    durable_task_cancellation_requested, reconcile_task_final_answer_prefix, safe_persistence_text,
    task_planner_logical_run_id,
};

const TASK_HANDOFF_ID_DOMAIN: &str = "sigil-task-handoff-v1";
const TASK_ID_DOMAIN: &str = "sigil-task-v1";
const TASK_ROUTING_POLICY_DOMAIN: &str = "sigil-task-routing-policy-v1";
const EXPLICIT_TASK_POLICY_DOMAIN: &str = "sigil-explicit-task-policy-v1";

/// Explicit source binding for already-persisted direct or queued user input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationSourceTurn {
    pub message_id: String,
    pub objective: String,
}

/// Runtime-owned admission service for one conversation-to-task transition.
#[derive(Debug, Clone)]
pub struct ConversationCoordinator {
    task_enabled: bool,
    routing_policy: TaskRoutingPolicy,
}

impl ConversationCoordinator {
    #[must_use]
    pub fn new(task_enabled: bool, routing_policy: TaskRoutingPolicy) -> Self {
        Self {
            task_enabled,
            routing_policy,
        }
    }

    /// Binds a root conversation run to its exact user turn and optional automatic handoff.
    ///
    /// The model only receives `request_task_planning` when task routing is enabled and set to
    /// `Auto`. Stable identities and the safe objective are frozen before provider dispatch.
    ///
    /// # Errors
    ///
    /// Returns an error when source identity is missing, the source conflicts with durable state,
    /// or existing handoff facts disagree with the deterministic binding.
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
        let effective_policy = if self.task_enabled {
            self.routing_policy
        } else {
            TaskRoutingPolicy::Manual
        };
        let task_handoff = if effective_policy == TaskRoutingPolicy::Auto {
            Some(self.binding_for_source(
                session,
                source_turn.clone(),
                parent_session_ref,
                source.objective,
                now_ms,
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
                    task_handoff,
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
            requested_at_ms: existing
                .and_then(|state| state.request.as_ref())
                .map_or(now_ms, |request| request.requested_at_ms),
            decided_at_ms: existing
                .and_then(|state| state.resolution.as_ref())
                .map_or(now_ms, |resolution| resolution.decided_at_ms),
        })
    }
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
                objective,
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
