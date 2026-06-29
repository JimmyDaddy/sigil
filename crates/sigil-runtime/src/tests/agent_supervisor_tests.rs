use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
    pin::Pin,
    sync::Arc,
};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use futures::{Stream, stream};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    Agent, AgentConfig, AgentFinalAnswerRef, AgentInvocationMode, AgentInvocationSource, AgentRole,
    AgentRouteId, AgentRunInput, AgentRunOptions, AgentRunOutcome, AgentRunTerminalReason,
    AgentThreadId, AgentThreadTerminalStatus, AgentUsageSummary, ApprovalMode, AutoApproveHandler,
    CompactionConfig, CompletionRequest, EventHandler, InteractionMode, JsonlSessionStore,
    MemoryConfig, ModelMessage, PermissionConfig, Provider, ProviderCapabilities, ProviderChunk,
    ReasoningStreamSupport, RootConfig, RunEvent, Session, SessionConfig, SessionLogEntry,
    SessionRef, TaskChildSessionRunRequest, TaskChildSessionRunner, TaskChildSessionStatus, TaskId,
    TaskRouteStatus, TaskStepId, TaskStepSpec, TaskSubagentApprovalRouteEntry, Tool, ToolAccess,
    ToolCall, ToolCategory, ToolContext, ToolError, ToolErrorKind, ToolPreviewCapability,
    ToolRegistry, ToolRegistryScope, ToolResult, ToolResultMeta, ToolSpec, UsageStats,
    WorkspaceConfig, child_session_ref,
};

use super::{
    AgentBudgetPolicy, AgentChatChildStart, AgentMailboxMessage, AgentProfileRegistry,
    AgentSupervisor, AgentSupervisorTaskChildRunner, AgentTaskChildStart, EXPLORE_PROFILE_ID,
    agent_terminal_status_from_task_child, task_child_status_from_outcome,
    tool_scope_is_write_capable,
};
use crate::AgentToolRuntime;

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

struct TextProvider {
    text: &'static str,
}

#[async_trait]
impl Provider for TextProvider {
    fn name(&self) -> &str {
        "text"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        provider_capabilities()
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::TextDelta(self.text.to_owned())),
            Ok(ProviderChunk::Done),
        ])))
    }
}

struct FailingProvider;

#[async_trait]
impl Provider for FailingProvider {
    fn name(&self) -> &str {
        "failing"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        provider_capabilities()
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        Err(anyhow!("child provider failed"))
    }
}

struct UsageProvider;
struct ToolCallingChildProvider;
struct ApprovalRouteTool;

#[async_trait]
impl Provider for UsageProvider {
    fn name(&self) -> &str {
        "usage"
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
                prompt_tokens: 8,
                completion_tokens: 5,
                ..UsageStats::default()
            })),
            Ok(ProviderChunk::TextDelta("too expensive".to_owned())),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[async_trait]
impl Provider for ToolCallingChildProvider {
    fn name(&self) -> &str {
        "tool-calling-child"
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
            .any(|message| matches!(message.role, sigil_kernel::MessageRole::Tool));
        if tool_result_seen {
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::TextDelta("tool route done".to_owned())),
                Ok(ProviderChunk::Done),
            ])));
        }

        let args = r#"{"path":"README.md"}"#;
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::ToolCallStart {
                id: "call-read-1".to_owned(),
                name: "read_file".to_owned(),
            }),
            Ok(ProviderChunk::ToolCallArgsDelta {
                id: "call-read-1".to_owned(),
                delta: args.to_owned(),
            }),
            Ok(ProviderChunk::ToolCallComplete(ToolCall {
                id: "call-read-1".to_owned(),
                name: "read_file".to_owned(),
                args_json: args.to_owned(),
            })),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[async_trait]
impl Tool for ApprovalRouteTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "read_file".to_owned(),
            description: "Read one file for approval route tests.".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" }
                },
                "required": ["path"]
            }),
            category: ToolCategory::File,
            access: ToolAccess::Read,
            preview: ToolPreviewCapability::None,
        }
    }

    fn permission_default_mode(
        &self,
        _ctx: &ToolContext,
        _args: &Value,
    ) -> Result<Option<ApprovalMode>> {
        Ok(Some(ApprovalMode::Ask))
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        call_id: String,
        _args: Value,
    ) -> Result<ToolResult> {
        Ok(ToolResult::ok(
            call_id,
            "read_file",
            "read contents",
            ToolResultMeta::default(),
        ))
    }
}

struct ResultReplayProvider;

#[async_trait]
impl Provider for ResultReplayProvider {
    fn name(&self) -> &str {
        "result-replay"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        let mut capabilities = provider_capabilities();
        capabilities.supports_agent_result_replay = true;
        capabilities
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::TextDelta("writer inspected".to_owned())),
            Ok(ProviderChunk::Done),
        ])))
    }
}

fn root_config() -> RootConfig {
    RootConfig {
        workspace: WorkspaceConfig {
            root: ".".to_owned(),
        },
        storage: Default::default(),
        session: SessionConfig {
            log_dir: Some(".sigil/sessions".to_owned()),
        },
        agent: AgentConfig {
            provider: "deepseek".to_owned(),
            model: "deepseek-v4-flash".to_owned(),
            max_turns: Some(12),
            tool_timeout_secs: 45,
        },
        permission: PermissionConfig::default(),
        memory: MemoryConfig { enabled: true },
        skills: Default::default(),
        compaction: CompactionConfig::default(),
        code_intelligence: sigil_kernel::CodeIntelligenceConfig::default(),
        terminal: Default::default(),
        execution: Default::default(),
        verification: Default::default(),
        appearance: Default::default(),
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

fn provider_capability_hash(capabilities: &ProviderCapabilities) -> Result<String> {
    let bytes = serde_json::to_vec(&serde_json::to_value(capabilities)?)?;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

fn run_options(workspace_root: PathBuf) -> AgentRunOptions {
    AgentRunOptions {
        workspace_root,
        max_turns: Some(4),
        tool_timeout_secs: 30,
        reasoning_effort: None,
        traffic_partition_key: None,
        interaction_mode: InteractionMode::Interactive,
        permission_config: PermissionConfig::default(),
        permission_context: sigil_kernel::PermissionEvaluationContext::default(),
        memory_config: MemoryConfig { enabled: false },
        compaction_config: CompactionConfig::default(),
    }
}

fn step(id: &str) -> Result<TaskStepSpec> {
    Ok(TaskStepSpec {
        step_id: TaskStepId::new(id)?,
        title: format!("run {id}"),
        display_name: Some(id.to_owned()),
        detail: Some("test child step".to_owned()),
        role: AgentRole::SubagentRead,
        depends_on: Vec::new(),
        mode: None,
        isolation: None,
    })
}

fn write_step(id: &str) -> Result<TaskStepSpec> {
    Ok(TaskStepSpec {
        step_id: TaskStepId::new(id)?,
        title: format!("write {id}"),
        display_name: Some(id.to_owned()),
        detail: Some("test write child step".to_owned()),
        role: AgentRole::SubagentWrite,
        depends_on: Vec::new(),
        mode: None,
        isolation: None,
    })
}

fn supervisor_with_budget(budget: AgentBudgetPolicy) -> Result<AgentSupervisor> {
    Ok(AgentSupervisor::new(
        AgentProfileRegistry::from_root_config(&root_config())?,
        budget,
        provider_capabilities(),
    ))
}

fn agent_route_statuses(session: &Session) -> Vec<sigil_kernel::AgentRouteStatus> {
    session
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::Control(sigil_kernel::ControlEntry::AgentApprovalRoute(route)) => {
                Some(route.status)
            }
            _ => None,
        })
        .collect()
}

fn task_route_statuses(session: &Session) -> Vec<TaskRouteStatus> {
    session
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::Control(sigil_kernel::ControlEntry::TaskSubagentApprovalRoute(
                TaskSubagentApprovalRouteEntry { status, .. },
            )) => Some(*status),
            _ => None,
        })
        .collect()
}

fn child_start(step: TaskStepSpec, workspace_root: PathBuf) -> Result<AgentTaskChildStart> {
    let task_id = TaskId::new("task_1")?;
    let child_task_id = TaskId::new(format!("child_v1_{}", step.step_id.as_str()))?;
    let child_session_ref = child_session_ref(&task_id, &step.step_id, &child_task_id)?;
    Ok(AgentTaskChildStart {
        task_id,
        parent_thread_id: AgentThreadId::new("main")?,
        parent_depth: 0,
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        plan_version: 1,
        step,
        child_task_id,
        child_session_ref,
        child_input: AgentRunInput::without_persisted_user_message(vec![ModelMessage::user(
            "inspect code",
        )]),
        objective: "inspect code".to_owned(),
        workspace_root,
        provider_capabilities: provider_capabilities(),
        role: AgentRole::SubagentRead,
        invocation_mode: AgentInvocationMode::Foreground,
        invocation_source: AgentInvocationSource::Task,
    })
}

fn chat_child_start(profile_id: &str, workspace_root: PathBuf) -> Result<AgentChatChildStart> {
    Ok(AgentChatChildStart {
        call_id: format!("call_{profile_id}"),
        budget_scope_id: TaskId::new("chat_1")?,
        parent_thread_id: AgentThreadId::new("main")?,
        parent_depth: 0,
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        profile_id: sigil_kernel::AgentProfileId::new(profile_id)?,
        role: AgentRole::SubagentRead,
        child_session_ref: SessionRef::new_relative(format!("children/{profile_id}.jsonl"))?,
        objective: "inspect code".to_owned(),
        prompt: "inspect code".to_owned(),
        workspace_root,
        provider_capabilities: provider_capabilities(),
        invocation_mode: AgentInvocationMode::JoinBeforeFinal,
        invocation_source: AgentInvocationSource::Chat,
        display_name_hint: Some("inspect".to_owned()),
    })
}

#[test]
fn supervisor_captures_profile_snapshot_before_spawn() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut config = root_config();
    config.task.allow_write_subagents = false;
    let supervisor = AgentSupervisor::new(
        AgentProfileRegistry::from_root_config(&config)?,
        AgentBudgetPolicy::from_root_config(&config),
        provider_capabilities(),
    );
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let thread = supervisor.begin_task_child_thread(
        &mut session,
        &mut handler,
        child_start(step("inspect")?, temp.path().to_path_buf())?,
    )?;

    let projection = session.agent_thread_state_projection();
    let projected = projection
        .threads
        .get(&thread.thread_id)
        .expect("thread was projected");
    assert_eq!(
        projected.profile_id.as_ref().map(|id| id.as_str()),
        Some(EXPLORE_PROFILE_ID)
    );
    assert!(!projection.profiles.is_empty());
    assert!(projected.run_context.is_some());
    assert!(handler.events.iter().any(|event| {
        matches!(
            event,
            RunEvent::Control(sigil_kernel::ControlEntry::AgentProfileCaptured(_))
        )
    }));
    Ok(())
}

#[test]
fn chat_child_start_rejects_invalid_disabled_and_model_invisible_profiles() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let supervisor = supervisor_with_budget(AgentBudgetPolicy::from_root_config(&root_config()))?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();

    let error = supervisor
        .begin_chat_child_thread(
            &mut session,
            &mut handler,
            chat_child_start("missing", temp.path().to_path_buf())?,
        )
        .expect_err("missing profile rejected");
    assert!(error.to_string().contains("not registered"));

    let error = supervisor
        .begin_chat_child_thread(
            &mut session,
            &mut handler,
            chat_child_start("plan", temp.path().to_path_buf())?,
        )
        .expect_err("model-invisible profile rejected");
    assert!(error.to_string().contains("not model-invocable"));

    let mut disabled_config = root_config();
    disabled_config.task.enabled = false;
    let disabled_supervisor = AgentSupervisor::new(
        AgentProfileRegistry::from_root_config(&disabled_config)?,
        AgentBudgetPolicy::from_root_config(&disabled_config),
        provider_capabilities(),
    );
    let error = disabled_supervisor
        .begin_chat_child_thread(
            &mut session,
            &mut handler,
            chat_child_start(EXPLORE_PROFILE_ID, temp.path().to_path_buf())?,
        )
        .expect_err("disabled profile rejected");
    assert!(error.to_string().contains("is disabled"));
    Ok(())
}

#[test]
fn chat_child_start_rejects_mention_when_profile_is_not_user_invocable() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let agent_dir = workspace.join(".sigil").join("agents").join("model-only");
    fs::create_dir_all(&agent_dir)?;
    fs::write(
        agent_dir.join("agent.toml"),
        r#"
description = "Model-only helper."
instructions = "Only the model may invoke this profile."
trust = "trusted"
invocation_policy = "model_allowed"
user_invocable = false
model_invocable = true
"#,
    )?;

    let registry =
        AgentProfileRegistry::from_root_config_with_workspace(&root_config(), &workspace)?;
    let supervisor = AgentSupervisor::new(
        registry,
        AgentBudgetPolicy::from_root_config(&root_config()),
        provider_capabilities(),
    );
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let mut start = chat_child_start("model-only", workspace)?;
    start.invocation_source = AgentInvocationSource::Mention;

    let error = supervisor
        .begin_chat_child_thread(&mut session, &mut handler, start)
        .expect_err("manual mention rejects non-user-invocable profile");

    assert!(error.to_string().contains("not user-invocable"));
    Ok(())
}

#[test]
fn chat_child_start_rejects_write_capable_profile_without_lease_support() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut config = root_config();
    config.task.subagent_read.tools = sigil_kernel::ToolAllowlistConfig {
        allow_all: false,
        names: vec!["write_file".to_owned()],
        prefixes: Vec::new(),
    };
    let supervisor = AgentSupervisor::new(
        AgentProfileRegistry::from_root_config(&config)?,
        AgentBudgetPolicy::from_root_config(&config),
        provider_capabilities(),
    );
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let mut start = chat_child_start(EXPLORE_PROFILE_ID, temp.path().to_path_buf())?;
    start.role = AgentRole::SubagentWrite;

    let error = supervisor
        .begin_chat_child_thread(&mut session, &mut handler, start)
        .expect_err("write-capable chat profile rejected");

    assert!(
        error
            .to_string()
            .contains("write-capable agent requires guarded changeset-only scope")
    );
    let projection = session.agent_thread_state_projection();
    assert!(projection.threads.values().any(|thread| {
        thread.status == sigil_kernel::AgentThreadStatus::Failed
            && thread
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains("write-capable agents require"))
    }));
    Ok(())
}

#[test]
fn record_chat_child_failure_appends_failed_status_and_releases_budget() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut budget = AgentBudgetPolicy::from_root_config(&root_config());
    budget.max_parallel_readonly = 2;
    budget.max_threads = 2;
    let supervisor = supervisor_with_budget(budget)?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let thread = supervisor.begin_chat_child_thread(
        &mut session,
        &mut handler,
        chat_child_start(EXPLORE_PROFILE_ID, temp.path().to_path_buf())?,
    )?;
    assert_eq!(supervisor.active_profile_ids().len(), 1);

    supervisor.record_chat_child_failure(
        &mut session,
        &mut handler,
        &thread,
        "child failed".to_owned(),
    )?;

    assert!(supervisor.active_profile_ids().is_empty());
    let projection = session.agent_thread_state_projection();
    let projected = projection
        .threads
        .get(&thread.thread_id)
        .expect("chat thread projected");
    assert_eq!(projected.status, sigil_kernel::AgentThreadStatus::Failed);
    assert_eq!(projected.reason.as_deref(), Some("child failed"));
    Ok(())
}

#[test]
fn record_chat_child_result_persists_final_answer_ref_and_releases_budget() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut budget = AgentBudgetPolicy::from_root_config(&root_config());
    budget.max_parallel_readonly = 2;
    budget.max_threads = 2;
    let supervisor = supervisor_with_budget(budget)?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let thread = supervisor.begin_chat_child_thread(
        &mut session,
        &mut handler,
        chat_child_start(EXPLORE_PROFILE_ID, temp.path().to_path_buf())?,
    )?;
    let final_answer_ref = AgentFinalAnswerRef {
        session_ref: thread.child_session_ref.clone(),
        message_id: "msg-child-final".to_owned(),
        content_hash: "sha256:child-final".to_owned(),
        char_count: "child done".chars().count(),
    };

    let handler_dyn: &mut (dyn EventHandler + Send) = &mut handler;
    supervisor.record_chat_child_result(
        &mut session,
        handler_dyn,
        &thread,
        TaskChildSessionStatus::Completed,
        "child done",
        &AgentRunOutcome::default(),
        None,
        Some(final_answer_ref.clone()),
    )?;

    assert!(supervisor.active_profile_ids().is_empty());
    let projection = session.agent_thread_state_projection();
    let projected = projection
        .threads
        .get(&thread.thread_id)
        .expect("chat thread projected");
    assert_eq!(projected.status, sigil_kernel::AgentThreadStatus::Completed);
    assert_eq!(
        projected
            .result
            .as_ref()
            .and_then(|result| result.final_answer_ref.as_ref()),
        Some(&final_answer_ref)
    );
    Ok(())
}

#[test]
fn send_agent_message_reports_inactive_thread_and_missing_mailbox() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut budget = AgentBudgetPolicy::from_root_config(&root_config());
    budget.max_threads = 2;
    budget.max_parallel_readonly = 2;
    budget.max_background_threads = 1;
    let supervisor = supervisor_with_budget(budget)?;

    let inactive_error = supervisor
        .send_agent_message(
            &AgentThreadId::new("missing")?,
            AgentMailboxMessage {
                route_id: AgentRouteId::new("route_missing")?,
                prompt: "follow up".to_owned(),
            },
        )
        .expect_err("inactive thread rejects mailbox message");
    assert_eq!(inactive_error, "agent thread is not active");

    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let foreground = supervisor.begin_chat_child_thread(
        &mut session,
        &mut handler,
        chat_child_start(EXPLORE_PROFILE_ID, temp.path().to_path_buf())?,
    )?;

    let missing_mailbox_error = supervisor
        .send_agent_message(
            &foreground.thread_id,
            AgentMailboxMessage {
                route_id: AgentRouteId::new("route_foreground")?,
                prompt: "follow up".to_owned(),
            },
        )
        .expect_err("foreground child has no active mailbox");
    assert_eq!(missing_mailbox_error, "agent thread has no active mailbox");

    let mut background_start = chat_child_start(EXPLORE_PROFILE_ID, temp.path().to_path_buf())?;
    background_start.call_id = "call_background_mailbox".to_owned();
    background_start.invocation_mode = AgentInvocationMode::Background;
    let mut background =
        supervisor.begin_chat_child_thread(&mut session, &mut handler, background_start)?;
    let route_id = AgentRouteId::new("route_background")?;
    supervisor
        .send_agent_message(
            &background.thread_id,
            AgentMailboxMessage {
                route_id: route_id.clone(),
                prompt: "continue".to_owned(),
            },
        )
        .map_err(|error| anyhow!(error))?;

    let received = background
        .mailbox_rx
        .as_mut()
        .expect("background child should have mailbox")
        .try_recv()
        .expect("message should be queued");
    assert_eq!(received.route_id, route_id);
    assert_eq!(received.prompt, "continue");
    Ok(())
}

#[tokio::test]
async fn route_agent_message_records_mailbox_delivery_state() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut budget = AgentBudgetPolicy::from_root_config(&root_config());
    budget.max_threads = 2;
    budget.max_parallel_readonly = 2;
    budget.max_background_threads = 1;
    let supervisor = supervisor_with_budget(budget)?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let mut background_start = chat_child_start(EXPLORE_PROFILE_ID, temp.path().to_path_buf())?;
    background_start.invocation_mode = AgentInvocationMode::Background;
    let background =
        supervisor.begin_chat_child_thread(&mut session, &mut handler, background_start)?;
    let mut runtime = AgentToolRuntime::new(supervisor, root_config(), ToolRegistry::new());

    let (_result, controls) = runtime
        .route_agent_message(
            &mut session,
            background.thread_id.clone(),
            "continue".to_owned(),
            &run_options(temp.path().to_path_buf()),
        )
        .await?;

    let mailbox_statuses = controls
        .iter()
        .filter_map(|control| match control {
            sigil_kernel::ControlEntry::AgentMailboxMessage(entry) => Some(entry.status),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        mailbox_statuses,
        vec![
            sigil_kernel::AgentMailboxStatus::Queued,
            sigil_kernel::AgentMailboxStatus::Delivered
        ]
    );
    let projection = session.agent_thread_state_projection();
    let mailbox = projection
        .mailbox_messages
        .values()
        .next()
        .expect("mailbox message should be projected");
    assert_eq!(mailbox.status, sigil_kernel::AgentMailboxStatus::Delivered);
    Ok(())
}

#[test]
fn foreground_background_request_reports_budget_and_missing_foreground() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut budget = AgentBudgetPolicy::from_root_config(&root_config());
    budget.max_threads = 2;
    budget.max_parallel_readonly = 2;
    budget.max_background_threads = 1;
    let supervisor = supervisor_with_budget(budget)?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();

    let no_foreground = supervisor
        .request_foreground_background()
        .expect_err("missing foreground child should reject background request");
    assert_eq!(
        no_foreground,
        "no foreground child agent is currently running"
    );

    let mut background_start = chat_child_start(EXPLORE_PROFILE_ID, temp.path().to_path_buf())?;
    background_start.call_id = "call_background_budget".to_owned();
    background_start.invocation_mode = AgentInvocationMode::Background;
    supervisor.begin_chat_child_thread(&mut session, &mut handler, background_start)?;

    let budget_error = supervisor
        .request_foreground_background()
        .expect_err("background budget should be enforced before searching foreground children");
    assert!(budget_error.contains("[task].max_background_threads=1"));
    Ok(())
}

#[test]
fn supervisor_enforces_max_depth() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut budget = AgentBudgetPolicy::from_root_config(&root_config());
    budget.max_depth = 0;
    let supervisor = supervisor_with_budget(budget)?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();

    let error = supervisor
        .begin_task_child_thread(
            &mut session,
            &mut handler,
            child_start(step("inspect")?, temp.path().to_path_buf())?,
        )
        .expect_err("max_depth=0 denies child thread");

    assert!(error.to_string().contains("agent budget denied"));
    let projection = session.agent_thread_state_projection();
    assert!(projection.threads.values().any(|thread| {
        thread
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("max_depth=0"))
    }));
    Ok(())
}

#[test]
fn supervisor_enforces_nested_depth_from_parent_thread() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut budget = AgentBudgetPolicy::from_root_config(&root_config());
    budget.max_depth = 1;
    let supervisor = supervisor_with_budget(budget)?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let mut start = child_start(step("nested")?, temp.path().to_path_buf())?;
    start.parent_thread_id = AgentThreadId::new("child_parent")?;
    start.parent_depth = 1;

    let error = supervisor
        .begin_task_child_thread(&mut session, &mut handler, start)
        .expect_err("nested child is denied at max_depth=1");

    assert!(error.to_string().contains("agent budget denied"));
    let projection = session.agent_thread_state_projection();
    assert!(projection.threads.values().any(|thread| {
        thread
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("max_depth=1"))
    }));
    Ok(())
}

#[test]
fn supervisor_enforces_max_spawn_fanout_per_turn() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut budget = AgentBudgetPolicy::from_root_config(&root_config());
    budget.max_parallel_readonly = 2;
    budget.max_threads = 3;
    budget.max_spawn_fanout_per_turn = 1;
    let supervisor = supervisor_with_budget(budget)?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();

    supervisor.begin_task_child_thread(
        &mut session,
        &mut handler,
        child_start(step("one")?, temp.path().to_path_buf())?,
    )?;
    let error = supervisor
        .begin_task_child_thread(
            &mut session,
            &mut handler,
            child_start(step("two")?, temp.path().to_path_buf())?,
        )
        .expect_err("fanout limit denies second spawn");

    assert!(error.to_string().contains("agent budget denied"));
    assert_eq!(supervisor.active_profile_ids().len(), 1);
    Ok(())
}

#[test]
fn reset_turn_budget_allows_next_spawn_window() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut budget = AgentBudgetPolicy::from_root_config(&root_config());
    budget.max_parallel_readonly = 2;
    budget.max_threads = 2;
    budget.max_spawn_fanout_per_turn = 1;
    let supervisor = supervisor_with_budget(budget)?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();

    supervisor.begin_task_child_thread(
        &mut session,
        &mut handler,
        child_start(step("one")?, temp.path().to_path_buf())?,
    )?;
    supervisor.reset_turn_budget();
    supervisor.begin_task_child_thread(
        &mut session,
        &mut handler,
        child_start(step("two")?, temp.path().to_path_buf())?,
    )?;

    assert_eq!(supervisor.active_profile_ids().len(), 2);
    Ok(())
}

#[test]
fn cancel_foreground_run_releases_active_child_and_appends_audit() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut budget = AgentBudgetPolicy::from_root_config(&root_config());
    budget.max_parallel_readonly = 2;
    budget.max_threads = 2;
    let supervisor = supervisor_with_budget(budget)?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();

    let first = supervisor.begin_task_child_thread(
        &mut session,
        &mut handler,
        child_start(step("one")?, temp.path().to_path_buf())?,
    )?;
    assert_eq!(supervisor.active_profile_ids().len(), 1);

    let impact = supervisor.cancel_foreground_run();
    assert_eq!(impact.foreground_children_interrupted.len(), 1);
    assert_eq!(
        impact.foreground_children_interrupted[0].thread_id,
        first.thread_id
    );
    assert!(supervisor.active_profile_ids().is_empty());

    AgentSupervisor::append_foreground_cancel_audit(
        &mut session,
        &mut handler,
        impact,
        "run cancelled from test",
    )?;
    let projection = session.agent_thread_state_projection();
    let thread = projection
        .threads
        .get(&first.thread_id)
        .expect("cancelled thread projected");
    assert_eq!(thread.status, sigil_kernel::AgentThreadStatus::Interrupted);
    assert_eq!(thread.reason.as_deref(), Some("run cancelled from test"));

    supervisor.reset_turn_budget();
    supervisor.begin_task_child_thread(
        &mut session,
        &mut handler,
        child_start(step("two")?, temp.path().to_path_buf())?,
    )?;
    assert_eq!(supervisor.active_profile_ids().len(), 1);
    Ok(())
}

#[test]
fn budget_policy_from_config_exposes_parallel_readonly_and_accessors() -> Result<()> {
    let mut config = root_config();
    config.task.max_child_sessions = 3;
    config.task.allow_parallel_readonly_subagents = true;
    config.task.max_parallel_readonly = Some(2);
    config.task.max_parallel_write = Some(1);
    config.task.max_background_threads = Some(2);
    config.task.max_spawn_fanout_per_turn = Some(2);
    config.task.max_agent_tokens_per_task = Some(123_456);
    let budget = AgentBudgetPolicy::from_root_config(&config);

    assert_eq!(budget.max_threads, 3);
    assert_eq!(budget.max_parallel_readonly, 2);
    assert_eq!(budget.max_parallel_write, 1);
    assert_eq!(budget.max_background_threads, 2);
    assert_eq!(budget.max_spawn_fanout_per_turn, 2);
    assert_eq!(budget.max_agent_tokens_per_task, 123_456);

    let supervisor = AgentSupervisor::new(
        AgentProfileRegistry::from_root_config(&config)?,
        budget,
        provider_capabilities(),
    );
    assert_eq!(supervisor.budget().max_threads, 3);
    assert_eq!(supervisor.registry().profiles().len(), 4);

    let mut default_config = root_config();
    default_config.task.max_child_sessions = 4;
    default_config.task.allow_parallel_readonly_subagents = true;
    let default_budget = AgentBudgetPolicy::from_root_config(&default_config);
    assert_eq!(default_budget.max_threads, 4);
    assert_eq!(default_budget.max_parallel_readonly, 3);
    assert_eq!(default_budget.max_spawn_fanout_per_turn, 4);
    assert_eq!(default_budget.max_agent_tokens_per_task, 200_000);

    default_config.task.allow_parallel_readonly_subagents = false;
    let serialized_readonly_budget = AgentBudgetPolicy::from_root_config(&default_config);
    assert_eq!(serialized_readonly_budget.max_parallel_readonly, 3);
    Ok(())
}

#[test]
fn budget_policy_uses_default_limits_when_config_values_are_omitted() {
    let mut config = root_config();
    config.task.max_child_sessions = 4;
    config.task.allow_parallel_readonly_subagents = true;
    config.task.max_parallel_readonly = None;
    config.task.max_spawn_fanout_per_turn = None;
    config.task.max_agent_tokens_per_task = None;

    let default_budget = AgentBudgetPolicy::from_root_config(std::hint::black_box(&config));

    assert_eq!(default_budget.max_parallel_readonly, 3);
    assert_eq!(default_budget.max_spawn_fanout_per_turn, 4);
    assert_eq!(default_budget.max_agent_tokens_per_task, 200_000);

    config.task.allow_parallel_readonly_subagents = false;
    let serialized_readonly_budget =
        AgentBudgetPolicy::from_root_config(std::hint::black_box(&config));

    assert_eq!(serialized_readonly_budget.max_parallel_readonly, 3);
}

#[test]
fn supervisor_denies_before_spawn_when_task_token_budget_is_exhausted() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut budget = AgentBudgetPolicy::from_root_config(&root_config());
    budget.max_agent_tokens_per_task = 0;
    let supervisor = supervisor_with_budget(budget)?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();

    let error = supervisor
        .begin_task_child_thread(
            &mut session,
            &mut handler,
            child_start(step("blocked")?, temp.path().to_path_buf())?,
        )
        .expect_err("zero token budget denies child before spawn");

    assert!(error.to_string().contains("agent budget denied"));
    let projection = session.agent_thread_state_projection();
    assert!(projection.threads.values().any(|thread| {
        thread
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("token budget exceeded before spawn"))
    }));
    Ok(())
}

#[test]
fn supervisor_enforces_background_read_budget() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut budget = AgentBudgetPolicy::from_root_config(&root_config());
    budget.max_background_threads = 0;
    let supervisor = supervisor_with_budget(budget)?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let mut start = child_start(step("background")?, temp.path().to_path_buf())?;
    start.invocation_mode = AgentInvocationMode::Background;

    let error = supervisor
        .begin_task_child_thread(&mut session, &mut handler, start)
        .expect_err("background budget denies read child");

    assert!(error.to_string().contains("agent budget denied"));
    let projection = session.agent_thread_state_projection();
    assert!(projection.threads.values().any(|thread| {
        thread
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("[task].max_background_threads=0"))
    }));
    Ok(())
}

#[test]
fn supervisor_enforces_parallel_readonly_budget() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut budget = AgentBudgetPolicy::from_root_config(&root_config());
    budget.max_parallel_readonly = 0;
    let supervisor = supervisor_with_budget(budget)?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();

    let error = supervisor
        .begin_task_child_thread(
            &mut session,
            &mut handler,
            child_start(step("readonly")?, temp.path().to_path_buf())?,
        )
        .expect_err("readonly budget denies read child");

    assert!(error.to_string().contains("agent budget denied"));
    let projection = session.agent_thread_state_projection();
    assert!(projection.threads.values().any(|thread| {
        thread
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("[task].max_parallel_readonly=0"))
    }));
    Ok(())
}

#[test]
fn supervisor_enforces_parallel_write_budget_for_readonly_scoped_writer() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut config = root_config();
    config.task.allow_write_subagents = false;
    let mut budget = AgentBudgetPolicy::from_root_config(&config);
    budget.max_parallel_write = 0;
    let supervisor = AgentSupervisor::new(
        AgentProfileRegistry::from_root_config(&config)?,
        budget,
        provider_capabilities(),
    );
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let mut start = child_start(write_step("write_budget")?, temp.path().to_path_buf())?;
    start.role = AgentRole::SubagentWrite;

    let error = supervisor
        .begin_task_child_thread(&mut session, &mut handler, start)
        .expect_err("write budget denies write child");

    assert!(error.to_string().contains("agent budget denied"));
    let projection = session.agent_thread_state_projection();
    assert!(projection.threads.values().any(|thread| {
        thread
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("[task].max_parallel_write=0"))
    }));
    Ok(())
}

#[test]
fn supervisor_allows_background_write_role_when_scope_is_readonly() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut config = root_config();
    config.task.allow_write_subagents = false;
    let mut budget = AgentBudgetPolicy::from_root_config(&config);
    budget.max_background_threads = 1;
    budget.max_parallel_write = 1;
    let supervisor = AgentSupervisor::new(
        AgentProfileRegistry::from_root_config(&config)?,
        budget,
        provider_capabilities(),
    );
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let mut start = child_start(write_step("background_write")?, temp.path().to_path_buf())?;
    start.role = AgentRole::SubagentWrite;
    start.invocation_mode = AgentInvocationMode::Background;

    let thread = supervisor
        .begin_task_child_thread(&mut session, &mut handler, start)
        .expect("read-only scoped write role should be allowed");

    assert!(thread.thread_id.as_str().starts_with("agent_v1_"));
    let projection = session.agent_thread_state_projection();
    assert!(projection.threads.values().any(|thread| {
        thread.status == sigil_kernel::AgentThreadStatus::Running
            && thread.reason.as_deref() == Some("child session started")
    }));
    Ok(())
}

#[test]
fn supervisor_denied_budget_appends_control_entry() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut budget = AgentBudgetPolicy::from_root_config(&root_config());
    budget.max_threads = 0;
    let supervisor = supervisor_with_budget(budget)?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();

    let _ = supervisor
        .begin_task_child_thread(
            &mut session,
            &mut handler,
            child_start(step("inspect")?, temp.path().to_path_buf())?,
        )
        .expect_err("thread budget denies child");

    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            sigil_kernel::SessionLogEntry::Control(
                sigil_kernel::ControlEntry::AgentThreadStatusChanged(status)
            ) if status.status == sigil_kernel::AgentThreadStatus::Failed
        )
    }));
    Ok(())
}

#[test]
fn task_child_status_and_terminal_status_cover_edges() {
    let mut max_turns = AgentRunOutcome {
        terminal_reason: AgentRunTerminalReason::MaxTurns,
        ..AgentRunOutcome::default()
    };
    assert_eq!(
        task_child_status_from_outcome("partial", &max_turns),
        TaskChildSessionStatus::Interrupted
    );

    max_turns.terminal_reason = AgentRunTerminalReason::FinalAnswer;
    max_turns.approval_denials = 1;
    assert_eq!(
        task_child_status_from_outcome("denied", &max_turns),
        TaskChildSessionStatus::Failed
    );

    assert_eq!(
        task_child_status_from_outcome(
            "",
            &AgentRunOutcome {
                tool_errors: vec![ToolError {
                    kind: ToolErrorKind::Internal,
                    message: "boom".to_owned(),
                    retryable: false,
                    details: Value::Null,
                }],
                ..AgentRunOutcome::default()
            }
        ),
        TaskChildSessionStatus::Failed
    );

    assert_eq!(
        agent_terminal_status_from_task_child(TaskChildSessionStatus::Started),
        AgentThreadTerminalStatus::Interrupted
    );
    assert_eq!(
        agent_terminal_status_from_task_child(TaskChildSessionStatus::Interrupted),
        AgentThreadTerminalStatus::Interrupted
    );
    assert_eq!(
        agent_terminal_status_from_task_child(TaskChildSessionStatus::Cancelled),
        AgentThreadTerminalStatus::Cancelled
    );
    assert_eq!(
        agent_terminal_status_from_task_child(TaskChildSessionStatus::Unavailable),
        AgentThreadTerminalStatus::Failed
    );
}

#[test]
fn supervisor_denies_background_write_agents() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut config = root_config();
    config.task.allow_write_subagents = true;
    let mut budget = AgentBudgetPolicy::from_root_config(&config);
    budget.max_background_threads = 2;
    let supervisor = AgentSupervisor::new(
        AgentProfileRegistry::from_root_config(&config)?,
        budget,
        provider_capabilities(),
    );
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let mut start = child_start(write_step("edit")?, temp.path().to_path_buf())?;
    start.role = AgentRole::SubagentWrite;
    start.invocation_mode = AgentInvocationMode::Background;

    let _error = supervisor
        .begin_task_child_thread(&mut session, &mut handler, start)
        .expect_err("background write agent is denied");

    let projection = session.agent_thread_state_projection();
    assert!(projection.threads.values().any(|thread| {
        thread.reason.as_deref().is_some_and(|reason| {
            reason.contains("write-capable agents require guarded changeset-only scope")
        })
    }));
    Ok(())
}

#[test]
fn supervisor_denies_unguarded_mcp_write_prefix_agents() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut config = root_config();
    config.task.allow_write_subagents = true;
    config.task.subagent_write.tools = sigil_kernel::ToolAllowlistConfig {
        allow_all: false,
        names: Vec::new(),
        prefixes: vec!["mcp__filesystem__".to_owned()],
    };
    let supervisor = AgentSupervisor::new(
        AgentProfileRegistry::from_root_config(&config)?,
        AgentBudgetPolicy::from_root_config(&config),
        provider_capabilities(),
    );
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let mut start = child_start(write_step("mcp_write")?, temp.path().to_path_buf())?;
    start.role = AgentRole::SubagentWrite;

    let error = supervisor
        .begin_task_child_thread(&mut session, &mut handler, start)
        .expect_err("unguarded mcp write prefix is denied");

    assert!(
        error
            .to_string()
            .contains("write-capable agent requires guarded changeset-only scope")
    );
    Ok(())
}

#[test]
fn supervisor_denies_foreground_write_capable_agents_without_changeset_guard() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut config = root_config();
    config.task.allow_write_subagents = true;
    let supervisor = AgentSupervisor::new(
        AgentProfileRegistry::from_root_config(&config)?,
        AgentBudgetPolicy::from_root_config(&config),
        provider_capabilities(),
    );
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let mut start = child_start(write_step("edit")?, temp.path().to_path_buf())?;
    start.role = AgentRole::SubagentWrite;

    let error = supervisor
        .begin_task_child_thread(&mut session, &mut handler, start)
        .expect_err("foreground write-capable agent is denied until guarded writes exist");

    assert!(
        error
            .to_string()
            .contains("write-capable agent requires guarded changeset-only scope")
    );
    Ok(())
}

#[test]
fn supervisor_allows_changeset_only_write_capable_agents() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut config = root_config();
    config.task.subagent_write.tools = sigil_kernel::ToolAllowlistConfig {
        allow_all: false,
        names: vec!["apply_changeset".to_owned()],
        prefixes: Vec::new(),
    };
    let supervisor = AgentSupervisor::new(
        AgentProfileRegistry::from_root_config(&config)?,
        AgentBudgetPolicy::from_root_config(&config),
        provider_capabilities(),
    );
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let mut start = child_start(write_step("changeset")?, temp.path().to_path_buf())?;
    start.role = AgentRole::SubagentWrite;

    let thread = supervisor.begin_task_child_thread(&mut session, &mut handler, start)?;

    let projection = session.agent_thread_state_projection();
    let projected = projection
        .threads
        .get(&thread.thread_id)
        .expect("thread should be projected");
    assert_eq!(projected.status, sigil_kernel::AgentThreadStatus::Running);
    assert_eq!(projected.reason.as_deref(), Some("child session started"));
    Ok(())
}

#[test]
fn supervisor_records_changed_paths_and_usage_in_agent_result() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let supervisor = supervisor_with_budget(AgentBudgetPolicy::from_root_config(&root_config()))?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let thread = supervisor.begin_task_child_thread(
        &mut session,
        &mut handler,
        child_start(step("inspect")?, temp.path().to_path_buf())?,
    )?;
    let mut outcome = AgentRunOutcome::default();
    outcome
        .changed_files
        .push("crates/sigil-runtime/src/lib.rs".to_owned());
    let usage = AgentUsageSummary {
        input_tokens: 8,
        output_tokens: 5,
        total_tokens: 13,
        cached_tokens: Some(2),
    };

    supervisor.record_task_child_result(
        &mut session,
        &mut handler,
        &thread,
        SessionRef::new_relative("children/task_1/inspect.jsonl")?,
        sigil_kernel::TaskChildSessionStatus::Completed,
        "done",
        &outcome,
        Some(usage.clone()),
        None,
    )?;

    let projection = session.agent_thread_state_projection();
    let projected = projection
        .threads
        .get(&thread.thread_id)
        .expect("thread was projected");
    let result = projected.result.as_ref().expect("result was recorded");
    assert_eq!(result.changed_paths, outcome.changed_files);
    assert_eq!(result.usage.as_ref(), Some(&usage));
    assert!(
        projected
            .merge_safe_points
            .iter()
            .any(|safe_point| safe_point.parent_thread_id.as_str() == "main")
    );
    Ok(())
}

#[test]
fn write_capable_scope_detects_specific_mcp_prefixes() {
    let mcp_scope = ToolRegistryScope {
        prefixes: vec!["mcp__gitlab__".to_owned()],
        ..ToolRegistryScope::default()
    };
    assert!(tool_scope_is_write_capable(&mcp_scope));

    let read_scope = ToolRegistryScope {
        names: BTreeSet::from(["grep".to_owned()]),
        ..ToolRegistryScope::default()
    };
    assert!(!tool_scope_is_write_capable(&read_scope));
}

#[test]
fn cancel_foreground_does_not_cancel_background_child() -> Result<()> {
    let supervisor = supervisor_with_budget(AgentBudgetPolicy::from_root_config(&root_config()))?;

    let impact = supervisor.cancel_foreground_run();

    assert_eq!(impact.background_children_cancelled, 0);
    Ok(())
}

#[test]
fn provider_background_resume_defaults_to_interrupted() -> Result<()> {
    let supervisor = supervisor_with_budget(AgentBudgetPolicy::from_root_config(&root_config()))?;

    assert!(!supervisor.supports_background_resume());
    Ok(())
}

#[tokio::test]
async fn supervisor_records_post_run_token_budget_warning_without_failing_child() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut budget = AgentBudgetPolicy::from_root_config(&root_config());
    budget.max_agent_tokens_per_task = 10;
    let supervisor = supervisor_with_budget(budget)?;
    let runner = AgentSupervisorTaskChildRunner::new(
        supervisor,
        Agent::new(Box::new(UsageProvider), ToolRegistry::new()),
        Agent::new(
            Box::new(TextProvider {
                text: "writer done",
            }),
            ToolRegistry::new(),
        ),
    );
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;

    let output = runner
        .run_child_session(
            &mut session,
            TaskChildSessionRunRequest {
                task: sigil_kernel::SequentialTaskRequest {
                    task_id: TaskId::new("task_1")?,
                    parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                    objective: "invoke agent".to_owned(),
                },
                plan_version: 1,
                step: step("usage")?,
                child_input: AgentRunInput::without_persisted_user_message(vec![
                    ModelMessage::user("apply skill"),
                ]),
                options: run_options(temp.path().to_path_buf()),
            },
            &mut handler,
            &mut approval,
        )
        .await?;

    assert_eq!(output.final_text, "too expensive");
    let projection = session.agent_thread_state_projection();
    let thread = projection.latest_thread().expect("child thread");
    assert_eq!(thread.status, sigil_kernel::AgentThreadStatus::Completed);
    assert!(handler.events.iter().any(|event| {
        matches!(event, RunEvent::Notice(message) if message.contains("agent budget warning after child completion"))
    }));
    Ok(())
}

#[tokio::test]
async fn supervisor_enforces_cumulative_agent_tokens_per_task() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut budget = AgentBudgetPolicy::from_root_config(&root_config());
    budget.max_agent_tokens_per_task = 20;
    let supervisor = supervisor_with_budget(budget)?;
    let runner = AgentSupervisorTaskChildRunner::new(
        supervisor,
        Agent::new(Box::new(UsageProvider), ToolRegistry::new()),
        Agent::new(
            Box::new(TextProvider {
                text: "writer done",
            }),
            ToolRegistry::new(),
        ),
    );
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;

    runner
        .run_child_session(
            &mut session,
            TaskChildSessionRunRequest {
                task: sigil_kernel::SequentialTaskRequest {
                    task_id: TaskId::new("task_1")?,
                    parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                    objective: "invoke agent".to_owned(),
                },
                plan_version: 1,
                step: step("usage_one")?,
                child_input: AgentRunInput::without_persisted_user_message(vec![
                    ModelMessage::user("apply skill"),
                ]),
                options: run_options(temp.path().to_path_buf()),
            },
            &mut handler,
            &mut approval,
        )
        .await?;

    runner
        .run_child_session(
            &mut session,
            TaskChildSessionRunRequest {
                task: sigil_kernel::SequentialTaskRequest {
                    task_id: TaskId::new("task_1")?,
                    parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                    objective: "invoke agent".to_owned(),
                },
                plan_version: 1,
                step: step("usage_two")?,
                child_input: AgentRunInput::without_persisted_user_message(vec![
                    ModelMessage::user("apply skill again"),
                ]),
                options: run_options(temp.path().to_path_buf()),
            },
            &mut handler,
            &mut approval,
        )
        .await?;

    let result = runner
        .run_child_session(
            &mut session,
            TaskChildSessionRunRequest {
                task: sigil_kernel::SequentialTaskRequest {
                    task_id: TaskId::new("task_1")?,
                    parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                    objective: "invoke agent".to_owned(),
                },
                plan_version: 1,
                step: step("usage_three")?,
                child_input: AgentRunInput::without_persisted_user_message(vec![
                    ModelMessage::user("apply skill after budget"),
                ]),
                options: run_options(temp.path().to_path_buf()),
            },
            &mut handler,
            &mut approval,
        )
        .await;

    let error = result.expect_err("cumulative task usage above budget must deny next spawn");
    assert!(
        error
            .to_string()
            .contains("agent budget denied child session")
    );
    let projection = session.agent_thread_state_projection();
    assert!(projection.threads.values().any(|thread| {
        thread
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("token budget exceeded before spawn"))
    }));
    Ok(())
}

#[tokio::test]
async fn child_run_context_uses_selected_role_provider_capabilities() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut config = root_config();
    config.task.allow_write_subagents = false;
    let supervisor = AgentSupervisor::new(
        AgentProfileRegistry::from_root_config(&config)?,
        AgentBudgetPolicy::from_root_config(&config),
        provider_capabilities(),
    );
    let runner = AgentSupervisorTaskChildRunner::new(
        supervisor,
        Agent::new(
            Box::new(TextProvider { text: "read done" }),
            ToolRegistry::new(),
        ),
        Agent::new(Box::new(ResultReplayProvider), ToolRegistry::new()),
    );
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;

    runner
        .run_child_session(
            &mut session,
            TaskChildSessionRunRequest {
                task: sigil_kernel::SequentialTaskRequest {
                    task_id: TaskId::new("task_1")?,
                    parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                    objective: "invoke writer".to_owned(),
                },
                plan_version: 1,
                step: write_step("inspect")?,
                child_input: AgentRunInput::without_persisted_user_message(vec![
                    ModelMessage::user("inspect only"),
                ]),
                options: run_options(temp.path().to_path_buf()),
            },
            &mut handler,
            &mut approval,
        )
        .await?;

    let mut expected = provider_capabilities();
    expected.supports_agent_result_replay = true;
    let expected_hash = provider_capability_hash(&expected)?;
    let projection = session.agent_thread_state_projection();
    let thread = projection
        .threads
        .values()
        .find(|thread| !thread.legacy_task)
        .expect("agent thread projected");
    assert_eq!(
        thread
            .run_context
            .as_ref()
            .map(|context| context.provider_capability_hash.as_str()),
        Some(expected_hash.as_str())
    );
    Ok(())
}

#[tokio::test]
async fn direct_child_skill_uses_supervisor() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config = root_config();
    let supervisor = AgentSupervisor::new(
        AgentProfileRegistry::from_root_config(&config)?,
        AgentBudgetPolicy::from_root_config(&config),
        provider_capabilities(),
    );
    let runner = AgentSupervisorTaskChildRunner::new(
        supervisor,
        Agent::new(
            Box::new(TextProvider { text: "child done" }),
            ToolRegistry::new(),
        ),
        Agent::new(
            Box::new(TextProvider {
                text: "writer done",
            }),
            ToolRegistry::new(),
        ),
    );
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;

    let output = runner
        .run_child_session(
            &mut session,
            TaskChildSessionRunRequest {
                task: sigil_kernel::SequentialTaskRequest {
                    task_id: TaskId::new("task_1")?,
                    parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                    objective: "invoke agent".to_owned(),
                },
                plan_version: 1,
                step: step("invoke_skill")?,
                child_input: AgentRunInput::without_persisted_user_message(vec![
                    ModelMessage::user("apply skill"),
                ]),
                options: run_options(temp.path().to_path_buf()),
            },
            &mut handler,
            &mut approval,
        )
        .await?;

    assert_eq!(output.final_text, "child done");
    let agent_projection = session.agent_thread_state_projection();
    assert_eq!(
        agent_projection
            .threads
            .values()
            .filter(|thread| !thread.legacy_task)
            .count(),
        1
    );
    assert_eq!(
        agent_projection
            .threads
            .values()
            .filter(|thread| thread.legacy_task)
            .count(),
        1
    );
    let task_projection = session.task_state_projection();
    let task = task_projection
        .tasks
        .get(&TaskId::new("task_1")?)
        .expect("task child session projected");
    assert_eq!(task.child_sessions.len(), 1);
    Ok(())
}

#[tokio::test]
async fn child_tool_approval_routes_are_audited_and_stored() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config = root_config();
    let supervisor = AgentSupervisor::new(
        AgentProfileRegistry::from_root_config(&config)?,
        AgentBudgetPolicy::from_root_config(&config),
        provider_capabilities(),
    );
    let mut read_tools = ToolRegistry::new();
    read_tools.register(Arc::new(ApprovalRouteTool));
    let runner = AgentSupervisorTaskChildRunner::new(
        supervisor,
        Agent::new(Box::new(ToolCallingChildProvider), read_tools),
        Agent::new(
            Box::new(TextProvider {
                text: "writer done",
            }),
            ToolRegistry::new(),
        ),
    );
    let store = JsonlSessionStore::new(temp.path().join("parent.jsonl"))?;
    let mut session = Session::load_from_store("deepseek", "deepseek-v4-flash", store)?;
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;

    let output = runner
        .run_child_session(
            &mut session,
            TaskChildSessionRunRequest {
                task: sigil_kernel::SequentialTaskRequest {
                    task_id: TaskId::new("task_1")?,
                    parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                    objective: "invoke agent".to_owned(),
                },
                plan_version: 1,
                step: step("approval_route")?,
                child_input: AgentRunInput::without_persisted_user_message(vec![
                    ModelMessage::user("read through approval"),
                ]),
                options: run_options(temp.path().to_path_buf()),
            },
            &mut handler,
            &mut approval,
        )
        .await?;

    assert_eq!(output.final_text, "tool route done");
    let agent_statuses = agent_route_statuses(&session);
    assert!(agent_statuses.contains(&sigil_kernel::AgentRouteStatus::Requested));
    assert!(agent_statuses.contains(&sigil_kernel::AgentRouteStatus::Resolved));
    let task_statuses = task_route_statuses(&session);
    assert!(task_statuses.contains(&TaskRouteStatus::Requested));
    assert!(task_statuses.contains(&TaskRouteStatus::Resolved));
    assert!(
        temp.path()
            .join("children/task_1/approval_route-child_v1_approval_route.jsonl")
            .exists()
    );
    Ok(())
}

#[tokio::test]
async fn failed_child_does_not_append_successful_parent_answer() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config = root_config();
    let supervisor = AgentSupervisor::new(
        AgentProfileRegistry::from_root_config(&config)?,
        AgentBudgetPolicy::from_root_config(&config),
        provider_capabilities(),
    );
    let runner = AgentSupervisorTaskChildRunner::new(
        supervisor,
        Agent::new(Box::new(FailingProvider), ToolRegistry::new()),
        Agent::new(
            Box::new(TextProvider {
                text: "writer done",
            }),
            ToolRegistry::new(),
        ),
    );
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;

    let result = runner
        .run_child_session(
            &mut session,
            TaskChildSessionRunRequest {
                task: sigil_kernel::SequentialTaskRequest {
                    task_id: TaskId::new("task_1")?,
                    parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                    objective: "invoke agent".to_owned(),
                },
                plan_version: 1,
                step: step("invoke_skill")?,
                child_input: AgentRunInput::without_persisted_user_message(vec![
                    ModelMessage::user("apply skill"),
                ]),
                options: run_options(temp.path().to_path_buf()),
            },
            &mut handler,
            &mut approval,
        )
        .await;

    assert!(result.is_err());
    assert!(session.messages().is_empty());
    let task_projection = session.task_state_projection();
    let task = task_projection
        .tasks
        .get(&TaskId::new("task_1")?)
        .expect("task child session projected");
    assert!(
        task.child_sessions
            .values()
            .any(|child| child.status == sigil_kernel::TaskChildSessionStatus::Failed)
    );
    Ok(())
}
