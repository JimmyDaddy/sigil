use anyhow::{Result, anyhow};
use sigil_kernel::{
    ControlEntry, RunCancellationOwner, Session, SessionLogEntry, SessionRef, TaskId, TaskRunEntry,
    TaskRunStatus,
};

use super::{finalize_task_root, resolve_task_continuation};

#[test]
fn shared_task_continuation_resolves_exact_or_latest_non_terminal_task() -> Result<()> {
    let parent_session_ref = SessionRef::new_relative("parent.jsonl")?;
    let mut session = Session::new("provider", "model");
    for (id, status) in [
        ("task-1", TaskRunStatus::Completed),
        ("task-2", TaskRunStatus::Paused),
    ] {
        session.append_control(ControlEntry::TaskRun(TaskRunEntry {
            task_id: TaskId::new(id)?,
            parent_session_ref: parent_session_ref.clone(),
            objective: format!("objective {id}"),
            status,
            reason: None,
        }))?;
    }

    let latest = resolve_task_continuation(&session, None)?;
    assert_eq!(latest.task_id.as_str(), "task-2");
    assert_eq!(latest.parent_session_ref, parent_session_ref);
    assert_eq!(latest.objective, "objective task-2");
    assert!(latest.needs_planning);

    let exact = resolve_task_continuation(&session, Some("task-2"))?;
    assert_eq!(exact, latest);
    assert!(
        resolve_task_continuation(&session, Some("task-1"))
            .expect_err("completed task should reject continuation")
            .to_string()
            .contains("already completed")
    );
    Ok(())
}

#[test]
fn failed_shared_task_execution_closes_started_task_once() -> Result<()> {
    let task_id = TaskId::new("task-1")?;
    let parent_session_ref = SessionRef::new_relative("parent.jsonl")?;
    let mut session = Session::new("provider", "model");
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref: parent_session_ref.clone(),
        objective: "ship shared runtime".to_owned(),
        status: TaskRunStatus::Started,
        reason: None,
    }))?;
    let cancellation = RunCancellationOwner::new().handle();

    let result = finalize_task_root(
        &mut session,
        &task_id,
        &parent_session_ref,
        "ship shared runtime",
        &cancellation,
        Err(anyhow!("planner failed")),
    );

    assert!(result.is_err());
    let task_runs = session
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::TaskRun(entry)) => Some(entry),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(task_runs.len(), 2);
    assert_eq!(task_runs[1].status, TaskRunStatus::Failed);
    assert!(
        task_runs[1]
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("planner failed"))
    );
    Ok(())
}

#[test]
fn successful_shared_task_execution_claims_natural_root_terminal() -> Result<()> {
    let task_id = TaskId::new("task-1")?;
    let parent_session_ref = SessionRef::new_relative("parent.jsonl")?;
    let mut session = Session::new("provider", "model");
    let cancellation = RunCancellationOwner::new().handle();

    let status = finalize_task_root(
        &mut session,
        &task_id,
        &parent_session_ref,
        "ship shared runtime",
        &cancellation,
        Ok(TaskRunStatus::Completed),
    )?;

    assert_eq!(status, TaskRunStatus::Completed);
    assert!(cancellation.is_naturally_finalized());
    assert!(session.entries().is_empty());
    Ok(())
}
