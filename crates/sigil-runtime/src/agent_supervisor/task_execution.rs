use std::{path::Path, sync::Arc};

use anyhow::{Context, Result, anyhow};
use sigil_kernel::verification::VerificationExecutionPortV1;
use sigil_kernel::{
    AgentRunOptions, ApprovalHandler, CheckDiscoverySource, CheckSpecRecordedEntry, ControlEntry,
    ConversationTurnRef, DEFAULT_TASK_VERIFICATION_SCOPE_HASH, DirectTaskRequest, EventHandler,
    EvidenceScope, RootConfig, RunCancellationHandle, RunCancellationOwner,
    RunCancellationRecorder, RunCancellationTarget, RunTaskGuard, Session, SessionLogEntry,
    SessionRef, TaskContinuationSelectedEntry, TaskExecutionAttemptStatus, TaskExecutionBindingV1,
    TaskGuidancePromotedEntry, TaskId, TaskPauseRequest, TaskRunCancellationScopeBoundEntry,
    TaskRunEntry, TaskRunStatus, ToolRegistry, VerificationPolicy, VerificationPolicyChangedEntry,
    check_specs_from_user_config, safe_persistence_text, stable_workspace_id,
};
use thiserror::Error;

use super::{
    AgentSupervisor,
    task_role_runtime::{TaskRoleProviderBuilder, TaskRoleRuntime, build_task_role_runtime},
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
        &task_id,
        TaskStopDisposition::Interrupted,
        reason,
    ) {
        Err(TaskStopStateError::TaskNotRunning { .. }) => Ok(None),
        result => result,
    }
}

/// Appends one ordered Task stop transition after the caller has proven run quiescence.
///
/// Active direct execution attempts are closed before the Task terminal control, all within one
/// ordered session-writer batch. The caller must pass the exact Task id owned by its scope.
///
/// # Errors
///
/// Returns a stable error when the exact Task is absent or no longer running, or when the complete
/// append-only transition cannot be persisted.
pub fn append_task_stop_state<H>(
    session: &mut Session,
    handler: &mut H,
    task_id: &TaskId,
    disposition: TaskStopDisposition,
    reason: &str,
) -> std::result::Result<Option<AppendedTaskStopState>, TaskStopStateError>
where
    H: EventHandler + ?Sized,
{
    let projection = session.task_state_projection();
    let task =
        projection
            .tasks
            .get(task_id)
            .ok_or_else(|| TaskStopStateError::TaskUnavailable {
                task_id: task_id.as_str().to_owned(),
            })?;
    if !matches!(task.status, TaskRunStatus::Started | TaskRunStatus::Running) {
        return Err(TaskStopStateError::TaskNotRunning {
            task_id: task.task_id.as_str().to_owned(),
        });
    }

    let task_id = task.task_id.clone();
    let parent_session_ref = task.parent_session_ref.clone();
    let objective = task.objective.clone();
    let safe_reason = safe_persistence_text(reason);
    let mut controls = Vec::new();
    for mut attempt in task
        .direct_execution_attempts
        .values()
        .filter(|attempt| attempt.status == TaskExecutionAttemptStatus::Started)
        .cloned()
    {
        attempt.status = match disposition {
            TaskStopDisposition::Cancelled => TaskExecutionAttemptStatus::Cancelled,
            TaskStopDisposition::Paused | TaskStopDisposition::Interrupted => {
                TaskExecutionAttemptStatus::Interrupted
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
        title: task
            .title
            .clone()
            .or_else(|| Some(sigil_kernel::task_semantic_title(&objective))),
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
    let value = requested_task_id.ok_or_else(|| anyhow!("an exact Task id is required"))?;
    let task_id = TaskId::new(value.to_owned())?;
    let task = projection
        .tasks
        .get(&task_id)
        .ok_or_else(|| anyhow!("task {value} is not present in this session"))?;
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
    if task.latest_plan_version.is_some() || task.direct_execution_admission.is_none() {
        anyhow::bail!(
            "task {} is not a current direct Task; direct execution authority is required",
            task.task_id.as_str()
        );
    }
    Ok(ResolvedTaskContinuation {
        task_id: task.task_id.clone(),
        parent_session_ref: task.parent_session_ref.clone(),
        objective: task.objective.clone(),
    })
}

fn resolve_task_guidance_promotion_source(
    session: &Session,
    task_id: &TaskId,
    promotion: &TaskGuidancePromotedEntry,
    exact_guidance: Option<&str>,
) -> Result<ConversationTurnRef> {
    promotion.validate_for_session(session.session_scope_id())?;
    if promotion.task_id != *task_id
        || !session.entries().iter().any(|entry| {
            matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::TaskGuidancePromoted(recorded))
                    if recorded == promotion
            )
        })
    {
        anyhow::bail!("direct Task guidance requires its exact durable queue promotion");
    }
    let exact_guidance =
        exact_guidance.context("promoted Task guidance is missing its process-local prompt")?;
    let projected = sigil_kernel::project_conversation_prompt_for_persistence(exact_guidance);
    if projected.prompt_hash != promotion.prompt_hash
        || projected.safe_prompt != promotion.guidance
        || projected.exact_prompt_required != promotion.exact_prompt_required
    {
        anyhow::bail!("promoted Task guidance does not match its exact durable binding");
    }
    Ok(promotion.source_turn.clone())
}

/// Runs one already-admitted task through the model-owned direct runtime.
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
        direct_task_runtime,
        executor_options,
    } = build_task_role_runtime(
        &root_config,
        &options,
        &base_registry,
        agent_supervisor,
        role_provider_builder,
        verification_execution_port,
    )
    .await
    .map_err(TaskExecutionPreflightError::RoleRuntimeConstruction)?;
    let direct_task_runtime = direct_task_runtime.with_cancellation(cancellation_handle);
    let direct_task_runtime = match tool_artifact_read_budget {
        Some(budget) => direct_task_runtime.with_tool_artifact_read_budget(budget),
        None => direct_task_runtime,
    };
    direct_task_runtime
        .run(
            session,
            DirectTaskRequest {
                task_id,
                parent_session_ref,
                objective,
            },
            executor_options.clone(),
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

    let task = resolve_task_continuation(session, requested_task_id.as_ref().map(TaskId::as_str))?;
    let guidance = guidance.filter(|value| !value.trim().is_empty());
    let guidance_source = if let Some(promotion) = guidance_promotion.as_ref() {
        if continuation_guidance_receipt.is_some() {
            anyhow::bail!(
                "direct Task guidance cannot combine queue promotion and continuation receipt"
            );
        }
        Some(resolve_task_guidance_promotion_source(
            session,
            &task.task_id,
            promotion,
            guidance.as_deref(),
        )?)
    } else if let Some(receipt) = continuation_guidance_receipt.as_ref() {
        receipt.validate_for_session(session.session_scope_id())?;
        if receipt.task_id != task.task_id
            || !session.entries().iter().any(|entry| {
                matches!(
                    entry,
                    SessionLogEntry::Control(ControlEntry::TaskContinuationSelected(recorded))
                        if recorded == receipt
                )
            })
        {
            anyhow::bail!("direct Task guidance requires its exact durable selection");
        }
        if receipt.control == sigil_kernel::TaskContinuationControlKind::ResumeTask {
            if guidance.is_some() {
                anyhow::bail!("resume Task continuation cannot carry guidance text");
            }
            None
        } else {
            Some(receipt.source_turn.clone())
        }
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
    let direct_guidance = guidance
        .as_deref()
        .map(|text| {
            guidance_source
                .as_ref()
                .map(|source| (text, source))
                .context("direct Task guidance requires its real source turn")
        })
        .transpose()?;

    materialize_task_verification_config(
        session,
        handler,
        &root_config,
        &options.workspace_root,
        &task.task_id,
    )
    .map_err(TaskExecutionPreflightError::VerificationMaterialization)?;

    let TaskRoleRuntime {
        direct_task_runtime,
        executor_options,
        ..
    } = build_task_role_runtime(
        &root_config,
        &options,
        &base_registry,
        agent_supervisor,
        role_provider_builder,
        verification_execution_port,
    )
    .await
    .map_err(TaskExecutionPreflightError::RoleRuntimeConstruction)?;
    let direct_task_runtime = direct_task_runtime.with_cancellation(cancellation_handle);
    let direct_task_runtime = match tool_artifact_read_budget {
        Some(budget) => direct_task_runtime.with_tool_artifact_read_budget(budget),
        None => direct_task_runtime,
    };
    let output = direct_task_runtime
        .continue_direct_run(
            session,
            DirectTaskRequest {
                task_id: task.task_id,
                parent_session_ref: task.parent_session_ref,
                objective: task.objective,
            },
            executor_options,
            direct_guidance,
            handler,
            approval_handler,
        )
        .await?;
    Ok(output.status)
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
    let disposition = sigil_kernel::agent::execution::execution_failure_disposition(error);
    let terminal_status = match disposition {
        sigil_kernel::agent::execution::ExecutionDisposition::Blocked => TaskRunStatus::Paused,
        sigil_kernel::agent::execution::ExecutionDisposition::Cancelled => TaskRunStatus::Cancelled,
        sigil_kernel::agent::execution::ExecutionDisposition::Interrupted => {
            TaskRunStatus::Interrupted
        }
        _ => TaskRunStatus::Failed,
    };
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
            status: terminal_status,
            reason: Some(safe_persistence_text(&format!(
                "task orchestration failed before a terminal state: {error:#}"
            ))),
        }))?;
    }
    if terminal_status == TaskRunStatus::Failed {
        result
    } else {
        Ok(terminal_status)
    }
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
    let evaluation = session
        .task_state_projection()
        .evaluate_root_terminal(task_id, TaskRunStatus::Completed, None)
        .ok_or_else(|| anyhow!("Task has no current direct execution authority"))?;
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
/// A continuation can fail before it starts a new direct-execution attempt (for example because
/// sensitive guidance must be re-entered after restart). Such an error belongs to the conversation
/// attempt, not to the durable Task. Once a new direct-execution attempt has started, the ordinary
/// root finalizer retains its fail-safe terminal behavior.
pub fn finalize_task_continuation_root(
    session: &mut Session,
    task_id: &TaskId,
    parent_session_ref: &SessionRef,
    objective: &str,
    terminal_cancellation: &RunCancellationHandle,
    continuation_entry_frontier: usize,
    result: Result<TaskRunStatus>,
) -> Result<TaskRunStatus> {
    let direct_execution_started = session
        .entries()
        .iter()
        .skip(continuation_entry_frontier)
        .any(|entry| {
            matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::TaskDirectExecutionAttemptV1(attempt))
                    if &attempt.task_id == task_id
                        && attempt.status == TaskExecutionAttemptStatus::Started
            )
        });
    if result.is_ok() || direct_execution_started {
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

    // Configuration supplies runnable candidates; only an explicit policy or an accepted
    // contract makes a check mandatory. Existing policy authority survives catalog refresh.
    if projection.latest_policy(&scope).is_none() {
        let mut policy =
            VerificationPolicy::no_checks_required(DEFAULT_TASK_VERIFICATION_SCOPE_HASH);
        policy.verification_scope = root_config
            .verification
            .scope_for_hash(DEFAULT_TASK_VERIFICATION_SCOPE_HASH);
        policy.auto_run = root_config.verification.auto_run;
        controls.push(ControlEntry::VerificationPolicyChanged(
            VerificationPolicyChangedEntry::new(scope, policy, source_event_id)?,
        ));
    }

    if !controls.is_empty() {
        handler.commit_controls(session, controls)?;
    }
    Ok(())
}

#[cfg(test)]
mod task_execution_tests {
    use super::*;

    fn promotion(
        session: &Session,
        task_id: TaskId,
        exact_guidance: &str,
    ) -> Result<TaskGuidancePromotedEntry> {
        let projected = sigil_kernel::project_conversation_prompt_for_persistence(exact_guidance);
        Ok(TaskGuidancePromotedEntry {
            queue_id: sigil_kernel::ConversationInputQueueId::new("queued_guidance")?,
            expected_queue_revision: sigil_kernel::ConversationQueueRevision {
                stream_sequence: 1,
                event_id: "queue_event_1".to_owned(),
            },
            task_id,
            source_turn: ConversationTurnRef::new(
                session.session_scope_id(),
                "queued_guidance_message",
                "queued_guidance_run",
            )?,
            prompt_hash: projected.prompt_hash,
            exact_prompt_required: projected.exact_prompt_required,
            guidance: projected.safe_prompt,
            dispatch_run_id: "queued_guidance_run".to_owned(),
            promoted_at_ms: 1,
        })
    }

    #[test]
    fn task_guidance_promotion_requires_exact_task_durable_event_and_process_prompt() -> Result<()>
    {
        let mut session = Session::new("test", "model");
        let task_id = TaskId::new("direct_task")?;
        let exact_guidance = "continue with the durable queue binding";
        let promotion = promotion(&session, task_id.clone(), exact_guidance)?;
        session.record_durably_appended_task_guidance_promotion(promotion.clone())?;

        let source = resolve_task_guidance_promotion_source(
            &session,
            &task_id,
            &promotion,
            Some(exact_guidance),
        )?;
        assert_eq!(source, promotion.source_turn);
        assert!(
            resolve_task_guidance_promotion_source(
                &session,
                &TaskId::new("different_task")?,
                &promotion,
                Some(exact_guidance),
            )
            .is_err()
        );
        assert!(
            resolve_task_guidance_promotion_source(
                &session,
                &task_id,
                &promotion,
                Some("different prompt"),
            )
            .is_err()
        );

        let empty_session = Session::new("test", "model");
        assert!(
            resolve_task_guidance_promotion_source(
                &empty_session,
                &task_id,
                &promotion,
                Some(exact_guidance),
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn continuation_failure_after_direct_attempt_uses_task_root_finalizer() -> Result<()> {
        let mut session = Session::new("test", "model");
        let task_id = TaskId::new("direct_task")?;
        let parent_session_ref = SessionRef::new_relative("session.jsonl")?;
        let objective = "continue the admitted task";
        session.append_control(ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: parent_session_ref.clone(),
            objective: objective.to_owned(),
            title: None,
            status: TaskRunStatus::Started,
            reason: None,
        }))?;
        let admission = sigil_kernel::TaskDirectExecutionAdmittedV1::task_request(
            task_id.clone(),
            objective,
            1,
        );
        session.append_control(ControlEntry::TaskDirectExecutionAdmittedV1(
            admission.clone(),
        ))?;
        let continuation_entry_frontier = session.entries().len();
        session.append_control(ControlEntry::TaskDirectExecutionAttemptV1(
            sigil_kernel::TaskDirectExecutionAttemptV1::started(&admission, 1),
        ))?;

        let result = finalize_task_continuation_root(
            &mut session,
            &task_id,
            &parent_session_ref,
            objective,
            &sigil_kernel::RunCancellationOwner::new().handle(),
            continuation_entry_frontier,
            Err(anyhow!("direct execution failed after dispatch")),
        );

        assert!(
            result.is_err(),
            "execution failure remains visible to the caller"
        );
        assert_eq!(
            session
                .task_state_projection()
                .tasks
                .get(&task_id)
                .map(|task| task.status),
            Some(TaskRunStatus::Failed),
            "a persisted direct attempt means execution crossed the resumable preflight boundary"
        );
        Ok(())
    }
}
