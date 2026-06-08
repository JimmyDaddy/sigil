use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    thread,
    time::Duration,
};

use anyhow::{Context, Result, anyhow};
use termquill_kernel::{
    Agent, AgentRunOptions, AgentRunResult, ApprovalHandler, CompactionConfig, CompactionRecord,
    CompactionThresholdStatus, EventHandler, InteractionMode, JsonlSessionStore, ReasoningEffort,
    RootConfig, RunEvent, Session, SessionLogEntry, ToolApproval, ToolCall, ToolRegistry, ToolSpec,
};
use termquill_provider_deepseek::{DeepSeekProvider, DeepSeekProviderConfig};

use crate::context_window::effective_compaction_config;

#[derive(Debug)]
pub enum WorkerCommand {
    SubmitPrompt {
        prompt: String,
        reasoning_effort: ReasoningEffort,
    },
    ApprovalDecision {
        call_id: String,
        approved: bool,
    },
    CancelRun,
    CompactNow,
    SwitchSession {
        session_log_path: PathBuf,
    },
    Shutdown,
}

#[derive(Debug)]
pub enum WorkerMessage {
    Event(Box<RunEvent>),
    Notice(String),
    RunStarted {
        prompt: String,
    },
    RunFinished {
        result: AgentRunResult,
        entries: Vec<SessionLogEntry>,
    },
    RunCancelled {
        session_log_path: PathBuf,
        provider_name: String,
        model_name: String,
        entries: Vec<SessionLogEntry>,
    },
    SessionSwitched {
        session_log_path: PathBuf,
        provider_name: String,
        model_name: String,
        entries: Vec<SessionLogEntry>,
    },
    SessionCompacted {
        session_log_path: PathBuf,
        provider_name: String,
        model_name: String,
        record: CompactionRecord,
        trigger: CompactionTrigger,
        entries: Vec<SessionLogEntry>,
    },
    RunFailed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionTrigger {
    Manual,
    AutomaticHardThreshold,
}

pub fn spawn_agent_worker(
    root_config: RootConfig,
    session_log_path: PathBuf,
    workspace_root: PathBuf,
) -> Result<(mpsc::Sender<WorkerCommand>, mpsc::Receiver<WorkerMessage>)> {
    let (command_tx, command_rx) = mpsc::channel();
    let (message_tx, message_rx) = mpsc::channel();

    let options = build_run_options(&root_config, workspace_root.clone());
    let provider_config = load_deepseek_config(&root_config)?;

    thread::Builder::new()
        .name("termquill-agent-worker".to_owned())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ = message_tx.send(WorkerMessage::RunFailed(format!("{error:#}")));
                    return;
                }
            };

            let mut registry = ToolRegistry::new();
            termquill_tools_builtin::register_builtin_tools(&mut registry);
            if let Err(error) = runtime.block_on(termquill_mcp::register_mcp_tools(
                &mut registry,
                &root_config.mcp_servers,
            )) {
                let _ = message_tx.send(WorkerMessage::RunFailed(format!("{error:#}")));
                return;
            }

            let provider = match DeepSeekProvider::new(provider_config) {
                Ok(provider) => provider,
                Err(error) => {
                    let _ = message_tx.send(WorkerMessage::RunFailed(format!("{error:#}")));
                    return;
                }
            };
            let agent = Arc::new(Agent::new(provider, registry));
            run_worker_loop(
                runtime,
                agent,
                root_config,
                session_log_path,
                options,
                command_rx,
                message_tx,
            );
        })
        .context("failed to spawn termquill agent worker")?;

    Ok((command_tx, message_rx))
}

fn run_worker_loop<P>(
    runtime: tokio::runtime::Runtime,
    agent: Arc<Agent<P>>,
    root_config: RootConfig,
    session_log_path: PathBuf,
    options: AgentRunOptions,
    command_rx: mpsc::Receiver<WorkerCommand>,
    message_tx: mpsc::Sender<WorkerMessage>,
) where
    P: termquill_kernel::Provider + Send + Sync + 'static,
{
    let mut current_session_log_path = session_log_path;
    let mut current_session = match load_session(
        &root_config.agent.provider,
        &root_config.agent.model,
        &current_session_log_path,
    ) {
        Ok(session) => Some(session),
        Err(error) => {
            let _ = message_tx.send(WorkerMessage::RunFailed(format!("{error:#}")));
            return;
        }
    };

    let (task_result_tx, task_result_rx) = mpsc::channel::<RunTaskResult>();
    let mut active_run: Option<ActiveRun> = None;
    let mut next_run_id = 1_u64;
    let mut discarded_run_ids = BTreeSet::new();

    loop {
        while let Ok(task_result) = task_result_rx.try_recv() {
            if discarded_run_ids.remove(&task_result.run_id) {
                continue;
            }
            active_run = None;
            current_session = Some(task_result.session);
            let auto_compaction = match current_session.as_mut() {
                Some(session) => {
                    let effective_config = effective_compaction_config(
                        session.provider_name(),
                        session.model_name(),
                        &options.compaction_config,
                    );
                    match auto_compact_session(session, &effective_config) {
                        Ok(record) => record,
                        Err(error) => {
                            let _ = message_tx.send(WorkerMessage::Notice(format!(
                                "automatic compaction skipped: {error}",
                            )));
                            None
                        }
                    }
                }
                None => None,
            };
            match task_result.result {
                Ok(run_result) => {
                    let entries = current_session
                        .as_ref()
                        .map(|session| session.entries().to_vec())
                        .unwrap_or_default();
                    let _ = message_tx.send(WorkerMessage::RunFinished {
                        result: run_result,
                        entries,
                    });
                }
                Err(error) => {
                    let _ = message_tx.send(WorkerMessage::RunFailed(error));
                }
            }
            if let (Some(session), Some(record)) = (current_session.as_ref(), auto_compaction) {
                let _ = message_tx.send(session_compacted_message(
                    &current_session_log_path,
                    session,
                    record,
                    CompactionTrigger::AutomaticHardThreshold,
                ));
            }
        }

        match command_rx.recv_timeout(Duration::from_millis(50)) {
            Ok(WorkerCommand::SubmitPrompt {
                prompt,
                reasoning_effort,
            }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "agent is already running".to_owned(),
                    ));
                    continue;
                }

                let Some(run_session) = current_session.take() else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "session state is unavailable".to_owned(),
                    ));
                    continue;
                };

                let _ = message_tx.send(WorkerMessage::RunStarted {
                    prompt: prompt.clone(),
                });

                let mut handler = ChannelEventHandler::new(message_tx.clone());
                let (approval_tx, approval_rx) = mpsc::channel();
                let agent = Arc::clone(&agent);
                let mut options = options.clone();
                options.reasoning_effort = Some(reasoning_effort);
                let task_result_tx = task_result_tx.clone();
                let run_id = next_run_id;
                next_run_id += 1;

                let handle = runtime.spawn(async move {
                    let mut run_session = run_session;
                    let result = {
                        let mut approval_handler = ChannelApprovalHandler::new(approval_rx);
                        agent
                            .run_with_approval(
                                &mut run_session,
                                prompt,
                                options,
                                &mut handler,
                                &mut approval_handler,
                            )
                            .await
                            .map_err(|error| format!("{error:#}"))
                    };
                    let _ = task_result_tx.send(RunTaskResult {
                        run_id,
                        session: run_session,
                        result,
                    });
                });

                active_run = Some(ActiveRun {
                    run_id,
                    handle,
                    approval_tx,
                });
            }
            Ok(WorkerCommand::ApprovalDecision { call_id, approved }) => {
                if let Some(active_run) = &active_run {
                    let approval = if approved {
                        ToolApproval::Approve
                    } else {
                        ToolApproval::Deny {
                            reason: "denied in TUI".to_owned(),
                        }
                    };
                    let _ = active_run
                        .approval_tx
                        .send(ApprovalSignal::Decision { call_id, approval });
                } else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "received stray approval decision without pending approval".to_owned(),
                    ));
                }
            }
            Ok(WorkerCommand::CancelRun) => {
                if let Some(active_run) = active_run.take() {
                    discarded_run_ids.insert(active_run.run_id);
                    let _ = active_run.approval_tx.send(ApprovalSignal::Cancel);
                    active_run.handle.abort();
                    match load_session(
                        &root_config.agent.provider,
                        &root_config.agent.model,
                        &current_session_log_path,
                    ) {
                        Ok(session) => {
                            let entries = session.entries().to_vec();
                            current_session = Some(session);
                            let _ = message_tx.send(WorkerMessage::RunCancelled {
                                session_log_path: current_session_log_path.clone(),
                                provider_name: current_session
                                    .as_ref()
                                    .map(|session| session.provider_name().to_owned())
                                    .unwrap_or_else(|| root_config.agent.provider.clone()),
                                model_name: current_session
                                    .as_ref()
                                    .map(|session| session.model_name().to_owned())
                                    .unwrap_or_else(|| root_config.agent.model.clone()),
                                entries,
                            });
                        }
                        Err(error) => {
                            let _ = message_tx.send(WorkerMessage::RunFailed(format!("{error:#}")));
                        }
                    }
                } else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "no active run to cancel".to_owned(),
                    ));
                }
            }
            Ok(WorkerCommand::CompactNow) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "cannot compact while the agent is running".to_owned(),
                    ));
                    continue;
                }
                let Some(mut session) = current_session.take() else {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "session state is unavailable".to_owned(),
                    ));
                    continue;
                };
                let effective_config = effective_compaction_config(
                    session.provider_name(),
                    session.model_name(),
                    &options.compaction_config,
                );
                match session.compact_now(&effective_config) {
                    Ok(record) => {
                        current_session = Some(session);
                        if let Some(session) = current_session.as_ref() {
                            let _ = message_tx.send(session_compacted_message(
                                &current_session_log_path,
                                session,
                                record,
                                CompactionTrigger::Manual,
                            ));
                        }
                    }
                    Err(error) => {
                        current_session = Some(session);
                        let _ = message_tx.send(WorkerMessage::RunFailed(format!("{error:#}")));
                    }
                }
            }
            Ok(WorkerCommand::SwitchSession { session_log_path }) => {
                if active_run.is_some() {
                    let _ = message_tx.send(WorkerMessage::RunFailed(
                        "cannot switch sessions while the agent is running".to_owned(),
                    ));
                    continue;
                }

                match load_session(
                    &root_config.agent.provider,
                    &root_config.agent.model,
                    &session_log_path,
                ) {
                    Ok(session) => {
                        let entries = session.entries().to_vec();
                        current_session_log_path = session_log_path.clone();
                        let provider_name = session.provider_name().to_owned();
                        let model_name = session.model_name().to_owned();
                        current_session = Some(session);
                        let _ = message_tx.send(WorkerMessage::SessionSwitched {
                            session_log_path,
                            provider_name,
                            model_name,
                            entries,
                        });
                    }
                    Err(error) => {
                        let _ = message_tx.send(WorkerMessage::RunFailed(format!("{error:#}")));
                    }
                }
            }
            Ok(WorkerCommand::Shutdown) => {
                if let Some(active_run) = active_run.take() {
                    discarded_run_ids.insert(active_run.run_id);
                    let _ = active_run.approval_tx.send(ApprovalSignal::Cancel);
                    active_run.handle.abort();
                }
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

#[derive(Clone)]
struct ChannelEventHandler {
    sender: mpsc::Sender<WorkerMessage>,
}

impl ChannelEventHandler {
    fn new(sender: mpsc::Sender<WorkerMessage>) -> Self {
        Self { sender }
    }
}

impl EventHandler for ChannelEventHandler {
    fn handle(&mut self, event: RunEvent) -> Result<()> {
        self.sender
            .send(WorkerMessage::Event(Box::new(event)))
            .map_err(|error| anyhow!("failed to forward run event: {error}"))
    }
}

struct ActiveRun {
    run_id: u64,
    handle: tokio::task::JoinHandle<()>,
    approval_tx: mpsc::Sender<ApprovalSignal>,
}

struct RunTaskResult {
    run_id: u64,
    session: Session,
    result: std::result::Result<AgentRunResult, String>,
}

#[derive(Debug, Clone)]
enum ApprovalSignal {
    Decision {
        call_id: String,
        approval: ToolApproval,
    },
    Cancel,
}

struct ChannelApprovalHandler {
    decision_rx: mpsc::Receiver<ApprovalSignal>,
}

impl ChannelApprovalHandler {
    fn new(decision_rx: mpsc::Receiver<ApprovalSignal>) -> Self {
        Self { decision_rx }
    }
}

impl ApprovalHandler for ChannelApprovalHandler {
    fn approve_tool_call(&mut self, call: &ToolCall, _spec: &ToolSpec) -> Result<ToolApproval> {
        loop {
            match self.decision_rx.recv() {
                Ok(ApprovalSignal::Decision { call_id, approval }) if call_id == call.id => {
                    return Ok(approval);
                }
                Ok(ApprovalSignal::Decision { .. }) => {}
                Ok(ApprovalSignal::Cancel) => {
                    return Ok(ToolApproval::Deny {
                        reason: "run cancelled from TUI".to_owned(),
                    });
                }
                Err(error) => {
                    return Err(anyhow!("approval channel closed: {error}"));
                }
            }
        }
    }
}

fn build_run_options(root_config: &RootConfig, workspace_root: PathBuf) -> AgentRunOptions {
    AgentRunOptions {
        workspace_root,
        max_turns: root_config.agent.max_turns,
        tool_timeout_secs: root_config.agent.tool_timeout_secs,
        reasoning_effort: Some(termquill_kernel::ReasoningEffort::Max),
        traffic_partition_key: Some("local-user".to_owned()),
        interaction_mode: InteractionMode::Interactive,
        permission_config: root_config.permission.clone(),
        memory_config: root_config.memory.clone(),
        compaction_config: root_config.compaction.clone(),
    }
}

fn load_deepseek_config(root_config: &RootConfig) -> Result<DeepSeekProviderConfig> {
    let provider_config_value = root_config
        .providers
        .get("deepseek")
        .cloned()
        .ok_or_else(|| anyhow!("missing [providers.deepseek] in termquill.toml"))?;
    serde_json::from_value(provider_config_value).context("invalid deepseek provider config")
}

fn load_session(provider_name: &str, model_name: &str, session_log_path: &Path) -> Result<Session> {
    let store = JsonlSessionStore::new(session_log_path)?;
    Session::load_from_store(provider_name.to_owned(), model_name.to_owned(), store)
}

fn auto_compact_session(
    session: &mut Session,
    config: &CompactionConfig,
) -> Result<Option<CompactionRecord>> {
    if config.threshold_status(session.stats().last_prompt_tokens)
        != CompactionThresholdStatus::Hard
    {
        return Ok(None);
    }
    if !session.can_compact(config) {
        return Ok(None);
    }

    session.compact_now(config).map(Some)
}

fn session_compacted_message(
    session_log_path: &Path,
    session: &Session,
    record: CompactionRecord,
    trigger: CompactionTrigger,
) -> WorkerMessage {
    WorkerMessage::SessionCompacted {
        session_log_path: session_log_path.to_path_buf(),
        provider_name: session.provider_name().to_owned(),
        model_name: session.model_name().to_owned(),
        record,
        trigger,
        entries: session.entries().to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, VecDeque},
        path::{Path, PathBuf},
        pin::Pin,
        sync::{Arc, Mutex, mpsc},
        thread,
        time::Duration,
    };

    use anyhow::{Context, Result, anyhow};
    use async_trait::async_trait;
    use futures::{Stream, stream};
    use tempfile::tempdir;
    use termquill_kernel::{
        Agent, AgentConfig, CompactionConfig, McpServerConfig, MemoryConfig, ModelMessage,
        PermissionConfig, Provider, ProviderCapabilities, ProviderChunk, ReasoningEffort,
        RootConfig, RunEvent, SessionConfig, SessionLogEntry, Tool, ToolCall, ToolContext,
        ToolRegistry, ToolResult, ToolResultMeta, ToolSpec, UsageStats, WorkspaceConfig,
    };

    use super::{
        CompactionTrigger, WorkerCommand, WorkerMessage, build_run_options, run_worker_loop,
    };

    fn test_root_config(workspace_root: &Path, provider: &str, model: &str) -> RootConfig {
        RootConfig {
            workspace: WorkspaceConfig {
                root: workspace_root.display().to_string(),
            },
            session: SessionConfig {
                log_dir: ".termquill/sessions".to_owned(),
            },
            agent: AgentConfig {
                provider: provider.to_owned(),
                model: model.to_owned(),
                max_turns: 8,
                tool_timeout_secs: 30,
            },
            permission: PermissionConfig::default(),
            memory: MemoryConfig { enabled: false },
            compaction: CompactionConfig::default(),
            providers: BTreeMap::new(),
            mcp_servers: Vec::<McpServerConfig>::new(),
        }
    }

    struct TestWorker {
        command_tx: mpsc::Sender<WorkerCommand>,
        message_rx: mpsc::Receiver<WorkerMessage>,
        handle: Option<thread::JoinHandle<()>>,
    }

    impl TestWorker {
        fn send(&self, command: WorkerCommand) -> Result<()> {
            self.command_tx
                .send(command)
                .map_err(|error| anyhow!("failed to send worker command: {error}"))
        }

        fn recv(&self) -> Result<WorkerMessage> {
            self.message_rx
                .recv_timeout(Duration::from_secs(3))
                .map_err(|error| anyhow!("timed out waiting for worker message: {error}"))
        }

        fn recv_until<F>(&self, predicate: F) -> Result<WorkerMessage>
        where
            F: Fn(&WorkerMessage) -> bool,
        {
            loop {
                let message = self.recv()?;
                if predicate(&message) {
                    return Ok(message);
                }
            }
        }

        fn shutdown(mut self) -> Result<()> {
            let _ = self.command_tx.send(WorkerCommand::Shutdown);
            if let Some(handle) = self.handle.take() {
                handle
                    .join()
                    .map_err(|_| anyhow!("worker thread panicked during shutdown"))?;
            }
            Ok(())
        }
    }

    impl Drop for TestWorker {
        fn drop(&mut self) {
            let _ = self.command_tx.send(WorkerCommand::Shutdown);
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    fn spawn_test_worker<P>(
        root_config: RootConfig,
        session_log_path: PathBuf,
        agent: Agent<P>,
        workspace_root: PathBuf,
    ) -> Result<TestWorker>
    where
        P: Provider + Send + Sync + 'static,
    {
        let (command_tx, command_rx) = mpsc::channel();
        let (message_tx, message_rx) = mpsc::channel();
        let options = build_run_options(&root_config, workspace_root);
        let agent = Arc::new(agent);
        let handle = thread::Builder::new()
            .name("termquill-test-agent-worker".to_owned())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                    .expect("test runtime should build");
                run_worker_loop(
                    runtime,
                    agent,
                    root_config,
                    session_log_path,
                    options,
                    command_rx,
                    message_tx,
                );
            })
            .context("failed to spawn test worker")?;

        Ok(TestWorker {
            command_tx,
            message_rx,
            handle: Some(handle),
        })
    }

    fn wait_for_session_entry<F>(session_log_path: &Path, predicate: F) -> Result<()>
    where
        F: Fn(&SessionLogEntry) -> bool,
    {
        for _ in 0..60 {
            let entries = termquill_kernel::JsonlSessionStore::read_entries(session_log_path)?;
            if entries.iter().any(&predicate) {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(25));
        }
        Err(anyhow!(
            "timed out waiting for durable session entry in {}",
            session_log_path.display()
        ))
    }

    #[derive(Clone)]
    enum StreamPlan {
        Chunks(Vec<ProviderChunk>),
        Pending,
    }

    #[derive(Clone)]
    struct PlannedProvider {
        plans: Arc<Mutex<VecDeque<StreamPlan>>>,
    }

    impl PlannedProvider {
        fn new(plans: Vec<StreamPlan>) -> Self {
            Self {
                plans: Arc::new(Mutex::new(VecDeque::from(plans))),
            }
        }
    }

    #[async_trait]
    impl Provider for PlannedProvider {
        fn name(&self) -> &str {
            "planned"
        }

        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                exact_prefix_cache: false,
                reports_cache_tokens: false,
                supports_reasoning_stream: true,
                supports_tool_stream: true,
                supports_background_tasks: false,
                supports_response_handles: false,
                supports_reasoning_artifacts: false,
                supports_structured_output: false,
                supports_assistant_prefix_seed: false,
                supports_schema_constrained_tools: false,
                supports_infill_completion: false,
                supports_system_fingerprint: false,
            }
        }

        async fn stream(
            &self,
            _request: termquill_kernel::CompletionRequest,
        ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
            let plan = self
                .plans
                .lock()
                .expect("plans mutex should not be poisoned")
                .pop_front()
                .unwrap_or(StreamPlan::Pending);
            let stream: Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>> = match plan {
                StreamPlan::Chunks(chunks) => {
                    Box::pin(stream::iter(chunks.into_iter().map(Ok::<_, anyhow::Error>)))
                }
                StreamPlan::Pending => Box::pin(stream::pending()),
            };
            Ok(stream)
        }
    }

    struct ApprovalFlowProvider;

    #[async_trait]
    impl Provider for ApprovalFlowProvider {
        fn name(&self) -> &str {
            "approval-flow"
        }

        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                exact_prefix_cache: false,
                reports_cache_tokens: false,
                supports_reasoning_stream: true,
                supports_tool_stream: true,
                supports_background_tasks: false,
                supports_response_handles: false,
                supports_reasoning_artifacts: false,
                supports_structured_output: false,
                supports_assistant_prefix_seed: false,
                supports_schema_constrained_tools: false,
                supports_infill_completion: false,
                supports_system_fingerprint: false,
            }
        }

        async fn stream(
            &self,
            request: termquill_kernel::CompletionRequest,
        ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
            let tool_used = request
                .messages
                .iter()
                .any(|message| matches!(message.role, termquill_kernel::MessageRole::Tool));
            if tool_used {
                Ok(Box::pin(stream::iter(vec![
                    Ok(ProviderChunk::TextDelta("approved run finished".to_owned())),
                    Ok(ProviderChunk::Done),
                ])))
            } else {
                Ok(Box::pin(stream::iter(vec![
                    Ok(ProviderChunk::ToolCallStart {
                        id: "call-1".to_owned(),
                        name: "write_file".to_owned(),
                    }),
                    Ok(ProviderChunk::ToolCallArgsDelta {
                        id: "call-1".to_owned(),
                        delta: r#"{"path":"note.txt"}"#.to_owned(),
                    }),
                    Ok(ProviderChunk::ToolCallComplete(ToolCall {
                        id: "call-1".to_owned(),
                        name: "write_file".to_owned(),
                        args_json: r#"{"path":"note.txt"}"#.to_owned(),
                    })),
                    Ok(ProviderChunk::Done),
                ])))
            }
        }
    }

    struct WriteTool;

    #[async_trait]
    impl Tool for WriteTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "write_file".to_owned(),
                description: "write".to_owned(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" }
                    },
                    "required": ["path"]
                }),
                read_only: false,
            }
        }

        async fn execute(
            &self,
            _ctx: ToolContext,
            call_id: String,
            _args: serde_json::Value,
        ) -> Result<ToolResult> {
            Ok(ToolResult {
                call_id,
                tool_name: "write_file".to_owned(),
                content: "wrote file".to_owned(),
                is_error: false,
                metadata: ToolResultMeta::default(),
            })
        }
    }

    #[test]
    fn submit_prompt_emits_started_event_and_finished_messages() -> Result<()> {
        let temp = tempdir()?;
        let workspace_root = temp.path().to_path_buf();
        let session_log_path = temp.path().join(".termquill/sessions/session-worker.jsonl");
        let root_config = test_root_config(&workspace_root, "planned", "planned-model");
        let provider = PlannedProvider::new(vec![StreamPlan::Chunks(vec![
            ProviderChunk::TextDelta("hello from worker".to_owned()),
            ProviderChunk::Done,
        ])]);
        let agent = Agent::new(provider, ToolRegistry::new());
        let worker = spawn_test_worker(root_config, session_log_path, agent, workspace_root)?;

        worker.send(WorkerCommand::SubmitPrompt {
            prompt: "hello".to_owned(),
            reasoning_effort: ReasoningEffort::Max,
        })?;
        let started = worker.recv()?;
        assert!(matches!(
            started,
            WorkerMessage::RunStarted { ref prompt } if prompt == "hello"
        ));

        let text_event = worker.recv_until(|message| {
            matches!(
                message,
                WorkerMessage::Event(event)
                    if matches!(event.as_ref(), RunEvent::TextDelta(delta) if delta == "hello from worker")
            )
        })?;
        assert!(matches!(
            text_event,
            WorkerMessage::Event(event)
                if matches!(event.as_ref(), RunEvent::TextDelta(delta) if delta == "hello from worker")
        ));

        let finished =
            worker.recv_until(|message| matches!(message, WorkerMessage::RunFinished { .. }))?;
        assert!(matches!(
            finished,
            WorkerMessage::RunFinished { ref result, ref entries }
                if result.final_text == "hello from worker"
                    && result.tool_calls == 0
                    && entries.iter().any(|entry| matches!(entry, SessionLogEntry::User(message) if message.content.as_deref() == Some("hello")))
        ));

        worker.shutdown()?;
        Ok(())
    }

    #[test]
    fn approval_decision_is_forwarded_to_active_run() -> Result<()> {
        let temp = tempdir()?;
        let workspace_root = temp.path().to_path_buf();
        let session_log_path = temp
            .path()
            .join(".termquill/sessions/session-approval.jsonl");
        let root_config = test_root_config(&workspace_root, "approval-flow", "approval-model");
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(WriteTool));
        let agent = Agent::new(ApprovalFlowProvider, registry);
        let worker = spawn_test_worker(root_config, session_log_path, agent, workspace_root)?;

        worker.send(WorkerCommand::SubmitPrompt {
            prompt: "write".to_owned(),
            reasoning_effort: ReasoningEffort::Max,
        })?;
        let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunStarted { .. }))?;
        let approval_request = worker.recv_until(|message| {
            matches!(
                message,
                WorkerMessage::Event(event)
                    if matches!(event.as_ref(), RunEvent::ToolApprovalRequested { call, .. } if call.id == "call-1")
            )
        })?;
        assert!(matches!(
            approval_request,
            WorkerMessage::Event(event)
                if matches!(event.as_ref(), RunEvent::ToolApprovalRequested { call, .. } if call.id == "call-1")
        ));

        worker.send(WorkerCommand::ApprovalDecision {
            call_id: "call-1".to_owned(),
            approved: true,
        })?;

        let approval_resolved = worker.recv_until(|message| {
            matches!(
                message,
                WorkerMessage::Event(event)
                    if matches!(event.as_ref(), RunEvent::ToolApprovalResolved { call_id, approved, .. } if call_id == "call-1" && *approved)
            )
        })?;
        assert!(matches!(
            approval_resolved,
            WorkerMessage::Event(event)
                if matches!(event.as_ref(), RunEvent::ToolApprovalResolved { call_id, approved, .. } if call_id == "call-1" && *approved)
        ));

        let finished =
            worker.recv_until(|message| matches!(message, WorkerMessage::RunFinished { .. }))?;
        assert!(matches!(
            finished,
            WorkerMessage::RunFinished { ref result, ref entries }
                if result.final_text == "approved run finished"
                    && result.tool_calls == 1
                    && entries.iter().any(|entry| matches!(entry, SessionLogEntry::ToolResult(message) if message.content.as_deref() == Some("wrote file")))
        ));

        worker.shutdown()?;
        Ok(())
    }

    #[test]
    fn switch_session_restores_identity_and_entries() -> Result<()> {
        let temp = tempdir()?;
        let workspace_root = temp.path().to_path_buf();
        let current_log_path = temp
            .path()
            .join(".termquill/sessions/session-current.jsonl");
        let restore_log_path = temp
            .path()
            .join(".termquill/sessions/session-restored.jsonl");
        let root_config = test_root_config(&workspace_root, "default-provider", "default-model");
        let provider = PlannedProvider::new(vec![]);
        let agent = Agent::new(provider, ToolRegistry::new());

        let restore_store = termquill_kernel::JsonlSessionStore::new(&restore_log_path)?;
        restore_store.append(&SessionLogEntry::Control(
            termquill_kernel::ControlEntry::SessionIdentity {
                provider_name: "restored-provider".to_owned(),
                model_name: "restored-model".to_owned(),
            },
        ))?;
        restore_store.append(&SessionLogEntry::User(ModelMessage::user(
            "restored prompt",
        )))?;

        let worker = spawn_test_worker(root_config, current_log_path, agent, workspace_root)?;
        worker.send(WorkerCommand::SwitchSession {
            session_log_path: restore_log_path.clone(),
        })?;
        let switched = worker
            .recv_until(|message| matches!(message, WorkerMessage::SessionSwitched { .. }))?;
        assert!(matches!(
            switched,
            WorkerMessage::SessionSwitched {
                ref session_log_path,
                ref provider_name,
                ref model_name,
                ref entries,
            }
                if session_log_path == &restore_log_path
                    && provider_name == "restored-provider"
                    && model_name == "restored-model"
                    && entries.iter().any(|entry| matches!(entry, SessionLogEntry::User(message) if message.content.as_deref() == Some("restored prompt")))
        ));

        worker.shutdown()?;
        Ok(())
    }

    #[test]
    fn compact_now_persists_record_and_restores_session_view() -> Result<()> {
        let temp = tempdir()?;
        let workspace_root = temp.path().to_path_buf();
        let session_log_path = temp
            .path()
            .join(".termquill/sessions/session-compact.jsonl");
        let expected_session_log_path = session_log_path.clone();
        let mut root_config = test_root_config(&workspace_root, "planned", "planned-model");
        root_config.compaction.tail_messages = 2;
        let store = termquill_kernel::JsonlSessionStore::new(&session_log_path)?;
        store.append(&SessionLogEntry::Control(
            termquill_kernel::ControlEntry::SessionIdentity {
                provider_name: "planned".to_owned(),
                model_name: "planned-model".to_owned(),
            },
        ))?;
        store.append(&SessionLogEntry::User(ModelMessage::user("one")))?;
        store.append(&SessionLogEntry::Assistant(ModelMessage::assistant(
            Some("two".to_owned()),
            Vec::new(),
        )))?;
        store.append(&SessionLogEntry::User(ModelMessage::user("three")))?;
        store.append(&SessionLogEntry::Assistant(ModelMessage::assistant(
            Some("four".to_owned()),
            Vec::new(),
        )))?;

        let provider = PlannedProvider::new(vec![]);
        let agent = Agent::new(provider, ToolRegistry::new());
        let worker =
            spawn_test_worker(root_config, session_log_path.clone(), agent, workspace_root)?;

        worker.send(WorkerCommand::CompactNow)?;
        let compacted = worker
            .recv_until(|message| matches!(message, WorkerMessage::SessionCompacted { .. }))?;
        assert!(matches!(
            compacted,
            WorkerMessage::SessionCompacted { ref session_log_path, trigger, ref entries, .. }
                if session_log_path == &expected_session_log_path
                    && trigger == CompactionTrigger::Manual
                    && entries.iter().any(|entry| matches!(entry, SessionLogEntry::Control(termquill_kernel::ControlEntry::CompactionApplied(_))))
        ));

        worker.shutdown()?;
        Ok(())
    }

    #[test]
    fn hard_threshold_run_is_auto_compacted_after_finish() -> Result<()> {
        let temp = tempdir()?;
        let workspace_root = temp.path().to_path_buf();
        let session_log_path = temp
            .path()
            .join(".termquill/sessions/session-auto-compact.jsonl");
        let mut root_config = test_root_config(&workspace_root, "planned", "planned-model");
        root_config.compaction.context_window_tokens = Some(100);
        root_config.compaction.soft_threshold_ratio = 0.5;
        root_config.compaction.hard_threshold_ratio = 0.8;
        root_config.compaction.tail_messages = 1;

        let provider = PlannedProvider::new(vec![StreamPlan::Chunks(vec![
            ProviderChunk::Usage(UsageStats {
                prompt_tokens: 90,
                completion_tokens: 12,
                cache_hit_tokens: 0,
                cache_miss_tokens: 90,
                input_cost: 0.0,
                output_cost: 0.0,
                cache_savings: 0.0,
                system_fingerprint: None,
            }),
            ProviderChunk::TextDelta("finished turn".to_owned()),
            ProviderChunk::Done,
        ])]);
        let agent = Agent::new(provider, ToolRegistry::new());
        let worker = spawn_test_worker(root_config, session_log_path, agent, workspace_root)?;

        worker.send(WorkerCommand::SubmitPrompt {
            prompt: "hello".to_owned(),
            reasoning_effort: ReasoningEffort::Max,
        })?;
        let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunStarted { .. }))?;
        let _ =
            worker.recv_until(|message| matches!(message, WorkerMessage::RunFinished { .. }))?;
        let compacted = worker
            .recv_until(|message| matches!(message, WorkerMessage::SessionCompacted { .. }))?;
        assert!(matches!(
            compacted,
            WorkerMessage::SessionCompacted { trigger, ref record, ref entries, .. }
                if trigger == CompactionTrigger::AutomaticHardThreshold
                    && record.compacted_message_count == 1
                    && entries.iter().any(|entry| matches!(entry, SessionLogEntry::Control(termquill_kernel::ControlEntry::CompactionApplied(saved)) if saved == record))
        ));

        worker.shutdown()?;
        Ok(())
    }

    #[test]
    fn provider_context_window_prevents_early_auto_compaction() -> Result<()> {
        let temp = tempdir()?;
        let workspace_root = temp.path().to_path_buf();
        let session_log_path = temp
            .path()
            .join(".termquill/sessions/session-provider-window.jsonl");
        let mut root_config = test_root_config(&workspace_root, "deepseek", "deepseek-v4-pro");
        root_config.compaction.context_window_tokens = Some(128_000);
        root_config.compaction.soft_threshold_ratio = 0.5;
        root_config.compaction.hard_threshold_ratio = 0.8;
        root_config.compaction.tail_messages = 1;

        let provider = PlannedProvider::new(vec![StreamPlan::Chunks(vec![
            ProviderChunk::Usage(UsageStats {
                prompt_tokens: 90_354,
                completion_tokens: 12,
                cache_hit_tokens: 0,
                cache_miss_tokens: 90_354,
                input_cost: 0.0,
                output_cost: 0.0,
                cache_savings: 0.0,
                system_fingerprint: None,
            }),
            ProviderChunk::TextDelta("finished turn".to_owned()),
            ProviderChunk::Done,
        ])]);
        let agent = Agent::new(provider, ToolRegistry::new());
        let worker =
            spawn_test_worker(root_config, session_log_path.clone(), agent, workspace_root)?;

        worker.send(WorkerCommand::SubmitPrompt {
            prompt: "hello".to_owned(),
            reasoning_effort: ReasoningEffort::Max,
        })?;
        let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunStarted { .. }))?;
        let finished =
            worker.recv_until(|message| matches!(message, WorkerMessage::RunFinished { .. }))?;
        assert!(matches!(
            finished,
            WorkerMessage::RunFinished { ref entries, .. }
                if !entries.iter().any(|entry| matches!(
                    entry,
                    SessionLogEntry::Control(
                        termquill_kernel::ControlEntry::CompactionApplied(_)
                    )
                ))
        ));

        let entries = termquill_kernel::JsonlSessionStore::read_entries(&session_log_path)?;
        assert!(!entries.iter().any(|entry| matches!(
            entry,
            SessionLogEntry::Control(termquill_kernel::ControlEntry::CompactionApplied(_))
        )));

        worker.shutdown()?;
        Ok(())
    }

    #[test]
    fn cancel_run_without_active_task_reports_error() -> Result<()> {
        let temp = tempdir()?;
        let workspace_root = temp.path().to_path_buf();
        let session_log_path = temp.path().join(".termquill/sessions/session-idle.jsonl");
        let root_config = test_root_config(&workspace_root, "planned", "planned-model");
        let provider = PlannedProvider::new(vec![]);
        let agent = Agent::new(provider, ToolRegistry::new());
        let worker = spawn_test_worker(root_config, session_log_path, agent, workspace_root)?;

        worker.send(WorkerCommand::CancelRun)?;
        let error = worker.recv_until(|message| matches!(message, WorkerMessage::RunFailed(_)))?;
        assert!(matches!(
            error,
            WorkerMessage::RunFailed(ref text) if text == "no active run to cancel"
        ));

        worker.shutdown()?;
        Ok(())
    }

    #[test]
    fn approval_decision_without_active_run_reports_error() -> Result<()> {
        let temp = tempdir()?;
        let workspace_root = temp.path().to_path_buf();
        let session_log_path = temp
            .path()
            .join(".termquill/sessions/session-stray-approval.jsonl");
        let root_config = test_root_config(&workspace_root, "planned", "planned-model");
        let provider = PlannedProvider::new(vec![]);
        let agent = Agent::new(provider, ToolRegistry::new());
        let worker = spawn_test_worker(root_config, session_log_path, agent, workspace_root)?;

        worker.send(WorkerCommand::ApprovalDecision {
            call_id: "missing-call".to_owned(),
            approved: true,
        })?;
        let error = worker.recv_until(|message| matches!(message, WorkerMessage::RunFailed(_)))?;
        assert!(matches!(
            error,
            WorkerMessage::RunFailed(ref text)
                if text == "received stray approval decision without pending approval"
        ));

        worker.shutdown()?;
        Ok(())
    }

    #[test]
    fn cancel_active_run_restores_current_session_from_log() -> Result<()> {
        let temp = tempdir()?;
        let workspace_root = temp.path().to_path_buf();
        let session_log_path = temp.path().join(".termquill/sessions/session-cancel.jsonl");
        let expected_session_log_path = session_log_path.clone();
        let root_config = test_root_config(&workspace_root, "planned", "planned-model");
        let provider = PlannedProvider::new(vec![StreamPlan::Pending]);
        let agent = Agent::new(provider, ToolRegistry::new());
        let worker =
            spawn_test_worker(root_config, session_log_path.clone(), agent, workspace_root)?;

        worker.send(WorkerCommand::SubmitPrompt {
            prompt: "hang forever".to_owned(),
            reasoning_effort: ReasoningEffort::Max,
        })?;
        let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunStarted { .. }))?;
        wait_for_session_entry(&session_log_path, |entry| {
            matches!(
                entry,
                SessionLogEntry::User(message)
                    if message.content.as_deref() == Some("hang forever")
            )
        })?;
        worker.send(WorkerCommand::CancelRun)?;
        let cancelled =
            worker.recv_until(|message| matches!(message, WorkerMessage::RunCancelled { .. }))?;
        assert!(matches!(
            cancelled,
            WorkerMessage::RunCancelled {
                ref session_log_path,
                ref provider_name,
                ref model_name,
                ref entries,
            }
                if session_log_path == &expected_session_log_path
                    && provider_name == "planned"
                    && model_name == "planned-model"
                    && entries.iter().any(|entry| matches!(entry, SessionLogEntry::User(message) if message.content.as_deref() == Some("hang forever")))
        ));

        worker.shutdown()?;
        Ok(())
    }

    #[test]
    fn switch_session_while_active_run_reports_error() -> Result<()> {
        let temp = tempdir()?;
        let workspace_root = temp.path().to_path_buf();
        let session_log_path = temp.path().join(".termquill/sessions/session-active.jsonl");
        let restore_log_path = temp.path().join(".termquill/sessions/session-other.jsonl");
        let root_config = test_root_config(&workspace_root, "planned", "planned-model");
        let provider = PlannedProvider::new(vec![StreamPlan::Pending]);
        let agent = Agent::new(provider, ToolRegistry::new());
        let worker =
            spawn_test_worker(root_config, session_log_path.clone(), agent, workspace_root)?;

        worker.send(WorkerCommand::SubmitPrompt {
            prompt: "keep running".to_owned(),
            reasoning_effort: ReasoningEffort::Max,
        })?;
        let _ = worker.recv_until(|message| matches!(message, WorkerMessage::RunStarted { .. }))?;
        worker.send(WorkerCommand::SwitchSession {
            session_log_path: restore_log_path,
        })?;
        let error = worker.recv_until(|message| matches!(message, WorkerMessage::RunFailed(_)))?;
        assert!(matches!(
            error,
            WorkerMessage::RunFailed(ref text)
                if text == "cannot switch sessions while the agent is running"
        ));

        worker.send(WorkerCommand::CancelRun)?;
        let _ =
            worker.recv_until(|message| matches!(message, WorkerMessage::RunCancelled { .. }))?;
        worker.shutdown()?;
        Ok(())
    }
}
