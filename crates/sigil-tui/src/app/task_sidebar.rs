use sigil_kernel::{
    ChildVerificationReceiptLinked, EvidenceScope, ReadinessEvaluatedEntry, RequiredAction,
    SessionLogEntry, TaskChildSessionEntry, TaskPlanProjection, TaskRunProjection, TaskRunStatus,
    TaskStateProjection, TaskStepId, TaskStepSpec, TaskStepStatus, TerminalTaskProjection,
    VerificationCheckRunEntry, VerificationCheckRunStatus, VerificationStateProjection,
    VerificationVerdict, VisibleCompletionState,
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
    let verification_projection = VerificationStateProjection::from_entries(entries);
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
            step_lines =
                task_sidebar_focus_lines(task, plan_version, plan, &verification_projection);
        }
    }
    if let Some((plan_version, step_id)) = &task.current_step {
        let readiness =
            task_step_readiness_by_id(task, *plan_version, step_id, &verification_projection);
        let step_spec = task_plan_step(task, *plan_version, step_id);
        let status = task
            .steps
            .get(&(*plan_version, step_id.clone()))
            .map(|step| task_step_display_label(step_spec, step.status, readiness))
            .unwrap_or("running");
        lines.push(format!(
            "current: v{plan_version}:{} {status}",
            step_id.as_str()
        ));
    } else if task.status == TaskRunStatus::Completed {
        if let Some((plan_version, step, status)) = task_sidebar_last_plan_step(task) {
            lines.push(format!(
                "last: v{plan_version}:{} {}",
                step.step_id.as_str(),
                task_step_display_label(
                    Some(step),
                    status,
                    task_step_readiness_by_id(
                        task,
                        plan_version,
                        &step.step_id,
                        &verification_projection,
                    ),
                )
            ));
        }
    } else if let Some((plan_version, step, status)) = task_sidebar_last_problem_step(task) {
        lines.push(format!(
            "last: v{plan_version}:{} {}",
            step.step_id.as_str(),
            task_step_display_label(
                Some(step),
                status,
                task_step_readiness_by_id(
                    task,
                    plan_version,
                    &step.step_id,
                    &verification_projection,
                ),
            )
        ));
    }
    if let Some((scope, readiness)) =
        task_sidebar_focus_readiness_with_scope(task, &verification_projection)
    {
        lines.push(format!(
            "verification: {}",
            verification_verdict_label(readiness.evaluation.verification_verdict)
        ));
        if let Some(summary) = readiness_reason_summary(&readiness.evaluation.reasons, 48) {
            lines.push(format!("verification reason: {summary}"));
        }
        if let Some(summary) = child_merge_recheck_summary(entries, task, readiness, 48) {
            lines.push(format!("merge: {summary}"));
        }
        for action in readiness.evaluation.required_actions.iter().take(2) {
            lines.extend(required_action_context_lines(action));
            if let Some(run) = latest_check_run_for_action(entries, &scope, action) {
                let mut check_line = format!(
                    "check: {} {}",
                    truncate_session_view_text(&run.check_spec_id, 32),
                    check_run_status_label(run.status)
                );
                if let Some(timeout_ms) = run.timeout_ms {
                    check_line.push_str(&format!(" timeout={timeout_ms} ms"));
                }
                lines.push(check_line);
                if let Some(reason) = run.reason.as_ref().filter(|value| !value.trim().is_empty()) {
                    lines.push(format!(
                        "check reason: {}",
                        truncate_session_view_text(reason, 48)
                    ));
                }
                if !check_run_status_blocks_action(run.status) {
                    lines.push(format!("action: {}", required_action_label(action)));
                }
            } else {
                lines.push(format!("action: {}", required_action_label(action)));
            }
        }
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
    let verification_projection = VerificationStateProjection::from_entries(entries);
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
        if let Some((scope, readiness)) =
            task_sidebar_focus_readiness_with_scope(task, &verification_projection)
        {
            detail.push_str(" · ");
            detail.push_str(verification_verdict_label(
                readiness.evaluation.verification_verdict,
            ));
            if let Some(reason_summary) =
                readiness_reason_summary(&readiness.evaluation.reasons, 32)
            {
                detail.push_str(" · ");
                detail.push_str(&reason_summary);
            }
            if let Some(summary) = child_merge_recheck_summary(entries, task, readiness, 40) {
                detail.push_str(" · ");
                detail.push_str(&summary);
            }
            if let Some(run) = latest_check_run_for_actions(
                entries,
                &scope,
                &readiness.evaluation.required_actions,
            ) {
                detail.push_str(" · check ");
                detail.push_str(check_run_status_label(run.status));
                if let Some(timeout_ms) = run.timeout_ms {
                    detail.push_str(&format!(" timeout={timeout_ms} ms"));
                }
            } else if let Some(action_context) =
                required_action_context_summary(&readiness.evaluation.required_actions)
            {
                detail.push_str(" · ");
                detail.push_str(&action_context);
            }
        }
        rows = task_strip_step_rows(task, plan_version, plan, &verification_projection);
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
    verification_projection: &VerificationStateProjection,
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
            let readiness = task_step_readiness(task, step, verification_projection);
            let marker = task_sidebar_step_marker(Some(step), status, readiness);
            format!(
                "{marker} {}. {} {} · {}",
                index + 1,
                task_step_display_label(Some(step), status, readiness),
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
    verification_projection: &VerificationStateProjection,
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
            let readiness = task_step_readiness(task, step, verification_projection);
            let label = if task_step_needs_user_verification(Some(step), status, readiness) {
                format!("{}. needs check · {}", index + 1, step.title)
            } else if step.is_review_advisory() && status == TaskStepStatus::Completed {
                format!("{}. reviewed · {}", index + 1, step.title)
            } else {
                format!("{}. {}", index + 1, step.title)
            };
            TaskStripRow {
                kind: task_step_status_kind(Some(step), status, readiness),
                label,
                detail: task_strip_step_detail(step, status, readiness),
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

fn task_sidebar_step_marker(
    step: Option<&TaskStepSpec>,
    status: TaskStepStatus,
    readiness: Option<&ReadinessEvaluatedEntry>,
) -> &'static str {
    status_symbol(task_step_status_kind(step, status, readiness))
}

fn task_step_status_kind(
    step: Option<&TaskStepSpec>,
    status: TaskStepStatus,
    readiness: Option<&ReadinessEvaluatedEntry>,
) -> StatusKind {
    if task_step_verification_failed(readiness) {
        return StatusKind::Error;
    }
    if task_step_needs_user_verification(step, status, readiness) {
        return StatusKind::Warning;
    }
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

fn task_step_readiness<'a>(
    task: &TaskRunProjection,
    step: &TaskStepSpec,
    verification_projection: &'a VerificationStateProjection,
) -> Option<&'a ReadinessEvaluatedEntry> {
    let scope = EvidenceScope::Step(format!(
        "{}:{}",
        task.task_id.as_str(),
        step.step_id.as_str()
    ));
    verification_projection.latest_readiness(&scope)
}

fn task_step_readiness_by_id<'a>(
    task: &TaskRunProjection,
    _plan_version: u32,
    step_id: &TaskStepId,
    verification_projection: &'a VerificationStateProjection,
) -> Option<&'a ReadinessEvaluatedEntry> {
    let scope = EvidenceScope::Step(format!("{}:{}", task.task_id.as_str(), step_id.as_str()));
    verification_projection.latest_readiness(&scope)
}

fn task_sidebar_focus_readiness_with_scope<'a>(
    task: &TaskRunProjection,
    verification_projection: &'a VerificationStateProjection,
) -> Option<(EvidenceScope, &'a ReadinessEvaluatedEntry)> {
    if let Some((_, step_id)) = &task.current_step {
        let scope = task_step_scope(task, step_id);
        return verification_projection
            .latest_readiness(&scope)
            .map(|readiness| (scope, readiness));
    }
    let (plan_version, step, _) = task_sidebar_last_problem_step(task)?;
    let scope = task_step_scope(task, &step.step_id);
    task_step_readiness_by_id(task, plan_version, &step.step_id, verification_projection)
        .map(|readiness| (scope, readiness))
}

fn task_sidebar_last_plan_step(
    task: &TaskRunProjection,
) -> Option<(u32, &TaskStepSpec, TaskStepStatus)> {
    let plan_version = task.latest_plan_version?;
    let plan = task.plans.get(&plan_version)?;
    let step = plan.steps.last()?;
    Some((
        plan_version,
        step,
        task_sidebar_step_status(task, plan_version, step),
    ))
}

fn task_sidebar_last_problem_step(
    task: &TaskRunProjection,
) -> Option<(u32, &TaskStepSpec, TaskStepStatus)> {
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
            Some((plan_version, step, status))
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

fn task_step_display_label(
    step: Option<&TaskStepSpec>,
    status: TaskStepStatus,
    readiness: Option<&ReadinessEvaluatedEntry>,
) -> &'static str {
    if task_step_needs_user_verification(step, status, readiness) {
        return "needs check";
    }
    if step.is_some_and(|step| step.is_review_advisory()) && status == TaskStepStatus::Completed {
        return "reviewed";
    }
    task_step_status_label(status)
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

fn task_step_needs_user_verification(
    step: Option<&TaskStepSpec>,
    status: TaskStepStatus,
    readiness: Option<&ReadinessEvaluatedEntry>,
) -> bool {
    if step.is_some_and(|step| step.requires_system_verifier())
        && status == TaskStepStatus::Completed
        && readiness.is_none()
    {
        return true;
    }
    readiness.is_some_and(|entry| {
        entry.evaluation.visible_state == VisibleCompletionState::NeedsUser
            || matches!(
                entry.evaluation.verification_verdict,
                VerificationVerdict::Missing
                    | VerificationVerdict::Stale
                    | VerificationVerdict::Inconclusive
            )
    })
}

fn task_step_verification_failed(readiness: Option<&ReadinessEvaluatedEntry>) -> bool {
    readiness.is_some_and(|entry| {
        matches!(
            entry.evaluation.verification_verdict,
            VerificationVerdict::Failed
        )
    })
}

fn task_plan_step<'a>(
    task: &'a TaskRunProjection,
    plan_version: u32,
    step_id: &TaskStepId,
) -> Option<&'a TaskStepSpec> {
    task.plans
        .get(&plan_version)
        .and_then(|plan| plan.steps.iter().find(|step| &step.step_id == step_id))
}

fn task_step_mode_label(step: &TaskStepSpec) -> &'static str {
    step.effective_mode().as_str()
}

fn task_strip_step_detail(
    step: &TaskStepSpec,
    status: TaskStepStatus,
    readiness: Option<&ReadinessEvaluatedEntry>,
) -> String {
    let label = task_step_display_label(Some(step), status, readiness);
    if step.is_review_advisory() || step.requires_system_verifier() {
        return format!(
            "{label} · {} · {}",
            task_step_mode_label(step),
            step.step_id.as_str()
        );
    }
    format!("{label} · {}", step.step_id.as_str())
}

fn verification_verdict_label(verdict: VerificationVerdict) -> &'static str {
    match verdict {
        VerificationVerdict::NotEvaluated => "not evaluated",
        VerificationVerdict::NotApplicable => "not applicable",
        VerificationVerdict::Pending => "pending",
        VerificationVerdict::Passed => "passed",
        VerificationVerdict::Failed => "failed",
        VerificationVerdict::Missing => "missing",
        VerificationVerdict::Inconclusive => "inconclusive",
        VerificationVerdict::Stale => "stale",
        VerificationVerdict::Skipped => "skipped",
    }
}

fn required_action_context_lines(action: &RequiredAction) -> Vec<String> {
    match action {
        RequiredAction::ApproveCheckExecution { check_spec_id } => {
            vec![format!(
                "check approval: {}",
                truncate_session_view_text(check_spec_id, 32)
            )]
        }
        RequiredAction::TrustWorkspace => vec!["workspace trust: required".to_owned()],
        _ => Vec::new(),
    }
}

fn required_action_context_summary(actions: &[RequiredAction]) -> Option<String> {
    actions.iter().find_map(|action| match action {
        RequiredAction::ApproveCheckExecution { check_spec_id } => Some(format!(
            "check approval {}",
            truncate_session_view_text(check_spec_id, 24)
        )),
        RequiredAction::TrustWorkspace => Some("workspace trust required".to_owned()),
        _ => None,
    })
}

fn readiness_reason_summary(
    reasons: &[sigil_kernel::ReadinessReason],
    max_chars: usize,
) -> Option<String> {
    let labels = reasons
        .iter()
        .filter_map(readiness_reason_compact_label)
        .collect::<Vec<_>>();
    let first = labels.first()?;
    if labels.len() > 1 {
        return Some(compact_first_with_more_suffix(
            first,
            labels.len() - 1,
            max_chars,
        ));
    }
    Some(truncate_session_view_text(first, max_chars))
}

fn compact_first_with_more_suffix(first: &str, remaining: usize, max_chars: usize) -> String {
    let suffix = format!(" +{remaining} more");
    let normalized = first.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() + suffix.chars().count() <= max_chars {
        return format!("{normalized}{suffix}");
    }
    let suffix_len = suffix.chars().count();
    let ellipsis_len = 3;
    let Some(prefix_len) = max_chars.checked_sub(suffix_len + ellipsis_len) else {
        return truncate_session_view_text(&format!("{normalized}{suffix}"), max_chars);
    };
    if prefix_len == 0 {
        return truncate_session_view_text(&format!("{normalized}{suffix}"), max_chars);
    }
    let prefix = normalized.chars().take(prefix_len).collect::<String>();
    format!("{prefix}...{suffix}")
}

fn readiness_reason_compact_label(reason: &sigil_kernel::ReadinessReason) -> Option<String> {
    match reason {
        sigil_kernel::ReadinessReason::VerificationStale(cause) => Some(format!(
            "stale {}",
            verification_stale_reason_compact_label(&cause.reason)
        )),
        sigil_kernel::ReadinessReason::WorkspaceMutationSource {
            source_label,
            recovery_hint,
            ..
        } => Some(
            recovery_hint
                .as_deref()
                .map(|hint| format!("{source_label}: {hint}"))
                .unwrap_or_else(|| source_label.clone()),
        ),
        sigil_kernel::ReadinessReason::WorkspaceUnknownDirty { event_id } => {
            let event = event_id
                .as_deref()
                .map(|value| format!(" {}", truncate_session_view_text(value, 16)))
                .unwrap_or_default();
            Some(format!("unknown workspace change{event}"))
        }
        sigil_kernel::ReadinessReason::CheckMutatedVerificationScope { check_spec_id } => {
            Some(format!(
                "check changed files {}",
                truncate_session_view_text(check_spec_id, 16)
            ))
        }
        sigil_kernel::ReadinessReason::ReceiptScopeMismatch { receipt_id } => Some(format!(
            "scope mismatch {}",
            truncate_session_view_text(receipt_id, 16)
        )),
        sigil_kernel::ReadinessReason::ReceiptSnapshotMismatch { receipt_id } => Some(format!(
            "snapshot mismatch {}",
            truncate_session_view_text(receipt_id, 16)
        )),
        _ => None,
    }
}

fn verification_stale_reason_compact_label(
    reason: &sigil_kernel::VerificationStaleReason,
) -> String {
    match reason {
        sigil_kernel::VerificationStaleReason::WorkspaceChanged(event_id) => {
            format!(
                "workspace changed {}",
                truncate_session_view_text(event_id, 16)
            )
        }
        sigil_kernel::VerificationStaleReason::CheckSpecChanged(event_id) => {
            format!(
                "check spec changed {}",
                truncate_session_view_text(event_id, 16)
            )
        }
        sigil_kernel::VerificationStaleReason::PolicyChanged(event_id) => {
            format!(
                "policy changed {}",
                truncate_session_view_text(event_id, 16)
            )
        }
        sigil_kernel::VerificationStaleReason::EnvironmentChanged(event_id) => {
            format!(
                "environment changed {}",
                truncate_session_view_text(event_id, 16)
            )
        }
        sigil_kernel::VerificationStaleReason::SandboxChanged(event_id) => {
            format!(
                "sandbox changed {}",
                truncate_session_view_text(event_id, 16)
            )
        }
        sigil_kernel::VerificationStaleReason::TrustChanged(event_id) => {
            format!(
                "workspace trust changed {}",
                truncate_session_view_text(event_id, 16)
            )
        }
        sigil_kernel::VerificationStaleReason::UnknownDirty(event_id) => {
            format!(
                "unknown workspace change {}",
                truncate_session_view_text(event_id, 16)
            )
        }
    }
}

fn child_merge_recheck_summary(
    entries: &[SessionLogEntry],
    task: &TaskRunProjection,
    readiness: &ReadinessEvaluatedEntry,
    max_chars: usize,
) -> Option<String> {
    let merge_event_id = readiness
        .evaluation
        .reasons
        .iter()
        .find_map(workspace_changed_event_id)?;
    let link = latest_child_verification_link_for_merge(entries, merge_event_id)?;
    let child = child_session_for_link(task, link);
    let child_label = child
        .map(|child| {
            let name = task
                .display_name_for_child_session(child)
                .unwrap_or_else(|| child.child_task_id.as_str());
            format!("{name} {}", task_child_session_status_label(child.status))
        })
        .unwrap_or_else(|| {
            format!(
                "child {}",
                truncate_session_view_text(&link.child_session_id, 16)
            )
        });
    Some(truncate_session_view_text(
        &format!("{child_label}; run parent check"),
        max_chars,
    ))
}

fn workspace_changed_event_id(reason: &sigil_kernel::ReadinessReason) -> Option<&str> {
    let sigil_kernel::ReadinessReason::VerificationStale(cause) = reason else {
        return None;
    };
    match &cause.reason {
        sigil_kernel::VerificationStaleReason::WorkspaceChanged(event_id) => Some(event_id),
        sigil_kernel::VerificationStaleReason::UnknownDirty(event_id) => Some(event_id),
        sigil_kernel::VerificationStaleReason::CheckSpecChanged(_)
        | sigil_kernel::VerificationStaleReason::PolicyChanged(_)
        | sigil_kernel::VerificationStaleReason::EnvironmentChanged(_)
        | sigil_kernel::VerificationStaleReason::SandboxChanged(_)
        | sigil_kernel::VerificationStaleReason::TrustChanged(_) => None,
    }
}

fn latest_child_verification_link_for_merge<'a>(
    entries: &'a [SessionLogEntry],
    merge_event_id: &str,
) -> Option<&'a ChildVerificationReceiptLinked> {
    entries.iter().rev().find_map(|entry| {
        let SessionLogEntry::Control(sigil_kernel::ControlEntry::ChildVerificationReceiptLinked(
            link,
        )) = entry
        else {
            return None;
        };
        link.merge_event_id
            .as_deref()
            .is_some_and(|event_id| event_id == merge_event_id)
            .then_some(link)
    })
}

fn child_session_for_link<'a>(
    task: &'a TaskRunProjection,
    link: &ChildVerificationReceiptLinked,
) -> Option<&'a TaskChildSessionEntry> {
    let matching = task
        .child_sessions
        .values()
        .filter(|child| child_session_matches_link(child, link))
        .collect::<Vec<_>>();
    if matching.len() == 1 {
        return matching.into_iter().next();
    }
    if task.child_sessions.len() == 1 {
        return task.child_sessions.values().next();
    }
    None
}

fn child_session_matches_link(
    child: &TaskChildSessionEntry,
    link: &ChildVerificationReceiptLinked,
) -> bool {
    let path = child.child_session_ref.as_path().to_string_lossy();
    path.contains(&link.child_session_id)
}

fn required_action_label(action: &RequiredAction) -> String {
    match action {
        RequiredAction::RunCheck { check_spec_id } => format!("run check {check_spec_id}"),
        RequiredAction::ApproveCheckExecution { check_spec_id } => {
            format!("check approval {check_spec_id}")
        }
        RequiredAction::TrustWorkspace => "workspace trust required".to_owned(),
        RequiredAction::ResolveUnknownDirty => "refresh source or run check".to_owned(),
        RequiredAction::ReRunNonWritingCheck { check_spec_id } => {
            format!("rerun non-writing check {check_spec_id}")
        }
        RequiredAction::ReviewVerificationFailure { receipt_id } => {
            format!("review verification failure {receipt_id}")
        }
        RequiredAction::ProvideVerificationConfig => "verification config required".to_owned(),
    }
}

fn latest_check_run_for_actions<'a>(
    entries: &'a [SessionLogEntry],
    scope: &EvidenceScope,
    actions: &[RequiredAction],
) -> Option<&'a VerificationCheckRunEntry> {
    actions
        .iter()
        .find_map(|action| latest_check_run_for_action(entries, scope, action))
}

fn latest_check_run_for_action<'a>(
    entries: &'a [SessionLogEntry],
    scope: &EvidenceScope,
    action: &RequiredAction,
) -> Option<&'a VerificationCheckRunEntry> {
    let check_spec_id = required_action_check_spec_id(action)?;
    entries.iter().rev().find_map(|entry| {
        let SessionLogEntry::Control(sigil_kernel::ControlEntry::VerificationCheckRun(run)) = entry
        else {
            return None;
        };
        (run.scope == *scope && run.check_spec_id == check_spec_id).then_some(run)
    })
}

fn required_action_check_spec_id(action: &RequiredAction) -> Option<&str> {
    match action {
        RequiredAction::RunCheck { check_spec_id }
        | RequiredAction::ApproveCheckExecution { check_spec_id }
        | RequiredAction::ReRunNonWritingCheck { check_spec_id } => Some(check_spec_id),
        RequiredAction::TrustWorkspace
        | RequiredAction::ResolveUnknownDirty
        | RequiredAction::ReviewVerificationFailure { .. }
        | RequiredAction::ProvideVerificationConfig => None,
    }
}

fn check_run_status_label(status: VerificationCheckRunStatus) -> &'static str {
    match status {
        VerificationCheckRunStatus::Queued => "queued",
        VerificationCheckRunStatus::Running => "running",
        VerificationCheckRunStatus::Succeeded => "succeeded",
        VerificationCheckRunStatus::Failed => "failed",
        VerificationCheckRunStatus::Skipped => "skipped",
        VerificationCheckRunStatus::Inconclusive => "inconclusive",
        VerificationCheckRunStatus::Errored => "errored",
    }
}

fn check_run_status_blocks_action(status: VerificationCheckRunStatus) -> bool {
    matches!(
        status,
        VerificationCheckRunStatus::Queued | VerificationCheckRunStatus::Running
    )
}

fn task_step_scope(task: &TaskRunProjection, step_id: &TaskStepId) -> EvidenceScope {
    EvidenceScope::Step(format!("{}:{}", task.task_id.as_str(), step_id.as_str()))
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

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/task_sidebar_tests.rs"]
mod tests;
