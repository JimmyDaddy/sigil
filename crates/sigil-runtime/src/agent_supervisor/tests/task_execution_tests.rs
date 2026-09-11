use anyhow::{Result, anyhow};
use sigil_kernel::{
    AgentRole, ControlEntry, ConversationTurnRef, NoopEventHandler, PlanId,
    ProviderTurnRecoveryTerminalDispositionV1, ProviderTurnRecoveryTerminalError,
    RunCancellationOwner, RunCancellationTarget, Session, SessionLogEntry, SessionRef,
    TaskContinuationControlKind, TaskContinuationSelectedEntry, TaskDirectExecutionAdmittedV1,
    TaskDirectExecutionAttemptV1, TaskId, TaskParticipantAttemptEntry,
    TaskParticipantAttemptStatus, TaskParticipantPurpose, TaskPauseRequest, TaskPlanEntry,
    TaskPlanStatus, TaskRunCancellationScopeBoundEntry, TaskRunEntry, TaskRunStatus, TaskStepEntry,
    TaskStepId, TaskStepMode, TaskStepSpec, TaskStepStatus,
    project_conversation_prompt_for_persistence, task_participant_attempt_id,
    task_participant_session_ref,
};

use super::{
    ResolvedTaskExecutionRoute, TaskExecutionPreflightError, TaskPauseValidationError,
    TaskStopDisposition, append_explicit_task_run_target, append_run_scoped_task_interruption,
    append_task_stop_state, finalize_task_root, prepare_task_run_cancellation,
    resolve_task_continuation, task_id_for_cancellation_scope,
    validate_continuation_guidance_authority, validate_task_pause_request,
};

#[test]
fn direct_task_accepts_exact_typed_follow_up_guidance() -> Result<()> {
    let guidance = "What is the current verification doing?";
    let projected = project_conversation_prompt_for_persistence(guidance);
    let selection = TaskContinuationSelectedEntry {
        task_id: TaskId::new("task-direct-guidance")?,
        source_turn: ConversationTurnRef::new(
            "session-direct-guidance",
            "message-direct-guidance",
            "run-direct-guidance",
        )?,
        plan_version: None,
        task_status: TaskRunStatus::Paused,
        plan_status: None,
        route_contract_fingerprint: "sha256:direct-guidance-route".to_owned(),
        control: TaskContinuationControlKind::ApplyCurrentRequestAsGuidance,
        prompt_hash: projected.prompt_hash,
        exact_prompt_required: projected.exact_prompt_required,
        guidance: projected.safe_prompt,
        selected_at_ms: 1,
    };

    assert!(!validate_continuation_guidance_authority(
        ResolvedTaskExecutionRoute::Direct,
        Some(guidance),
        None,
        Some(&selection),
    )?);
    Ok(())
}

#[test]
fn shared_task_continuation_resolves_exact_or_latest_non_terminal_task() -> Result<()> {
    let parent_session_ref = SessionRef::new_relative("parent.jsonl")?;
    let mut session = Session::new("provider", "model");
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: TaskId::new("task-1")?,
        parent_session_ref: parent_session_ref.clone(),
        objective: "objective task-1".to_owned(),
        title: None,
        status: TaskRunStatus::Started,
        reason: None,
    }))?;
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
    assert!(latest.needs_planning());

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
fn run_scoped_task_stop_selects_only_its_durable_binding() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let store = sigil_kernel::JsonlSessionStore::new(directory.path().join("session.jsonl"))?;
    let mut session = Session::new("provider", "model").with_store(store);
    let older_task = TaskId::new("task-older")?;
    let active_task = TaskId::new("task-active")?;
    let newer_task = TaskId::new("task-newer")?;
    for (task_id, scope_id) in [
        (&older_task, "scope-older"),
        (&active_task, "scope-active"),
        (&newer_task, "scope-newer"),
    ] {
        session.append_controls(vec![
            ControlEntry::TaskRun(TaskRunEntry {
                task_id: task_id.clone(),
                parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                objective: "interrupt only the bound task".to_owned(),
                title: None,
                status: TaskRunStatus::Running,
                reason: None,
            }),
            ControlEntry::TaskRunCancellationScopeBound(TaskRunCancellationScopeBoundEntry {
                task_id: task_id.clone(),
                run_scope_id: scope_id.to_owned(),
            }),
        ])?;
    }
    let entry_count = session.entries().len();
    for (target, scope_id) in [
        (RunCancellationTarget::Run, "scope-unbound"),
        (
            RunCancellationTarget::Task {
                task_id: older_task.as_str().to_owned(),
            },
            "scope-active",
        ),
        (
            RunCancellationTarget::AgentThread {
                thread_id: "agent-thread".to_owned(),
            },
            "scope-active",
        ),
    ] {
        assert!(task_id_for_cancellation_scope(session.entries(), &target, scope_id).is_none());
    }
    assert_eq!(session.entries().len(), entry_count);
    let selected = task_id_for_cancellation_scope(
        session.entries(),
        &RunCancellationTarget::Run,
        "scope-active",
    )
    .expect("the active root scope should resolve its exact task");
    assert_eq!(selected, active_task);
    append_run_scoped_task_interruption(
        &mut session,
        &mut NoopEventHandler,
        "scope-active",
        "task run stopped after root quiescence",
    )?;
    let projection = session.task_state_projection();
    assert_eq!(
        projection.tasks[&active_task].status,
        TaskRunStatus::Interrupted
    );
    assert_eq!(projection.tasks[&older_task].status, TaskRunStatus::Running);
    assert_eq!(projection.tasks[&newer_task].status, TaskRunStatus::Running);
    Ok(())
}

#[test]
fn run_scoped_task_stop_preserves_outcomes_committed_during_cancellation() -> Result<()> {
    for status in [
        TaskRunStatus::Paused,
        TaskRunStatus::Completed,
        TaskRunStatus::Failed,
        TaskRunStatus::Cancelled,
        TaskRunStatus::Interrupted,
    ] {
        let mut session = Session::new("provider", "model");
        let task_id = TaskId::new("task-settled")?;
        session.append_control(ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
            objective: "preserve the settled outcome".to_owned(),
            title: None,
            status: TaskRunStatus::Started,
            reason: None,
        }))?;
        if status == TaskRunStatus::Completed {
            let admission = TaskDirectExecutionAdmittedV1::planner_fallback(
                task_id.clone(),
                "preserve the settled outcome",
                "planner-attempt-settled",
                1,
            );
            let mut attempt = TaskDirectExecutionAttemptV1::started(&admission, 1);
            session.append_controls(vec![
                ControlEntry::TaskDirectExecutionAdmittedV1(admission),
                ControlEntry::TaskDirectExecutionAttemptV1(attempt.clone()),
            ])?;
            let text = "direct execution completed";
            let message = sigil_kernel::ModelMessage::assistant_with_kind(
                Some(text.to_owned()),
                Vec::new(),
                sigil_kernel::AssistantMessageKind::FinalAnswer,
            );
            attempt.status = TaskParticipantAttemptStatus::Completed;
            attempt.final_message_id = Some(message.id.clone());
            attempt.output_hash = Some(format!(
                "sha256:{}",
                sigil_kernel::sha256_hex(text.as_bytes())
            ));
            session.append_assistant_message(message)?;
            session.append_control(ControlEntry::TaskDirectExecutionAttemptV1(attempt))?;
        }
        session.append_controls(vec![
            ControlEntry::TaskRun(TaskRunEntry {
                task_id: task_id.clone(),
                parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                objective: "preserve the settled outcome".to_owned(),
                title: None,
                status,
                reason: Some("settled while cancellation drained".to_owned()),
            }),
            ControlEntry::TaskRunCancellationScopeBound(TaskRunCancellationScopeBoundEntry {
                task_id: task_id.clone(),
                run_scope_id: "scope-stopping".to_owned(),
            }),
        ])?;
        let before = serde_json::to_value(session.entries())?;
        assert!(
            append_run_scoped_task_interruption(
                &mut session,
                &mut NoopEventHandler,
                "scope-stopping",
                "root quiescence confirmed",
            )?
            .is_none()
        );
        assert_eq!(serde_json::to_value(session.entries())?, before);
        assert_eq!(
            session.task_state_projection().tasks[&task_id].status,
            status
        );
    }
    Ok(())
}

#[test]
fn run_scoped_task_stop_rejects_a_binding_without_its_task() -> Result<()> {
    let mut session = Session::new("provider", "model");
    session.append_control(ControlEntry::TaskRunCancellationScopeBound(
        TaskRunCancellationScopeBoundEntry {
            task_id: TaskId::new("task-missing")?,
            run_scope_id: "scope-stopping".to_owned(),
        },
    ))?;
    assert!(matches!(
        append_run_scoped_task_interruption(
            &mut session,
            &mut NoopEventHandler,
            "scope-stopping",
            "root quiescence confirmed",
        ),
        Err(super::TaskStopStateError::TaskUnavailable { .. })
    ));
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
        Err(TaskPauseValidationError::ExecutionAuthorityChanged)
    );
    Ok(())
}

#[test]
fn direct_task_pause_and_continuation_bind_the_admission_without_a_plan() -> Result<()> {
    let task_id = TaskId::new("task-direct-pause")?;
    let objective = "execute directly";
    let scope_id = "scope-direct";
    let admission = TaskDirectExecutionAdmittedV1::approved_plan(
        task_id.clone(),
        objective,
        PlanId::new("plan-direct")?,
        format!("sha256:{}", "a".repeat(64)),
        1,
    );
    let attempt = TaskDirectExecutionAttemptV1::started(&admission, 1);
    let mut session = Session::new("provider", "model");
    session.append_controls(vec![
        ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
            objective: objective.to_owned(),
            title: None,
            status: TaskRunStatus::Running,
            reason: None,
        }),
        ControlEntry::TaskDirectExecutionAdmittedV1(admission.clone()),
        ControlEntry::TaskDirectExecutionAttemptV1(attempt.clone()),
        ControlEntry::TaskRunCancellationScopeBound(TaskRunCancellationScopeBoundEntry {
            task_id: task_id.clone(),
            run_scope_id: scope_id.to_owned(),
        }),
    ])?;

    let continuation = resolve_task_continuation(&session, Some(task_id.as_str()))?;
    assert!(continuation.is_direct());
    assert!(!continuation.needs_planning());

    let request = TaskPauseRequest::direct(task_id.clone(), admission.admission_id.clone());
    validate_task_pause_request(
        &request,
        &RunCancellationTarget::Run,
        scope_id,
        session.entries(),
    )?;
    let mut handler = NoopEventHandler;
    let stopped = append_task_stop_state(
        &mut session,
        &mut handler,
        Some(&task_id),
        TaskStopDisposition::Paused,
        "paused from test",
    )?
    .expect("direct task stop should append");
    assert_eq!(stopped.status(), TaskRunStatus::Paused);
    let task = session
        .task_state_projection()
        .tasks
        .get(&task_id)
        .cloned()
        .expect("direct task remains durable");
    assert_eq!(task.status, TaskRunStatus::Paused);
    assert_eq!(
        task.direct_execution_attempts
            .get(&attempt.attempt_id)
            .map(|attempt| attempt.status),
        Some(TaskParticipantAttemptStatus::Interrupted)
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

    let mut handler = NoopEventHandler;
    let appended = append_task_stop_state(
        &mut session,
        &mut handler,
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
fn shared_task_cancellation_closes_dag_descendants_and_started_participants() -> Result<()> {
    let task_id = TaskId::new("task-stop-cancel")?;
    let cancelled_step = TaskStepId::new("cancelled")?;
    let dependent_step = TaskStepId::new("dependent")?;
    let participant_id = task_participant_attempt_id(
        &task_id,
        TaskParticipantPurpose::Step,
        Some(1),
        Some(&dependent_step),
        1,
    )?;
    let step = |step_id: TaskStepId, depends_on: Vec<TaskStepId>| TaskStepSpec {
        title: step_id.as_str().to_owned(),
        display_name: None,
        detail: None,
        step_id,
        role: AgentRole::SubagentRead,
        depends_on,
        intent_refs: Vec::new(),
        mode: Some(TaskStepMode::Read),
        isolation: None,
    };
    let mut session = Session::new("provider", "model");
    session.append_controls(vec![
        ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
            objective: "cancel a durable DAG".to_owned(),
            title: None,
            status: TaskRunStatus::Running,
            reason: None,
        }),
        ControlEntry::TaskPlan(TaskPlanEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            status: TaskPlanStatus::Accepted,
            steps: vec![
                step(cancelled_step.clone(), Vec::new()),
                step(dependent_step.clone(), vec![cancelled_step.clone()]),
            ],
            reason: None,
        }),
        ControlEntry::TaskStep(TaskStepEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            step_id: cancelled_step,
            role: AgentRole::SubagentRead,
            status: TaskStepStatus::Cancelled,
            title: Some("cancelled".to_owned()),
            summary: None,
            reason: Some("user cancellation".to_owned()),
        }),
        ControlEntry::TaskParticipantAttempt(TaskParticipantAttemptEntry {
            attempt_id: participant_id.clone(),
            task_id: task_id.clone(),
            purpose: TaskParticipantPurpose::Step,
            ordinal: 1,
            plan_version: Some(1),
            step_id: Some(dependent_step.clone()),
            role: AgentRole::SubagentRead,
            child_session_ref: task_participant_session_ref(&task_id, &participant_id)?,
            status: TaskParticipantAttemptStatus::Started,
            reason: None,
        }),
    ])?;

    let mut handler = NoopEventHandler;
    let appended = append_task_stop_state(
        &mut session,
        &mut handler,
        Some(&task_id),
        TaskStopDisposition::Cancelled,
        "user requested cancellation",
    )?
    .expect("exact running task should append its cancellation closure");

    assert!(appended.controls().iter().any(|control| {
        matches!(
            control,
            ControlEntry::TaskStep(TaskStepEntry {
                step_id,
                status: TaskStepStatus::Cancelled,
                ..
            }) if step_id == &dependent_step
        )
    }));
    assert!(appended.controls().iter().any(|control| {
        matches!(
            control,
            ControlEntry::TaskParticipantAttempt(TaskParticipantAttemptEntry {
                attempt_id,
                status: TaskParticipantAttemptStatus::Cancelled,
                ..
            }) if attempt_id == &participant_id
        )
    }));
    let projection = session.task_state_projection();
    let task = projection.tasks.get(&task_id).expect("cancelled task");
    assert_eq!(task.status, TaskRunStatus::Cancelled);
    assert_eq!(
        task.steps.get(&(1, dependent_step)).map(|step| step.status),
        Some(TaskStepStatus::Cancelled)
    );
    assert_eq!(
        task.participant_attempts
            .get(&participant_id)
            .map(|attempt| attempt.status),
        Some(TaskParticipantAttemptStatus::Cancelled)
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
fn zero_dispatch_task_preflight_blocker_pauses_without_persisting_private_error() -> Result<()> {
    let task_id = TaskId::new("task-preflight")?;
    let parent_session_ref = SessionRef::new_relative("parent.jsonl")?;
    let mut session = Session::new("provider", "model");
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref: parent_session_ref.clone(),
        objective: "execute after repairing provider configuration".to_owned(),
        title: None,
        status: TaskRunStatus::Started,
        reason: None,
    }))?;
    let cancellation = RunCancellationOwner::new().handle();

    let status = finalize_task_root(
        &mut session,
        &task_id,
        &parent_session_ref,
        "execute after repairing provider configuration",
        &cancellation,
        Err(anyhow::Error::new(
            TaskExecutionPreflightError::RoleRuntimeConstruction(anyhow!(
                "private endpoint and credential detail"
            )),
        )),
    )?;

    assert_eq!(status, TaskRunStatus::Paused);
    let task = session
        .task_state_projection()
        .tasks
        .get(&task_id)
        .cloned()
        .expect("preflight-blocked task remains durable");
    assert_eq!(task.status, TaskRunStatus::Paused);
    assert_eq!(
        task.reason.as_deref(),
        Some("task_role_runtime_preflight_blocked")
    );
    assert!(!format!("{task:?}").contains("private endpoint"));
    let entry_count = session.entries().len();
    finalize_task_root(
        &mut session,
        &task_id,
        &parent_session_ref,
        "execute after repairing provider configuration",
        &cancellation,
        Err(anyhow::Error::new(
            TaskExecutionPreflightError::RoleRuntimeConstruction(anyhow!(
                "the same unavailable executor"
            )),
        )),
    )?;
    assert_eq!(session.entries().len(), entry_count);
    Ok(())
}

#[test]
fn recovery_blocker_does_not_collapse_root_task_to_failed() -> Result<()> {
    let task_id = TaskId::new("task-recovery")?;
    let parent_session_ref = SessionRef::new_relative("parent.jsonl")?;
    let mut session = Session::new("provider", "model");
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref: parent_session_ref.clone(),
        objective: "resume exact provider frontier".to_owned(),
        title: None,
        status: TaskRunStatus::Running,
        reason: None,
    }))?;
    let cancellation = RunCancellationOwner::new().handle();

    let status = finalize_task_root(
        &mut session,
        &task_id,
        &parent_session_ref,
        "resume exact provider frontier",
        &cancellation,
        Err(anyhow::Error::new(ProviderTurnRecoveryTerminalError {
            disposition: ProviderTurnRecoveryTerminalDispositionV1::Paused,
            reason_code: "provider_retry_budget_exhausted",
        })),
    )?;

    assert_eq!(status, TaskRunStatus::Paused);
    let task_projection = session.task_state_projection();
    let task = task_projection
        .tasks
        .get(&task_id)
        .expect("task remains durable");
    assert_eq!(task.status, TaskRunStatus::Paused);
    assert!(
        task.steps
            .values()
            .all(|step| step.status != TaskStepStatus::Cancelled)
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

#[test]
fn root_finalizer_downgrades_incomplete_direct_completion_to_paused() -> Result<()> {
    let task_id = TaskId::new("task-direct-incomplete")?;
    let parent_session_ref = SessionRef::new_relative("parent.jsonl")?;
    let objective = "complete only after the direct attempt closes";
    let admission = TaskDirectExecutionAdmittedV1::planner_fallback(
        task_id.clone(),
        objective,
        "planner-attempt-incomplete",
        1,
    );
    let attempt = TaskDirectExecutionAttemptV1::started(&admission, 1);
    let mut session = Session::new("provider", "model");
    session.append_controls(vec![
        ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: parent_session_ref.clone(),
            objective: objective.to_owned(),
            title: None,
            status: TaskRunStatus::Running,
            reason: None,
        }),
        ControlEntry::TaskDirectExecutionAdmittedV1(admission),
        ControlEntry::TaskDirectExecutionAttemptV1(attempt),
    ])?;
    let cancellation = RunCancellationOwner::new().handle();

    let status = finalize_task_root(
        &mut session,
        &task_id,
        &parent_session_ref,
        objective,
        &cancellation,
        Ok(TaskRunStatus::Completed),
    )?;

    assert_eq!(status, TaskRunStatus::Paused);
    let task = session
        .task_state_projection()
        .tasks
        .get(&task_id)
        .cloned()
        .expect("direct task remains resumable");
    assert_eq!(task.status, TaskRunStatus::Paused);
    assert_eq!(
        task.reason.as_deref(),
        Some("task completion blocked: unfinished_direct_execution")
    );
    Ok(())
}

#[test]
fn unplanned_task_accepts_guidance_without_an_accepted_plan_authority() -> Result<()> {
    assert!(validate_continuation_guidance_authority(
        ResolvedTaskExecutionRoute::NeedsPlanning,
        Some("Inspect the retry boundary first"),
        None,
        None,
    )?);
    Ok(())
}

#[test]
fn task_verification_uses_explicit_and_promoted_checks_without_repository_rediscovery() -> Result<()>
{
    let root = tempfile::tempdir()?;
    std::fs::write(root.path().join("package.json"), "{malformed")?;
    std::fs::create_dir_all(root.path().join(".github/workflows"))?;
    std::fs::write(root.path().join(".github/workflows/ci.yml"), "[malformed")?;
    let mut config: sigil_kernel::RootConfig =
        toml::from_str("config_version = 2\n[agent]\nmodel = \"test-model\"")?;
    config.verification.checks = vec![sigil_kernel::VerificationCheckConfig {
        id: "explicit-required".to_owned(),
        command: "true".to_owned(),
        args: Vec::new(),
        cwd: None,
        effect: sigil_kernel::ToolEffect::ReadOnly,
    }];
    let mut session = Session::new("test", "model");
    let promoted = sigil_kernel::CandidateCheck {
        source: sigil_kernel::CheckDiscoverySource::PackageScript,
        command: sigil_kernel::CheckCommand {
            command: "false".to_owned(),
            args: Vec::new(),
            cwd: None,
        },
        source_event_id: "approved-package-check".to_owned(),
        workspace_trust_snapshot_id: "workspace-trust".to_owned(),
    }
    .promote(
        "promoted-required",
        sigil_kernel::DEFAULT_TASK_VERIFICATION_SCOPE_HASH,
        sigil_kernel::ToolEffect::ReadOnly,
        sigil_kernel::CheckPromotion::UserApproved {
            approval_event_id: "approval".to_owned(),
        },
    )?;
    session.append_control(ControlEntry::CheckSpecRecorded(
        sigil_kernel::CheckSpecRecordedEntry::new(
            sigil_kernel::EvidenceScope::Workspace(sigil_kernel::stable_workspace_id(root.path())?),
            promoted.clone(),
            "approved-package-check",
        ),
    ))?;
    let task_id = TaskId::new("verification-ablation")?;
    super::materialize_task_verification_config(
        &mut session,
        &mut NoopEventHandler,
        &config,
        root.path(),
        &task_id,
    )?;
    let entries = session.entries().len();
    super::materialize_task_verification_config(
        &mut session,
        &mut NoopEventHandler,
        &config,
        root.path(),
        &task_id,
    )?;
    assert_eq!(session.entries().len(), entries);
    let projection = session.verification_state_projection();
    let scope = sigil_kernel::EvidenceScope::Task(task_id.as_str().to_owned());
    let policy = &projection
        .latest_policy(&scope)
        .expect("required Task checks")
        .policy;
    assert_eq!(policy.required_checks.len(), 2);
    assert!(!policy.allow_unverified_completion);
    assert_eq!(
        policy.completion_criteria,
        sigil_kernel::CompletionCriteria::AllRequiredChecks
    );
    assert_eq!(
        projection
            .check_spec(&scope, "promoted-required")
            .expect("promoted spec")
            .trusted_check,
        promoted
    );
    config.verification.checks[0].id.clear();
    assert!(
        super::materialize_task_verification_config(
            &mut session,
            &mut NoopEventHandler,
            &config,
            root.path(),
            &task_id
        )
        .is_err()
    );
    assert_eq!(session.entries().len(), entries);
    Ok(())
}
