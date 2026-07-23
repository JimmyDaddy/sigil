use std::time::Duration;

use anyhow::{Result, anyhow};
use sigil_kernel::{
    Agent, AgentRole, AgentRunInput, AgentRunPurpose, ControlEntry, JsonlSessionStore,
    ModelMessage, PlanArtifactProjection, PlanDecision, PlanTaskStartMode, ProviderChunk,
    ReasoningEffort, Session, SessionLogEntry, SessionRef, TaskAdmissionReason,
    TaskAdmissionTrigger, TaskHandoffRequestedEntry, TaskIsolationMode, TaskPlanStatus,
    TaskRoutingPolicy, TaskRunStatus, TaskStepMode, TaskStepStatus, ToolCall, ToolRegistry,
};
use tempfile::tempdir;

use super::{
    super::{WorkerCommand, WorkerMessage},
    common::{
        PlannedProvider, StreamPlan, planned_role_provider_builder, spawn_test_worker,
        spawn_test_worker_with_role_provider_builder, test_root_config,
    },
};

#[test]
fn ordinary_chat_auto_handoff_runs_durable_task_under_the_same_worker_run() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-auto-task-handoff-e2e.jsonl");
    let mut root_config = test_root_config(&workspace_root, "planned", "planned-model");
    root_config.task.routing_policy = TaskRoutingPolicy::Auto;
    let handoff_args = r#"{"reason_codes":["cross_layer","long_verification"]}"#;
    let provider = PlannedProvider::new(vec![StreamPlan::Chunks(vec![
        ProviderChunk::ToolCallStart {
            id: "handoff-call".to_owned(),
            name: "request_task_planning".to_owned(),
        },
        ProviderChunk::ToolCallArgsDelta {
            id: "handoff-call".to_owned(),
            delta: handoff_args.to_owned(),
        },
        ProviderChunk::ToolCallComplete(ToolCall {
            id: "handoff-call".to_owned(),
            name: "request_task_planning".to_owned(),
            args_json: handoff_args.to_owned(),
        }),
        ProviderChunk::Done,
    ])]);
    let task_plan_args = r#"{
        "plan_version": 1,
        "status": "accepted",
        "steps": [{
            "step_id": "inspect_runtime",
            "title": "Inspect runtime handoff",
            "role": "executor",
            "mode": "read",
            "isolation": "shared_read_only"
        }]
    }"#;
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
            ProviderChunk::TextDelta("durable task completed".to_owned()),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta("durable task synthesis completed".to_owned()),
            ProviderChunk::Done,
        ]),
    ]);
    let worker = spawn_test_worker_with_role_provider_builder(
        root_config,
        session_log_path.clone(),
        Agent::new(provider, ToolRegistry::new()),
        workspace_root,
        role_provider_builder,
    )?;

    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "inspect the runtime and verify the cross-layer handoff".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunStarted { .. }))?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::TaskRunStarted { .. }))?;
    let finished = worker.recv_until_with_timeout(Duration::from_secs(10), |message| {
        matches!(message, WorkerMessage::TaskRunFinished { .. })
    })?;
    let WorkerMessage::TaskRunFinished {
        status, entries, ..
    } = finished
    else {
        unreachable!("recv_until only returns TaskRunFinished");
    };
    assert_eq!(status, TaskRunStatus::Completed);
    assert_eq!(
        entries
            .iter()
            .filter(|entry| matches!(entry, SessionLogEntry::User(_)))
            .count(),
        1,
        "planner and executor prompts must remain transient"
    );
    assert_eq!(
        entries
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::TaskHandoffRequested(_))
            ))
            .count(),
        1
    );
    assert_eq!(
        entries
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::TaskHandoffResolved(_))
            ))
            .count(),
        1
    );
    assert_eq!(
        entries
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::TaskRunCancellationScopeBound(_))
            ))
            .count(),
        1,
        "automatic handoff must bind the task to its inherited root cancellation scope"
    );
    worker.shutdown()?;
    Ok(())
}

#[test]
fn auto_handoff_preflight_failure_persists_and_projects_failed_task_state() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-auto-task-preflight-failure.jsonl");
    let mut root_config = test_root_config(&workspace_root, "planned", "planned-model");
    root_config.task.routing_policy = TaskRoutingPolicy::Auto;
    let handoff_args = r#"{"reason_codes":["cross_layer"]}"#;
    let provider = PlannedProvider::new(vec![StreamPlan::Chunks(vec![
        ProviderChunk::ToolCallStart {
            id: "handoff-preflight-failure".to_owned(),
            name: "request_task_planning".to_owned(),
        },
        ProviderChunk::ToolCallArgsDelta {
            id: "handoff-preflight-failure".to_owned(),
            delta: handoff_args.to_owned(),
        },
        ProviderChunk::ToolCallComplete(ToolCall {
            id: "handoff-preflight-failure".to_owned(),
            name: "request_task_planning".to_owned(),
            args_json: handoff_args.to_owned(),
        }),
        ProviderChunk::Done,
    ])]);
    let worker = spawn_test_worker(
        root_config,
        session_log_path,
        Agent::new(provider, ToolRegistry::new()),
        workspace_root,
    )?;

    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "run a task whose role provider cannot be built".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let finished = worker.recv_until_with_timeout(Duration::from_secs(10), |message| {
        matches!(message, WorkerMessage::TaskRunFinished { .. })
    })?;
    let WorkerMessage::TaskRunFinished {
        status, entries, ..
    } = finished
    else {
        unreachable!("recv_until only returns TaskRunFinished");
    };
    assert_eq!(status, TaskRunStatus::Failed);
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::TaskRun(run))
            if run.status == TaskRunStatus::Failed
    )));
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunFailed(_)))?;
    worker.shutdown()?;
    Ok(())
}

#[test]
fn ordinary_simple_chat_in_auto_mode_remains_a_chat_without_task_admission() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-auto-simple-chat-e2e.jsonl");
    let mut root_config = test_root_config(&workspace_root, "planned", "planned-model");
    root_config.task.routing_policy = TaskRoutingPolicy::Auto;
    let provider = PlannedProvider::new(vec![StreamPlan::Chunks(vec![
        ProviderChunk::TextDelta("A concise direct answer.".to_owned()),
        ProviderChunk::Done,
    ])]);
    let worker = spawn_test_worker_with_role_provider_builder(
        root_config,
        session_log_path,
        Agent::new(provider, ToolRegistry::new()),
        workspace_root,
        planned_role_provider_builder(Vec::new()),
    )?;

    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "what does this symbol mean?".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let finished =
        worker.recv_until(|message| matches!(message, WorkerMessage::RunFinished { .. }))?;
    let WorkerMessage::RunFinished { entries, .. } = finished else {
        unreachable!("recv_until only returns RunFinished");
    };
    assert!(!entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(
            ControlEntry::TaskHandoffRequested(_)
                | ControlEntry::TaskHandoffResolved(_)
                | ControlEntry::TaskRun(_)
        )
    )));
    worker.shutdown()?;
    Ok(())
}

#[test]
fn startup_reconciles_requested_handoff_and_resumes_task_without_replaying_chat_provider()
-> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-auto-handoff-recovery-e2e.jsonl");
    let store = JsonlSessionStore::new(&session_log_path)?;
    let mut session = Session::new("planned", "planned-model").with_store(store);
    let parent_session_ref = SessionRef::new_relative(
        session_log_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("session.jsonl"),
    )?;
    let input = AgentRunInput::user("recover this cross-layer task");
    let bound = sigil_runtime::ConversationCoordinator::new(true, TaskRoutingPolicy::Auto)
        .bind_conversation_input(
            &session,
            input,
            parent_session_ref,
            "foreground-run-crashed",
            None,
            31,
        )?;
    let AgentRunPurpose::Conversation(context) = bound.purpose.expect("conversation purpose")
    else {
        panic!("expected conversation purpose");
    };
    let binding = context.task_handoff.expect("auto handoff binding");
    let mut source_message = ModelMessage::user("recover this cross-layer task");
    source_message.id = binding.source_turn.message_id.clone();
    session.append_user_message(source_message)?;
    session.append_control(ControlEntry::TaskHandoffRequested(
        TaskHandoffRequestedEntry {
            handoff_id: binding.handoff_id,
            source_turn: binding.source_turn,
            trigger: TaskAdmissionTrigger::ModelRequested,
            reason_codes: vec![TaskAdmissionReason::CrossLayer],
            recovery_objective: None,
            policy_snapshot_hash: binding.policy_snapshot_hash,
            requested_at_ms: binding.requested_at_ms,
        },
    ))?;
    drop(session);

    let mut root_config = test_root_config(&workspace_root, "planned", "planned-model");
    root_config.task.routing_policy = TaskRoutingPolicy::Auto;
    let task_plan_args = r#"{
        "plan_version": 1,
        "status": "accepted",
        "steps": [{
            "step_id": "resume_recovered_task",
            "title": "Resume recovered task",
            "role": "executor"
        }]
    }"#;
    let role_provider_builder = planned_role_provider_builder(vec![
        StreamPlan::Chunks(vec![
            ProviderChunk::ToolCallStart {
                id: "recovered-task-plan-call".to_owned(),
                name: "task_plan_update".to_owned(),
            },
            ProviderChunk::ToolCallArgsDelta {
                id: "recovered-task-plan-call".to_owned(),
                delta: task_plan_args.to_owned(),
            },
            ProviderChunk::ToolCallComplete(ToolCall {
                id: "recovered-task-plan-call".to_owned(),
                name: "task_plan_update".to_owned(),
                args_json: task_plan_args.to_owned(),
            }),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta("recovered task completed".to_owned()),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta("recovered task synthesis completed".to_owned()),
            ProviderChunk::Done,
        ]),
    ]);
    let worker = spawn_test_worker_with_role_provider_builder(
        root_config,
        session_log_path,
        Agent::new(PlannedProvider::new(Vec::new()), ToolRegistry::new()),
        workspace_root,
        role_provider_builder,
    )?;

    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::TaskRunStarted { .. }))?;
    let finished = worker.recv_until_with_timeout(Duration::from_secs(10), |message| {
        matches!(message, WorkerMessage::TaskRunFinished { .. })
    })?;
    assert!(matches!(
        finished,
        WorkerMessage::TaskRunFinished {
            status: TaskRunStatus::Completed,
            ..
        }
    ));
    worker.shutdown()?;
    Ok(())
}

#[test]
fn explicit_task_command_uses_typed_handoff_admission_before_planning() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-explicit-task-handoff-e2e.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let task_plan_args = r#"{
        "plan_version": 1,
        "status": "accepted",
        "steps": [{
            "step_id": "execute_explicit_task",
            "title": "Execute explicit task",
            "role": "executor"
        }]
    }"#;
    let role_provider_builder = planned_role_provider_builder(vec![
        StreamPlan::Chunks(vec![
            ProviderChunk::ToolCallStart {
                id: "explicit-task-plan-call".to_owned(),
                name: "task_plan_update".to_owned(),
            },
            ProviderChunk::ToolCallArgsDelta {
                id: "explicit-task-plan-call".to_owned(),
                delta: task_plan_args.to_owned(),
            },
            ProviderChunk::ToolCallComplete(ToolCall {
                id: "explicit-task-plan-call".to_owned(),
                name: "task_plan_update".to_owned(),
                args_json: task_plan_args.to_owned(),
            }),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta("explicit task completed".to_owned()),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta("explicit task synthesis completed".to_owned()),
            ProviderChunk::Done,
        ]),
    ]);
    let worker = spawn_test_worker_with_role_provider_builder(
        root_config,
        session_log_path,
        Agent::new(PlannedProvider::new(Vec::new()), ToolRegistry::new()),
        workspace_root,
        role_provider_builder,
    )?;

    worker.send(WorkerCommand::SubmitTask {
        prompt: "run the explicit durable task".to_owned(),
    })?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::TaskRunStarted { .. }))?;
    let finished = worker.recv_until_with_timeout(Duration::from_secs(10), |message| {
        matches!(message, WorkerMessage::TaskRunFinished { .. })
    })?;
    let WorkerMessage::TaskRunFinished {
        status, entries, ..
    } = finished
    else {
        unreachable!("recv_until only returns TaskRunFinished");
    };
    assert_eq!(status, TaskRunStatus::Completed);
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::TaskHandoffRequested(request))
            if request.trigger == sigil_kernel::TaskAdmissionTrigger::ExplicitTaskCommand
    )));
    assert_eq!(
        entries
            .iter()
            .filter(|entry| matches!(entry, SessionLogEntry::User(_)))
            .count(),
        1
    );
    worker.shutdown()?;
    Ok(())
}

#[test]
fn plan_handoff_run_now_promotes_approved_dag_without_replanning() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-plan-handoff-e2e.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let provider = PlannedProvider::new(vec![StreamPlan::Chunks(vec![
        ProviderChunk::TextDelta(
            r#"Plan:

```sigil-plan-v2
{
  "summary": "Inspect approved README plan",
  "steps": [
    {
      "id": "inspect-approved-plan",
      "title": "Inspect README.md",
      "role": "executor",
      "depends_on": [],
      "mode": "read",
      "isolation": "shared_read_only",
      "target_paths": ["README.md"]
    },
    {
      "id": "report-typo-status",
      "title": "Report whether the approved typo fix is needed",
      "role": "executor",
      "depends_on": ["inspect-approved-plan"],
      "mode": "read",
      "isolation": "shared_read_only",
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
            ProviderChunk::TextDelta("approved plan inspection complete".to_owned()),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta("approved plan report complete".to_owned()),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta("approved plan complete".to_owned()),
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
    assert_eq!(created_task.task_plan_version, 1);
    assert_eq!(created_task.step_mapping.len(), 2);
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::PlanDecisionRecorded(decision))
            if decision.decision == PlanDecision::Accepted
    )));
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::TaskPlan(plan))
            if plan.task_id == created_task.task_id
                && plan.status == TaskPlanStatus::Accepted
                && plan.steps.len() == 2
    )));
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
        .expect("approved plan should be promoted to an executable task plan");
    assert_eq!(task_plan.status, TaskPlanStatus::Accepted);
    assert_eq!(task_plan.steps.len(), 2);
    let step = &task_plan.steps[0];
    assert_eq!(step.title, "Inspect README.md");
    assert_eq!(step.role, AgentRole::Executor);
    assert_eq!(step.effective_mode(), TaskStepMode::Read);
    assert_eq!(
        step.effective_isolation(),
        TaskIsolationMode::SharedReadOnly
    );
    assert_eq!(
        task_plan.steps[1].depends_on,
        vec![task_plan.steps[0].step_id.clone()]
    );

    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::TaskStep(step))
            if step.step_id.as_str() == "inspect-approved-plan"
                && step.status == TaskStepStatus::Completed
    )));
    assert!(!entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::TaskParticipantAttempt(attempt))
            if attempt.purpose == sigil_kernel::TaskParticipantPurpose::Planner
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
    assert!(
        worker
            .recv_until_with_timeout(Duration::from_millis(100), |message| {
                matches!(message, WorkerMessage::RunFailed(_))
            })
            .is_err(),
        "a naturally completed task must not emit a trailing RunFailed"
    );

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
