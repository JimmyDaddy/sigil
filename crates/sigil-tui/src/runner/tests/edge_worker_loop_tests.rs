use std::{
    fs,
    path::PathBuf,
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

use anyhow::Result;
use sigil_kernel::{
    Agent, ControlEntry, JsonlSessionStore, ReasoningEffort, RootConfig, SessionLogEntry,
    ToolRegistry,
};
use tempfile::tempdir;

use super::{
    super::{
        WorkerCommand, WorkerMessage, elicitation_bridge::ChannelMcpElicitationHandler,
        worker_loop::run_worker_loop,
    },
    common::{PlannedProvider, StreamPlan, test_root_config},
};

struct ManualLoopWorker {
    command_tx: mpsc::Sender<WorkerCommand>,
    message_rx: mpsc::Receiver<WorkerMessage>,
    handle: Option<thread::JoinHandle<()>>,
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
                elicitation_handler,
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
