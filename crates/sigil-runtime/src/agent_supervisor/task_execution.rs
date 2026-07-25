use std::path::Path;

use anyhow::{Result, anyhow};
use sigil_kernel::{
    AgentRunOptions, ApprovalHandler, CheckDiscoverySource, CheckPromotion, CheckSpec,
    CheckSpecRecordedEntry, CompletionCriteria, ControlEntry, DEFAULT_TASK_VERIFICATION_SCOPE_HASH,
    DiscoveredCheck, EventHandler, EvidenceScope, RootConfig, RunCancellationHandle, RunEvent,
    SandboxProfileRequirement, SequentialTaskRequest, Session, SessionRef,
    TaskGuidancePromotedEntry, TaskId, TaskRunEntry, TaskRunStatus, ToolRegistry,
    VerificationPolicy, VerificationPolicyChangedEntry, WorkspaceTrustRequirement,
    discover_candidate_checks_with_user_config, safe_persistence_text, stable_workspace_id,
};

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
    pub handler: &'a mut H,
    pub cancellation_handle: RunCancellationHandle,
}

/// Host-owned durable Task selected for an explicit continuation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTaskContinuation {
    pub task_id: TaskId,
    pub parent_session_ref: SessionRef,
    pub objective: String,
    pub needs_planning: bool,
}

/// Complete host-owned material needed to continue one durable Task.
pub struct ContinuedTaskExecution<'a, H> {
    pub requested_task_id: Option<TaskId>,
    pub guidance: Option<String>,
    pub guidance_promotion: Option<TaskGuidancePromotedEntry>,
    pub root_config: RootConfig,
    pub options: AgentRunOptions,
    pub base_registry: ToolRegistry,
    pub agent_supervisor: AgentSupervisor,
    pub role_provider_builder: &'a dyn TaskRoleProviderBuilder,
    pub handler: &'a mut H,
    pub cancellation_handle: RunCancellationHandle,
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
    Ok(ResolvedTaskContinuation {
        task_id: task.task_id.clone(),
        parent_session_ref: task.parent_session_ref.clone(),
        objective: task.objective.clone(),
        needs_planning: task.latest_plan_version.is_none(),
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
        handler,
        cancellation_handle,
    } = request;
    materialize_task_verification_config(
        session,
        handler,
        &root_config,
        &options.workspace_root,
        &task_id,
    )?;
    let TaskRoleRuntime {
        orchestrator,
        planner_options,
        executor_options,
        subagent_read_options,
        subagent_write_options,
    } = build_task_role_runtime(
        &root_config,
        &options,
        &base_registry,
        agent_supervisor,
        role_provider_builder,
    )?;
    orchestrator
        .with_cancellation(cancellation_handle)
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
        guidance_promotion,
        root_config,
        options,
        base_registry,
        agent_supervisor,
        role_provider_builder,
        handler,
        cancellation_handle,
    } = request;
    let task = resolve_task_continuation(session, requested_task_id.as_ref().map(TaskId::as_str))?;
    if task.needs_planning && guidance.is_some() {
        return Err(anyhow!(
            "recovered task has no accepted plan; continue it without guidance to rerun the planner"
        ));
    }
    materialize_task_verification_config(
        session,
        handler,
        &root_config,
        &options.workspace_root,
        &task.task_id,
    )?;
    let TaskRoleRuntime {
        orchestrator,
        planner_options,
        executor_options,
        subagent_read_options,
        subagent_write_options,
    } = build_task_role_runtime(
        &root_config,
        &options,
        &base_registry,
        agent_supervisor,
        role_provider_builder,
    )?;
    let task_request = SequentialTaskRequest {
        task_id: task.task_id,
        parent_session_ref: task.parent_session_ref,
        objective: task.objective,
    };
    let orchestrator = orchestrator.with_cancellation(cancellation_handle);
    let output = if task.needs_planning {
        if guidance_promotion.is_some() {
            return Err(anyhow!(
                "recovered task has no accepted plan; guidance promotion is not applicable"
            ));
        }
        orchestrator
            .run(
                session,
                task_request,
                planner_options,
                executor_options,
                subagent_read_options,
                subagent_write_options,
                root_config.task.max_plan_steps,
                handler,
                approval_handler,
            )
            .await
    } else {
        match (guidance, guidance_promotion) {
            (Some(guidance), Some(promotion)) => {
                orchestrator
                    .continue_run_with_guidance_review(
                        session,
                        task_request,
                        planner_options,
                        executor_options,
                        subagent_read_options,
                        subagent_write_options,
                        root_config.task.max_plan_steps,
                        guidance,
                        promotion,
                        handler,
                        approval_handler,
                    )
                    .await
            }
            (guidance, None) => {
                orchestrator
                    .continue_run(
                        session,
                        task_request,
                        executor_options,
                        subagent_read_options,
                        subagent_write_options,
                        guidance,
                        handler,
                        approval_handler,
                    )
                    .await
            }
            (None, Some(_)) => Err(anyhow!(
                "task guidance promotion is missing exact prompt material"
            )),
        }
    }?;
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
    let Err(error) = &result else {
        return result;
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
            status: TaskRunStatus::Failed,
            reason: Some(safe_persistence_text(&format!(
                "task orchestration failed before a terminal state: {error:#}"
            ))),
        }))?;
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
    let trust_entry = projection.workspace_trust.get(&workspace_id);
    let workspace_trust_snapshot_id = trust_entry
        .map(|entry| entry.workspace_trust_snapshot_id.clone())
        .unwrap_or_else(|| format!("workspace-trust:unknown:{workspace_id}"));
    let workspace_scope = EvidenceScope::Workspace(workspace_id);
    let discovered = discover_candidate_checks_with_user_config(
        workspace_root,
        workspace_trust_snapshot_id,
        source_event_id.clone(),
        &root_config.verification,
    )?;
    let mut entries = Vec::new();
    for candidate in discovered {
        let source = candidate.candidate.source;
        let candidate_source_event_id = candidate.candidate.source_event_id.clone();
        let promoted = match source {
            CheckDiscoverySource::UserExplicitConfig => {
                let promotion = CheckPromotion::ExplicitUserConfig {
                    config_event_id: source_event_id.clone(),
                };
                candidate.promote(DEFAULT_TASK_VERIFICATION_SCOPE_HASH, promotion)
            }
            _ => match workspace_promoted_check_for_candidate(
                &projection,
                &workspace_scope,
                &candidate,
            ) {
                Some(trusted) => Ok(trusted),
                None => continue,
            },
        }?;
        entries.push(CheckSpecRecordedEntry::new(
            scope.clone(),
            promoted,
            candidate_source_event_id,
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

    for control in controls {
        session.append_control(control.clone())?;
        handler.handle(RunEvent::Control(control))?;
    }
    Ok(())
}

fn workspace_promoted_check_for_candidate(
    projection: &sigil_kernel::VerificationStateProjection,
    workspace_scope: &EvidenceScope,
    candidate: &DiscoveredCheck,
) -> Option<sigil_kernel::TrustedCheckSpec> {
    let entry = projection.check_spec(workspace_scope, &candidate.suggested_check_spec_id)?;
    let expected = CheckSpec::new(
        candidate.suggested_check_spec_id.clone(),
        candidate.candidate.command.clone(),
        candidate.effect,
        DEFAULT_TASK_VERIFICATION_SCOPE_HASH,
    );
    let trusted = &entry.trusted_check;
    if trusted.source != candidate.candidate.source
        || trusted.check_spec.check_spec_hash != expected.check_spec_hash
    {
        return None;
    }
    Some(trusted.clone())
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
