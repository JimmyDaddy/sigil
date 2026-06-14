use anyhow::Result;

use crate::{
    AgentRole, ControlEntry, Session, SessionLogEntry, SessionRef, TASK_PLAN_UPDATE_TOOL_NAME,
    TaskChildSessionEntry, TaskChildSessionStatus, TaskId, TaskPlanEntry, TaskPlanStatus,
    TaskPlanUpdateContext, TaskRouteId, TaskRouteStatus, TaskRunEntry, TaskRunStatus,
    TaskStateProjection, TaskStepEntry, TaskStepId, TaskStepSpec, TaskStepStatus,
    TaskSubagentApprovalRouteEntry, TaskSubagentElicitationRouteEntry, ToolCall, child_session_ref,
    task_plan_update_entry, task_plan_update_result_content,
};

fn task_id(value: &str) -> Result<TaskId> {
    TaskId::new(value)
}

fn step_id(value: &str) -> Result<TaskStepId> {
    TaskStepId::new(value)
}

fn session_ref(value: &str) -> Result<SessionRef> {
    SessionRef::new_relative(value)
}

fn run_entry(status: TaskRunStatus) -> Result<ControlEntry> {
    Ok(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id("task_1")?,
        parent_session_ref: session_ref("parent.jsonl")?,
        objective: "ship planner".to_owned(),
        status,
        reason: None,
    }))
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

    assert!(TaskRunStatus::Completed.is_terminal());
    assert!(TaskRunStatus::Failed.is_terminal());
    assert!(TaskRunStatus::Cancelled.is_terminal());
    assert!(TaskRunStatus::Interrupted.is_terminal());
    assert!(!TaskRunStatus::Paused.is_terminal());

    assert!(TaskStepStatus::Completed.is_terminal());
    assert!(TaskStepStatus::Blocked.is_terminal());
    assert!(TaskStepStatus::Interrupted.is_terminal());
    assert!(!TaskStepStatus::Running.is_terminal());
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
        reference.resolve(std::path::Path::new(".sigil/sessions")),
        std::path::Path::new(".sigil/sessions/children/task_1/step_2-child_1.jsonl")
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
                detail: Some("read code".to_owned()),
                role: AgentRole::Planner,
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
fn task_plan_update_parses_valid_plan_and_rejects_invalid_shapes() -> Result<()> {
    let context = TaskPlanUpdateContext {
        task_id: task_id("task_1")?,
        max_plan_steps: 1,
    };
    let call = ToolCall {
        id: "call-1".to_owned(),
        name: TASK_PLAN_UPDATE_TOOL_NAME.to_owned(),
        args_json: r#"{"plan_version":1,"status":"accepted","steps":[{"step_id":"step_1","title":"inspect","detail":"read first","role":"planner"}],"reason":"initial"}"#.to_owned(),
    };

    let entry = task_plan_update_entry(&context, &call)?;

    assert_eq!(entry.plan_version, 1);
    assert_eq!(entry.status, TaskPlanStatus::Accepted);
    assert_eq!(entry.steps[0].role, AgentRole::Planner);
    assert_eq!(entry.steps[0].detail.as_deref(), Some("read first"));
    assert!(task_plan_update_result_content(&entry).contains(r#""steps":1"#));

    let wrong_tool = ToolCall {
        name: "other".to_owned(),
        ..call.clone()
    };
    assert!(task_plan_update_entry(&context, &wrong_tool).is_err());

    let zero_version = ToolCall {
        args_json: r#"{"plan_version":0,"status":"accepted","steps":[{"step_id":"step_1","title":"inspect","role":"executor"}]}"#.to_owned(),
        ..call.clone()
    };
    assert!(task_plan_update_entry(&context, &zero_version).is_err());

    let too_many_steps = ToolCall {
        args_json: r#"{"plan_version":1,"status":"accepted","steps":[{"step_id":"step_1","title":"inspect","role":"executor"},{"step_id":"step_2","title":"edit","role":"subagent_write"}]}"#.to_owned(),
        ..call.clone()
    };
    assert!(task_plan_update_entry(&context, &too_many_steps).is_err());

    let unsupported_status = ToolCall {
        args_json: r#"{"plan_version":1,"status":"done","steps":[{"step_id":"step_1","title":"inspect","role":"executor"}]}"#.to_owned(),
        ..call
    };
    assert!(task_plan_update_entry(&context, &unsupported_status).is_err());
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
                detail: None,
                role: AgentRole::Executor,
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
            status: TaskRunStatus::Started,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id("task_a")?,
            parent_session_ref: session_ref("parent.jsonl")?,
            objective: "second".to_owned(),
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
fn task_projection_tracks_latest_unfinished_task_by_replay_order() -> Result<()> {
    let entries = vec![
        SessionLogEntry::Control(ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id("task_1")?,
            parent_session_ref: session_ref("parent.jsonl")?,
            objective: "first".to_owned(),
            status: TaskRunStatus::Failed,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id("task_2")?,
            parent_session_ref: session_ref("parent.jsonl")?,
            objective: "second".to_owned(),
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
            status: TaskRunStatus::Completed,
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id("task_2")?,
            parent_session_ref: session_ref("parent.jsonl")?,
            objective: "second".to_owned(),
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
fn task_projection_supersedes_previous_accepted_plan() -> Result<()> {
    let projection = TaskStateProjection::from_entries(&[
        SessionLogEntry::Control(run_entry(TaskRunStatus::Started)?),
        SessionLogEntry::Control(ControlEntry::TaskPlan(TaskPlanEntry {
            task_id: task_id("task_1")?,
            plan_version: 1,
            status: TaskPlanStatus::Accepted,
            steps: Vec::new(),
            reason: None,
        })),
        SessionLogEntry::Control(ControlEntry::TaskPlan(TaskPlanEntry {
            task_id: task_id("task_1")?,
            plan_version: 2,
            status: TaskPlanStatus::Accepted,
            steps: Vec::new(),
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
