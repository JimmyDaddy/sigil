use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use futures::{Stream, stream};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    Agent, AgentConfig, AgentDelegationAdmissionEntry, AgentFinalAnswerRef, AgentInvocationMode,
    AgentInvocationSource, AgentRole, AgentRouteId, AgentRunInput, AgentRunOptions,
    AgentRunOutcome, AgentRunTerminalReason, AgentThreadId, AgentThreadTerminalStatus,
    AgentUsageSummary, ApprovalMode, AutoApproveHandler, CompactionConfig, CompletionRequest,
    DelegationAuthorityRecord, EventHandler, InteractionMode, JsonlSessionStore, MemoryConfig,
    MessageRole, ModelMessage, MultiAgentMode, PermissionConfig, Provider, ProviderCapabilities,
    ProviderChunk, ReasoningStreamSupport, RootConfig, RunCancellationOwner, RunEvent, Session,
    SessionConfig, SessionLogEntry, SessionRef, TASK_PLAN_UPDATE_TOOL_NAME,
    TaskChildSessionRunRequest, TaskChildSessionRunner, TaskChildSessionStatus, TaskId,
    TaskParticipantAttemptId, TaskParticipantPurpose, TaskPlanUpdateContext,
    TaskPlannerSessionRunRequest, TaskRouteStatus, TaskStepId, TaskStepSpec,
    TaskSubagentApprovalRouteEntry, Tool, ToolAccess, ToolCall, ToolCategory, ToolContext,
    ToolError, ToolErrorKind, ToolPreviewCapability, ToolRegistry, ToolRegistryScope, ToolResult,
    ToolResultMeta, ToolSpec, UsageStats, WorkspaceConfig, child_session_ref,
    task_participant_attempt_id, task_participant_session_ref,
};

use super::{
    AgentBudgetPolicy, AgentChatChildStart, AgentMailboxMessage, AgentProfileRegistry,
    AgentResultMaterialization, AgentSupervisor, AgentSupervisorTaskChildRunner,
    AgentTaskChildStart, REQUEST_TASK_DISCOVERY_TOOL_NAME, agent_terminal_status_from_task_child,
    task_child_status_from_outcome, tool_scope_is_write_capable,
};
use crate::{AgentToolRuntime, EXPLORE_PROFILE_ID};

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

fn participant_attempt_id_for(step_id: &str) -> Result<TaskParticipantAttemptId> {
    task_participant_attempt_id(
        &TaskId::new("task_1")?,
        TaskParticipantPurpose::Step,
        Some(1),
        Some(&TaskStepId::new(step_id)?),
        1,
    )
}

fn participant_session_ref_for(step_id: &str) -> Result<SessionRef> {
    let task_id = TaskId::new("task_1")?;
    let attempt_id = participant_attempt_id_for(step_id)?;
    task_participant_session_ref(&task_id, &attempt_id)
}

struct TextProvider {
    text: &'static str,
}

struct PlannerDiscoveryProvider {
    observed_results: Arc<Mutex<Option<String>>>,
}

struct ParallelDiscoveryProvider {
    barrier: Arc<tokio::sync::Barrier>,
    active: Arc<AtomicUsize>,
    max_active: Arc<AtomicUsize>,
}

struct RejectedDiscoveryPlannerProvider {
    observed_error: Arc<Mutex<Option<String>>>,
}

struct RepeatedDiscoveryPlannerProvider {
    observed_rejection: Arc<Mutex<Option<String>>>,
}

struct CountingDiscoveryProvider {
    starts: Arc<AtomicUsize>,
}

struct ParallelTaskChildProvider {
    barrier: Arc<tokio::sync::Barrier>,
    active: Arc<AtomicUsize>,
    max_active: Arc<AtomicUsize>,
    completion_order: Arc<Mutex<Vec<String>>>,
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

#[async_trait]
impl Provider for ParallelTaskChildProvider {
    fn name(&self) -> &str {
        "parallel-task-child"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        provider_capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let step_id = ["read_a", "read_b"]
            .into_iter()
            .find(|step_id| {
                request.messages.iter().any(|message| {
                    message
                        .content
                        .as_deref()
                        .is_some_and(|content| content.contains(step_id))
                })
            })
            .ok_or_else(|| anyhow!("parallel child request did not identify a test step"))?;
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(active, Ordering::SeqCst);
        self.barrier.wait().await;
        if step_id == "read_a" {
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        }
        self.completion_order
            .lock()
            .expect("completion order should not be poisoned")
            .push(step_id.to_owned());
        self.active.fetch_sub(1, Ordering::SeqCst);
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::TextDelta("parallel read done".to_owned())),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[async_trait]
impl Provider for PlannerDiscoveryProvider {
    fn name(&self) -> &str {
        "planner-discovery"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        provider_capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        if let Some(results) = request
            .messages
            .iter()
            .filter_map(|message| message.content.as_deref())
            .find(|content| content.contains(r#""type":"task_discovery_results""#))
        {
            *self
                .observed_results
                .lock()
                .expect("planner discovery observation lock should not be poisoned") =
                Some(results.to_owned());
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::ToolCallComplete(ToolCall {
                    id: "call-plan-after-discovery".to_owned(),
                    name: TASK_PLAN_UPDATE_TOOL_NAME.to_owned(),
                    args_json: json!({
                        "plan_version": 1,
                        "status": "accepted",
                        "steps": [{
                            "step_id": "implement",
                            "title": "Implement the verified change",
                            "role": "executor"
                        }]
                    })
                    .to_string(),
                })),
                Ok(ProviderChunk::Done),
            ])));
        }

        assert!(
            !request
                .messages
                .iter()
                .any(|message| matches!(message.role, MessageRole::Tool)),
            "planner should not receive a polling turn before discovery results"
        );
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::ToolCallComplete(ToolCall {
                id: "call-task-discovery".to_owned(),
                name: REQUEST_TASK_DISCOVERY_TOOL_NAME.to_owned(),
                args_json: json!({
                    "probes": [
                        {
                            "probe_id": "runtime",
                            "title": "Inspect runtime",
                            "objective": "Inspect runtime orchestration boundaries",
                            "path_hints": ["crates/sigil-runtime"]
                        },
                        {
                            "probe_id": "kernel",
                            "title": "Inspect kernel",
                            "objective": "Inspect kernel task contracts",
                            "path_hints": ["crates/sigil-kernel"]
                        }
                    ]
                })
                .to_string(),
            })),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[async_trait]
impl Provider for ParallelDiscoveryProvider {
    fn name(&self) -> &str {
        "parallel-discovery"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        provider_capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(active, Ordering::SeqCst);
        self.barrier.wait().await;
        self.active.fetch_sub(1, Ordering::SeqCst);
        let scope = request
            .messages
            .iter()
            .filter_map(|message| message.content.as_deref())
            .find(|content| content.contains("Assigned objective"))
            .unwrap_or("unknown discovery scope");
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::TextDelta(format!(
                "discovery complete: {scope}"
            ))),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[async_trait]
impl Provider for RejectedDiscoveryPlannerProvider {
    fn name(&self) -> &str {
        "rejected-discovery-planner"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        provider_capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        if let Some(error) = request
            .messages
            .iter()
            .filter(|message| matches!(message.role, MessageRole::Tool))
            .filter_map(|message| message.content.as_deref())
            .find(|content| content.contains("overlapping path hints"))
        {
            *self
                .observed_error
                .lock()
                .expect("planner discovery error observation lock should not be poisoned") =
                Some(error.to_owned());
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::ToolCallComplete(ToolCall {
                    id: "call-plan-after-rejection".to_owned(),
                    name: TASK_PLAN_UPDATE_TOOL_NAME.to_owned(),
                    args_json: json!({
                        "plan_version": 1,
                        "status": "accepted",
                        "steps": [{
                            "step_id": "implement",
                            "title": "Implement without duplicated research",
                            "role": "executor"
                        }]
                    })
                    .to_string(),
                })),
                Ok(ProviderChunk::Done),
            ])));
        }

        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::ToolCallComplete(ToolCall {
                id: "call-overlapping-discovery".to_owned(),
                name: REQUEST_TASK_DISCOVERY_TOOL_NAME.to_owned(),
                args_json: json!({
                    "probes": [
                        {
                            "probe_id": "runtime",
                            "title": "Inspect runtime",
                            "objective": "Inspect all runtime orchestration",
                            "path_hints": ["crates/sigil-runtime"]
                        },
                        {
                            "probe_id": "runtime-src",
                            "title": "Inspect runtime source",
                            "objective": "Inspect runtime source details",
                            "path_hints": ["crates/sigil-runtime/src"]
                        }
                    ]
                })
                .to_string(),
            })),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[async_trait]
impl Provider for RepeatedDiscoveryPlannerProvider {
    fn name(&self) -> &str {
        "repeated-discovery-planner"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        provider_capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        if let Some(rejection) = request
            .messages
            .iter()
            .filter(|message| matches!(message.role, MessageRole::Tool))
            .filter_map(|message| message.content.as_deref())
            .find(|content| content.contains("at most once per planning attempt"))
        {
            *self
                .observed_rejection
                .lock()
                .expect("planner discovery rejection lock should not be poisoned") =
                Some(rejection.to_owned());
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::ToolCallComplete(ToolCall {
                    id: "call-plan-after-repeat".to_owned(),
                    name: TASK_PLAN_UPDATE_TOOL_NAME.to_owned(),
                    args_json: json!({
                        "plan_version": 1,
                        "status": "accepted",
                        "steps": [{
                            "step_id": "implement",
                            "title": "Implement after one research round",
                            "role": "executor"
                        }]
                    })
                    .to_string(),
                })),
                Ok(ProviderChunk::Done),
            ])));
        }

        let has_discovery_results = request
            .messages
            .iter()
            .filter(|message| matches!(message.role, MessageRole::Tool))
            .filter_map(|message| message.content.as_deref())
            .any(|content| content.contains(r#""type":"task_discovery_results""#));
        let call_id = if has_discovery_results {
            "call-repeat-discovery"
        } else {
            "call-initial-discovery"
        };
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::ToolCallComplete(ToolCall {
                id: call_id.to_owned(),
                name: REQUEST_TASK_DISCOVERY_TOOL_NAME.to_owned(),
                args_json: json!({
                    "probes": [{
                        "probe_id": "runtime",
                        "title": "Inspect runtime",
                        "objective": "Inspect runtime orchestration",
                        "path_hints": ["crates/sigil-runtime"]
                    }]
                })
                .to_string(),
            })),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[async_trait]
impl Provider for CountingDiscoveryProvider {
    fn name(&self) -> &str {
        "counting-discovery"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        provider_capabilities()
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::TextDelta(
                "unexpected discovery start".to_owned(),
            )),
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
            network_effect: None,
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
            retention: Default::default(),
        },
        agent: AgentConfig {
            provider: "deepseek".to_owned(),
            model: "deepseek-v4-flash".to_owned(),
            max_turns: Some(12),
            tool_timeout_secs: 45,
        },
        permission: PermissionConfig::default(),
        model_request: Default::default(),
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
            }),
        )]),
        web: Default::default(),
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

#[test]
fn root_budget_allows_one_planner_owned_discovery_level() {
    let budget = AgentBudgetPolicy::from_root_config(&root_config());

    assert_eq!(budget.max_depth, 2);
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
        batch_id: None,
        batch_member_key: None,
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
    let call_id = format!("call_{profile_id}");
    let profile_id = sigil_kernel::AgentProfileId::new(profile_id)?;
    let thread_id = super::chat_agent_thread_id_for_call(&call_id, &profile_id)?;
    Ok(AgentChatChildStart {
        call_id,
        budget_scope_id: TaskId::new("chat_1")?,
        parent_thread_id: AgentThreadId::new("main")?,
        parent_depth: 0,
        batch_id: None,
        batch_member_key: None,
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        profile_id: profile_id.clone(),
        role: AgentRole::SubagentRead,
        child_session_ref: SessionRef::new_relative(format!(
            "children/{}.jsonl",
            profile_id.as_str()
        ))?,
        objective: "inspect code".to_owned(),
        prompt: "inspect code".to_owned(),
        workspace_root,
        provider_capabilities: provider_capabilities(),
        invocation_mode: AgentInvocationMode::JoinBeforeFinal,
        invocation_source: AgentInvocationSource::Chat,
        delegation_admission: AgentDelegationAdmissionEntry {
            thread_id,
            profile_id,
            invocation_mode: AgentInvocationMode::JoinBeforeFinal,
            invocation_source: AgentInvocationSource::Chat,
            authority: DelegationAuthorityRecord::ModelProactive,
            objective_hash: super::hash_text("inspect code"),
            tool_contract_fingerprint: "sha256:test-contracts".to_owned(),
            admitted_at_ms: None,
        },
        display_name_hint: Some("inspect".to_owned()),
    })
}

fn rebind_chat_delegation_admission(start: &mut AgentChatChildStart) -> Result<()> {
    start.delegation_admission.thread_id =
        super::chat_agent_thread_id_for_call(&start.call_id, &start.profile_id)?;
    start.delegation_admission.profile_id = start.profile_id.clone();
    start.delegation_admission.invocation_mode = start.invocation_mode;
    start.delegation_admission.invocation_source = start.invocation_source;
    start.delegation_admission.objective_hash =
        super::hash_text(&sigil_kernel::safe_persistence_text(&start.objective));
    Ok(())
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
fn supervisor_rejects_incomplete_batch_identity_before_control_append() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let supervisor = supervisor_with_budget(AgentBudgetPolicy::from_root_config(&root_config()))?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let mut start = child_start(step("inspect")?, temp.path().to_path_buf())?;
    start.batch_id = Some(sigil_kernel::AgentBatchId::new("batch_incomplete")?);

    let error = supervisor
        .begin_task_child_thread(&mut session, &mut handler, start)
        .expect_err("incomplete batch identity should be rejected");

    assert!(
        error
            .to_string()
            .contains("requires both batch id and member key")
    );
    assert!(session.entries().is_empty());
    assert!(handler.events.is_empty());
    Ok(())
}

#[test]
fn chat_child_start_projects_sensitive_objective_and_prompt_hash_before_control_append()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let supervisor = supervisor_with_budget(AgentBudgetPolicy::from_root_config(&root_config()))?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let raw = "inspect https://example.com/private?signature=thread-start-secret exactly";
    let mut start = chat_child_start(EXPLORE_PROFILE_ID, temp.path().to_path_buf())?;
    start.objective = raw.to_owned();
    start.prompt = raw.to_owned();
    rebind_chat_delegation_admission(&mut start)?;

    let thread = supervisor.begin_chat_child_thread(&mut session, &mut handler, start)?;

    let durable = serde_json::to_string(session.entries())?;
    assert!(!durable.contains("thread-start-secret"));
    assert!(!durable.contains(raw));
    let projected = session
        .agent_thread_state_projection()
        .threads
        .get(&thread.thread_id)
        .cloned()
        .expect("thread should project");
    let safe = sigil_kernel::safe_persistence_text(raw);
    assert_eq!(projected.objective, safe);
    assert_eq!(projected.prompt_hash, super::hash_text(&safe));
    assert_ne!(projected.prompt_hash, super::hash_text(raw));
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
    budget.max_subagents = 2;
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
    budget.max_subagents = 2;
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
        &AgentResultMaterialization::inline("child done", Some(final_answer_ref.clone())),
        &AgentRunOutcome::default(),
        None,
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
    budget.max_subagents = 2;
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
    rebind_chat_delegation_admission(&mut background_start)?;
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
    budget.max_subagents = 2;
    let supervisor = supervisor_with_budget(budget)?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let mut background_start = chat_child_start(EXPLORE_PROFILE_ID, temp.path().to_path_buf())?;
    background_start.invocation_mode = AgentInvocationMode::Background;
    rebind_chat_delegation_admission(&mut background_start)?;
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
fn foreground_background_request_reports_missing_foreground() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut budget = AgentBudgetPolicy::from_root_config(&root_config());
    budget.max_subagents = 2;
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
    rebind_chat_delegation_admission(&mut background_start)?;
    supervisor.begin_chat_child_thread(&mut session, &mut handler, background_start)?;

    let missing_foreground = supervisor
        .request_foreground_background()
        .expect_err("background-only state has no foreground child to move");
    assert_eq!(
        missing_foreground,
        "no foreground child agent is currently running"
    );
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
fn supervisor_enforces_max_subagents() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut budget = AgentBudgetPolicy::from_root_config(&root_config());
    budget.max_subagents = 1;
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
        .expect_err("max_subagents denies second active child");

    assert!(error.to_string().contains("agent budget denied"));
    assert!(
        error
            .to_string()
            .contains("agent thread budget exceeded: [task].max_subagents=1")
    );
    assert_eq!(supervisor.active_profile_ids().len(), 1);
    Ok(())
}

#[test]
fn chat_batch_reservation_is_atomic_and_claimed_by_child_start() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut budget = AgentBudgetPolicy::from_root_config(&root_config());
    budget.max_subagents = 2;
    let supervisor = supervisor_with_budget(budget)?;
    let mut first = chat_child_start(EXPLORE_PROFILE_ID, temp.path().to_path_buf())?;
    first.call_id = "call_batch_first".to_owned();
    rebind_chat_delegation_admission(&mut first)?;
    let mut second = chat_child_start(EXPLORE_PROFILE_ID, temp.path().to_path_buf())?;
    second.call_id = "call_batch_second".to_owned();
    rebind_chat_delegation_admission(&mut second)?;
    let starts = vec![first, second];

    let reservation = supervisor.reserve_chat_child_batch(&starts)?;
    assert_eq!(supervisor.active_profile_ids().len(), 2);
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let threads = starts
        .into_iter()
        .map(|start| supervisor.begin_chat_child_thread(&mut session, &mut handler, start))
        .collect::<Result<Vec<_>>>()?;
    reservation.commit();

    assert_eq!(threads.len(), 2);
    assert_eq!(supervisor.active_profile_ids().len(), 2);
    for thread in threads {
        supervisor.record_chat_child_failure(
            &mut session,
            &mut handler,
            &thread,
            "test cleanup".to_owned(),
        )?;
    }
    assert!(supervisor.active_profile_ids().is_empty());
    Ok(())
}

#[test]
fn background_chat_batch_reservation_is_atomic_and_creates_mailboxes() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut budget = AgentBudgetPolicy::from_root_config(&root_config());
    budget.max_subagents = 2;
    let supervisor = supervisor_with_budget(budget)?;
    let mut first = chat_child_start(EXPLORE_PROFILE_ID, temp.path().to_path_buf())?;
    first.call_id = "call_background_batch_first".to_owned();
    first.invocation_mode = AgentInvocationMode::Background;
    rebind_chat_delegation_admission(&mut first)?;
    let mut second = chat_child_start(EXPLORE_PROFILE_ID, temp.path().to_path_buf())?;
    second.call_id = "call_background_batch_second".to_owned();
    second.invocation_mode = AgentInvocationMode::Background;
    rebind_chat_delegation_admission(&mut second)?;
    let starts = vec![first, second];

    let reservation = supervisor.reserve_chat_child_batch(&starts)?;
    assert_eq!(supervisor.active_profile_ids().len(), 2);
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let threads = starts
        .into_iter()
        .map(|start| supervisor.begin_chat_child_thread(&mut session, &mut handler, start))
        .collect::<Result<Vec<_>>>()?;
    reservation.commit();

    assert!(threads.iter().all(|thread| thread.mailbox_rx.is_some()));
    for thread in threads {
        supervisor.record_chat_child_failure(
            &mut session,
            &mut handler,
            &thread,
            "test cleanup".to_owned(),
        )?;
    }
    assert!(supervisor.active_profile_ids().is_empty());
    Ok(())
}

#[test]
fn chat_batch_reservation_rejects_mixed_invocation_modes() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let supervisor = supervisor_with_budget(AgentBudgetPolicy::from_root_config(&root_config()))?;
    let first = chat_child_start(EXPLORE_PROFILE_ID, temp.path().to_path_buf())?;
    let mut second = chat_child_start(EXPLORE_PROFILE_ID, temp.path().to_path_buf())?;
    second.call_id = "call_background_batch_second".to_owned();
    second.invocation_mode = AgentInvocationMode::Background;
    rebind_chat_delegation_admission(&mut second)?;

    let error = supervisor
        .reserve_chat_child_batch(&[first, second])
        .err()
        .expect("mixed invocation modes should be rejected");

    assert!(error.to_string().contains("cannot mix invocation modes"));
    assert!(supervisor.active_profile_ids().is_empty());
    Ok(())
}

#[test]
fn dropped_chat_batch_reservation_releases_every_slot() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let supervisor = supervisor_with_budget(AgentBudgetPolicy::from_root_config(&root_config()))?;
    let mut first = chat_child_start(EXPLORE_PROFILE_ID, temp.path().to_path_buf())?;
    first.call_id = "call_batch_drop_first".to_owned();
    rebind_chat_delegation_admission(&mut first)?;
    let mut second = chat_child_start(EXPLORE_PROFILE_ID, temp.path().to_path_buf())?;
    second.call_id = "call_batch_drop_second".to_owned();
    rebind_chat_delegation_admission(&mut second)?;

    let reservation = supervisor.reserve_chat_child_batch(&[first, second])?;
    assert_eq!(supervisor.active_profile_ids().len(), 2);
    drop(reservation);

    assert!(supervisor.active_profile_ids().is_empty());
    Ok(())
}

#[test]
fn chat_batch_reservation_rejects_capacity_without_partial_slots() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut budget = AgentBudgetPolicy::from_root_config(&root_config());
    budget.max_subagents = 1;
    let supervisor = supervisor_with_budget(budget)?;
    let mut first = chat_child_start(EXPLORE_PROFILE_ID, temp.path().to_path_buf())?;
    first.call_id = "call_batch_first".to_owned();
    rebind_chat_delegation_admission(&mut first)?;
    let mut second = chat_child_start(EXPLORE_PROFILE_ID, temp.path().to_path_buf())?;
    second.call_id = "call_batch_second".to_owned();
    rebind_chat_delegation_admission(&mut second)?;

    let error = supervisor
        .reserve_chat_child_batch(&[first, second])
        .err()
        .expect("oversized batch should be rejected");

    assert!(error.to_string().contains("requested=2"));
    assert!(supervisor.active_profile_ids().is_empty());
    Ok(())
}

#[test]
fn chat_batch_reservation_rejects_duplicate_identity_without_partial_slots() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let supervisor = supervisor_with_budget(AgentBudgetPolicy::from_root_config(&root_config()))?;
    let start = chat_child_start(EXPLORE_PROFILE_ID, temp.path().to_path_buf())?;

    let error = supervisor
        .reserve_chat_child_batch(&[start.clone(), start])
        .err()
        .expect("duplicate batch identity should be rejected");

    assert!(error.to_string().contains("duplicate thread"));
    assert!(supervisor.active_profile_ids().is_empty());
    Ok(())
}

#[test]
fn release_allows_next_spawn_after_max_subagents_slot_opens() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut budget = AgentBudgetPolicy::from_root_config(&root_config());
    budget.max_subagents = 1;
    let supervisor = supervisor_with_budget(budget)?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();

    let first = supervisor.begin_task_child_thread(
        &mut session,
        &mut handler,
        child_start(step("one")?, temp.path().to_path_buf())?,
    )?;
    supervisor.record_task_child_result(
        &mut session,
        &mut handler,
        &first,
        SessionRef::new_relative("children/task_1/one.jsonl")?,
        TaskChildSessionStatus::Completed,
        &AgentResultMaterialization::inline("one done", None),
        &AgentRunOutcome::default(),
        None,
    )?;
    supervisor.begin_task_child_thread(
        &mut session,
        &mut handler,
        child_start(step("two")?, temp.path().to_path_buf())?,
    )?;

    assert_eq!(supervisor.active_profile_ids().len(), 1);
    Ok(())
}

#[test]
fn cancel_foreground_run_releases_active_child_and_appends_audit() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut budget = AgentBudgetPolicy::from_root_config(&root_config());
    budget.max_subagents = 2;
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

    supervisor.begin_task_child_thread(
        &mut session,
        &mut handler,
        child_start(step("two")?, temp.path().to_path_buf())?,
    )?;
    assert_eq!(supervisor.active_profile_ids().len(), 1);
    Ok(())
}

#[test]
fn budget_policy_from_config_exposes_max_subagents_and_accessors() -> Result<()> {
    let mut config = root_config();
    config.task.max_subagents = 3;
    let budget = AgentBudgetPolicy::from_root_config(&config);

    assert_eq!(budget.max_subagents, 3);

    let supervisor = AgentSupervisor::new(
        AgentProfileRegistry::from_root_config(&config)?,
        budget,
        provider_capabilities(),
    );
    assert_eq!(supervisor.budget().max_subagents, 3);
    assert_eq!(supervisor.registry().profiles().len(), 4);

    let mut default_config = root_config();
    default_config.task.max_subagents = 4;
    let default_budget = AgentBudgetPolicy::from_root_config(&default_config);
    assert_eq!(default_budget.max_subagents, 4);

    Ok(())
}

#[test]
fn budget_policy_uses_default_limits_when_config_values_are_omitted() {
    let config = root_config();

    let default_budget = AgentBudgetPolicy::from_root_config(std::hint::black_box(&config));

    assert_eq!(default_budget.max_subagents, 8);
}

#[test]
fn supervisor_enforces_max_subagents_for_background_read_child() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut budget = AgentBudgetPolicy::from_root_config(&root_config());
    budget.max_subagents = 0;
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
            .is_some_and(|reason| reason.contains("[task].max_subagents=0"))
    }));
    Ok(())
}

#[test]
fn supervisor_enforces_max_subagents_for_readonly_child() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut budget = AgentBudgetPolicy::from_root_config(&root_config());
    budget.max_subagents = 0;
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
            .is_some_and(|reason| reason.contains("[task].max_subagents=0"))
    }));
    Ok(())
}

#[test]
fn supervisor_enforces_max_subagents_for_readonly_scoped_writer() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut config = root_config();
    config.task.allow_write_subagents = false;
    let mut budget = AgentBudgetPolicy::from_root_config(&config);
    budget.max_subagents = 0;
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
            .is_some_and(|reason| reason.contains("[task].max_subagents=0"))
    }));
    Ok(())
}

#[test]
fn supervisor_denies_background_worker_even_when_scope_is_readonly() -> Result<()> {
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
    let mut start = child_start(write_step("background_write")?, temp.path().to_path_buf())?;
    start.role = AgentRole::SubagentWrite;
    start.invocation_mode = AgentInvocationMode::Background;

    let error = supervisor
        .begin_task_child_thread(&mut session, &mut handler, start)
        .expect_err("background worker still requires isolated merge support");

    assert!(
        error
            .to_string()
            .contains("background write-capable agent requires isolated merge support")
    );
    let projection = session.agent_thread_state_projection();
    assert!(projection.threads.values().any(|thread| {
        thread.status == sigil_kernel::AgentThreadStatus::Failed
            && thread.reason.as_deref().is_some_and(|reason| {
                reason.contains("background write-capable agents require isolated merge support")
            })
    }));
    Ok(())
}

#[test]
fn supervisor_denied_budget_appends_control_entry() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut budget = AgentBudgetPolicy::from_root_config(&root_config());
    budget.max_subagents = 0;
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
    let supervisor = AgentSupervisor::new(
        AgentProfileRegistry::from_root_config(&config)?,
        AgentBudgetPolicy::from_root_config(&config),
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
            reason.contains("background write-capable agents require isolated merge support")
        })
    }));
    Ok(())
}

#[test]
fn supervisor_worker_scope_ignores_unguarded_mcp_write_prefix_config() -> Result<()> {
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

    let thread = supervisor.begin_task_child_thread(&mut session, &mut handler, start)?;

    assert!(
        thread.thread_id.as_str().starts_with("agent_v1_"),
        "worker should keep its guarded builtin scope instead of inheriting unguarded role config"
    );
    let profile = supervisor
        .registry()
        .get(&sigil_kernel::AgentProfileId::new("worker")?)
        .expect("worker profile exists");
    assert!(
        !profile
            .profile
            .tool_scope
            .prefixes
            .iter()
            .any(|prefix| prefix == "mcp__filesystem__")
    );
    Ok(())
}

#[test]
fn supervisor_allows_default_worker_changeset_only_foreground() -> Result<()> {
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

    let thread = supervisor.begin_task_child_thread(&mut session, &mut handler, start)?;

    assert!(thread.thread_id.as_str().starts_with("agent_v1_"));
    Ok(())
}

#[test]
fn supervisor_worker_scope_ignores_apply_changeset_config_widening() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut config = root_config();
    config.task.allow_write_subagents = true;
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
    start.step.mode = Some(sigil_kernel::TaskStepMode::Write);
    start.step.isolation = Some(sigil_kernel::TaskIsolationMode::ChangesetOnly);

    let thread = supervisor.begin_task_child_thread(&mut session, &mut handler, start)?;

    assert!(thread.thread_id.as_str().starts_with("agent_v1_"));
    let profile = supervisor
        .registry()
        .get(&sigil_kernel::AgentProfileId::new("worker")?)
        .expect("worker profile exists");
    assert!(!profile.profile.tool_scope.names.contains("apply_changeset"));
    Ok(())
}

#[test]
fn supervisor_allows_changeset_only_scoped_write_agents() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut config = root_config();
    config.task.allow_write_subagents = true;
    let scope = sigil_kernel::changeset_only_child_tool_scope();
    config.task.subagent_write.tools = sigil_kernel::ToolAllowlistConfig {
        allow_all: scope.allow_all,
        names: scope.names.into_iter().collect(),
        prefixes: scope.prefixes,
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
    start.step.mode = Some(sigil_kernel::TaskStepMode::Write);
    start.step.isolation = Some(sigil_kernel::TaskIsolationMode::ChangesetOnly);

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
        &AgentResultMaterialization::inline("done", None),
        &outcome,
        Some(usage.clone()),
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
async fn planner_postprocess_failure_marks_thread_failed_and_releases_slot() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut budget = AgentBudgetPolicy::from_root_config(&root_config());
    budget.max_subagents = 1;
    let supervisor = supervisor_with_budget(budget)?;
    let runner = AgentSupervisorTaskChildRunner::new_with_task_roles(
        supervisor.clone(),
        Agent::new(
            Box::new(TextProvider {
                text: "planner returned prose without committing a plan",
            }),
            ToolRegistry::new(),
        ),
        Agent::new(
            Box::new(TextProvider {
                text: "executor done",
            }),
            ToolRegistry::new(),
        ),
        Agent::new(
            Box::new(TextProvider {
                text: "reader done",
            }),
            ToolRegistry::new(),
        ),
        Agent::new(
            Box::new(TextProvider {
                text: "writer done",
            }),
            ToolRegistry::new(),
        ),
        Agent::new(
            Box::new(TextProvider {
                text: "synthesis done",
            }),
            ToolRegistry::new(),
        ),
    );
    let parent_store = JsonlSessionStore::new(temp.path().join("parent.jsonl"))?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(parent_store);
    let task_id = TaskId::new("task_planner_postprocess")?;
    let attempt_id =
        task_participant_attempt_id(&task_id, TaskParticipantPurpose::Planner, None, None, 1)?;
    let child_session_ref = task_participant_session_ref(&task_id, &attempt_id)?;
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;

    let error = runner
        .run_planner_session(
            &mut session,
            TaskPlannerSessionRunRequest {
                task: sigil_kernel::SequentialTaskRequest {
                    task_id: task_id.clone(),
                    parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                    objective: "produce a durable task plan".to_owned(),
                },
                attempt_id,
                child_session_ref,
                child_input: AgentRunInput::without_persisted_user_message(vec![
                    ModelMessage::user("plan the task"),
                ]),
                options: run_options(temp.path().to_path_buf()),
                discovery_options: run_options(temp.path().to_path_buf()),
            },
            &mut handler,
            &mut approval,
        )
        .await
        .expect_err("planner prose without task_plan_update must fail postprocessing");

    assert!(
        error
            .to_string()
            .contains("did not produce an accepted plan")
    );
    let projection = session.agent_thread_state_projection();
    let failed = projection
        .latest_thread()
        .expect("planner thread is projected");
    assert_eq!(failed.status, sigil_kernel::AgentThreadStatus::Failed);
    assert!(
        failed
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("did not produce an accepted plan"))
    );
    assert!(supervisor.active_profile_ids().is_empty());

    supervisor.begin_task_child_thread(
        &mut session,
        &mut handler,
        child_start(step("slot-reused")?, temp.path().to_path_buf())?,
    )?;
    assert_eq!(supervisor.active_profile_ids().len(), 1);
    Ok(())
}

#[tokio::test]
async fn planner_discovery_runs_bounded_probes_in_parallel_and_resumes_without_polling()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut budget = AgentBudgetPolicy::from_root_config(&root_config());
    budget.max_subagents = 4;
    let supervisor = supervisor_with_budget(budget)?;
    let observed_results = Arc::new(Mutex::new(None));
    let active = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));
    let mut explore_tools = ToolRegistry::new();
    explore_tools.register(Arc::new(ApprovalRouteTool));
    let runner = AgentSupervisorTaskChildRunner::new_with_task_roles(
        supervisor.clone(),
        Agent::new(
            Box::new(PlannerDiscoveryProvider {
                observed_results: Arc::clone(&observed_results),
            }),
            ToolRegistry::new(),
        ),
        Agent::new(
            Box::new(TextProvider {
                text: "executor done",
            }),
            ToolRegistry::new(),
        ),
        Agent::new(
            Box::new(ParallelDiscoveryProvider {
                barrier: Arc::new(tokio::sync::Barrier::new(2)),
                active: Arc::clone(&active),
                max_active: Arc::clone(&max_active),
            }),
            explore_tools,
        ),
        Agent::new(
            Box::new(TextProvider {
                text: "writer done",
            }),
            ToolRegistry::new(),
        ),
        Agent::new(
            Box::new(TextProvider {
                text: "synthesis done",
            }),
            ToolRegistry::new(),
        ),
    )
    .with_planner_discovery_policy(MultiAgentMode::ExplicitRequestOnly, 3);
    let parent_store = JsonlSessionStore::new(temp.path().join("parent.jsonl"))?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(parent_store);
    let task_id = TaskId::new("task_planner_discovery")?;
    let attempt_id =
        task_participant_attempt_id(&task_id, TaskParticipantPurpose::Planner, None, None, 1)?;
    let child_session_ref = task_participant_session_ref(&task_id, &attempt_id)?;
    let cancellation = RunCancellationOwner::new();
    let planner_input =
        AgentRunInput::without_persisted_user_message(vec![ModelMessage::user("plan the task")])
            .with_task_plan_update(TaskPlanUpdateContext {
                task_id: task_id.clone(),
                max_plan_steps: 12,
                max_plan_versions: 3,
            })
            .with_cancellation(cancellation.handle());
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;

    let output = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        runner.run_planner_session(
            &mut session,
            TaskPlannerSessionRunRequest {
                task: sigil_kernel::SequentialTaskRequest {
                    task_id: task_id.clone(),
                    parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                    objective: "inspect kernel and runtime before implementation".to_owned(),
                },
                attempt_id,
                child_session_ref,
                child_input: planner_input,
                options: run_options(temp.path().to_path_buf()),
                discovery_options: run_options(temp.path().to_path_buf()),
            },
            &mut handler,
            &mut approval,
        ),
    )
    .await
    .expect("planner discovery should complete without polling")?;

    assert_eq!(output.accepted_plan.plan_version, 1);
    assert_eq!(max_active.load(Ordering::SeqCst), 2);
    assert_eq!(active.load(Ordering::SeqCst), 0);
    let results = observed_results
        .lock()
        .expect("planner discovery observation lock should not be poisoned")
        .clone()
        .expect("planner should receive discovery results");
    let result_envelope: Value = serde_json::from_str(&results)?;
    let results: Value = serde_json::from_str(
        result_envelope["content"]
            .as_str()
            .expect("planner discovery result content should be a string"),
    )?;
    assert_eq!(results["type"], "task_discovery_results");
    assert!(
        results["batch_id"]
            .as_str()
            .is_some_and(|batch_id| batch_id.starts_with("discovery_"))
    );
    assert_eq!(results["members"][0]["probe_id"], "kernel");
    assert_eq!(results["members"][1]["probe_id"], "runtime");
    assert!(
        results["members"]
            .as_array()
            .is_some_and(|members| members.iter().all(|member| member["status"] == "completed"))
    );
    let projection = session.agent_thread_state_projection();
    assert_eq!(projection.threads.len(), 3);
    assert!(projection.threads.values().all(|thread| {
        thread.status == sigil_kernel::AgentThreadStatus::Completed
            && thread.result.as_ref().is_some_and(|result| {
                result.status == sigil_kernel::AgentThreadTerminalStatus::Completed
            })
    }));
    let batch = projection
        .batches
        .values()
        .next()
        .expect("planner discovery batch projection");
    assert_eq!(results["batch_id"].as_str(), Some(batch.batch_id.as_str()));
    let parent_thread_id = batch
        .parent_thread_id
        .as_ref()
        .expect("planner discovery batch should retain its planner parent");
    assert!(
        projection
            .threads
            .get(parent_thread_id)
            .is_some_and(|thread| thread.batch_id.is_none())
    );
    assert_eq!(batch.member_thread_ids.len(), 2);
    assert_eq!(
        batch
            .member_keys
            .keys()
            .map(AgentRouteId::as_str)
            .collect::<Vec<_>>(),
        vec!["kernel", "runtime"]
    );
    assert!(supervisor.active_profile_ids().is_empty());
    Ok(())
}

#[tokio::test]
async fn planner_discovery_rejects_overlapping_batch_before_any_provider_start() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut budget = AgentBudgetPolicy::from_root_config(&root_config());
    budget.max_subagents = 4;
    let supervisor = supervisor_with_budget(budget)?;
    let observed_error = Arc::new(Mutex::new(None));
    let starts = Arc::new(AtomicUsize::new(0));
    let mut explore_tools = ToolRegistry::new();
    explore_tools.register(Arc::new(ApprovalRouteTool));
    let runner = AgentSupervisorTaskChildRunner::new_with_task_roles(
        supervisor.clone(),
        Agent::new(
            Box::new(RejectedDiscoveryPlannerProvider {
                observed_error: Arc::clone(&observed_error),
            }),
            ToolRegistry::new(),
        ),
        Agent::new(
            Box::new(TextProvider {
                text: "executor done",
            }),
            ToolRegistry::new(),
        ),
        Agent::new(
            Box::new(CountingDiscoveryProvider {
                starts: Arc::clone(&starts),
            }),
            explore_tools,
        ),
        Agent::new(
            Box::new(TextProvider {
                text: "writer done",
            }),
            ToolRegistry::new(),
        ),
        Agent::new(
            Box::new(TextProvider {
                text: "synthesis done",
            }),
            ToolRegistry::new(),
        ),
    )
    .with_planner_discovery_policy(MultiAgentMode::ExplicitRequestOnly, 3);
    let parent_store = JsonlSessionStore::new(temp.path().join("parent.jsonl"))?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(parent_store);
    let task_id = TaskId::new("task_rejected_planner_discovery")?;
    let attempt_id =
        task_participant_attempt_id(&task_id, TaskParticipantPurpose::Planner, None, None, 1)?;
    let child_session_ref = task_participant_session_ref(&task_id, &attempt_id)?;
    let cancellation = RunCancellationOwner::new();
    let planner_input =
        AgentRunInput::without_persisted_user_message(vec![ModelMessage::user("plan the task")])
            .with_task_plan_update(TaskPlanUpdateContext {
                task_id: task_id.clone(),
                max_plan_steps: 12,
                max_plan_versions: 3,
            })
            .with_cancellation(cancellation.handle());
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;

    let output = runner
        .run_planner_session(
            &mut session,
            TaskPlannerSessionRunRequest {
                task: sigil_kernel::SequentialTaskRequest {
                    task_id,
                    parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                    objective: "inspect runtime before implementation".to_owned(),
                },
                attempt_id,
                child_session_ref,
                child_input: planner_input,
                options: run_options(temp.path().to_path_buf()),
                discovery_options: run_options(temp.path().to_path_buf()),
            },
            &mut handler,
            &mut approval,
        )
        .await?;

    assert_eq!(output.accepted_plan.plan_version, 1);
    assert_eq!(starts.load(Ordering::SeqCst), 0);
    assert!(
        observed_error
            .lock()
            .expect("planner discovery error observation lock should not be poisoned")
            .as_deref()
            .is_some_and(|error| error.contains("whole_batch_rejected"))
    );
    let projection = session.agent_thread_state_projection();
    assert_eq!(projection.threads.len(), 1);
    assert!(supervisor.active_profile_ids().is_empty());
    Ok(())
}

#[tokio::test]
async fn planner_discovery_allows_only_one_batch_per_planning_attempt() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut budget = AgentBudgetPolicy::from_root_config(&root_config());
    budget.max_subagents = 4;
    let supervisor = supervisor_with_budget(budget)?;
    let observed_rejection = Arc::new(Mutex::new(None));
    let starts = Arc::new(AtomicUsize::new(0));
    let mut explore_tools = ToolRegistry::new();
    explore_tools.register(Arc::new(ApprovalRouteTool));
    let runner = AgentSupervisorTaskChildRunner::new_with_task_roles(
        supervisor.clone(),
        Agent::new(
            Box::new(RepeatedDiscoveryPlannerProvider {
                observed_rejection: Arc::clone(&observed_rejection),
            }),
            ToolRegistry::new(),
        ),
        Agent::new(
            Box::new(TextProvider {
                text: "executor done",
            }),
            ToolRegistry::new(),
        ),
        Agent::new(
            Box::new(CountingDiscoveryProvider {
                starts: Arc::clone(&starts),
            }),
            explore_tools,
        ),
        Agent::new(
            Box::new(TextProvider {
                text: "writer done",
            }),
            ToolRegistry::new(),
        ),
        Agent::new(
            Box::new(TextProvider {
                text: "synthesis done",
            }),
            ToolRegistry::new(),
        ),
    )
    .with_planner_discovery_policy(MultiAgentMode::ExplicitRequestOnly, 3);
    let parent_store = JsonlSessionStore::new(temp.path().join("parent.jsonl"))?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(parent_store);
    let task_id = TaskId::new("task_repeated_planner_discovery")?;
    let attempt_id =
        task_participant_attempt_id(&task_id, TaskParticipantPurpose::Planner, None, None, 1)?;
    let child_session_ref = task_participant_session_ref(&task_id, &attempt_id)?;
    let cancellation = RunCancellationOwner::new();
    let planner_input =
        AgentRunInput::without_persisted_user_message(vec![ModelMessage::user("plan the task")])
            .with_task_plan_update(TaskPlanUpdateContext {
                task_id: task_id.clone(),
                max_plan_steps: 12,
                max_plan_versions: 3,
            })
            .with_cancellation(cancellation.handle());
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;

    let output = runner
        .run_planner_session(
            &mut session,
            TaskPlannerSessionRunRequest {
                task: sigil_kernel::SequentialTaskRequest {
                    task_id,
                    parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                    objective: "inspect runtime before implementation".to_owned(),
                },
                attempt_id,
                child_session_ref,
                child_input: planner_input,
                options: run_options(temp.path().to_path_buf()),
                discovery_options: run_options(temp.path().to_path_buf()),
            },
            &mut handler,
            &mut approval,
        )
        .await?;

    assert_eq!(output.accepted_plan.plan_version, 1);
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    assert!(
        observed_rejection
            .lock()
            .expect("planner discovery rejection lock should not be poisoned")
            .as_deref()
            .is_some_and(|rejection| rejection.contains("whole_batch_rejected"))
    );
    let projection = session.agent_thread_state_projection();
    assert_eq!(projection.threads.len(), 2);
    assert!(supervisor.active_profile_ids().is_empty());
    Ok(())
}

#[tokio::test]
async fn supervisor_records_post_run_usage_without_budget_warning() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let supervisor = supervisor_with_budget(AgentBudgetPolicy::from_root_config(&root_config()))?;
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
                attempt_id: participant_attempt_id_for("usage")?,
                child_session_ref: participant_session_ref_for("usage")?,
                child_input: AgentRunInput::without_persisted_user_message(vec![
                    ModelMessage::user("apply skill"),
                ]),
                options: run_options(temp.path().to_path_buf()),
                changeset_only_base_snapshot_id: None,
            },
            &mut handler,
            &mut approval,
        )
        .await?;

    assert_eq!(output.final_text, "too expensive");
    let projection = session.agent_thread_state_projection();
    let thread = projection.latest_thread().expect("child thread");
    assert_eq!(thread.status, sigil_kernel::AgentThreadStatus::Completed);
    assert_eq!(
        thread
            .result
            .as_ref()
            .and_then(|result| result.usage.as_ref())
            .map(|usage| usage.total_tokens),
        Some(13)
    );
    assert!(!handler.events.iter().any(|event| {
        matches!(event, RunEvent::Notice(message) if message.contains("agent budget warning"))
    }));
    Ok(())
}

#[tokio::test]
async fn task_read_batch_overlaps_provider_runs_and_commits_in_request_order() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let supervisor = supervisor_with_budget(AgentBudgetPolicy::from_root_config(&root_config()))?;
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let active = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));
    let completion_order = Arc::new(Mutex::new(Vec::new()));
    let runner = AgentSupervisorTaskChildRunner::new(
        supervisor.clone(),
        Agent::new(
            Box::new(ParallelTaskChildProvider {
                barrier,
                active: Arc::clone(&active),
                max_active: Arc::clone(&max_active),
                completion_order: Arc::clone(&completion_order),
            }),
            ToolRegistry::new(),
        ),
        Agent::new(
            Box::new(TextProvider {
                text: "writer done",
            }),
            ToolRegistry::new(),
        ),
    );
    let task = sigil_kernel::SequentialTaskRequest {
        task_id: TaskId::new("task_1")?,
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "inspect in parallel".to_owned(),
    };
    let requests = ["read_a", "read_b"]
        .into_iter()
        .map(|step_id| {
            Ok(TaskChildSessionRunRequest {
                task: task.clone(),
                plan_version: 1,
                step: step(step_id)?,
                attempt_id: participant_attempt_id_for(step_id)?,
                child_session_ref: participant_session_ref_for(step_id)?,
                child_input: AgentRunInput::without_persisted_user_message(vec![
                    ModelMessage::user(format!("inspect {step_id}")),
                ]),
                options: run_options(temp.path().to_path_buf()),
                changeset_only_base_snapshot_id: None,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let expected_attempts = requests
        .iter()
        .map(|request| request.attempt_id.clone())
        .collect::<Vec<_>>();
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;

    let outputs = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        runner.run_child_session_batch(&mut session, requests, &mut handler, &mut approval),
    )
    .await
    .expect("parallel provider barrier should complete")?;

    assert_eq!(outputs.len(), 2);
    assert_eq!(max_active.load(Ordering::SeqCst), 2);
    assert_eq!(
        *completion_order
            .lock()
            .expect("completion order should not be poisoned"),
        vec!["read_b", "read_a"]
    );
    assert_eq!(
        outputs
            .into_iter()
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .map(|output| output.attempt_id)
            .collect::<Vec<_>>(),
        expected_attempts
    );
    let projection = session.agent_thread_state_projection();
    assert_eq!(projection.threads.len(), 2);
    assert!(
        projection
            .threads
            .values()
            .all(|thread| thread.status == sigil_kernel::AgentThreadStatus::Completed)
    );
    assert_eq!(
        session
            .entries()
            .iter()
            .filter_map(|entry| match entry {
                SessionLogEntry::Control(sigil_kernel::ControlEntry::TaskChildSession(child))
                    if child.status == TaskChildSessionStatus::Completed =>
                {
                    Some(child.step_id.as_str())
                }
                _ => None,
            })
            .collect::<Vec<_>>(),
        vec!["read_a", "read_b"],
        "parent terminal commits should remain in stable request order"
    );
    assert!(supervisor.active_profile_ids().is_empty());
    Ok(())
}

#[tokio::test]
async fn supervisor_records_cumulative_agent_tokens_without_denial() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let supervisor = supervisor_with_budget(AgentBudgetPolicy::from_root_config(&root_config()))?;
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
                attempt_id: participant_attempt_id_for("usage_one")?,
                child_session_ref: participant_session_ref_for("usage_one")?,
                child_input: AgentRunInput::without_persisted_user_message(vec![
                    ModelMessage::user("apply skill"),
                ]),
                options: run_options(temp.path().to_path_buf()),
                changeset_only_base_snapshot_id: None,
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
                attempt_id: participant_attempt_id_for("usage_two")?,
                child_session_ref: participant_session_ref_for("usage_two")?,
                child_input: AgentRunInput::without_persisted_user_message(vec![
                    ModelMessage::user("apply skill again"),
                ]),
                options: run_options(temp.path().to_path_buf()),
                changeset_only_base_snapshot_id: None,
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
                step: step("usage_three")?,
                attempt_id: participant_attempt_id_for("usage_three")?,
                child_session_ref: participant_session_ref_for("usage_three")?,
                child_input: AgentRunInput::without_persisted_user_message(vec![
                    ModelMessage::user("apply skill after budget"),
                ]),
                options: run_options(temp.path().to_path_buf()),
                changeset_only_base_snapshot_id: None,
            },
            &mut handler,
            &mut approval,
        )
        .await?;

    let projection = session.agent_thread_state_projection();
    let completed_usage = projection
        .threads
        .values()
        .filter(|thread| thread.status == sigil_kernel::AgentThreadStatus::Completed)
        .filter_map(|thread| {
            thread
                .result
                .as_ref()
                .and_then(|result| result.usage.as_ref())
                .map(|usage| usage.total_tokens)
        })
        .collect::<Vec<_>>();
    assert_eq!(completed_usage, vec![13, 13, 13]);
    assert!(!projection.threads.values().any(|thread| {
        thread
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("token budget"))
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
                attempt_id: participant_attempt_id_for("inspect")?,
                child_session_ref: participant_session_ref_for("inspect")?,
                child_input: AgentRunInput::without_persisted_user_message(vec![
                    ModelMessage::user("inspect only"),
                ]),
                options: run_options(temp.path().to_path_buf()),
                changeset_only_base_snapshot_id: None,
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
        .next()
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
                attempt_id: participant_attempt_id_for("invoke_skill")?,
                child_session_ref: participant_session_ref_for("invoke_skill")?,
                child_input: AgentRunInput::without_persisted_user_message(vec![
                    ModelMessage::user("apply skill"),
                ]),
                options: run_options(temp.path().to_path_buf()),
                changeset_only_base_snapshot_id: None,
            },
            &mut handler,
            &mut approval,
        )
        .await?;

    assert_eq!(output.final_text, "child done");
    let agent_projection = session.agent_thread_state_projection();
    assert_eq!(agent_projection.threads.len(), 1);
    let task_projection = session.task_state_projection();
    let task = task_projection
        .tasks
        .get(&TaskId::new("task_1")?)
        .expect("task child session projected");
    assert_eq!(task.child_sessions.len(), 1);
    assert!(!handler.events.iter().any(|event| matches!(
        event,
        RunEvent::AssistantMessage(_) | RunEvent::TextDelta(_)
    )));
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
                attempt_id: participant_attempt_id_for("approval_route")?,
                child_session_ref: participant_session_ref_for("approval_route")?,
                child_input: AgentRunInput::without_persisted_user_message(vec![
                    ModelMessage::user("read through approval"),
                ]),
                options: run_options(temp.path().to_path_buf()),
                changeset_only_base_snapshot_id: None,
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
        participant_session_ref_for("approval_route")?
            .resolve(temp.path())
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
                attempt_id: participant_attempt_id_for("invoke_skill")?,
                child_session_ref: participant_session_ref_for("invoke_skill")?,
                child_input: AgentRunInput::without_persisted_user_message(vec![
                    ModelMessage::user("apply skill"),
                ]),
                options: run_options(temp.path().to_path_buf()),
                changeset_only_base_snapshot_id: None,
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
