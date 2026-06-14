use std::{
    fs,
    path::PathBuf,
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

use anyhow::Result;
use sigil_kernel::{
    Agent, AgentRole, ControlEntry, JsonlSessionStore, McpElicitationDecision, McpElicitationEntry,
    ReasoningEffort, RootConfig, Session, SessionLogEntry, SessionRef, TaskChildSessionEntry,
    TaskChildSessionStatus, TaskId, TaskPlanEntry, TaskPlanStatus, TaskRouteStatus, TaskRunEntry,
    TaskRunStatus, TaskStepEntry, TaskStepId, TaskStepSpec, TaskStepStatus, ToolRegistry,
};
use sigil_runtime::McpRuntimeEventHandler;
use tempfile::tempdir;

use super::{
    super::{
        WorkerCommand, WorkerMessage,
        elicitation_bridge::ChannelMcpElicitationHandler,
        mcp_event_bridge::{ChannelMcpRuntimeEventHandler, McpRuntimeEvent},
        worker_loop::append_cancelled_task_state,
        worker_loop::{
            WorkerLoopMcpHandlers, append_mcp_elicitation_audits, next_task_id,
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
            detail: None,
            role: AgentRole::Executor,
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
            detail: None,
            role: AgentRole::SubagentWrite,
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
            detail: None,
            role: AgentRole::SubagentWrite,
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
        workspace_root,
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

    fs::write(&session_log_path, "{not-json}")?;

    worker.send(WorkerCommand::CancelRun)?;
    let failure = worker.recv(Duration::from_secs(3))?;

    assert!(matches!(
        failure,
        WorkerMessage::RunFailed(ref error)
            if error.contains("expected") || error.contains("failed to")
    ));

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
