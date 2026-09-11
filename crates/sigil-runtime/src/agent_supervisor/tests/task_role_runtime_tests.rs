use anyhow::Result;
use sigil_kernel::{
    AgentRole, ControlEntry, Session, SessionRef, TaskId, TaskPlanEntry, TaskPlanStatus,
    TaskRunEntry, TaskRunStatus, TaskStepEntry, TaskStepId, TaskStepStatus,
};

use super::{
    configured_max_parallel_changeset_steps, configured_max_parallel_read_steps,
    configured_provider_route_concurrency_limit, task_role_demand_for_continuation,
};

#[test]
fn task_role_parallelism_has_nonzero_independent_bounds_and_a_shared_route_ceiling() {
    let mut config = sigil_kernel::TaskConfig {
        max_parallel_read_steps: 4,
        max_parallel_changeset_steps: 2,
        ..sigil_kernel::TaskConfig::default()
    };
    assert_eq!(configured_max_parallel_read_steps(&config), 4);
    assert_eq!(configured_max_parallel_changeset_steps(&config), 2);
    assert_eq!(configured_provider_route_concurrency_limit(&config), 4);

    config.max_parallel_read_steps = 2;
    config.max_parallel_changeset_steps = 3;
    assert_eq!(configured_provider_route_concurrency_limit(&config), 3);

    config.max_parallel_read_steps = 0;
    config.max_parallel_changeset_steps = 0;
    assert_eq!(configured_max_parallel_read_steps(&config), 1);
    assert_eq!(configured_max_parallel_changeset_steps(&config), 1);
    assert_eq!(configured_provider_route_concurrency_limit(&config), 1);
}

#[test]
fn planned_continuation_demands_only_unfinished_step_roles() -> Result<()> {
    let task_id = TaskId::new("task-role-demand")?;
    let read_step = TaskStepId::new("read")?;
    let write_step = TaskStepId::new("write")?;
    let parent_ref = SessionRef::new_relative("parent.jsonl")?;
    let mut session = Session::new("provider", "model");
    session.append_controls(vec![
        ControlEntry::TaskRun(TaskRunEntry {
            task_id: task_id.clone(),
            parent_session_ref: parent_ref,
            objective: "inspect then write".to_owned(),
            title: None,
            status: TaskRunStatus::Paused,
            reason: None,
        }),
        ControlEntry::TaskPlan(TaskPlanEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            status: TaskPlanStatus::Accepted,
            steps: vec![
                sigil_kernel::TaskStepSpec {
                    step_id: read_step.clone(),
                    title: "inspect".to_owned(),
                    display_name: None,
                    detail: None,
                    role: AgentRole::SubagentRead,
                    depends_on: Vec::new(),
                    intent_refs: Vec::new(),
                    mode: None,
                    isolation: None,
                },
                sigil_kernel::TaskStepSpec {
                    step_id: write_step.clone(),
                    title: "write".to_owned(),
                    display_name: None,
                    detail: None,
                    role: AgentRole::SubagentWrite,
                    depends_on: vec![read_step.clone()],
                    intent_refs: Vec::new(),
                    mode: None,
                    isolation: None,
                },
            ],
            reason: None,
        }),
        ControlEntry::TaskStep(TaskStepEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            step_id: read_step,
            role: AgentRole::SubagentRead,
            status: TaskStepStatus::Completed,
            title: Some("inspect".to_owned()),
            summary: None,
            reason: None,
        }),
    ])?;

    let demand = task_role_demand_for_continuation(&session, &task_id, false)?;
    assert!(!demand.planner);
    assert!(!demand.executor);
    assert!(!demand.subagent_read);
    assert!(demand.subagent_write);
    assert!(demand.synthesis);

    let demand_with_guidance = task_role_demand_for_continuation(&session, &task_id, true)?;
    assert!(demand_with_guidance.planner);
    assert!(demand_with_guidance.subagent_write);
    Ok(())
}
