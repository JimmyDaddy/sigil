use std::time::Duration;

use anyhow::{Result, anyhow};
use sigil_kernel::{
    Agent, AgentRole, ControlEntry, PlanArtifactProjection, PlanDecision, PlanTaskStartMode,
    ProviderChunk, ReasoningEffort, SessionLogEntry, TaskIsolationMode, TaskPlanStatus,
    TaskRunStatus, TaskStepMode, TaskStepStatus, ToolCall, ToolRegistry,
};
use tempfile::tempdir;

use super::{
    super::{WorkerCommand, WorkerMessage},
    common::{
        PlannedProvider, StreamPlan, planned_role_provider_builder,
        spawn_test_worker_with_role_provider_builder, test_root_config,
    },
};

#[test]
fn plan_handoff_run_now_uses_normal_task_planner_and_executes_resulting_plan() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-plan-handoff-e2e.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let task_plan_args = r#"{
        "plan_version": 1,
        "status": "accepted",
        "steps": [
            {
                "step_id": "inspect_approved_plan",
                "title": "Inspect README.md from approved plan",
                "detail": "Preserves the user-approved plan scope before reporting.",
                "role": "executor",
                "mode": "read",
                "isolation": "shared_read_only"
            }
        ],
        "reason": "converted approved plan into normal task plan"
    }"#;
    let provider = PlannedProvider::new(vec![StreamPlan::Chunks(vec![
        ProviderChunk::TextDelta(
            r#"Plan:

```sigil-plan-v1
{
  "summary": "Inspect approved README plan",
  "steps": [
    {
      "id": "inspect-approved-plan",
      "title": "Inspect README.md",
      "target_paths": ["README.md"]
    },
    {
      "id": "report-typo-status",
      "title": "Report whether the approved typo fix is needed",
      "target_paths": ["README.md"]
    }
  ],
  "target_paths": ["README.md"]
}
```
"#
            .to_owned(),
        ),
        ProviderChunk::Done,
    ])]);
    let role_provider_builder = planned_role_provider_builder(vec![
        StreamPlan::Chunks(vec![
            ProviderChunk::ToolCallStart {
                id: "task-plan-call".to_owned(),
                name: "task_plan_update".to_owned(),
            },
            ProviderChunk::ToolCallArgsDelta {
                id: "task-plan-call".to_owned(),
                delta: task_plan_args.to_owned(),
            },
            ProviderChunk::ToolCallComplete(ToolCall {
                id: "task-plan-call".to_owned(),
                name: "task_plan_update".to_owned(),
                args_json: task_plan_args.to_owned(),
            }),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta("approved plan inspection complete".to_owned()),
            ProviderChunk::Done,
        ]),
    ]);
    let agent = Agent::new(provider, ToolRegistry::new());
    let worker = spawn_test_worker_with_role_provider_builder(
        root_config,
        session_log_path.clone(),
        agent,
        workspace_root,
        role_provider_builder,
    )?;

    worker.send(WorkerCommand::SubmitPlanPrompt {
        prompt: "plan README typo review".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::PlanRunStarted { .. }))?;
    let finished =
        worker.recv_until(|message| matches!(message, WorkerMessage::PlanRunFinished { .. }))?;
    let WorkerMessage::PlanRunFinished { entries, .. } = finished else {
        unreachable!("recv_until only returns PlanRunFinished");
    };
    let projection = PlanArtifactProjection::from_entries(&entries);
    let draft = projection
        .latest_pending_plan()
        .expect("plan run should append durable draft")
        .clone();

    worker.send(WorkerCommand::CreateTaskFromPlan {
        plan_id: draft.plan_id.as_str().to_owned(),
        expected_plan_hash: draft.plan_hash.clone(),
        start_mode: PlanTaskStartMode::CreateAndRun,
        permission_grant: None,
    })?;
    let created = worker
        .recv_until(|message| matches!(message, WorkerMessage::TaskCreatedFromPlan { .. }))?;
    let WorkerMessage::TaskCreatedFromPlan {
        entry: created_task,
        start_mode,
        entries,
    } = created
    else {
        unreachable!("recv_until only returns TaskCreatedFromPlan");
    };
    assert_eq!(start_mode, PlanTaskStartMode::CreateAndRun);
    assert_eq!(created_task.plan_id, draft.plan_id);
    assert_eq!(created_task.plan_hash, draft.plan_hash);
    assert_eq!(created_task.task_plan_version, 0);
    assert!(created_task.step_mapping.is_empty());
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::PlanDecisionRecorded(decision))
            if decision.decision == PlanDecision::Accepted
    )));
    assert!(
        !entries
            .iter()
            .any(|entry| matches!(entry, SessionLogEntry::Control(ControlEntry::TaskPlan(_)))),
        "plan handoff must not materialize /plan output into task steps before the /task planner"
    );
    assert!(!entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::CheckSpecRecorded(_))
    )));

    let started =
        worker.recv_until(|message| matches!(message, WorkerMessage::TaskRunStarted { .. }))?;
    assert!(matches!(
        started,
        WorkerMessage::TaskRunStarted { ref objective, .. }
            if objective.contains("Execute the following user-approved structured plan")
                && objective.contains("Inspect README.md")
    ));

    let finished = worker
        .recv_until_with_timeout(Duration::from_secs(10), |message| {
            matches!(
                message,
                WorkerMessage::TaskRunFinished { .. } | WorkerMessage::RunFailed(_)
            )
        })
        .map_err(|error| {
            let entries = sigil_kernel::JsonlSessionStore::read_entries(&session_log_path)
                .unwrap_or_default();
            anyhow!(
                "{error}; durable entries: {}",
                control_entry_debug(&entries)
            )
        })?;
    if let WorkerMessage::RunFailed(error) = &finished {
        return Err(anyhow!("task run failed: {error}"));
    }
    let WorkerMessage::TaskRunFinished {
        task_id,
        status,
        entries,
    } = finished
    else {
        unreachable!("recv_until only returns TaskRunFinished");
    };
    assert_eq!(task_id, created_task.task_id.as_str());
    assert_eq!(status, TaskRunStatus::Completed);

    let task_plan = entries
        .iter()
        .find_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::TaskPlan(plan))
                if plan.task_id == created_task.task_id =>
            {
                Some(plan)
            }
            _ => None,
        })
        .expect("normal /task planner should append an executable task plan");
    assert_eq!(task_plan.status, TaskPlanStatus::Accepted);
    assert_eq!(task_plan.steps.len(), 1);
    let step = &task_plan.steps[0];
    assert_eq!(step.title, "Inspect README.md from approved plan");
    assert_eq!(step.role, AgentRole::Executor);
    assert_eq!(step.effective_mode(), TaskStepMode::Read);
    assert_eq!(
        step.effective_isolation(),
        TaskIsolationMode::SharedReadOnly
    );

    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::TaskStep(step))
            if step.step_id.as_str() == "inspect_approved_plan"
                && step.status == TaskStepStatus::Completed
    )));
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::TaskCreatedFromPlan(created))
            if created.task_id == created_task.task_id
    )));
    assert!(!entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::CheckSpecRecorded(_))
    )));

    worker.shutdown()?;
    Ok(())
}

fn control_entry_debug(entries: &[SessionLogEntry]) -> String {
    entries
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::TaskRun(run)) => Some(format!(
                "TaskRun({:?},{})",
                run.status,
                run.reason.as_deref().unwrap_or("")
            )),
            SessionLogEntry::Control(ControlEntry::TaskPlan(plan)) => Some(format!(
                "TaskPlan({:?},steps={})",
                plan.status,
                plan.steps.len()
            )),
            SessionLogEntry::Control(ControlEntry::TaskStep(step)) => Some(format!(
                "TaskStep({},{:?})",
                step.step_id.as_str(),
                step.status
            )),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" -> ")
}
