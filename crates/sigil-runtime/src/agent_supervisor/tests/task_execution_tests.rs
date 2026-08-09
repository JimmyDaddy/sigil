use anyhow::{Result, anyhow};
use sigil_kernel::{
    AgentRole, ControlEntry, RunCancellationOwner, RunCancellationTarget, Session, SessionLogEntry,
    SessionRef, TaskId, TaskPauseRequest, TaskPlanEntry, TaskPlanStatus,
    TaskRunCancellationScopeBoundEntry, TaskRunEntry, TaskRunStatus, TaskStepEntry, TaskStepId,
    TaskStepStatus,
};

use super::{
    TaskPauseValidationError, TaskStopDisposition, append_explicit_task_run_target,
    append_task_stop_state, finalize_task_root, prepare_task_run_cancellation,
    resolve_task_continuation, validate_task_pause_request,
};

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
            title: None,
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
fn shared_task_cancellation_scope_is_bound_before_dispatch() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let store = sigil_kernel::JsonlSessionStore::new(directory.path().join("session.jsonl"))?;
    let mut session = Session::new("provider", "model").with_store(store);
    let task_id = TaskId::new("task-1")?;

    let prepared = prepare_task_run_cancellation(&mut session, &task_id)?;

    let binding = session
        .entries()
        .iter()
        .find_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::TaskRunCancellationScopeBound(binding)) => {
                Some(binding)
            }
            _ => None,
        })
        .expect("cancellation binding should be durable before dispatch");
    assert_eq!(binding.task_id, task_id);
    assert_eq!(binding.run_scope_id, prepared.handle.scope_id());
    assert_eq!(
        prepared.owner.handle().scope_id(),
        prepared.handle.scope_id()
    );
    let _durable_recorder = prepared.recorder;
    drop(prepared.task_guard);
    Ok(())
}

#[test]
fn explicit_task_run_target_restores_focus_once_for_the_exact_bound_scope() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let store = sigil_kernel::JsonlSessionStore::new(directory.path().join("session.jsonl"))?;
    let mut session = Session::new("provider", "model").with_store(store);
    let task_id = TaskId::new("task-explicit-focus")?;
    session.append_controls(vec![
        ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
            objective: "continue exact task".to_owned(),
            title: None,
            status: TaskRunStatus::Started,
            reason: None,
        }),
        ControlEntry::TaskPlan(TaskPlanEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            status: TaskPlanStatus::Accepted,
            steps: Vec::new(),
            reason: None,
        }),
        ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
            objective: "continue exact task".to_owned(),
            title: None,
            status: TaskRunStatus::Paused,
            reason: None,
        }),
    ])?;
    session.append_user_message(sigil_kernel::ModelMessage::user("unrelated chat"))?;
    assert!(session.task_state_projection().current_task().is_none());
    let prepared = prepare_task_run_cancellation(&mut session, &task_id)?;
    let mut handler = sigil_kernel::NoopEventHandler;

    append_explicit_task_run_target(
        &mut session,
        &mut handler,
        &task_id,
        prepared.handle.scope_id(),
    )?;
    append_explicit_task_run_target(
        &mut session,
        &mut handler,
        &task_id,
        prepared.handle.scope_id(),
    )?;

    assert_eq!(
        session
            .task_state_projection()
            .current_task()
            .map(|task| &task.task_id),
        Some(&task_id)
    );
    assert_eq!(
        session
            .entries()
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::TaskRunTargetSelected(_))
            ))
            .count(),
        1
    );
    assert!(
        append_explicit_task_run_target(
            &mut session,
            &mut handler,
            &task_id,
            "another-unbound-scope",
        )
        .is_err()
    );
    drop(prepared.task_guard);
    Ok(())
}

#[test]
fn shared_task_pause_validation_binds_exact_plan_and_active_scope() -> Result<()> {
    let task_id = TaskId::new("task-pause")?;
    let scope_id = "scope-active";
    let mut session = Session::new("provider", "model");
    session.append_controls(vec![
        ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
            objective: "pause exact task".to_owned(),
            title: None,

            status: TaskRunStatus::Running,
            reason: None,
        }),
        ControlEntry::TaskPlan(TaskPlanEntry {
            task_id: task_id.clone(),
            plan_version: 2,
            status: TaskPlanStatus::Accepted,
            steps: Vec::new(),
            reason: None,
        }),
        ControlEntry::TaskRunCancellationScopeBound(TaskRunCancellationScopeBoundEntry {
            task_id: task_id.clone(),
            run_scope_id: scope_id.to_owned(),
        }),
    ])?;
    let request = TaskPauseRequest::new(task_id.clone(), 2);

    validate_task_pause_request(
        &request,
        &RunCancellationTarget::Run,
        scope_id,
        session.entries(),
    )?;
    let stale = TaskPauseRequest::new(task_id, 1);
    assert_eq!(
        validate_task_pause_request(
            &stale,
            &RunCancellationTarget::Run,
            scope_id,
            session.entries(),
        ),
        Err(TaskPauseValidationError::PlanChanged)
    );
    Ok(())
}

#[test]
fn shared_task_stop_transition_closes_steps_before_task_in_one_writer_batch() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let store = sigil_kernel::JsonlSessionStore::new(directory.path().join("session.jsonl"))?;
    let mut session = Session::new("provider", "model").with_store(store);
    let task_id = TaskId::new("task-stop")?;
    let step_id = TaskStepId::new("step-1")?;
    session.append_controls(vec![
        ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
            objective: "stop exact task".to_owned(),
            title: None,

            status: TaskRunStatus::Running,
            reason: None,
        }),
        ControlEntry::TaskStep(TaskStepEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            step_id: step_id.clone(),
            role: AgentRole::Executor,
            status: TaskStepStatus::Running,
            title: Some("active step".to_owned()),
            summary: None,
            reason: None,
        }),
    ])?;

    let appended = append_task_stop_state(
        &mut session,
        Some(&task_id),
        TaskStopDisposition::Paused,
        "pause after quiescence",
    )?
    .expect("exact running task should append a stop transition");

    assert_eq!(appended.task_id(), &task_id);
    assert_eq!(appended.status(), TaskRunStatus::Paused);
    assert!(matches!(
        appended.controls(),
        [
            ControlEntry::TaskStep(TaskStepEntry {
                step_id: stopped_step,
                status: TaskStepStatus::Interrupted,
                ..
            }),
            ControlEntry::TaskRun(TaskRunEntry {

                status: TaskRunStatus::Paused,
                ..
            }),
        ] if stopped_step == &step_id
    ));
    let projection = session.task_state_projection();
    let task = projection.tasks.get(&task_id).expect("paused task");
    assert_eq!(task.status, TaskRunStatus::Paused);
    assert_eq!(
        task.steps
            .values()
            .find(|step| step.step_id == step_id)
            .expect("stopped step")
            .status,
        TaskStepStatus::Interrupted
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
        title: None,

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
