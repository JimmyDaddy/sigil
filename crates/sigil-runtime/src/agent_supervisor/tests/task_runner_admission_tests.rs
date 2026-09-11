use super::*;
use sigil_kernel::{
    TaskParticipantAttemptEntry, TaskParticipantAttemptStatus, TaskParticipantContext,
    TaskParticipantPurpose, TaskRunEntry, task_participant_attempt_id,
};

fn unscoped_child_request() -> Result<TaskChildSessionRunRequest> {
    let task_id = TaskId::new("ordinary-child-fixture")?;
    let step_id = TaskStepId::new("read")?;
    let attempt_id = task_participant_attempt_id(
        &task_id,
        TaskParticipantPurpose::Step,
        Some(1),
        Some(&step_id),
        1,
    )?;
    Ok(TaskChildSessionRunRequest {
        task: SequentialTaskRequest {
            task_id: task_id.clone(),
            parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
            objective: "Run an ordinary supervisor child".to_owned(),
        },
        plan_version: 1,
        step: TaskStepSpec {
            step_id,
            title: "Read".to_owned(),
            display_name: None,
            detail: None,
            role: AgentRole::SubagentRead,
            depends_on: Vec::new(),
            intent_refs: Vec::new(),
            mode: Some(TaskStepMode::Read),
            isolation: None,
        },
        child_session_ref: sigil_kernel::task_participant_session_ref(&task_id, &attempt_id)?,
        child_input: AgentRunInput::without_persisted_user_message(Vec::new())
            .with_logical_run_id(task_participant_logical_run_id(&attempt_id)),
        attempt_id,
        options: AgentRunOptions {
            workspace_root: std::path::PathBuf::from("."),
            max_turns: Some(1),
            tool_timeout_secs: 1,
            reasoning_effort: None,
            traffic_partition_key: None,
            interaction_mode: InteractionMode::Headless,
            permission_config: sigil_kernel::PermissionConfig::default(),
            permission_context: Default::default(),
            permission_mode_override: None,
            memory_config: sigil_kernel::MemoryConfig::with_enabled(false),
            compaction_config: Default::default(),
            tool_authority: None,
        },
        isolated_base_snapshot_id: None,
    })
}

fn participant_fixture() -> Result<(
    Session,
    TaskChildSessionRunRequest,
    TaskParticipantAttemptEntry,
)> {
    let mut parent = Session::new("fixture", "model");
    let mut request = unscoped_child_request()?;
    request.child_input.purpose = Some(sigil_kernel::AgentRunPurpose::TaskParticipant(
        TaskParticipantContext {
            task_id: request.task.task_id.clone(),
            plan_version: request.plan_version,
            step_id: request.step.step_id.clone(),
            attempt_id: request.attempt_id.clone(),
        },
    ));
    let attempt = TaskParticipantAttemptEntry {
        attempt_id: request.attempt_id.clone(),
        task_id: request.task.task_id.clone(),
        purpose: TaskParticipantPurpose::Step,
        ordinal: 1,
        plan_version: Some(request.plan_version),
        step_id: Some(request.step.step_id.clone()),
        role: request.step.role,
        child_session_ref: request.child_session_ref.clone(),
        status: TaskParticipantAttemptStatus::Started,
        reason: None,
    };
    parent.append_controls(vec![
        ControlEntry::TaskRun(TaskRunEntry {
            task_id: request.task.task_id.clone(),
            parent_session_ref: request.task.parent_session_ref.clone(),
            objective: request.task.objective.clone(),
            title: None,
            status: TaskRunStatus::Running,
            reason: None,
        }),
        ControlEntry::TaskParticipantAttempt(attempt.clone()),
    ])?;
    Ok((parent, request, attempt))
}

#[test]
fn ordinary_child_metadata_does_not_invent_a_task_admission() -> Result<()> {
    let parent = Session::new("fixture", "model");
    validate_task_participant_admission(&parent, &unscoped_child_request()?)?;
    assert!(parent.task_state_projection().tasks.is_empty());
    Ok(())
}

#[test]
fn explicit_participant_requires_matching_durable_task_admission() -> Result<()> {
    let (parent, mut request, _) = participant_fixture()?;
    validate_task_participant_admission(&parent, &request)?;
    let missing_parent = Session::new("fixture", "model");
    assert!(
        validate_task_participant_admission(&missing_parent, &request)
            .expect_err("an explicit participant requires its parent Task")
            .to_string()
            .contains("lost its parent Task")
    );
    request.plan_version += 1;
    assert!(
        validate_task_participant_admission(&parent, &request)
            .expect_err("input and request must name the same admitted scope")
            .to_string()
            .contains("does not match its admitted request")
    );
    Ok(())
}

#[test]
fn participant_admission_rejects_a_mismatched_current_attempt() -> Result<()> {
    for mismatch in 0..3 {
        let (source, mut request, mut attempt) = participant_fixture()?;
        match mismatch {
            0 => {
                attempt.purpose = TaskParticipantPurpose::Planner;
                attempt.plan_version = None;
                attempt.step_id = None;
                attempt.role = AgentRole::Planner;
            }
            1 => attempt.plan_version = Some(request.plan_version + 1),
            2 => attempt.step_id = Some(TaskStepId::new("another-step")?),
            _ => unreachable!("the fixture covers three identity mismatches"),
        }
        attempt.attempt_id = task_participant_attempt_id(
            &attempt.task_id,
            attempt.purpose,
            attempt.plan_version,
            attempt.step_id.as_ref(),
            attempt.ordinal,
        )?;
        attempt.child_session_ref =
            sigil_kernel::task_participant_session_ref(&attempt.task_id, &attempt.attempt_id)?;
        attempt.validate_shape()?;
        request.attempt_id = attempt.attempt_id.clone();
        request.child_session_ref = attempt.child_session_ref.clone();
        let Some(sigil_kernel::AgentRunPurpose::TaskParticipant(context)) =
            request.child_input.purpose.as_mut()
        else {
            unreachable!("the fixture creates a participant purpose");
        };
        context.attempt_id = request.attempt_id.clone();
        let mut parent = Session::new("fixture", "model");
        parent.append_controls(
            source
                .entries()
                .iter()
                .filter_map(|entry| match entry {
                    SessionLogEntry::Control(ControlEntry::TaskRun(run)) => {
                        Some(ControlEntry::TaskRun(run.clone()))
                    }
                    _ => None,
                })
                .collect(),
        )?;
        parent.append_control(ControlEntry::TaskParticipantAttempt(attempt.clone()))?;
        let projection = parent.task_state_projection();
        let task = &projection.tasks[&request.task.task_id];
        assert_eq!(task.participant_conflicts, 0);
        assert_eq!(task.participant_attempts[&request.attempt_id], attempt);
        assert!(
            validate_task_participant_admission(&parent, &request)
                .expect_err("the admitted attempt must match the participant scope")
                .to_string()
                .contains("lost its admitted attempt")
        );
    }
    Ok(())
}

#[test]
fn participant_admission_rejects_a_changed_child_identity() -> Result<()> {
    let (parent, mut request, _) = participant_fixture()?;
    request.child_session_ref = SessionRef::new_relative("another-child.jsonl")?;
    assert!(
        validate_task_participant_admission(&parent, &request)
            .expect_err("the current child must retain its durable identity")
            .to_string()
            .contains("current child identity changed")
    );
    Ok(())
}

#[test]
fn participant_admission_does_not_read_or_create_historical_children() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (source, mut request, mut current) = participant_fixture()?;
    let mut older = current.clone();
    older.status = TaskParticipantAttemptStatus::Interrupted;
    let historical_path = older.child_session_ref.resolve(temp.path());
    current.ordinal = 2;
    current.attempt_id = task_participant_attempt_id(
        &current.task_id,
        current.purpose,
        current.plan_version,
        current.step_id.as_ref(),
        current.ordinal,
    )?;
    current.child_session_ref =
        sigil_kernel::task_participant_session_ref(&current.task_id, &current.attempt_id)?;
    request.attempt_id = current.attempt_id.clone();
    request.child_session_ref = current.child_session_ref.clone();
    let Some(sigil_kernel::AgentRunPurpose::TaskParticipant(context)) =
        request.child_input.purpose.as_mut()
    else {
        unreachable!("the fixture creates a participant purpose");
    };
    context.attempt_id = request.attempt_id.clone();
    let mut parent = Session::new("fixture", "model")
        .with_store(JsonlSessionStore::new(temp.path().join("parent.jsonl"))?);
    parent.append_controls(
        source
            .entries()
            .iter()
            .filter_map(|entry| match entry {
                SessionLogEntry::Control(ControlEntry::TaskRun(run)) => {
                    Some(ControlEntry::TaskRun(run.clone()))
                }
                _ => None,
            })
            .collect(),
    )?;
    parent.append_controls(vec![
        ControlEntry::TaskParticipantAttempt(older),
        ControlEntry::TaskParticipantAttempt(current),
    ])?;
    let projection = parent.task_state_projection();
    let task = &projection.tasks[&request.task.task_id];
    assert_eq!(task.participant_conflicts, 0);
    assert_eq!(task.participant_attempts.len(), 2);
    validate_task_participant_admission(&parent, &request)?;
    assert!(!historical_path.exists());
    std::fs::create_dir_all(
        historical_path
            .parent()
            .expect("the child has a parent directory"),
    )?;
    std::fs::write(&historical_path, b"not a session record\n")?;
    validate_task_participant_admission(&parent, &request)?;
    assert_eq!(std::fs::read(historical_path)?, b"not a session record\n");
    Ok(())
}

#[test]
fn participant_admission_rejects_an_unregistered_attempt() -> Result<()> {
    let (parent, mut request, _) = participant_fixture()?;
    request.attempt_id = TaskParticipantAttemptId::new("unregistered-attempt")?;
    let Some(sigil_kernel::AgentRunPurpose::TaskParticipant(context)) =
        request.child_input.purpose.as_mut()
    else {
        unreachable!("the fixture creates a participant purpose");
    };
    context.attempt_id = request.attempt_id.clone();
    assert!(
        validate_task_participant_admission(&parent, &request)
            .expect_err("request identity does not replace a durable attempt")
            .to_string()
            .contains("lost its admitted attempt")
    );
    Ok(())
}
