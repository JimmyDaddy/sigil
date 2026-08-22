use super::write_lease::has_active_task_write_lease;
use super::*;

pub(super) fn run_status_from_step_status(status: TaskStepStatus) -> RunStatus {
    match status {
        TaskStepStatus::Pending | TaskStepStatus::Running => RunStatus::Running,
        TaskStepStatus::Completed => RunStatus::Completed,
        TaskStepStatus::Failed => RunStatus::Failed,
        TaskStepStatus::Blocked => RunStatus::Blocked,
        TaskStepStatus::Cancelled => RunStatus::Cancelled,
        TaskStepStatus::Interrupted => RunStatus::Interrupted,
        TaskStepStatus::Superseded => RunStatus::Cancelled,
    }
}

pub(super) fn latest_executable_plan(task: &TaskRunProjection) -> Result<(u32, Vec<TaskStepSpec>)> {
    let plan_version = task
        .latest_plan_version
        .ok_or_else(|| anyhow!("planner did not create task plan"))?;
    let plan = task
        .plans
        .get(&plan_version)
        .ok_or_else(|| anyhow!("missing projected task plan v{plan_version}"))?;
    if plan.status != TaskPlanStatus::Accepted {
        return Err(anyhow!("task plan v{plan_version} is not accepted"));
    }
    Ok((plan_version, plan.steps.clone()))
}

pub(super) struct TaskRunnableSelection {
    pub(super) steps: Vec<TaskStepSpec>,
    pub(super) paused_reason: Option<String>,
}

pub(super) fn runnable_steps_for_continue(
    session: &Session,
    task: &TaskRunProjection,
    plan_version: u32,
    plan_steps: &[TaskStepSpec],
    max_parallel_read_steps: usize,
    max_parallel_changeset_steps: usize,
    step_options: [&AgentRunOptions; 3],
) -> Result<TaskRunnableSelection> {
    let Some(plan) = task.plans.get(&plan_version) else {
        return Ok(TaskRunnableSelection {
            steps: resumable_steps(task, plan_version, plan_steps),
            paused_reason: None,
        });
    };
    let Some(graph) = plan.graph.as_ref() else {
        if let Some(error) = plan.graph_validation_error.as_deref() {
            bail!("task plan v{plan_version} graph is invalid: {error}");
        }
        return Ok(TaskRunnableSelection {
            steps: resumable_steps(task, plan_version, plan_steps),
            paused_reason: None,
        });
    };

    let active_write_lease = has_active_task_write_lease(session, step_options)?;
    let queue = graph.ready_queue_with_active_write_lease(
        &task.steps,
        TaskReadyQueueOptions::new(max_parallel_read_steps.max(1))
            .with_max_concurrent_changeset_only(max_parallel_changeset_steps.max(1)),
        active_write_lease,
    );
    let step_ids = if !queue.read_only_batch.is_empty() {
        queue
            .read_only_batch
            .iter()
            .map(|step| step.step_id.clone())
            .collect::<Vec<_>>()
    } else if !queue.changeset_only_batch.is_empty() {
        queue
            .changeset_only_batch
            .iter()
            .map(|step| step.step_id.clone())
            .collect::<Vec<_>>()
    } else if !queue.worktree_batch.is_empty() {
        queue
            .worktree_batch
            .iter()
            .map(|step| step.step_id.clone())
            .collect::<Vec<_>>()
    } else if let Some(step) = queue.sequential_step.as_ref() {
        vec![step.step_id.clone()]
    } else {
        Vec::new()
    };
    let steps = step_ids
        .iter()
        .map(|step_id| {
            plan_steps
                .iter()
                .find(|step| &step.step_id == step_id)
                .cloned()
                .ok_or_else(|| anyhow!("task graph references missing step {}", step_id.as_str()))
        })
        .collect::<Result<Vec<_>>>()?;
    let paused_reason = if steps.is_empty() {
        if queue.deferred.is_empty() {
            if plan_steps_all_completed(task, plan_version, plan_steps) {
                None
            } else {
                Some(format!(
                    "plan v{plan_version} has no ready steps; waiting for dependencies"
                ))
            }
        } else {
            Some(format!(
                "plan v{plan_version} has deferred steps: {}",
                queue
                    .deferred
                    .iter()
                    .map(|step| format!(
                        "{}:{}",
                        step.step_id.as_str(),
                        task_ready_deferred_reason_label(step.reason)
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        }
    } else {
        None
    };
    Ok(TaskRunnableSelection {
        steps,
        paused_reason,
    })
}

pub(super) const DEFAULT_TASK_READ_ONLY_CONCURRENCY: usize = 4;

pub(super) fn task_ready_deferred_reason_label(reason: TaskReadyDeferredReason) -> &'static str {
    match reason {
        TaskReadyDeferredReason::ActiveWriteLease => "active_write_lease",
        TaskReadyDeferredReason::ConcurrencyBudget => "concurrency_budget",
        TaskReadyDeferredReason::RunningReadOnly => "running_read_only",
        TaskReadyDeferredReason::RunningChangesetOnly => "running_changeset_only",
        TaskReadyDeferredReason::RunningWorktree => "running_worktree",
        TaskReadyDeferredReason::RunningWrite => "running_write",
        TaskReadyDeferredReason::SequentialWrite => "sequential_write",
    }
}

pub(super) fn resumable_steps(
    task: &TaskRunProjection,
    plan_version: u32,
    plan_steps: &[TaskStepSpec],
) -> Vec<TaskStepSpec> {
    plan_steps
        .iter()
        .filter(|step| {
            !matches!(
                task.steps
                    .get(&(plan_version, step.step_id.clone()))
                    .map(|projected| projected.status),
                Some(TaskStepStatus::Completed)
            )
        })
        .cloned()
        .collect()
}

pub(super) fn plan_steps_all_completed(
    task: &TaskRunProjection,
    plan_version: u32,
    plan_steps: &[TaskStepSpec],
) -> bool {
    plan_steps.iter().all(|step| {
        task.steps
            .get(&(plan_version, step.step_id.clone()))
            .is_some_and(|projected| projected.status == TaskStepStatus::Completed)
    })
}

pub(super) fn cancels_dependent_steps(status: TaskStepStatus) -> bool {
    matches!(status, TaskStepStatus::Cancelled)
}

/// A failed predecessor is actionable task state, not task cancellation. Keep every downstream
/// contract durable and explicitly blocked so a repair/replan can resume the same accepted plan.
pub(super) fn append_blocked_dependent_steps<H>(
    session: &mut Session,
    handler: &mut H,
    task_id: &TaskId,
    plan_version: u32,
    plan_steps: &[TaskStepSpec],
    failed_step_id: &TaskStepId,
) -> Result<usize>
where
    H: EventHandler + Send,
{
    let projected = session.task_state_projection();
    let Some(task) = projected.tasks.get(task_id) else {
        return Ok(0);
    };
    let mut blocked = BTreeSet::<TaskStepId>::new();
    loop {
        let mut changed = false;
        for step in plan_steps {
            if &step.step_id == failed_step_id || blocked.contains(&step.step_id) {
                continue;
            }
            let depends_on_failed = step
                .depends_on
                .iter()
                .any(|dependency| dependency == failed_step_id || blocked.contains(dependency));
            if !depends_on_failed
                || task
                    .steps
                    .get(&(plan_version, step.step_id.clone()))
                    .is_some_and(|projection| projection.status.is_terminal())
            {
                continue;
            }
            blocked.insert(step.step_id.clone());
            changed = true;
        }
        if !changed {
            break;
        }
    }
    let mut count = 0;
    for step_id in blocked {
        let Some(step) = plan_steps.iter().find(|step| step.step_id == step_id) else {
            continue;
        };
        append_task_step(
            session,
            handler,
            task_id,
            plan_version,
            step,
            TaskStepStatus::Blocked,
            None,
            Some(format!("upstream_failed:{}", failed_step_id.as_str())),
        )?;
        count += 1;
    }
    Ok(count)
}

pub(super) fn append_cancelled_dependent_steps<H>(
    session: &mut Session,
    handler: &mut H,
    task_id: &TaskId,
    plan_version: u32,
    plan_steps: &[TaskStepSpec],
    failed_step_id: &TaskStepId,
    failed_status: TaskStepStatus,
) -> Result<usize>
where
    H: EventHandler + Send,
{
    let projected = session.task_state_projection();
    let Some(task) = projected.tasks.get(task_id) else {
        return Ok(0);
    };
    let mut cancelled = BTreeSet::<TaskStepId>::new();
    loop {
        let mut changed = false;
        for step in plan_steps {
            if &step.step_id == failed_step_id || cancelled.contains(&step.step_id) {
                continue;
            }
            let depends_on_failed = step
                .depends_on
                .iter()
                .any(|dependency| dependency == failed_step_id || cancelled.contains(dependency));
            if !depends_on_failed {
                continue;
            }
            if task
                .steps
                .get(&(plan_version, step.step_id.clone()))
                .is_some_and(|projection| projection.status.is_terminal())
            {
                continue;
            }
            cancelled.insert(step.step_id.clone());
            changed = true;
        }
        if !changed {
            break;
        }
    }
    let mut count = 0;
    for step_id in cancelled {
        let Some(step) = plan_steps.iter().find(|step| step.step_id == step_id) else {
            continue;
        };
        append_task_step(
            session,
            handler,
            task_id,
            plan_version,
            step,
            TaskStepStatus::Cancelled,
            None,
            Some(format!(
                "dependency {} ended with {}",
                failed_step_id.as_str(),
                task_step_status_label(failed_status)
            )),
        )?;
        count += 1;
    }
    Ok(count)
}

pub(super) fn task_step_status_label(status: TaskStepStatus) -> &'static str {
    match status {
        TaskStepStatus::Pending => "pending",
        TaskStepStatus::Running => "running",
        TaskStepStatus::Completed => "completed",
        TaskStepStatus::Failed => "failed",
        TaskStepStatus::Blocked => "blocked",
        TaskStepStatus::Cancelled => "cancelled",
        TaskStepStatus::Interrupted => "interrupted",
        TaskStepStatus::Superseded => "superseded",
    }
}

pub(super) fn step_status_from_outcome(output: &StepRunOutput) -> TaskStepStatus {
    if output.outcome.terminal_reason == AgentRunTerminalReason::MaxTurns
        || !output.outcome.interrupted_tool_calls.is_empty()
    {
        TaskStepStatus::Interrupted
    } else if output
        .outcome
        .terminal_reason
        .blocks_successful_completion()
        // Tool errors belong to the attempt history.  They are only a current blocker when the
        // participant still has no accepted final answer.  This is what allows a denied command
        // followed by a safe alternate command to finish as a completed step with a warning.
        || unresolved_blocking_tool_error(output)
        || (output.outcome.approval_denials > 0 && output.final_text.trim().is_empty())
    {
        TaskStepStatus::Blocked
    } else if !output.outcome.tool_errors.is_empty() && output.final_text.trim().is_empty() {
        TaskStepStatus::Failed
    } else if output.changeset_proposal.is_some() {
        TaskStepStatus::Blocked
    } else if output.final_text.trim().is_empty() {
        // A task participant must leave a bounded, durable completion report. Tool activity
        // alone is not an acceptance signal: without a final report the host cannot safely
        // distinguish a completed step from a provider stream that ended mid-protocol.
        TaskStepStatus::Blocked
    } else {
        TaskStepStatus::Completed
    }
}

pub(super) fn step_status_after_readiness(
    status: TaskStepStatus,
    readiness: &ReadinessEvaluatedEntry,
) -> TaskStepStatus {
    if status == TaskStepStatus::Completed && readiness_blocks_step(readiness) {
        TaskStepStatus::Blocked
    } else {
        status
    }
}

pub(super) fn readiness_blocks_step(readiness: &ReadinessEvaluatedEntry) -> bool {
    readiness
        .evaluation
        .required_actions
        .iter()
        .any(required_action_blocks_task_step)
}

pub(super) fn required_action_blocks_task_step(action: &RequiredAction) -> bool {
    !matches!(action, RequiredAction::ProvideVerificationConfig)
}

pub(super) fn step_reason_from_output(
    status: TaskStepStatus,
    output: &StepRunOutput,
) -> Option<String> {
    if status == TaskStepStatus::Blocked
        && output.outcome.terminal_reason == AgentRunTerminalReason::RepairReplanRequired
    {
        return Some(
            "repair/replan required: repeated semantic frontier produced no bounded final result"
                .to_owned(),
        );
    }
    if status == TaskStepStatus::Blocked && output.changeset_proposal.is_some() {
        return Some("changeset ready for merge review".to_owned());
    }
    if status == TaskStepStatus::Blocked && output.final_text.trim().is_empty() {
        return Some("participant ended without a bounded completion report".to_owned());
    }
    if status == TaskStepStatus::Completed {
        return None;
    }
    let error = output.outcome.tool_errors.iter().rev().find(|error| {
        status != TaskStepStatus::Blocked
            || matches!(
                error.kind,
                ToolErrorKind::ApprovalRequired
                    | ToolErrorKind::ApprovalDenied
                    | ToolErrorKind::PermissionDenied
                    | ToolErrorKind::PathOutsideWorkspace
                    | ToolErrorKind::ExternalDirectoryRequired
            )
            || recovery_tool_error_is_active(error)
    })?;
    Some(error.message.clone())
}

pub(super) fn step_reason_after_readiness(
    status: TaskStepStatus,
    output: &StepRunOutput,
    readiness: &ReadinessEvaluatedEntry,
) -> Option<String> {
    if status == TaskStepStatus::Blocked && readiness_blocks_step(readiness) {
        let blockers = readiness
            .evaluation
            .required_actions
            .iter()
            .filter(|action| required_action_blocks_task_step(action))
            .map(|action| format!("{action:?}"))
            .collect::<Vec<_>>();
        return Some(format!(
            "unresolved readiness blocker: {}",
            blockers.join(", ")
        ));
    }
    step_reason_from_output(status, output)
}

pub(super) fn task_status_from_step_status(status: TaskStepStatus) -> TaskRunStatus {
    match status {
        TaskStepStatus::Completed => TaskRunStatus::Completed,
        TaskStepStatus::Failed => TaskRunStatus::Failed,
        TaskStepStatus::Cancelled => TaskRunStatus::Cancelled,
        TaskStepStatus::Interrupted => TaskRunStatus::Interrupted,
        TaskStepStatus::Pending
        | TaskStepStatus::Running
        | TaskStepStatus::Blocked
        | TaskStepStatus::Superseded => TaskRunStatus::Paused,
    }
}

pub(super) fn step_terminal_reason(step_id: &TaskStepId, status: TaskStepStatus) -> String {
    match status {
        TaskStepStatus::Failed => format!("step {} failed", step_id.as_str()),
        TaskStepStatus::Blocked => format!("step {} blocked", step_id.as_str()),
        TaskStepStatus::Cancelled => format!("step {} cancelled", step_id.as_str()),
        TaskStepStatus::Interrupted => format!("step {} interrupted", step_id.as_str()),
        TaskStepStatus::Superseded => format!("step {} superseded", step_id.as_str()),
        TaskStepStatus::Pending | TaskStepStatus::Running | TaskStepStatus::Completed => {
            format!("step {} stopped", step_id.as_str())
        }
    }
}

pub(super) fn has_blocking_tool_error(outcome: &AgentRunOutcome) -> bool {
    outcome.tool_errors.iter().any(|error| {
        matches!(
            error.kind,
            ToolErrorKind::ApprovalRequired
                | ToolErrorKind::ApprovalDenied
                | ToolErrorKind::PermissionDenied
                | ToolErrorKind::PathOutsideWorkspace
                | ToolErrorKind::ExternalDirectoryRequired
                | ToolErrorKind::ResourceExhausted
                | ToolErrorKind::DurabilityRequired
                | ToolErrorKind::StalePreparedMutation
                | ToolErrorKind::WorkspaceConflict
                | ToolErrorKind::EffectReconciliationRequired
        )
    })
}

pub(super) fn has_active_recovery_tool_blocker(outcome: &AgentRunOutcome) -> bool {
    outcome
        .tool_errors
        .iter()
        .any(recovery_tool_error_is_active)
}

fn recovery_tool_error_is_active(error: &crate::ToolError) -> bool {
    error.kind.is_recovery_blocker()
        && error
            .details
            .get("active")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true)
}

/// Returns true only when a blocking tool error is still preventing a final answer.  A tool error
/// from an earlier command is historical evidence once the participant has produced a final
/// answer; it must not poison the parent task status.
pub(super) fn unresolved_blocking_tool_error(output: &StepRunOutput) -> bool {
    has_active_recovery_tool_blocker(&output.outcome)
        || (output.final_text.trim().is_empty() && has_blocking_tool_error(&output.outcome))
}
