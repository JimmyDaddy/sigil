use anyhow::Result;

use crate::{
    AgentRole, ControlEntry, Session, SessionLogEntry, SessionRef,
    TASK_AGENT_DISPLAY_NAME_MAX_CHARS, TASK_PLAN_UPDATE_TOOL_NAME,
    TaskChildSessionDisplayNameEntry, TaskChildSessionEntry, TaskChildSessionStatus,
    TaskGraphProjection, TaskId, TaskIsolationMode, TaskPlanEntry, TaskPlanStatus,
    TaskPlanUpdateContext, TaskReadyDeferredReason, TaskReadyQueueOptions, TaskRouteId,
    TaskRouteStatus, TaskRunEntry, TaskRunStatus, TaskStateProjection, TaskStepEntry, TaskStepId,
    TaskStepMode, TaskStepProjection, TaskStepSpec, TaskStepStatus, TaskSubagentApprovalRouteEntry,
    TaskSubagentElicitationRouteEntry, ToolCall, child_session_ref,
    normalize_task_agent_display_name, task_plan_update_entry, task_plan_update_result_content,
    task_plan_update_tool_spec, validate_task_plan_graph_steps,
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

fn read_step(id: &str, depends_on: Vec<TaskStepId>) -> Result<TaskStepSpec> {
    Ok(TaskStepSpec {
        step_id: step_id(id)?,
        title: format!("Read {id}"),
        display_name: None,
        detail: None,
        role: AgentRole::SubagentRead,
        depends_on,
        mode: Some(TaskStepMode::Read),
        isolation: Some(TaskIsolationMode::SharedReadOnly),
    })
}

fn write_step(id: &str, depends_on: Vec<TaskStepId>) -> Result<TaskStepSpec> {
    Ok(TaskStepSpec {
        step_id: step_id(id)?,
        title: format!("Write {id}"),
        display_name: None,
        detail: None,
        role: AgentRole::Executor,
        depends_on,
        mode: Some(TaskStepMode::Write),
        isolation: Some(TaskIsolationMode::SequentialWorkspaceWrite),
    })
}

fn step_projection(
    task_id: TaskId,
    plan_version: u32,
    step_id: &str,
    status: TaskStepStatus,
) -> Result<TaskStepProjection> {
    Ok(TaskStepProjection {
        task_id,
        plan_version,
        step_id: crate::TaskStepId::new(step_id)?,
        role: AgentRole::Executor,
        status,
        title: Some(step_id.to_owned()),
        summary: None,
        reason: None,
    })
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
fn task_plan_update_parses_valid_plan_and_rejects_invalid_shapes() -> Result<()> {
    let context = TaskPlanUpdateContext {
        task_id: task_id("task_1")?,
        max_plan_steps: 1,
        max_plan_versions: 1,
    };
    let call = ToolCall {
        id: "call-1".to_owned(),
        name: TASK_PLAN_UPDATE_TOOL_NAME.to_owned(),
        args_json: r#"{"plan_version":1,"status":"accepted","steps":[{"step_id":"step_1","title":"inspect","display_name":"Scout 1","detail":"read first","role":"planner"}],"reason":"initial"}"#.to_owned(),
    };

    let entry = task_plan_update_entry(&context, &call)?;

    assert_eq!(entry.plan_version, 1);
    assert_eq!(entry.status, TaskPlanStatus::Accepted);
    assert_eq!(entry.steps[0].role, AgentRole::Planner);
    assert_eq!(entry.steps[0].display_name.as_deref(), Some("Scout 1"));
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
        ..call.clone()
    };
    assert!(task_plan_update_entry(&context, &unsupported_status).is_err());

    let invalid_display_name = ToolCall {
        args_json: r#"{"plan_version":1,"status":"accepted","steps":[{"step_id":"step_1","title":"inspect","display_name":"bad\nname","role":"executor"}]}"#.to_owned(),
        ..call
    };
    assert!(task_plan_update_entry(&context, &invalid_display_name).is_err());
    Ok(())
}

#[test]
fn task_replan_budget_rejects_plan_versions_beyond_limit() -> Result<()> {
    let context = TaskPlanUpdateContext {
        task_id: task_id("task_1")?,
        max_plan_steps: 1,
        max_plan_versions: 2,
    };
    let call = ToolCall {
        id: "call-1".to_owned(),
        name: TASK_PLAN_UPDATE_TOOL_NAME.to_owned(),
        args_json: r#"{"plan_version":3,"status":"accepted","steps":[{"step_id":"step_1","title":"inspect","role":"planner"}]}"#.to_owned(),
    };

    let error = task_plan_update_entry(&context, &call)
        .expect_err("plan version above bounded replan budget should fail");

    assert!(
        error
            .to_string()
            .contains("task plan version 3 exceeds maximum 2")
    );
    Ok(())
}

#[test]
fn task_plan_update_tool_spec_explains_subagent_delegation_roles() {
    let spec = task_plan_update_tool_spec();

    assert!(spec.description.contains("Do not call task"));
    assert!(spec.description.contains("subagent_read"));
    assert!(spec.description.contains("subagent_write"));
    assert!(spec.input_schema.to_string().contains("display_name"));
    assert!(spec.input_schema.to_string().contains("depends_on"));
    assert!(spec.input_schema.to_string().contains("shared_read_only"));
    assert!(
        spec.input_schema
            .to_string()
            .contains("sequential_workspace_write")
    );
    assert!(
        spec.input_schema
            .to_string()
            .contains("delegated read-only verification")
    );
}

#[test]
fn task_plan_update_normalizes_model_mode_isolation_mismatches() -> Result<()> {
    let context = TaskPlanUpdateContext {
        task_id: task_id("task_1")?,
        max_plan_steps: 3,
        max_plan_versions: 1,
    };
    let call = ToolCall {
        id: "call-1".to_owned(),
        name: TASK_PLAN_UPDATE_TOOL_NAME.to_owned(),
        args_json: r#"{
            "plan_version":1,
            "status":"accepted",
            "steps":[
                {
                    "step_id":"write",
                    "title":"Write",
                    "role":"executor",
                    "mode":"write",
                    "isolation":"shared_read_only"
                },
                {
                    "step_id":"read",
                    "title":"Read",
                    "role":"executor",
                    "mode":"read",
                    "isolation":"sequential_workspace_write"
                },
                {
                    "step_id":"defaulted",
                    "title":"Defaulted",
                    "role":"executor"
                }
            ]
        }"#
        .to_owned(),
    };

    let entry = task_plan_update_entry(&context, &call)?;

    assert_eq!(
        entry.steps[0].effective_isolation(),
        TaskIsolationMode::SequentialWorkspaceWrite
    );
    assert_eq!(
        entry.steps[1].effective_isolation(),
        TaskIsolationMode::SharedReadOnly
    );
    assert_eq!(entry.steps[2].effective_mode(), TaskStepMode::Write);
    assert_eq!(
        entry.steps[2].effective_isolation(),
        TaskIsolationMode::SequentialWorkspaceWrite
    );
    Ok(())
}

#[test]
fn task_dag_schema_parses_valid_metadata_and_projects_graph() -> Result<()> {
    let context = TaskPlanUpdateContext {
        task_id: task_id("task_1")?,
        max_plan_steps: 3,
        max_plan_versions: 1,
    };
    let call = ToolCall {
        id: "call-1".to_owned(),
        name: TASK_PLAN_UPDATE_TOOL_NAME.to_owned(),
        args_json: r#"{
            "plan_version":1,
            "status":"accepted",
            "steps":[
                {
                    "step_id":"explore",
                    "title":"Explore",
                    "role":"subagent_read",
                    "mode":"read",
                    "isolation":"shared_read_only"
                },
                {
                    "step_id":"implement",
                    "title":"Implement",
                    "role":"executor",
                    "depends_on":["explore"],
                    "mode":"write",
                    "isolation":"sequential_workspace_write"
                },
                {
                    "step_id":"verify",
                    "title":"Verify",
                    "role":"executor",
                    "depends_on":["implement"],
                    "mode":"verify",
                    "isolation":"shared_read_only"
                }
            ]
        }"#
        .to_owned(),
    };

    let entry = task_plan_update_entry(&context, &call)?;

    assert_eq!(entry.steps[0].effective_mode(), TaskStepMode::Read);
    assert_eq!(
        entry.steps[1].effective_isolation(),
        TaskIsolationMode::SequentialWorkspaceWrite
    );
    assert_eq!(entry.steps[2].depends_on, vec![step_id("implement")?]);

    let graph = TaskGraphProjection::from_plan_entry(&entry)?;
    assert_eq!(graph.graph_version, 1);
    assert_eq!(graph.steps.len(), 3);
    assert_eq!(graph.steps[0].mode, TaskStepMode::Read);
    assert_eq!(graph.steps[1].depends_on, vec![step_id("explore")?]);

    let projection = TaskStateProjection::from_entries(&[SessionLogEntry::Control(
        ControlEntry::TaskPlan(entry),
    )]);
    let projected_graph = projection
        .latest_task()
        .and_then(|task| task.plans.get(&1))
        .and_then(|plan| plan.graph.as_ref())
        .expect("accepted plan should project valid task graph");
    assert_eq!(projected_graph.steps[2].mode, TaskStepMode::Verify);
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
        mode: Some(TaskStepMode::Write),
        isolation: Some(TaskIsolationMode::SequentialWorkspaceWrite),
    };
    validate_task_plan_graph_steps(&[read_step.clone(), write_step.clone()])?;

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
fn task_dag_read_only_ready_queue_batches_independent_read_steps() -> Result<()> {
    let graph = TaskGraphProjection::from_plan_entry(&TaskPlanEntry {
        task_id: task_id("task_1")?,
        plan_version: 1,
        status: TaskPlanStatus::Accepted,
        steps: vec![
            read_step("read_a", Vec::new())?,
            read_step("read_b", Vec::new())?,
            write_step("write", vec![step_id("read_a")?, step_id("read_b")?])?,
        ],
        reason: None,
    })?;

    let queue = graph.ready_queue(
        &std::collections::BTreeMap::new(),
        TaskReadyQueueOptions::new(2),
    );

    assert_eq!(
        queue
            .read_only_batch
            .iter()
            .map(|step| step.step_id.as_str())
            .collect::<Vec<_>>(),
        vec!["read_a", "read_b"]
    );
    assert!(queue.sequential_step.is_none());
    assert!(queue.deferred.iter().all(|step| {
        step.reason == TaskReadyDeferredReason::SequentialWrite
            || step.reason == TaskReadyDeferredReason::ConcurrencyBudget
    }));
    Ok(())
}

#[test]
fn task_dag_read_only_ready_queue_respects_concurrency_budget() -> Result<()> {
    let graph = TaskGraphProjection::from_plan_entry(&TaskPlanEntry {
        task_id: task_id("task_1")?,
        plan_version: 1,
        status: TaskPlanStatus::Accepted,
        steps: vec![
            read_step("read_a", Vec::new())?,
            read_step("read_b", Vec::new())?,
        ],
        reason: None,
    })?;

    let queue = graph.ready_queue(
        &std::collections::BTreeMap::new(),
        TaskReadyQueueOptions::new(1),
    );

    assert_eq!(queue.read_only_batch.len(), 1);
    assert_eq!(queue.read_only_batch[0].step_id.as_str(), "read_a");
    assert_eq!(queue.deferred.len(), 1);
    assert_eq!(queue.deferred[0].step_id.as_str(), "read_b");
    assert_eq!(
        queue.deferred[0].reason,
        TaskReadyDeferredReason::ConcurrencyBudget
    );
    Ok(())
}

#[test]
fn task_dag_read_only_ready_queue_keeps_write_steps_sequential() -> Result<()> {
    let graph = TaskGraphProjection::from_plan_entry(&TaskPlanEntry {
        task_id: task_id("task_1")?,
        plan_version: 1,
        status: TaskPlanStatus::Accepted,
        steps: vec![
            read_step("read", Vec::new())?,
            write_step("write", Vec::new())?,
        ],
        reason: None,
    })?;

    let queue = graph.ready_queue(
        &std::collections::BTreeMap::new(),
        TaskReadyQueueOptions::new(4),
    );

    assert_eq!(queue.read_only_batch.len(), 1);
    assert_eq!(queue.read_only_batch[0].step_id.as_str(), "read");
    assert!(queue.sequential_step.is_none());
    assert_eq!(queue.deferred.len(), 1);
    assert_eq!(queue.deferred[0].step_id.as_str(), "write");
    assert_eq!(
        queue.deferred[0].reason,
        TaskReadyDeferredReason::SequentialWrite
    );

    let completed_read_statuses = std::collections::BTreeMap::from([(
        (1, step_id("read")?),
        step_projection(task_id("task_1")?, 1, "read", TaskStepStatus::Completed)?,
    )]);
    let write_queue = graph.ready_queue(&completed_read_statuses, TaskReadyQueueOptions::new(4));
    assert!(write_queue.read_only_batch.is_empty());
    assert_eq!(
        write_queue
            .sequential_step
            .as_ref()
            .map(|step| step.step_id.as_str()),
        Some("write")
    );
    Ok(())
}

#[test]
fn task_dag_read_only_ready_queue_blocks_when_write_is_running() -> Result<()> {
    let graph = TaskGraphProjection::from_plan_entry(&TaskPlanEntry {
        task_id: task_id("task_1")?,
        plan_version: 1,
        status: TaskPlanStatus::Accepted,
        steps: vec![
            write_step("write", Vec::new())?,
            read_step("read", Vec::new())?,
        ],
        reason: None,
    })?;
    let statuses = std::collections::BTreeMap::from([(
        (1, step_id("write")?),
        step_projection(task_id("task_1")?, 1, "write", TaskStepStatus::Running)?,
    )]);

    let queue = graph.ready_queue(&statuses, TaskReadyQueueOptions::new(4));

    assert!(queue.read_only_batch.is_empty());
    assert!(queue.sequential_step.is_none());
    assert!(queue.deferred.iter().any(|step| {
        step.step_id.as_str() == "read" && step.reason == TaskReadyDeferredReason::RunningWrite
    }));
    Ok(())
}

#[test]
fn task_dag_ready_queue_blocks_when_write_lease_is_active() -> Result<()> {
    let graph = TaskGraphProjection::from_plan_entry(&TaskPlanEntry {
        task_id: task_id("task_1")?,
        plan_version: 1,
        status: TaskPlanStatus::Accepted,
        steps: vec![
            read_step("read", Vec::new())?,
            write_step("write", Vec::new())?,
        ],
        reason: None,
    })?;

    let queue = graph.ready_queue_with_active_write_lease(
        &std::collections::BTreeMap::new(),
        TaskReadyQueueOptions::new(4),
        true,
    );

    assert!(queue.read_only_batch.is_empty());
    assert!(queue.sequential_step.is_none());
    assert_eq!(
        queue
            .deferred
            .iter()
            .map(|step| (step.step_id.as_str().to_owned(), step.reason))
            .collect::<Vec<_>>(),
        vec![
            ("read".to_owned(), TaskReadyDeferredReason::ActiveWriteLease),
            (
                "write".to_owned(),
                TaskReadyDeferredReason::ActiveWriteLease
            ),
        ]
    );
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
        mode: Some(TaskStepMode::Verify),
        isolation: Some(TaskIsolationMode::SharedReadOnly),
    };

    assert!(review_step.is_review_advisory());
    assert!(!review_step.requires_system_verifier());
    assert!(!verify_step.is_review_advisory());
    assert!(verify_step.requires_system_verifier());

    let entry = TaskPlanEntry {
        task_id: task_id("task_1")?,
        plan_version: 1,
        status: TaskPlanStatus::Accepted,
        steps: vec![review_step, verify_step],
        reason: None,
    };
    let graph = TaskGraphProjection::from_plan_entry(&entry)?;
    assert_eq!(graph.steps[0].mode, TaskStepMode::Review);
    assert_eq!(graph.steps[1].mode, TaskStepMode::Verify);
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
