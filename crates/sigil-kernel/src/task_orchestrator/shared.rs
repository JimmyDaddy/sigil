use super::*;

pub(super) fn append_task_control<H>(
    session: &mut Session,
    handler: &mut H,
    control: ControlEntry,
) -> Result<()>
where
    H: EventHandler + Send,
{
    handler.commit_controls(session, vec![control]).map(|_| ())
}

pub(super) fn append_task_controls<H>(
    session: &mut Session,
    handler: &mut H,
    controls: Vec<ControlEntry>,
) -> Result<()>
where
    H: EventHandler + Send,
{
    // A multi-control task transition is one recovery contract. The event-handler commit
    // boundary coordinates the session writer's crash-safe bundle intent with matching delivery.
    handler.commit_controls(session, controls).map(|_| ())
}

pub(super) fn append_task_control_with_event<H>(
    session: &mut Session,
    handler: &mut H,
    control: ControlEntry,
) -> Result<Option<StoredEvent>>
where
    H: EventHandler + Send,
{
    let mut events = handler.commit_controls(session, vec![control])?;
    Ok(events.pop())
}

pub(super) fn append_task_run<H>(
    session: &mut Session,
    handler: &mut H,
    request: &DirectTaskRequest,
    status: TaskRunStatus,
    reason: Option<String>,
) -> Result<()>
where
    H: EventHandler + Send,
{
    let title = session
        .task_state_projection()
        .tasks
        .get(&request.task_id)
        .and_then(|task| task.title.clone())
        .unwrap_or_else(|| crate::task_semantic_title(&request.objective));
    append_task_control(
        session,
        handler,
        ControlEntry::TaskRun(TaskRunEntry {
            task_id: request.task_id.clone(),
            parent_session_ref: request.parent_session_ref.clone(),
            objective: crate::safe_persistence_text(&request.objective),
            title: Some(title),
            status,
            reason: reason.as_deref().map(crate::safe_persistence_text),
        }),
    )
}
