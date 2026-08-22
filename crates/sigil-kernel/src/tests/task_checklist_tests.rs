use anyhow::Result;

use crate::{
    AgentRole, PlanDraftStep, TaskChecklistItemStatusV1, TaskChecklistUpdateContextV1, TaskId,
    ToolCall, task_checklist_from_plan_steps, task_checklist_update_entry,
};

fn draft_step(step_id: &str, title: &str) -> PlanDraftStep {
    PlanDraftStep {
        step_id: step_id.to_owned(),
        title: title.to_owned(),
        display_name: None,
        detail: None,
        role: Some(AgentRole::Executor),
        depends_on: Vec::new(),
        intent_aliases: Vec::new(),
        mode: None,
        isolation: None,
        target_paths: Vec::new(),
        required_capabilities: Vec::new(),
        deliverables: Vec::new(),
        acceptance_criteria: Vec::new(),
        suggested_checks: Vec::new(),
        risk: None,
        notes: Vec::new(),
    }
}

#[test]
fn one_plan_step_does_not_become_a_task_list() -> Result<()> {
    let checklist = task_checklist_from_plan_steps(
        TaskId::new("task-one")?,
        &[draft_step("only", "Execute task")],
    );
    assert!(checklist.is_none());
    Ok(())
}

#[test]
fn multiple_plan_steps_seed_display_only_checklist() -> Result<()> {
    let checklist = task_checklist_from_plan_steps(
        TaskId::new("task-many")?,
        &[
            draft_step("inspect", "Inspect the current flow"),
            draft_step("fix", "Implement the fix"),
        ],
    )
    .expect("two plan labels produce a checklist");
    checklist.validate()?;
    assert_eq!(checklist.items.len(), 2);
    assert!(
        checklist
            .items
            .iter()
            .all(|item| item.status == TaskChecklistItemStatusV1::Pending)
    );
    Ok(())
}

#[test]
fn model_update_is_bounded_and_has_one_in_progress_item() -> Result<()> {
    let call = ToolCall {
        id: "call-1".to_owned(),
        name: crate::UPDATE_TASK_CHECKLIST_TOOL_NAME.to_owned(),
        args_json: serde_json::json!({
            "items": [
                {"text": "Inspect", "status": "completed"},
                {"text": "Implement", "status": "in_progress"}
            ]
        })
        .to_string(),
    };
    let entry = task_checklist_update_entry(
        &TaskChecklistUpdateContextV1 {
            task_id: TaskId::new("task-update")?,
            current_revision: 1,
        },
        &call,
    )?;
    assert_eq!(entry.revision, 2);
    assert_eq!(entry.items[1].status, TaskChecklistItemStatusV1::InProgress);
    Ok(())
}
