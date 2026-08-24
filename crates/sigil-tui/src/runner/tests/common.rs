use std::{
    collections::{BTreeMap, VecDeque},
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex, OnceLock, mpsc},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use futures::{Stream, StreamExt, stream};
use sigil_kernel::{
    Agent, AgentConfig, CompactionConfig, ConnectionId, ControlEntry, McpServerConfig,
    MemoryConfig, ModelRef, OrchestrationEvalReportManifestV1, OrchestrationEvalRouteGateV1,
    OrchestrationEvalRouteIdentityV1, OrchestrationEvalRouteStatus, PermissionConfig, Provider,
    ProviderCapabilities, ProviderChunk, ReasoningStreamSupport, RootConfig, SessionConfig,
    SessionLogEntry, TaskConfig, TaskRoutingPolicy, Tool, ToolAccess, ToolCall, ToolCategory,
    ToolContext, ToolPreviewCapability, ToolResult, ToolResultMeta, ToolSpec, WorkspaceConfig,
    stable_event_hash,
};

use super::super::{
    WorkerCommand, WorkerCommandSender, WorkerMessage,
    elicitation_bridge::ChannelMcpElicitationHandler,
    mcp_event_bridge::ChannelMcpRuntimeEventHandler,
    terminal_lifecycle_bridge::ChannelTerminalLifecycleRouter,
    worker_event::WorkerMcpRuntimeEventSender,
    worker_loop::{
        RuntimeTaskRoleProviderBuilder, TaskRoleProviderBuilder, WorkerLoopMcpHandlers,
        WorkerLoopSessionAttachment, WorkerLoopTerminalRuntime, run_worker_loop,
    },
};

pub(super) fn test_root_config(workspace_root: &Path, provider: &str, model: &str) -> RootConfig {
    RootConfig {
        config_version: 2,
        workspace: WorkspaceConfig {
            root: workspace_root.display().to_string(),
        },
        storage: Default::default(),
        session: SessionConfig {
            log_dir: Some(".sigil/sessions".to_owned()),
            retention: Default::default(),
        },
        agent: AgentConfig {
            runtime_provider: provider.to_owned(),
            connection: None,
            model: model.to_owned(),
            max_turns: None,
            tool_timeout_secs: 30,
        },
        model_request: Default::default(),
        permission: PermissionConfig::default(),
        memory: MemoryConfig::with_enabled(false),
        skills: Default::default(),
        compaction: CompactionConfig::default(),
        code_intelligence: Default::default(),
        terminal: Default::default(),
        execution: Default::default(),
        verification: Default::default(),
        appearance: Default::default(),
        task: TaskConfig {
            // Ordinary chat fixtures stay explicit: the release default is now `Auto`, and only
            // `routed_test_root_config` opts specific tests into automatic routing.
            routing_policy: TaskRoutingPolicy::Manual,
            ..Default::default()
        },
        connections: BTreeMap::new(),
        web: Default::default(),
        mcp_servers: Vec::<McpServerConfig>::new(),
    }
}

pub(super) fn routed_test_root_config(workspace_root: &Path, model: &str) -> RootConfig {
    let mut config = test_root_config(workspace_root, "planned", model);
    let connection_id = ConnectionId::new("test-default").expect("test connection id");
    config.config_version = sigil_kernel::CONFIG_VERSION_V2;
    config.agent.connection = Some(connection_id);
    config.connections.insert(
        "test-default".to_owned(),
        serde_json::json!({
            "label": "Test default",
            "provider": "deepseek",
            "protocol": "deepseek",
            "base_url": "https://api.deepseek.com",
            "credential": {
                "source": "environment",
                "name": "SIGIL_API_KEY"
            }
        }),
    );
    config
}

/// Routed config with an unauthenticated custom connection.
///
/// Admission-driven run fixtures use this instead of the environment-credential config so the
/// honest credential probe never depends on a process-global environment variable that parallel
/// tests may temporarily remove.
pub(super) fn routed_unauthenticated_test_root_config(
    workspace_root: &Path,
    model: &str,
) -> RootConfig {
    let mut config = test_root_config(workspace_root, "planned", model);
    let connection_id = ConnectionId::new("test-default").expect("test connection id");
    config.config_version = sigil_kernel::CONFIG_VERSION_V2;
    config.agent.connection = Some(connection_id);
    config.connections.insert(
        "test-default".to_owned(),
        serde_json::json!({
            "label": "Test default",
            "provider": "custom",
            "protocol": "chat_completions",
            "base_url": "http://127.0.0.1:1",
            "credential": { "source": "none" }
        }),
    );
    config
}

pub(super) fn routed_session_identity(
    root_config: &RootConfig,
    model: &str,
) -> Result<ControlEntry> {
    let connection_id = root_config
        .agent
        .connection
        .clone()
        .context("test route requires a default connection")?;
    let model_ref = ModelRef::new(connection_id, model.to_owned())?;
    let (provider_name, route) =
        sigil_runtime::provider_connections::resolve_model_route(root_config, &model_ref)
            .map_err(anyhow::Error::new)?;
    Ok(ControlEntry::SessionIdentity {
        provider_name,
        model_name: model.to_owned(),
        resolved_model_route: Some(route),
    })
}

pub(super) fn submit_plan_draft_chunks(call_id: &str, args_json: &str) -> Vec<ProviderChunk> {
    vec![
        ProviderChunk::ToolCallStart {
            id: call_id.to_owned(),
            name: sigil_kernel::SUBMIT_PLAN_DRAFT_TOOL_NAME.to_owned(),
        },
        ProviderChunk::ToolCallArgsDelta {
            id: call_id.to_owned(),
            delta: args_json.to_owned(),
        },
        ProviderChunk::ToolCallComplete(ToolCall {
            id: call_id.to_owned(),
            name: sigil_kernel::SUBMIT_PLAN_DRAFT_TOOL_NAME.to_owned(),
            args_json: args_json.to_owned(),
        }),
        ProviderChunk::Done,
    ]
}

pub(super) struct TestWorker {
    command_tx: WorkerCommandSender,
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
        self.recv_until(|message| !matches!(message, WorkerMessage::WorkerReady))
    }

    pub(super) fn recv_with_timeout(&self, timeout: Duration) -> Result<WorkerMessage> {
        self.message_rx
            .recv_timeout(timeout)
            .map_err(|error| anyhow!("timed out waiting for worker message: {error}"))
    }

    pub(super) fn recv_until<F>(&self, predicate: F) -> Result<WorkerMessage>
    where
        F: Fn(&WorkerMessage) -> bool,
    {
        self.recv_until_with_timeout(Duration::from_secs(3), predicate)
    }

    pub(super) fn recv_until_with_timeout<F>(
        &self,
        timeout: Duration,
        predicate: F,
    ) -> Result<WorkerMessage>
    where
        F: Fn(&WorkerMessage) -> bool,
    {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(anyhow!("timed out waiting for worker message"));
            }
            let message = self.recv_with_timeout(remaining)?;
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

/// Installs a release-qualified rollout manifest for the deterministic test route once per
/// process and points `SIGIL_ORCHESTRATION_ROLLOUT_MANIFEST` at it.
///
/// The manifest qualifies `deepseek` + `planned-model` with the standard Auto task config, which
/// matches the routed test fixtures that exercise DirectTask handoffs. Tests that use other task
/// configs or models keep the ReviewFirst baseline.
pub(super) fn install_qualified_rollout_manifest() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        let task = TaskConfig {
            routing_policy: TaskRoutingPolicy::Auto,
            ..TaskConfig::default()
        };
        let task_config_digest = sigil_runtime::orchestration_task_config_digest(&task)
            .expect("test task config digest");
        let build = sigil_runtime::ORCHESTRATION_RUNTIME_BUILD_ID;
        let commit = build
            .rsplit_once('+')
            .expect("test build identity includes commit")
            .1;
        let digest = format!("sha256:{}", "a".repeat(64));
        let identity = OrchestrationEvalRouteIdentityV1 {
            provider_adapter: "deepseek".to_owned(),
            provider_kind: "deepseek".to_owned(),
            endpoint_family: "openai_chat_completions".to_owned(),
            canonical_model_id: "planned-model".to_owned(),
            canonical_model_version: "Planned-Model@fp-test".to_owned(),
            route_fingerprint: digest.clone(),
            routing_prompt_digest: digest.clone(),
            planner_prompt_digest: digest.clone(),
            system_prompt_digest: digest.clone(),
            tool_profile_contract_digest: digest.clone(),
            task_config_digest,
            corpus_version: "rfc-0063-orchestration-v1".to_owned(),
            corpus_digest: digest,
            sigil_commit: commit.to_owned(),
            sigil_build: build.to_owned(),
        };
        let identity_digest =
            stable_event_hash(serde_json::to_vec(&identity).expect("serialize route identity"));
        let gate = OrchestrationEvalRouteGateV1 {
            identity,
            identity_digest,
            status: OrchestrationEvalRouteStatus::Qualified,
            chat_cases: 20,
            plan_review_cases: 15,
            direct_task_cases: 15,
            eligible_chat_cases: 20,
            eligible_plan_review_cases: 15,
            eligible_direct_task_cases: 15,
            provider_admitted_repetitions: 150,
            completed_repetitions: 150,
            chat_to_task_false_positive_rate_ppm: Some(0),
            plan_review_to_task_premature_rate_ppm: Some(0),
            direct_task_miss_rate_ppm: Some(0),
            chat_to_plan_review_overroute_rate_ppm: Some(0),
            plan_review_miss_rate_ppm: Some(0),
            cases_with_majority_misroute: 0,
            cases_with_duplicate_repetition_identity: 0,
            hard_invariant_violations: 0,
            reasons: Vec::new(),
        };
        let report = OrchestrationEvalReportManifestV1 {
            report_schema_version: 2,
            campaign_id: "campaign-rfc-0063-tui-test".to_owned(),
            started_at_unix_ms: 1,
            ended_at_unix_ms: 2,
            requested_repetitions: 150,
            results_jsonl_path: "private/results.jsonl".into(),
            summary_path: "private/summary.md".into(),
            route_gates: vec![gate],
        };
        let manifest = sigil_runtime::build_orchestration_rollout_manifest(&report)
            .expect("test manifest should build");
        let path = std::env::temp_dir().join(format!(
            "sigil-tui-rollout-test-{}.json",
            std::process::id()
        ));
        let bytes = serde_json::to_vec(&manifest).expect("manifest should serialize");
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, &bytes).expect("manifest temp write should succeed");
        std::fs::rename(&temporary, &path).expect("manifest rename should succeed");
        unsafe {
            std::env::set_var("SIGIL_ORCHESTRATION_ROLLOUT_MANIFEST", &path);
        }
    });
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
    spawn_test_worker_with_role_provider_builder(
        root_config,
        session_log_path,
        agent,
        workspace_root,
        Arc::new(RuntimeTaskRoleProviderBuilder),
    )
}

pub(super) fn spawn_test_worker_with_role_provider_builder<P>(
    root_config: RootConfig,
    session_log_path: PathBuf,
    agent: Agent<P>,
    workspace_root: PathBuf,
    role_provider_builder: Arc<dyn TaskRoleProviderBuilder>,
) -> Result<TestWorker>
where
    P: Provider + Send + Sync + 'static,
{
    install_qualified_rollout_manifest();
    let (event_tx, event_rx) = mpsc::channel();
    let (urgent_tx, urgent_rx) = mpsc::channel();
    let command_tx = WorkerCommandSender::new(event_tx.clone(), urgent_tx);
    let (message_tx, message_rx) = mpsc::channel();
    let options = sigil_runtime::build_run_options(
        &root_config,
        workspace_root.clone(),
        sigil_kernel::InteractionMode::Interactive,
        None,
    );
    let agent = Arc::new(agent);
    let elicitation_handler = Arc::new(ChannelMcpElicitationHandler::new(message_tx.clone()));
    let mcp_event_handler = Arc::new(ChannelMcpRuntimeEventHandler::new(
        WorkerMcpRuntimeEventSender::new(event_tx.clone()),
    ));
    let terminal_lifecycle_router = ChannelTerminalLifecycleRouter::new(event_tx.clone());
    let handle = thread::Builder::new()
        .name("sigil-test-agent-worker".to_owned())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("test runtime should build");
            let context_resolver =
                sigil_runtime::RequestContextResolver::request_local(workspace_root.clone());
            let attachment_lease = sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease::acquire(
                &session_log_path,
            )
            .expect("test worker should acquire its session attachment");
            run_worker_loop(
                runtime,
                agent,
                root_config,
                workspace_root.join("sigil.toml"),
                workspace_root,
                WorkerLoopSessionAttachment::new(session_log_path, attachment_lease),
                options,
                std::sync::Arc::new(sigil_kernel::PermissionModeOverride::new()),
                (event_tx, event_rx, urgent_rx),
                message_tx,
                WorkerLoopMcpHandlers {
                    elicitation_handler,
                    event_handler: mcp_event_handler,
                    role_provider_builder,
                    context_resolver,
                },
                WorkerLoopTerminalRuntime::new(terminal_lifecycle_router, None),
                None,
                None,
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
    for _ in 0..200 {
        let entries = sigil_kernel::JsonlSessionStore::read_entries(session_log_path)?;
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
    GatedChunks {
        gate: Arc<tokio::sync::Notify>,
        chunks: Vec<ProviderChunk>,
    },
    Pending,
    Fail(&'static str),
}

#[derive(Clone)]
pub(super) struct PlannedProvider {
    plans: Arc<Mutex<VecDeque<StreamPlan>>>,
    stream_started: Option<mpsc::Sender<()>>,
}

impl PlannedProvider {
    pub(super) fn new(plans: Vec<StreamPlan>) -> Self {
        Self {
            plans: Arc::new(Mutex::new(VecDeque::from(plans))),
            stream_started: None,
        }
    }

    pub(super) fn new_with_stream_start_signal(
        plans: Vec<StreamPlan>,
    ) -> (Self, mpsc::Receiver<()>) {
        let (stream_started_tx, stream_started_rx) = mpsc::channel();
        (
            Self {
                plans: Arc::new(Mutex::new(VecDeque::from(plans))),
                stream_started: Some(stream_started_tx),
            },
            stream_started_rx,
        )
    }
}

pub(super) fn planned_role_provider_builder(
    plans: Vec<StreamPlan>,
) -> Arc<dyn TaskRoleProviderBuilder> {
    Arc::new(PlannedRoleProviderBuilder {
        plans: Arc::new(Mutex::new(VecDeque::from(plans))),
        stream_started: None,
    })
}

pub(super) fn failing_role_provider_builder() -> Arc<dyn TaskRoleProviderBuilder> {
    Arc::new(FailingRoleProviderBuilder)
}

struct FailingRoleProviderBuilder;

#[async_trait]
impl TaskRoleProviderBuilder for FailingRoleProviderBuilder {
    async fn build(
        &self,
        _root_config: &RootConfig,
        _role: sigil_kernel::AgentRole,
    ) -> Result<Box<dyn Provider>> {
        Err(anyhow!("injected task role provider construction failure"))
    }
}

pub(super) fn planned_role_provider_builder_with_stream_start_signal(
    plans: Vec<StreamPlan>,
) -> (Arc<dyn TaskRoleProviderBuilder>, mpsc::Receiver<()>) {
    let (stream_started_tx, stream_started_rx) = mpsc::channel();
    (
        Arc::new(PlannedRoleProviderBuilder {
            plans: Arc::new(Mutex::new(VecDeque::from(plans))),
            stream_started: Some(stream_started_tx),
        }),
        stream_started_rx,
    )
}

struct PlannedRoleProviderBuilder {
    plans: Arc<Mutex<VecDeque<StreamPlan>>>,
    stream_started: Option<mpsc::Sender<()>>,
}

#[async_trait]
impl TaskRoleProviderBuilder for PlannedRoleProviderBuilder {
    async fn build(
        &self,
        _root_config: &RootConfig,
        _role: sigil_kernel::AgentRole,
    ) -> Result<Box<dyn Provider>> {
        Ok(Box::new(PlannedProvider {
            plans: Arc::clone(&self.plans),
            stream_started: self.stream_started.clone(),
        }))
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
            reasoning_stream: ReasoningStreamSupport::Native,
            supports_reasoning_effort: true,
            supports_tool_stream: true,
            supports_background_tasks: false,
            supports_response_handles: false,
            supports_reasoning_artifacts: false,
            supports_structured_output: false,
            supports_assistant_prefix_seed: false,
            supports_schema_constrained_tools: false,
            supports_agent_background_resume: false,
            supports_agent_thread_usage: false,
            supports_agent_result_replay: false,
            supports_infill_completion: false,
            supports_system_fingerprint: false,
            tool_name_max_chars: 64,
        }
    }

    async fn stream(
        &self,
        _request: sigil_kernel::CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let plan = self
            .plans
            .lock()
            .expect("plans mutex should not be poisoned")
            .pop_front()
            .unwrap_or(StreamPlan::Pending);
        if let Some(stream_started) = &self.stream_started {
            let _ = stream_started.send(());
        }
        let stream: Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>> = match plan {
            StreamPlan::Chunks(chunks) => {
                Box::pin(stream::iter(chunks.into_iter().map(Ok::<_, anyhow::Error>)))
            }
            StreamPlan::GatedChunks { gate, chunks } => Box::pin(
                stream::once(async move {
                    gate.notified().await;
                    chunks
                })
                .flat_map(|chunks| stream::iter(chunks.into_iter().map(Ok::<_, anyhow::Error>))),
            ),
            StreamPlan::Pending => Box::pin(stream::pending()),
            StreamPlan::Fail(error) => return Err(anyhow!(error.to_owned())),
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
            reasoning_stream: ReasoningStreamSupport::Native,
            supports_reasoning_effort: true,
            supports_tool_stream: true,
            supports_background_tasks: false,
            supports_response_handles: false,
            supports_reasoning_artifacts: false,
            supports_structured_output: false,
            supports_assistant_prefix_seed: false,
            supports_schema_constrained_tools: false,
            supports_agent_background_resume: false,
            supports_agent_thread_usage: false,
            supports_agent_result_replay: false,
            supports_infill_completion: false,
            supports_system_fingerprint: false,
            tool_name_max_chars: 64,
        }
    }

    async fn stream(
        &self,
        request: sigil_kernel::CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let tool_used = request
            .messages
            .iter()
            .any(|message| matches!(message.role, sigil_kernel::MessageRole::Tool));
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
            network_effect: None,
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
