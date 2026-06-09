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
use termquill_kernel::{
    Agent, AgentConfig, CompactionConfig, McpServerConfig, MemoryConfig, PermissionConfig,
    Provider, ProviderCapabilities, ProviderChunk, RootConfig, SessionConfig, SessionLogEntry,
    Tool, ToolAccess, ToolCall, ToolCategory, ToolContext, ToolPreviewCapability, ToolResult,
    ToolResultMeta, ToolSpec, WorkspaceConfig,
};

use super::super::{WorkerCommand, WorkerMessage, worker_loop::run_worker_loop};

pub(super) fn test_root_config(workspace_root: &Path, provider: &str, model: &str) -> RootConfig {
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
            max_turns: None,
            tool_timeout_secs: 30,
        },
        permission: PermissionConfig::default(),
        memory: MemoryConfig { enabled: false },
        compaction: CompactionConfig::default(),
        providers: BTreeMap::new(),
        mcp_servers: Vec::<McpServerConfig>::new(),
    }
}

pub(super) struct TestWorker {
    command_tx: mpsc::Sender<WorkerCommand>,
    message_rx: mpsc::Receiver<WorkerMessage>,
    handle: Option<thread::JoinHandle<()>>,
}

impl TestWorker {
    pub(super) fn send(&self, command: WorkerCommand) -> Result<()> {
        self.command_tx
            .send(command)
            .map_err(|error| anyhow!("failed to send worker command: {error}"))
    }

    pub(super) fn recv(&self) -> Result<WorkerMessage> {
        self.message_rx
            .recv_timeout(Duration::from_secs(3))
            .map_err(|error| anyhow!("timed out waiting for worker message: {error}"))
    }

    pub(super) fn recv_until<F>(&self, predicate: F) -> Result<WorkerMessage>
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

    pub(super) fn shutdown(mut self) -> Result<()> {
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

pub(super) fn spawn_test_worker<P>(
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
    let options = termquill_runtime::build_run_options(
        &root_config,
        workspace_root,
        termquill_kernel::InteractionMode::Interactive,
    );
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

pub(super) fn wait_for_session_entry<F>(session_log_path: &Path, predicate: F) -> Result<()>
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
pub(super) enum StreamPlan {
    Chunks(Vec<ProviderChunk>),
    Pending,
}

#[derive(Clone)]
pub(super) struct PlannedProvider {
    plans: Arc<Mutex<VecDeque<StreamPlan>>>,
}

impl PlannedProvider {
    pub(super) fn new(plans: Vec<StreamPlan>) -> Self {
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
            tool_name_max_chars: 64,
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

pub(super) struct ApprovalFlowProvider;

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
            tool_name_max_chars: 64,
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

pub(super) struct WriteTool;

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
            category: ToolCategory::File,
            access: ToolAccess::Write,
            preview: ToolPreviewCapability::Required,
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
            "write_file".to_owned(),
            "wrote file".to_owned(),
            ToolResultMeta::default(),
        ))
    }
}
