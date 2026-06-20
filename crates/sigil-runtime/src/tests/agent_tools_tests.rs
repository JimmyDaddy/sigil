use std::{collections::BTreeMap, path::PathBuf, pin::Pin, sync::Arc};

use anyhow::Result;
use async_trait::async_trait;
use futures::{Stream, stream};
use serde_json::json;
use sigil_kernel::{
    Agent, AgentConfig, AgentRunInput, AgentRunOptions, AgentThreadStatus, AgentToolDelegate,
    AutoApproveHandler, CompactionConfig, CompletionRequest, ControlEntry, EventHandler,
    InteractionMode, MemoryConfig, MessageRole, PermissionConfig, Provider, ProviderCapabilities,
    ProviderChunk, ReasoningEffort, ReasoningStreamSupport, RootConfig, RunEvent, Session,
    SessionConfig, ToolCall, ToolRegistry, UsageStats, WorkspaceConfig,
};

use super::{
    AgentBudgetPolicy, AgentProfileRegistry, AgentSupervisor, AgentToolProviderFactory,
    AgentToolRuntime, CLOSE_AGENT_TOOL_NAME, MESSAGE_AGENT_TOOL_NAME, SPAWN_AGENT_TOOL_NAME,
    WAIT_AGENT_TOOL_NAME, chat_agent_thread_id_for_call, register_agent_tools,
};

#[derive(Default)]
struct RecordingEventHandler {
    events: Vec<RunEvent>,
}

impl EventHandler for RecordingEventHandler {
    fn handle(&mut self, event: RunEvent) -> Result<()> {
        self.events.push(event);
        Ok(())
    }
}

struct ChildTextProvider {
    text: String,
}

#[async_trait]
impl Provider for ChildTextProvider {
    fn name(&self) -> &str {
        "child-text"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        provider_capabilities()
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::Usage(UsageStats {
                prompt_tokens: 3,
                completion_tokens: 2,
                ..UsageStats::default()
            })),
            Ok(ProviderChunk::TextDelta(self.text.clone())),
            Ok(ProviderChunk::Done),
        ])))
    }
}

struct ParentSpawnProvider;

#[async_trait]
impl Provider for ParentSpawnProvider {
    fn name(&self) -> &str {
        "parent-spawn"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        provider_capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let tool_result_seen = request
            .messages
            .iter()
            .any(|message| matches!(message.role, MessageRole::Tool));
        if tool_result_seen {
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::TextDelta(
                    "parent final includes child summary".to_owned(),
                )),
                Ok(ProviderChunk::Done),
            ])));
        }
        let args = json!({
            "profile_id": "explore",
            "objective": "inspect runtime",
            "prompt": "summarize runtime",
            "mode": "join_before_final",
            "display_name_hint": "runtime review"
        })
        .to_string();
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::ToolCallComplete(ToolCall {
                id: "call-spawn-1".to_owned(),
                name: SPAWN_AGENT_TOOL_NAME.to_owned(),
                args_json: args,
            })),
            Ok(ProviderChunk::Done),
        ])))
    }
}

struct StaticProviderFactory;

impl AgentToolProviderFactory for StaticProviderFactory {
    fn build_provider(
        &self,
        _root_config: &RootConfig,
        _role: sigil_kernel::AgentRole,
        _profile_id: &sigil_kernel::AgentProfileId,
    ) -> Result<Box<dyn Provider>> {
        Ok(Box::new(ChildTextProvider {
            text: "child summary only".to_owned(),
        }))
    }
}

struct RejectingProviderFactory;

impl AgentToolProviderFactory for RejectingProviderFactory {
    fn build_provider(
        &self,
        _root_config: &RootConfig,
        _role: sigil_kernel::AgentRole,
        _profile_id: &sigil_kernel::AgentProfileId,
    ) -> Result<Box<dyn Provider>> {
        anyhow::bail!("provider factory should not be called for rejected profiles")
    }
}

struct TextProviderFactory {
    text: String,
}

impl AgentToolProviderFactory for TextProviderFactory {
    fn build_provider(
        &self,
        _root_config: &RootConfig,
        _role: sigil_kernel::AgentRole,
        _profile_id: &sigil_kernel::AgentProfileId,
    ) -> Result<Box<dyn Provider>> {
        Ok(Box::new(ChildTextProvider {
            text: self.text.clone(),
        }))
    }
}

#[test]
fn spawn_agent_tool_schema_uses_stable_profile_id() -> Result<()> {
    let config = root_config();
    let mut registry = ToolRegistry::new();
    register_agent_tools(&mut registry, &config)?;

    let spec = registry
        .spec_for(SPAWN_AGENT_TOOL_NAME)
        .expect("spawn_agent registered");
    assert!(spec.description.contains("explore"));
    assert!(!spec.description.contains("worker:"));
    assert!(spec.input_schema["properties"].get("profile_id").is_some());
    assert!(
        spec.input_schema["required"]
            .as_array()
            .is_some_and(|required| required.iter().any(|value| value == "profile_id"))
    );
    assert!(
        spec.input_schema["properties"]
            .get("display_name_hint")
            .is_some()
    );
    let modes = spec.input_schema["properties"]["mode"]["enum"]
        .as_array()
        .expect("mode enum");
    assert!(!modes.iter().any(|mode| mode == "background"));
    assert!(registry.spec_for(MESSAGE_AGENT_TOOL_NAME).is_none());
    Ok(())
}

#[tokio::test]
async fn spawn_agent_preview_contains_source_trust_mode_scope_budget() -> Result<()> {
    let config = root_config();
    let mut registry = ToolRegistry::new();
    register_agent_tools(&mut registry, &config)?;
    let preview = registry
        .preview(
            sigil_kernel::ToolContext {
                workspace_root: std::env::temp_dir(),
                timeout_secs: 30,
            },
            ToolCall {
                id: "call-preview".to_owned(),
                name: SPAWN_AGENT_TOOL_NAME.to_owned(),
                args_json: json!({
                    "profile_id": "explore",
                    "objective": "inspect",
                    "prompt": "inspect",
                    "mode": "join_before_final"
                })
                .to_string(),
            },
        )
        .await?
        .expect("spawn preview");

    assert!(preview.body.contains("source:"));
    assert!(preview.body.contains("trust:"));
    assert!(preview.body.contains("mode: join_before_final"));
    assert!(preview.body.contains("objective: inspect"));
    assert!(preview.body.contains("tool_scope:"));
    assert!(preview.body.contains("budget:"));
    Ok(())
}

#[tokio::test]
async fn ordinary_chat_explicit_subagent_prompt_spawns_child() -> Result<()> {
    let config = root_config();
    let mut registry = ToolRegistry::new();
    register_agent_tools(&mut registry, &config)?;
    let supervisor = supervisor(&config)?;
    let mut agent_delegate = AgentToolRuntime::with_provider_factory(
        supervisor,
        config.clone(),
        registry.clone(),
        Arc::new(StaticProviderFactory),
    );
    let agent = Agent::new(ParentSpawnProvider, registry);
    let mut session = Session::new("parent-spawn", "mock-model");
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;

    let output = agent
        .run_with_approval_input_and_agent_delegate(
            &mut session,
            AgentRunInput::user("use a sub agent to inspect runtime"),
            run_options(std::env::temp_dir()),
            &mut handler,
            &mut approval,
            &mut agent_delegate,
        )
        .await?;

    assert_eq!(
        output.result.final_text,
        "parent final includes child summary"
    );
    let projection = session.agent_thread_state_projection();
    let thread = projection.latest_thread().expect("child agent projected");
    assert_eq!(thread.status, AgentThreadStatus::Completed);
    assert!(
        thread
            .result
            .as_ref()
            .is_some_and(|result| result.summary == "child summary only")
    );
    assert!(session.messages().iter().any(|message| {
        matches!(message.role, MessageRole::Tool)
            && message.tool_call_id.as_deref() == Some("call-spawn-1")
            && message
                .content
                .as_deref()
                .is_some_and(|content| content.contains("child summary only"))
    }));
    Ok(())
}

#[tokio::test]
async fn wait_and_close_agent_use_bounded_thread_projection() -> Result<()> {
    let (mut runtime, mut session, thread_id) = spawned_runtime_session().await?;
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;
    let options = run_options(std::env::temp_dir());

    let wait = runtime
        .handle_agent_tool_call(
            &mut session,
            &ToolCall {
                id: "call-wait".to_owned(),
                name: WAIT_AGENT_TOOL_NAME.to_owned(),
                args_json: json!({ "thread_id": thread_id.as_str() }).to_string(),
            },
            &options,
            &mut handler,
            &mut approval,
        )
        .await?
        .expect("wait_agent handled");
    assert!(wait.content.contains("child summary only"));
    assert!(!wait.content.contains("system:base"));

    let close = runtime
        .handle_agent_tool_call(
            &mut session,
            &ToolCall {
                id: "call-close".to_owned(),
                name: CLOSE_AGENT_TOOL_NAME.to_owned(),
                args_json: json!({ "thread_id": thread_id.as_str() }).to_string(),
            },
            &options,
            &mut handler,
            &mut approval,
        )
        .await?
        .expect("close_agent handled");
    assert!(!close.is_error());
    assert!(close.control_entries.iter().any(|entry| {
        matches!(entry, ControlEntry::AgentThreadClosed(close) if close.thread_id == thread_id)
    }));
    Ok(())
}

#[tokio::test]
async fn spawn_agent_rejects_background_mode_without_starting_thread() -> Result<()> {
    let config = root_config();
    let mut registry = ToolRegistry::new();
    register_agent_tools(&mut registry, &config)?;
    let supervisor = supervisor(&config)?;
    let mut runtime = AgentToolRuntime::with_provider_factory(
        supervisor,
        config,
        registry,
        Arc::new(StaticProviderFactory),
    );
    let mut session = Session::new("parent", "model");
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;

    let result = runtime
        .handle_agent_tool_call(
            &mut session,
            &ToolCall {
                id: "call-background".to_owned(),
                name: SPAWN_AGENT_TOOL_NAME.to_owned(),
                args_json: json!({
                    "profile_id": "explore",
                    "objective": "inspect",
                    "prompt": "inspect",
                    "mode": "background"
                })
                .to_string(),
            },
            &run_options(std::env::temp_dir()),
            &mut handler,
            &mut approval,
        )
        .await?
        .expect("spawn handled");

    assert!(result.is_error());
    assert!(result.content.contains("background agent mode requires"));
    assert!(session.agent_thread_state_projection().threads.is_empty());
    Ok(())
}

#[tokio::test]
async fn spawn_agent_rejects_model_invisible_profile_before_building_provider() -> Result<()> {
    let config = root_config();
    let mut registry = ToolRegistry::new();
    register_agent_tools(&mut registry, &config)?;
    let supervisor = supervisor(&config)?;
    let mut runtime = AgentToolRuntime::with_provider_factory(
        supervisor,
        config,
        registry,
        Arc::new(RejectingProviderFactory),
    );
    let mut session = Session::new("parent", "model");
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;

    let result = runtime
        .handle_agent_tool_call(
            &mut session,
            &ToolCall {
                id: "call-model-invisible".to_owned(),
                name: SPAWN_AGENT_TOOL_NAME.to_owned(),
                args_json: json!({
                    "profile_id": "plan",
                    "objective": "inspect",
                    "prompt": "inspect",
                    "mode": "join_before_final"
                })
                .to_string(),
            },
            &run_options(std::env::temp_dir()),
            &mut handler,
            &mut approval,
        )
        .await?
        .expect("spawn handled");

    assert!(result.is_error());
    assert!(result.content.contains("not model-invocable"));
    assert!(session.agent_thread_state_projection().threads.is_empty());
    Ok(())
}

#[tokio::test]
async fn wait_agent_applies_summary_limit_and_durable_summary_is_bounded() -> Result<()> {
    let config = root_config();
    let mut registry = ToolRegistry::new();
    register_agent_tools(&mut registry, &config)?;
    let supervisor = supervisor(&config)?;
    let mut runtime = AgentToolRuntime::with_provider_factory(
        supervisor,
        config,
        registry.clone(),
        Arc::new(TextProviderFactory {
            text: "x".repeat(5_001),
        }),
    );
    let mut session = Session::new("parent", "model");
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;
    let call = ToolCall {
        id: "call-long".to_owned(),
        name: SPAWN_AGENT_TOOL_NAME.to_owned(),
        args_json: json!({
            "profile_id": "explore",
            "objective": "inspect",
            "prompt": "inspect",
            "mode": "join_before_final"
        })
        .to_string(),
    };
    let _ = runtime
        .handle_agent_tool_call(
            &mut session,
            &call,
            &run_options(std::env::temp_dir()),
            &mut handler,
            &mut approval,
        )
        .await?
        .expect("spawn handled");
    let thread_id =
        chat_agent_thread_id_for_call(&call.id, &sigil_kernel::AgentProfileId::new("explore")?)?;
    let projection = session.agent_thread_state_projection();
    let result = projection
        .threads
        .get(&thread_id)
        .and_then(|thread| thread.result.as_ref())
        .expect("thread result");
    assert_eq!(result.summary.chars().count(), 4_000);
    assert!(result.summary_truncated);
    assert_eq!(result.original_summary_chars, Some(5_001));

    let wait = runtime
        .handle_agent_tool_call(
            &mut session,
            &ToolCall {
                id: "call-wait-long".to_owned(),
                name: WAIT_AGENT_TOOL_NAME.to_owned(),
                args_json: json!({
                    "thread_id": thread_id.as_str(),
                    "max_summary_chars": 200
                })
                .to_string(),
            },
            &run_options(std::env::temp_dir()),
            &mut handler,
            &mut approval,
        )
        .await?
        .expect("wait handled");
    let payload: serde_json::Value = serde_json::from_str(&wait.content)?;
    assert_eq!(
        payload["summary"]
            .as_str()
            .expect("summary")
            .chars()
            .count(),
        200
    );
    assert_eq!(payload["summary_truncated"], true);
    assert_eq!(payload["original_summary_chars"], 5_001);
    Ok(())
}

#[tokio::test]
async fn spawn_agent_enforces_max_fanout() -> Result<()> {
    let config = root_config();
    let mut registry = ToolRegistry::new();
    register_agent_tools(&mut registry, &config)?;
    let mut budget = AgentBudgetPolicy::from_root_config(&config);
    budget.max_spawn_fanout_per_turn = 0;
    let supervisor = AgentSupervisor::new(
        AgentProfileRegistry::from_root_config(&config)?,
        budget,
        provider_capabilities(),
    );
    let mut runtime = AgentToolRuntime::with_provider_factory(
        supervisor,
        config,
        registry,
        Arc::new(StaticProviderFactory),
    );
    let mut session = Session::new("parent", "model");
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;
    let result = runtime
        .handle_agent_tool_call(
            &mut session,
            &ToolCall {
                id: "call-fanout".to_owned(),
                name: SPAWN_AGENT_TOOL_NAME.to_owned(),
                args_json: json!({
                    "profile_id": "explore",
                    "objective": "inspect",
                    "prompt": "inspect",
                    "mode": "join_before_final"
                })
                .to_string(),
            },
            &run_options(std::env::temp_dir()),
            &mut handler,
            &mut approval,
        )
        .await?
        .expect("spawn handled");

    assert!(result.is_error());
    let thread_id = chat_agent_thread_id_for_call(
        "call-fanout",
        &sigil_kernel::AgentProfileId::new("explore")?,
    )?;
    let projection = session.agent_thread_state_projection();
    let thread = projection
        .threads
        .get(&thread_id)
        .expect("failed thread projected");
    assert_eq!(thread.status, AgentThreadStatus::Failed);
    assert!(
        thread
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("fan-out budget"))
    );
    Ok(())
}

async fn spawned_runtime_session()
-> Result<(AgentToolRuntime, Session, sigil_kernel::AgentThreadId)> {
    let config = root_config();
    let mut registry = ToolRegistry::new();
    register_agent_tools(&mut registry, &config)?;
    let supervisor = supervisor(&config)?;
    let mut runtime = AgentToolRuntime::with_provider_factory(
        supervisor,
        config,
        registry.clone(),
        Arc::new(StaticProviderFactory),
    );
    let mut session = Session::new("parent", "model");
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;
    let call = ToolCall {
        id: "call-spawn-direct".to_owned(),
        name: SPAWN_AGENT_TOOL_NAME.to_owned(),
        args_json: json!({
            "profile_id": "explore",
            "objective": "inspect",
            "prompt": "inspect",
            "mode": "join_before_final"
        })
        .to_string(),
    };
    let _ = runtime
        .handle_agent_tool_call(
            &mut session,
            &call,
            &run_options(std::env::temp_dir()),
            &mut handler,
            &mut approval,
        )
        .await?
        .expect("spawn handled");
    let thread_id =
        chat_agent_thread_id_for_call(&call.id, &sigil_kernel::AgentProfileId::new("explore")?)?;
    Ok((runtime, session, thread_id))
}

fn supervisor(config: &RootConfig) -> Result<AgentSupervisor> {
    Ok(AgentSupervisor::new(
        AgentProfileRegistry::from_root_config(config)?,
        AgentBudgetPolicy::from_root_config(config),
        provider_capabilities(),
    ))
}

fn root_config() -> RootConfig {
    RootConfig {
        workspace: WorkspaceConfig {
            root: ".".to_owned(),
        },
        session: SessionConfig {
            log_dir: ".sigil/sessions".to_owned(),
        },
        agent: AgentConfig {
            provider: "deepseek".to_owned(),
            model: "deepseek-v4-flash".to_owned(),
            max_turns: Some(4),
            tool_timeout_secs: 30,
        },
        permission: PermissionConfig::default(),
        memory: MemoryConfig { enabled: false },
        skills: Default::default(),
        compaction: CompactionConfig::default(),
        code_intelligence: sigil_kernel::CodeIntelligenceConfig::default(),
        terminal: Default::default(),
        task: Default::default(),
        providers: BTreeMap::from([(
            "deepseek".to_owned(),
            json!({
                "base_url": "https://example.com",
                "model": "deepseek-v4-flash",
            }),
        )]),
        mcp_servers: Vec::new(),
    }
}

fn run_options(workspace_root: PathBuf) -> AgentRunOptions {
    AgentRunOptions {
        workspace_root,
        max_turns: Some(4),
        tool_timeout_secs: 30,
        reasoning_effort: Some(ReasoningEffort::Medium),
        traffic_partition_key: None,
        interaction_mode: InteractionMode::Interactive,
        permission_config: PermissionConfig::default(),
        memory_config: MemoryConfig { enabled: false },
        compaction_config: CompactionConfig::default(),
    }
}

fn provider_capabilities() -> ProviderCapabilities {
    ProviderCapabilities {
        exact_prefix_cache: true,
        reports_cache_tokens: true,
        reasoning_stream: ReasoningStreamSupport::Native,
        supports_reasoning_effort: true,
        supports_tool_stream: true,
        supports_background_tasks: false,
        supports_response_handles: false,
        supports_reasoning_artifacts: false,
        supports_structured_output: true,
        supports_assistant_prefix_seed: false,
        supports_schema_constrained_tools: true,
        supports_agent_background_resume: false,
        supports_agent_thread_usage: false,
        supports_agent_result_replay: false,
        supports_infill_completion: false,
        supports_system_fingerprint: true,
        tool_name_max_chars: 64,
    }
}
