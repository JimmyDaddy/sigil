use std::collections::BTreeSet;

use anyhow::Result;
use sha2::{Digest, Sha256};

use crate::task::{TaskRootCompletionBlockerV1, TaskRootTerminalCandidateV1};
use crate::{
    AgentRole, AgentThreadId, ControlEntry, ConversationTurnRef, ModelMessage, Session,
    SessionLogEntry, SessionRef, TASK_AGENT_DISPLAY_NAME_MAX_CHARS,
    TASK_PARTICIPANT_RESULT_CHANGED_PATH_MAX_ITEMS, TaskApprovalRouteBinding,
    TaskChildSessionDisplayNameEntry, TaskChildSessionEntry, TaskChildSessionStatus,
    TaskContinuationSelectedEntry, TaskDirectExecutionAdmittedV1, TaskDirectExecutionAttemptV1,
    TaskExecutionAttemptStatus, TaskId, TaskIsolationMode, TaskParticipantAttemptEntry,
    TaskParticipantAttemptId, TaskParticipantAttemptStatus, TaskParticipantPurpose,
    TaskParticipantResultEntry, TaskPauseRequest, TaskPlanEntry, TaskPlanStatus, TaskRouteId,
    TaskRouteStatus, TaskRunCancellationScopeBoundEntry, TaskRunEntry, TaskRunStatus,
    TaskRunTargetSelectedEntry, TaskStateProjection, TaskStepEntry, TaskStepId, TaskStepMode,
    TaskStepSpec, TaskStepStatus, TaskSubagentApprovalRouteEntry,
    TaskSubagentElicitationRouteEntry, child_session_ref, derive_task_execution_segments,
    normalize_task_agent_display_name, project_conversation_prompt_for_persistence,
    stale_task_approval_routes_for_restore, task_participant_attempt_id,
    task_participant_session_ref, task_semantic_title, validate_task_plan_graph_steps,
};

#[test]
fn execution_segments_only_join_exact_linear_execution_contracts() -> Result<()> {
    let step = |id: &str, depends_on: Vec<TaskStepId>, isolation| -> Result<TaskStepSpec> {
        Ok(TaskStepSpec {
            step_id: TaskStepId::new(id)?,
            title: id.to_owned(),
            display_name: None,
            detail: None,
            role: AgentRole::Executor,
            depends_on,
            intent_refs: Vec::new(),
            mode: Some(TaskStepMode::Write),
            isolation: Some(isolation),
        })
    };
    let one = TaskStepId::new("one")?;
    let two = TaskStepId::new("two")?;
    let segments = derive_task_execution_segments(&[
        step(
            "one",
            Vec::new(),
            TaskIsolationMode::SequentialWorkspaceWrite,
        )?,
        step(
            "two",
            vec![one.clone()],
            TaskIsolationMode::SequentialWorkspaceWrite,
        )?,
        step(
            "three",
            vec![one, two],
            TaskIsolationMode::SequentialWorkspaceWrite,
        )?,
        step("four", Vec::new(), TaskIsolationMode::ChangesetOnly)?,
    ]);
    assert_eq!(segments.len(), 3);
    assert_eq!(
        segments[0].step_ids,
        vec![TaskStepId::new("one")?, TaskStepId::new("two")?]
    );
    Ok(())
}

#[test]
fn task_semantic_title_prefers_the_approved_plan_summary() {
    let objective = "Execute the following user-approved structured plan with the configured approval requirements.\n\nApproved structured plan:\n\nSummary: 修复权限默认值与测试\n\nSteps:\n1. Inspect";

    assert_eq!(task_semantic_title(objective), "修复权限默认值与测试");
    assert_eq!(
        task_semantic_title("\n  Inspect the workspace\nnext"),
        "Inspect the workspace"
    );
}

fn task_id(value: &str) -> Result<TaskId> {
    TaskId::new(value)
}

fn step_id(value: &str) -> Result<TaskStepId> {
    TaskStepId::new(value)
}

fn session_ref(value: &str) -> Result<SessionRef> {
    SessionRef::new_relative(value)
}

fn approval_binding() -> Result<TaskApprovalRouteBinding> {
    Ok(TaskApprovalRouteBinding {
        batch_id: "batch_1".to_owned(),
        source_thread_id: AgentThreadId::new("thread_1")?,
        attempt_id: TaskParticipantAttemptId::new("attempt_1")?,
        permission_signature: format!("sha256:{}", "a".repeat(64)),
        policy_fingerprint: format!("sha256:{}", "b".repeat(64)),
        aggregation_signature: format!("sha256:{}", "d".repeat(64)),
        source_workspace_id: format!("workspace:{}", "c".repeat(64)),
        isolation: TaskIsolationMode::SequentialWorkspaceWrite,
        requested_at_ms: 10,
        expires_at_ms: 20,
    })
}

fn run_entry(status: TaskRunStatus) -> Result<ControlEntry> {
    Ok(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id("task_1")?,
        parent_session_ref: session_ref("parent.jsonl")?,
        objective: "ship planner".to_owned(),
        title: None,
        status,
        reason: None,
    }))
}

fn read_step(id: &str, depends_on: Vec<TaskStepId>) -> Result<TaskStepSpec> {
    Ok(TaskStepSpec {
        step_id: step_id(id)?,
        title: format!("Read {id}"),
        display_name: None,
        detail: None,
        role: AgentRole::SubagentRead,
        depends_on,
        intent_refs: Vec::new(),
        mode: Some(TaskStepMode::Read),
        isolation: Some(TaskIsolationMode::SharedReadOnly),
    })
}

fn sha256_prefixed(value: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(value.as_bytes());
    format!("sha256:{:x}", digest.finalize())
}

#[test]
fn task_identifiers_reject_path_unsafe_values() {
    assert!(TaskId::new("").is_err());
    assert!(TaskId::new("../task").is_err());
    assert!(TaskId::new("task/slash").is_err());
    assert!(TaskId::new("a".repeat(97)).is_err());
    assert!(TaskStepId::new("step 1").is_err());
    assert!(TaskRouteId::new("route:1").is_err());
    assert!(TaskId::new("task_1-alpha").is_ok());
    assert_eq!(
        TaskRouteId::new("route_1")
            .expect("route id should parse")
            .as_str(),
        "route_1"
    );
}

#[test]
fn session_ref_rejects_absolute_and_parent_paths() {
    assert!(SessionRef::new_relative("").is_err());
    assert!(SessionRef::new_relative(".").is_err());
    assert!(SessionRef::new_relative("/tmp/child.jsonl").is_err());
    assert!(SessionRef::new_relative("../child.jsonl").is_err());
    assert!(SessionRef::new_relative("children/../child.jsonl").is_err());
    assert!(SessionRef::new_relative("children/./child.jsonl").is_ok());
    assert!(SessionRef::new_relative("children/task_1/step_1-child.jsonl").is_ok());
}

#[test]
fn task_role_and_status_labels_are_stable() {
    assert_eq!(AgentRole::Planner.as_str(), "planner");
    assert_eq!(AgentRole::Executor.as_str(), "executor");
    assert_eq!(AgentRole::SubagentRead.as_str(), "subagent_read");
    assert_eq!(AgentRole::SubagentWrite.as_str(), "subagent_write");
    assert_eq!(TaskStepMode::Read.as_str(), "read");
    assert_eq!(TaskStepMode::Write.as_str(), "write");
    assert_eq!(TaskStepMode::Review.as_str(), "review");
    assert_eq!(TaskStepMode::Verify.as_str(), "verify");
    assert_eq!(
        TaskIsolationMode::SharedReadOnly.as_str(),
        "shared_read_only"
    );
    assert_eq!(
        TaskIsolationMode::SequentialWorkspaceWrite.as_str(),
        "sequential_workspace_write"
    );
    assert_eq!(TaskIsolationMode::ChangesetOnly.as_str(), "changeset_only");
    assert_eq!(TaskIsolationMode::Worktree.as_str(), "worktree");

    assert!(TaskRunStatus::Completed.is_terminal());
    assert!(TaskRunStatus::Failed.is_terminal());
    assert!(TaskRunStatus::Cancelled.is_terminal());
    assert!(TaskRunStatus::Interrupted.is_terminal());
    assert!(!TaskRunStatus::Paused.is_terminal());

    assert!(TaskStepStatus::Completed.is_terminal());
    assert!(TaskStepStatus::Blocked.is_terminal());
    assert!(TaskStepStatus::Interrupted.is_terminal());
    assert!(TaskStepStatus::Superseded.is_terminal());
    assert!(!TaskStepStatus::Running.is_terminal());
}

#[test]
fn task_status_as_str_matches_serde_wire_values() -> Result<()> {
    for (status, expected) in [
        (TaskRunStatus::Started, "started"),
        (TaskRunStatus::Running, "running"),
        (TaskRunStatus::Paused, "paused"),
        (TaskRunStatus::Completed, "completed"),
        (TaskRunStatus::Failed, "failed"),
        (TaskRunStatus::Cancelled, "cancelled"),
        (TaskRunStatus::Interrupted, "interrupted"),
    ] {
        assert_eq!(status.as_str(), expected);
        assert_eq!(
            serde_json::to_value(status)?,
            serde_json::Value::String(expected.to_owned())
        );
    }

    for (status, expected) in [
        (TaskPlanStatus::Proposed, "proposed"),
        (TaskPlanStatus::Accepted, "accepted"),
        (TaskPlanStatus::Superseded, "superseded"),
        (TaskPlanStatus::Rejected, "rejected"),
    ] {
        assert_eq!(status.as_str(), expected);
        assert_eq!(
            serde_json::to_value(status)?,
            serde_json::Value::String(expected.to_owned())
        );
    }

    for (status, expected) in [
        (TaskStepStatus::Pending, "pending"),
        (TaskStepStatus::Running, "running"),
        (TaskStepStatus::Completed, "completed"),
        (TaskStepStatus::Failed, "failed"),
        (TaskStepStatus::Blocked, "blocked"),
        (TaskStepStatus::Cancelled, "cancelled"),
        (TaskStepStatus::Interrupted, "interrupted"),
        (TaskStepStatus::Superseded, "superseded"),
    ] {
        assert_eq!(status.as_str(), expected);
        assert_eq!(
            serde_json::to_value(status)?,
            serde_json::Value::String(expected.to_owned())
        );
    }

    Ok(())
}

#[test]
fn task_pause_request_binds_exact_direct_authority() -> Result<()> {
    let mut request = TaskPauseRequest::direct(task_id("task_1")?, "admission-1");

    assert!(request.has_exact_identity());
    assert!(request.request_id.starts_with("task-pause-"));
    request.execution = crate::TaskExecutionBindingV1::Direct {
        admission_id: "admission-2".to_owned(),
    };
    assert!(!request.has_exact_identity());
    request.request_id = request.expected_request_id();
    assert!(request.has_exact_identity());
    request.execution = crate::TaskExecutionBindingV1::Direct {
        admission_id: String::new(),
    };
    request.request_id = request.expected_request_id();
    assert!(!request.has_exact_identity());
    Ok(())
}

#[test]
fn child_session_ref_uses_stable_relative_layout() -> Result<()> {
    let reference = child_session_ref(
        &task_id("task_1")?,
        &step_id("step_2")?,
        &task_id("child_1")?,
    )?;

    assert_eq!(
        reference.as_path(),
        std::path::Path::new("children/task_1/step_2-child_1.jsonl")
    );
    assert_eq!(
        reference.resolve(std::path::Path::new("sessions")),
        std::path::Path::new("sessions/children/task_1/step_2-child_1.jsonl")
    );
    Ok(())
}

#[test]
fn task_agent_display_name_normalization_rejects_unsafe_values() -> Result<()> {
    assert_eq!(
        normalize_task_agent_display_name("  德语译员  ")?,
        "德语译员"
    );
    assert!(normalize_task_agent_display_name("").is_err());
    assert!(normalize_task_agent_display_name(" \t ").is_err());
    assert!(normalize_task_agent_display_name("bad\nname").is_err());
    assert!(
        normalize_task_agent_display_name(&"a".repeat(TASK_AGENT_DISPLAY_NAME_MAX_CHARS + 1))
            .is_err()
    );
    Ok(())
}

#[test]
fn task_control_entries_roundtrip() -> Result<()> {
    let entries = vec![
        run_entry(TaskRunStatus::Started)?,
        ControlEntry::TaskPlan(TaskPlanEntry {
            task_id: task_id("task_1")?,
            plan_version: 1,
            status: TaskPlanStatus::Accepted,
            steps: vec![TaskStepSpec {
                step_id: step_id("step_1")?,
                title: "inspect".to_owned(),
                display_name: None,
                detail: Some("read code".to_owned()),
                role: AgentRole::Planner,
                depends_on: Vec::new(),
                intent_refs: Vec::new(),
                mode: None,
                isolation: None,
            }],
            reason: None,
        }),
        ControlEntry::TaskStep(TaskStepEntry {
            task_id: task_id("task_1")?,
            plan_version: 1,
            step_id: step_id("step_1")?,
            role: AgentRole::Executor,
            status: TaskStepStatus::Completed,
            title: Some("inspect".to_owned()),
            summary: Some("done".to_owned()),
            reason: None,
        }),
        ControlEntry::TaskChildSessionDisplayName(TaskChildSessionDisplayNameEntry {
            task_id: task_id("task_1")?,
            plan_version: 1,
            step_id: step_id("step_1")?,
            child_task_id: task_id("child_1")?,
            display_name: "德语译员".to_owned(),
        }),
    ];

    for entry in entries {
        let session_entry = SessionLogEntry::Control(entry.clone());
        let encoded = serde_json::to_string(&session_entry)?;
        let decoded: SessionLogEntry = serde_json::from_str(&encoded)?;
        assert!(matches!(decoded, SessionLogEntry::Control(_)));
    }
    Ok(())
}

#[test]
fn task_dag_schema_rejects_missing_dependencies_cycles_and_bad_isolation() -> Result<()> {
    let read_step = TaskStepSpec {
        step_id: step_id("read")?,
        title: "read".to_owned(),
        display_name: None,
        detail: None,
        role: AgentRole::SubagentRead,
        depends_on: Vec::new(),
        intent_refs: Vec::new(),
        mode: Some(TaskStepMode::Read),
        isolation: Some(TaskIsolationMode::SharedReadOnly),
    };
    let write_step = TaskStepSpec {
        step_id: step_id("write")?,
        title: "write".to_owned(),
        display_name: None,
        detail: None,
        role: AgentRole::Executor,
        depends_on: vec![step_id("read")?],
        intent_refs: Vec::new(),
        mode: Some(TaskStepMode::Write),
        isolation: Some(TaskIsolationMode::SequentialWorkspaceWrite),
    };
    let worktree_step = TaskStepSpec {
        step_id: step_id("isolated_write")?,
        title: "isolated write".to_owned(),
        display_name: None,
        detail: None,
        role: AgentRole::SubagentWrite,
        depends_on: vec![step_id("read")?],
        intent_refs: Vec::new(),
        mode: Some(TaskStepMode::Write),
        isolation: Some(TaskIsolationMode::Worktree),
    };
    validate_task_plan_graph_steps(&[read_step.clone(), write_step.clone(), worktree_step])?;

    let mut missing_dependency = write_step.clone();
    missing_dependency.depends_on = vec![step_id("missing")?];
    assert!(validate_task_plan_graph_steps(&[read_step.clone(), missing_dependency]).is_err());

    let duplicate_id = TaskStepSpec {
        step_id: step_id("read")?,
        title: "duplicate".to_owned(),
        display_name: None,
        detail: None,
        role: AgentRole::SubagentRead,
        depends_on: Vec::new(),
        intent_refs: Vec::new(),
        mode: Some(TaskStepMode::Read),
        isolation: Some(TaskIsolationMode::SharedReadOnly),
    };
    assert!(validate_task_plan_graph_steps(&[read_step.clone(), duplicate_id]).is_err());

    let mut self_dependency = read_step.clone();
    self_dependency.depends_on = vec![step_id("read")?];
    assert!(validate_task_plan_graph_steps(&[self_dependency]).is_err());

    let mut repeated_dependency = write_step.clone();
    repeated_dependency.depends_on = vec![step_id("read")?, step_id("read")?];
    assert!(validate_task_plan_graph_steps(&[read_step.clone(), repeated_dependency]).is_err());

    let mut first_cycle = read_step.clone();
    first_cycle.depends_on = vec![step_id("write")?];
    let mut second_cycle = write_step.clone();
    second_cycle.depends_on = vec![step_id("read")?];
    assert!(validate_task_plan_graph_steps(&[first_cycle, second_cycle]).is_err());

    let mut unsafe_write = write_step.clone();
    unsafe_write.isolation = Some(TaskIsolationMode::SharedReadOnly);
    assert!(validate_task_plan_graph_steps(&[read_step.clone(), unsafe_write]).is_err());

    let mut over_isolated_read = read_step;
    over_isolated_read.isolation = Some(TaskIsolationMode::SequentialWorkspaceWrite);
    assert!(validate_task_plan_graph_steps(&[over_isolated_read, write_step]).is_err());
    Ok(())
}

#[test]
fn task_dag_read_only_write_denial_rejects_shared_read_only_write_step() -> Result<()> {
    let unsafe_write = TaskStepSpec {
        step_id: step_id("write")?,
        title: "Unsafe write".to_owned(),
        display_name: None,
        detail: None,
        role: AgentRole::Executor,
        depends_on: Vec::new(),
        intent_refs: Vec::new(),
        mode: Some(TaskStepMode::Write),
        isolation: Some(TaskIsolationMode::SharedReadOnly),
    };

    let error = validate_task_plan_graph_steps(&[unsafe_write])
        .expect_err("write steps must not claim shared_read_only isolation");

    assert!(error.to_string().contains("cannot use shared_read_only"));
    Ok(())
}

#[test]
fn task_verify_mode_separates_review_advisory_from_system_verifier() -> Result<()> {
    let review_step = TaskStepSpec {
        step_id: step_id("review")?,
        title: "Review".to_owned(),
        display_name: None,
        detail: None,
        role: AgentRole::SubagentRead,
        depends_on: Vec::new(),
        intent_refs: Vec::new(),
        mode: Some(TaskStepMode::Review),
        isolation: Some(TaskIsolationMode::SharedReadOnly),
    };
    let verify_step = TaskStepSpec {
        step_id: step_id("verify")?,
        title: "Verify".to_owned(),
        display_name: None,
        detail: None,
        role: AgentRole::Executor,
        depends_on: vec![step_id("review")?],
        intent_refs: Vec::new(),
        mode: Some(TaskStepMode::Verify),
        isolation: Some(TaskIsolationMode::SharedReadOnly),
    };

    assert!(review_step.is_review_advisory());
    assert!(!review_step.requires_system_verifier());
    assert!(!verify_step.is_review_advisory());
    assert!(verify_step.requires_system_verifier());

    assert_eq!(review_step.effective_mode(), TaskStepMode::Review);
    assert_eq!(verify_step.effective_mode(), TaskStepMode::Verify);
    Ok(())
}

#[test]
fn task_projection_replays_run_plan_and_step_state() -> Result<()> {
    let entries = vec![
        SessionLogEntry::Control(run_entry(TaskRunStatus::Started)?),
        SessionLogEntry::Control(ControlEntry::TaskPlan(TaskPlanEntry {
            task_id: task_id("task_1")?,
            plan_version: 1,
            status: TaskPlanStatus::Accepted,
            steps: vec![TaskStepSpec {
                step_id: step_id("step_1")?,
                title: "implement".to_owned(),
                display_name: None,
                detail: None,
                role: AgentRole::Executor,
                depends_on: Vec::new(),
                intent_refs: Vec::new(),
                mode: None,
                isolation: None,
            }],
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskStep(TaskStepEntry {
            task_id: task_id("task_1")?,
            plan_version: 1,
            step_id: step_id("step_1")?,
            role: AgentRole::Executor,
            status: TaskStepStatus::Running,
            title: Some("implement".to_owned()),
            summary: None,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskStep(TaskStepEntry {
            task_id: task_id("task_1")?,
            plan_version: 1,
            step_id: step_id("step_1")?,
            role: AgentRole::Executor,
            status: TaskStepStatus::Completed,
            title: Some("implement".to_owned()),
            summary: Some("implemented".to_owned()),
            reason: None,
        })),
        SessionLogEntry::Control(run_entry(TaskRunStatus::Completed)?),
    ];
    let session = Session::from_entries("mock", "model", entries);
    let projection = session.task_state_projection();
    let task = projection
        .tasks
        .get(&task_id("task_1")?)
        .ok_or_else(|| anyhow::anyhow!("missing task projection"))?;

    assert_eq!(task.status, TaskRunStatus::Completed);
    assert_eq!(task.latest_plan_version, Some(1));
    assert_eq!(
        task.steps
            .get(&(1, step_id("step_1")?))
            .map(|step| step.status),
        Some(TaskStepStatus::Completed)
    );
    assert_eq!(task.current_step, None);
    Ok(())
}

#[test]
fn task_projection_tracks_latest_task_by_replay_order() -> Result<()> {
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id("task_z")?,
            parent_session_ref: session_ref("parent.jsonl")?,
            objective: "first".to_owned(),
            title: None,

            status: TaskRunStatus::Started,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id("task_a")?,
            parent_session_ref: session_ref("parent.jsonl")?,
            objective: "second".to_owned(),
            title: None,

            status: TaskRunStatus::Started,
            reason: None,
        })),
    ];
    let projection = TaskStateProjection::from_entries(&entries);

    assert_eq!(
        projection.latest_task().map(|task| task.task_id.as_str()),
        Some("task_a")
    );
    assert_eq!(
        projection
            .latest_task_id
            .as_ref()
            .map(|task_id| task_id.as_str()),
        Some("task_a")
    );
    assert_eq!(
        projection
            .latest_unfinished_task()
            .map(|task| task.task_id.as_str()),
        Some("task_a")
    );
    Ok(())
}

#[test]
fn task_projection_rejects_a_standalone_completed_claim() -> Result<()> {
    let task_id = task_id("standalone_completed")?;
    let projection = TaskStateProjection::from_entries(&[SessionLogEntry::Control(
        ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: session_ref("parent.jsonl")?,
            objective: "must not manufacture completion".to_owned(),
            title: None,
            status: TaskRunStatus::Completed,
            reason: None,
        }),
    )]);

    assert!(!projection.tasks.contains_key(&task_id));
    assert!(projection.latest_task().is_none());
    Ok(())
}

#[test]
fn task_projection_keeps_the_first_semantic_title_across_lifecycle_updates() -> Result<()> {
    let task_id = task_id("task_title")?;
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: session_ref("parent.jsonl")?,
            objective: "generic internal execution wrapper".to_owned(),
            title: Some("用户可读的计划标题".to_owned()),
            status: TaskRunStatus::Started,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: session_ref("parent.jsonl")?,
            objective: "generic internal execution wrapper".to_owned(),
            title: Some("generic internal execution wrapper".to_owned()),
            status: TaskRunStatus::Running,
            reason: None,
        })),
    ];

    let projection = TaskStateProjection::from_entries(&entries);
    assert_eq!(
        projection
            .tasks
            .get(&task_id)
            .and_then(|task| task.title.as_deref()),
        Some("用户可读的计划标题")
    );
    Ok(())
}

#[test]
fn task_projection_tracks_all_active_steps_and_keeps_current_step_compatible() -> Result<()> {
    let read_a = step_id("read_a")?;
    let read_b = step_id("read_b")?;
    let running_step = |step_id: TaskStepId| {
        SessionLogEntry::Control(ControlEntry::TaskStep(TaskStepEntry {
            task_id: task_id("task_1").expect("valid task id"),
            plan_version: 1,
            step_id,
            role: AgentRole::SubagentRead,
            status: TaskStepStatus::Running,
            title: None,
            summary: None,
            reason: None,
        }))
    };
    let completed_step = |step_id: TaskStepId| {
        SessionLogEntry::Control(ControlEntry::TaskStep(TaskStepEntry {
            task_id: task_id("task_1").expect("valid task id"),
            plan_version: 1,
            step_id,
            role: AgentRole::SubagentRead,
            status: TaskStepStatus::Completed,
            title: None,
            summary: Some("done".to_owned()),
            reason: None,
        }))
    };
    let base = vec![
        SessionLogEntry::Control(run_entry(TaskRunStatus::Running)?),
        running_step(read_a.clone()),
        running_step(read_b.clone()),
    ];

    let projection = TaskStateProjection::from_entries(&base);
    let task = projection.latest_task().expect("task should project");
    assert_eq!(
        task.active_steps,
        BTreeSet::from([(1, read_a.clone()), (1, read_b.clone())])
    );
    assert_eq!(task.current_step, None);

    let mut one_active = base.clone();
    one_active.push(completed_step(read_a.clone()));
    let projection = TaskStateProjection::from_entries(&one_active);
    let task = projection.latest_task().expect("task should project");
    assert_eq!(task.active_steps, BTreeSet::from([(1, read_b.clone())]));
    assert_eq!(task.current_step, Some((1, read_b.clone())));

    one_active.push(completed_step(read_b));
    let projection = TaskStateProjection::from_entries(&one_active);
    let task = projection.latest_task().expect("task should project");
    assert!(task.active_steps.is_empty());
    assert_eq!(task.current_step, None);

    let mut terminal = base;
    terminal.push(SessionLogEntry::Control(run_entry(
        TaskRunStatus::Interrupted,
    )?));
    let projection = TaskStateProjection::from_entries(&terminal);
    let task = projection.latest_task().expect("task should project");
    assert!(task.active_steps.is_empty());
    assert_eq!(task.current_step, None);
    Ok(())
}

#[test]
fn task_projection_tracks_latest_unfinished_task_by_replay_order() -> Result<()> {
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id("task_1")?,
            parent_session_ref: session_ref("parent.jsonl")?,
            objective: "first".to_owned(),
            title: None,

            status: TaskRunStatus::Failed,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id("task_2")?,
            parent_session_ref: session_ref("parent.jsonl")?,
            objective: "second".to_owned(),
            title: None,

            status: TaskRunStatus::Started,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id("task_2")?,
            parent_session_ref: session_ref("parent.jsonl")?,
            objective: "second".to_owned(),
            title: None,

            status: TaskRunStatus::Completed,
            reason: None,
        })),
    ];
    let projection = TaskStateProjection::from_entries(&entries);

    assert_eq!(
        projection.latest_task().map(|task| task.task_id.as_str()),
        Some("task_2")
    );
    assert_eq!(
        projection
            .latest_unfinished_task()
            .map(|task| task.task_id.as_str()),
        Some("task_1")
    );
    Ok(())
}

#[test]
fn task_projection_returns_none_when_latest_tasks_are_final() -> Result<()> {
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id("task_1")?,
            parent_session_ref: session_ref("parent.jsonl")?,
            objective: "first".to_owned(),
            title: None,

            status: TaskRunStatus::Completed,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id("task_2")?,
            parent_session_ref: session_ref("parent.jsonl")?,
            objective: "second".to_owned(),
            title: None,

            status: TaskRunStatus::Cancelled,
            reason: None,
        })),
    ];
    let projection = TaskStateProjection::from_entries(&entries);

    assert_eq!(
        projection.latest_task().map(|task| task.task_id.as_str()),
        Some("task_2")
    );
    assert!(projection.latest_unfinished_task().is_none());
    Ok(())
}

#[test]
fn task_projection_tracks_duplicate_terminal_entries() -> Result<()> {
    let projection = TaskStateProjection::from_entries(&[
        SessionLogEntry::Control(run_entry(TaskRunStatus::Started)?),
        SessionLogEntry::Control(run_entry(TaskRunStatus::Completed)?),
        SessionLogEntry::Control(run_entry(TaskRunStatus::Failed)?),
    ]);
    let task = projection
        .tasks
        .get(&task_id("task_1")?)
        .ok_or_else(|| anyhow::anyhow!("missing task projection"))?;

    assert_eq!(task.status, TaskRunStatus::Completed);
    assert_eq!(task.duplicate_terminal_entries, 1);
    Ok(())
}

#[test]
fn task_projection_allows_resumable_terminal_task_and_step_to_continue() -> Result<()> {
    let step_id = step_id("step_1")?;
    let projection = TaskStateProjection::from_entries(&[
        SessionLogEntry::Control(run_entry(TaskRunStatus::Started)?),
        SessionLogEntry::Control(ControlEntry::TaskStep(TaskStepEntry {
            task_id: task_id("task_1")?,
            plan_version: 1,
            step_id: step_id.clone(),
            role: AgentRole::Executor,
            status: TaskStepStatus::Failed,
            title: Some("inspect".to_owned()),
            summary: None,
            reason: Some("failed".to_owned()),
        })),
        SessionLogEntry::Control(run_entry(TaskRunStatus::Failed)?),
        SessionLogEntry::Control(run_entry(TaskRunStatus::Running)?),
        SessionLogEntry::Control(ControlEntry::TaskStep(TaskStepEntry {
            task_id: task_id("task_1")?,
            plan_version: 1,
            step_id: step_id.clone(),
            role: AgentRole::Executor,
            status: TaskStepStatus::Running,
            title: Some("inspect".to_owned()),
            summary: None,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskStep(TaskStepEntry {
            task_id: task_id("task_1")?,
            plan_version: 1,
            step_id: step_id.clone(),
            role: AgentRole::Executor,
            status: TaskStepStatus::Completed,
            title: Some("inspect".to_owned()),
            summary: Some("done".to_owned()),
            reason: None,
        })),
        SessionLogEntry::Control(run_entry(TaskRunStatus::Completed)?),
    ]);
    let task = projection
        .tasks
        .get(&task_id("task_1")?)
        .ok_or_else(|| anyhow::anyhow!("missing task projection"))?;

    assert_eq!(task.status, TaskRunStatus::Completed);
    assert_eq!(
        task.steps.get(&(1, step_id)).map(|step| step.status),
        Some(TaskStepStatus::Completed)
    );
    assert_eq!(task.duplicate_terminal_entries, 0);
    assert_eq!(task.current_step, None);
    Ok(())
}

#[test]
fn task_projection_tracks_duplicate_final_step_entries() -> Result<()> {
    let step_id = step_id("step_1")?;
    let projection = TaskStateProjection::from_entries(&[
        SessionLogEntry::Control(run_entry(TaskRunStatus::Started)?),
        SessionLogEntry::Control(ControlEntry::TaskStep(TaskStepEntry {
            task_id: task_id("task_1")?,
            plan_version: 1,
            step_id: step_id.clone(),
            role: AgentRole::Executor,
            status: TaskStepStatus::Completed,
            title: Some("inspect".to_owned()),
            summary: Some("done".to_owned()),
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskStep(TaskStepEntry {
            task_id: task_id("task_1")?,
            plan_version: 1,
            step_id,
            role: AgentRole::Executor,
            status: TaskStepStatus::Failed,
            title: Some("inspect".to_owned()),
            summary: None,
            reason: Some("late failure".to_owned()),
        })),
    ]);
    let task = projection
        .tasks
        .get(&task_id("task_1")?)
        .ok_or_else(|| anyhow::anyhow!("missing task projection"))?;

    assert_eq!(task.duplicate_terminal_entries, 1);
    Ok(())
}

#[test]
fn task_projection_creates_placeholder_for_plan_before_run() -> Result<()> {
    let projection = TaskStateProjection::from_entries(&[SessionLogEntry::Control(
        ControlEntry::TaskPlan(TaskPlanEntry {
            task_id: task_id("task_1")?,
            plan_version: 1,
            status: TaskPlanStatus::Proposed,
            steps: Vec::new(),
            reason: None,
        }),
    )]);
    let task = projection
        .tasks
        .get(&task_id("task_1")?)
        .ok_or_else(|| anyhow::anyhow!("missing placeholder task"))?;

    assert_eq!(task.objective, "");
    assert_eq!(
        task.parent_session_ref.as_path(),
        std::path::Path::new("unknown.jsonl")
    );
    assert_eq!(task.status, TaskRunStatus::Started);
    assert_eq!(task.latest_plan_version, Some(1));
    Ok(())
}

#[test]
fn task_root_terminal_evaluator_requires_direct_attempt_terminal_candidate() -> Result<()> {
    let task_id = task_id("task-terminal-direct")?;
    let objective = "complete exactly one direct task";
    let admission = TaskDirectExecutionAdmittedV1::task_request(task_id.clone(), objective, 1);
    let attempt = TaskDirectExecutionAttemptV1::started(&admission, 1);
    let projection = TaskStateProjection::from_entries(&[
        SessionLogEntry::Control(ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: session_ref("parent.jsonl")?,
            objective: objective.to_owned(),
            title: None,
            status: TaskRunStatus::Running,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskDirectExecutionAdmittedV1(admission)),
        SessionLogEntry::Control(ControlEntry::TaskDirectExecutionAttemptV1(attempt.clone())),
    ]);

    let unfinished = projection
        .evaluate_root_terminal(&task_id, TaskRunStatus::Completed, None)
        .expect("direct task should project");
    assert_eq!(unfinished.effective_status, TaskRunStatus::Paused);
    assert_eq!(
        unfinished.primary_completion_blocker(),
        Some(TaskRootCompletionBlockerV1::UnfinishedDirectExecution)
    );
    let candidate = TaskRootTerminalCandidateV1::DirectExecution {
        attempt_id: attempt.attempt_id,
        status: TaskExecutionAttemptStatus::Completed,
    };
    assert!(
        projection
            .evaluate_root_terminal(&task_id, TaskRunStatus::Completed, Some(&candidate))
            .expect("direct task should project")
            .allows_completed()
    );
    Ok(())
}

#[test]
fn direct_task_completion_waits_for_exact_owned_background_agents() -> Result<()> {
    let direct_task_id = task_id("task-background-owner")?;
    let other_task_id = task_id("another-task-background-owner")?;
    let objective = "complete the direct task after its owned background work";
    let direct_admission =
        TaskDirectExecutionAdmittedV1::task_request(direct_task_id.clone(), objective, 1);
    let attempt = TaskDirectExecutionAttemptV1::started(&direct_admission, 1);
    let thread_id = AgentThreadId::new("agent_direct_task_background")?;
    let unrelated_thread_id = AgentThreadId::new("agent_other_task_background")?;
    let grant_for = |grant_task_id: TaskId| crate::AgentInvocationGrantRecord {
        grant_fingerprint: format!("sha256:{}", "a".repeat(64)),
        source: crate::AgentInvocationGrantSource::DirectTask {
            task_id: grant_task_id.clone(),
        },
        authority: crate::DelegationAuthorityRecord::DirectTask {
            task_id: grant_task_id,
        },
        profile_id: crate::AgentProfileId::new("explore").expect("profile id"),
        role: AgentRole::SubagentRead,
        isolation: TaskIsolationMode::SharedReadOnly,
        permission_upper_bound_fingerprint: format!("sha256:{}", "b".repeat(64)),
        network_upper_bound: crate::NetworkPolicy::Deny,
        tool_contract_fingerprint: format!("sha256:{}", "c".repeat(64)),
        workspace_snapshot_id: None,
        root_run_fingerprint: format!("sha256:{}", "d".repeat(64)),
        root_cancellation_scope_fingerprint: format!("sha256:{}", "e".repeat(64)),
        expires_at_ms: 100,
    };
    let child_admission =
        |thread_id: AgentThreadId, grant_task_id: TaskId| crate::AgentDelegationAdmissionEntry {
            thread_id,
            profile_id: crate::AgentProfileId::new("explore").expect("profile id"),
            invocation_mode: crate::AgentInvocationMode::Background,
            invocation_source: crate::AgentInvocationSource::Task,
            authority: crate::DelegationAuthorityRecord::DirectTask {
                task_id: direct_task_id.clone(),
            },
            objective_hash: format!("sha256:{}", "f".repeat(64)),
            tool_contract_fingerprint: format!("sha256:{}", "c".repeat(64)),
            invocation_grant: Some(grant_for(grant_task_id)),
            admitted_at_ms: Some(2),
        };
    let mut entries = vec![
        SessionLogEntry::Control(ControlEntry::TaskRun(TaskRunEntry {
            task_id: direct_task_id.clone(),
            parent_session_ref: session_ref("parent.jsonl")?,
            objective: objective.to_owned(),
            title: None,
            status: TaskRunStatus::Running,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskDirectExecutionAdmittedV1(
            direct_admission.clone(),
        )),
        SessionLogEntry::Control(ControlEntry::TaskDirectExecutionAttemptV1(attempt.clone())),
        SessionLogEntry::Control(ControlEntry::AgentDelegationAdmitted(child_admission(
            thread_id.clone(),
            direct_task_id.clone(),
        ))),
        SessionLogEntry::Control(ControlEntry::AgentDelegationAdmitted(child_admission(
            unrelated_thread_id,
            other_task_id,
        ))),
    ];
    let candidate = TaskRootTerminalCandidateV1::DirectExecution {
        attempt_id: attempt.attempt_id,
        status: TaskExecutionAttemptStatus::Completed,
    };
    let projection = TaskStateProjection::from_entries(&entries);
    assert_eq!(
        projection.direct_task_for_background_agent(&thread_id),
        Some(&direct_task_id)
    );
    assert_eq!(
        projection.unfinished_direct_task_background_agents(&direct_task_id),
        vec![thread_id.clone()]
    );
    assert_eq!(
        projection.direct_task_background_agents(&direct_task_id),
        vec![thread_id.clone()]
    );
    assert_eq!(
        projection
            .direct_task_for_background_agent(&AgentThreadId::new("agent_other_task_background")?),
        None,
        "a grant for another Task must not be attributed to this Direct Task"
    );
    let evaluation = projection
        .evaluate_root_terminal(&direct_task_id, TaskRunStatus::Completed, Some(&candidate))
        .expect("direct task should project");
    assert_eq!(evaluation.effective_status, TaskRunStatus::Paused);
    assert_eq!(
        evaluation.unfinished_direct_task_background_agents,
        vec![thread_id.clone()]
    );
    assert!(
        evaluation
            .completion_blockers
            .contains(&TaskRootCompletionBlockerV1::UnfinishedBackgroundAgent)
    );

    let mut interrupted_entries = entries.clone();
    interrupted_entries.push(SessionLogEntry::Control(ControlEntry::AgentRunInterrupted(
        crate::AgentRunInterruptedEntry {
            thread_id: thread_id.clone(),
            attempt_id: crate::AgentRunAttemptId::new("attempt-owner-lost")?,
            reason: "agent run interrupted during session restore".to_owned(),
        },
    )));
    let interrupted = TaskStateProjection::from_entries(&interrupted_entries);
    assert_eq!(
        interrupted.agent_thread_status(&thread_id),
        Some(crate::AgentThreadStatus::Interrupted),
        "attempt-level recovery must reach the Task projection"
    );

    let mut cancelled_entries = entries.clone();
    cancelled_entries.push(SessionLogEntry::Control(
        ControlEntry::AgentThreadStatusChanged(crate::AgentThreadStatusChangedEntry {
            thread_id: thread_id.clone(),
            status: crate::AgentThreadStatus::Cancelled,
            reason: Some("parent Task cancelled".to_owned()),
            updated_at_ms: Some(4),
        }),
    ));
    cancelled_entries.push(SessionLogEntry::Control(ControlEntry::AgentRunInterrupted(
        crate::AgentRunInterruptedEntry {
            thread_id: thread_id.clone(),
            attempt_id: crate::AgentRunAttemptId::new("attempt-cancelled")?,
            reason: "the cancelled attempt did not finish normally".to_owned(),
        },
    )));
    let cancelled = TaskStateProjection::from_entries(&cancelled_entries);
    assert_eq!(
        cancelled.agent_thread_status(&thread_id),
        Some(crate::AgentThreadStatus::Cancelled),
        "attempt interruption must not overwrite a durable terminal thread status"
    );
    assert!(
        cancelled
            .unfinished_direct_task_background_agents(&direct_task_id)
            .is_empty()
    );

    entries.push(SessionLogEntry::Control(
        ControlEntry::AgentThreadStatusChanged(crate::AgentThreadStatusChangedEntry {
            thread_id: thread_id.clone(),
            status: crate::AgentThreadStatus::Completed,
            reason: None,
            updated_at_ms: Some(3),
        }),
    ));
    let settled = TaskStateProjection::from_entries(&entries)
        .evaluate_root_terminal(&direct_task_id, TaskRunStatus::Completed, Some(&candidate))
        .expect("direct task should project after child status completion");
    assert_eq!(settled.effective_status, TaskRunStatus::Paused);
    assert_eq!(
        settled.unfinished_direct_task_background_agents,
        vec![thread_id.clone()],
        "a completed status is not collected until the child result is durable"
    );
    entries.push(SessionLogEntry::Control(
        ControlEntry::AgentThreadResultRecorded(crate::AgentThreadResultRecordedEntry {
            result: crate::AgentThreadResult {
                thread_id: thread_id.clone(),
                session_ref: SessionRef::new_relative(
                    "children/agents/agent_direct_task_background.jsonl",
                )?,
                status: crate::AgentThreadTerminalStatus::Completed,
                summary: "child result".to_owned(),
                summary_truncated: false,
                original_summary_chars: None,
                artifacts: Vec::new(),
                changed_paths: Vec::new(),
                risks: Vec::new(),
                followups: Vec::new(),
                usage: None,
                output_hash: "sha256:child-result".to_owned(),
                final_answer_ref: None,
            },
        }),
    ));
    let settled = TaskStateProjection::from_entries(&entries)
        .evaluate_root_terminal(&direct_task_id, TaskRunStatus::Completed, Some(&candidate))
        .expect("direct task should project after child result is durable");
    assert!(settled.allows_completed());
    assert!(settled.unfinished_direct_task_background_agents.is_empty());
    Ok(())
}

#[test]
fn task_root_terminal_evaluator_blocks_failed_dependencies_and_started_participants() -> Result<()>
{
    let task_id = task_id("task-terminal-dag")?;
    let failed_step = step_id("failed")?;
    let dependent_step = step_id("dependent")?;
    let participant_id = task_participant_attempt_id(
        &task_id,
        TaskParticipantPurpose::Step,
        Some(1),
        Some(&dependent_step),
        1,
    )?;
    let projection = TaskStateProjection::from_entries(&[
        SessionLogEntry::Control(ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: session_ref("parent.jsonl")?,
            objective: "complete DAG only after dependencies settle".to_owned(),
            title: None,
            status: TaskRunStatus::Running,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskPlan(TaskPlanEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            status: TaskPlanStatus::Accepted,
            steps: vec![
                read_step("failed", Vec::new())?,
                read_step("dependent", vec![failed_step.clone()])?,
            ],
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskStep(TaskStepEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            step_id: failed_step,
            role: AgentRole::SubagentRead,
            status: TaskStepStatus::Failed,
            title: Some("failed".to_owned()),
            summary: None,
            reason: Some("provider failed".to_owned()),
        })),
        SessionLogEntry::Control(ControlEntry::TaskParticipantAttempt(
            TaskParticipantAttemptEntry {
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
            },
        )),
    ]);

    let evaluation = projection
        .evaluate_root_terminal(&task_id, TaskRunStatus::Completed, None)
        .expect("DAG task should project");
    assert_eq!(evaluation.effective_status, TaskRunStatus::Paused);
    assert_eq!(
        evaluation.blocked_dependency_steps,
        vec![dependent_step.clone()]
    );
    assert_eq!(evaluation.unfinished_steps, vec![dependent_step]);
    assert_eq!(evaluation.unfinished_participants, vec![participant_id]);
    assert!(
        evaluation
            .completion_blockers
            .contains(&TaskRootCompletionBlockerV1::FailedDependency)
    );
    assert!(
        evaluation
            .completion_blockers
            .contains(&TaskRootCompletionBlockerV1::UnfinishedParticipant)
    );
    Ok(())
}

#[test]
fn task_root_terminal_evaluator_returns_the_full_cancellation_closure() -> Result<()> {
    let task_id = task_id("task-terminal-cancel")?;
    let cancelled_step = step_id("cancelled")?;
    let dependent_step = step_id("dependent")?;
    let leaf_step = step_id("leaf")?;
    let projection = TaskStateProjection::from_entries(&[
        SessionLogEntry::Control(ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: session_ref("parent.jsonl")?,
            objective: "cancel the DAG exactly".to_owned(),
            title: None,
            status: TaskRunStatus::Running,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskPlan(TaskPlanEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            status: TaskPlanStatus::Accepted,
            steps: vec![
                read_step("cancelled", Vec::new())?,
                read_step("dependent", vec![cancelled_step.clone()])?,
                read_step("leaf", vec![dependent_step.clone()])?,
            ],
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskStep(TaskStepEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            step_id: cancelled_step,
            role: AgentRole::SubagentRead,
            status: TaskStepStatus::Cancelled,
            title: Some("cancelled".to_owned()),
            summary: None,
            reason: Some("user cancelled".to_owned()),
        })),
    ]);

    let evaluation = projection
        .evaluate_root_terminal(&task_id, TaskRunStatus::Cancelled, None)
        .expect("DAG task should project");
    assert_eq!(evaluation.effective_status, TaskRunStatus::Cancelled);
    assert_eq!(
        evaluation.cancelled_dependency_steps,
        vec![dependent_step.clone(), leaf_step.clone()]
    );
    assert_eq!(
        evaluation.cancellation_closure,
        vec![dependent_step, leaf_step]
    );
    assert!(
        evaluation
            .completion_blockers
            .contains(&TaskRootCompletionBlockerV1::CancelledDependency)
    );
    Ok(())
}

#[test]
fn task_projection_supersedes_previous_accepted_plan() -> Result<()> {
    let completed_step = step_id("completed")?;
    let pending_step = step_id("pending")?;
    let projection = TaskStateProjection::from_entries(&[
        SessionLogEntry::Control(run_entry(TaskRunStatus::Started)?),
        SessionLogEntry::Control(ControlEntry::TaskPlan(TaskPlanEntry {
            task_id: task_id("task_1")?,
            plan_version: 1,
            status: TaskPlanStatus::Accepted,
            steps: vec![
                TaskStepSpec {
                    step_id: completed_step.clone(),
                    title: "Completed".to_owned(),
                    display_name: None,
                    detail: None,
                    role: AgentRole::Executor,
                    depends_on: Vec::new(),
                    intent_refs: Vec::new(),
                    mode: Some(TaskStepMode::Write),
                    isolation: Some(TaskIsolationMode::SequentialWorkspaceWrite),
                },
                TaskStepSpec {
                    step_id: pending_step.clone(),
                    title: "Pending".to_owned(),
                    display_name: None,
                    detail: None,
                    role: AgentRole::Planner,
                    depends_on: Vec::new(),
                    intent_refs: Vec::new(),
                    mode: Some(TaskStepMode::Read),
                    isolation: Some(TaskIsolationMode::SharedReadOnly),
                },
            ],
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskStep(TaskStepEntry {
            task_id: task_id("task_1")?,
            plan_version: 1,
            step_id: completed_step.clone(),
            role: AgentRole::Executor,
            status: TaskStepStatus::Completed,
            title: Some("Completed".to_owned()),
            summary: Some("done".to_owned()),
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskPlan(TaskPlanEntry {
            task_id: task_id("task_1")?,
            plan_version: 2,
            status: TaskPlanStatus::Accepted,
            steps: vec![TaskStepSpec {
                step_id: step_id("next")?,
                title: "Next".to_owned(),
                display_name: None,
                detail: None,
                role: AgentRole::Planner,
                depends_on: Vec::new(),
                intent_refs: Vec::new(),
                mode: Some(TaskStepMode::Read),
                isolation: Some(TaskIsolationMode::SharedReadOnly),
            }],
            reason: Some("replan".to_owned()),
        })),
    ]);
    let task = projection
        .tasks
        .get(&task_id("task_1")?)
        .ok_or_else(|| anyhow::anyhow!("missing task projection"))?;

    assert_eq!(task.latest_plan_version, Some(2));
    assert!(task.superseded_plan_versions.contains(&1));
    assert_eq!(
        task.plans.get(&1).map(|plan| plan.status),
        Some(TaskPlanStatus::Superseded)
    );
    assert_eq!(
        task.steps.get(&(1, completed_step)).map(|step| step.status),
        Some(TaskStepStatus::Completed)
    );
    let pending_projection = task
        .steps
        .get(&(1, pending_step))
        .ok_or_else(|| anyhow::anyhow!("missing superseded step"))?;
    assert_eq!(pending_projection.status, TaskStepStatus::Superseded);
    assert_eq!(
        pending_projection.reason.as_deref(),
        Some("superseded by accepted plan v2")
    );
    Ok(())
}

#[test]
fn task_replan_projection_clears_current_step_from_superseded_plan() -> Result<()> {
    let step = step_id("step_1")?;
    let projection = TaskStateProjection::from_entries(&[
        SessionLogEntry::Control(run_entry(TaskRunStatus::Started)?),
        SessionLogEntry::Control(ControlEntry::TaskPlan(TaskPlanEntry {
            task_id: task_id("task_1")?,
            plan_version: 1,
            status: TaskPlanStatus::Accepted,
            steps: vec![TaskStepSpec {
                step_id: step.clone(),
                title: "Running".to_owned(),
                display_name: None,
                detail: None,
                role: AgentRole::Executor,
                depends_on: Vec::new(),
                intent_refs: Vec::new(),
                mode: Some(TaskStepMode::Write),
                isolation: Some(TaskIsolationMode::SequentialWorkspaceWrite),
            }],
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskStep(TaskStepEntry {
            task_id: task_id("task_1")?,
            plan_version: 1,
            step_id: step.clone(),
            role: AgentRole::Executor,
            status: TaskStepStatus::Running,
            title: Some("Running".to_owned()),
            summary: None,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskPlan(TaskPlanEntry {
            task_id: task_id("task_1")?,
            plan_version: 2,
            status: TaskPlanStatus::Accepted,
            steps: vec![TaskStepSpec {
                step_id: step_id("next")?,
                title: "Next".to_owned(),
                display_name: None,
                detail: None,
                role: AgentRole::Planner,
                depends_on: Vec::new(),
                intent_refs: Vec::new(),
                mode: Some(TaskStepMode::Read),
                isolation: Some(TaskIsolationMode::SharedReadOnly),
            }],
            reason: Some("replan".to_owned()),
        })),
    ]);
    let task = projection
        .tasks
        .get(&task_id("task_1")?)
        .ok_or_else(|| anyhow::anyhow!("missing task projection"))?;

    assert_eq!(task.current_step, None);
    assert!(task.active_steps.is_empty());
    assert_eq!(
        task.steps.get(&(1, step)).map(|step| step.status),
        Some(TaskStepStatus::Superseded)
    );
    Ok(())
}

#[test]
fn task_projection_marks_unverified_routes_and_unavailable_children() -> Result<()> {
    let projection = TaskStateProjection::from_entries(&[
        SessionLogEntry::Control(run_entry(TaskRunStatus::Started)?),
        SessionLogEntry::Control(ControlEntry::TaskSubagentApprovalRoute(
            TaskSubagentApprovalRouteEntry {
                route_id: TaskRouteId::new("route_1")?,
                task_id: task_id("task_1")?,
                plan_version: 1,
                step_id: step_id("step_1")?,
                role: AgentRole::SubagentWrite,
                child_session_ref: session_ref("children/task_1/step_1-child_1.jsonl")?,
                call_id: "call-1".to_owned(),
                tool_name: "write_file".to_owned(),
                binding: None,
                status: TaskRouteStatus::Requested,
            },
        )),
        SessionLogEntry::Control(ControlEntry::TaskChildSession(TaskChildSessionEntry {
            task_id: task_id("task_1")?,
            plan_version: 1,
            step_id: step_id("step_1")?,
            child_task_id: task_id("child_1")?,
            child_session_ref: session_ref("children/task_1/step_1-child_1.jsonl")?,
            role: AgentRole::SubagentWrite,
            status: TaskChildSessionStatus::Unavailable,
            summary_hash: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskSubagentElicitationRoute(
            TaskSubagentElicitationRouteEntry {
                route_id: TaskRouteId::new("route_2")?,
                task_id: task_id("task_1")?,
                plan_version: 1,
                step_id: step_id("step_1")?,
                role: AgentRole::SubagentWrite,
                child_session_ref: session_ref("children/task_1/step_1-child_1.jsonl")?,
                server_name: "mcp".to_owned(),
                status: TaskRouteStatus::Requested,
            },
        )),
    ]);
    let task = projection
        .tasks
        .get(&task_id("task_1")?)
        .ok_or_else(|| anyhow::anyhow!("missing task projection"))?;

    assert!(task.route_unverified);
    assert!(task.child_unavailable);
    assert_eq!(task.approval_routes.len(), 1);
    assert_eq!(task.elicitation_routes.len(), 1);
    Ok(())
}

#[test]
fn task_projection_keeps_verified_subagent_routes_clean() -> Result<()> {
    let child_ref = session_ref("children/task_1/step_1-child_1.jsonl")?;
    let projection = TaskStateProjection::from_entries(&[
        SessionLogEntry::Control(run_entry(TaskRunStatus::Started)?),
        SessionLogEntry::Control(ControlEntry::TaskChildSession(TaskChildSessionEntry {
            task_id: task_id("task_1")?,
            plan_version: 1,
            step_id: step_id("step_1")?,
            child_task_id: task_id("child_1")?,
            child_session_ref: child_ref.clone(),
            role: AgentRole::SubagentWrite,
            status: TaskChildSessionStatus::Started,
            summary_hash: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskSubagentApprovalRoute(
            TaskSubagentApprovalRouteEntry {
                route_id: TaskRouteId::new("route_1")?,
                task_id: task_id("task_1")?,
                plan_version: 1,
                step_id: step_id("step_1")?,
                role: AgentRole::SubagentWrite,
                child_session_ref: child_ref,
                call_id: "call-1".to_owned(),
                tool_name: "write_file".to_owned(),
                binding: Some(approval_binding()?),
                status: TaskRouteStatus::Resolved,
            },
        )),
    ]);
    let task = projection
        .tasks
        .get(&task_id("task_1")?)
        .ok_or_else(|| anyhow::anyhow!("missing task projection"))?;

    assert!(!task.route_unverified);
    assert_eq!(task.approval_routes.len(), 1);
    Ok(())
}

#[test]
fn task_approval_restore_marks_pending_route_stale_without_reusing_binding() -> Result<()> {
    let route = TaskSubagentApprovalRouteEntry {
        route_id: TaskRouteId::new("route_1")?,
        task_id: task_id("task_1")?,
        plan_version: 1,
        step_id: step_id("step_1")?,
        role: AgentRole::SubagentWrite,
        child_session_ref: session_ref("children/task_1/step_1-child_1.jsonl")?,
        call_id: "call-1".to_owned(),
        tool_name: "write_file".to_owned(),
        binding: Some(approval_binding()?),
        status: TaskRouteStatus::Requested,
    };
    let stale = stale_task_approval_routes_for_restore(&[SessionLogEntry::Control(
        ControlEntry::TaskSubagentApprovalRoute(route.clone()),
    )]);

    assert_eq!(stale.len(), 1);
    assert_eq!(stale[0].status, TaskRouteStatus::Stale);
    assert_eq!(stale[0].binding, route.binding);
    Ok(())
}

#[test]
fn task_projection_replays_child_session_display_name_entries() -> Result<()> {
    let child = TaskChildSessionEntry {
        task_id: task_id("task_1")?,
        plan_version: 1,
        step_id: step_id("step_1")?,
        child_task_id: task_id("child_1")?,
        child_session_ref: session_ref("children/task_1/step_1-child_1.jsonl")?,
        role: AgentRole::SubagentWrite,
        status: TaskChildSessionStatus::Completed,
        summary_hash: None,
    };
    let projection = TaskStateProjection::from_entries(&[
        SessionLogEntry::Control(run_entry(TaskRunStatus::Started)?),
        SessionLogEntry::Control(ControlEntry::TaskChildSession(child.clone())),
        SessionLogEntry::Control(ControlEntry::TaskChildSessionDisplayName(
            TaskChildSessionDisplayNameEntry {
                task_id: task_id("task_1")?,
                plan_version: 1,
                step_id: step_id("step_1")?,
                child_task_id: task_id("child_1")?,
                display_name: "  德语译员  ".to_owned(),
            },
        )),
    ]);
    let task = projection
        .tasks
        .get(&task_id("task_1")?)
        .ok_or_else(|| anyhow::anyhow!("missing task projection"))?;

    assert_eq!(
        task.display_name_for_child_session(&child),
        Some("德语译员")
    );
    Ok(())
}

#[test]
fn task_projection_rejects_orphan_participant_results() -> Result<()> {
    let task_id = task_id("task_1")?;
    let step_id = step_id("inspect")?;
    let attempt_id = task_participant_attempt_id(
        &task_id,
        TaskParticipantPurpose::Step,
        Some(1),
        Some(&step_id),
        1,
    )?;
    let summary = "orphan step result".to_owned();
    let projection = TaskStateProjection::from_entries(&[
        SessionLogEntry::Control(run_entry(TaskRunStatus::Started)?),
        SessionLogEntry::Control(ControlEntry::TaskParticipantResult(
            TaskParticipantResultEntry {
                attempt_id,
                task_id: task_id.clone(),
                summary_hash: sha256_prefixed(&summary),
                output_hash: format!("sha256:{}", "0".repeat(64)),
                summary,
                summary_truncated: false,
                terminal_status: None,
                final_answer_ref: None,
                artifact_refs: Vec::new(),
                changed_paths: Vec::new(),
                verification_refs: Vec::new(),
            },
        )),
    ]);
    let task = projection
        .tasks
        .get(&task_id)
        .ok_or_else(|| anyhow::anyhow!("missing task projection"))?;

    assert!(task.participant_results.is_empty());
    assert_eq!(task.participant_conflicts, 1);
    Ok(())
}

#[test]
fn participant_result_shape_rejects_unbounded_parent_reference_lists() -> Result<()> {
    let task_id = task_id("task_1")?;
    let step_id = step_id("inspect")?;
    let attempt_id = task_participant_attempt_id(
        &task_id,
        TaskParticipantPurpose::Step,
        Some(1),
        Some(&step_id),
        1,
    )?;
    let summary = "bounded summary".to_owned();
    let entry = TaskParticipantResultEntry {
        attempt_id,
        task_id,
        summary_hash: sha256_prefixed(&summary),
        output_hash: format!("sha256:{}", "0".repeat(64)),
        summary,
        summary_truncated: false,
        terminal_status: None,
        final_answer_ref: None,
        artifact_refs: Vec::new(),
        changed_paths: vec!["path".to_owned(); TASK_PARTICIPANT_RESULT_CHANGED_PATH_MAX_ITEMS + 1],
        verification_refs: Vec::new(),
    };

    let error = entry
        .validate_shape()
        .expect_err("unbounded changed paths must fail closed");
    assert!(format!("{error:#}").contains("too many changed paths"));
    Ok(())
}

fn resumable_task_entries() -> Result<Vec<SessionLogEntry>> {
    Ok(vec![
        SessionLogEntry::Control(run_entry(TaskRunStatus::Started)?),
        SessionLogEntry::Control(ControlEntry::TaskPlan(TaskPlanEntry {
            task_id: task_id("task_1")?,
            plan_version: 1,
            status: TaskPlanStatus::Accepted,
            steps: vec![read_step("step_1", Vec::new())?],
            reason: Some("accepted v1".to_owned()),
        })),
        SessionLogEntry::Control(run_entry(TaskRunStatus::Paused)?),
    ])
}

fn continuation_selection(
    session_scope_id: &str,
    message_id: &str,
) -> Result<TaskContinuationSelectedEntry> {
    let guidance = "continue with the revised requirement";
    let prompt = project_conversation_prompt_for_persistence(guidance);
    Ok(TaskContinuationSelectedEntry {
        task_id: task_id("task_1")?,
        source_turn: ConversationTurnRef::new(session_scope_id, message_id, "continuation-run-1")?,
        task_status: TaskRunStatus::Paused,
        route_contract_fingerprint: "sha256:continuation-route".to_owned(),
        control: crate::TaskContinuationControlKind::ApplyCurrentRequestAsGuidance,
        prompt_hash: prompt.prompt_hash,
        exact_prompt_required: prompt.exact_prompt_required,
        guidance: prompt.safe_prompt,
        selected_at_ms: 42,
    })
}

#[test]
fn unrelated_chat_clears_task_focus_and_late_task_activity_does_not_reclaim_it() -> Result<()> {
    let mut entries = resumable_task_entries()?;
    let initial = TaskStateProjection::from_entries(&entries);
    assert_eq!(
        initial.current_task().map(|task| task.task_id.as_str()),
        Some("task_1")
    );

    entries.push(SessionLogEntry::User(ModelMessage::user(
        "explain an unrelated module",
    )));
    entries.push(SessionLogEntry::Control(ControlEntry::TaskStep(
        TaskStepEntry {
            task_id: task_id("task_1")?,
            plan_version: 1,
            step_id: step_id("step_1")?,
            role: AgentRole::SubagentRead,
            status: TaskStepStatus::Interrupted,
            title: Some("late task update".to_owned()),
            summary: None,
            reason: Some("arrived after chat admission".to_owned()),
        },
    )));
    entries.push(SessionLogEntry::Control(run_entry(TaskRunStatus::Paused)?));

    let projection = TaskStateProjection::from_entries(&entries);
    assert!(projection.current_task().is_none());
    assert_eq!(
        projection.latest_task().map(|task| task.task_id.as_str()),
        Some("task_1")
    );
    assert_eq!(projection.focus_conflicts, 0);
    Ok(())
}

#[test]
fn explicit_plan_draft_clears_task_focus_and_late_task_activity_does_not_reclaim_it() -> Result<()>
{
    let mut entries = resumable_task_entries()?;
    entries.push(SessionLogEntry::Control(ControlEntry::PlanDraftCreated(
        crate::PlanDraftCreatedEntry {
            plan_id: crate::PlanId::new("plan-explicit-after-task")?,
            schema_version: 2,
            source: crate::PlanSourceRef::default(),
            plan_hash: "sha256:explicit-plan-after-task".to_owned(),
            summary: "Review an unrelated implementation plan".to_owned(),
            inline_text: None,
            steps: Vec::new(),
            intent_proposal: None,
            target_paths: Vec::new(),
            suggested_checks: Vec::new(),
            risk: None,
            notes: Vec::new(),
            workspace_snapshot_id: None,
            created_at_ms: 42,
        },
    )));
    entries.push(SessionLogEntry::Control(ControlEntry::TaskStep(
        TaskStepEntry {
            task_id: task_id("task_1")?,
            plan_version: 1,
            step_id: step_id("step_1")?,
            role: AgentRole::SubagentRead,
            status: TaskStepStatus::Interrupted,
            title: Some("late task update".to_owned()),
            summary: None,
            reason: Some("arrived after explicit plan draft".to_owned()),
        },
    )));

    let projection = TaskStateProjection::from_entries(&entries);
    assert!(projection.current_task().is_none());
    assert_eq!(
        projection.latest_task().map(|task| task.task_id.as_str()),
        Some("task_1")
    );
    Ok(())
}

#[test]
fn exact_continuation_selection_restores_focus_but_stale_plan_fails_closed() -> Result<()> {
    let task = task_id("task_1")?;
    let mut entries = vec![
        SessionLogEntry::Control(run_entry(TaskRunStatus::Started)?),
        SessionLogEntry::Control(ControlEntry::TaskDirectExecutionAdmittedV1(
            TaskDirectExecutionAdmittedV1::task_request(task, "ship planner", 1),
        )),
        SessionLogEntry::Control(run_entry(TaskRunStatus::Paused)?),
    ];
    let mut user = ModelMessage::user("continue with the revised requirement");
    user.id = "continuation-message-1".to_owned();
    entries.push(SessionLogEntry::User(user));
    entries.push(SessionLogEntry::Control(
        ControlEntry::TaskContinuationSelected(continuation_selection(
            "session-1",
            "continuation-message-1",
        )?),
    ));

    let selected = TaskStateProjection::from_entries(&entries);
    assert_eq!(
        selected.current_task().map(|task| task.task_id.as_str()),
        Some("task_1")
    );
    assert_eq!(selected.focus_conflicts, 0);

    entries.push(SessionLogEntry::User(ModelMessage::user(
        "another unrelated request",
    )));
    let mut stale_selection = continuation_selection("session-1", "continuation-message-1")?;
    stale_selection.task_status = TaskRunStatus::Started;
    entries.push(SessionLogEntry::Control(
        ControlEntry::TaskContinuationSelected(stale_selection),
    ));
    let stale = TaskStateProjection::from_entries(&entries);
    assert!(stale.current_task().is_none());
    assert_eq!(stale.focus_conflicts, 1);
    Ok(())
}

#[test]
fn explicit_run_target_selection_restores_focus_but_stale_plan_fails_closed() -> Result<()> {
    let mut entries = resumable_task_entries()?;
    entries.push(SessionLogEntry::User(ModelMessage::user(
        "continue the exact paused task",
    )));
    entries.push(SessionLogEntry::Control(
        ControlEntry::TaskRunCancellationScopeBound(TaskRunCancellationScopeBoundEntry {
            task_id: task_id("task_1")?,
            run_scope_id: "explicit-run-scope-1".to_owned(),
        }),
    ));
    let selected = TaskRunTargetSelectedEntry::new(
        task_id("task_1")?,
        "explicit-run-scope-1",
        TaskRunStatus::Paused,
        Some(1),
        Some(TaskPlanStatus::Accepted),
    );
    entries.push(SessionLogEntry::Control(
        ControlEntry::TaskRunTargetSelected(selected.clone()),
    ));

    let focused = TaskStateProjection::from_entries(&entries);
    assert_eq!(
        focused.current_task().map(|task| task.task_id.as_str()),
        Some("task_1")
    );
    assert_eq!(focused.focus_conflicts, 0);

    entries.push(SessionLogEntry::User(ModelMessage::user(
        "switch back to unrelated chat",
    )));
    entries.push(SessionLogEntry::Control(ControlEntry::TaskPlan(
        TaskPlanEntry {
            task_id: task_id("task_1")?,
            plan_version: 2,
            status: TaskPlanStatus::Accepted,
            steps: vec![read_step("step_2", Vec::new())?],
            reason: Some("accepted v2".to_owned()),
        },
    )));
    entries.push(SessionLogEntry::Control(
        ControlEntry::TaskRunTargetSelected(selected),
    ));
    let stale = TaskStateProjection::from_entries(&entries);
    assert!(stale.current_task().is_none());
    assert_eq!(stale.focus_conflicts, 1);
    Ok(())
}

#[test]
fn task_participant_result_ignores_retired_completion_claim_field() -> Result<()> {
    let task_id = task_id("task_without_legacy_claim")?;
    let step_id = step_id("step_without_legacy_claim")?;
    let attempt_id = task_participant_attempt_id(
        &task_id,
        TaskParticipantPurpose::Step,
        Some(1),
        Some(&step_id),
        1,
    )?;
    let summary = "Current participant result".to_owned();
    let mut value = serde_json::to_value(TaskParticipantResultEntry {
        attempt_id,
        task_id,
        summary_hash: sha256_prefixed(&summary),
        output_hash: format!("sha256:{}", "e".repeat(64)),
        summary: summary.clone(),
        summary_truncated: false,
        terminal_status: Some(TaskParticipantAttemptStatus::Completed),
        final_answer_ref: None,
        artifact_refs: Vec::new(),
        changed_paths: Vec::new(),
        verification_refs: Vec::new(),
    })?;
    value
        .as_object_mut()
        .expect("serialized participant result is an object")
        .insert(
            "completion_claim".to_owned(),
            serde_json::json!({"schema_version": "retired", "subject": [null]}),
        );

    let parsed: TaskParticipantResultEntry = serde_json::from_value(value)?;
    parsed.validate_shape()?;
    assert_eq!(parsed.summary, summary);
    Ok(())
}
