use std::{
    fs,
    path::PathBuf,
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

use crate::runner::TerminalTaskControlIdentity;
use anyhow::Result;
use sigil_kernel::{
    Agent, AgentInvocationMode, AgentInvocationSource, AgentProfileId, AgentProfileSnapshotId,
    AgentResultContinuationEntry, AgentResultContinuationStatus, AgentRole,
    AgentRunContextSnapshot, AgentRunDisposition, AgentRunOutcome, AgentRunOutput, AgentRunResult,
    AgentThreadId, AgentThreadStartedEntry, AgentThreadStatus, AgentThreadStatusChangedEntry,
    ControlEntry, DEFAULT_TASK_VERIFICATION_SCOPE_HASH, DurableEventType, ExecutionCleanupStatus,
    JsonlSessionStore, McpElicitationDecision, McpElicitationEntry, ModelMessage,
    MutationEventRecorder, PlanDecision, PlanDecisionActor, PlanDecisionRecordedEntry,
    PlanSourceRef, PlanTaskStartMode, Provider, PublicIntentStackStateV1, ReasoningEffort,
    RootConfig, Session, SessionLogEntry, SessionRef, SessionStreamRecord, TaskChildSessionEntry,
    TaskChildSessionStatus, TaskCreatedFromPlanEntry, TaskId, TaskPlanEntry, TaskPlanStatus,
    TaskRouteStatus, TaskRunEntry, TaskRunStatus, TaskStepEntry, TaskStepId, TaskStepSpec,
    TaskStepStatus, TerminalTaskEntry, TerminalTaskHandle, TerminalTaskId, TerminalTaskStatus,
    ToolCall, ToolContext, ToolEffect, ToolExecutionEntry, ToolExecutionStatus, ToolRegistry,
    ToolResultMeta, UsageStats, VerificationScope, WorkspaceMutationDetected,
    WorkspaceRootSnapshot, plan_draft_created_entry, plan_task_input_from_draft,
    session_io_lock_metrics, task_id_from_plan_draft, task_plan_from_plan_draft,
};
use sigil_runtime::McpRuntimeEventHandler;
use tempfile::tempdir;

use super::{
    super::{
        McpActivationStatus, WorkerCommand, WorkerCommandSender, WorkerMessage,
        elicitation_bridge::ChannelMcpElicitationHandler,
        mcp_event_bridge::{ChannelMcpRuntimeEventHandler, McpRuntimeEvent},
        terminal_lifecycle_bridge::ChannelTerminalLifecycleRouter,
        worker_event::WorkerMcpRuntimeEventSender,
        worker_loop::{
            CreateTaskFromPlanRequest, RuntimeTaskRoleProviderBuilder, WorkerLoopMcpHandlers,
            WorkerLoopTerminalRuntime, agent_result_continuation_run_result,
            append_mcp_elicitation_audits, artifact_gc_task_metrics, cancel_terminal_task,
            close_agent_thread, create_task_from_plan, durable_terminal_tool_result_metadata,
            next_task_id, partition_agent_result_continuations,
            pending_agent_continuations_from_active_projection,
            pending_agent_result_continuations_from_session, plan_handoff_workspace_snapshot_id,
            queued_background_ready_transient_context, resolve_continue_task, run_worker_loop,
            session_ref_for_log_path, worker_reactor_metrics,
        },
        worker_loop::{append_cancelled_task_state, append_paused_task_state},
    },
    common::{PlannedProvider, StreamPlan, spawn_test_worker, test_root_config},
};

struct ManualLoopWorker {
    command_tx: WorkerCommandSender,
    message_rx: mpsc::Receiver<WorkerMessage>,
    handle: Option<thread::JoinHandle<()>>,
}

#[test]
fn next_task_id_uses_session_local_counter() -> Result<()> {
    let mut session = Session::new("deepseek", "model");

    assert_eq!(
        next_task_id(&session).map_err(anyhow::Error::msg)?.as_str(),
        "task_1"
    );

    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: TaskId::new("task_1")?,
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "first".to_owned(),
        status: TaskRunStatus::Completed,
        reason: None,
    }))?;
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: TaskId::new("task_3")?,
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "third".to_owned(),
        status: TaskRunStatus::Completed,
        reason: None,
    }))?;

    assert_eq!(
        next_task_id(&session).map_err(anyhow::Error::msg)?.as_str(),
        "task_2"
    );
    Ok(())
}

#[test]
fn task_from_plan_reconciles_decision_only_crash_prefix_with_the_same_task_id() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp.path().join(".sigil/sessions/plan-prefix.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let base_snapshot = plan_handoff_workspace_snapshot_id(&root_config, &workspace_root)
        .map_err(anyhow::Error::msg)?;
    let store = JsonlSessionStore::new(&session_log_path)?;
    let mut session = Session::load_from_store("planned", "planned-model", store)?;
    let draft = plan_draft_created_entry(
        r#"```sigil-plan-v2
{"summary":"Inspect","steps":[{"step_id":"inspect","title":"Inspect","role":"executor","depends_on":[],"mode":"read","isolation":"shared_read_only"}]}
```"#,
        PlanSourceRef::default(),
        1,
        base_snapshot,
    )?
    .expect("structured plan draft");
    session.append_control(ControlEntry::PlanDraftCreated(draft.clone()))?;
    let stable_task_id = task_id_from_plan_draft(&draft)?;
    session.append_control(ControlEntry::PlanDecisionRecorded(
        PlanDecisionRecordedEntry {
            plan_id: draft.plan_id.clone(),
            plan_hash: draft.plan_hash.clone(),
            decision: PlanDecision::Accepted,
            decided_by: PlanDecisionActor::User,
            decided_at_ms: 2,
            reason: Some("created task from plan".to_owned()),
        },
    ))?;
    assert_eq!(
        session
            .plan_artifact_projection()
            .latest_pending_plan()
            .map(|pending| &pending.plan_id),
        Some(&draft.plan_id)
    );
    let mut current_session = Some(session);

    let created = create_task_from_plan(
        &root_config,
        &workspace_root,
        &session_log_path,
        &mut current_session,
        CreateTaskFromPlanRequest {
            plan_id: draft.plan_id.as_str().to_owned(),
            expected_plan_hash: draft.plan_hash,
            start_mode: PlanTaskStartMode::CreateAndRun,
            permission_grant: None,
        },
    )
    .map_err(anyhow::Error::msg)?;

    assert_eq!(created.task_id, stable_task_id);
    assert_eq!(
        created
            .entries
            .iter()
            .filter(|entry| matches!(entry, SessionLogEntry::Control(ControlEntry::TaskRun(_))))
            .count(),
        1
    );
    assert_eq!(
        created
            .entries
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::PlanDecisionRecorded(_))
            ))
            .count(),
        1
    );
    assert_eq!(created.entry.task_plan_version, 1);
    assert_eq!(created.entry.task_id, stable_task_id);
    Ok(())
}

#[test]
fn task_from_plan_acceptance_atomically_admits_and_binds_model_proposed_intents() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp.path().join(".sigil/sessions/plan-intents.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let base_snapshot = plan_handoff_workspace_snapshot_id(&root_config, &workspace_root)
        .map_err(anyhow::Error::msg)?;
    let store = JsonlSessionStore::new(&session_log_path)?;
    let mut session = Session::load_from_store("planned", "planned-model", store)?;
    let draft = plan_draft_created_entry(
        r#"```sigil-plan-v2
{
  "summary": "Implement retry and telemetry",
  "intents": [
    {
      "intent_alias": "retry",
      "title": "Retry behavior",
      "statement": "Retry failed operations safely.",
      "acceptance_criteria": [{
        "criterion_alias": "retry-test",
        "statement": "Retry behavior is covered by a passing regression test.",
        "required": true
      }],
      "depends_on_aliases": []
    },
    {
      "intent_alias": "telemetry",
      "title": "Retry telemetry",
      "statement": "Expose retry outcomes to operators.",
      "acceptance_criteria": [{
        "criterion_alias": "telemetry-test",
        "statement": "Telemetry output is covered by a passing regression test.",
        "required": true
      }],
      "depends_on_aliases": ["retry"]
    }
  ],
  "steps": [
    {
      "step_id": "implement-retry",
      "title": "Implement retry behavior",
      "role": "executor",
      "depends_on": [],
      "intent_aliases": ["retry"],
      "mode": "write",
      "isolation": "sequential_workspace_write",
      "target_paths": ["src/retry.rs"]
    },
    {
      "step_id": "add-telemetry",
      "title": "Add retry telemetry",
      "role": "executor",
      "depends_on": ["implement-retry"],
      "intent_aliases": ["telemetry"],
      "mode": "write",
      "isolation": "sequential_workspace_write",
      "target_paths": ["src/telemetry.rs"]
    }
  ],
  "target_paths": ["src/retry.rs", "src/telemetry.rs"]
}
```"#,
        PlanSourceRef::default(),
        1,
        base_snapshot,
    )?
    .expect("structured intent plan draft");
    session.append_control(ControlEntry::PlanDraftCreated(draft.clone()))?;
    let mut current_session = Some(session);

    let created = create_task_from_plan(
        &root_config,
        &workspace_root,
        &session_log_path,
        &mut current_session,
        CreateTaskFromPlanRequest {
            plan_id: draft.plan_id.as_str().to_owned(),
            expected_plan_hash: draft.plan_hash,
            start_mode: PlanTaskStartMode::CreatePaused,
            permission_grant: None,
        },
    )
    .map_err(anyhow::Error::msg)?;

    let session = current_session.expect("task creation should retain the durable session");
    let task = session
        .task_state_projection()
        .tasks
        .get(&created.task_id)
        .cloned()
        .expect("accepted task should exist");
    let accepted_plan = task
        .plans
        .get(&1)
        .expect("directly promoted task plan should exist");
    assert_eq!(accepted_plan.steps[0].intent_refs.len(), 1);
    assert_eq!(accepted_plan.steps[1].intent_refs.len(), 1);
    assert_ne!(
        accepted_plan.steps[0].intent_refs,
        accepted_plan.steps[1].intent_refs
    );
    let PublicIntentStackStateV1::Available { stack, .. } =
        session.public_intent_stack_state_for_workspace(&workspace_root)?
    else {
        panic!("accepted model proposal should create an available Intent Stack");
    };
    assert_eq!(stack.intents.len(), 2);
    assert!(
        stack
            .intents
            .iter()
            .any(|intent| intent.title == "Retry behavior")
    );
    assert!(
        stack
            .intents
            .iter()
            .any(|intent| intent.title == "Retry telemetry")
    );
    Ok(())
}

#[test]
fn task_from_plan_reconciles_created_anchor_before_acceptance_without_duplicates() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp.path().join(".sigil/sessions/plan-anchor-prefix.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let base_snapshot = plan_handoff_workspace_snapshot_id(&root_config, &workspace_root)
        .map_err(anyhow::Error::msg)?;
    let store = JsonlSessionStore::new(&session_log_path)?;
    let mut session = Session::load_from_store("planned", "planned-model", store)?;
    let draft = plan_draft_created_entry(
        r#"```sigil-plan-v2
{"summary":"Inspect","steps":[{"step_id":"inspect","title":"Inspect","role":"executor","depends_on":[],"mode":"read","isolation":"shared_read_only"}]}
```"#,
        PlanSourceRef::default(),
        1,
        base_snapshot,
    )?
    .expect("structured plan draft");
    session.append_control(ControlEntry::PlanDraftCreated(draft.clone()))?;
    let stable_task_id = task_id_from_plan_draft(&draft)?;
    let objective = plan_task_input_from_draft(&draft);
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: stable_task_id.clone(),
        parent_session_ref: session_ref_for_log_path(&session_log_path)
            .map_err(anyhow::Error::msg)?,
        objective,
        status: TaskRunStatus::Started,
        reason: Some(format!("created from plan {}", draft.plan_id.as_str())),
    }))?;
    let promotion = task_plan_from_plan_draft(&draft, stable_task_id.clone(), 1)?
        .expect("v2 plan should promote");
    let promoted = promotion.task_plan;
    let step_mapping = promotion.step_mapping;
    session.append_control(ControlEntry::TaskPlan(promoted))?;
    session.append_control(ControlEntry::TaskCreatedFromPlan(
        TaskCreatedFromPlanEntry {
            plan_id: draft.plan_id.clone(),
            plan_hash: draft.plan_hash.clone(),
            task_id: stable_task_id.clone(),
            task_plan_version: 1,
            step_mapping,
            stale_reason: None,
            created_at_ms: 2,
        },
    ))?;
    assert!(
        session
            .plan_artifact_projection()
            .latest_pending_plan()
            .is_some()
    );
    let mut current_session = Some(session);

    let created = create_task_from_plan(
        &root_config,
        &workspace_root,
        &session_log_path,
        &mut current_session,
        CreateTaskFromPlanRequest {
            plan_id: draft.plan_id.as_str().to_owned(),
            expected_plan_hash: draft.plan_hash,
            start_mode: PlanTaskStartMode::CreateAndRun,
            permission_grant: None,
        },
    )
    .map_err(anyhow::Error::msg)?;

    assert_eq!(created.task_id, stable_task_id);
    assert_eq!(
        created
            .entries
            .iter()
            .filter(|entry| matches!(entry, SessionLogEntry::Control(ControlEntry::TaskRun(_))))
            .count(),
        1
    );
    assert_eq!(
        created
            .entries
            .iter()
            .filter(|entry| matches!(entry, SessionLogEntry::Control(ControlEntry::TaskPlan(_))))
            .count(),
        1
    );
    assert_eq!(
        created
            .entries
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::TaskCreatedFromPlan(_))
            ))
            .count(),
        1
    );
    assert_eq!(
        created
            .entries
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::PlanDecisionRecorded(decision))
                    if decision.decision == PlanDecision::Accepted
            ))
            .count(),
        1
    );
    assert!(
        current_session
            .as_ref()
            .expect("session remains available")
            .plan_artifact_projection()
            .latest_pending_plan()
            .is_none()
    );
    Ok(())
}

#[test]
fn task_from_plan_without_base_snapshot_uses_compatibility_planner() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/plan-no-base-snapshot.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let store = JsonlSessionStore::new(&session_log_path)?;
    let mut session = Session::load_from_store("planned", "planned-model", store)?;
    let draft = plan_draft_created_entry(
        r#"```sigil-plan-v2
{"summary":"Inspect","steps":[{"step_id":"inspect","title":"Inspect","role":"executor","depends_on":[],"mode":"read","isolation":"shared_read_only"}]}
```"#,
        PlanSourceRef::default(),
        1,
        None,
    )?
    .expect("structured plan draft");
    session.append_control(ControlEntry::PlanDraftCreated(draft.clone()))?;
    let mut current_session = Some(session);

    let created = create_task_from_plan(
        &root_config,
        &workspace_root,
        &session_log_path,
        &mut current_session,
        CreateTaskFromPlanRequest {
            plan_id: draft.plan_id.as_str().to_owned(),
            expected_plan_hash: draft.plan_hash,
            start_mode: PlanTaskStartMode::CreateAndRun,
            permission_grant: None,
        },
    )
    .map_err(anyhow::Error::msg)?;

    assert_eq!(created.entry.task_plan_version, 0);
    assert!(created.entry.step_mapping.is_empty());
    assert!(
        created
            .entry
            .stale_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("base workspace snapshot is unavailable"))
    );
    let task = current_session
        .as_ref()
        .expect("session remains available")
        .task_state_projection()
        .tasks
        .get(&created.task_id)
        .cloned()
        .expect("task remains projected");
    assert!(task.plans.is_empty());
    assert_eq!(task.status, TaskRunStatus::Started);
    Ok(())
}

#[test]
fn task_from_plan_refuses_stale_retry_after_promoted_plan_crash_prefix() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().join("workspace");
    fs::create_dir_all(&workspace_root)?;
    fs::write(workspace_root.join("README.md"), "snapshot a\n")?;
    let session_log_path = temp.path().join(".sigil/sessions/plan-drift-prefix.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let base_snapshot = plan_handoff_workspace_snapshot_id(&root_config, &workspace_root)
        .map_err(anyhow::Error::msg)?;
    let store = JsonlSessionStore::new(&session_log_path)?;
    let mut session = Session::load_from_store("planned", "planned-model", store)?;
    let draft = plan_draft_created_entry(
        r#"```sigil-plan-v2
{"summary":"Inspect","steps":[{"step_id":"inspect","title":"Inspect","role":"executor","depends_on":[],"mode":"read","isolation":"shared_read_only"}]}
```"#,
        PlanSourceRef::default(),
        1,
        base_snapshot,
    )?
    .expect("structured plan draft");
    session.append_control(ControlEntry::PlanDraftCreated(draft.clone()))?;
    let stable_task_id = task_id_from_plan_draft(&draft)?;
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: stable_task_id.clone(),
        parent_session_ref: session_ref_for_log_path(&session_log_path)
            .map_err(anyhow::Error::msg)?,
        objective: plan_task_input_from_draft(&draft),
        status: TaskRunStatus::Started,
        reason: Some(format!("created from plan {}", draft.plan_id.as_str())),
    }))?;
    let promoted = task_plan_from_plan_draft(&draft, stable_task_id.clone(), 1)?
        .expect("v2 plan should promote")
        .task_plan;
    session.append_control(ControlEntry::TaskPlan(promoted))?;
    fs::write(workspace_root.join("README.md"), "snapshot b\n")?;
    let mut current_session = Some(session);

    let error = match create_task_from_plan(
        &root_config,
        &workspace_root,
        &session_log_path,
        &mut current_session,
        CreateTaskFromPlanRequest {
            plan_id: draft.plan_id.as_str().to_owned(),
            expected_plan_hash: draft.plan_hash,
            start_mode: PlanTaskStartMode::CreateAndRun,
            permission_grant: None,
        },
    ) {
        Ok(_) => panic!("workspace drift must not reuse an earlier promoted plan"),
        Err(error) => error,
    };

    assert!(error.contains("workspace drift"));
    let projection = current_session
        .as_ref()
        .expect("session remains available")
        .plan_artifact_projection();
    assert!(!projection.task_created_for_plan(&draft.plan_id));
    assert!(projection.latest_pending_plan().is_some());
    let task_projection = current_session
        .as_ref()
        .expect("session remains available")
        .task_state_projection();
    let task = task_projection
        .tasks
        .get(&stable_task_id)
        .expect("stale prefix task remains auditable");
    assert_eq!(task.status, TaskRunStatus::Cancelled);
    assert_eq!(
        task.plans
            .get(&1)
            .expect("promoted plan remains audited")
            .status,
        TaskPlanStatus::Superseded
    );
    let continue_error = resolve_continue_task(
        current_session.as_ref().expect("session remains available"),
        Some(stable_task_id.as_str().to_owned()),
    )
    .expect_err("stale cancelled prefix must not be continuable");
    assert!(continue_error.contains("cancelled"));
    Ok(())
}

#[test]
fn agent_result_continuation_partition_keeps_background_non_blocking() -> Result<()> {
    let temp = tempdir()?;
    let mut session = Session::new("planned", "planned-model");
    let join_thread = AgentThreadId::new("agent_join")?;
    let background_thread = AgentThreadId::new("agent_background")?;
    session.append_control(ControlEntry::AgentThreadStarted(
        test_agent_thread_started_entry(
            temp.path(),
            join_thread.clone(),
            AgentInvocationMode::JoinBeforeFinal,
        )?,
    ))?;
    session.append_control(ControlEntry::AgentThreadStarted(
        test_agent_thread_started_entry(
            temp.path(),
            background_thread.clone(),
            AgentInvocationMode::Background,
        )?,
    ))?;

    let (blocking, non_blocking) = partition_agent_result_continuations(
        Some(&session),
        vec![join_thread.clone(), background_thread.clone()],
    );

    assert_eq!(blocking, vec![join_thread]);
    assert_eq!(non_blocking, vec![background_thread]);
    Ok(())
}

#[test]
fn pending_agent_result_continuations_restore_started_statuses() -> Result<()> {
    let mut session = Session::new("planned", "planned-model");
    let pending = AgentThreadId::new("agent_pending")?;
    let started = AgentThreadId::new("agent_started")?;
    let completed = AgentThreadId::new("agent_completed")?;
    for (thread_id, status) in [
        (pending.clone(), AgentResultContinuationStatus::Pending),
        (started.clone(), AgentResultContinuationStatus::Started),
        (completed, AgentResultContinuationStatus::Completed),
    ] {
        session.append_control(ControlEntry::AgentResultContinuation(
            AgentResultContinuationEntry {
                thread_id,
                status,
                reason: None,
                updated_at_ms: Some(1),
            },
        ))?;
    }

    let restored = pending_agent_result_continuations_from_session(Some(&session));

    assert_eq!(restored, vec![pending, started]);
    Ok(())
}

#[test]
fn detached_durable_continuation_is_visible_through_active_projection() -> Result<()> {
    let temp = tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("continuation-projection.jsonl"))?;
    let session = Session::load_from_store("planned", "planned-model", store.clone())?;
    let thread_id = AgentThreadId::new("detached_pending")?;
    store.append(&SessionLogEntry::Control(
        ControlEntry::AgentResultContinuation(AgentResultContinuationEntry {
            thread_id: thread_id.clone(),
            status: AgentResultContinuationStatus::Pending,
            reason: None,
            updated_at_ms: Some(1),
        }),
    ))?;

    assert_eq!(
        pending_agent_continuations_from_active_projection(&session).map_err(anyhow::Error::msg)?,
        vec![thread_id]
    );
    Ok(())
}

#[test]
#[ignore = "release-profile long-session evidence"]
fn worker_reactor_idle_long_session_evidence() -> Result<()> {
    const TARGET_DURABLE_BYTES: u64 = 10 * 1024 * 1024;
    const PROMPT_TOKENS: u64 = 216_803;
    const CONTEXT_WINDOW_TOKENS: u64 = 985_468;
    let idle_seconds = std::env::var("SIGIL_IDLE_EVIDENCE_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| *seconds >= 30)
        .unwrap_or(30);

    let temp = tempdir()?;
    let session_path = temp.path().join("idle-session.jsonl");
    let store = JsonlSessionStore::new(&session_path)?;
    let note_payload = "long-session-evidence ".repeat(1_100);
    let mut ordinal = 0_u64;
    while std::fs::metadata(&session_path)
        .map(|metadata| metadata.len())
        .unwrap_or_default()
        < TARGET_DURABLE_BYTES
    {
        store.append(&SessionLogEntry::Control(ControlEntry::Note {
            kind: format!("long_session_fixture_{ordinal}"),
            data: serde_json::Value::String(note_payload.clone()),
        }))?;
        ordinal = ordinal.saturating_add(1);
    }
    store.append(&SessionLogEntry::Control(ControlEntry::UsageSnapshot(
        UsageStats {
            prompt_tokens: PROMPT_TOKENS,
            ..UsageStats::default()
        },
    )))?;
    let durable_bytes = std::fs::metadata(&session_path)?.len();
    drop(store);

    let mut root_config = test_root_config(temp.path(), "planned", "planned-model");
    root_config.compaction.context_window_tokens = Some(u32::try_from(CONTEXT_WINDOW_TOKENS)?);
    let worker = spawn_test_worker(
        root_config,
        session_path,
        Agent::new(PlannedProvider::new(Vec::new()), ToolRegistry::new()),
        temp.path().to_path_buf(),
    )?;
    assert!(matches!(
        worker.recv_with_timeout(Duration::from_secs(60))?,
        WorkerMessage::WorkerReady
    ));
    assert!(
        worker
            .recv_with_timeout(Duration::from_millis(250))
            .is_err(),
        "idle worker emitted output while settling onto its blocking receive"
    );
    let reactor_before = worker_reactor_metrics();
    let artifact_gc_before = artifact_gc_task_metrics();
    let locks_before = session_io_lock_metrics();
    let started = Instant::now();
    assert!(
        worker
            .recv_with_timeout(Duration::from_secs(idle_seconds))
            .is_err(),
        "idle worker emitted output without an external event or armed deadline"
    );
    let elapsed_ms = started.elapsed().as_millis() as u64;
    let reactor_idle = worker_reactor_metrics().saturating_delta(reactor_before);
    let artifact_gc_idle = artifact_gc_task_metrics().saturating_delta(artifact_gc_before);
    let locks_idle = session_io_lock_metrics().saturating_delta(locks_before);
    assert_eq!(reactor_idle.event_wake_total, 0);
    assert_eq!(reactor_idle.deadline_total, 0);
    assert_eq!(reactor_idle.advancement_total, 0);
    assert_eq!(artifact_gc_idle.started_total, 0);
    assert_eq!(artifact_gc_idle.completed_total, 0);
    assert_eq!(locks_idle.shared_lock_attempt_total, 0);
    assert_eq!(locks_idle.exclusive_lock_attempt_total, 0);
    assert_eq!(locks_idle.contention_total, 0);
    assert_eq!(locks_idle.failure_total, 0);
    worker.shutdown()?;
    let reactor_with_teardown = worker_reactor_metrics().saturating_delta(reactor_before);
    println!(
        "SIGIL_LONG_SESSION_EVIDENCE {}",
        serde_json::json!({
            "schema_version": 1,
            "scenario": format!("worker_reactor_idle_10mib_{idle_seconds}s"),
            "scale": durable_bytes,
            "elapsed_ms": elapsed_ms,
            "facts": {
                "durable_bytes": durable_bytes,
                "durable_entry_count": ordinal.saturating_add(1),
                "prompt_tokens": PROMPT_TOKENS,
                "context_window_tokens": CONTEXT_WINDOW_TOKENS,
                "context_utilization_percent": PROMPT_TOKENS.saturating_mul(100) / CONTEXT_WINDOW_TOKENS,
                "idle_event_wake_count": reactor_idle.event_wake_total,
                "idle_deadline_count": reactor_idle.deadline_total,
                "idle_advancement_count": reactor_idle.advancement_total,
                "idle_shared_lock_attempt_count": locks_idle.shared_lock_attempt_total,
                "idle_exclusive_lock_attempt_count": locks_idle.exclusive_lock_attempt_total,
                "idle_lock_contention_count": locks_idle.contention_total,
                "idle_lock_failure_count": locks_idle.failure_total,
                "teardown_event_count": reactor_with_teardown.event_wake_total,
            }
        })
    );
    Ok(())
}

#[test]
fn agent_result_continuation_requires_final_answer_disposition() {
    let result = AgentRunResult {
        final_text: String::new(),
        tool_calls: 0,
        final_message_id: None,
    };
    let interrupted = AgentRunOutput {
        result: result.clone(),
        outcome: AgentRunOutcome::default(),
        disposition: AgentRunDisposition::Interrupted,
    };
    assert!(agent_result_continuation_run_result(interrupted).is_err());

    let final_answer = AgentRunOutput {
        result: AgentRunResult {
            final_text: "done".to_owned(),
            ..result
        },
        outcome: AgentRunOutcome::default(),
        disposition: AgentRunDisposition::FinalAnswer,
    };
    assert_eq!(
        agent_result_continuation_run_result(final_answer)
            .expect("final answer should complete the continuation")
            .final_text,
        "done"
    );
}

#[test]
fn queued_background_ready_notice_is_bounded_transient_context() -> Result<()> {
    let mut session = Session::new("planned", "planned-model");
    for index in 1..=6 {
        session.append_control(ControlEntry::AgentResultContinuation(
            AgentResultContinuationEntry {
                thread_id: AgentThreadId::new(format!("agent_ready_{index}"))?,
                status: AgentResultContinuationStatus::Pending,
                reason: None,
                updated_at_ms: Some(index),
            },
        ))?;
    }

    let context = queued_background_ready_transient_context(Some(&session));

    assert_eq!(context.len(), 1);
    let content = context[0]
        .content
        .as_deref()
        .expect("ready notice should have content");
    assert!(content.contains("Background agent result ready notice"));
    assert!(content.contains("agent_ready_1"));
    assert!(content.contains("agent_ready_5"));
    assert!(content.contains("and 1 more"));
    assert!(!content.contains("agent_ready_6"));
    Ok(())
}

fn test_agent_thread_started_entry(
    workspace_root: &std::path::Path,
    thread_id: AgentThreadId,
    invocation_mode: AgentInvocationMode,
) -> Result<AgentThreadStartedEntry> {
    let snapshot_id = AgentProfileSnapshotId::new(format!("snapshot_{}", thread_id.as_str()))?;
    Ok(AgentThreadStartedEntry {
        thread_id: thread_id.clone(),
        parent_thread_id: None,
        batch_id: None,
        batch_member_key: None,
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        thread_session_ref: SessionRef::new_relative(format!(
            "children/{}.jsonl",
            thread_id.as_str()
        ))?,
        profile_id: AgentProfileId::new("explore")?,
        profile_snapshot_id: snapshot_id.clone(),
        run_context: AgentRunContextSnapshot {
            profile_snapshot_id: snapshot_id,
            provider: "planned".to_owned(),
            model: "planned-model".to_owned(),
            model_ref: None,
            reasoning_effort: None,
            workspace_root: WorkspaceRootSnapshot::new(workspace_root.display().to_string())?,
            effective_tool_scope_hash: String::new(),
            effective_permission_policy_hash: String::new(),
            effective_mcp_scope_hash: String::new(),
            provider_capability_hash: String::new(),
            model_visible_agent_index_hash: None,
            budget_policy_hash: String::new(),
            provider_background_handle_ref: None,
        },
        objective: "inspect".to_owned(),
        prompt_hash: "prompt-hash".to_owned(),
        invocation_mode,
        invocation_source: AgentInvocationSource::Chat,
        display_name: None,
        created_at_ms: None,
    })
}

#[test]
fn resolve_continue_task_uses_latest_unfinished_task() -> Result<()> {
    let mut session = Session::new("deepseek", "model");
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: TaskId::new("task_1")?,
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "resume me".to_owned(),
        status: TaskRunStatus::Failed,
        reason: None,
    }))?;
    session.append_control(ControlEntry::TaskPlan(TaskPlanEntry {
        task_id: TaskId::new("task_1")?,
        plan_version: 1,
        status: TaskPlanStatus::Accepted,
        steps: vec![TaskStepSpec {
            step_id: TaskStepId::new("step_1")?,
            title: "retry".to_owned(),
            display_name: None,
            detail: None,
            role: AgentRole::Executor,
            depends_on: Vec::new(),
            intent_refs: Vec::new(),
            mode: None,
            isolation: None,
        }],
        reason: None,
    }))?;
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: TaskId::new("task_2")?,
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "already done".to_owned(),
        status: TaskRunStatus::Completed,
        reason: None,
    }))?;

    let (task_id, task_id_value, objective, needs_planning) =
        resolve_continue_task(&session, None).map_err(anyhow::Error::msg)?;

    assert_eq!(task_id.as_str(), "task_1");
    assert_eq!(task_id_value, "task_1");
    assert_eq!(objective, "resume me");
    assert!(!needs_planning);
    Ok(())
}

#[test]
fn resolve_continue_task_reports_latest_completed_task() -> Result<()> {
    let mut session = Session::new("deepseek", "model");
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: TaskId::new("task_1")?,
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "already done".to_owned(),
        status: TaskRunStatus::Completed,
        reason: None,
    }))?;
    session.append_control(ControlEntry::TaskPlan(TaskPlanEntry {
        task_id: TaskId::new("task_1")?,
        plan_version: 1,
        status: TaskPlanStatus::Accepted,
        steps: vec![TaskStepSpec {
            step_id: TaskStepId::new("step_1")?,
            title: "done".to_owned(),
            display_name: None,
            detail: None,
            role: AgentRole::Executor,
            depends_on: Vec::new(),
            intent_refs: Vec::new(),
            mode: None,
            isolation: None,
        }],
        reason: None,
    }))?;

    let error = match resolve_continue_task(&session, None) {
        Ok((task_id, _, _, _)) => {
            anyhow::bail!("completed task unexpectedly resumed: {task_id:?}")
        }
        Err(error) => error,
    };

    assert_eq!(error, "task task_1 is already completed");
    Ok(())
}

#[test]
fn resolve_continue_task_rejects_an_exact_cancelled_task() -> Result<()> {
    let mut session = Session::new("deepseek", "model");
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: TaskId::new("task_cancelled")?,
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "do not revive".to_owned(),
        status: TaskRunStatus::Cancelled,
        reason: None,
    }))?;

    let error = resolve_continue_task(&session, Some("task_cancelled".to_owned()))
        .expect_err("cancelled task must not resume");

    assert_eq!(error, "task task_cancelled is cancelled");
    Ok(())
}

#[test]
fn append_cancelled_task_state_marks_active_task_step_and_child() -> Result<()> {
    let mut session = Session::new("deepseek", "model");
    let task_id = TaskId::new("task_1")?;
    let step_ids = [TaskStepId::new("step_1")?, TaskStepId::new("step_2")?];
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "cancel task".to_owned(),
        status: TaskRunStatus::Running,
        reason: None,
    }))?;
    session.append_control(ControlEntry::TaskPlan(TaskPlanEntry {
        task_id: task_id.clone(),
        plan_version: 1,
        status: TaskPlanStatus::Accepted,
        steps: step_ids
            .iter()
            .map(|step_id| TaskStepSpec {
                step_id: step_id.clone(),
                title: format!("running {}", step_id.as_str()),
                display_name: None,
                detail: None,
                role: AgentRole::SubagentRead,
                depends_on: Vec::new(),
                intent_refs: Vec::new(),
                mode: Some(sigil_kernel::TaskStepMode::Read),
                isolation: Some(sigil_kernel::TaskIsolationMode::SharedReadOnly),
            })
            .collect(),
        reason: None,
    }))?;
    for (index, step_id) in step_ids.iter().enumerate() {
        session.append_control(ControlEntry::TaskStep(TaskStepEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            step_id: step_id.clone(),
            role: AgentRole::SubagentRead,
            status: TaskStepStatus::Running,
            title: Some(format!("running {}", step_id.as_str())),
            summary: None,
            reason: None,
        }))?;
        session.append_control(ControlEntry::TaskChildSession(TaskChildSessionEntry {
            task_id: task_id.clone(),
            plan_version: 1,
            step_id: step_id.clone(),
            child_task_id: TaskId::new(format!("child_{}", index + 1))?,
            child_session_ref: SessionRef::new_relative(format!(
                "children/task_1/{}-child_{}.jsonl",
                step_id.as_str(),
                index + 1
            ))?,
            role: AgentRole::SubagentRead,
            status: TaskChildSessionStatus::Started,
            summary_hash: None,
        }))?;
    }

    append_cancelled_task_state(&mut session).map_err(anyhow::Error::msg)?;

    assert_eq!(
        session
            .entries()
            .iter()
            .filter(|entry| {
                matches!(
                    entry,
                    SessionLogEntry::Control(ControlEntry::TaskStep(step))
                        if step.status == TaskStepStatus::Cancelled
                )
            })
            .count(),
        2
    );
    assert_eq!(
        session
            .entries()
            .iter()
            .filter(|entry| {
                matches!(
                    entry,
                    SessionLogEntry::Control(ControlEntry::TaskChildSession(child))
                        if child.status == TaskChildSessionStatus::Cancelled
                )
            })
            .count(),
        2
    );
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TaskRun(run))
                if run.status == TaskRunStatus::Cancelled
        )
    }));
    Ok(())
}

#[test]
fn append_paused_task_state_keeps_interrupted_step_resumable() -> Result<()> {
    let mut session = Session::new("deepseek", "model");
    let task_id = TaskId::new("task_1")?;
    let step_id = TaskStepId::new("step_1")?;
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "pause task".to_owned(),
        status: TaskRunStatus::Running,
        reason: None,
    }))?;
    session.append_control(ControlEntry::TaskPlan(TaskPlanEntry {
        task_id: task_id.clone(),
        plan_version: 1,
        status: TaskPlanStatus::Accepted,
        steps: vec![TaskStepSpec {
            step_id: step_id.clone(),
            title: "running step".to_owned(),
            display_name: None,
            detail: None,
            role: AgentRole::SubagentRead,
            depends_on: Vec::new(),
            intent_refs: Vec::new(),
            mode: Some(sigil_kernel::TaskStepMode::Read),
            isolation: Some(sigil_kernel::TaskIsolationMode::SharedReadOnly),
        }],
        reason: None,
    }))?;
    session.append_control(ControlEntry::TaskStep(TaskStepEntry {
        task_id: task_id.clone(),
        plan_version: 1,
        step_id: step_id.clone(),
        role: AgentRole::SubagentRead,
        status: TaskStepStatus::Running,
        title: Some("running step".to_owned()),
        summary: None,
        reason: None,
    }))?;
    session.append_control(ControlEntry::TaskChildSession(TaskChildSessionEntry {
        task_id: task_id.clone(),
        plan_version: 1,
        step_id,
        child_task_id: TaskId::new("child_1")?,
        child_session_ref: SessionRef::new_relative("children/task_1/step_1-child_1.jsonl")?,
        role: AgentRole::SubagentRead,
        status: TaskChildSessionStatus::Started,
        summary_hash: None,
    }))?;
    let unrelated_task_id = TaskId::new("task_2")?;
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: unrelated_task_id.clone(),
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "unrelated running task".to_owned(),
        status: TaskRunStatus::Running,
        reason: None,
    }))?;

    append_paused_task_state(&mut session, task_id.as_str()).map_err(anyhow::Error::msg)?;

    let projection = session.task_state_projection();
    let task = projection.tasks.get(&task_id).expect("paused task");
    assert_eq!(task.status, TaskRunStatus::Paused);
    assert!(task.steps.values().any(|step| {
        step.step_id == TaskStepId::new("step_1").expect("step id")
            && step.status == TaskStepStatus::Interrupted
    }));
    assert!(task.child_sessions.values().any(|child| {
        child.child_task_id == TaskId::new("child_1").expect("child id")
            && child.status == TaskChildSessionStatus::Interrupted
    }));
    let plan = task.plans.get(&1).expect("accepted plan");
    let ready = plan
        .graph
        .as_ref()
        .expect("valid task graph")
        .ready_steps(&task.steps);
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].step_id.as_str(), "step_1");
    assert_eq!(
        projection
            .tasks
            .get(&unrelated_task_id)
            .expect("unrelated task")
            .status,
        TaskRunStatus::Running,
        "pause must not use latest-task fallback after validating an exact target"
    );
    Ok(())
}

#[test]
fn close_agent_thread_appends_runtime_close_control() -> Result<()> {
    let temp = tempdir()?;
    let root_config = test_root_config(temp.path(), "planned", "planned-model");
    let session_log_path = temp.path().join(".sigil/sessions/session-agent.jsonl");
    let store = JsonlSessionStore::new(&session_log_path)?;
    let mut session = Session::load_from_store("planned", "planned-model", store)?;
    let thread_id = AgentThreadId::new("thread_1")?;
    let snapshot_id = AgentProfileSnapshotId::new("snapshot_1")?;

    session.append_control(ControlEntry::AgentThreadStarted(AgentThreadStartedEntry {
        thread_id: thread_id.clone(),
        parent_thread_id: None,
        batch_id: None,
        batch_member_key: None,
        parent_session_ref: SessionRef::new_relative("session-agent.jsonl")?,
        thread_session_ref: SessionRef::new_relative("children/thread_1.jsonl")?,
        profile_id: AgentProfileId::new("explore")?,
        profile_snapshot_id: snapshot_id.clone(),
        run_context: AgentRunContextSnapshot {
            profile_snapshot_id: snapshot_id,
            provider: "planned".to_owned(),
            model: "planned-model".to_owned(),
            model_ref: None,
            reasoning_effort: None,
            workspace_root: WorkspaceRootSnapshot::new(temp.path().display().to_string())?,
            effective_tool_scope_hash: String::new(),
            effective_permission_policy_hash: String::new(),
            effective_mcp_scope_hash: String::new(),
            provider_capability_hash: String::new(),
            model_visible_agent_index_hash: None,
            budget_policy_hash: String::new(),
            provider_background_handle_ref: None,
        },
        objective: "inspect kernel".to_owned(),
        prompt_hash: "prompt-hash".to_owned(),
        invocation_mode: AgentInvocationMode::Foreground,
        invocation_source: AgentInvocationSource::Chat,
        display_name: Some("kernel map".to_owned()),
        created_at_ms: None,
    }))?;
    session.append_control(ControlEntry::AgentThreadStatusChanged(
        AgentThreadStatusChangedEntry {
            thread_id: thread_id.clone(),
            status: AgentThreadStatus::Completed,
            reason: None,
            updated_at_ms: None,
        },
    ))?;
    let mut current_session = None;

    let (closed_thread_id, entries) = close_agent_thread(
        &root_config,
        &session_log_path,
        &mut current_session,
        thread_id.clone(),
        Some("closed from TUI /agent".to_owned()),
    )
    .map_err(anyhow::Error::msg)?;

    assert_eq!(closed_thread_id, thread_id);
    assert!(entries.iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::AgentThreadClosed(close))
                if close.thread_id == thread_id
                    && close.reason.as_deref() == Some("closed from TUI /agent")
        )
    }));
    let persisted = JsonlSessionStore::read_entries(&session_log_path)?;
    assert!(persisted.iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::AgentThreadClosed(close))
                if close.thread_id == thread_id
        )
    }));
    Ok(())
}

#[test]
fn cancel_terminal_task_audits_success_and_uses_final_terminal_output() -> Result<()> {
    let temp = tempdir()?;
    let root_config = test_root_config(temp.path(), "planned", "planned-model");
    let provider = PlannedProvider::new(Vec::new());
    let (message_tx, _message_rx) = mpsc::channel();
    let elicitation_handler = Arc::new(ChannelMcpElicitationHandler::new(message_tx));
    let (mcp_event_tx, _mcp_event_rx) = mpsc::channel();
    let mcp_event_handler = Arc::new(ChannelMcpRuntimeEventHandler::new_test(mcp_event_tx));
    let surface = sigil_runtime::build_tool_surface_without_eager_mcp_with_workspace_trust(
        &root_config,
        &provider.capabilities(),
        temp.path().to_path_buf(),
        elicitation_handler,
        mcp_event_handler,
        sigil_kernel::WorkspaceTrust::Unknown,
    )?;
    let registry = surface.registry;
    let terminal_control = surface.terminal_control;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let session_log_path = temp.path().join(".sigil/sessions/session-terminal.jsonl");
    let store = JsonlSessionStore::new(&session_log_path)?;
    let mut session = Session::load_from_store("planned", "planned-model", store.clone())?;
    let recorder = MutationEventRecorder::new(store);
    let start_profile = recorder.execution_mutation_profile(
        temp.path(),
        &VerificationScope::all_tracked(DEFAULT_TASK_VERIFICATION_SCOPE_HASH),
        "call-terminal-start",
        "terminal_start",
        ToolEffect::Unknown,
    )?;
    session.append_control(ControlEntry::ToolExecution(Box::new(ToolExecutionEntry {
        call_id: "call-terminal-start".to_owned(),
        tool_name: "terminal_start".to_owned(),
        status: ToolExecutionStatus::Started,
        duration_ms: None,
        subjects: Vec::new(),
        changed_files: Vec::new(),
        metadata: ToolResultMeta {
            details: serde_json::json!({
                "execution_mutation_profile": start_profile,
            }),
            ..ToolResultMeta::default()
        },
        error: None,
        model_content_hash: None,
    })))?;
    let tool_context = ToolContext::new(temp.path().to_path_buf(), 5);
    let task_id = "terminal-cancel-audit";
    let start = runtime.block_on(
        registry.execute(
            tool_context.clone(),
            ToolCall {
                id: "call-terminal-start".to_owned(),
                name: "terminal_start".to_owned(),
                args_json: serde_json::json!({
                    "task_id": task_id,
                    "command": "printf terminal-mutated > terminal-mutated.txt; printf cancel-tail; sleep 5",
                    "mode": "background"
                })
                .to_string(),
            },
        ),
    )?;
    let start_entry = TerminalTaskEntry::from_tool_result_details(&start.metadata.details)?
        .expect("terminal_start should return terminal metadata");
    runtime.block_on(wait_for_terminal_output(
        &registry,
        tool_context.clone(),
        task_id,
        "cancel-tail",
    ))?;

    session.append_control(ControlEntry::ToolExecution(Box::new(ToolExecutionEntry {
        call_id: "call-terminal-start".to_owned(),
        tool_name: "terminal_start".to_owned(),
        status: ToolExecutionStatus::Completed,
        duration_ms: Some(1),
        subjects: Vec::new(),
        changed_files: Vec::new(),
        metadata: durable_terminal_tool_result_metadata(&start.metadata),
        error: None,
        model_content_hash: Some("0".repeat(64)),
    })))?;
    let live_entry =
        runtime.block_on(terminal_control.status(temp.path(), &TerminalTaskId::new(task_id)?))?;
    assert!(live_entry.generation >= start_entry.generation);
    session.append_control(ControlEntry::TerminalTask(live_entry))?;
    let terminal_identity = TerminalTaskControlIdentity {
        session_scope_id: session.session_scope_id().to_owned(),
        run_id: "foreground-run-terminal-cancel".to_owned(),
        task_id: task_id.to_owned(),
        expected_generation: session
            .terminal_task_projection()
            .tasks
            .get(&TerminalTaskId::new(task_id)?)
            .expect("started terminal should be projected")
            .generation,
    };
    let options = sigil_runtime::build_run_options(
        &root_config,
        temp.path().to_path_buf(),
        sigil_kernel::InteractionMode::Interactive,
    );
    let mut current_session = None;

    let stale_identity = TerminalTaskControlIdentity {
        expected_generation: terminal_identity.expected_generation.saturating_sub(1),
        ..terminal_identity.clone()
    };
    let stale_error = cancel_terminal_task(
        &runtime,
        registry.clone(),
        &terminal_control,
        &root_config,
        &options,
        &session_log_path,
        &mut current_session,
        &stale_identity,
    )
    .expect_err("stale terminal generation must fail before cancellation");
    assert!(stale_error.contains("generation changed"));

    let (entry, entries) = cancel_terminal_task(
        &runtime,
        registry,
        &terminal_control,
        &root_config,
        &options,
        &session_log_path,
        &mut current_session,
        &terminal_identity,
    )
    .map_err(anyhow::Error::msg)?;

    assert!(matches!(entry.status, TerminalTaskStatus::Cancelled));
    assert!(matches!(
        entry.cleanup.as_ref().map(|cleanup| cleanup.status),
        Some(ExecutionCleanupStatus::Completed)
    ));
    assert!(entry.output_preview.is_none());
    let output_hash = entry
        .output_hash
        .as_deref()
        .expect("cancelled terminal should retain a final output digest");
    let output_digest = output_hash.strip_prefix("sha256:").unwrap_or(output_hash);
    assert_eq!(output_digest.len(), 64);
    assert!(output_digest.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert!(entry.output_total_bytes > 0);
    let planned_hash = entries.iter().find_map(|entry| match entry {
        SessionLogEntry::Control(ControlEntry::ToolPermissionPlannedV2(planned))
            if planned.tool_name == "terminal_cancel" =>
        {
            Some(planned.plan_hash.clone())
        }
        _ => None,
    });
    assert!(
        planned_hash.is_some(),
        "terminal cancel should persist its V2 plan"
    );
    assert!(entries.iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
                if execution.tool_name == "terminal_cancel"
                    && execution.status == ToolExecutionStatus::Started
                    && execution.model_content_hash.is_none()
                    && execution.metadata.details.get("permission_plan_hash")
                        .and_then(serde_json::Value::as_str)
                        == planned_hash.as_deref()
        )
    }));
    assert!(entries.iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
                if execution.tool_name == "terminal_cancel"
                    && execution.status == ToolExecutionStatus::Completed
                    && execution.model_content_hash.is_some()
                    && execution.error.is_none()
        )
    }));
    assert!(entries.iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TerminalTask(task))
                if task.handle.task_id.as_str() == task_id
                    && matches!(task.status, TerminalTaskStatus::Cancelled)
                    && task.output_hash.is_some()
        )
    }));
    let detected = JsonlSessionStore::read_event_records(&session_log_path)?
        .into_iter()
        .filter_map(|record| match record {
            SessionStreamRecord::Stored(event)
                if event.event_type == DurableEventType::WorkspaceMutationDetected.as_str() =>
            {
                Some(event)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(detected.len(), 1);
    let payload: WorkspaceMutationDetected = serde_json::from_value(detected[0].payload.clone())?;
    assert_eq!(payload.tool_call_id.as_deref(), Some("call-terminal-start"));
    assert_eq!(payload.tool_name, "terminal_start");
    assert!(!payload.unknown_dirty);
    assert!(payload.from_workspace_snapshot_id.is_some());
    assert!(payload.to_workspace_snapshot_id.is_some());
    Ok(())
}

#[test]
fn cancel_terminal_task_audits_tool_failure() -> Result<()> {
    let temp = tempdir()?;
    let root_config = test_root_config(temp.path(), "planned", "planned-model");
    let provider = PlannedProvider::new(Vec::new());
    let (message_tx, _message_rx) = mpsc::channel();
    let elicitation_handler = Arc::new(ChannelMcpElicitationHandler::new(message_tx));
    let (mcp_event_tx, _mcp_event_rx) = mpsc::channel();
    let mcp_event_handler = Arc::new(ChannelMcpRuntimeEventHandler::new_test(mcp_event_tx));
    let surface = sigil_runtime::build_tool_surface_without_eager_mcp_with_workspace_trust(
        &root_config,
        &provider.capabilities(),
        temp.path().to_path_buf(),
        elicitation_handler,
        mcp_event_handler,
        sigil_kernel::WorkspaceTrust::Unknown,
    )?;
    let registry = surface.registry;
    let terminal_control = surface.terminal_control;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-terminal-failed.jsonl");
    let store = JsonlSessionStore::new(&session_log_path)?;
    let mut session = Session::load_from_store("planned", "planned-model", store)?;
    session.append_control(ControlEntry::TerminalTask(edge_terminal_entry(
        "terminal-missing-manager",
        TerminalTaskStatus::Running,
    )?))?;
    let terminal_identity = TerminalTaskControlIdentity {
        session_scope_id: session.session_scope_id().to_owned(),
        run_id: "foreground-run-terminal-missing".to_owned(),
        task_id: "terminal-missing-manager".to_owned(),
        expected_generation: session
            .terminal_task_projection()
            .tasks
            .get(&TerminalTaskId::new("terminal-missing-manager")?)
            .expect("terminal fixture should be projected")
            .generation,
    };
    let options = sigil_runtime::build_run_options(
        &root_config,
        temp.path().to_path_buf(),
        sigil_kernel::InteractionMode::Interactive,
    );
    let mut current_session = None;

    let error = cancel_terminal_task(
        &runtime,
        registry,
        &terminal_control,
        &root_config,
        &options,
        &session_log_path,
        &mut current_session,
        &terminal_identity,
    )
    .expect_err("unknown manager task should fail");
    let entries = current_session
        .expect("failed cancel should still keep audited session")
        .entries()
        .to_vec();

    assert!(error.contains("terminal cancel failed"));
    assert!(entries.iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
                if execution.tool_name == "terminal_cancel"
                    && execution.status == ToolExecutionStatus::Started
        )
    }));
    assert!(entries.iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
                if execution.tool_name == "terminal_cancel"
                    && execution.status == ToolExecutionStatus::Failed
                    && execution.error.is_some()
                    && execution.model_content_hash.is_some()
        )
    }));
    Ok(())
}

#[test]
fn append_mcp_elicitation_audits_adds_subagent_route_summary() -> Result<()> {
    let mut session = Session::new("deepseek", "model");
    let task_id = TaskId::new("task_1")?;
    let step_id = TaskStepId::new("step_1")?;
    seed_running_subagent_task(&mut session, &task_id, &step_id)?;
    let audit_buffer = Arc::new(std::sync::Mutex::new(vec![ControlEntry::McpElicitation(
        Box::new(McpElicitationEntry::new(
            "server-a",
            "Need a value",
            &serde_json::json!({
                "type": "object",
                "properties": {
                    "answer": { "type": "string" }
                }
            }),
            McpElicitationDecision::Accepted,
            Some(&serde_json::json!({ "answer": "redacted" })),
        )),
    )]));

    append_mcp_elicitation_audits(&mut session, &audit_buffer).map_err(anyhow::Error::msg)?;

    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TaskSubagentElicitationRoute(route))
                if route.server_name == "server-a"
                    && route.status == TaskRouteStatus::Resolved
                    && route.step_id == step_id
        )
    }));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::McpElicitation(elicitation))
                if elicitation.server_name == "server-a"
        )
    }));
    Ok(())
}

#[test]
fn append_mcp_elicitation_audits_does_not_guess_between_active_children() -> Result<()> {
    let mut session = Session::new("deepseek", "model");
    let task_id = TaskId::new("task_1")?;
    let first_step_id = TaskStepId::new("step_1")?;
    let second_step_id = TaskStepId::new("step_2")?;
    seed_running_subagent_task(&mut session, &task_id, &first_step_id)?;
    session.append_control(ControlEntry::TaskStep(TaskStepEntry {
        task_id: task_id.clone(),
        plan_version: 1,
        step_id: second_step_id.clone(),
        role: AgentRole::SubagentRead,
        status: TaskStepStatus::Running,
        title: Some("second child".to_owned()),
        summary: None,
        reason: None,
    }))?;
    session.append_control(ControlEntry::TaskChildSession(TaskChildSessionEntry {
        task_id,
        plan_version: 1,
        step_id: second_step_id,
        child_task_id: TaskId::new("child_2")?,
        child_session_ref: SessionRef::new_relative("children/task_1/step_2-child_2.jsonl")?,
        role: AgentRole::SubagentRead,
        status: TaskChildSessionStatus::Started,
        summary_hash: None,
    }))?;
    let audit_buffer = Arc::new(std::sync::Mutex::new(vec![ControlEntry::McpElicitation(
        Box::new(McpElicitationEntry::new(
            "server-a",
            "Need a value",
            &serde_json::json!({"type": "object"}),
            McpElicitationDecision::Accepted,
            Some(&serde_json::json!({})),
        )),
    )]));

    append_mcp_elicitation_audits(&mut session, &audit_buffer).map_err(anyhow::Error::msg)?;

    assert!(!session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TaskSubagentElicitationRoute(_))
        )
    }));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::McpElicitation(elicitation))
                if elicitation.server_name == "server-a"
        )
    }));
    Ok(())
}

#[test]
fn append_mcp_elicitation_audits_routes_after_task_completion() -> Result<()> {
    let mut session = Session::new("deepseek", "model");
    let task_id = TaskId::new("task_1")?;
    let step_id = TaskStepId::new("step_1")?;
    seed_running_subagent_task(&mut session, &task_id, &step_id)?;
    session.append_control(ControlEntry::TaskChildSession(TaskChildSessionEntry {
        task_id: task_id.clone(),
        plan_version: 1,
        step_id: step_id.clone(),
        child_task_id: TaskId::new("child_1")?,
        child_session_ref: SessionRef::new_relative("children/task_1/step_1-child_1.jsonl")?,
        role: AgentRole::SubagentWrite,
        status: TaskChildSessionStatus::Completed,
        summary_hash: Some("hash".to_owned()),
    }))?;
    session.append_control(ControlEntry::TaskStep(TaskStepEntry {
        task_id: task_id.clone(),
        plan_version: 1,
        step_id: step_id.clone(),
        role: AgentRole::SubagentWrite,
        status: TaskStepStatus::Completed,
        title: Some("child".to_owned()),
        summary: Some("done".to_owned()),
        reason: None,
    }))?;
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id,
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "subagent task".to_owned(),
        status: TaskRunStatus::Completed,
        reason: None,
    }))?;
    let audit_buffer = Arc::new(std::sync::Mutex::new(vec![ControlEntry::McpElicitation(
        Box::new(McpElicitationEntry::new(
            "server-a",
            "Need a value",
            &serde_json::json!({
                "type": "object",
                "properties": {
                    "answer": { "type": "string" }
                }
            }),
            McpElicitationDecision::Accepted,
            Some(&serde_json::json!({ "answer": "redacted" })),
        )),
    )]));

    append_mcp_elicitation_audits(&mut session, &audit_buffer).map_err(anyhow::Error::msg)?;

    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TaskSubagentElicitationRoute(route))
                if route.server_name == "server-a"
                    && route.status == TaskRouteStatus::Resolved
                    && route.step_id == step_id
        )
    }));
    Ok(())
}

fn seed_running_subagent_task(
    session: &mut Session,
    task_id: &TaskId,
    step_id: &TaskStepId,
) -> Result<()> {
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "subagent task".to_owned(),
        status: TaskRunStatus::Running,
        reason: None,
    }))?;
    session.append_control(ControlEntry::TaskPlan(TaskPlanEntry {
        task_id: task_id.clone(),
        plan_version: 1,
        status: TaskPlanStatus::Accepted,
        steps: vec![TaskStepSpec {
            step_id: step_id.clone(),
            title: "child".to_owned(),
            display_name: None,
            detail: None,
            role: AgentRole::SubagentWrite,
            depends_on: Vec::new(),
            intent_refs: Vec::new(),
            mode: None,
            isolation: None,
        }],
        reason: None,
    }))?;
    session.append_control(ControlEntry::TaskStep(TaskStepEntry {
        task_id: task_id.clone(),
        plan_version: 1,
        step_id: step_id.clone(),
        role: AgentRole::SubagentWrite,
        status: TaskStepStatus::Running,
        title: Some("child".to_owned()),
        summary: None,
        reason: None,
    }))?;
    session.append_control(ControlEntry::TaskChildSession(TaskChildSessionEntry {
        task_id: task_id.clone(),
        plan_version: 1,
        step_id: step_id.clone(),
        child_task_id: TaskId::new("child_1")?,
        child_session_ref: SessionRef::new_relative("children/task_1/step_1-child_1.jsonl")?,
        role: AgentRole::SubagentWrite,
        status: TaskChildSessionStatus::Started,
        summary_hash: None,
    }))?;
    Ok(())
}

impl ManualLoopWorker {
    fn send(&self, command: WorkerCommand) -> Result<()> {
        self.command_tx
            .send(command)
            .map_err(|error| anyhow::anyhow!("failed to send worker command: {error}"))
    }

    fn send_shutdown(&self) -> Result<()> {
        self.send(WorkerCommand::Shutdown)
    }

    fn recv(&self, timeout: Duration) -> Result<WorkerMessage> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(anyhow::anyhow!("timed out waiting for worker message"));
            }
            let message = self.message_rx.recv_timeout(remaining).map_err(|error| {
                anyhow::anyhow!("timed out waiting for worker message: {error}")
            })?;
            if !matches!(message, WorkerMessage::WorkerReady) {
                return Ok(message);
            }
        }
    }

    fn recv_until_with_timeout<F>(&self, timeout: Duration, predicate: F) -> Result<WorkerMessage>
    where
        F: Fn(&WorkerMessage) -> bool,
    {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(anyhow::anyhow!("timed out waiting for worker message"));
            }
            let message = self.recv(remaining)?;
            if predicate(&message) {
                return Ok(message);
            }
        }
    }

    fn recv_optional(&self, timeout: Duration) -> Result<Option<WorkerMessage>> {
        match self.message_rx.recv_timeout(timeout) {
            Ok(message) => Ok(Some(message)),
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
            Err(mpsc::RecvTimeoutError::Disconnected) => Ok(None),
        }
    }

    fn join(mut self) -> Result<()> {
        if let Some(handle) = self.handle.take() {
            handle
                .join()
                .map_err(|_| anyhow::anyhow!("worker thread panicked during shutdown"))?;
        }
        Ok(())
    }
}

fn spawn_loop_with_shared_agent(
    root_config: RootConfig,
    session_log_path: PathBuf,
    workspace_root: PathBuf,
    agent: Arc<Agent<PlannedProvider>>,
) -> Result<ManualLoopWorker> {
    let (event_tx, event_rx) = mpsc::channel();
    let (urgent_tx, urgent_rx) = mpsc::channel();
    let command_tx = WorkerCommandSender::new(event_tx.clone(), urgent_tx);
    let (message_tx, message_rx) = mpsc::channel();
    let options = sigil_runtime::build_run_options(
        &root_config,
        workspace_root.clone(),
        sigil_kernel::InteractionMode::Interactive,
    );
    let agent_for_loop = Arc::clone(&agent);
    let elicitation_handler = Arc::new(ChannelMcpElicitationHandler::new(message_tx.clone()));
    let mcp_event_handler = Arc::new(ChannelMcpRuntimeEventHandler::new(
        WorkerMcpRuntimeEventSender::new(event_tx.clone()),
    ));
    let terminal_lifecycle_router = ChannelTerminalLifecycleRouter::new(event_tx.clone());

    let handle = thread::Builder::new()
        .name("sigil-edge-worker-loop-test".to_owned())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("edge worker runtime should build");
            let context_resolver =
                sigil_runtime::RequestContextResolver::request_local(workspace_root.clone());
            run_worker_loop(
                runtime,
                agent_for_loop,
                root_config,
                workspace_root,
                session_log_path,
                options,
                (event_tx, event_rx, urgent_rx),
                message_tx,
                WorkerLoopMcpHandlers {
                    elicitation_handler,
                    event_handler: mcp_event_handler,
                    role_provider_builder: Arc::new(RuntimeTaskRoleProviderBuilder),
                    context_resolver,
                },
                WorkerLoopTerminalRuntime::new(terminal_lifecycle_router, None),
            );
        })
        .map_err(|error| anyhow::anyhow!("failed to spawn worker loop: {error}"))?;

    Ok(ManualLoopWorker {
        command_tx,
        message_rx,
        handle: Some(handle),
    })
}

async fn wait_for_terminal_output(
    registry: &ToolRegistry,
    tool_context: ToolContext,
    task_id: &str,
    expected: &str,
) -> Result<()> {
    for attempt in 0..40 {
        let read = registry
            .execute(
                tool_context.clone(),
                ToolCall {
                    id: format!("call-terminal-read-{attempt}"),
                    name: "terminal_read".to_owned(),
                    args_json: serde_json::json!({
                    "task_id": task_id,
                    "offset": 0,
                    "limit_bytes": 1024,
                    "include_content": true
                    })
                    .to_string(),
                },
            )
            .await?;
        if read.content.contains(expected) {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    anyhow::bail!("terminal output did not include {expected}");
}

fn edge_terminal_entry(task_id: &str, status: TerminalTaskStatus) -> Result<TerminalTaskEntry> {
    Ok(TerminalTaskEntry {
        schema_version: sigil_kernel::terminal_task::TERMINAL_TASK_SCHEMA_VERSION,
        handle: TerminalTaskHandle {
            task_id: TerminalTaskId::new(task_id)?,
            command_sha256: "0".repeat(64),
            cwd_label: ".".to_owned(),
            shell_label: "sh".to_owned(),
            shell_sha256: "1".repeat(64),
            log_ref: format!("terminal-log:{task_id}"),
            created_at_ms: 10,
            execution_backend: None,
            execution_backend_capabilities: None,
            enforcement_backend: None,
            enforcement_backend_capabilities: None,
            sandbox_profile: None,
        },
        generation: 1,
        status,
        readiness: sigil_kernel::TerminalReadinessStatus::None,
        output_preview: None,
        output_hash: Some(sigil_kernel::stable_event_hash("old output")),
        output_truncated: false,
        output_total_bytes: 0,
        output_limit_bytes: None,
        output_termination_reason: None,
        cleanup: None,
        updated_at_ms: 20,
    })
}

#[test]
fn mcp_runtime_event_handler_forwards_channel_events() -> Result<()> {
    let (event_tx, event_rx) = mpsc::channel();
    let handler = ChannelMcpRuntimeEventHandler::new_test(event_tx);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    runtime.block_on(async {
        handler
            .progress(sigil_runtime::McpProgressNotification {
                server_name: "filesystem".to_owned(),
                progress_token: "scan".to_owned(),
                progress: Some(1.0),
                total: Some(2.0),
                message: Some("Scanning".to_owned()),
            })
            .await?;
        handler
            .list_changed(sigil_runtime::McpListChangedNotification {
                server_name: "filesystem".to_owned(),
                kind: sigil_runtime::McpListChangedKind::Tools,
            })
            .await
    })?;

    let progress = event_rx.recv_timeout(Duration::from_secs(1))?;
    assert!(matches!(
        progress,
        McpRuntimeEvent::Progress(notification)
            if notification.server_name == "filesystem"
                && notification.progress_token == "scan"
                && notification.message.as_deref() == Some("Scanning")
    ));
    let list_changed = event_rx.recv_timeout(Duration::from_secs(1))?;
    assert!(matches!(
        list_changed,
        McpRuntimeEvent::ListChanged(notification)
            if notification.server_name == "filesystem"
                && notification.kind == sigil_runtime::McpListChangedKind::Tools
    ));
    Ok(())
}

#[test]
fn activate_lazy_mcp_reports_shared_agent_error_when_mutation_is_blocked() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp.path().join(".sigil/sessions/shared-activate.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let agent = Arc::new(Agent::new(
        PlannedProvider::new(vec![StreamPlan::Pending]),
        ToolRegistry::new(),
    ));

    let worker = spawn_loop_with_shared_agent(
        root_config,
        session_log_path,
        workspace_root,
        Arc::clone(&agent),
    )?;

    worker.send(WorkerCommand::ActivateLazyMcp {
        server_name: Some("ready-lazy".to_owned()),
    })?;

    let failure = worker.recv(Duration::from_secs(3))?;
    assert!(matches!(
        failure,
        WorkerMessage::RunFailed(ref error) if error == "cannot activate MCP while agent registry is shared"
    ));

    worker.send_shutdown()?;
    worker.join()
}

#[test]
fn refresh_mcp_server_keeps_pending_intent_when_agent_registry_is_shared() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp.path().join(".sigil/sessions/shared-refresh.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let agent = Arc::new(Agent::new(
        PlannedProvider::new(vec![StreamPlan::Pending]),
        ToolRegistry::new(),
    ));

    let worker = spawn_loop_with_shared_agent(
        root_config,
        session_log_path,
        workspace_root,
        Arc::clone(&agent),
    )?;

    worker.send(WorkerCommand::RefreshMcpServer {
        server_name: "missing".to_owned(),
    })?;

    let failure = worker.recv(Duration::from_secs(3))?;
    assert!(matches!(
        failure,
        WorkerMessage::RunFailed(ref error) if error == "cannot refresh MCP while agent registry is shared"
    ));

    drop(agent);

    let deadline = Instant::now() + Duration::from_secs(3);
    let mut saw_refreshing = false;
    let mut saw_deferred = false;
    while Instant::now() < deadline && !saw_deferred {
        let Some(message) = worker.recv_optional(Duration::from_millis(250))? else {
            continue;
        };
        match message {
            WorkerMessage::McpActivationStatus {
                server_name: Some(server_name),
                status: McpActivationStatus::Refreshing,
            } if server_name == "missing" => {
                saw_refreshing = true;
            }
            WorkerMessage::McpActivationStatus {
                server_name: Some(server_name),
                status: McpActivationStatus::Deferred,
            } if server_name == "missing" => {
                saw_deferred = true;
            }
            _ => {}
        }
    }

    assert!(
        saw_refreshing,
        "pending refresh should retry when registry is free"
    );
    assert!(
        saw_deferred,
        "retried missing server should resolve as deferred"
    );

    worker.send_shutdown()?;
    worker.join()
}

#[test]
fn cancel_run_reports_load_error_if_session_log_cannot_be_reloaded() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp.path().join(".sigil/sessions/cancel-reload-fail.jsonl");
    let store = JsonlSessionStore::new(&session_log_path)?;
    store.append(&SessionLogEntry::Control(ControlEntry::SessionIdentity {
        provider_name: "planned".to_owned(),
        model_name: "planned-model".to_owned(),
        resolved_model_route: None,
    }))?;

    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let agent = Arc::new(Agent::new(
        PlannedProvider::new(vec![StreamPlan::Pending]),
        ToolRegistry::new(),
    ));

    let worker = spawn_loop_with_shared_agent(
        root_config,
        session_log_path.clone(),
        workspace_root,
        Arc::clone(&agent),
    )?;

    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "never finishes".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let _ = worker.recv(Duration::from_secs(3))?;

    fs::write(
        &session_log_path,
        format!(
            "{{not-json}}\n{}\n",
            serde_json::to_string(&SessionLogEntry::User(ModelMessage::user("valid tail")))?
        ),
    )?;

    worker.send(WorkerCommand::CancelRun)?;
    let failure = worker.recv_until_with_timeout(Duration::from_secs(3), |message| {
        let text = match message {
            WorkerMessage::RunFailed(error) | WorkerMessage::Notice(error) => error,
            _ => return false,
        };
        text.contains("expected")
            || text.contains("failed to")
            || text.contains("middle corruption")
    })?;

    assert!(
        matches!(
            failure,
            WorkerMessage::RunFailed(_) | WorkerMessage::Notice(_)
        ),
        "unexpected cancel failure message: {failure:?}"
    );

    worker.send_shutdown()?;
    worker.join()
}

#[test]
fn shutdown_with_active_run_emits_an_honest_cancellation_terminal() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp.path().join(".sigil/sessions/shutdown-active.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let agent = Arc::new(Agent::new(
        PlannedProvider::new(vec![StreamPlan::Pending]),
        ToolRegistry::new(),
    ));

    let worker = spawn_loop_with_shared_agent(
        root_config,
        session_log_path,
        workspace_root,
        Arc::clone(&agent),
    )?;

    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "hold forever".to_owned(),
        reasoning_effort: ReasoningEffort::Max,
    })?;
    let _ = worker.recv(Duration::from_secs(3))?;

    worker.send_shutdown()?;
    let timeout_deadline = Instant::now() + Duration::from_secs(3);
    let mut saw_terminal = false;
    loop {
        if Instant::now() >= timeout_deadline {
            break;
        }
        match worker.recv_optional(Duration::from_millis(80))? {
            Some(WorkerMessage::RunCancelled { .. } | WorkerMessage::RunInterrupted { .. }) => {
                saw_terminal = true;
                break;
            }
            Some(_) => continue,
            None => break,
        }
    }

    worker.join()?;
    assert!(
        saw_terminal,
        "shutdown must emit a durable cancellation terminal"
    );
    Ok(())
}

#[test]
fn shutdown_without_active_run_does_not_emit_events() -> Result<()> {
    let temp = tempdir()?;
    let workspace_root = temp.path().to_path_buf();
    let session_log_path = temp.path().join(".sigil/sessions/shutdown-idle.jsonl");
    let root_config = test_root_config(&workspace_root, "planned", "planned-model");
    let agent = Arc::new(Agent::new(
        PlannedProvider::new(Vec::new()),
        ToolRegistry::new(),
    ));

    let worker = spawn_loop_with_shared_agent(
        root_config,
        session_log_path,
        workspace_root,
        Arc::clone(&agent),
    )?;

    worker.send_shutdown()?;
    let message = worker.recv(Duration::from_millis(200));
    assert!(
        message.is_err(),
        "idle shutdown should close without emitting run messages"
    );
    worker.join()?;
    Ok(())
}
