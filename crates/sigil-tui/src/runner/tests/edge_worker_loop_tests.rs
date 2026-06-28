use std::{
    fs,
    path::PathBuf,
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

use anyhow::Result;
use sigil_kernel::{
    Agent, AgentInvocationMode, AgentInvocationSource, AgentProfileId, AgentProfileSnapshotId,
    AgentResultContinuationEntry, AgentResultContinuationStatus, AgentRole,
    AgentRunContextSnapshot, AgentThreadId, AgentThreadStartedEntry, AgentThreadStatus,
    AgentThreadStatusChangedEntry, ControlEntry, DEFAULT_TASK_VERIFICATION_SCOPE_HASH,
    DurableEventType, JsonlSessionStore, McpElicitationDecision, McpElicitationEntry, ModelMessage,
    MutationEventRecorder, PlanApprovalPermission, Provider, ReasoningEffort, RootConfig, Session,
    SessionLogEntry, SessionRef, SessionStreamRecord, TaskChildSessionEntry,
    TaskChildSessionStatus, TaskId, TaskPlanEntry, TaskPlanStatus, TaskRouteStatus, TaskRunEntry,
    TaskRunStatus, TaskStepEntry, TaskStepId, TaskStepSpec, TaskStepStatus, TerminalTaskEntry,
    TerminalTaskHandle, TerminalTaskId, TerminalTaskStatus, ToolCall, ToolContext, ToolEffect,
    ToolExecutionEntry, ToolExecutionStatus, ToolRegistry, ToolResultMeta, VerificationScope,
    WorkspaceMutationDetected, WorkspaceRootSnapshot,
};
use sigil_runtime::McpRuntimeEventHandler;
use tempfile::tempdir;

use super::{
    super::{
        McpActivationStatus, WorkerCommand, WorkerMessage,
        elicitation_bridge::ChannelMcpElicitationHandler,
        mcp_event_bridge::{ChannelMcpRuntimeEventHandler, McpRuntimeEvent},
        worker_loop::append_cancelled_task_state,
        worker_loop::{
            PlanApprovalRequest, WorkerLoopMcpHandlers, agent_delegation_requirement_for_prompt,
            append_mcp_elicitation_audits, approve_plan, cancel_terminal_task, close_agent_thread,
            next_task_id, partition_agent_result_continuations,
            pending_agent_result_continuations_from_session,
            queued_background_ready_transient_context, refresh_terminal_task_statuses,
            resolve_continue_task, run_worker_loop,
        },
    },
    common::{PlannedProvider, StreamPlan, test_root_config},
};

struct ManualLoopWorker {
    command_tx: mpsc::Sender<WorkerCommand>,
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
fn agent_delegation_requirement_detects_explicit_subagent_prompts() {
    assert!(
        agent_delegation_requirement_for_prompt(
            "梳理 crates/sigil-tui，同时必须用子 agent 梳理 kernel"
        )
        .is_some()
    );
    assert!(
        agent_delegation_requirement_for_prompt(
            "Please use a sub-agent to inspect the runtime crate"
        )
        .is_some()
    );
    assert!(agent_delegation_requirement_for_prompt("讨论一下 subagent 设计").is_none());
    assert!(agent_delegation_requirement_for_prompt("这次不要用子 agent").is_none());
    assert!(agent_delegation_requirement_for_prompt("不需要子 agent，主 agent 直接回答").is_none());
    assert!(agent_delegation_requirement_for_prompt("无需子 agent，直接解释").is_none());
    assert!(agent_delegation_requirement_for_prompt("别开子 agent，保持单 agent").is_none());
    assert!(agent_delegation_requirement_for_prompt("answer without sub-agent").is_none());
    assert!(agent_delegation_requirement_for_prompt("please don't use a subagent").is_none());
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

    let (task_id, task_id_value, objective) =
        resolve_continue_task(&session, None).map_err(anyhow::Error::msg)?;

    assert_eq!(task_id.as_str(), "task_1");
    assert_eq!(task_id_value, "task_1");
    assert_eq!(objective, "resume me");
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
            mode: None,
            isolation: None,
        }],
        reason: None,
    }))?;

    let error = match resolve_continue_task(&session, None) {
        Ok((task_id, _, _)) => anyhow::bail!("completed task unexpectedly resumed: {task_id:?}"),
        Err(error) => error,
    };

    assert_eq!(error, "task task_1 is already completed");
    Ok(())
}

#[test]
fn append_cancelled_task_state_marks_active_task_step_and_child() -> Result<()> {
    let mut session = Session::new("deepseek", "model");
    let task_id = TaskId::new("task_1")?;
    let step_id = TaskStepId::new("step_1")?;
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
        steps: vec![TaskStepSpec {
            step_id: step_id.clone(),
            title: "running".to_owned(),
            display_name: None,
            detail: None,
            role: AgentRole::SubagentWrite,
            depends_on: Vec::new(),
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
        title: Some("running".to_owned()),
        summary: None,
        reason: None,
    }))?;
    session.append_control(ControlEntry::TaskChildSession(TaskChildSessionEntry {
        task_id: task_id.clone(),
        plan_version: 1,
        step_id,
        child_task_id: TaskId::new("child_1")?,
        child_session_ref: SessionRef::new_relative("children/task_1/step_1-child_1.jsonl")?,
        role: AgentRole::SubagentWrite,
        status: TaskChildSessionStatus::Started,
        summary_hash: None,
    }))?;

    append_cancelled_task_state(&mut session).map_err(anyhow::Error::msg)?;

    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TaskStep(step))
                if step.status == TaskStepStatus::Cancelled
        )
    }));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TaskChildSession(child))
                if child.status == TaskChildSessionStatus::Cancelled
        )
    }));
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
fn close_agent_thread_appends_runtime_close_control() -> Result<()> {
    let temp = tempdir()?;
    let root_config = test_root_config(temp.path(), "planned", "planned-model");
    let session_log_path = temp.path().join(".sigil/sessions/session-agent.jsonl");
    let store = JsonlSessionStore::new(&session_log_path)?;
    let mut session = Session::new("planned", "planned-model").with_store(store);
    let thread_id = AgentThreadId::new("thread_1")?;
    let snapshot_id = AgentProfileSnapshotId::new("snapshot_1")?;

    session.append_control(ControlEntry::AgentThreadStarted(AgentThreadStartedEntry {
        thread_id: thread_id.clone(),
        parent_thread_id: None,
        parent_session_ref: SessionRef::new_relative("session-agent.jsonl")?,
        thread_session_ref: SessionRef::new_relative("children/thread_1.jsonl")?,
        profile_id: AgentProfileId::new("explore")?,
        profile_snapshot_id: snapshot_id.clone(),
        run_context: AgentRunContextSnapshot {
            profile_snapshot_id: snapshot_id,
            provider: "planned".to_owned(),
            model: "planned-model".to_owned(),
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
fn approve_plan_appends_plan_approved_control() -> Result<()> {
    let temp = tempdir()?;
    let root_config = test_root_config(temp.path(), "planned", "planned-model");
    let session_log_path = temp.path().join(".sigil/sessions/session-plan.jsonl");
    let store = JsonlSessionStore::new(&session_log_path)?;
    let session = Session::new("planned", "planned-model").with_store(store);
    let mut current_session = Some(session);

    let (entry, entries) = approve_plan(
        &root_config,
        &session_log_path,
        &mut current_session,
        PlanApprovalRequest {
            plan_text:
                "1. inspect crates/sigil-tui\n2. edit crates/sigil-tui/src/app.rs with preview"
                    .to_owned(),
            permission: PlanApprovalPermission::WorkspaceEdits,
            scope_summary: "inspect and edit".to_owned(),
            clear_planning_context: true,
        },
    )
    .map_err(anyhow::Error::msg)?;

    assert_eq!(entry.plan_version, 1);
    assert_eq!(entry.permission, PlanApprovalPermission::WorkspaceEdits);
    assert_eq!(entry.scope.summary, "inspect and edit");
    assert_eq!(entry.scope.workspace_paths, vec!["crates/sigil-tui"]);
    assert!(entry.clear_planning_context);
    assert!(entries.iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::PlanApproved(approved))
                if approved.permission == PlanApprovalPermission::WorkspaceEdits
        )
    }));
    let persisted = JsonlSessionStore::read_entries(&session_log_path)?;
    assert!(persisted.iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::PlanApproved(approved))
                if approved.scope.summary == "inspect and edit"
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
    let mcp_event_handler = Arc::new(ChannelMcpRuntimeEventHandler::new(mcp_event_tx));
    let registry = sigil_runtime::build_tool_registry_without_eager_mcp(
        &root_config,
        &provider.capabilities(),
        temp.path().to_path_buf(),
        elicitation_handler,
        mcp_event_handler,
    )?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let session_log_path = temp.path().join(".sigil/sessions/session-terminal.jsonl");
    let store = JsonlSessionStore::new(&session_log_path)?;
    let mut session = Session::new("planned", "planned-model").with_store(store.clone());
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
                    "command": "printf terminal-mutated > terminal-mutated.txt; printf cancel-tail; sleep 5"
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
        metadata: start.metadata.clone(),
        error: None,
        model_content_hash: Some("terminal-start-result".to_owned()),
    })))?;
    session.append_control(ControlEntry::TerminalTask(start_entry))?;
    let options = sigil_runtime::build_run_options(
        &root_config,
        temp.path().to_path_buf(),
        sigil_kernel::InteractionMode::Interactive,
    );
    let mut current_session = None;

    let (entry, entries) = cancel_terminal_task(
        &runtime,
        registry,
        &root_config,
        &options,
        &session_log_path,
        &mut current_session,
        task_id.to_owned(),
    )
    .map_err(anyhow::Error::msg)?;

    assert!(matches!(entry.status, TerminalTaskStatus::Cancelled));
    assert!(
        entry
            .output_preview
            .as_deref()
            .is_some_and(|preview| preview.contains("cancel-tail"))
    );
    assert!(entry.output_hash.is_some());
    assert!(entries.iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
                if execution.tool_name == "terminal_cancel"
                    && execution.status == ToolExecutionStatus::Started
                    && execution.model_content_hash.is_none()
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
fn refresh_terminal_task_statuses_audits_natural_exit_and_workspace_mutation() -> Result<()> {
    let temp = tempdir()?;
    let root_config = test_root_config(temp.path(), "planned", "planned-model");
    let provider = PlannedProvider::new(Vec::new());
    let (message_tx, _message_rx) = mpsc::channel();
    let elicitation_handler = Arc::new(ChannelMcpElicitationHandler::new(message_tx));
    let (mcp_event_tx, _mcp_event_rx) = mpsc::channel();
    let mcp_event_handler = Arc::new(ChannelMcpRuntimeEventHandler::new(mcp_event_tx));
    let registry = sigil_runtime::build_tool_registry_without_eager_mcp(
        &root_config,
        &provider.capabilities(),
        temp.path().to_path_buf(),
        elicitation_handler,
        mcp_event_handler,
    )?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let session_log_path = temp.path().join(".sigil/sessions/session-terminal.jsonl");
    let store = JsonlSessionStore::new(&session_log_path)?;
    let mut session = Session::new("planned", "planned-model").with_store(store.clone());
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
    let task_id = "terminal-natural-exit";
    let start = runtime.block_on(
        registry.execute(
            tool_context.clone(),
            ToolCall {
                id: "call-terminal-start".to_owned(),
                name: "terminal_start".to_owned(),
                args_json: serde_json::json!({
                    "task_id": task_id,
                    "command": "printf terminal-mutated > terminal-natural.txt; printf natural-tail"
                })
                .to_string(),
            },
        ),
    )?;
    let start_entry = TerminalTaskEntry::from_tool_result_details(&start.metadata.details)?
        .expect("terminal_start should return terminal metadata");
    runtime.block_on(wait_for_terminal_output(
        &registry,
        tool_context,
        task_id,
        "natural-tail",
    ))?;

    session.append_control(ControlEntry::ToolExecution(Box::new(ToolExecutionEntry {
        call_id: "call-terminal-start".to_owned(),
        tool_name: "terminal_start".to_owned(),
        status: ToolExecutionStatus::Completed,
        duration_ms: Some(1),
        subjects: Vec::new(),
        changed_files: Vec::new(),
        metadata: start.metadata.clone(),
        error: None,
        model_content_hash: Some("terminal-start-result".to_owned()),
    })))?;
    session.append_control(ControlEntry::TerminalTask(start_entry))?;

    let options = sigil_runtime::build_run_options(
        &root_config,
        temp.path().to_path_buf(),
        sigil_kernel::InteractionMode::Interactive,
    );
    let mut current_session = Some(session);
    let mut updates = Vec::new();
    for _ in 0..40 {
        updates = refresh_terminal_task_statuses(
            &runtime,
            &registry,
            &options,
            &session_log_path,
            &mut current_session,
        )
        .map_err(anyhow::Error::msg)?;
        if !updates.is_empty() {
            break;
        }
        runtime.block_on(tokio::time::sleep(Duration::from_millis(25)));
    }

    assert_eq!(updates.len(), 1);
    let (entry, entries) = updates.remove(0);
    assert!(matches!(
        entry.status,
        TerminalTaskStatus::Exited { exit_code: Some(0) }
    ));
    assert!(entries.iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TerminalTask(task))
                if task.handle.task_id.as_str() == task_id
                    && matches!(task.status, TerminalTaskStatus::Exited { exit_code: Some(0) })
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
    let mcp_event_handler = Arc::new(ChannelMcpRuntimeEventHandler::new(mcp_event_tx));
    let registry = sigil_runtime::build_tool_registry_without_eager_mcp(
        &root_config,
        &provider.capabilities(),
        temp.path().to_path_buf(),
        elicitation_handler,
        mcp_event_handler,
    )?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let session_log_path = temp
        .path()
        .join(".sigil/sessions/session-terminal-failed.jsonl");
    let store = JsonlSessionStore::new(&session_log_path)?;
    let mut session = Session::new("planned", "planned-model").with_store(store);
    session.append_control(ControlEntry::TerminalTask(edge_terminal_entry(
        "terminal-missing-manager",
        TerminalTaskStatus::Running,
    )?))?;
    let options = sigil_runtime::build_run_options(
        &root_config,
        temp.path().to_path_buf(),
        sigil_kernel::InteractionMode::Interactive,
    );
    let mut current_session = None;

    let error = cancel_terminal_task(
        &runtime,
        registry,
        &root_config,
        &options,
        &session_log_path,
        &mut current_session,
        "terminal-missing-manager".to_owned(),
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
        self.message_rx
            .recv_timeout(timeout)
            .map_err(|error| anyhow::anyhow!("timed out waiting for worker message: {error}"))
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
    let (command_tx, command_rx) = mpsc::channel();
    let (message_tx, message_rx) = mpsc::channel();
    let options = sigil_runtime::build_run_options(
        &root_config,
        workspace_root.clone(),
        sigil_kernel::InteractionMode::Interactive,
    );
    let provider_capabilities = agent.provider_capabilities();
    let agent_for_loop = Arc::clone(&agent);
    let elicitation_handler = Arc::new(ChannelMcpElicitationHandler::new(message_tx.clone()));
    let (mcp_event_tx, mcp_event_rx) = mpsc::channel();
    let mcp_event_handler = Arc::new(ChannelMcpRuntimeEventHandler::new(mcp_event_tx));

    let handle = thread::Builder::new()
        .name("sigil-edge-worker-loop-test".to_owned())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("edge worker runtime should build");
            run_worker_loop(
                runtime,
                agent_for_loop,
                root_config,
                provider_capabilities,
                workspace_root,
                session_log_path,
                options,
                command_rx,
                message_tx,
                WorkerLoopMcpHandlers {
                    elicitation_handler,
                    event_handler: mcp_event_handler,
                    event_rx: mcp_event_rx,
                },
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
                        "limit_bytes": 1024
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
        handle: TerminalTaskHandle {
            task_id: TerminalTaskId::new(task_id)?,
            command: "sleep 5".to_owned(),
            cwd: PathBuf::from("."),
            shell: "sh".to_owned(),
            log_path: PathBuf::from(".sigil/terminal")
                .join(task_id)
                .join("output.log"),
            created_at_ms: 10,
            execution_backend: None,
            execution_backend_capabilities: None,
        },
        status,
        output_preview: Some("old preview".to_owned()),
        output_hash: Some("sha256:old".to_owned()),
        output_truncated: false,
        updated_at_ms: 20,
    })
}

#[test]
fn mcp_runtime_event_handler_forwards_channel_events() -> Result<()> {
    let (event_tx, event_rx) = mpsc::channel();
    let handler = ChannelMcpRuntimeEventHandler::new(event_tx);
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
    let failure = worker.recv(Duration::from_secs(3))?;

    assert!(
        matches!(
            failure,
            WorkerMessage::RunFailed(ref error)
                if error.contains("expected")
                    || error.contains("failed to")
                    || error.contains("middle corruption")
        ),
        "unexpected cancel failure message: {failure:?}"
    );

    worker.send_shutdown()?;
    worker.join()
}

#[test]
fn shutdown_with_active_run_does_not_emit_run_cancelled_event() -> Result<()> {
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
    let timeout_deadline = Instant::now() + Duration::from_millis(400);
    loop {
        if Instant::now() >= timeout_deadline {
            break;
        }
        match worker.recv_optional(Duration::from_millis(80))? {
            Some(WorkerMessage::RunCancelled { .. }) => {
                anyhow::bail!("unexpected RunCancelled during shutdown with active run")
            }
            Some(_) => continue,
            None => break,
        }
    }

    worker.join()?;
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
