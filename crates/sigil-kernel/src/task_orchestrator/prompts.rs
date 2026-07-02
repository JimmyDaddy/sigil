use super::*;

pub(super) fn planner_prompt(objective: &str) -> String {
    format!(
        "Create an executable plan for this task. Call task_plan_update with an accepted plan before any execution. After task_plan_update succeeds, stop; do not inspect files, execute steps, or summarize execution progress. Do not call a task or subagent tool. Use role executor for ordinary main-session reads and edits, including sequential_workspace_write steps. To delegate read-only research or verification, add role subagent_read steps. Use role subagent_write only for delegated changeset-only write proposals with isolation changeset_only; do not pair subagent_write with sequential_workspace_write. If the objective contains a user-approved plan, preserve its stated scope and order; only add, remove, or reorder steps when needed for correctness, and include the reason in the affected step detail.\n\nObjective:\n{objective}"
    )
}

pub(super) fn normalize_task_guidance(guidance: Option<String>) -> Option<String> {
    guidance
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

pub(super) fn task_continue_reason(plan_version: u32, guidance: Option<&str>) -> String {
    match guidance {
        Some(value) => format!(
            "continuing plan v{plan_version}; user guidance: {}",
            value.trim()
        ),
        None => format!("continuing plan v{plan_version}"),
    }
}

pub(super) fn executor_step_prompt(
    objective: &str,
    plan_version: u32,
    step: &TaskStepSpec,
    guidance: Option<&str>,
) -> String {
    role_step_prompt(
        "Execute task step.",
        objective,
        plan_version,
        step,
        guidance,
    )
}

pub(super) fn subagent_step_prompt(
    objective: &str,
    plan_version: u32,
    step: &TaskStepSpec,
    guidance: Option<&str>,
) -> String {
    role_step_prompt(
        "Execute this delegated subagent step in the child session. Keep output bounded and focused on the step result.",
        objective,
        plan_version,
        step,
        guidance,
    )
}

pub(super) fn role_step_prompt(
    heading: &str,
    objective: &str,
    plan_version: u32,
    step: &TaskStepSpec,
    guidance: Option<&str>,
) -> String {
    let detail = step
        .detail
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("-");
    let mut prompt = format!(
        "{heading}\n\nObjective:\n{objective}\nPlan version: {plan_version}\nStep: {}\nTitle: {}\nDetail: {detail}\nRole: {}",
        step.step_id.as_str(),
        step.title,
        step.role.as_str()
    );
    if let Some(guidance) = guidance.filter(|value| !value.trim().is_empty()) {
        prompt.push_str("\n\nUser guidance for this continuation:\n");
        prompt.push_str(guidance.trim());
    }
    prompt
}
