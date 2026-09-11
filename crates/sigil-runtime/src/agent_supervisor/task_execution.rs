use std::{collections::BTreeMap, path::Path, sync::Arc};

use anyhow::{Context, Result, anyhow};
use sigil_kernel::verification::VerificationExecutionPortV1;
use sigil_kernel::{
    AgentRunOptions, ApprovalHandler, CheckDiscoverySource, CheckPromotion, CheckSpecRecordedEntry,
    CompletionCriteria, ControlEntry, DEFAULT_TASK_VERIFICATION_SCOPE_HASH, EventHandler,
    EvidenceScope, RecoverableTaskGuidanceReviewAuthority, RootConfig, RunCancellationHandle,
    RunCancellationOwner, RunCancellationRecorder, RunCancellationTarget, RunTaskGuard,
    SandboxProfileRequirement, SequentialTaskRequest, Session, SessionLogEntry, SessionRef,
    TaskChildSessionStatus, TaskContinuationSelectedEntry, TaskExecutionBindingV1,
    TaskGuidancePromotedEntry, TaskId, TaskParticipantAttemptEntry, TaskParticipantAttemptStatus,
    TaskPauseRequest, TaskRunCancellationScopeBoundEntry, TaskRunEntry, TaskRunStatus,
    TaskRunTargetSelectedEntry, TaskStepEntry, TaskStepStatus, ToolRegistry, VerificationPolicy,
    VerificationPolicyChangedEntry, WorkspaceTrustRequirement, check_specs_from_user_config,
    recoverable_task_guidance, recoverable_task_guidance_review,
    recoverable_task_guidance_review_retry_controls, safe_persistence_text, stable_workspace_id,
};
use thiserror::Error;

use super::{
    AgentSupervisor,
    task_role_runtime::{
        TaskRoleDemand, TaskRoleProviderBuilder, TaskRoleRuntime,
        build_task_role_runtime_for_route, build_task_role_runtime_for_route_with_demand,
        task_role_demand_for_continuation,
    },
};

/// Complete host-owned material needed to execute one already-admitted durable task.
pub struct AdmittedTaskExecution<'a, H> {
    pub task_id: TaskId,
    pub parent_session_ref: SessionRef,
    pub objective: String,
    pub root_config: RootConfig,
    pub options: AgentRunOptions,
    pub base_registry: ToolRegistry,
    pub agent_supervisor: AgentSupervisor,
    pub role_provider_builder: &'a dyn TaskRoleProviderBuilder,
    pub verification_execution_port: Arc<dyn VerificationExecutionPortV1>,
    pub handler: &'a mut H,
    pub cancellation_handle: RunCancellationHandle,
    pub tool_artifact_read_budget: Option<sigil_kernel::ToolArtifactReadBudgetV1>,
}

/// Host-owned durable Task selected for an explicit continuation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTaskContinuation {
    pub task_id: TaskId,
    pub parent_session_ref: SessionRef,
    pub objective: String,
    pub execution_route: ResolvedTaskExecutionRoute,
}

/// First-class execution route selected by durable Task authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedTaskExecutionRoute {
    NeedsPlanning,
    Planned,
    Direct,
}

impl ResolvedTaskContinuation {
    /// Returns whether this Task has no admitted execution authority yet.
    #[must_use]
    pub fn needs_planning(&self) -> bool {
        self.execution_route == ResolvedTaskExecutionRoute::NeedsPlanning
    }

    /// Returns whether this Task executes from a direct admission rather than a TaskPlan.
    #[must_use]
    pub fn is_direct(&self) -> bool {
        self.execution_route == ResolvedTaskExecutionRoute::Direct
    }
}

/// Complete host-owned material needed to continue one durable Task.
pub struct ContinuedTaskExecution<'a, H> {
    pub requested_task_id: Option<TaskId>,
    pub guidance: Option<String>,
    /// Real root conversation run owning an explicit guidance command, when no routed receipt exists.
    pub explicit_guidance_run_id: Option<String>,
    pub guidance_promotion: Option<TaskGuidancePromotedEntry>,
    pub continuation_guidance_receipt: Option<TaskContinuationSelectedEntry>,
    pub root_config: RootConfig,
    pub options: AgentRunOptions,
    pub base_registry: ToolRegistry,
    pub agent_supervisor: AgentSupervisor,
    pub role_provider_builder: &'a dyn TaskRoleProviderBuilder,
    pub verification_execution_port: Arc<dyn VerificationExecutionPortV1>,
    pub handler: &'a mut H,
    pub cancellation_handle: RunCancellationHandle,
    pub tool_artifact_read_budget: Option<sigil_kernel::ToolArtifactReadBudgetV1>,
}

/// Typed zero-dispatch blocker raised while preparing an admitted Task execution.
///
/// These failures happen before any participant provider request or tool effect. The durable
/// Task therefore remains resumable after the user repairs configuration or the environment.
#[derive(Debug, Error)]
pub enum TaskExecutionPreflightError {
    #[error("task verification preflight failed")]
    VerificationMaterialization(#[source] anyhow::Error),
    #[error("task role runtime preflight failed")]
    RoleRuntimeConstruction(#[source] anyhow::Error),
}

impl TaskExecutionPreflightError {
    const fn reason_code(&self) -> &'static str {
        match self {
            Self::VerificationMaterialization(_) => "task_verification_preflight_blocked",
            Self::RoleRuntimeConstruction(_) => "task_role_runtime_preflight_blocked",
        }
    }
}

/// Root cancellation authority and durable recorder for one Task execution.
pub struct PreparedTaskRunCancellation {
    pub owner: RunCancellationOwner,
    pub recorder: RunCancellationRecorder,
    pub handle: RunCancellationHandle,
    pub task_guard: RunTaskGuard,
}

/// Durable terminal state written after an active Task run has stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStopDisposition {
    /// The Task remains explicitly resumable.
    Paused,
    /// The user cancelled the Task after cleanup was confirmed.
    Cancelled,
    /// Cleanup, execution ownership, or the requested binding could not be confirmed.
    Interrupted,
}

impl TaskStopDisposition {
    fn task_status(self) -> TaskRunStatus {
        match self {
            Self::Paused => TaskRunStatus::Paused,
            Self::Cancelled => TaskRunStatus::Cancelled,
            Self::Interrupted => TaskRunStatus::Interrupted,
        }
    }

    fn step_status(self) -> TaskStepStatus {
        match self {
            Self::Cancelled => TaskStepStatus::Cancelled,
            Self::Paused | Self::Interrupted => TaskStepStatus::Interrupted,
        }
    }

    fn child_status(self) -> TaskChildSessionStatus {
        match self {
            Self::Cancelled => TaskChildSessionStatus::Cancelled,
            Self::Paused | Self::Interrupted => TaskChildSessionStatus::Interrupted,
        }
    }
}

/// Exact durable Task state transition appended after run quiescence.
#[derive(Debug, Clone)]
pub struct AppendedTaskStopState {
    task_id: TaskId,
    status: TaskRunStatus,
    controls: Vec<ControlEntry>,
}

impl AppendedTaskStopState {
    /// Returns the exact Task whose terminal control state was appended.
    #[must_use]
    pub fn task_id(&self) -> &TaskId {
        &self.task_id
    }

    /// Returns the Task status written by this transition.
    #[must_use]
    pub fn status(&self) -> TaskRunStatus {
        self.status
    }

    /// Returns the ordered controls written by this transition.
    #[must_use]
    pub fn controls(&self) -> &[ControlEntry] {
        &self.controls
    }
}

/// Stable validation failures for an exact Task pause action.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum TaskPauseValidationError {
    #[error("request identity does not match the rendered task binding")]
    InvalidRequestIdentity,
    #[error("active run belongs to another task")]
    TargetMismatch,
    #[error("task is no longer available")]
    TaskUnavailable,
    #[error("task execution authority changed since the pause action was rendered")]
    ExecutionAuthorityChanged,
    #[error("task is no longer running")]
    TaskNotRunning,
}

/// Stable failures while writing a quiesced Task stop transition.
#[derive(Debug, Error)]
pub enum TaskStopStateError {
    #[error("active task {task_id} is no longer available")]
    TaskUnavailable { task_id: String },
    #[error("task {task_id} is no longer running")]
    TaskNotRunning { task_id: String },
    #[error("failed to append stopped task state")]
    Persistence(#[source] anyhow::Error),
}

/// Creates a root cancellation scope and durably binds it to one exact Task.
///
/// The binding is appended before this function returns, so adapters cannot dispatch Task work
/// with an untracked cancellation scope.
///
/// # Errors
///
/// Returns an error when the durable cancellation recorder, root task guard, or Task binding
/// cannot be created.
pub fn prepare_task_run_cancellation(
    session: &mut Session,
    task_id: &TaskId,
) -> Result<PreparedTaskRunCancellation> {
    let recorder = session.run_cancellation_recorder()?;
    let owner = RunCancellationOwner::new();
    let handle = owner.handle();
    let task_guard = handle.register_task()?;
    bind_task_run_cancellation_scope(session, task_id, &handle)?;
    Ok(PreparedTaskRunCancellation {
        owner,
        recorder,
        handle,
        task_guard,
    })
}

/// Durably binds an existing root cancellation scope to one exact Task.
///
/// # Errors
///
/// Returns an error when the append-only control entry cannot be persisted.
pub fn bind_task_run_cancellation_scope(
    session: &mut Session,
    task_id: &TaskId,
    handle: &RunCancellationHandle,
) -> Result<()> {
    session.append_control(ControlEntry::TaskRunCancellationScopeBound(
        TaskRunCancellationScopeBoundEntry {
            task_id: task_id.clone(),
            run_scope_id: handle.scope_id().to_owned(),
        },
    ))
}

/// Validates that one rendered pause action still owns the active Task cancellation scope.
///
/// # Errors
///
/// Returns a stable validation error when the request identity, active scope, Task, plan version,
/// or running status changed since the action was rendered.
pub fn validate_task_pause_request(
    request: &TaskPauseRequest,
    cancellation_target: &RunCancellationTarget,
    active_scope_id: &str,
    entries: &[SessionLogEntry],
) -> std::result::Result<(), TaskPauseValidationError> {
    if !request.has_exact_identity() {
        return Err(TaskPauseValidationError::InvalidRequestIdentity);
    }
    let scope_matches = entries.iter().rev().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TaskRunCancellationScopeBound(binding))
                if binding.task_id == request.task_id
                    && binding.run_scope_id == active_scope_id
        )
    });
    let target_matches = match cancellation_target {
        RunCancellationTarget::Task { task_id } => {
            task_id == request.task_id.as_str() && scope_matches
        }
        RunCancellationTarget::Run => scope_matches,
        RunCancellationTarget::AgentThread { .. } => false,
    };
    if !target_matches {
        return Err(TaskPauseValidationError::TargetMismatch);
    }
    let projection = sigil_kernel::TaskStateProjection::from_entries(entries);
    let task = projection
        .tasks
        .get(&request.task_id)
        .ok_or(TaskPauseValidationError::TaskUnavailable)?;
    let authority_matches = match &request.execution {
        TaskExecutionBindingV1::Plan { plan_version } => {
            task.latest_plan_version == Some(*plan_version)
                && !task.superseded_plan_versions.contains(plan_version)
        }
        TaskExecutionBindingV1::Direct { admission_id } => task
            .direct_execution_admission
            .as_ref()
            .is_some_and(|admission| admission.admission_id == *admission_id),
    };
    if !authority_matches {
        return Err(TaskPauseValidationError::ExecutionAuthorityChanged);
    }
    if !matches!(task.status, TaskRunStatus::Started | TaskRunStatus::Running) {
        return Err(TaskPauseValidationError::TaskNotRunning);
    }
    Ok(())
}

/// Resolves the exact Task bound to one active cancellation scope.
///
/// A run-scoped cancellation only owns a Task when the durable log contains a matching
/// `TaskRunCancellationScopeBound` entry. This prevents an ordinary chat cancellation from
/// changing an unrelated older Task.
#[must_use]
pub(crate) fn task_id_for_cancellation_scope(
    entries: &[SessionLogEntry],
    cancellation_target: &RunCancellationTarget,
    active_scope_id: &str,
) -> Option<TaskId> {
    match cancellation_target {
        RunCancellationTarget::Task { task_id } => {
            entries.iter().rev().find_map(|entry| match entry {
                SessionLogEntry::Control(ControlEntry::TaskRunCancellationScopeBound(binding))
                    if binding.run_scope_id == active_scope_id
                        && binding.task_id.as_str() == task_id =>
                {
                    Some(binding.task_id.clone())
                }
                _ => None,
            })
        }
        RunCancellationTarget::Run => entries.iter().rev().find_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::TaskRunCancellationScopeBound(binding))
                if binding.run_scope_id == active_scope_id =>
            {
                Some(binding.task_id.clone())
            }
            _ => None,
        }),
        RunCancellationTarget::AgentThread { .. } => None,
    }
}

/// Interrupts only the Task bound to this root scope after its caller has proven quiescence.
///
/// An already settled Task retains its durable outcome; cancellation may race with a pause or
/// terminal append before the root run joins. An unbound ordinary run does not select any Task.
///
/// # Errors
///
/// Returns an error when a bound Task is absent or its append-only stop transition cannot persist.
pub fn append_run_scoped_task_interruption<H>(
    session: &mut Session,
    handler: &mut H,
    active_scope_id: &str,
    reason: &str,
) -> std::result::Result<Option<AppendedTaskStopState>, TaskStopStateError>
where
    H: EventHandler + ?Sized,
{
    let Some(task_id) = task_id_for_cancellation_scope(
        session.entries(),
        &RunCancellationTarget::Run,
        active_scope_id,
    ) else {
        return Ok(None);
    };
    match append_task_stop_state(
        session,
        handler,
        Some(&task_id),
        TaskStopDisposition::Interrupted,
        reason,
    ) {
        Err(TaskStopStateError::TaskNotRunning { .. }) => Ok(None),
        result => result,
    }
}

/// Appends one ordered Task stop transition after the caller has proven run quiescence.
///
/// Active steps and child sessions are closed before the Task terminal control, all within one
/// ordered session-writer batch. Passing `None` selects the latest Task for TUI compatibility;
/// application adapters should pass an exact Task id derived from the active cancellation scope.
///
/// # Errors
///
/// Returns a stable error when the exact Task is absent or no longer running, or when the complete
/// append-only transition cannot be persisted.
pub fn append_task_stop_state<H>(
    session: &mut Session,
    handler: &mut H,
    exact_task_id: Option<&TaskId>,
    disposition: TaskStopDisposition,
    reason: &str,
) -> std::result::Result<Option<AppendedTaskStopState>, TaskStopStateError>
where
    H: EventHandler + ?Sized,
{
    let projection = session.task_state_projection();
    let task =
        match exact_task_id {
            Some(task_id) => projection.tasks.get(task_id).ok_or_else(|| {
                TaskStopStateError::TaskUnavailable {
                    task_id: task_id.as_str().to_owned(),
                }
            })?,
            None => {
                let Some(task) = projection.latest_task() else {
                    return Ok(None);
                };
                task
            }
        };
    if !matches!(task.status, TaskRunStatus::Started | TaskRunStatus::Running) {
        if exact_task_id.is_none() {
            return Ok(None);
        }
        return Err(TaskStopStateError::TaskNotRunning {
            task_id: task.task_id.as_str().to_owned(),
        });
    }
    let task_id = task.task_id.clone();
    let parent_session_ref = task.parent_session_ref.clone();
    let objective = task.objective.clone();
    let title = task
        .title
        .clone()
        .unwrap_or_else(|| sigil_kernel::task_semantic_title(&objective));
    let cancellation_closure = if disposition == TaskStopDisposition::Cancelled {
        projection
            .evaluate_root_terminal(&task_id, TaskRunStatus::Cancelled, None)
            .map(|evaluation| evaluation.cancellation_closure)
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let mut active_steps = task
        .active_steps
        .iter()
        .filter_map(|key| task.steps.get(key))
        .filter(|step| !step.status.is_terminal())
        .cloned()
        .map(|step| {
            (
                step.step_id.clone(),
                TaskStepEntry {
                    task_id: task_id.clone(),
                    plan_version: step.plan_version,
                    step_id: step.step_id,
                    role: step.role,
                    status: step.status,
                    title: step.title,
                    summary: step.summary,
                    reason: step.reason,
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    if !cancellation_closure.is_empty()
        && let Some(plan) = task
            .latest_plan_version
            .and_then(|version| task.plans.get(&version))
    {
        for step_id in cancellation_closure {
            let Some(step) = plan.steps.iter().find(|step| step.step_id == step_id) else {
                continue;
            };
            active_steps
                .entry(step_id.clone())
                .or_insert_with(|| TaskStepEntry {
                    task_id: task_id.clone(),
                    plan_version: plan.plan_version,
                    step_id,
                    role: step.role,
                    status: TaskStepStatus::Pending,
                    title: Some(step.title.clone()),
                    summary: None,
                    reason: None,
                });
        }
    }
    let active_participants = match disposition {
        // A root cancellation must leave no active DAG participant behind. Paused and
        // interrupted Task paths retain their existing recovery semantics.
        TaskStopDisposition::Cancelled => task
            .participant_attempts
            .values()
            .filter(|attempt| attempt.status == TaskParticipantAttemptStatus::Started)
            .cloned()
            .collect::<Vec<TaskParticipantAttemptEntry>>(),
        TaskStopDisposition::Paused | TaskStopDisposition::Interrupted => Vec::new(),
    };
    let active_children = task
        .child_sessions
        .values()
        .filter(|child| child.status == TaskChildSessionStatus::Started)
        .cloned()
        .collect::<Vec<_>>();
    let active_direct_attempts = task
        .direct_execution_attempts
        .values()
        .filter(|attempt| attempt.status == TaskParticipantAttemptStatus::Started)
        .cloned()
        .collect::<Vec<_>>();
    let safe_reason = safe_persistence_text(reason);
    let mut controls = Vec::with_capacity(
        active_steps.len()
            + active_participants.len()
            + active_children.len()
            + active_direct_attempts.len()
            + 1,
    );
    for step in active_steps.into_values() {
        controls.push(ControlEntry::TaskStep(TaskStepEntry {
            task_id: task_id.clone(),
            plan_version: step.plan_version,
            step_id: step.step_id,
            role: step.role,
            status: disposition.step_status(),
            title: step.title.as_deref().map(safe_persistence_text),
            summary: None,
            reason: Some(safe_reason.clone()),
        }));
    }
    for mut attempt in active_participants {
        attempt.status = match disposition {
            TaskStopDisposition::Cancelled => TaskParticipantAttemptStatus::Cancelled,
            TaskStopDisposition::Paused | TaskStopDisposition::Interrupted => {
                TaskParticipantAttemptStatus::Interrupted
            }
        };
        attempt.reason = Some(safe_reason.clone());
        controls.push(ControlEntry::TaskParticipantAttempt(attempt));
    }
    for mut child in active_children {
        child.status = disposition.child_status();
        controls.push(ControlEntry::TaskChildSession(child));
    }
    for mut attempt in active_direct_attempts {
        attempt.status = match disposition {
            TaskStopDisposition::Cancelled => TaskParticipantAttemptStatus::Cancelled,
            TaskStopDisposition::Paused | TaskStopDisposition::Interrupted => {
                TaskParticipantAttemptStatus::Interrupted
            }
        };
        attempt.reason = Some(safe_reason.clone());
        controls.push(ControlEntry::TaskDirectExecutionAttemptV1(attempt));
    }
    let status = disposition.task_status();
    controls.push(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref,
        objective: safe_persistence_text(&objective),
        title: Some(title),
        status,
        reason: Some(safe_reason),
    }));
    handler
        .commit_controls(session, controls.clone())
        .map_err(TaskStopStateError::Persistence)?;
    Ok(Some(AppendedTaskStopState {
        task_id,
        status,
        controls,
    }))
}

/// Resolves one exact durable Task continuation without creating execution authority.
///
/// # Errors
///
/// Returns an error when the requested Task is absent or already terminal.
pub fn resolve_task_continuation(
    session: &Session,
    requested_task_id: Option<&str>,
) -> Result<ResolvedTaskContinuation> {
    let projection = session.task_state_projection();
    let task = match requested_task_id {
        Some(value) => {
            let task_id = TaskId::new(value.to_owned())?;
            projection
                .tasks
                .get(&task_id)
                .ok_or_else(|| anyhow!("task {value} is not present in this session"))?
        }
        None => projection
            .latest_unfinished_task()
            .or_else(|| projection.latest_task())
            .ok_or_else(|| anyhow!("no task is available to continue"))?,
    };
    match task.status {
        TaskRunStatus::Completed => {
            return Err(anyhow!(
                "task {} is already completed",
                task.task_id.as_str()
            ));
        }
        TaskRunStatus::Cancelled => {
            return Err(anyhow!("task {} is cancelled", task.task_id.as_str()));
        }
        TaskRunStatus::Started
        | TaskRunStatus::Running
        | TaskRunStatus::Paused
        | TaskRunStatus::Failed
        | TaskRunStatus::Interrupted => {}
    }
    let execution_route = if task.latest_plan_version.is_some() {
        ResolvedTaskExecutionRoute::Planned
    } else if task.direct_execution_admission.is_some() {
        ResolvedTaskExecutionRoute::Direct
    } else {
        ResolvedTaskExecutionRoute::NeedsPlanning
    };
    Ok(ResolvedTaskContinuation {
        task_id: task.task_id.clone(),
        parent_session_ref: task.parent_session_ref.clone(),
        objective: task.objective.clone(),
        execution_route,
    })
}

/// Runs one already-admitted task through the shared planner/executor/subagent/synthesis runtime.
///
/// # Errors
///
/// Returns an error when verification materialization, role construction, or orchestration fails.
pub async fn run_admitted_task_execution<H, A>(
    session: &mut Session,
    request: AdmittedTaskExecution<'_, H>,
    approval_handler: &mut A,
) -> Result<TaskRunStatus>
where
    H: EventHandler + Send,
    A: ApprovalHandler + Send,
{
    let AdmittedTaskExecution {
        task_id,
        parent_session_ref,
        objective,
        root_config,
        options,
        base_registry,
        agent_supervisor,
        role_provider_builder,
        verification_execution_port,
        handler,
        cancellation_handle,
        tool_artifact_read_budget,
    } = request;
    materialize_task_verification_config(
        session,
        handler,
        &root_config,
        &options.workspace_root,
        &task_id,
    )
    .map_err(TaskExecutionPreflightError::VerificationMaterialization)?;
    let TaskRoleRuntime {
        orchestrator,
        planner_options,
        executor_options,
        subagent_read_options,
        subagent_write_options,
    } = build_task_role_runtime_for_route(
        &root_config,
        &options,
        &base_registry,
        agent_supervisor,
        role_provider_builder,
        verification_execution_port,
        resolve_task_continuation(session, Some(task_id.as_str()))?.execution_route,
    )
    .await
    .map_err(TaskExecutionPreflightError::RoleRuntimeConstruction)?;
    let orchestrator = orchestrator.with_cancellation(cancellation_handle);
    let orchestrator = match tool_artifact_read_budget {
        Some(budget) => orchestrator.with_tool_artifact_read_budget(budget),
        None => orchestrator,
    };
    orchestrator
        .run(
            session,
            SequentialTaskRequest {
                task_id,
                parent_session_ref,
                objective,
            },
            planner_options,
            executor_options,
            subagent_read_options,
            subagent_write_options,
            root_config.task.max_plan_steps,
            handler,
            approval_handler,
        )
        .await
        .map(|output| output.status)
}

/// Continues one resolved durable Task through the shared role runtime.
///
/// # Errors
///
/// Returns an error when guidance authority is incomplete, verification materialization or role
/// construction fails, or orchestration cannot continue.
pub async fn continue_task_execution<H, A>(
    session: &mut Session,
    request: ContinuedTaskExecution<'_, H>,
    approval_handler: &mut A,
) -> Result<TaskRunStatus>
where
    H: EventHandler + Send,
    A: ApprovalHandler + Send,
{
    let ContinuedTaskExecution {
        requested_task_id,
        guidance,
        explicit_guidance_run_id,
        guidance_promotion,
        continuation_guidance_receipt,
        root_config,
        options,
        base_registry,
        agent_supervisor,
        role_provider_builder,
        verification_execution_port,
        handler,
        cancellation_handle,
        tool_artifact_read_budget,
    } = request;
    // The caller already resolved the routing model's typed ResumeTask vs ApplyTaskGuidance
    // choice. Never reinterpret the localized prompt text here.
    let guidance = guidance.filter(|value| !value.trim().is_empty());
    let resume_receipt = continuation_guidance_receipt
        .as_ref()
        .is_some_and(|receipt| {
            receipt.control == sigil_kernel::TaskContinuationControlKind::ResumeTask
        });
    let task = resolve_task_continuation(session, requested_task_id.as_ref().map(TaskId::as_str))?;
    let mut explicit_focus_required = validate_continuation_guidance_authority(
        task.execution_route,
        guidance.as_deref(),
        guidance_promotion.as_ref(),
        continuation_guidance_receipt.as_ref(),
    )?;
    if continuation_guidance_receipt.is_some()
        && session.task_state_projection().current_task_id.as_ref() != Some(&task.task_id)
    {
        // A recovered typed selection may belong to an earlier user turn. The new source turn
        // deliberately clears conversation focus, so dispatch must durably reselect the exact
        // already-validated Task before any provider executes.
        explicit_focus_required = true;
    }
    let recoverable_materialization =
        if task.execution_route == ResolvedTaskExecutionRoute::Planned && !resume_receipt {
            // Every continuation entry point resolves unfinished durable guidance before it creates a
            // new authority or performs provider I/O. This pure admission check prevents direct,
            // typed, queued, and slash continuations from forking an already-accepted review after a
            // crash. Sensitive, incomplete, or conflicting materializations fail before focus moves.
            recoverable_task_guidance(session, &task.task_id, guidance.as_deref())?
        } else {
            None
        };
    let recoverable_review = if task.execution_route == ResolvedTaskExecutionRoute::Planned
        && recoverable_materialization.is_none()
        && !resume_receipt
    {
        recoverable_task_guidance_review(session, &task.task_id, guidance.as_deref())?
    } else {
        None
    };
    if let Some(recovered) = recoverable_materialization.as_ref() {
        let incoming_matches = match (
            guidance_promotion.as_ref(),
            continuation_guidance_receipt.as_ref(),
        ) {
            (Some(incoming), None) => {
                recovered.matches_promotion(incoming)
                    && session.entries().iter().any(|entry| {
                        matches!(
                            entry,
                            SessionLogEntry::Control(ControlEntry::TaskGuidancePromoted(recorded))
                                if recorded == incoming
                        )
                    })
            }
            (None, Some(incoming)) => {
                recovered.matches_continuation_selection(incoming)?
                    && session.entries().iter().any(|entry| {
                        matches!(
                            entry,
                            SessionLogEntry::Control(
                                ControlEntry::TaskContinuationSelected(recorded)
                            ) if recorded == incoming
                        )
                    })
            }
            (None, None) => true,
            (Some(_), Some(_)) => false,
        };
        if !incoming_matches {
            return Err(anyhow!(
                "incoming task guidance authority conflicts with unfinished durable materialization"
            ));
        }
    }
    if let Some(recovered) = recoverable_review.as_ref() {
        let incoming_matches = match (
            &recovered.authority,
            guidance_promotion.as_ref(),
            continuation_guidance_receipt.as_ref(),
        ) {
            (RecoverableTaskGuidanceReviewAuthority::Promoted(recorded), Some(incoming), None) => {
                recorded.as_ref() == incoming
            }
            (
                RecoverableTaskGuidanceReviewAuthority::ContinuationSelected(recorded),
                None,
                Some(incoming),
            ) => recorded.as_ref() == incoming,
            (_, None, None) => true,
            _ => false,
        };
        if !incoming_matches {
            return Err(anyhow!(
                "incoming task guidance authority conflicts with an unfinished durable review"
            ));
        }
    }
    if let Some(recovered) = recoverable_review.as_ref() {
        let retry_controls = recoverable_task_guidance_review_retry_controls(session, recovered)?;
        if !retry_controls.is_empty() {
            handler.commit_controls(session, retry_controls)?;
        }
    }
    if explicit_focus_required {
        append_explicit_task_run_target(
            session,
            handler,
            &task.task_id,
            cancellation_handle.scope_id(),
        )?;
    }
    materialize_task_verification_config(
        session,
        handler,
        &root_config,
        &options.workspace_root,
        &task.task_id,
    )
    .map_err(TaskExecutionPreflightError::VerificationMaterialization)?;
    let role_demand = if task.execution_route == ResolvedTaskExecutionRoute::Planned {
        task_role_demand_for_continuation(
            session,
            &task.task_id,
            guidance.is_some()
                || guidance_promotion.is_some()
                || continuation_guidance_receipt.is_some(),
        )?
    } else {
        TaskRoleDemand::all()
    };
    let TaskRoleRuntime {
        orchestrator,
        planner_options,
        executor_options,
        subagent_read_options,
        subagent_write_options,
    } = build_task_role_runtime_for_route_with_demand(
        &root_config,
        &options,
        &base_registry,
        agent_supervisor,
        role_provider_builder,
        verification_execution_port,
        task.execution_route,
        role_demand,
    )
    .await
    .map_err(TaskExecutionPreflightError::RoleRuntimeConstruction)?;
    let task_request = SequentialTaskRequest {
        task_id: task.task_id,
        parent_session_ref: task.parent_session_ref,
        objective: task.objective,
    };
    let orchestrator = orchestrator.with_cancellation(cancellation_handle);
    let orchestrator = match tool_artifact_read_budget {
        Some(budget) => orchestrator.with_tool_artifact_read_budget(budget),
        None => orchestrator,
    };
    let guidance_source = if task.execution_route == ResolvedTaskExecutionRoute::Planned {
        None
    } else if let Some(receipt) = continuation_guidance_receipt.as_ref() {
        Some(receipt.source_turn.clone())
    } else if let Some(run_id) = explicit_guidance_run_id.as_ref() {
        let source_id = sigil_kernel::stable_event_hash(serde_json::to_vec(&(
            session.session_scope_id(),
            run_id,
        ))?);
        Some(sigil_kernel::ConversationTurnRef::new(
            session.session_scope_id(),
            format!("task-guidance-{source_id}"),
            run_id.clone(),
        )?)
    } else {
        None
    };
    let direct_guidance = if task.execution_route != ResolvedTaskExecutionRoute::Planned {
        guidance
            .as_deref()
            .map(|text| {
                guidance_source
                    .as_ref()
                    .map(|source| (text, source))
                    .context("unplanned Task guidance requires its real source turn")
            })
            .transpose()?
    } else {
        None
    };
    let output = continued_task_dispatch(
        &orchestrator,
        session,
        ContinuedTaskDispatch {
            execution_route: task.execution_route,
            task_request,
            planner_options,
            executor_options,
            subagent_read_options,
            subagent_write_options,
            max_plan_steps: root_config.task.max_plan_steps,
            guidance: guidance.clone(),
            guidance_promotion,
            continuation_guidance_receipt,
            recoverable_materialization,
            recoverable_review,
        },
        direct_guidance,
        handler,
        approval_handler,
    )?
    .await?;
    Ok(output.status)
}

struct ContinuedTaskDispatch {
    execution_route: ResolvedTaskExecutionRoute,
    task_request: SequentialTaskRequest,
    planner_options: AgentRunOptions,
    executor_options: AgentRunOptions,
    subagent_read_options: AgentRunOptions,
    subagent_write_options: AgentRunOptions,
    max_plan_steps: usize,
    guidance: Option<String>,
    guidance_promotion: Option<TaskGuidancePromotedEntry>,
    continuation_guidance_receipt: Option<TaskContinuationSelectedEntry>,
    recoverable_materialization: Option<sigil_kernel::RecoverableTaskGuidance>,
    recoverable_review: Option<sigil_kernel::RecoverableTaskGuidanceReview>,
}

// Construct the selected large orchestration future outside every polling frame. In debug
// builds, constructing these futures inside `continue_task_execution` reserves hundreds of KiB
// of temporary stack slots that remain live while all nested agent futures are polled.
// This synchronous frame exits before polling begins; cancellation remains owned by the caller.
#[inline(never)]
fn continued_task_dispatch<'a, H, A>(
    orchestrator: &'a sigil_kernel::SequentialTaskOrchestrator<
        super::task_runner::AgentSupervisorTaskChildRunner,
    >,
    session: &'a mut Session,
    dispatch: ContinuedTaskDispatch,
    direct_guidance: Option<(&'a str, &'a sigil_kernel::ConversationTurnRef)>,
    handler: &'a mut H,
    approval_handler: &'a mut A,
) -> Result<futures::future::BoxFuture<'a, Result<sigil_kernel::SequentialTaskRunOutput>>>
where
    H: EventHandler + Send,
    A: ApprovalHandler + Send,
{
    let ContinuedTaskDispatch {
        execution_route,
        task_request,
        planner_options,
        executor_options,
        subagent_read_options,
        subagent_write_options,
        max_plan_steps,
        guidance,
        guidance_promotion,
        continuation_guidance_receipt,
        recoverable_materialization,
        recoverable_review,
    } = dispatch;
    let future: futures::future::BoxFuture<'a, Result<sigil_kernel::SequentialTaskRunOutput>> =
        if execution_route == ResolvedTaskExecutionRoute::NeedsPlanning {
            if let Some(receipt) = continuation_guidance_receipt.as_ref() {
                receipt.validate_for_session(session.session_scope_id())?;
                if receipt.task_id != task_request.task_id || !session.entries().iter().any(|entry| {
                    matches!(entry, SessionLogEntry::Control(ControlEntry::TaskContinuationSelected(recorded)) if recorded == receipt)
                }) {
                    return Err(anyhow!("initial task guidance requires its exact durable selection"));
                }
            }
            Box::pin(orchestrator.run_with_initial_guidance(
                session,
                task_request,
                planner_options,
                executor_options,
                subagent_read_options,
                subagent_write_options,
                max_plan_steps,
                direct_guidance,
                handler,
                approval_handler,
            ))
        } else if execution_route == ResolvedTaskExecutionRoute::Direct {
            Box::pin(orchestrator.continue_direct_run(
                session,
                task_request,
                executor_options,
                direct_guidance,
                handler,
                approval_handler,
            ))
        } else if let Some(recovered) = recoverable_materialization {
            Box::pin(orchestrator.continue_run(
                session,
                task_request,
                executor_options,
                subagent_read_options,
                subagent_write_options,
                Some(recovered.guidance),
                handler,
                approval_handler,
            ))
        } else if let Some(recovered) = recoverable_review {
            match recovered.authority {
                RecoverableTaskGuidanceReviewAuthority::Promoted(promotion) => {
                    Box::pin(orchestrator.continue_run_with_guidance_review(
                        session,
                        task_request,
                        planner_options,
                        executor_options,
                        subagent_read_options,
                        subagent_write_options,
                        max_plan_steps,
                        recovered.guidance,
                        *promotion,
                        handler,
                        approval_handler,
                    ))
                }
                RecoverableTaskGuidanceReviewAuthority::ContinuationSelected(selection) => {
                    Box::pin(orchestrator.continue_run_with_conversation_guidance_review(
                        session,
                        task_request,
                        planner_options,
                        executor_options,
                        subagent_read_options,
                        subagent_write_options,
                        max_plan_steps,
                        recovered.guidance,
                        *selection,
                        handler,
                        approval_handler,
                    ))
                }
            }
        } else {
            match (guidance, guidance_promotion, continuation_guidance_receipt) {
                (Some(guidance), Some(promotion), None) => {
                    Box::pin(orchestrator.continue_run_with_guidance_review(
                        session,
                        task_request,
                        planner_options,
                        executor_options,
                        subagent_read_options,
                        subagent_write_options,
                        max_plan_steps,
                        guidance,
                        promotion,
                        handler,
                        approval_handler,
                    ))
                }
                (Some(guidance), None, Some(selection)) => {
                    Box::pin(orchestrator.continue_run_with_conversation_guidance_review(
                        session,
                        task_request,
                        planner_options,
                        executor_options,
                        subagent_read_options,
                        subagent_write_options,
                        max_plan_steps,
                        guidance,
                        selection,
                        handler,
                        approval_handler,
                    ))
                }
                (guidance, None, None) => Box::pin(orchestrator.continue_run(
                    session,
                    task_request,
                    executor_options,
                    subagent_read_options,
                    subagent_write_options,
                    guidance,
                    handler,
                    approval_handler,
                )),
                (None, Some(_), None) => {
                    return Err(anyhow!(
                        "task guidance promotion is missing its guidance material"
                    ));
                }
                (None, None, Some(receipt)) if receipt.guidance.trim().is_empty() => {
                    Box::pin(orchestrator.continue_run(
                        session,
                        task_request,
                        executor_options,
                        subagent_read_options,
                        subagent_write_options,
                        None,
                        handler,
                        approval_handler,
                    ))
                }
                (None, None, Some(_)) => {
                    // A recovered selection may carry only the safe durable projection. Resume from
                    // that projection instead of requiring the process-local exact source prompt.
                    Box::pin(orchestrator.continue_run(
                        session,
                        task_request,
                        executor_options,
                        subagent_read_options,
                        subagent_write_options,
                        None,
                        handler,
                        approval_handler,
                    ))
                }
                (_, Some(_), Some(_)) => {
                    return Err(anyhow!(
                        "task continuation supplied conflicting guidance authorities"
                    ));
                }
            }
        };
    Ok(future)
}

fn validate_continuation_guidance_authority(
    execution_route: ResolvedTaskExecutionRoute,
    guidance: Option<&str>,
    guidance_promotion: Option<&TaskGuidancePromotedEntry>,
    continuation_guidance_receipt: Option<&TaskContinuationSelectedEntry>,
) -> Result<bool> {
    if execution_route == ResolvedTaskExecutionRoute::NeedsPlanning {
        if guidance_promotion.is_some()
            || continuation_guidance_receipt.is_some_and(|receipt| {
                receipt.plan_version.is_some() || receipt.plan_status.is_some()
            })
        {
            return Err(anyhow!(
                "initial task guidance cannot use an accepted-plan authority"
            ));
        }
        return Ok(continuation_guidance_receipt.is_none());
    }
    match (guidance, guidance_promotion, continuation_guidance_receipt) {
        (Some(_), Some(_), None) | (Some(_), None, Some(_)) => Ok(false),
        (_, None, None) => Ok(true),
        (None, Some(_), None) => Err(anyhow!(
            "task guidance promotion is missing its guidance material"
        )),
        (None, None, Some(_)) => Ok(false),
        (_, Some(_), Some(_)) => Err(anyhow!(
            "task continuation supplied conflicting guidance authorities"
        )),
    }
}

fn append_explicit_task_run_target<H>(
    session: &mut Session,
    handler: &mut H,
    task_id: &TaskId,
    run_scope_id: &str,
) -> Result<()>
where
    H: EventHandler,
{
    let latest_bound_scope = session
        .entries()
        .iter()
        .rev()
        .find_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::TaskRunCancellationScopeBound(binding))
                if &binding.task_id == task_id =>
            {
                Some(binding.run_scope_id.as_str())
            }
            _ => None,
        });
    if latest_bound_scope != Some(run_scope_id) {
        return Err(anyhow!(
            "explicit task continuation cancellation scope is not durably bound to the selected task"
        ));
    }
    let projection = session.task_state_projection();
    let task = projection
        .tasks
        .get(task_id)
        .ok_or_else(|| anyhow!("explicit task continuation target is no longer present"))?;
    let plan_status = task
        .latest_plan_version
        .and_then(|version| task.plans.get(&version).map(|plan| plan.status));
    let selected = TaskRunTargetSelectedEntry::new(
        task_id.clone(),
        run_scope_id,
        task.status,
        task.latest_plan_version,
        plan_status,
    );
    if let Some(existing) = session.entries().iter().find_map(|entry| match entry {
        SessionLogEntry::Control(ControlEntry::TaskRunTargetSelected(existing))
            if existing.selection_id == selected.selection_id =>
        {
            Some(existing)
        }
        _ => None,
    }) {
        if existing != &selected {
            return Err(anyhow!(
                "explicit task continuation selection has conflicting durable facts"
            ));
        }
        return Ok(());
    }
    handler.commit_controls(session, vec![ControlEntry::TaskRunTargetSelected(selected)])?;
    Ok(())
}

/// Runs an admitted handoff task and atomically claims the shared root terminal.
///
/// # Errors
///
/// Returns an error when orchestration fails, cancellation won the terminal race, or the failed
/// task state cannot be persisted.
pub async fn run_admitted_task_to_root_terminal<H, A>(
    session: &mut Session,
    request: AdmittedTaskExecution<'_, H>,
    approval_handler: &mut A,
) -> Result<TaskRunStatus>
where
    H: EventHandler + Send,
    A: ApprovalHandler + Send,
{
    let terminal_cancellation = request.cancellation_handle.clone();
    let terminal_task_id = request.task_id.clone();
    let terminal_parent_session_ref = request.parent_session_ref.clone();
    let terminal_objective = request.objective.clone();
    let result = run_admitted_task_execution(session, request, approval_handler).await;
    finalize_task_root(
        session,
        &terminal_task_id,
        &terminal_parent_session_ref,
        &terminal_objective,
        &terminal_cancellation,
        result,
    )
}

/// Claims natural root completion and persists a failed task terminal when orchestration escaped
/// before writing one.
pub fn finalize_task_root(
    session: &mut Session,
    task_id: &TaskId,
    parent_session_ref: &SessionRef,
    objective: &str,
    terminal_cancellation: &RunCancellationHandle,
    result: Result<TaskRunStatus>,
) -> Result<TaskRunStatus> {
    if !terminal_cancellation.is_naturally_finalized()
        && !terminal_cancellation.try_finalize_naturally()
    {
        return Err(anyhow!("run cancellation won the task terminal-state race"));
    }
    let result =
        normalize_completed_task_terminal(session, task_id, parent_session_ref, objective, result);
    let Err(error) = &result else {
        return result;
    };
    // A provider-turn terminal has already been classified from durable physical-attempt and
    // effect evidence.  It is deliberately an error to stop the current async owner, but it is
    // not a Task failure unless the policy explicitly called it irrecoverable.  Keep this
    // guard at the root terminal so a future orchestration path cannot accidentally collapse a
    // recoverable blocker back into `TaskRunStatus::Failed`.
    if let Some(recovery) = error.downcast_ref::<sigil_kernel::ProviderTurnRecoveryTerminalError>()
    {
        let status = match recovery.disposition {
            sigil_kernel::ProviderTurnRecoveryTerminalDispositionV1::Blocked
            | sigil_kernel::ProviderTurnRecoveryTerminalDispositionV1::Paused => {
                TaskRunStatus::Paused
            }
            sigil_kernel::ProviderTurnRecoveryTerminalDispositionV1::Cancelled => {
                TaskRunStatus::Cancelled
            }
            sigil_kernel::ProviderTurnRecoveryTerminalDispositionV1::Irrecoverable => {
                TaskRunStatus::Failed
            }
        };
        let current_status = session
            .task_state_projection()
            .tasks
            .get(task_id)
            .map(|task| task.status);
        if matches!(
            current_status,
            Some(TaskRunStatus::Started | TaskRunStatus::Running)
        ) {
            session.append_control(ControlEntry::TaskRun(TaskRunEntry {
                task_id: task_id.clone(),
                parent_session_ref: parent_session_ref.clone(),
                objective: safe_persistence_text(objective),
                title: None,
                status,
                reason: Some(safe_persistence_text(&format!(
                    "provider turn recovery {:?}: {}",
                    recovery.disposition, recovery.reason_code
                ))),
            }))?;
        }
        return Ok(status);
    }
    // Verification and role-runtime preparation are zero-dispatch boundaries: no participant
    // provider request or tool effect has started. Keep the admitted Task resumable so repairing
    // configuration or credentials and pressing Continue can retry the same durable authority.
    if let Some(preflight) = error.downcast_ref::<TaskExecutionPreflightError>() {
        let projection = session.task_state_projection();
        let needs_pause = projection.tasks.get(task_id).is_some_and(|task| {
            matches!(task.status, TaskRunStatus::Started | TaskRunStatus::Running)
                || (task.status == TaskRunStatus::Paused
                    && task.reason.as_deref() != Some(preflight.reason_code()))
        });
        if needs_pause {
            session.append_control(ControlEntry::TaskRun(TaskRunEntry {
                task_id: task_id.clone(),
                parent_session_ref: parent_session_ref.clone(),
                objective: safe_persistence_text(objective),
                title: None,
                status: TaskRunStatus::Paused,
                reason: Some(preflight.reason_code().to_owned()),
            }))?;
        }
        return Ok(TaskRunStatus::Paused);
    }
    let status = session
        .task_state_projection()
        .tasks
        .get(task_id)
        .map(|task| task.status);
    if matches!(
        status,
        Some(TaskRunStatus::Started | TaskRunStatus::Running)
    ) {
        session.append_control(ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: parent_session_ref.clone(),
            objective: safe_persistence_text(objective),
            title: None,
            status: TaskRunStatus::Failed,
            reason: Some(safe_persistence_text(&format!(
                "task orchestration failed before a terminal state: {error:#}"
            ))),
        }))?;
    }
    result
}

/// Downgrades an unsupported root completion claim to the resumable terminal selected by the
/// shared durable evaluator.
fn normalize_completed_task_terminal(
    session: &mut Session,
    task_id: &TaskId,
    parent_session_ref: &SessionRef,
    objective: &str,
    result: Result<TaskRunStatus>,
) -> Result<TaskRunStatus> {
    let status = result?;
    if status != TaskRunStatus::Completed {
        return Ok(status);
    }
    let Some(evaluation) = session.task_state_projection().evaluate_root_terminal(
        task_id,
        TaskRunStatus::Completed,
        None,
    ) else {
        // Keep legacy callers that do not yet have a durable direct/DAG authority unchanged.
        return Ok(status);
    };
    if evaluation.allows_completed() {
        return Ok(status);
    }
    let current_status = session
        .task_state_projection()
        .tasks
        .get(task_id)
        .map(|task| task.status);
    if matches!(
        current_status,
        Some(TaskRunStatus::Started | TaskRunStatus::Running)
    ) {
        let reason_code = evaluation
            .primary_completion_blocker()
            .map_or("unfinished_task_root", |blocker| blocker.reason_code());
        session.append_control(ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: parent_session_ref.clone(),
            objective: safe_persistence_text(objective),
            title: None,
            status: TaskRunStatus::Paused,
            reason: Some(format!("task completion blocked: {reason_code}")),
        }))?;
    }
    Ok(TaskRunStatus::Paused)
}

/// Finalizes one continuation without turning admission/re-entry failures into Task failure.
///
/// A continuation can fail before it starts a new role attempt (for example because sensitive
/// guidance must be re-entered after restart). Such an error belongs to the conversation attempt,
/// not to the durable Task. Once a new participant attempt has started, the ordinary root
/// finalizer retains its fail-safe terminal behavior.
pub fn finalize_task_continuation_root(
    session: &mut Session,
    task_id: &TaskId,
    parent_session_ref: &SessionRef,
    objective: &str,
    terminal_cancellation: &RunCancellationHandle,
    continuation_entry_frontier: usize,
    result: Result<TaskRunStatus>,
) -> Result<TaskRunStatus> {
    let participant_started = session
        .entries()
        .iter()
        .skip(continuation_entry_frontier)
        .any(|entry| {
            matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::TaskParticipantAttempt(attempt))
                    if &attempt.task_id == task_id
                        && attempt.status == TaskParticipantAttemptStatus::Started
            )
        });
    if result.is_ok() || participant_started {
        return finalize_task_root(
            session,
            task_id,
            parent_session_ref,
            objective,
            terminal_cancellation,
            result,
        );
    }
    if !terminal_cancellation.is_naturally_finalized()
        && !terminal_cancellation.try_finalize_naturally()
    {
        return Err(anyhow!("run cancellation won the task terminal-state race"));
    }
    result
}

/// Materializes trusted verification configuration into one task scope and publishes the same
/// controls through the active product event handler.
pub fn materialize_task_verification_config<H>(
    session: &mut Session,
    handler: &mut H,
    root_config: &RootConfig,
    workspace_root: &Path,
    task_id: &TaskId,
) -> Result<()>
where
    H: EventHandler,
{
    let scope = EvidenceScope::Task(task_id.as_str().to_owned());
    let source_event_id = format!("config:verification:{}", task_id.as_str());
    let projection = session.verification_state_projection();
    let workspace_id = stable_workspace_id(workspace_root)?;
    let workspace_scope = EvidenceScope::Workspace(workspace_id);
    let mut entries = check_specs_from_user_config(
        workspace_root,
        &root_config.verification,
        scope.clone(),
        DEFAULT_TASK_VERIFICATION_SCOPE_HASH,
        source_event_id.clone(),
    )?;
    for promoted in projection.check_specs_for_scopes(&[workspace_scope]) {
        if promoted.trusted_check.source == CheckDiscoverySource::UserExplicitConfig
            || promoted.trusted_check.check_spec.verification_scope_hash
                != DEFAULT_TASK_VERIFICATION_SCOPE_HASH
            || entries.iter().any(|entry| {
                entry.trusted_check.check_spec.check_spec_id
                    == promoted.trusted_check.check_spec.check_spec_id
            })
        {
            continue;
        }
        entries.push(CheckSpecRecordedEntry::new(
            scope.clone(),
            promoted.trusted_check.clone(),
            promoted.source_event_id.clone(),
        ));
    }
    if entries.is_empty() {
        return Ok(());
    }

    let projection = session.verification_state_projection();
    let mut controls = Vec::new();
    for entry in &entries {
        let check_id = entry.trusted_check.check_spec.check_spec_id.as_str();
        let needs_append = projection
            .check_spec(&scope, check_id)
            .is_none_or(|current| {
                current.trusted_check.check_spec.check_spec_hash
                    != entry.trusted_check.check_spec.check_spec_hash
            });
        if needs_append {
            controls.push(ControlEntry::CheckSpecRecorded(entry.clone()));
        }
    }

    let required_checks = entries
        .iter()
        .map(|entry| entry.trusted_check.check_spec.clone())
        .collect::<Vec<_>>();
    let policy = VerificationPolicy {
        required_checks,
        completion_criteria: CompletionCriteria::AllRequiredChecks,
        verification_scope: root_config
            .verification
            .scope_for_hash(DEFAULT_TASK_VERIFICATION_SCOPE_HASH),
        sandbox_profile: SandboxProfileRequirement::None,
        workspace_trust_requirement: check_spec_entries_workspace_trust_requirement(&entries),
        allow_unverified_completion: false,
        timeout_ms: None,
        auto_run: root_config.verification.auto_run,
    };
    let policy_entry = VerificationPolicyChangedEntry::new(scope.clone(), policy, source_event_id)?;
    let needs_policy_append = projection
        .latest_policy(&scope)
        .is_none_or(|current| current.policy_hash != policy_entry.policy_hash);
    if needs_policy_append {
        controls.push(ControlEntry::VerificationPolicyChanged(policy_entry));
    }

    if !controls.is_empty() {
        handler.commit_controls(session, controls)?;
    }
    Ok(())
}

fn check_spec_entries_workspace_trust_requirement(
    entries: &[CheckSpecRecordedEntry],
) -> WorkspaceTrustRequirement {
    if entries.iter().any(|entry| {
        matches!(
            entry.trusted_check.promoted_by,
            CheckPromotion::WorkspaceTrusted { .. }
        )
    }) {
        return WorkspaceTrustRequirement::Trusted;
    }
    if entries.iter().any(|entry| {
        matches!(
            entry.trusted_check.promoted_by,
            CheckPromotion::UserApproved { .. } | CheckPromotion::Sandboxed { .. }
        )
    }) {
        return WorkspaceTrustRequirement::ApprovalOrSandbox;
    }
    WorkspaceTrustRequirement::None
}

#[cfg(test)]
#[path = "tests/task_execution_tests.rs"]
mod tests;
