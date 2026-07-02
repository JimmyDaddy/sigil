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
        TaskReadyQueueOptions::new(DEFAULT_TASK_READ_ONLY_CONCURRENCY),
        active_write_lease,
    );
    let step_ids = if !queue.read_only_batch.is_empty() {
        queue
            .read_only_batch
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

const DEFAULT_TASK_READ_ONLY_CONCURRENCY: usize = 4;

pub(super) fn task_ready_deferred_reason_label(reason: TaskReadyDeferredReason) -> &'static str {
    match reason {
        TaskReadyDeferredReason::ActiveWriteLease => "active_write_lease",
        TaskReadyDeferredReason::ConcurrencyBudget => "concurrency_budget",
        TaskReadyDeferredReason::RunningReadOnly => "running_read_only",
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
    matches!(
        status,
        TaskStepStatus::Failed | TaskStepStatus::Cancelled | TaskStepStatus::Interrupted
    )
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
    } else if output.outcome.approval_denials > 0 || has_blocking_tool_error(&output.outcome) {
        TaskStepStatus::Blocked
    } else if !output.outcome.tool_errors.is_empty() && output.final_text.trim().is_empty() {
        TaskStepStatus::Failed
    } else if output.changeset_proposal.is_some() {
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
    if status == TaskStepStatus::Blocked && output.changeset_proposal.is_some() {
        return Some("changeset ready for merge review".to_owned());
    }
    let error = output.outcome.tool_errors.first()?;
    if status == TaskStepStatus::Completed {
        Some(format!("recovered tool error: {}", error.message))
    } else {
        Some(error.message.clone())
    }
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

pub(super) fn child_status_from_output(output: &StepRunOutput) -> TaskChildSessionStatus {
    if output.outcome.terminal_reason == AgentRunTerminalReason::MaxTurns
        || !output.outcome.interrupted_tool_calls.is_empty()
    {
        TaskChildSessionStatus::Interrupted
    } else if output.outcome.approval_denials > 0
        || has_blocking_tool_error(&output.outcome)
        || (!output.outcome.tool_errors.is_empty() && output.final_text.trim().is_empty())
    {
        TaskChildSessionStatus::Failed
    } else {
        TaskChildSessionStatus::Completed
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
        )
    })
}
