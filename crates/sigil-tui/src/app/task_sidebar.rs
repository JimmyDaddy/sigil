use sigil_kernel::{
    SessionLogEntry, TaskPlanProjection, TaskRunProjection, TaskRunStatus, TaskStateProjection,
    TaskStepId, TaskStepSpec, TaskStepStatus, TerminalTaskProjection,
};

use crate::ui::{StatusKind, status_symbol};

use super::formatting::truncate_session_view_text;

const TASK_SIDEBAR_STEP_LIMIT: usize = 6;
const TASK_STRIP_STEP_LIMIT: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TaskStripView {
    pub(crate) title: String,
    pub(crate) detail: String,
    pub(crate) rows: Vec<TaskStripRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TaskStripRow {
    pub(crate) kind: StatusKind,
    pub(crate) label: String,
    pub(crate) detail: String,
    pub(crate) active: bool,
}

pub(super) fn task_sidebar_lines(entries: &[SessionLogEntry]) -> Vec<String> {
    let terminal_lines = terminal_task_sidebar_lines(entries);
    let projection = TaskStateProjection::from_entries(entries);
    let Some(task) = projection.latest_task() else {
        return terminal_lines;
    };
    let mut lines = vec![
        format!("task: {}", task.task_id.as_str()),
        format!("status: {}", task_run_status_label(task.status)),
    ];
    let mut step_lines = Vec::new();
    if let Some(plan_version) = task.latest_plan_version {
        lines.push(format!("plan: v{plan_version}"));
        if let Some(plan) = task.plans.get(&plan_version) {
            let completed_steps = plan
                .steps
                .iter()
                .filter(|step| {
                    task.steps
                        .get(&(plan_version, step.step_id.clone()))
                        .is_some_and(|projected| projected.status == TaskStepStatus::Completed)
                })
                .count();
            lines.push(format!(
                "progress: {completed_steps}/{} done",
                plan.steps.len()
            ));
            step_lines = task_sidebar_focus_lines(task, plan_version, plan);
        }
    }
    if let Some((plan_version, step_id)) = &task.current_step {
        let status = task
            .steps
            .get(&(*plan_version, step_id.clone()))
            .map(|step| task_step_status_label(step.status))
            .unwrap_or("running");
        lines.push(format!(
            "current: v{plan_version}:{} {status}",
            step_id.as_str()
        ));
    } else if task.status == TaskRunStatus::Completed {
        if let Some((plan_version, step_id, status)) = task_sidebar_last_plan_step(task) {
            lines.push(format!(
                "last: v{plan_version}:{} {}",
                step_id.as_str(),
                task_step_status_label(status)
            ));
        }
    } else if let Some((plan_version, step_id, status)) = task_sidebar_last_problem_step(task) {
        lines.push(format!(
            "last: v{plan_version}:{} {}",
            step_id.as_str(),
            task_step_status_label(status)
        ));
    }
    if let Some(reason) = task
        .reason
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        lines.push(format!(
            "reason: {}",
            truncate_session_view_text(reason, 48)
        ));
    }
    lines.extend(step_lines);
    if task.route_unverified {
        lines.push("routes: unverified".to_owned());
    }
    if task.child_unavailable {
        lines.push("child: unavailable".to_owned());
    }
    lines.extend(terminal_lines);
    lines
}

pub(crate) fn task_strip_view(entries: &[SessionLogEntry]) -> Option<TaskStripView> {
    let projection = TaskStateProjection::from_entries(entries);
    let task = projection.latest_task()?;
    let mut rows = Vec::new();
    let mut detail = task_run_status_label(task.status).to_owned();

    if let Some(plan_version) = task.latest_plan_version
        && let Some(plan) = task.plans.get(&plan_version)
    {
        let completed_steps = plan
            .steps
            .iter()
            .filter(|step| {
                task.steps
                    .get(&(plan_version, step.step_id.clone()))
                    .is_some_and(|projected| projected.status == TaskStepStatus::Completed)
            })
            .count();
        detail = format!(
            "{} · v{plan_version} · {completed_steps}/{} done",
            task_run_status_label(task.status),
            plan.steps.len()
        );
        rows = task_strip_step_rows(task, plan_version, plan);
    }

    if rows.is_empty() {
        rows.push(TaskStripRow {
            kind: task_run_status_kind(task.status),
            label: task.objective.clone(),
            detail: task_run_status_label(task.status).to_owned(),
            active: !matches!(
                task.status,
                TaskRunStatus::Completed
                    | TaskRunStatus::Failed
                    | TaskRunStatus::Cancelled
                    | TaskRunStatus::Interrupted
            ),
        });
    }

    Some(TaskStripView {
        title: format!("Task {}", task.task_id.as_str()),
        detail,
        rows,
    })
}

fn terminal_task_sidebar_lines(entries: &[SessionLogEntry]) -> Vec<String> {
    let projection = TerminalTaskProjection::from_entries(entries);
    let running_count = projection.active_task_ids.len();
    if running_count == 0 {
        return Vec::new();
    }
    let mut lines = vec![format!("terminal: {running_count} running")];
    let latest_active = projection
        .latest()
        .filter(|summary| summary.status.is_active())
        .or_else(|| {
            projection
                .active_task_ids
                .iter()
                .rev()
                .find_map(|task_id| projection.tasks.get(task_id))
        });
    if let Some(latest) = latest_active {
        lines.push(format!(
            "terminal latest: {} {}",
            latest.handle.task_id.as_str(),
            latest.status.as_str()
        ));
    }
    lines
}

fn task_sidebar_focus_lines(
    task: &TaskRunProjection,
    plan_version: u32,
    plan: &TaskPlanProjection,
) -> Vec<String> {
    let focus_index = task_sidebar_focus_step_index(task, plan_version, plan);
    let mut selected_indices: Vec<usize> =
        (0..plan.steps.len().min(TASK_SIDEBAR_STEP_LIMIT)).collect();
    if let Some(focus_index) = focus_index
        && focus_index >= selected_indices.len()
        && !selected_indices.is_empty()
    {
        selected_indices.pop();
        selected_indices.push(focus_index);
    }

    let mut lines = selected_indices
        .iter()
        .map(|index| {
            let step = &plan.steps[*index];
            let status = task_sidebar_step_status(task, plan_version, step);
            let marker = task_sidebar_step_marker(status);
            format!(
                "{marker} {}. {} {} · {}",
                index + 1,
                task_step_status_label(status),
                step.step_id.as_str(),
                step.title
            )
        })
        .collect::<Vec<_>>();
    let hidden_steps = plan.steps.len().saturating_sub(selected_indices.len());
    if hidden_steps > 0 {
        let summary = task_sidebar_hidden_step_summary(task, plan_version, plan, &selected_indices);
        lines.push(format!("+{hidden_steps} more steps · {summary}"));
    }
    lines
}

fn task_strip_step_rows(
    task: &TaskRunProjection,
    plan_version: u32,
    plan: &TaskPlanProjection,
) -> Vec<TaskStripRow> {
    let focus_index = task_sidebar_focus_step_index(task, plan_version, plan);
    let mut selected_indices: Vec<usize> =
        (0..plan.steps.len().min(TASK_STRIP_STEP_LIMIT)).collect();
    if let Some(focus_index) = focus_index
        && focus_index >= selected_indices.len()
        && !selected_indices.is_empty()
    {
        selected_indices.pop();
        selected_indices.push(focus_index);
    }

    let mut rows = selected_indices
        .iter()
        .map(|index| {
            let step = &plan.steps[*index];
            let status = task_sidebar_step_status(task, plan_version, step);
            TaskStripRow {
                kind: task_step_status_kind(status),
                label: format!("{}. {}", index + 1, step.title),
                detail: format!(
                    "{} · {}",
                    task_step_status_label(status),
                    step.step_id.as_str()
                ),
                active: focus_index == Some(*index),
            }
        })
        .collect::<Vec<_>>();
    let hidden_steps = plan.steps.len().saturating_sub(selected_indices.len());
    if hidden_steps > 0 {
        let summary = task_sidebar_hidden_step_summary(task, plan_version, plan, &selected_indices);
        rows.push(TaskStripRow {
            kind: StatusKind::Unknown,
            label: format!("+{hidden_steps} more steps"),
            detail: summary,
            active: false,
        });
    }
    rows
}

fn task_sidebar_hidden_step_summary(
    task: &TaskRunProjection,
    plan_version: u32,
    plan: &TaskPlanProjection,
    selected_indices: &[usize],
) -> String {
    let mut pending = 0usize;
    let mut running = 0usize;
    let mut completed = 0usize;
    let mut failed = 0usize;
    let mut blocked = 0usize;
    let mut cancelled = 0usize;
    let mut interrupted = 0usize;
    for (index, step) in plan.steps.iter().enumerate() {
        if selected_indices.contains(&index) {
            continue;
        }
        match task_sidebar_step_status(task, plan_version, step) {
            TaskStepStatus::Pending => pending += 1,
            TaskStepStatus::Running => running += 1,
            TaskStepStatus::Completed => completed += 1,
            TaskStepStatus::Failed => failed += 1,
            TaskStepStatus::Blocked => blocked += 1,
            TaskStepStatus::Cancelled => cancelled += 1,
            TaskStepStatus::Interrupted => interrupted += 1,
        }
    }
    let mut parts = Vec::new();
    for (count, label) in [
        (running, "running"),
        (failed, "failed"),
        (blocked, "blocked"),
        (cancelled, "cancelled"),
        (interrupted, "interrupted"),
        (pending, "pending"),
        (completed, "completed"),
    ] {
        if count > 0 {
            parts.push(format!("{count} {label}"));
        }
    }
    parts.join(", ")
}

fn task_sidebar_step_marker(status: TaskStepStatus) -> &'static str {
    status_symbol(task_step_status_kind(status))
}

fn task_step_status_kind(status: TaskStepStatus) -> StatusKind {
    match status {
        TaskStepStatus::Pending => StatusKind::Pending,
        TaskStepStatus::Running => StatusKind::Running,
        TaskStepStatus::Completed => StatusKind::Success,
        TaskStepStatus::Failed
        | TaskStepStatus::Blocked
        | TaskStepStatus::Cancelled
        | TaskStepStatus::Interrupted => StatusKind::Error,
    }
}

fn task_run_status_kind(status: TaskRunStatus) -> StatusKind {
    match status {
        TaskRunStatus::Started | TaskRunStatus::Running => StatusKind::Running,
        TaskRunStatus::Paused => StatusKind::Warning,
        TaskRunStatus::Completed => StatusKind::Success,
        TaskRunStatus::Failed | TaskRunStatus::Cancelled | TaskRunStatus::Interrupted => {
            StatusKind::Error
        }
    }
}

fn task_sidebar_focus_step_index(
    task: &TaskRunProjection,
    plan_version: u32,
    plan: &TaskPlanProjection,
) -> Option<usize> {
    if let Some((current_plan_version, current_step_id)) = &task.current_step
        && *current_plan_version == plan_version
        && let Some(index) = plan
            .steps
            .iter()
            .position(|step| &step.step_id == current_step_id)
    {
        return Some(index);
    }
    if task.status == TaskRunStatus::Completed && !plan.steps.is_empty() {
        return Some(plan.steps.len() - 1);
    }
    plan.steps
        .iter()
        .position(|step| {
            matches!(
                task_sidebar_step_status(task, plan_version, step),
                TaskStepStatus::Failed
                    | TaskStepStatus::Blocked
                    | TaskStepStatus::Interrupted
                    | TaskStepStatus::Cancelled
            )
        })
        .or_else(|| {
            plan.steps.iter().position(|step| {
                task_sidebar_step_status(task, plan_version, step) != TaskStepStatus::Completed
            })
        })
}

fn task_sidebar_step_status(
    task: &TaskRunProjection,
    plan_version: u32,
    step: &TaskStepSpec,
) -> TaskStepStatus {
    task.steps
        .get(&(plan_version, step.step_id.clone()))
        .map(|projected| projected.status)
        .unwrap_or(TaskStepStatus::Pending)
}

fn task_sidebar_last_plan_step(
    task: &TaskRunProjection,
) -> Option<(u32, TaskStepId, TaskStepStatus)> {
    let plan_version = task.latest_plan_version?;
    let plan = task.plans.get(&plan_version)?;
    let step = plan.steps.last()?;
    Some((
        plan_version,
        step.step_id.clone(),
        task_sidebar_step_status(task, plan_version, step),
    ))
}

fn task_sidebar_last_problem_step(
    task: &TaskRunProjection,
) -> Option<(u32, TaskStepId, TaskStepStatus)> {
    let plan_version = task.latest_plan_version?;
    let plan = task.plans.get(&plan_version)?;
    plan.steps.iter().find_map(|step| {
        let status = task_sidebar_step_status(task, plan_version, step);
        if matches!(
            status,
            TaskStepStatus::Failed
                | TaskStepStatus::Blocked
                | TaskStepStatus::Interrupted
                | TaskStepStatus::Cancelled
        ) {
            Some((plan_version, step.step_id.clone(), status))
        } else {
            None
        }
    })
}

pub(super) fn task_run_status_label(status: TaskRunStatus) -> &'static str {
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

fn task_step_status_label(status: TaskStepStatus) -> &'static str {
    match status {
        TaskStepStatus::Pending => "pending",
        TaskStepStatus::Running => "running",
        TaskStepStatus::Completed => "completed",
        TaskStepStatus::Failed => "failed",
        TaskStepStatus::Blocked => "blocked",
        TaskStepStatus::Cancelled => "cancelled",
        TaskStepStatus::Interrupted => "interrupted",
    }
}

pub(super) fn task_child_session_status_label(
    status: sigil_kernel::TaskChildSessionStatus,
) -> &'static str {
    match status {
        sigil_kernel::TaskChildSessionStatus::Started => "started",
        sigil_kernel::TaskChildSessionStatus::Completed => "completed",
        sigil_kernel::TaskChildSessionStatus::Failed => "failed",
        sigil_kernel::TaskChildSessionStatus::Cancelled => "cancelled",
        sigil_kernel::TaskChildSessionStatus::Interrupted => "interrupted",
        sigil_kernel::TaskChildSessionStatus::Unavailable => "unavailable",
    }
}
