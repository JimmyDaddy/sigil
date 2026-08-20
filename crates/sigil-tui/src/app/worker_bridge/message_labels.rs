use sigil_kernel::{
    ControlEntry, ConversationInputTarget, TaskRunStatus, TaskStateProjection, TaskStepStatus,
};

use super::super::formatting::summarize_terminal_reason;

pub(super) fn task_run_status_label(status: sigil_kernel::TaskRunStatus) -> &'static str {
    match status {
        sigil_kernel::TaskRunStatus::Started => "started",
        sigil_kernel::TaskRunStatus::Running => "running",
        sigil_kernel::TaskRunStatus::Paused => "paused",
        sigil_kernel::TaskRunStatus::Completed => "completed",
        sigil_kernel::TaskRunStatus::Failed => "failed",
        sigil_kernel::TaskRunStatus::Cancelled => "cancelled",
        sigil_kernel::TaskRunStatus::Interrupted => "interrupted",
    }
}

pub(super) fn task_run_finish_notice(
    task_id: &str,
    status: sigil_kernel::TaskRunStatus,
    entries: &[sigil_kernel::SessionLogEntry],
) -> String {
    let label = task_run_status_label(status);
    let reason = entries.iter().rev().find_map(|entry| {
        let sigil_kernel::SessionLogEntry::Control(ControlEntry::TaskRun(run)) = entry else {
            return None;
        };
        if run.task_id.as_str() == task_id
            && run.status == status
            && !matches!(status, sigil_kernel::TaskRunStatus::Completed)
        {
            return run
                .reason
                .as_deref()
                .filter(|value| !value.trim().is_empty());
        }
        None
    });
    if let Some(reason) = reason {
        format!("task {task_id} {label}: {reason}")
    } else {
        format!("task {task_id} {label}")
    }
}

pub(super) fn task_run_terminal_timeline_notice(
    task_id: &str,
    status: TaskRunStatus,
    entries: &[sigil_kernel::SessionLogEntry],
) -> Option<String> {
    let status_label = match status {
        TaskRunStatus::Failed => "Task failed",
        TaskRunStatus::Interrupted => "Task interrupted",
        TaskRunStatus::Started
        | TaskRunStatus::Running
        | TaskRunStatus::Paused
        | TaskRunStatus::Completed
        | TaskRunStatus::Cancelled => return None,
    };
    let projection = TaskStateProjection::from_entries(entries);
    let task = projection
        .tasks
        .values()
        .find(|task| task.task_id.as_str() == task_id);
    let Some(task) = task else {
        return Some(status_label.to_owned());
    };
    let title = task
        .title
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(task.objective.as_str());
    let mut notice = format!("{status_label}: {title}");
    let failed_step_title = task.latest_plan_version.and_then(|plan_version| {
        let plan = task.plans.get(&plan_version)?;
        plan.steps.iter().find_map(|step| {
            let projected = task.steps.get(&(plan_version, step.step_id.clone()))?;
            matches!(
                projected.status,
                TaskStepStatus::Failed | TaskStepStatus::Interrupted
            )
            .then_some(step.title.as_str())
        })
    });
    if let Some(reason) = task
        .reason
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        let reason = summarize_terminal_reason(reason, 120);
        if let Some(step_title) = failed_step_title {
            notice.push_str(&format!("\n{step_title} — {reason}"));
        } else {
            notice.push_str(&format!("\n{reason}"));
        }
    }
    Some(notice)
}

pub(super) fn summarize_queued_prompt(prompt: &str) -> String {
    let normalized = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= 48 {
        normalized
    } else {
        format!("{}...", normalized.chars().take(45).collect::<String>())
    }
}

pub(super) fn queued_prompt_summary_noun(target: &ConversationInputTarget) -> &'static str {
    match target {
        ConversationInputTarget::MainThread => "follow-up",
        ConversationInputTarget::AgentThread { .. } => "agent message",
        ConversationInputTarget::Task { .. } => "task guidance",
    }
}
