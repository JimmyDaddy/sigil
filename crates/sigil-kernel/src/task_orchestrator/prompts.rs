use super::*;

pub(super) fn planner_prompt(objective: &str) -> String {
    format!(
        "Create an executable plan for this task. Call task_plan_update with an accepted plan before any execution. After task_plan_update succeeds, stop; do not inspect files, execute steps, or summarize execution progress. Do not call a task or subagent tool. Use role executor for ordinary task-participant reads and edits, including sequential_workspace_write steps. To delegate read-only research or verification, add role subagent_read steps. Use role subagent_write only for delegated changeset-only write proposals with isolation changeset_only; do not pair subagent_write with sequential_workspace_write. If the objective contains a user-approved plan, preserve its stated scope and order; only add, remove, or reorder steps when needed for correctness, and include the reason in the affected step detail.\n\nObjective:\n{objective}"
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

pub(super) fn task_synthesis_prompt(
    session: &Session,
    request: &SequentialTaskRequest,
    plan_version: u32,
) -> Result<String> {
    let projection = session.task_state_projection();
    let task = projection
        .tasks
        .get(&request.task_id)
        .ok_or_else(|| anyhow!("task is missing before synthesis prompt assembly"))?;
    let plan = task
        .plans
        .get(&plan_version)
        .filter(|plan| plan.status == TaskPlanStatus::Accepted)
        .ok_or_else(|| anyhow!("task has no accepted plan for final synthesis"))?;
    if task.participant_attempts.values().any(|attempt| {
        attempt.status == TaskParticipantAttemptStatus::Started
            && attempt.purpose != TaskParticipantPurpose::Synthesis
    }) {
        bail!("task still has an active participant before final synthesis");
    }

    let mut results = Vec::new();
    for step in &plan.steps {
        let step_projection = task
            .steps
            .get(&(plan_version, step.step_id.clone()))
            .ok_or_else(|| {
                anyhow!(
                    "task step {} has no terminal projection",
                    step.step_id.as_str()
                )
            })?;
        if step_projection.status != TaskStepStatus::Completed {
            bail!(
                "task step {} is not completed before final synthesis",
                step.step_id.as_str()
            );
        }
        let result = task
            .participant_attempts_for(
                TaskParticipantPurpose::Step,
                Some(plan_version),
                Some(&step.step_id),
            )
            .into_iter()
            .rev()
            .find_map(|attempt| task.participant_results.get(&attempt.attempt_id))
            .map(|result| {
                let result_ref = result
                    .final_answer_ref
                    .as_ref()
                    .map(|reference| {
                        format!(
                            "{}#{}",
                            reference.session_ref.as_path().display(),
                            reference.message_id
                        )
                    })
                    .unwrap_or_else(|| "-".to_owned());
                format!(
                    "- {} [{}]\n  result_ref: {}\n  summary: {}",
                    step.title,
                    step.step_id.as_str(),
                    result_ref,
                    result.summary
                )
            })
            .unwrap_or_else(|| {
                format!(
                    "- {} [{}]\n  result_ref: legacy\n  summary: {}",
                    step.title,
                    step.step_id.as_str(),
                    step_projection.summary.as_deref().unwrap_or("completed")
                )
            });
        results.push(result);
    }

    Ok(format!(
        "Produce the single user-visible final answer for this completed task. Use only the immutable objective, accepted plan, and bounded participant results below. Do not claim work that the results do not support. Do not call tools, modify files, create another task, or expose internal participant identifiers. Keep the answer concise and under 4000 characters.\n\nObjective:\n{}\n\nAccepted plan v{}:\n{}\n\nParticipant results:\n{}",
        request.objective,
        plan_version,
        plan.steps
            .iter()
            .map(|step| format!("- {} [{}]", step.title, step.step_id.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        results.join("\n")
    ))
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
