use std::{collections::BTreeSet, sync::Arc, time::Duration};

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use sigil_kernel::{
    Agent, AgentRole, AgentRunInput, AgentRunPurpose, CONTINUE_EXISTING_TASK_TOOL_NAME,
    CONTINUE_WITHOUT_TASK_PLANNING_TOOL_NAME, ControlEntry, ConversationInputKind,
    ConversationInputQueueId, ConversationInputQueuedEntry, ConversationInputStatus,
    ConversationInputTarget, JsonlSessionStore, ModelMessage, MultiAgentMode,
    PlanArtifactProjection, PlanTaskStartMode, ProviderChunk, ReasoningEffort, Session,
    SessionLogEntry, SessionRef, TASK_GUIDANCE_APPLY_TOOL_NAME, TaskAdmissionReason,
    TaskAdmissionTrigger, TaskHandoffRequestedEntry, TaskId, TaskIsolationMode, TaskPauseRequest,
    TaskPlanEntry, TaskPlanStatus, TaskRoutingPolicy, TaskRunEntry, TaskRunStatus, TaskStepId,
    TaskStepMode, TaskStepSpec, TaskStepStatus, Tool, ToolAccess, ToolCall, ToolCategory,
    ToolContext, ToolPreviewCapability, ToolRegistry, ToolResult, ToolResultMeta, ToolSpec,
    project_conversation_prompt_for_persistence,
};
use tempfile::tempdir;

use super::{
    super::{WorkerCommand, WorkerMessage},
    common::{
        PlannedProvider, StreamPlan, planned_role_provider_builder,
        planned_role_provider_builder_with_stream_start_signal, routed_test_root_config,
        routed_unauthenticated_test_root_config, spawn_test_worker,
        spawn_test_worker_with_role_provider_builder, submit_plan_draft_chunks, test_root_config,
        wait_for_session_entry,
    },
};

struct PlannerDiscoveryReadTool;

#[async_trait]
impl Tool for PlannerDiscoveryReadTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "read_file".to_owned(),
            description: "Read one workspace file during planner discovery tests.".to_owned(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" }
                },
                "required": ["path"]
            }),
            category: ToolCategory::File,
            access: ToolAccess::Read,
            network_effect: None,
            preview: ToolPreviewCapability::None,
        }
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        call_id: String,
        _args: serde_json::Value,
    ) -> Result<ToolResult> {
        Ok(ToolResult::ok(
            call_id,
            "read_file",
            "read contents",
            ToolResultMeta::default(),
        ))
    }
}

fn task_workspace_read_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(PlannerDiscoveryReadTool));
    registry
}

#[test]
fn ordinary_chat_auto_handoff_runs_durable_task_under_the_same_worker_run() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-auto-task-handoff-e2e.jsonl");
    let mut root_config = routed_test_root_config(&workspace_root, "planned-model");
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
        Agent::new(provider, task_workspace_read_registry()),
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
    assert_eq!(
        status,
        TaskRunStatus::Completed,
        "unexpected durable task entries: {entries:#?}"
    );
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
fn task_planner_question_resumes_under_the_same_supervised_tui_task() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-task-planner-question-e2e.jsonl");
    let mut root_config = routed_test_root_config(&workspace_root, "planned-model");
    root_config.task.routing_policy = TaskRoutingPolicy::Auto;
    let handoff_args = r#"{"reason_codes":["cross_layer","long_verification"]}"#;
    let provider = PlannedProvider::new(vec![StreamPlan::Chunks(vec![
        ProviderChunk::ToolCallStart {
            id: "question-handoff-call".to_owned(),
            name: "request_task_planning".to_owned(),
        },
        ProviderChunk::ToolCallArgsDelta {
            id: "question-handoff-call".to_owned(),
            delta: handoff_args.to_owned(),
        },
        ProviderChunk::ToolCallComplete(ToolCall {
            id: "question-handoff-call".to_owned(),
            name: "request_task_planning".to_owned(),
            args_json: handoff_args.to_owned(),
        }),
        ProviderChunk::Done,
    ])]);
    let question_args = r#"{
        "prompt": "Choose the subsystem to inspect",
        "questions": [{
            "id": "scope",
            "header": "Scope",
            "question": "Which subsystem should the task inspect?",
            "required": true,
            "field": {
                "kind": "text",
                "multiline": false,
                "max_chars": 128
            }
        }]
    }"#;
    let plan_args = r#"{
        "plan_version": 1,
        "status": "accepted",
        "steps": [{
            "step_id": "inspect_runtime",
            "title": "Inspect the selected runtime subsystem",
            "role": "executor",
            "mode": "read",
            "isolation": "shared_read_only"
        }]
    }"#;
    let role_provider_builder = planned_role_provider_builder(vec![
        StreamPlan::Chunks(vec![
            ProviderChunk::ToolCallStart {
                id: "task-planner-question-call".to_owned(),
                name: sigil_kernel::REQUEST_USER_INPUT_TOOL_NAME.to_owned(),
            },
            ProviderChunk::ToolCallArgsDelta {
                id: "task-planner-question-call".to_owned(),
                delta: question_args.to_owned(),
            },
            ProviderChunk::ToolCallComplete(ToolCall {
                id: "task-planner-question-call".to_owned(),
                name: sigil_kernel::REQUEST_USER_INPUT_TOOL_NAME.to_owned(),
                args_json: question_args.to_owned(),
            }),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::ToolCallStart {
                id: "task-plan-after-answer".to_owned(),
                name: sigil_kernel::TASK_PLAN_UPDATE_TOOL_NAME.to_owned(),
            },
            ProviderChunk::ToolCallArgsDelta {
                id: "task-plan-after-answer".to_owned(),
                delta: plan_args.to_owned(),
            },
            ProviderChunk::ToolCallComplete(ToolCall {
                id: "task-plan-after-answer".to_owned(),
                name: sigil_kernel::TASK_PLAN_UPDATE_TOOL_NAME.to_owned(),
                args_json: plan_args.to_owned(),
            }),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta("task step completed after clarification".to_owned()),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta("task synthesis completed after clarification".to_owned()),
            ProviderChunk::Done,
        ]),
    ]);
    let worker = spawn_test_worker_with_role_provider_builder(
        root_config,
        session_log_path.clone(),
        Agent::new(provider, task_workspace_read_registry()),
        workspace_root,
        role_provider_builder,
    )?;

    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "inspect the selected subsystem and verify the handoff".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunStarted { .. }))?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::TaskRunStarted { .. }))?;
    let paused = worker.recv_until_with_timeout(Duration::from_secs(10), |message| {
        matches!(
            message,
            WorkerMessage::TaskRunFinished {
                status: TaskRunStatus::Paused,
                ..
            }
        )
    })?;
    let WorkerMessage::TaskRunFinished { entries, .. } = paused else {
        unreachable!("recv_until only returns paused TaskRunFinished");
    };
    let task_id = entries
        .iter()
        .rev()
        .find_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::TaskRun(run)) => Some(run.task_id.clone()),
            _ => None,
        })
        .expect("paused task must remain projected");
    let route_id = entries
        .iter()
        .rev()
        .find_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::AgentUserInputRoute(route))
                if route.budget_scope_id == task_id =>
            {
                Some(route.route_id.clone())
            }
            _ => None,
        })
        .expect("paused planner task must retain its exact attention route");
    let requested = worker.recv_until_with_timeout(Duration::from_secs(10), |message| {
        matches!(message, WorkerMessage::UserInputRequested { .. })
    })?;
    let WorkerMessage::UserInputRequested { request, .. } = requested else {
        unreachable!("recv_until only returns UserInputRequested");
    };
    assert!(matches!(
        request.source,
        sigil_kernel::UserInputSourceV1::Planner { .. }
    ));

    worker.send(WorkerCommand::SubmitUserInputDecision {
        command_id: Some("task-planner-question-e2e-answer".to_owned()),
        request_id: request.identity.request_id.as_str().to_owned(),
        generation: request.identity.generation,
        expected_request_hash: request.request_hash.clone(),
        decision: sigil_kernel::UserInputDecisionV1::Submitted {
            answers: vec![sigil_kernel::UserInputAnswerV1 {
                question_id: "scope".to_owned(),
                value: sigil_kernel::UserInputAnswerValueV1::Text {
                    value: "runtime".to_owned(),
                },
            }],
        },
    })?;
    let _ = worker.recv_until_with_timeout(Duration::from_secs(10), |message| {
        matches!(message, WorkerMessage::TaskRunStarted { .. })
    })?;
    let completed = worker.recv_until_with_timeout(Duration::from_secs(10), |message| {
        matches!(
            message,
            WorkerMessage::TaskRunFinished {
                status: TaskRunStatus::Completed,
                ..
            }
        )
    })?;
    let WorkerMessage::TaskRunFinished {
        status, entries, ..
    } = completed
    else {
        unreachable!("recv_until only returns completed TaskRunFinished");
    };
    assert_eq!(status, TaskRunStatus::Completed);
    let projection = sigil_kernel::AgentUserInputRouteProjectionV1::from_session_entries(&entries)?;
    assert_eq!(projection.pending().count(), 0);
    assert_eq!(
        projection.route(&route_id).map(|route| route.status),
        Some(sigil_kernel::AgentRouteStatus::Resolved)
    );
    let task = Session::load_from_store(
        "planned",
        "planned-model",
        JsonlSessionStore::new(&session_log_path)?,
    )?
    .task_state_projection()
    .tasks
    .get(&task_id)
    .cloned()
    .expect("completed task must remain durable");
    let planner_attempts = task
        .participant_attempts_for(sigil_kernel::TaskParticipantPurpose::Planner, None, None)
        .into_iter()
        .collect::<Vec<_>>();
    assert_eq!(planner_attempts.len(), 1);
    assert_eq!(
        planner_attempts[0].status,
        sigil_kernel::TaskParticipantAttemptStatus::Completed
    );
    worker.shutdown()?;
    Ok(())
}

#[test]
fn automatic_handoff_task_can_pause_stop_and_resume_on_its_inherited_run_scope() -> Result<()> {
    let worker_timeout = Duration::from_secs(30);
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-auto-task-pause-resume-e2e.jsonl");
    let mut root_config = routed_test_root_config(&workspace_root, "planned-model");
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
    let (role_provider_builder, role_stream_started_rx) =
        planned_role_provider_builder_with_stream_start_signal(vec![
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
            StreamPlan::Pending,
            StreamPlan::Pending,
            StreamPlan::Chunks(vec![
                ProviderChunk::TextDelta("resumed task completed".to_owned()),
                ProviderChunk::Done,
            ]),
            StreamPlan::Chunks(vec![
                ProviderChunk::TextDelta("resumed task synthesis completed".to_owned()),
                ProviderChunk::Done,
            ]),
        ]);
    let worker = spawn_test_worker_with_role_provider_builder(
        root_config,
        session_log_path.clone(),
        Agent::new(provider, task_workspace_read_registry()),
        workspace_root,
        role_provider_builder,
    )?;

    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "inspect the runtime, pause safely, then resume".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let _ = worker
        .recv_until_with_timeout(worker_timeout, |message| {
            matches!(message, WorkerMessage::RunStarted { .. })
        })
        .context("waiting for root run start before automatic handoff")?;
    let started = worker
        .recv_until_with_timeout(worker_timeout, |message| {
            matches!(message, WorkerMessage::TaskRunStarted { .. })
        })
        .context("waiting for automatic durable task start")?;
    let WorkerMessage::TaskRunStarted { task_id, .. } = started else {
        unreachable!("recv_until only returns TaskRunStarted");
    };
    let expected_task_id = TaskId::new(task_id)?;
    role_stream_started_rx
        .recv_timeout(worker_timeout)
        .context("waiting for task planner provider stream to start")?;
    role_stream_started_rx
        .recv_timeout(worker_timeout)
        .context("waiting for task executor provider stream to start")?;
    wait_for_session_entry(&session_log_path, |entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TaskStep(step))
                if step.task_id == expected_task_id && step.status == TaskStepStatus::Running
        )
    })
    .context("waiting for the task executor step to become durable and running")?;

    worker.send(WorkerCommand::PauseTask {
        request: TaskPauseRequest::new(expected_task_id.clone(), 1),
    })?;
    let requested = worker
        .recv_until_with_timeout(worker_timeout, |message| {
            matches!(message, WorkerMessage::TaskPauseRequested { .. })
        })
        .context("waiting for exact task pause acknowledgement")?;
    assert!(matches!(
        requested,
        WorkerMessage::TaskPauseRequested { ref task_id }
            if task_id == expected_task_id.as_str()
    ));
    let paused = worker
        .recv_until_with_timeout(worker_timeout, |message| {
            matches!(message, WorkerMessage::TaskRunPaused { .. })
        })
        .context("waiting for quiescent durable task pause")?;
    let WorkerMessage::TaskRunPaused {
        task_id: paused_task_id,
        entries,
        ..
    } = paused
    else {
        unreachable!("recv_until only returns TaskRunPaused");
    };
    assert_eq!(paused_task_id, expected_task_id.as_str());
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::TaskRun(task))
            if task.task_id == expected_task_id && task.status == TaskRunStatus::Paused
    )));
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::TaskStep(step))
            if step.task_id == expected_task_id && step.status == TaskStepStatus::Interrupted
    )));

    worker.send(WorkerCommand::ContinueTask {
        task_id: Some(expected_task_id.as_str().to_owned()),
        guidance: None,
    })?;
    let _ = worker
        .recv_until_with_timeout(worker_timeout, |message| {
            matches!(message, WorkerMessage::TaskRunStarted { .. })
        })
        .context("waiting for paused task to resume")?;
    role_stream_started_rx
        .recv_timeout(worker_timeout)
        .context("waiting for resumed task executor stream to start")?;
    wait_for_session_entry(&session_log_path, |entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TaskStep(step))
                if step.task_id == expected_task_id && step.status == TaskStepStatus::Running
        )
    })
    .context("waiting for the resumed task step to become running")?;

    worker.send(WorkerCommand::CancelRun)?;
    let interrupted = worker
        .recv_until_with_timeout(worker_timeout, |message| {
            matches!(message, WorkerMessage::RunInterrupted { .. })
        })
        .context("waiting for stopped task run to remain resumable")?;
    let WorkerMessage::RunInterrupted { entries, .. } = interrupted else {
        unreachable!("recv_until only returns RunInterrupted");
    };
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::TaskRun(task))
            if task.task_id == expected_task_id && task.status == TaskRunStatus::Interrupted
    )));
    assert!(!entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::TaskRun(task))
            if task.task_id == expected_task_id && task.status == TaskRunStatus::Cancelled
    )));

    worker.send(WorkerCommand::ContinueTask {
        task_id: Some(expected_task_id.as_str().to_owned()),
        guidance: None,
    })?;
    let _ = worker
        .recv_until_with_timeout(worker_timeout, |message| {
            matches!(message, WorkerMessage::TaskRunStarted { .. })
        })
        .context("waiting for interrupted task to resume")?;
    let finished = worker
        .recv_until_with_timeout(worker_timeout, |message| {
            matches!(message, WorkerMessage::TaskRunFinished { .. })
        })
        .map_err(|error| {
            let entries = JsonlSessionStore::read_entries(&session_log_path).unwrap_or_default();
            anyhow!(
                "waiting for resumed task completion: {error:#}; durable entries: {}",
                control_entry_debug(&entries)
            )
        })?;
    assert!(matches!(
        finished,
        WorkerMessage::TaskRunFinished {
            ref task_id,
            status: TaskRunStatus::Completed,
            ..
        } if task_id == expected_task_id.as_str()
    ));

    worker.shutdown()?;
    Ok(())
}

#[test]
fn queued_task_guidance_promotes_at_idle_safe_point_and_continues_exact_task() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-task-guidance-e2e.jsonl");
    let store = JsonlSessionStore::new(&session_log_path)?;
    let mut session = Session::load_from_store("planned", "planned-model", store)?;
    let task_id = TaskId::new("task_guidance_e2e")?;
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref: SessionRef::new_relative(
            session_log_path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("session.jsonl"),
        )?,
        objective: "finish the recovered task".to_owned(),
        title: None,

        status: TaskRunStatus::Paused,
        reason: Some("waiting at a scheduler safe point".to_owned()),
    }))?;
    session.append_control(ControlEntry::TaskPlan(TaskPlanEntry {
        task_id: task_id.clone(),
        plan_version: 1,
        status: TaskPlanStatus::Accepted,
        steps: vec![TaskStepSpec {
            step_id: TaskStepId::new("finish")?,
            title: "Finish the pending work".to_owned(),
            display_name: None,
            detail: None,
            role: AgentRole::Executor,
            depends_on: Vec::new(),
            intent_refs: Vec::new(),
            mode: Some(TaskStepMode::Read),
            isolation: Some(TaskIsolationMode::SharedReadOnly),
        }],
        reason: None,
    }))?;
    let queue_id = ConversationInputQueueId::new("queue_task_guidance_e2e")?;
    let guidance = project_conversation_prompt_for_persistence("prioritize the restart edge");
    session.append_control(ControlEntry::ConversationInputQueued(
        ConversationInputQueuedEntry {
            queue_id: queue_id.clone(),
            target: ConversationInputTarget::Task {
                task_id: task_id.clone(),
            },
            kind: ConversationInputKind::TaskGuidance,
            prompt_hash: guidance.prompt_hash,
            prompt: guidance.safe_prompt,
            reasoning_effort: Some(ReasoningEffort::High),
            created_at_ms: Some(1),
        },
    ))?;
    drop(session);

    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let role_provider_builder = planned_role_provider_builder(vec![
        StreamPlan::Chunks(vec![
            ProviderChunk::ToolCallStart {
                id: "call-task-guidance-apply".to_owned(),
                name: TASK_GUIDANCE_APPLY_TOOL_NAME.to_owned(),
            },
            ProviderChunk::ToolCallArgsDelta {
                id: "call-task-guidance-apply".to_owned(),
                delta: r#"{"reason":"prioritizes_pending_step","target_step_ids":["finish"]}"#
                    .to_owned(),
            },
            ProviderChunk::ToolCallComplete(ToolCall {
                id: "call-task-guidance-apply".to_owned(),
                name: TASK_GUIDANCE_APPLY_TOOL_NAME.to_owned(),
                args_json: r#"{"reason":"prioritizes_pending_step","target_step_ids":["finish"]}"#
                    .to_owned(),
            }),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta("guided step completed".to_owned()),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta("guided task synthesis completed".to_owned()),
            ProviderChunk::Done,
        ]),
    ]);
    let worker = spawn_test_worker_with_role_provider_builder(
        root_config,
        session_log_path,
        Agent::new(
            PlannedProvider::new(Vec::new()),
            task_workspace_read_registry(),
        ),
        workspace_root,
        role_provider_builder,
    )?;

    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::TaskRunStarted { .. }))?;
    let finished = worker.recv_until_with_timeout(Duration::from_secs(10), |message| {
        matches!(message, WorkerMessage::TaskRunFinished { .. })
    })?;
    let WorkerMessage::TaskRunFinished {
        task_id: finished_task_id,
        status,
        entries,
    } = finished
    else {
        unreachable!("recv_until only returns TaskRunFinished");
    };
    assert_eq!(finished_task_id, task_id.as_str());
    assert_eq!(status, TaskRunStatus::Completed);
    assert_eq!(
        entries
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::TaskGuidancePromoted(promoted))
                    if promoted.queue_id == queue_id && promoted.task_id == task_id
            ))
            .count(),
        1
    );
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::TaskGuidanceApplied(applied))
            if applied.queue_id == queue_id
                && applied.task_id == task_id
                && applied.target_step_ids == vec![TaskStepId::new("finish").expect("valid step id")]
    )));
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::ConversationInputStatusChanged(changed))
            if changed.queue_id == queue_id && changed.status == ConversationInputStatus::Delivered
    )));
    assert!(
        entries
            .iter()
            .all(|entry| !matches!(entry, SessionLogEntry::User(_))),
        "task guidance must remain transient instead of entering parent user history"
    );
    assert!(entries.iter().all(|entry| !matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::TaskRun(run))
            if run.reason.as_deref().is_some_and(|reason| reason.contains("prioritize the restart edge"))
    )));
    worker.shutdown()?;
    Ok(())
}

#[derive(Clone, Copy)]
enum TypedTaskContinuationDispatch {
    Direct,
    QueuedRunNext,
}

fn run_typed_task_continuation_from_conversation(
    dispatch: TypedTaskContinuationDispatch,
) -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-typed-task-continuation-e2e.jsonl");
    let store = JsonlSessionStore::new(&session_log_path)?;
    let mut session = Session::load_from_store("planned", "planned-model", store)?;
    let task_id = TaskId::new("typed_task_continuation_e2e")?;
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref: SessionRef::new_relative(
            session_log_path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("session.jsonl"),
        )?,
        objective: "finish the current durable task".to_owned(),
        title: None,
        status: TaskRunStatus::Paused,
        reason: Some("waiting for a semantic follow-up".to_owned()),
    }))?;
    session.append_control(ControlEntry::TaskPlan(TaskPlanEntry {
        task_id: task_id.clone(),
        plan_version: 1,
        status: TaskPlanStatus::Accepted,
        steps: vec![TaskStepSpec {
            step_id: TaskStepId::new("finish")?,
            title: "Finish the pending work".to_owned(),
            display_name: None,
            detail: None,
            role: AgentRole::Executor,
            depends_on: Vec::new(),
            intent_refs: Vec::new(),
            mode: Some(TaskStepMode::Read),
            isolation: Some(TaskIsolationMode::SharedReadOnly),
        }],
        reason: None,
    }))?;
    drop(session);

    let exact_guidance = "finish the task we were already working on";
    let continue_args =
        r#"{"reason":"continue_current_task","action":"apply_current_request_as_guidance"}"#;
    let provider = PlannedProvider::new(vec![StreamPlan::Chunks(vec![
        ProviderChunk::ToolCallStart {
            id: "continue-existing-task".to_owned(),
            name: CONTINUE_EXISTING_TASK_TOOL_NAME.to_owned(),
        },
        ProviderChunk::ToolCallArgsDelta {
            id: "continue-existing-task".to_owned(),
            delta: continue_args.to_owned(),
        },
        ProviderChunk::ToolCallComplete(ToolCall {
            id: "continue-existing-task".to_owned(),
            name: CONTINUE_EXISTING_TASK_TOOL_NAME.to_owned(),
            args_json: continue_args.to_owned(),
        }),
        ProviderChunk::Done,
    ])]);
    let guidance_args = r#"{"reason":"prioritizes_pending_step","target_step_ids":["finish"]}"#;
    let role_provider_builder = planned_role_provider_builder(vec![
        StreamPlan::Chunks(vec![
            ProviderChunk::ToolCallStart {
                id: "apply-conversation-guidance".to_owned(),
                name: TASK_GUIDANCE_APPLY_TOOL_NAME.to_owned(),
            },
            ProviderChunk::ToolCallArgsDelta {
                id: "apply-conversation-guidance".to_owned(),
                delta: guidance_args.to_owned(),
            },
            ProviderChunk::ToolCallComplete(ToolCall {
                id: "apply-conversation-guidance".to_owned(),
                name: TASK_GUIDANCE_APPLY_TOOL_NAME.to_owned(),
                args_json: guidance_args.to_owned(),
            }),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta("continued task step completed".to_owned()),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta("continued task synthesis completed".to_owned()),
            ProviderChunk::Done,
        ]),
    ]);
    let mut root_config = routed_test_root_config(&workspace_root, "planned-model");
    root_config.task.routing_policy = TaskRoutingPolicy::Auto;
    let worker = spawn_test_worker_with_role_provider_builder(
        root_config,
        session_log_path.clone(),
        Agent::new(provider, ToolRegistry::new()),
        workspace_root,
        role_provider_builder,
    )?;

    match dispatch {
        TypedTaskContinuationDispatch::Direct => {
            worker.send(WorkerCommand::SubmitPrompt {
                prompt: exact_guidance.to_owned(),
                reasoning_effort: ReasoningEffort::High,
            })?;
            let _ = worker
                .recv_until(|message| matches!(message, WorkerMessage::RunStarted { .. }))
                .context("direct conversation did not start")?;
        }
        TypedTaskContinuationDispatch::QueuedRunNext => {
            worker.send(WorkerCommand::SetConversationQueuePaused { paused: true })?;
            let _ = worker
                .recv_until(|message| {
                    matches!(
                        message,
                        WorkerMessage::ConversationQueueUpdated { paused: true, .. }
                    )
                })
                .context("conversation queue did not pause")?;
            worker.send(WorkerCommand::QueueConversationInput {
                prompt: exact_guidance.to_owned(),
                kind: ConversationInputKind::Chat,
                target: ConversationInputTarget::MainThread,
                reasoning_effort: ReasoningEffort::High,
            })?;
            let queued = worker
                .recv_until(|message| {
                    matches!(
                        message,
                        WorkerMessage::ConversationQueueUpdated {
                            items,
                            paused: true,
                            ..
                        } if items.len() == 1 && items[0].queued.prompt == exact_guidance
                    )
                })
                .context("conversation follow-up was not durably queued")?;
            let queue_id = match queued {
                WorkerMessage::ConversationQueueUpdated { items, .. } => {
                    items[0].queued.queue_id.clone()
                }
                _ => unreachable!("queue predicate guarantees an update"),
            };
            worker.send(WorkerCommand::PromoteQueuedConversationInput { queue_id })?;
            let _ = worker
                .recv_until_with_timeout(Duration::from_secs(10), |message| {
                    matches!(
                        message,
                        WorkerMessage::ConversationQueueDispatchStarted { prompt, .. }
                            if prompt == exact_guidance
                    )
                })
                .context("Run next did not dispatch the typed Task continuation")?;
        }
    }

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let started = loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            anyhow::bail!("typed Task continuation did not start");
        }
        let message = worker.recv_with_timeout(remaining)?;
        match message {
            WorkerMessage::TaskRunStarted {
                task_id: ref started_task_id,
                ..
            } if started_task_id == task_id.as_str() => break message,
            WorkerMessage::RunFailed(error) => {
                let entries = JsonlSessionStore::read_entries(&session_log_path)?;
                anyhow::bail!(
                    "typed Task continuation failed before start: {error}; durable entries: {}",
                    control_entry_debug(&entries)
                );
            }
            _ => {}
        }
    };
    assert!(matches!(started, WorkerMessage::TaskRunStarted { .. }));
    let finished = worker
        .recv_until_with_timeout(Duration::from_secs(10), |message| {
            matches!(
                message,
                WorkerMessage::TaskRunFinished { task_id: finished, .. }
                    if finished == task_id.as_str()
            )
        })
        .context("typed Task continuation did not reach a terminal state")?;
    let WorkerMessage::TaskRunFinished {
        status, entries, ..
    } = finished
    else {
        unreachable!("recv_until only returns TaskRunFinished");
    };
    assert_eq!(status, TaskRunStatus::Completed);
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::TaskContinuationSelected(selected))
            if selected.task_id == task_id && selected.guidance == exact_guidance
    )));
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::TaskGuidanceApplied(applied))
            if applied.task_id == task_id
                && applied.target_step_ids
                    == vec![TaskStepId::new("finish").expect("valid step id")]
    )));
    assert_eq!(
        entries
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::TaskRunCancellationScopeBound(bound))
                    if bound.task_id == task_id
            ))
            .count(),
        1
    );
    assert_eq!(
        entries
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::User(message)
                    if message.content.as_deref() == Some(exact_guidance)
            ))
            .count(),
        1,
        "conversation continuation guidance must enter parent history exactly once"
    );

    worker.shutdown()?;
    Ok(())
}

#[test]
fn direct_conversation_continues_exact_current_task_with_guidance_review() -> Result<()> {
    run_typed_task_continuation_from_conversation(TypedTaskContinuationDispatch::Direct)
}

#[test]
fn queued_run_next_continues_exact_current_task_with_guidance_review() -> Result<()> {
    run_typed_task_continuation_from_conversation(TypedTaskContinuationDispatch::QueuedRunNext)
}

#[derive(Clone, Copy)]
enum ExplicitTaskContinuationPlan {
    Accepted,
    Missing,
}

fn run_explicit_task_continuation_after_user_clear(
    plan: ExplicitTaskContinuationPlan,
) -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let suffix = match plan {
        ExplicitTaskContinuationPlan::Accepted => "accepted-plan",
        ExplicitTaskContinuationPlan::Missing => "missing-plan",
    };
    let session_log_path = temp.path().join(format!(
        ".sigil/sessions/session-explicit-continue-{suffix}.jsonl"
    ));
    let store = JsonlSessionStore::new(&session_log_path)?;
    let mut session = Session::load_from_store("planned", "planned-model", store)?;
    let task_id = TaskId::new(format!("explicit_continue_{suffix}"))?;
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref: SessionRef::new_relative(
            session_log_path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("session.jsonl"),
        )?,
        objective: "resume the exact recovered task".to_owned(),
        title: None,
        status: TaskRunStatus::Paused,
        reason: Some("waiting for an explicit continuation".to_owned()),
    }))?;
    if matches!(plan, ExplicitTaskContinuationPlan::Accepted) {
        session.append_control(ControlEntry::TaskPlan(TaskPlanEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            status: TaskPlanStatus::Accepted,
            steps: vec![TaskStepSpec {
                step_id: TaskStepId::new("finish")?,
                title: "Finish the recovered work".to_owned(),
                display_name: None,
                detail: None,
                role: AgentRole::Executor,
                depends_on: Vec::new(),
                intent_refs: Vec::new(),
                mode: Some(TaskStepMode::Read),
                isolation: Some(TaskIsolationMode::SharedReadOnly),
            }],
            reason: None,
        }))?;
    }
    session.append_user_message(ModelMessage::user("explain an unrelated module first"))?;
    assert!(
        session.task_state_projection().current_task().is_none(),
        "the ordinary User turn must clear Task focus before /task continue"
    );
    drop(session);

    let mut role_plans = Vec::new();
    if matches!(plan, ExplicitTaskContinuationPlan::Missing) {
        let task_plan_args = r#"{
            "plan_version": 1,
            "status": "accepted",
            "steps": [{
                "step_id": "finish",
                "title": "Finish the recovered work",
                "role": "executor",
                "mode": "read",
                "isolation": "shared_read_only"
            }]
        }"#;
        role_plans.push(StreamPlan::Chunks(vec![
            ProviderChunk::ToolCallStart {
                id: "recover-task-plan".to_owned(),
                name: "task_plan_update".to_owned(),
            },
            ProviderChunk::ToolCallArgsDelta {
                id: "recover-task-plan".to_owned(),
                delta: task_plan_args.to_owned(),
            },
            ProviderChunk::ToolCallComplete(ToolCall {
                id: "recover-task-plan".to_owned(),
                name: "task_plan_update".to_owned(),
                args_json: task_plan_args.to_owned(),
            }),
            ProviderChunk::Done,
        ]));
    }
    role_plans.extend([
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta("explicit continuation step completed".to_owned()),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta("explicit continuation synthesis completed".to_owned()),
            ProviderChunk::Done,
        ]),
    ]);
    let worker = spawn_test_worker_with_role_provider_builder(
        test_root_config(&workspace_root, "planned", "planned-model"),
        session_log_path,
        Agent::new(
            PlannedProvider::new(Vec::new()),
            task_workspace_read_registry(),
        ),
        workspace_root,
        planned_role_provider_builder(role_plans),
    )?;

    worker.send(WorkerCommand::ContinueTask {
        task_id: None,
        guidance: None,
    })?;
    let _ = worker
        .recv_until_with_timeout(Duration::from_secs(10), |message| {
            matches!(
                message,
                WorkerMessage::TaskRunStarted { task_id: started, .. }
                    if started == task_id.as_str()
            )
        })
        .context("explicit Task continuation did not start")?;
    let finished = worker
        .recv_until_with_timeout(Duration::from_secs(10), |message| {
            matches!(
                message,
                WorkerMessage::TaskRunFinished { task_id: finished, .. }
                    if finished == task_id.as_str()
            )
        })
        .context("explicit Task continuation did not finish")?;
    let WorkerMessage::TaskRunFinished {
        status, entries, ..
    } = finished
    else {
        unreachable!("recv_until only returns TaskRunFinished");
    };
    assert_eq!(status, TaskRunStatus::Completed);
    assert_eq!(
        sigil_kernel::TaskStateProjection::from_entries(&entries)
            .current_task()
            .map(|task| &task.task_id),
        Some(&task_id)
    );
    let selections = entries
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::TaskRunTargetSelected(selected))
                if selected.task_id == task_id =>
            {
                Some(selected)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(selections.len(), 1);
    assert_eq!(selections[0].task_status, TaskRunStatus::Paused);
    assert_eq!(
        selections[0].plan_version,
        match plan {
            ExplicitTaskContinuationPlan::Accepted => Some(1),
            ExplicitTaskContinuationPlan::Missing => None,
        }
    );
    assert_eq!(
        entries
            .iter()
            .filter(|entry| matches!(entry, SessionLogEntry::User(_)))
            .count(),
        1,
        "/task continue must not synthesize another parent User turn"
    );

    worker.shutdown()?;
    Ok(())
}

#[test]
fn explicit_continue_refocuses_paused_task_after_an_ordinary_user_turn() -> Result<()> {
    run_explicit_task_continuation_after_user_clear(ExplicitTaskContinuationPlan::Accepted)
}

#[test]
fn explicit_continue_replans_no_plan_task_through_the_shared_continuation_runtime() -> Result<()> {
    run_explicit_task_continuation_after_user_clear(ExplicitTaskContinuationPlan::Missing)
}

#[test]
fn run_next_resumes_paused_task_guidance_after_its_initial_wake_was_consumed() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-paused-task-guidance-run-next.jsonl");
    let store = JsonlSessionStore::new(&session_log_path)?;
    let mut session = Session::load_from_store("planned", "planned-model", store)?;
    let task_id = TaskId::new("paused_task_guidance_run_next")?;
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref: SessionRef::new_relative(
            session_log_path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("session.jsonl"),
        )?,
        objective: "resume task guidance only after Run next".to_owned(),
        title: None,

        status: TaskRunStatus::Paused,
        reason: Some("waiting for user guidance".to_owned()),
    }))?;
    session.append_control(ControlEntry::TaskPlan(TaskPlanEntry {
        task_id: task_id.clone(),
        plan_version: 1,
        status: TaskPlanStatus::Accepted,
        steps: vec![TaskStepSpec {
            step_id: TaskStepId::new("finish")?,
            title: "Finish the pending work".to_owned(),
            display_name: None,
            detail: None,
            role: AgentRole::Executor,
            depends_on: Vec::new(),
            intent_refs: Vec::new(),
            mode: Some(TaskStepMode::Read),
            isolation: Some(TaskIsolationMode::SharedReadOnly),
        }],
        reason: None,
    }))?;
    let queue_id = ConversationInputQueueId::new("queue_paused_task_guidance_run_next")?;
    let guidance = project_conversation_prompt_for_persistence("apply this guidance next");
    session.append_control(ControlEntry::ConversationInputQueued(
        ConversationInputQueuedEntry {
            queue_id: queue_id.clone(),
            target: ConversationInputTarget::Task {
                task_id: task_id.clone(),
            },
            kind: ConversationInputKind::TaskGuidance,
            prompt_hash: guidance.prompt_hash,
            prompt: guidance.safe_prompt,
            reasoning_effort: Some(ReasoningEffort::High),
            created_at_ms: Some(1),
        },
    ))?;
    session.append_control(ControlEntry::ConversationInputQueueControl(
        sigil_kernel::ConversationInputQueueControlEntry {
            action: sigil_kernel::ConversationInputQueueControlAction::Pause,
            reason: Some("exercise Run next wake".to_owned()),
            updated_at_ms: Some(2),
        },
    ))?;
    drop(session);

    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let worker = spawn_test_worker_with_role_provider_builder(
        root_config,
        session_log_path,
        Agent::new(PlannedProvider::new(Vec::new()), ToolRegistry::new()),
        workspace_root,
        planned_role_provider_builder(vec![StreamPlan::Pending]),
    )?;

    let _ = worker.recv_until_with_timeout(Duration::from_secs(3), |message| {
        matches!(message, WorkerMessage::Notice(notice) if notice.contains("task guidance is waiting"))
    })?;
    worker.send(WorkerCommand::PromoteQueuedConversationInput { queue_id })?;
    let started = worker.recv_until_with_timeout(Duration::from_secs(10), |message| {
        matches!(message, WorkerMessage::TaskRunStarted { task_id: started, .. }
            if started == task_id.as_str())
    })?;
    assert!(matches!(started, WorkerMessage::TaskRunStarted { .. }));

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
    let mut root_config = routed_test_root_config(&workspace_root, "planned-model");
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
    let mut root_config = routed_test_root_config(&workspace_root, "planned-model");
    root_config.task.routing_policy = TaskRoutingPolicy::Auto;
    let direct_args = r#"{"reason":"does_not_meet_task_planning_criteria"}"#;
    let provider = PlannedProvider::new(vec![
        StreamPlan::Chunks(vec![
            ProviderChunk::ToolCallStart {
                id: "direct-routing-call".to_owned(),
                name: CONTINUE_WITHOUT_TASK_PLANNING_TOOL_NAME.to_owned(),
            },
            ProviderChunk::ToolCallArgsDelta {
                id: "direct-routing-call".to_owned(),
                delta: direct_args.to_owned(),
            },
            ProviderChunk::ToolCallComplete(ToolCall {
                id: "direct-routing-call".to_owned(),
                name: CONTINUE_WITHOUT_TASK_PLANNING_TOOL_NAME.to_owned(),
                args_json: direct_args.to_owned(),
            }),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta("A concise direct answer.".to_owned()),
            ProviderChunk::Done,
        ]),
    ]);
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
    assert!(entries.iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Assistant(message)
                if message.content.as_deref() == Some("A concise direct answer.")
        )
    }));
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
    let mut session = Session::load_from_store("planned", "planned-model", store)?;
    let parent_session_ref = SessionRef::new_relative(
        session_log_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("session.jsonl"),
    )?;
    let input = AgentRunInput::user("recover this cross-layer task");
    let bound = sigil_runtime::ConversationCoordinator::new(true, TaskRoutingPolicy::Auto)
        .with_route_capability_evidence(sigil_runtime::RouteCapabilityEvidence {
            provider_supports_routing_tools: true,
            route_qualified: true,
        })
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
    let queue_id = ConversationInputQueueId::new("queue_before_recovered_handoff")?;
    let follow_up = project_conversation_prompt_for_persistence(
        "apply this follow-up before starting the recovered task",
    );
    session.append_control(ControlEntry::ConversationInputQueued(
        ConversationInputQueuedEntry {
            queue_id: queue_id.clone(),
            target: ConversationInputTarget::MainThread,
            kind: ConversationInputKind::Chat,
            prompt_hash: follow_up.prompt_hash,
            prompt: follow_up.safe_prompt,
            reasoning_effort: Some(ReasoningEffort::High),
            created_at_ms: Some(32),
        },
    ))?;
    drop(session);

    let mut root_config = routed_test_root_config(&workspace_root, "planned-model");
    root_config.task.routing_policy = TaskRoutingPolicy::Auto;
    let task_plan_args = r#"{
        "plan_version": 1,
        "status": "accepted",
        "steps": [{
            "step_id": "resume_recovered_task",
            "title": "Resume recovered task",
            "role": "executor",
            "mode": "read",
            "isolation": "shared_read_only"
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
    let direct_args = r#"{"reason":"does_not_meet_task_planning_criteria"}"#;
    let worker = spawn_test_worker_with_role_provider_builder(
        root_config,
        session_log_path,
        Agent::new(
            PlannedProvider::new(vec![
                StreamPlan::Chunks(vec![
                    ProviderChunk::ToolCallStart {
                        id: "recovered-follow-up-route".to_owned(),
                        name: CONTINUE_WITHOUT_TASK_PLANNING_TOOL_NAME.to_owned(),
                    },
                    ProviderChunk::ToolCallArgsDelta {
                        id: "recovered-follow-up-route".to_owned(),
                        delta: direct_args.to_owned(),
                    },
                    ProviderChunk::ToolCallComplete(ToolCall {
                        id: "recovered-follow-up-route".to_owned(),
                        name: CONTINUE_WITHOUT_TASK_PLANNING_TOOL_NAME.to_owned(),
                        args_json: direct_args.to_owned(),
                    }),
                    ProviderChunk::Done,
                ]),
                StreamPlan::Chunks(vec![
                    ProviderChunk::TextDelta("follow-up handled first".to_owned()),
                    ProviderChunk::Done,
                ]),
            ]),
            task_workspace_read_registry(),
        ),
        workspace_root,
        role_provider_builder,
    )?;

    let dispatched = worker
        .recv_until_with_timeout(Duration::from_secs(10), |message| {
            matches!(
                message,
                WorkerMessage::ConversationQueueDispatchStarted { .. }
            )
        })
        .context("waiting for queued follow-up dispatch during startup recovery")?;
    assert!(matches!(
        dispatched,
        WorkerMessage::ConversationQueueDispatchStarted { queue_id: dispatched, .. }
            if dispatched == queue_id
    ));
    let _ = worker
        .recv_until_with_timeout(Duration::from_secs(10), |message| {
            matches!(message, WorkerMessage::TaskRunStarted { .. })
        })
        .context("waiting for recovered task to start after queued follow-up")?;
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
            "role": "executor",
            "mode": "read",
            "isolation": "shared_read_only"
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
        Agent::new(
            PlannedProvider::new(Vec::new()),
            task_workspace_read_registry(),
        ),
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
fn explicit_task_planner_uses_configured_discovery_fanout_in_tui_runtime() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-planner-discovery-e2e.jsonl");
    let mut root_config = routed_test_root_config(&workspace_root, "planned-model");
    root_config.task.multi_agent_mode = MultiAgentMode::ExplicitRequestOnly;
    root_config.task.max_planning_research_agents = 2;
    root_config.task.max_subagents = 4;
    let discovery_args = r#"{
        "probes": [
            {
                "probe_id": "kernel",
                "title": "Inspect kernel",
                "objective": "Inspect task contracts",
                "path_hints": ["crates/sigil-kernel"]
            },
            {
                "probe_id": "runtime",
                "title": "Inspect runtime",
                "objective": "Inspect orchestration wiring",
                "path_hints": ["crates/sigil-runtime"]
            }
        ]
    }"#;
    let task_plan_args = r#"{
        "plan_version": 1,
        "status": "accepted",
        "steps": [{
            "step_id": "execute_after_discovery",
            "title": "Execute after discovery",
            "role": "executor",
            "mode": "read",
            "isolation": "shared_read_only"
        }]
    }"#;
    let role_provider_builder = planned_role_provider_builder(vec![
        StreamPlan::Chunks(vec![
            ProviderChunk::ToolCallComplete(ToolCall {
                id: "planner-discovery-call".to_owned(),
                name: sigil_runtime::REQUEST_TASK_DISCOVERY_TOOL_NAME.to_owned(),
                args_json: discovery_args.to_owned(),
            }),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta("kernel discovery complete".to_owned()),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta("runtime discovery complete".to_owned()),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::ToolCallComplete(ToolCall {
                id: "task-plan-after-discovery".to_owned(),
                name: "task_plan_update".to_owned(),
                args_json: task_plan_args.to_owned(),
            }),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta("discovery-backed task completed".to_owned()),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta("discovery-backed synthesis completed".to_owned()),
            ProviderChunk::Done,
        ]),
    ]);
    let registry = task_workspace_read_registry();
    let worker = spawn_test_worker_with_role_provider_builder(
        root_config,
        session_log_path,
        Agent::new(PlannedProvider::new(Vec::new()), registry),
        workspace_root,
        role_provider_builder,
    )?;

    worker.send(WorkerCommand::SubmitTask {
        prompt: "inspect kernel and runtime before implementing".to_owned(),
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
    let explore_threads = entries
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::AgentThreadStarted(started))
                if started.profile_id.as_str() == sigil_runtime::EXPLORE_PROFILE_ID =>
            {
                Some(started.thread_id.clone())
            }
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(explore_threads.len(), 2);
    let completed_explore_threads = entries
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::AgentThreadResultRecorded(result))
                if result.result.status == sigil_kernel::AgentThreadTerminalStatus::Completed
                    && explore_threads.contains(&result.result.thread_id) =>
            {
                Some(result.result.thread_id.clone())
            }
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(completed_explore_threads, explore_threads);
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
    let root_config = routed_unauthenticated_test_root_config(&workspace_root, "planned-model");
    let draft_args = r#"{
  "schema_version": 2,
  "summary": "Inspect approved README plan",
  "steps": [
    {
      "step_id": "inspect-approved-plan",
      "title": "Inspect README.md",
      "role": "executor",
      "depends_on": [],
      "mode": "read",
      "isolation": "shared_read_only",
      "target_paths": ["README.md"]
    },
    {
      "step_id": "report-typo-status",
      "title": "Report whether the approved typo fix is needed",
      "role": "executor",
      "depends_on": ["inspect-approved-plan"],
      "mode": "read",
      "isolation": "shared_read_only",
      "target_paths": ["README.md"]
    }
  ],
  "target_paths": ["README.md"],
  "suggested_checks": ["cargo test -p sigil-tui plan_handoff"]
}"#;
    let provider = PlannedProvider::new(vec![StreamPlan::Chunks(submit_plan_draft_chunks(
        "approved-plan-draft",
        draft_args,
    ))]);
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
    let agent = Agent::new(provider, task_workspace_read_registry());
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
    let _ = worker
        .recv_until(|message| matches!(message, WorkerMessage::PlanRunStarted { .. }))
        .context("explicit plan review did not start")?;
    let finished = worker
        .recv_until_with_timeout(Duration::from_secs(10), |message| {
            matches!(
                message,
                WorkerMessage::PlanRunFinished { .. } | WorkerMessage::RunFailed(_)
            )
        })
        .map_err(|error| {
            let entries = JsonlSessionStore::read_entries(&session_log_path).unwrap_or_default();
            let child_entries = entries
                .iter()
                .find_map(|entry| match entry {
                    SessionLogEntry::Control(ControlEntry::PlanReviewAttempt(attempt)) => {
                        Some(attempt.child_session_ref.resolve(
                            session_log_path.parent().unwrap_or_else(|| std::path::Path::new(".")),
                        ))
                    }
                    _ => None,
                })
                .and_then(|path| JsonlSessionStore::read_entries(path).ok())
                .unwrap_or_default();
            anyhow!(
                "explicit plan review did not finish: {error:#}; durable entries: {entries:?}; child entries: {child_entries:?}"
            )
        })?;
    if let WorkerMessage::RunFailed(error) = &finished {
        return Err(anyhow!("explicit plan review failed: {error}"));
    }
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
        .recv_until(|message| matches!(message, WorkerMessage::TaskCreatedFromPlan { .. }))
        .context("approved plan did not create its task")?;
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
    // RFC-0067: the single adoption authority carries the accepted plan; old multi-record
    // promotion artifacts no longer exist.
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::PlanExecutionAdoptedV1(_))
    )));
    let projection = sigil_kernel::TaskStateProjection::from_entries(&entries);
    let adopted_plan = projection
        .tasks
        .get(&created_task.task_id)
        .and_then(|task| task.plans.get(&1))
        .expect("approved plan should be promoted to an executable task plan");
    assert_eq!(adopted_plan.status, TaskPlanStatus::Accepted);
    assert_eq!(adopted_plan.steps.len(), 2);
    assert!(!entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::CheckSpecRecorded(_))
            | SessionLogEntry::Control(ControlEntry::PlanDecisionRecorded(_))
            | SessionLogEntry::Control(ControlEntry::TaskPlan(_))
    )));

    let started = worker
        .recv_until(|message| matches!(message, WorkerMessage::TaskRunStarted { .. }))
        .context("approved task did not start")?;
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

    let task_projection = sigil_kernel::TaskStateProjection::from_entries(&entries);
    let task_plan = task_projection
        .tasks
        .get(&created_task.task_id)
        .and_then(|task| task.plans.get(&1))
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
    let plan_artifacts = sigil_kernel::PlanArtifactProjection::from_entries(&entries);
    assert!(
        plan_artifacts
            .adoptions
            .values()
            .flatten()
            .any(|adoption| adoption.task_id == created_task.task_id)
    );
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

#[test]
fn ordinary_chat_plan_review_route_commits_typed_draft_and_surfaces_plan_ready() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-auto-plan-review-e2e.jsonl");
    let mut root_config = routed_test_root_config(&workspace_root, "planned-model");
    root_config.task.routing_policy = TaskRoutingPolicy::Auto;
    let review_args = r#"{"reason_codes":["architectural_tradeoff","scope_uncertain"]}"#;
    let draft_args = r#"{
        "schema_version": 2,
        "summary": "Migrate the coordinator",
        "steps": [{
            "step_id": "migrate_1",
            "title": "Migrate coordinator",
            "role": "executor",
            "mode": "write",
            "isolation": "sequential_workspace_write",
            "target_paths": ["src/coordinator.rs"]
        }],
        "target_paths": ["src/coordinator.rs"],
        "suggested_checks": ["cargo test"]
    }"#;
    let provider = PlannedProvider::new(vec![
        StreamPlan::Chunks(vec![
            ProviderChunk::ToolCallStart {
                id: "review-call".to_owned(),
                name: sigil_kernel::REQUEST_PLAN_REVIEW_TOOL_NAME.to_owned(),
            },
            ProviderChunk::ToolCallArgsDelta {
                id: "review-call".to_owned(),
                delta: review_args.to_owned(),
            },
            ProviderChunk::ToolCallComplete(ToolCall {
                id: "review-call".to_owned(),
                name: sigil_kernel::REQUEST_PLAN_REVIEW_TOOL_NAME.to_owned(),
                args_json: review_args.to_owned(),
            }),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::ToolCallStart {
                id: "draft-call".to_owned(),
                name: sigil_kernel::SUBMIT_PLAN_DRAFT_TOOL_NAME.to_owned(),
            },
            ProviderChunk::ToolCallArgsDelta {
                id: "draft-call".to_owned(),
                delta: draft_args.to_owned(),
            },
            ProviderChunk::ToolCallComplete(ToolCall {
                id: "draft-call".to_owned(),
                name: sigil_kernel::SUBMIT_PLAN_DRAFT_TOOL_NAME.to_owned(),
                args_json: draft_args.to_owned(),
            }),
            ProviderChunk::Done,
        ]),
    ]);
    let worker = spawn_test_worker(
        root_config,
        session_log_path.clone(),
        Agent::new(provider, ToolRegistry::new()),
        workspace_root,
    )?;

    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "design the coordinator migration before touching anything".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunStarted { .. }))?;
    let finished = worker.recv_until_with_timeout(Duration::from_secs(10), |message| {
        matches!(message, WorkerMessage::PlanRunFinished { .. })
    })?;
    let WorkerMessage::PlanRunFinished { entries, .. } = finished else {
        unreachable!("recv_until only returns PlanRunFinished");
    };
    assert_eq!(
        entries
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::ConversationRouteDecisionRecorded(
                    decision
                )) if decision.route == sigil_kernel::ConversationRoute::PlanReview
            ))
            .count(),
        1,
        "routing microturn records exactly one PlanReview decision"
    );
    assert_eq!(
        entries
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::PlanDraftCreated(_))
            ))
            .count(),
        1,
        "plan review commits exactly one typed draft"
    );
    assert_eq!(
        entries
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::PlanReviewAttempt(attempt))
                    if attempt.status == sigil_kernel::PlanReviewAttemptStatus::DraftReady
            ))
            .count(),
        1,
        "plan review attempt reaches DraftReady"
    );
    assert_eq!(
        entries
            .iter()
            .filter(|entry| matches!(entry, SessionLogEntry::User(_)))
            .count(),
        1,
        "the original user turn is written exactly once"
    );
    assert!(
        entries.iter().all(|entry| !matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TaskHandoffRequested(_))
        )),
        "plan review must not create a task handoff"
    );
    worker.shutdown()?;
    Ok(())
}

#[test]
fn plan_revision_runs_supervised_review_returns_session_and_surfaces_new_draft() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-revise-plan-e2e.jsonl");
    let mut root_config = routed_test_root_config(&workspace_root, "planned-model");
    root_config.task.routing_policy = TaskRoutingPolicy::Auto;
    let review_args = r#"{"reason_codes":["architectural_tradeoff"]}"#;
    let draft_1_args = r#"{
        "schema_version": 2,
        "summary": "Migrate the coordinator",
        "steps": [{
            "step_id": "migrate_1",
            "title": "Migrate coordinator",
            "role": "executor",
            "mode": "write",
            "isolation": "sequential_workspace_write",
            "target_paths": ["src/coordinator.rs"]
        }],
        "target_paths": ["src/coordinator.rs"],
        "suggested_checks": ["cargo test"]
    }"#;
    let draft_2_args = r#"{
        "schema_version": 2,
        "summary": "Revised coordinator migration",
        "steps": [{
            "step_id": "migrate_2",
            "title": "Revise migration",
            "role": "executor",
            "mode": "write",
            "isolation": "sequential_workspace_write",
            "target_paths": ["src/coordinator.rs"]
        }],
        "target_paths": ["src/coordinator.rs"],
        "suggested_checks": ["cargo test"]
    }"#;
    let provider = PlannedProvider::new(vec![
        StreamPlan::Chunks(vec![
            ProviderChunk::ToolCallStart {
                id: "review-call".to_owned(),
                name: sigil_kernel::REQUEST_PLAN_REVIEW_TOOL_NAME.to_owned(),
            },
            ProviderChunk::ToolCallArgsDelta {
                id: "review-call".to_owned(),
                delta: review_args.to_owned(),
            },
            ProviderChunk::ToolCallComplete(ToolCall {
                id: "review-call".to_owned(),
                name: sigil_kernel::REQUEST_PLAN_REVIEW_TOOL_NAME.to_owned(),
                args_json: review_args.to_owned(),
            }),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::ToolCallStart {
                id: "draft-call".to_owned(),
                name: sigil_kernel::SUBMIT_PLAN_DRAFT_TOOL_NAME.to_owned(),
            },
            ProviderChunk::ToolCallArgsDelta {
                id: "draft-call".to_owned(),
                delta: draft_1_args.to_owned(),
            },
            ProviderChunk::ToolCallComplete(ToolCall {
                id: "draft-call".to_owned(),
                name: sigil_kernel::SUBMIT_PLAN_DRAFT_TOOL_NAME.to_owned(),
                args_json: draft_1_args.to_owned(),
            }),
            ProviderChunk::Done,
        ]),
        StreamPlan::Chunks(vec![
            ProviderChunk::ToolCallStart {
                id: "revision-draft-call".to_owned(),
                name: sigil_kernel::SUBMIT_PLAN_DRAFT_TOOL_NAME.to_owned(),
            },
            ProviderChunk::ToolCallArgsDelta {
                id: "revision-draft-call".to_owned(),
                delta: draft_2_args.to_owned(),
            },
            ProviderChunk::ToolCallComplete(ToolCall {
                id: "revision-draft-call".to_owned(),
                name: sigil_kernel::SUBMIT_PLAN_DRAFT_TOOL_NAME.to_owned(),
                args_json: draft_2_args.to_owned(),
            }),
            ProviderChunk::Done,
        ]),
    ]);
    let worker = spawn_test_worker(
        root_config,
        session_log_path.clone(),
        Agent::new(provider, ToolRegistry::new()),
        workspace_root,
    )?;

    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "design the coordinator migration before touching anything".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunStarted { .. }))?;
    let finished = worker.recv_until_with_timeout(Duration::from_secs(10), |message| {
        matches!(message, WorkerMessage::PlanRunFinished { .. })
    })?;
    let WorkerMessage::PlanRunFinished { entries, .. } = finished else {
        unreachable!("recv_until only returns PlanRunFinished");
    };
    let draft_1 = entries
        .iter()
        .find_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::PlanDraftCreated(draft)) => Some(draft.clone()),
            _ => None,
        })
        .expect("first plan review must commit a typed draft");
    assert_eq!(draft_1.summary, "Migrate the coordinator");

    // Revise runs a supervised second plan review: the worker owns the run, restores the
    // session, and surfaces the new draft through PlanRunFinished like any plan review.
    worker.send(WorkerCommand::RevisePlan {
        plan_id: draft_1.plan_id.as_str().to_owned(),
        expected_plan_hash: draft_1.plan_hash.clone(),
    })?;
    let guidance = worker.recv_until_with_timeout(Duration::from_secs(10), |message| {
        matches!(message, WorkerMessage::UserInputRequested { .. })
    })?;
    let WorkerMessage::UserInputRequested { request, .. } = guidance else {
        unreachable!("recv_until only returns UserInputRequested");
    };
    worker.send(WorkerCommand::SubmitUserInputDecision {
        command_id: Some("revision-guidance-e2e".to_owned()),
        request_id: request.identity.request_id.as_str().to_owned(),
        generation: request.identity.generation,
        expected_request_hash: request.request_hash,
        decision: sigil_kernel::UserInputDecisionV1::Submitted {
            answers: vec![sigil_kernel::UserInputAnswerV1 {
                question_id: "revision_guidance".to_owned(),
                value: sigil_kernel::UserInputAnswerValueV1::Text {
                    value: "Preserve the public API and split the migration step.".to_owned(),
                },
            }],
        },
    })?;
    let started = worker.recv_until_with_timeout(Duration::from_secs(10), |message| {
        matches!(message, WorkerMessage::PlanRunStarted { .. })
    })?;
    let WorkerMessage::PlanRunStarted { prompt } = started else {
        unreachable!("recv_until only returns PlanRunStarted");
    };
    assert!(prompt.contains("plan revision"));
    let revised = worker.recv_until_with_timeout(Duration::from_secs(10), |message| {
        matches!(message, WorkerMessage::PlanRunFinished { .. })
    })?;
    let WorkerMessage::PlanRunFinished { entries, .. } = revised else {
        unreachable!("recv_until only returns PlanRunFinished");
    };
    let drafts = entries
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::PlanDraftCreated(draft)) => Some(draft.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(drafts.len(), 2, "the restored session carries both drafts");
    assert!(
        drafts
            .iter()
            .any(|draft| draft.summary == "Revised coordinator migration"),
        "the revision draft is committed into the returned session"
    );
    assert!(
        drafts
            .iter()
            .any(|draft| draft.summary == "Migrate the coordinator"),
        "the original draft is preserved in the returned session"
    );
    assert_eq!(
        entries
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::PlanDecisionRecorded(decision))
                    if decision.decision == sigil_kernel::PlanDecision::RevisionRequested
            ))
            .count(),
        1,
        "the RevisionRequested decision is durable in the returned session"
    );
    let revised_draft_plan_id = drafts
        .iter()
        .find(|draft| draft.summary == "Revised coordinator migration")
        .map(|draft| draft.plan_id.as_str())
        .unwrap_or("");
    assert!(
        entries.iter().any(|entry| matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::PlanReviewAttempt(attempt))
                if attempt.status == sigil_kernel::PlanReviewAttemptStatus::DraftReady
                    && attempt.plan_id.as_str() == revised_draft_plan_id
        )),
        "the revision attempt terminates as DraftReady in the returned session"
    );
    worker.shutdown()?;
    Ok(())
}
