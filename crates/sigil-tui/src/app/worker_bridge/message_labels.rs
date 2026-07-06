use sigil_kernel::{ControlEntry, ConversationInputTarget};

pub(super) fn plan_approval_permission_label(
    permission: sigil_kernel::PlanApprovalPermission,
) -> &'static str {
    match permission {
        sigil_kernel::PlanApprovalPermission::Ask => "ask",
        sigil_kernel::PlanApprovalPermission::WorkspaceEdits => "workspace_edits",
    }
}

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
    }
}
