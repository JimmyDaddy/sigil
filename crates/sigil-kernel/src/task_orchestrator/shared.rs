use super::*;

pub(super) fn append_task_control<H>(
    session: &mut Session,
    handler: &mut H,
    control: ControlEntry,
) -> Result<()>
where
    H: EventHandler + Send,
{
    session.append_control(control.clone())?;
    handler.handle(RunEvent::Control(control))
}

pub(super) fn append_task_run<H>(
    session: &mut Session,
    handler: &mut H,
    request: &SequentialTaskRequest,
    status: TaskRunStatus,
    reason: Option<String>,
) -> Result<()>
where
    H: EventHandler + Send,
{
    append_task_control(
        session,
        handler,
        ControlEntry::TaskRun(TaskRunEntry {
            task_id: request.task_id.clone(),
            parent_session_ref: request.parent_session_ref.clone(),
            objective: crate::safe_persistence_text(&request.objective),
            status,
            reason: reason.as_deref().map(crate::safe_persistence_text),
        }),
    )
}

pub(super) fn append_task_step<H>(
    session: &mut Session,
    handler: &mut H,
    task_id: &TaskId,
    plan_version: u32,
    step: &TaskStepSpec,
    status: TaskStepStatus,
    summary: Option<String>,
    reason: Option<String>,
) -> Result<()>
where
    H: EventHandler + Send,
{
    append_task_control(
        session,
        handler,
        ControlEntry::TaskStep(TaskStepEntry {
            task_id: task_id.clone(),
            plan_version,
            step_id: step.step_id.clone(),
            role: step.role,
            status,
            title: Some(crate::safe_persistence_text(&step.title)),
            summary: summary.as_deref().map(crate::safe_persistence_text),
            reason: reason.as_deref().map(crate::safe_persistence_text),
        }),
    )
}

#[cfg(test)]
pub(super) fn route_id_for_call(
    task_id: &TaskId,
    step_id: &TaskStepId,
    call_id: &str,
) -> Result<TaskRouteId> {
    let mut hasher = Sha256::new();
    hasher.update(task_id.as_str().as_bytes());
    hasher.update(b":");
    hasher.update(step_id.as_str().as_bytes());
    hasher.update(b":");
    hasher.update(call_id.as_bytes());
    let digest = hasher.finalize();
    TaskRouteId::new(format!(
        "route_{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        digest[0], digest[1], digest[2], digest[3], digest[4], digest[5]
    ))
}

#[cfg(test)]
pub(super) fn hash_text(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    format!("{:x}", hasher.finalize())
}
