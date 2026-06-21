use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
};

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    Agent, AgentApprovalRouteEntry, AgentInvocationMode, AgentInvocationSource, AgentProfileId,
    AgentRole, AgentRouteId, AgentRouteStatus, AgentRunOutcome, AgentThreadClosedEntry,
    AgentThreadId, AgentThreadMessageRoutedEntry, AgentThreadProjection, AgentThreadResult,
    AgentThreadStatus, AgentThreadTerminalStatus, AgentToolDelegate, AgentTrustState,
    AgentUsageSummary, ApprovalHandler, ControlEntry, EventHandler, JsonlSessionStore,
    ModelMessage, NoopEventHandler, Provider, RootConfig, RunEvent, Session, SessionLogEntry,
    SessionRef, TaskChildSessionStatus, TaskId, Tool, ToolAccess, ToolApproval, ToolCall,
    ToolCategory, ToolContext, ToolErrorKind, ToolPreview, ToolPreviewCapability, ToolRegistry,
    ToolResult, ToolResultMeta, ToolSpec, ToolSubject,
};

use crate::{
    AgentBudgetPolicy, AgentMailboxMessage, AgentProfileRegistry, AgentSupervisor,
    ResolvedAgentProfile, WORKER_PROFILE_ID, build_role_provider, build_role_run_options,
    build_role_tool_registry, chat_agent_thread_id_for_call,
};

pub const SPAWN_AGENT_TOOL_NAME: &str = "spawn_agent";
pub const WAIT_AGENT_TOOL_NAME: &str = "wait_agent";
pub const READ_AGENT_RESULT_TOOL_NAME: &str = "read_agent_result";
pub const MESSAGE_AGENT_TOOL_NAME: &str = "message_agent";
pub const CLOSE_AGENT_TOOL_NAME: &str = "close_agent";

const MAIN_THREAD_ID: &str = "main";
const DEFAULT_RESULT_SUMMARY_LIMIT: usize = 4_000;
const MIN_RESULT_SUMMARY_LIMIT: usize = 200;
const DEFAULT_RESULT_PAGE_LIMIT: usize = 4_000;
const MAX_RESULT_PAGE_LIMIT: usize = 12_000;

/// Registers the model-visible agent tool surface into a runtime tool registry.
///
/// The actual child-thread execution is handled by [`AgentToolRuntime`]. These tool
/// implementations provide stable schemas, permission subjects, previews, and a safe fallback
/// error if an entrypoint registers them without a delegation hook.
pub fn register_agent_tools(registry: &mut ToolRegistry, root_config: &RootConfig) -> Result<()> {
    let profile_registry = AgentProfileRegistry::from_root_config(root_config)?;
    let budget = AgentBudgetPolicy::from_root_config(root_config);
    register_agent_tools_with_registry(registry, profile_registry, budget)
}

pub fn register_agent_tools_with_workspace(
    registry: &mut ToolRegistry,
    root_config: &RootConfig,
    workspace_root: &Path,
) -> Result<()> {
    let profile_registry =
        AgentProfileRegistry::from_root_config_with_workspace(root_config, workspace_root)?;
    let budget = AgentBudgetPolicy::from_root_config(root_config);
    register_agent_tools_with_registry(registry, profile_registry, budget)
}

pub fn register_agent_tools_with_workspace_and_entries(
    registry: &mut ToolRegistry,
    root_config: &RootConfig,
    workspace_root: &Path,
    entries: &[SessionLogEntry],
) -> Result<()> {
    let profile_registry = AgentProfileRegistry::from_root_config_with_workspace_and_entries(
        root_config,
        workspace_root,
        entries,
    )?;
    let budget = AgentBudgetPolicy::from_root_config(root_config);
    register_agent_tools_with_registry(registry, profile_registry, budget)
}

pub fn register_agent_tools_with_registry(
    registry: &mut ToolRegistry,
    profile_registry: AgentProfileRegistry,
    budget: AgentBudgetPolicy,
) -> Result<()> {
    let index = profile_registry.model_visible_index(&Default::default())?;
    let surface = Arc::new(AgentToolSurface {
        profile_registry,
        budget,
        profile_index_description: profile_index_description(&index),
    });
    for kind in AgentToolKind::ALL {
        registry.register(Arc::new(AgentTool {
            kind,
            surface: Arc::clone(&surface),
        }));
    }
    Ok(())
}

/// Builds the same close result used by the model-visible `close_agent` tool.
#[must_use]
pub fn close_agent_thread(
    session: &Session,
    thread_id: AgentThreadId,
    reason: Option<String>,
) -> ToolResult {
    let thread_id_value = thread_id.as_str().to_owned();
    let args = match reason {
        Some(reason) => json!({
            "thread_id": thread_id_value,
            "reason": reason,
        }),
        None => json!({
            "thread_id": thread_id_value,
        }),
    };
    let call = ToolCall {
        id: format!("runtime-close-agent-{}", thread_id.as_str()),
        name: CLOSE_AGENT_TOOL_NAME.to_owned(),
        args_json: args.to_string(),
    };
    close_agent_from_args(session, &call, &args)
}

/// Runtime delegate that executes approved agent-thread tool calls.
pub struct AgentToolRuntime {
    supervisor: AgentSupervisor,
    root_config: RootConfig,
    base_registry: ToolRegistry,
    provider_factory: Arc<dyn AgentToolProviderFactory>,
    background_runs: BTreeMap<AgentThreadId, BackgroundChatAgentHandle>,
}

/// Result of a user-directed foreground agent invocation.
#[derive(Debug, Clone)]
pub struct ManualAgentInvocationResult {
    pub thread_id: AgentThreadId,
    pub result: Option<AgentThreadResult>,
}

struct BackgroundChatAgentHandle {
    thread: BackgroundChatAgentThreadRecord,
    handle: tokio::task::JoinHandle<Result<BackgroundChatAgentResult>>,
}

struct BackgroundChatAgentThreadRecord {
    thread_id: AgentThreadId,
    attempt_id: sigil_kernel::AgentRunAttemptId,
    profile_id: AgentProfileId,
    parent_thread_id: AgentThreadId,
    child_session_ref: SessionRef,
    budget_scope_id: TaskId,
}

impl BackgroundChatAgentThreadRecord {
    fn from_thread(thread: &crate::AgentChatChildThread) -> Self {
        Self {
            thread_id: thread.thread_id.clone(),
            attempt_id: thread.attempt_id.clone(),
            profile_id: thread.profile_id.clone(),
            parent_thread_id: thread.parent_thread_id.clone(),
            child_session_ref: thread.child_session_ref.clone(),
            budget_scope_id: thread.budget_scope_id.clone(),
        }
    }

    fn to_runtime_thread(&self) -> crate::AgentChatChildThread {
        crate::AgentChatChildThread {
            thread_id: self.thread_id.clone(),
            attempt_id: self.attempt_id.clone(),
            profile_id: self.profile_id.clone(),
            parent_thread_id: self.parent_thread_id.clone(),
            child_session_ref: self.child_session_ref.clone(),
            budget_scope_id: self.budget_scope_id.clone(),
            mailbox_rx: None,
        }
    }
}

struct BackgroundChatAgentResult {
    final_text: String,
    outcome: AgentRunOutcome,
    usage: AgentUsageSummary,
    status: TaskChildSessionStatus,
}

impl AgentToolRuntime {
    #[must_use]
    pub fn new(
        supervisor: AgentSupervisor,
        root_config: RootConfig,
        base_registry: ToolRegistry,
    ) -> Self {
        Self {
            supervisor,
            root_config,
            base_registry,
            provider_factory: Arc::new(DefaultAgentToolProviderFactory),
            background_runs: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn with_provider_factory(
        supervisor: AgentSupervisor,
        root_config: RootConfig,
        base_registry: ToolRegistry,
        provider_factory: Arc<dyn AgentToolProviderFactory>,
    ) -> Self {
        Self {
            supervisor,
            root_config,
            base_registry,
            provider_factory,
            background_runs: BTreeMap::new(),
        }
    }

    fn resolve_spawn_profile(&self, profile_id: &AgentProfileId) -> Result<ResolvedAgentProfile> {
        let resolved = self
            .supervisor
            .registry()
            .get(profile_id)
            .with_context(|| format!("agent profile {} is not registered", profile_id.as_str()))?;
        if !resolved.effective_enabled() {
            return Err(anyhow!("agent profile {} is disabled", profile_id.as_str()));
        }
        if resolved.trust_state != AgentTrustState::Trusted {
            return Err(anyhow!(
                "agent profile {} is not trusted",
                profile_id.as_str()
            ));
        }
        if !resolved.effective_model_invocation_allowed() {
            return Err(anyhow!(
                "agent profile {} is not model-invocable",
                profile_id.as_str()
            ));
        }
        Ok(resolved.clone())
    }

    fn resolve_manual_profile(&self, profile_id: &AgentProfileId) -> Result<ResolvedAgentProfile> {
        let resolved = self
            .supervisor
            .registry()
            .get(profile_id)
            .with_context(|| format!("agent profile {} is not registered", profile_id.as_str()))?;
        if !resolved.effective_enabled() {
            return Err(anyhow!("agent profile {} is disabled", profile_id.as_str()));
        }
        if resolved.trust_state != AgentTrustState::Trusted {
            return Err(anyhow!(
                "agent profile {} is not trusted",
                profile_id.as_str()
            ));
        }
        if !resolved.effective_user_invocation_allowed() {
            return Err(anyhow!(
                "agent profile {} is not user-invocable",
                profile_id.as_str()
            ));
        }
        Ok(resolved.clone())
    }

    pub async fn invoke_agent_profile(
        &self,
        session: &mut Session,
        profile_id: AgentProfileId,
        prompt: String,
        options: &sigil_kernel::AgentRunOptions,
        handler: &mut (dyn EventHandler + Send),
        approval_handler: &mut (dyn ApprovalHandler + Send),
    ) -> Result<ManualAgentInvocationResult> {
        let resolved_profile = self.resolve_manual_profile(&profile_id)?;
        let call_id = manual_agent_call_id(session, &profile_id, &prompt);
        let call = ToolCall {
            id: call_id,
            name: SPAWN_AGENT_TOOL_NAME.to_owned(),
            args_json: json!({
                "profile_id": profile_id.as_str(),
                "objective": &prompt,
                "prompt": &prompt,
                "mode": "join_before_final",
            })
            .to_string(),
        };
        let request = ChatAgentRunRequest {
            profile_id,
            objective: prompt.clone(),
            prompt,
            mode: AgentInvocationMode::JoinBeforeFinal,
            display_name_hint: None,
            invocation_source: AgentInvocationSource::Mention,
            resolved_profile,
        };
        let thread_id = self
            .run_chat_agent(session, &call, request, options, handler, approval_handler)
            .await?;
        let result = session
            .agent_thread_state_projection()
            .threads
            .get(&thread_id)
            .and_then(|thread| thread.result.clone());
        Ok(ManualAgentInvocationResult { thread_id, result })
    }
}

pub trait AgentToolProviderFactory: Send + Sync {
    fn build_provider(
        &self,
        root_config: &RootConfig,
        role: AgentRole,
        profile_id: &AgentProfileId,
    ) -> Result<Box<dyn Provider>>;
}

struct DefaultAgentToolProviderFactory;

impl AgentToolProviderFactory for DefaultAgentToolProviderFactory {
    fn build_provider(
        &self,
        root_config: &RootConfig,
        role: AgentRole,
        _profile_id: &AgentProfileId,
    ) -> Result<Box<dyn Provider>> {
        build_role_provider(root_config, role)
    }
}

#[async_trait]
impl AgentToolDelegate for AgentToolRuntime {
    async fn handle_agent_tool_call(
        &mut self,
        session: &mut Session,
        call: &ToolCall,
        options: &sigil_kernel::AgentRunOptions,
        handler: &mut (dyn EventHandler + Send),
        approval_handler: &mut (dyn ApprovalHandler + Send),
    ) -> Result<Option<ToolResult>> {
        let Some(kind) = AgentToolKind::from_name(&call.name) else {
            return Ok(None);
        };
        let args = parse_tool_args(call)?;
        let result = match kind {
            AgentToolKind::Spawn => {
                self.spawn_agent(session, call, &args, options, handler, approval_handler)
                    .await
            }
            AgentToolKind::Wait => self.wait_agent(session, call, &args, handler).await,
            AgentToolKind::ReadResult => self.read_agent_result(session, call, &args),
            AgentToolKind::Message => self.message_agent(session, call, &args),
            AgentToolKind::Close => self.close_agent(session, call, &args),
        };
        Ok(Some(result))
    }
}

impl AgentToolRuntime {
    async fn spawn_agent(
        &mut self,
        session: &mut Session,
        call: &ToolCall,
        args: &Value,
        options: &sigil_kernel::AgentRunOptions,
        handler: &mut (dyn EventHandler + Send),
        approval_handler: &mut (dyn ApprovalHandler + Send),
    ) -> ToolResult {
        let parsed = match SpawnAgentArgs::parse(args) {
            Ok(parsed) => parsed,
            Err(error) => {
                return ToolResult::error(
                    call.id.clone(),
                    call.name.clone(),
                    ToolErrorKind::InvalidInput,
                    error.to_string(),
                );
            }
        };
        let role = role_for_profile_id(&parsed.profile_id);
        let resolved_profile = match self.resolve_spawn_profile(&parsed.profile_id) {
            Ok(profile) => profile,
            Err(error) => {
                return ToolResult::error(
                    call.id.clone(),
                    call.name.clone(),
                    ToolErrorKind::PermissionDenied,
                    format!("{error:#}"),
                );
            }
        };
        let profile_tool_scope = resolved_profile.profile.tool_scope.clone();
        let child_provider =
            match self
                .provider_factory
                .build_provider(&self.root_config, role, &parsed.profile_id)
            {
                Ok(provider) => provider,
                Err(error) => {
                    return ToolResult::error(
                        call.id.clone(),
                        call.name.clone(),
                        ToolErrorKind::Internal,
                        format!("failed to build child agent provider: {error:#}"),
                    );
                }
            };
        let child_capabilities = child_provider.capabilities();
        let thread_id = match chat_agent_thread_id_for_call(&call.id, &parsed.profile_id) {
            Ok(thread_id) => thread_id,
            Err(error) => {
                return ToolResult::error(
                    call.id.clone(),
                    call.name.clone(),
                    ToolErrorKind::InvalidInput,
                    error.to_string(),
                );
            }
        };
        let parent_session_ref = match parent_session_ref(session) {
            Ok(reference) => reference,
            Err(error) => {
                return ToolResult::error(
                    call.id.clone(),
                    call.name.clone(),
                    ToolErrorKind::Internal,
                    error.to_string(),
                );
            }
        };
        let child_session_ref = match agent_child_session_ref(&thread_id) {
            Ok(reference) => reference,
            Err(error) => {
                return ToolResult::error(
                    call.id.clone(),
                    call.name.clone(),
                    ToolErrorKind::Internal,
                    error.to_string(),
                );
            }
        };
        let budget_scope_id = match chat_budget_scope_id(&call.id) {
            Ok(task_id) => task_id,
            Err(error) => {
                return ToolResult::error(
                    call.id.clone(),
                    call.name.clone(),
                    ToolErrorKind::Internal,
                    error.to_string(),
                );
            }
        };
        let mut child_thread = match self.supervisor.begin_chat_child_thread(
            session,
            handler,
            crate::AgentChatChildStart {
                call_id: call.id.clone(),
                budget_scope_id: budget_scope_id.clone(),
                parent_thread_id: match AgentThreadId::new(MAIN_THREAD_ID) {
                    Ok(thread_id) => thread_id,
                    Err(error) => {
                        return ToolResult::error(
                            call.id.clone(),
                            call.name.clone(),
                            ToolErrorKind::Internal,
                            error.to_string(),
                        );
                    }
                },
                parent_depth: 0,
                parent_session_ref,
                profile_id: parsed.profile_id.clone(),
                role,
                child_session_ref: child_session_ref.clone(),
                objective: parsed.objective.clone(),
                prompt: parsed.prompt.clone(),
                workspace_root: options.workspace_root.clone(),
                provider_capabilities: child_capabilities,
                invocation_mode: parsed.mode,
                invocation_source: AgentInvocationSource::Chat,
                display_name_hint: parsed.display_name_hint.clone(),
            },
        ) {
            Ok(thread) => thread,
            Err(error) => {
                return ToolResult::error(
                    call.id.clone(),
                    call.name.clone(),
                    ToolErrorKind::PermissionDenied,
                    format!("{error:#}"),
                );
            }
        };

        let mut child_session = match build_agent_child_session(session, &child_session_ref) {
            Ok(session) => session,
            Err(error) => {
                let _ = self.supervisor.record_chat_child_failure(
                    session,
                    handler,
                    &child_thread,
                    format!("{error:#}"),
                );
                return ToolResult::error(
                    call.id.clone(),
                    call.name.clone(),
                    ToolErrorKind::Internal,
                    error.to_string(),
                );
            }
        };
        let child_registry = build_role_tool_registry(&self.base_registry, &self.root_config, role)
            .into_registry()
            .scoped(profile_tool_scope)
            .into_registry();
        let child_agent = Agent::new(child_provider, child_registry);
        let mut child_messages = Vec::new();
        if let Some(system_prompt) = agent_profile_system_prompt(&resolved_profile) {
            child_messages.push(ModelMessage::system(system_prompt));
        }
        child_messages.push(ModelMessage::user(parsed.prompt.clone()));
        let child_input =
            sigil_kernel::AgentRunInput::without_persisted_user_message(child_messages);
        let child_options = build_role_run_options(
            &self.root_config,
            options.workspace_root.clone(),
            options.interaction_mode,
            role,
        );

        if matches!(parsed.mode, AgentInvocationMode::Background) {
            let Some(mailbox_rx) = child_thread.mailbox_rx.take() else {
                let _ = self.supervisor.record_chat_child_failure(
                    session,
                    handler,
                    &child_thread,
                    "background agent mailbox was not created".to_owned(),
                );
                return ToolResult::error(
                    call.id.clone(),
                    call.name.clone(),
                    ToolErrorKind::Internal,
                    "background agent mailbox was not created",
                );
            };
            let thread_id = child_thread.thread_id.clone();
            let handle = tokio::spawn(run_background_chat_agent(
                child_agent,
                child_session,
                child_input,
                child_options,
                mailbox_rx,
            ));
            self.background_runs.insert(
                thread_id.clone(),
                BackgroundChatAgentHandle {
                    thread: BackgroundChatAgentThreadRecord::from_thread(&child_thread),
                    handle,
                },
            );
            let projection = session.agent_thread_state_projection();
            if let Some(thread) = projection.threads.get(&thread_id) {
                return agent_status_tool_result(call, thread);
            }
            return ToolResult::ok(
                call.id.clone(),
                call.name.clone(),
                format!("agent thread {} is running", thread_id.as_str()),
                ToolResultMeta {
                    details: json!({
                        "thread_id": thread_id.as_str(),
                        "status": "running"
                    }),
                    ..ToolResultMeta::default()
                },
            );
        }

        let _thread_guard = ChatChildThreadGuard {
            supervisor: self.supervisor.clone(),
            thread_id: child_thread.thread_id.clone(),
        };
        let output = {
            let mut child_handler = ForwardEventHandler { inner: handler };
            let mut route_handler = ChatAgentApprovalRouteHandler {
                inner: approval_handler,
                parent_session: session,
                source_thread_id: child_thread.thread_id.clone(),
            };
            child_agent
                .run_with_approval_input(
                    &mut child_session,
                    child_input,
                    child_options,
                    &mut child_handler,
                    &mut route_handler,
                )
                .await
        };
        let output = match output {
            Ok(output) => output,
            Err(error) => {
                let _ = self.supervisor.record_chat_child_failure(
                    session,
                    handler,
                    &child_thread,
                    format!("{error:#}"),
                );
                return ToolResult::error(
                    call.id.clone(),
                    call.name.clone(),
                    ToolErrorKind::Internal,
                    format!("child agent failed: {error:#}"),
                );
            }
        };
        let final_text = output.result.final_text;
        let outcome = output.outcome;
        let usage = usage_summary_from_stats(child_session.stats());
        let budget_warning = self
            .supervisor
            .validate_usage_budget(&budget_scope_id, &usage)
            .err()
            .map(|error| format!("{error:#}"));
        let status = child_status_from_outcome(&final_text, &outcome);
        if let Err(error) = self.supervisor.record_chat_child_result(
            session,
            handler,
            &child_thread,
            status,
            &final_text,
            &outcome,
            Some(usage),
        ) {
            return ToolResult::error(
                call.id.clone(),
                call.name.clone(),
                ToolErrorKind::Internal,
                error.to_string(),
            );
        }
        if let Some(warning) = budget_warning {
            let _ = handler.handle(RunEvent::Notice(format!(
                "agent budget warning after child completion: {warning}"
            )));
        }
        let result = session
            .agent_thread_state_projection()
            .threads
            .get(&child_thread.thread_id)
            .and_then(|thread| thread.result.clone());
        agent_result_tool_result(
            call,
            &child_thread.thread_id,
            result.as_ref(),
            DEFAULT_RESULT_SUMMARY_LIMIT,
        )
    }

    async fn run_chat_agent(
        &self,
        session: &mut Session,
        call: &ToolCall,
        request: ChatAgentRunRequest,
        options: &sigil_kernel::AgentRunOptions,
        handler: &mut (dyn EventHandler + Send),
        approval_handler: &mut (dyn ApprovalHandler + Send),
    ) -> Result<AgentThreadId> {
        let role = role_for_profile_id(&request.profile_id);
        if matches!(request.mode, AgentInvocationMode::Background) {
            return Err(anyhow!(
                "background agent mode requires provider-backed agent mailbox support"
            ));
        }

        let profile_tool_scope = request.resolved_profile.profile.tool_scope.clone();
        let child_provider = self
            .provider_factory
            .build_provider(&self.root_config, role, &request.profile_id)
            .with_context(|| {
                format!(
                    "failed to build child agent provider for {}",
                    request.profile_id.as_str()
                )
            })?;
        let child_capabilities = child_provider.capabilities();
        let thread_id = chat_agent_thread_id_for_call(&call.id, &request.profile_id)?;
        let parent_session_ref = parent_session_ref(session)?;
        let child_session_ref = agent_child_session_ref(&thread_id)?;
        let budget_scope_id = chat_budget_scope_id(&call.id)?;
        let parent_thread_id = AgentThreadId::new(MAIN_THREAD_ID)?;
        let child_thread = self.supervisor.begin_chat_child_thread(
            session,
            handler,
            crate::AgentChatChildStart {
                call_id: call.id.clone(),
                budget_scope_id: budget_scope_id.clone(),
                parent_thread_id,
                parent_depth: 0,
                parent_session_ref,
                profile_id: request.profile_id.clone(),
                role,
                child_session_ref: child_session_ref.clone(),
                objective: request.objective.clone(),
                prompt: request.prompt.clone(),
                workspace_root: options.workspace_root.clone(),
                provider_capabilities: child_capabilities,
                invocation_mode: request.mode,
                invocation_source: request.invocation_source,
                display_name_hint: request.display_name_hint.clone(),
            },
        )?;
        let _thread_guard = ChatChildThreadGuard {
            supervisor: self.supervisor.clone(),
            thread_id: child_thread.thread_id.clone(),
        };

        let mut child_session = match build_agent_child_session(session, &child_session_ref) {
            Ok(session) => session,
            Err(error) => {
                let _ = self.supervisor.record_chat_child_failure(
                    session,
                    handler,
                    &child_thread,
                    format!("{error:#}"),
                );
                return Err(error);
            }
        };
        let child_registry = build_role_tool_registry(&self.base_registry, &self.root_config, role)
            .into_registry()
            .scoped(profile_tool_scope)
            .into_registry();
        let child_agent = Agent::new(child_provider, child_registry);
        let mut child_messages = Vec::new();
        if let Some(system_prompt) = agent_profile_system_prompt(&request.resolved_profile) {
            child_messages.push(ModelMessage::system(system_prompt));
        }
        child_messages.push(ModelMessage::user(request.prompt.clone()));
        let child_input =
            sigil_kernel::AgentRunInput::without_persisted_user_message(child_messages);
        let child_options = build_role_run_options(
            &self.root_config,
            options.workspace_root.clone(),
            options.interaction_mode,
            role,
        );
        let output = {
            let mut child_handler = ForwardEventHandler { inner: handler };
            let mut route_handler = ChatAgentApprovalRouteHandler {
                inner: approval_handler,
                parent_session: session,
                source_thread_id: child_thread.thread_id.clone(),
            };
            child_agent
                .run_with_approval_input(
                    &mut child_session,
                    child_input,
                    child_options,
                    &mut child_handler,
                    &mut route_handler,
                )
                .await
        };
        let output = match output {
            Ok(output) => output,
            Err(error) => {
                let _ = self.supervisor.record_chat_child_failure(
                    session,
                    handler,
                    &child_thread,
                    format!("{error:#}"),
                );
                return Err(error).context("child agent failed");
            }
        };
        let final_text = output.result.final_text;
        let outcome = output.outcome;
        let usage = usage_summary_from_stats(child_session.stats());
        let budget_warning = self
            .supervisor
            .validate_usage_budget(&budget_scope_id, &usage)
            .err()
            .map(|error| format!("{error:#}"));
        let status = child_status_from_outcome(&final_text, &outcome);
        self.supervisor.record_chat_child_result(
            session,
            handler,
            &child_thread,
            status,
            &final_text,
            &outcome,
            Some(usage),
        )?;
        if let Some(warning) = budget_warning {
            let _ = handler.handle(RunEvent::Notice(format!(
                "agent budget warning after child completion: {warning}"
            )));
        }
        Ok(child_thread.thread_id)
    }

    async fn wait_agent(
        &mut self,
        session: &mut Session,
        call: &ToolCall,
        args: &Value,
        handler: &mut (dyn EventHandler + Send),
    ) -> ToolResult {
        let thread_id = match thread_id_arg(args) {
            Ok(thread_id) => thread_id,
            Err(error) => {
                return ToolResult::error(
                    call.id.clone(),
                    call.name.clone(),
                    ToolErrorKind::InvalidInput,
                    error.to_string(),
                );
            }
        };
        if let Some(background) = self.background_runs.get(&thread_id)
            && !background.handle.is_finished()
        {
            let projection = session.agent_thread_state_projection();
            if let Some(thread) = projection.threads.get(&thread_id) {
                return agent_status_tool_result(call, thread);
            }
        }
        if self
            .background_runs
            .get(&thread_id)
            .is_some_and(|background| background.handle.is_finished())
            && let Some(background) = self.background_runs.remove(&thread_id)
        {
            let thread = background.thread.to_runtime_thread();
            match background.handle.await {
                Ok(Ok(output)) => {
                    let budget_warning = self
                        .supervisor
                        .validate_usage_budget(&thread.budget_scope_id, &output.usage)
                        .err()
                        .map(|error| format!("{error:#}"));
                    if let Err(error) = self.supervisor.record_chat_child_result(
                        session,
                        handler,
                        &thread,
                        output.status,
                        &output.final_text,
                        &output.outcome,
                        Some(output.usage),
                    ) {
                        return ToolResult::error(
                            call.id.clone(),
                            call.name.clone(),
                            ToolErrorKind::Internal,
                            error.to_string(),
                        );
                    }
                    if let Some(warning) = budget_warning {
                        let _ = handler.handle(RunEvent::Notice(format!(
                            "agent budget warning after child completion: {warning}"
                        )));
                    }
                }
                Ok(Err(error)) => {
                    let reason = format!("{error:#}");
                    let _ = self.supervisor.record_chat_child_failure(
                        session,
                        handler,
                        &thread,
                        reason.clone(),
                    );
                    return ToolResult::error(
                        call.id.clone(),
                        call.name.clone(),
                        ToolErrorKind::Internal,
                        format!("background child agent failed: {reason}"),
                    );
                }
                Err(error) => {
                    let reason = format!("background child agent join failed: {error}");
                    let _ = self.supervisor.record_chat_child_failure(
                        session,
                        handler,
                        &thread,
                        reason.clone(),
                    );
                    return ToolResult::error(
                        call.id.clone(),
                        call.name.clone(),
                        ToolErrorKind::Internal,
                        reason,
                    );
                }
            }
        }
        let projection = session.agent_thread_state_projection();
        let Some(thread) = projection.threads.get(&thread_id) else {
            return ToolResult::error(
                call.id.clone(),
                call.name.clone(),
                ToolErrorKind::NotFound,
                format!("agent thread {} was not found", thread_id.as_str()),
            );
        };
        agent_status_tool_result(call, thread)
    }

    fn read_agent_result(&self, session: &Session, call: &ToolCall, args: &Value) -> ToolResult {
        let thread_id = match thread_id_arg(args) {
            Ok(thread_id) => thread_id,
            Err(error) => {
                return ToolResult::error(
                    call.id.clone(),
                    call.name.clone(),
                    ToolErrorKind::InvalidInput,
                    error.to_string(),
                );
            }
        };
        let result_page_request = match required_result_page_request_arg(args) {
            Ok(request) => request,
            Err(error) => {
                return ToolResult::error(
                    call.id.clone(),
                    call.name.clone(),
                    ToolErrorKind::InvalidInput,
                    error.to_string(),
                );
            }
        };
        let projection = session.agent_thread_state_projection();
        let Some(thread) = projection.threads.get(&thread_id) else {
            return ToolResult::error(
                call.id.clone(),
                call.name.clone(),
                ToolErrorKind::NotFound,
                format!("agent thread {} was not found", thread_id.as_str()),
            );
        };
        let Some(result) = thread.result.as_ref() else {
            return ToolResult::error(
                call.id.clone(),
                call.name.clone(),
                ToolErrorKind::Unsupported,
                format!(
                    "agent thread {} has no terminal result yet",
                    thread_id.as_str()
                ),
            );
        };
        let result_page = match read_agent_result_page(session, result, result_page_request) {
            Ok(page) => page,
            Err(error) => {
                return ToolResult::error(
                    call.id.clone(),
                    call.name.clone(),
                    ToolErrorKind::Internal,
                    error.to_string(),
                );
            }
        };
        agent_result_page_tool_result(call, result, &result_page)
    }

    fn message_agent(&self, session: &Session, call: &ToolCall, args: &Value) -> ToolResult {
        let thread_id = match thread_id_arg(args) {
            Ok(thread_id) => thread_id,
            Err(error) => {
                return ToolResult::error(
                    call.id.clone(),
                    call.name.clone(),
                    ToolErrorKind::InvalidInput,
                    error.to_string(),
                );
            }
        };
        let prompt = match required_string(args, "prompt") {
            Ok(prompt) => prompt,
            Err(error) => {
                return ToolResult::error(
                    call.id.clone(),
                    call.name.clone(),
                    ToolErrorKind::InvalidInput,
                    error.to_string(),
                );
            }
        };
        let projection = session.agent_thread_state_projection();
        let Some(thread) = projection.threads.get(&thread_id) else {
            return ToolResult::error(
                call.id.clone(),
                call.name.clone(),
                ToolErrorKind::NotFound,
                format!("agent thread {} was not found", thread_id.as_str()),
            );
        };
        let route_id = match agent_route_id_for_call(&thread_id, &call.id) {
            Ok(route_id) => route_id,
            Err(error) => {
                return ToolResult::error(
                    call.id.clone(),
                    call.name.clone(),
                    ToolErrorKind::Internal,
                    error.to_string(),
                );
            }
        };
        let source_thread_id = match AgentThreadId::new(MAIN_THREAD_ID) {
            Ok(thread_id) => thread_id,
            Err(error) => {
                return ToolResult::error(
                    call.id.clone(),
                    call.name.clone(),
                    ToolErrorKind::Internal,
                    error.to_string(),
                );
            }
        };
        let prompt_hash = hash_text(&prompt);
        let requested = AgentThreadMessageRoutedEntry {
            route_id: route_id.clone(),
            source_thread_id: source_thread_id.clone(),
            target_thread_id: thread_id.clone(),
            prompt_hash: prompt_hash.clone(),
            prompt: Some(prompt.clone()),
            status: AgentRouteStatus::Requested,
        };
        let delivery = if thread.status.is_terminal() {
            Err(format!(
                "agent thread {} is {}",
                thread_id.as_str(),
                thread_status_label(thread.status)
            ))
        } else {
            self.supervisor.send_agent_message(
                &thread_id,
                AgentMailboxMessage {
                    route_id: route_id.clone(),
                    prompt: prompt.clone(),
                },
            )
        };
        match delivery {
            Ok(()) => ToolResult::ok(
                call.id.clone(),
                call.name.clone(),
                format!("message queued for agent thread {}", thread_id.as_str()),
                ToolResultMeta {
                    details: json!({
                        "thread_id": thread_id.as_str(),
                        "route_id": route_id.as_str(),
                        "status": "resolved"
                    }),
                    ..ToolResultMeta::default()
                },
            )
            .with_control_entry(ControlEntry::AgentThreadMessageRouted(requested))
            .with_control_entry(ControlEntry::AgentThreadMessageRouted(
                AgentThreadMessageRoutedEntry {
                    route_id,
                    source_thread_id,
                    target_thread_id: thread_id,
                    prompt_hash,
                    prompt: None,
                    status: AgentRouteStatus::Resolved,
                },
            )),
            Err(reason) => ToolResult::error(
                call.id.clone(),
                call.name.clone(),
                ToolErrorKind::Unsupported,
                format!(
                    "agent thread {} cannot accept live messages: {}",
                    thread_id.as_str(),
                    reason
                ),
            )
            .with_control_entry(ControlEntry::AgentThreadMessageRouted(requested))
            .with_control_entry(ControlEntry::AgentThreadMessageRouted(
                AgentThreadMessageRoutedEntry {
                    route_id,
                    source_thread_id,
                    target_thread_id: thread_id,
                    prompt_hash,
                    prompt: None,
                    status: AgentRouteStatus::Rejected,
                },
            )),
        }
    }

    fn close_agent(&self, session: &Session, call: &ToolCall, args: &Value) -> ToolResult {
        close_agent_from_args(session, call, args)
    }
}

fn close_agent_from_args(session: &Session, call: &ToolCall, args: &Value) -> ToolResult {
    let thread_id = match thread_id_arg(args) {
        Ok(thread_id) => thread_id,
        Err(error) => {
            return ToolResult::error(
                call.id.clone(),
                call.name.clone(),
                ToolErrorKind::InvalidInput,
                error.to_string(),
            );
        }
    };
    let projection = session.agent_thread_state_projection();
    let Some(thread) = projection.threads.get(&thread_id) else {
        return ToolResult::error(
            call.id.clone(),
            call.name.clone(),
            ToolErrorKind::NotFound,
            format!("agent thread {} was not found", thread_id.as_str()),
        );
    };
    if !thread.status.is_terminal() {
        return ToolResult::error(
            call.id.clone(),
            call.name.clone(),
            ToolErrorKind::Unsupported,
            format!(
                "agent thread {} is {}; close_agent only closes terminal threads",
                thread_id.as_str(),
                thread_status_label(thread.status)
            ),
        );
    }
    let reason = optional_string(args, "reason");
    ToolResult::ok(
        call.id.clone(),
        call.name.clone(),
        format!("agent thread {} closed", thread_id.as_str()),
        ToolResultMeta::default(),
    )
    .with_control_entry(ControlEntry::AgentThreadClosed(AgentThreadClosedEntry {
        thread_id,
        reason,
    }))
}

struct AgentToolSurface {
    profile_registry: AgentProfileRegistry,
    budget: AgentBudgetPolicy,
    profile_index_description: String,
}

#[derive(Debug, Clone, Copy)]
enum AgentToolKind {
    Spawn,
    Wait,
    ReadResult,
    Message,
    Close,
}

impl AgentToolKind {
    const ALL: [Self; 5] = [
        Self::Spawn,
        Self::Wait,
        Self::ReadResult,
        Self::Message,
        Self::Close,
    ];

    fn from_name(name: &str) -> Option<Self> {
        match name {
            SPAWN_AGENT_TOOL_NAME => Some(Self::Spawn),
            WAIT_AGENT_TOOL_NAME => Some(Self::Wait),
            READ_AGENT_RESULT_TOOL_NAME => Some(Self::ReadResult),
            MESSAGE_AGENT_TOOL_NAME => Some(Self::Message),
            CLOSE_AGENT_TOOL_NAME => Some(Self::Close),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Spawn => SPAWN_AGENT_TOOL_NAME,
            Self::Wait => WAIT_AGENT_TOOL_NAME,
            Self::ReadResult => READ_AGENT_RESULT_TOOL_NAME,
            Self::Message => MESSAGE_AGENT_TOOL_NAME,
            Self::Close => CLOSE_AGENT_TOOL_NAME,
        }
    }
}

struct AgentTool {
    kind: AgentToolKind,
    surface: Arc<AgentToolSurface>,
}

#[async_trait]
impl Tool for AgentTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.kind.name().to_owned(),
            description: self.description(),
            input_schema: self.input_schema(),
            category: ToolCategory::Agent,
            access: match self.kind {
                AgentToolKind::Wait | AgentToolKind::ReadResult => ToolAccess::Read,
                AgentToolKind::Spawn | AgentToolKind::Message | AgentToolKind::Close => {
                    ToolAccess::Execute
                }
            },
            preview: match self.kind {
                AgentToolKind::Spawn => ToolPreviewCapability::Required,
                AgentToolKind::Wait
                | AgentToolKind::ReadResult
                | AgentToolKind::Message
                | AgentToolKind::Close => ToolPreviewCapability::Optional,
            },
        }
    }

    fn permission_subjects(&self, _ctx: &ToolContext, args: &Value) -> Result<Vec<ToolSubject>> {
        let subject = match self.kind {
            AgentToolKind::Spawn => ToolSubject::agent(required_string(args, "profile_id")?),
            AgentToolKind::Wait
            | AgentToolKind::ReadResult
            | AgentToolKind::Message
            | AgentToolKind::Close => ToolSubject::agent(required_string(args, "thread_id")?),
        };
        Ok(vec![subject])
    }

    fn permission_default_mode(
        &self,
        _ctx: &ToolContext,
        _args: &Value,
    ) -> Result<Option<sigil_kernel::ApprovalMode>> {
        Ok(match self.kind {
            AgentToolKind::Spawn | AgentToolKind::Message | AgentToolKind::Close => {
                Some(sigil_kernel::ApprovalMode::Ask)
            }
            AgentToolKind::Wait | AgentToolKind::ReadResult => {
                Some(sigil_kernel::ApprovalMode::Allow)
            }
        })
    }

    async fn preview(&self, _ctx: ToolContext, args: Value) -> Result<Option<ToolPreview>> {
        Ok(match self.kind {
            AgentToolKind::Spawn => Some(self.spawn_preview(&args)?),
            AgentToolKind::Wait => Some(simple_agent_preview(
                "Wait for agent",
                &format!("thread {}", required_string(&args, "thread_id")?),
            )),
            AgentToolKind::ReadResult => Some(simple_agent_preview(
                "Read agent result",
                &format!("thread {}", required_string(&args, "thread_id")?),
            )),
            AgentToolKind::Message => Some(simple_agent_preview(
                "Message agent",
                &format!("thread {}", required_string(&args, "thread_id")?),
            )),
            AgentToolKind::Close => Some(simple_agent_preview(
                "Close agent",
                &format!("thread {}", required_string(&args, "thread_id")?),
            )),
        })
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        call_id: String,
        _args: Value,
    ) -> Result<ToolResult> {
        Ok(ToolResult::error(
            call_id,
            self.kind.name(),
            ToolErrorKind::Unsupported,
            "agent tools require a runtime agent delegation handler",
        ))
    }
}

impl AgentTool {
    fn description(&self) -> String {
        match self.kind {
            AgentToolKind::Spawn => format!(
                "Spawn a child agent when the user explicitly asks for delegated, parallel, sub-agent, or child-agent work. You must delegate the requested non-overlapping scope instead of completing that same scope yourself, and use join_before_final when the child result is needed before your final answer. Use stable profile_id values, not display names.\n{}",
                self.surface.profile_index_description
            ),
            AgentToolKind::Wait => {
                "Wait for an agent thread status update and return lightweight status plus result references only. Does not return child result text; use read_agent_result when the user explicitly needs result details."
                    .to_owned()
            }
            AgentToolKind::ReadResult => {
                "Explicitly read a bounded page from a completed child agent final answer. Use only when the parent needs details beyond the bounded agent summary; do not request full child transcripts."
                    .to_owned()
            }
            AgentToolKind::Message => {
                "Send follow-up instructions to an active background child agent mailbox. Use only for steering an already spawned agent thread; wait_agent is still required to collect terminal results."
                    .to_owned()
            }
            AgentToolKind::Close => {
                "Close a completed, failed, cancelled, or interrupted agent thread.".to_owned()
            }
        }
    }

    fn input_schema(&self) -> Value {
        match self.kind {
            AgentToolKind::Spawn => json!({
                "type": "object",
                "properties": {
                    "profile_id": {
                        "type": "string",
                        "description": "Stable agent profile id from the model-visible agent index."
                    },
                    "objective": { "type": "string" },
                    "prompt": { "type": "string" },
                    "mode": {
                        "type": "string",
                        "enum": ["foreground", "join_before_final", "background"],
                        "default": "join_before_final"
                    },
                    "display_name_hint": { "type": "string" }
                },
                "required": ["profile_id", "objective", "prompt"],
                "additionalProperties": false
            }),
            AgentToolKind::Wait => json!({
                "type": "object",
                "properties": {
                    "thread_id": { "type": "string" }
                },
                "required": ["thread_id"],
                "additionalProperties": false
            }),
            AgentToolKind::ReadResult => json!({
                "type": "object",
                "properties": {
                    "thread_id": { "type": "string" },
                    "offset_chars": {
                        "type": "integer",
                        "default": 0,
                        "minimum": 0,
                        "description": "Character offset into the child agent final answer."
                    },
                    "max_chars": {
                        "type": "integer",
                        "minimum": 200,
                        "maximum": 12000,
                        "default": 4000,
                        "description": "Maximum characters to return from the child agent final answer."
                    }
                },
                "required": ["thread_id"],
                "additionalProperties": false
            }),
            AgentToolKind::Message => json!({
                "type": "object",
                "properties": {
                    "thread_id": { "type": "string" },
                    "prompt": { "type": "string" }
                },
                "required": ["thread_id", "prompt"],
                "additionalProperties": false
            }),
            AgentToolKind::Close => json!({
                "type": "object",
                "properties": {
                    "thread_id": { "type": "string" },
                    "reason": { "type": "string" }
                },
                "required": ["thread_id"],
                "additionalProperties": false
            }),
        }
    }

    fn spawn_preview(&self, args: &Value) -> Result<ToolPreview> {
        let parsed = SpawnAgentArgs::parse(args)?;
        let resolved = self
            .surface
            .profile_registry
            .get(&parsed.profile_id)
            .with_context(|| {
                format!(
                    "agent profile {} is not registered",
                    parsed.profile_id.as_str()
                )
            })?;
        let profile = &resolved.profile;
        let body = [
            format!("profile_id: {}", parsed.profile_id.as_str()),
            format!("source: {:?}", resolved.source),
            format!("trust: {:?}", resolved.trust_state),
            format!("mode: {}", invocation_mode_label(parsed.mode)),
            format!("objective: {}", parsed.objective),
            format!(
                "provider: {}",
                profile.provider.as_deref().unwrap_or("session default")
            ),
            format!(
                "model: {}",
                profile.model.as_deref().unwrap_or("session default")
            ),
            format!("tool_scope: {}", tool_scope_summary(&profile.tool_scope)),
            format!("mcp_servers: {}", profile.mcp_servers.len()),
            format!(
                "budget: max_threads={} max_fanout_per_turn={} max_tokens_per_agent={}",
                self.surface.budget.max_threads,
                self.surface.budget.max_spawn_fanout_per_turn,
                self.surface.budget.max_agent_tokens_per_task
            ),
        ]
        .join("\n");
        Ok(ToolPreview {
            title: format!("Spawn agent {}", parsed.profile_id.as_str()),
            summary: format!(
                "{} · {} · {}",
                invocation_mode_label(parsed.mode),
                resolved.trust_state_string(),
                resolved.source_string()
            ),
            body,
            changed_files: Vec::new(),
            file_diffs: Vec::new(),
        })
    }
}

trait AgentToolResolvedProfileExt {
    fn trust_state_string(&self) -> &'static str;
    fn source_string(&self) -> &'static str;
}

impl AgentToolResolvedProfileExt for crate::ResolvedAgentProfile {
    fn trust_state_string(&self) -> &'static str {
        match self.trust_state {
            sigil_kernel::AgentTrustState::Trusted => "trusted",
            sigil_kernel::AgentTrustState::NeedsReview => "needs_review",
            sigil_kernel::AgentTrustState::Disabled => "disabled",
            sigil_kernel::AgentTrustState::Unknown => "unknown",
        }
    }

    fn source_string(&self) -> &'static str {
        match self.source {
            sigil_kernel::AgentProfileSource::Workspace => "workspace",
            sigil_kernel::AgentProfileSource::User => "user",
            sigil_kernel::AgentProfileSource::Plugin { .. } => "plugin",
            sigil_kernel::AgentProfileSource::Compatibility { .. } => "compatibility",
            sigil_kernel::AgentProfileSource::System => "system",
            sigil_kernel::AgentProfileSource::LegacyTask => "legacy_task",
            sigil_kernel::AgentProfileSource::Unknown => "unknown",
        }
    }
}

struct SpawnAgentArgs {
    profile_id: AgentProfileId,
    objective: String,
    prompt: String,
    mode: AgentInvocationMode,
    display_name_hint: Option<String>,
}

struct ChatAgentRunRequest {
    profile_id: AgentProfileId,
    objective: String,
    prompt: String,
    mode: AgentInvocationMode,
    display_name_hint: Option<String>,
    invocation_source: AgentInvocationSource,
    resolved_profile: ResolvedAgentProfile,
}

impl SpawnAgentArgs {
    fn parse(args: &Value) -> Result<Self> {
        Ok(Self {
            profile_id: AgentProfileId::new(required_string(args, "profile_id")?)?,
            objective: required_string(args, "objective")?,
            prompt: required_string(args, "prompt")?,
            mode: optional_string(args, "mode")
                .as_deref()
                .map(parse_invocation_mode)
                .transpose()?
                .unwrap_or(AgentInvocationMode::JoinBeforeFinal),
            display_name_hint: optional_string(args, "display_name_hint"),
        })
    }
}

struct ChatAgentApprovalRouteHandler<'a> {
    inner: &'a mut (dyn ApprovalHandler + Send),
    parent_session: &'a mut Session,
    source_thread_id: AgentThreadId,
}

struct BackgroundApprovalHandler;

struct ForwardEventHandler<'a> {
    inner: &'a mut (dyn EventHandler + Send),
}

struct ChatChildThreadGuard {
    supervisor: AgentSupervisor,
    thread_id: AgentThreadId,
}

impl Drop for ChatChildThreadGuard {
    fn drop(&mut self) {
        self.supervisor.release_runtime_thread(&self.thread_id);
    }
}

impl EventHandler for ForwardEventHandler<'_> {
    fn handle(&mut self, event: RunEvent) -> Result<()> {
        self.inner.handle(event)
    }
}

impl ApprovalHandler for BackgroundApprovalHandler {
    fn approve_tool_call(&mut self, call: &ToolCall, _spec: &ToolSpec) -> Result<ToolApproval> {
        Ok(ToolApproval::Deny {
            reason: format!(
                "background agent cannot request interactive approval for {}",
                call.name
            ),
        })
    }
}

impl ApprovalHandler for ChatAgentApprovalRouteHandler<'_> {
    fn approve_tool_call(&mut self, call: &ToolCall, spec: &ToolSpec) -> Result<ToolApproval> {
        let route_id = agent_route_id_for_call(&self.source_thread_id, &call.id)?;
        self.parent_session
            .append_control(ControlEntry::AgentApprovalRoute(AgentApprovalRouteEntry {
                route_id: route_id.clone(),
                source_thread_id: self.source_thread_id.clone(),
                target_thread_id: None,
                call_id: call.id.clone(),
                tool_name: call.name.clone(),
                status: AgentRouteStatus::Requested,
            }))?;
        let approval = self.inner.approve_tool_call(call, spec)?;
        let status = match approval {
            ToolApproval::Approve => AgentRouteStatus::Resolved,
            ToolApproval::Deny { .. } => AgentRouteStatus::Rejected,
        };
        self.parent_session
            .append_control(ControlEntry::AgentApprovalRoute(AgentApprovalRouteEntry {
                route_id,
                source_thread_id: self.source_thread_id.clone(),
                target_thread_id: None,
                call_id: call.id.clone(),
                tool_name: call.name.clone(),
                status,
            }))?;
        Ok(approval)
    }
}

async fn run_background_chat_agent(
    child_agent: Agent<Box<dyn Provider>>,
    mut child_session: Session,
    initial_input: sigil_kernel::AgentRunInput,
    child_options: sigil_kernel::AgentRunOptions,
    mailbox_rx: mpsc::Receiver<AgentMailboxMessage>,
) -> Result<BackgroundChatAgentResult> {
    let mut handler = NoopEventHandler;
    let mut approval_handler = BackgroundApprovalHandler;
    let mut latest_output = child_agent
        .run_with_approval_input(
            &mut child_session,
            initial_input,
            child_options.clone(),
            &mut handler,
            &mut approval_handler,
        )
        .await?;

    loop {
        let mut prompts = Vec::new();
        while let Ok(message) = mailbox_rx.try_recv() {
            prompts.push(format!(
                "route {}:\n{}",
                message.route_id.as_str(),
                message.prompt.trim()
            ));
        }
        if prompts.is_empty() {
            break;
        }
        let followup_prompt = format!(
            "Parent agent sent follow-up instructions while this child agent was active.\n\n{}",
            prompts.join("\n\n")
        );
        latest_output = child_agent
            .run_with_approval_input(
                &mut child_session,
                sigil_kernel::AgentRunInput::user(followup_prompt),
                child_options.clone(),
                &mut handler,
                &mut approval_handler,
            )
            .await?;
    }

    let final_text = latest_output.result.final_text;
    let outcome = latest_output.outcome;
    let usage = usage_summary_from_stats(child_session.stats());
    let status = child_status_from_outcome(&final_text, &outcome);
    Ok(BackgroundChatAgentResult {
        final_text,
        outcome,
        usage,
        status,
    })
}

fn profile_index_description(index: &crate::ModelVisibleAgentIndex) -> String {
    if index.entries.is_empty() {
        return "No trusted model-invocable agent profiles are currently available.".to_owned();
    }
    let entries = index
        .entries
        .iter()
        .map(|entry| {
            format!(
                "- {}: {:?}; result_policy={}; {}",
                entry.profile_id.as_str(),
                entry.kind,
                entry.result_policy.as_str(),
                entry.description
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    if index.hidden_count == 0 {
        format!("Available profile_id values:\n{entries}")
    } else {
        format!(
            "Available profile_id values:\n{entries}\n{} additional profiles hidden by index limit.",
            index.hidden_count
        )
    }
}

fn agent_profile_system_prompt(profile: &ResolvedAgentProfile) -> Option<String> {
    let mut parts = Vec::new();
    if !profile.profile.description.trim().is_empty() {
        parts.push(format!(
            "Agent profile: {}\nDescription: {}",
            profile.profile.id.as_str(),
            profile.profile.description.trim()
        ));
    }
    if !profile.profile.instructions.trim().is_empty() {
        parts.push(format!(
            "Instructions:\n{}",
            profile.profile.instructions.trim()
        ));
    }
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

fn simple_agent_preview(title: &str, summary: &str) -> ToolPreview {
    ToolPreview {
        title: title.to_owned(),
        summary: summary.to_owned(),
        body: summary.to_owned(),
        changed_files: Vec::new(),
        file_diffs: Vec::new(),
    }
}

fn parse_tool_args(call: &ToolCall) -> Result<Value> {
    serde_json::from_str(&call.args_json)
        .with_context(|| format!("invalid tool args for {}", call.name))
}

fn required_string(args: &Value, key: &str) -> Result<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("missing required string field {key}"))
}

fn optional_string(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn thread_id_arg(args: &Value) -> Result<AgentThreadId> {
    AgentThreadId::new(required_string(args, "thread_id")?)
}

#[derive(Debug, Clone, Copy)]
struct ResultPageRequest {
    offset_chars: usize,
    max_chars: usize,
}

#[derive(Debug, Clone)]
struct ResultPage {
    text: String,
    offset_chars: usize,
    returned_chars: usize,
    total_chars: usize,
    next_offset_chars: Option<usize>,
    truncated: bool,
}

fn required_result_page_request_arg(args: &Value) -> Result<ResultPageRequest> {
    let offset_chars = optional_usize_arg(args, "offset_chars")?.unwrap_or(0);
    let max_chars = optional_usize_arg(args, "max_chars")?.unwrap_or(DEFAULT_RESULT_PAGE_LIMIT);
    if !(MIN_RESULT_SUMMARY_LIMIT..=MAX_RESULT_PAGE_LIMIT).contains(&max_chars) {
        return Err(anyhow!(
            "max_chars must be between {MIN_RESULT_SUMMARY_LIMIT} and {MAX_RESULT_PAGE_LIMIT}"
        ));
    }
    Ok(ResultPageRequest {
        offset_chars,
        max_chars,
    })
}

fn optional_usize_arg(args: &Value, key: &str) -> Result<Option<usize>> {
    let Some(value) = args.get(key) else {
        return Ok(None);
    };
    value
        .as_u64()
        .and_then(|value| usize::try_from(value).ok())
        .map(Some)
        .ok_or_else(|| anyhow!("{key} must be an integer"))
}

fn parse_invocation_mode(value: &str) -> Result<AgentInvocationMode> {
    match value {
        "foreground" => Ok(AgentInvocationMode::Foreground),
        "join_before_final" => Ok(AgentInvocationMode::JoinBeforeFinal),
        "background" => Ok(AgentInvocationMode::Background),
        other => Err(anyhow!("unsupported agent invocation mode {other}")),
    }
}

fn invocation_mode_label(mode: AgentInvocationMode) -> &'static str {
    match mode {
        AgentInvocationMode::Foreground => "foreground",
        AgentInvocationMode::Background => "background",
        AgentInvocationMode::JoinBeforeFinal => "join_before_final",
        AgentInvocationMode::Unknown => "unknown",
    }
}

fn role_for_profile_id(profile_id: &AgentProfileId) -> AgentRole {
    if profile_id.as_str() == WORKER_PROFILE_ID {
        AgentRole::SubagentWrite
    } else {
        AgentRole::SubagentRead
    }
}

fn parent_session_ref(session: &Session) -> Result<SessionRef> {
    let Some(path) = session.store_path() else {
        return SessionRef::new_relative("current.jsonl");
    };
    let file_name = path
        .file_name()
        .ok_or_else(|| anyhow!("parent session path has no file name"))?;
    SessionRef::new_relative(PathBuf::from(file_name))
}

fn agent_child_session_ref(thread_id: &AgentThreadId) -> Result<SessionRef> {
    SessionRef::new_relative(
        PathBuf::from("children")
            .join("agents")
            .join(format!("{}.jsonl", thread_id.as_str())),
    )
}

fn build_agent_child_session(parent_session: &Session, child_ref: &SessionRef) -> Result<Session> {
    if let Some(parent_path) = parent_session.store_path() {
        let parent_dir = parent_path.parent().unwrap_or_else(|| Path::new("."));
        let store = JsonlSessionStore::new(child_ref.resolve(parent_dir))?;
        return Session::load_from_store(
            parent_session.provider_name(),
            parent_session.model_name(),
            store,
        );
    }
    Ok(Session::new(
        parent_session.provider_name(),
        parent_session.model_name(),
    ))
}

fn chat_budget_scope_id(call_id: &str) -> Result<TaskId> {
    TaskId::new(format!("chat_{}", short_digest(&hash_text(call_id))))
}

fn manual_agent_call_id(session: &Session, profile_id: &AgentProfileId, prompt: &str) -> String {
    format!(
        "manual_agent_{}_{}",
        profile_id.as_str(),
        short_digest(&hash_text(&format!(
            "{}:{}:{}",
            session.entries().len(),
            profile_id.as_str(),
            prompt
        )))
    )
}

fn usage_summary_from_stats(stats: &sigil_kernel::SessionStats) -> AgentUsageSummary {
    AgentUsageSummary {
        input_tokens: stats.prompt_tokens,
        output_tokens: stats.completion_tokens,
        total_tokens: stats.prompt_tokens + stats.completion_tokens,
        cached_tokens: Some(stats.cache_hit_tokens),
    }
}

fn child_status_from_outcome(
    final_text: &str,
    outcome: &sigil_kernel::AgentRunOutcome,
) -> TaskChildSessionStatus {
    if outcome.terminal_reason == sigil_kernel::AgentRunTerminalReason::MaxTurns
        || !outcome.interrupted_tool_calls.is_empty()
    {
        TaskChildSessionStatus::Interrupted
    } else if outcome.approval_denials > 0
        || (!outcome.tool_errors.is_empty() && final_text.trim().is_empty())
    {
        TaskChildSessionStatus::Failed
    } else {
        TaskChildSessionStatus::Completed
    }
}

fn read_agent_result_page(
    parent_session: &Session,
    result: &AgentThreadResult,
    request: ResultPageRequest,
) -> Result<ResultPage> {
    let Some(parent_path) = parent_session.store_path() else {
        return Err(anyhow!(
            "agent result page unavailable because parent session has no durable store"
        ));
    };
    let parent_dir = parent_path.parent().unwrap_or_else(|| Path::new("."));
    let child_path = result.session_ref.resolve(parent_dir);
    let entries = JsonlSessionStore::read_entries(&child_path).with_context(|| {
        format!(
            "failed to read child agent session {}",
            child_path.display()
        )
    })?;
    let final_text =
        agent_final_text_from_entries(&entries, &result.output_hash).with_context(|| {
            format!(
                "failed to read final answer from child agent session {}",
                child_path.display()
            )
        })?;
    Ok(slice_result_page(&final_text, request))
}

fn agent_final_text_from_entries(entries: &[SessionLogEntry], output_hash: &str) -> Result<String> {
    let mut latest_assistant_text = None;
    for entry in entries {
        let SessionLogEntry::Assistant(message) = entry else {
            continue;
        };
        let Some(content) = message
            .content
            .as_ref()
            .filter(|content| !content.is_empty())
        else {
            continue;
        };
        if hash_text(content) == output_hash {
            return Ok(content.clone());
        }
        latest_assistant_text = Some(content.clone());
    }
    latest_assistant_text
        .ok_or_else(|| anyhow!("child agent session has no assistant final answer"))
}

fn slice_result_page(full_text: &str, request: ResultPageRequest) -> ResultPage {
    let total_chars = full_text.chars().count();
    let text = full_text
        .chars()
        .skip(request.offset_chars)
        .take(request.max_chars)
        .collect::<String>();
    let returned_chars = text.chars().count();
    let end_offset = request.offset_chars.saturating_add(returned_chars);
    let truncated = end_offset < total_chars;
    ResultPage {
        text,
        offset_chars: request.offset_chars,
        returned_chars,
        total_chars,
        next_offset_chars: truncated.then_some(end_offset),
        truncated,
    }
}

fn agent_result_tool_result(
    call: &ToolCall,
    thread_id: &AgentThreadId,
    result: Option<&sigil_kernel::AgentThreadResult>,
    max_summary_chars: usize,
) -> ToolResult {
    let Some(result) = result else {
        return ToolResult::ok(
            call.id.clone(),
            call.name.clone(),
            format!("agent thread {} is still running", thread_id.as_str()),
            ToolResultMeta {
                details: json!({
                    "thread_id": thread_id.as_str(),
                    "status": "running"
                }),
                ..ToolResultMeta::default()
            },
        );
    };
    let summary = bounded_summary(&result.summary, max_summary_chars);
    let summary_truncated =
        result.summary_truncated || summary.chars().count() < result.summary.chars().count();
    let result_fetch = json!({
        "tool": READ_AGENT_RESULT_TOOL_NAME,
        "thread_id": result.thread_id.as_str(),
        "offset_chars": 0,
        "max_chars": DEFAULT_RESULT_PAGE_LIMIT,
        "max_page_chars": MAX_RESULT_PAGE_LIMIT
    });
    let payload = json!({
        "thread_id": result.thread_id.as_str(),
        "status": terminal_status_label(result.status),
        "session_ref": result.session_ref.as_path().display().to_string(),
        "summary": summary,
        "summary_truncated": summary_truncated,
        "original_summary_chars": result.original_summary_chars,
        "changed_paths": result.changed_paths,
        "artifacts": result.artifacts,
        "risks": result.risks,
        "followups": result.followups,
        "usage": result.usage,
        "output_hash": result.output_hash,
        "truncated": summary_truncated,
        "full_result_available": !result.artifacts.is_empty(),
        "result_fetch": result_fetch
    });
    ToolResult::ok(
        call.id.clone(),
        call.name.clone(),
        serde_json::to_string(&payload)
            .unwrap_or_else(|error| format!("failed to serialize agent result: {error}")),
        ToolResultMeta {
            truncated: summary_truncated,
            limit_bytes: Some(max_summary_chars as u64),
            details: json!({
                "thread_id": result.thread_id.as_str(),
                "status": terminal_status_label(result.status),
                "output_hash": result.output_hash,
                "summary_truncated": summary_truncated,
                "original_summary_chars": result.original_summary_chars,
            }),
            ..ToolResultMeta::default()
        },
    )
}

fn agent_status_tool_result(call: &ToolCall, thread: &AgentThreadProjection) -> ToolResult {
    let result = thread.result.as_ref();
    let payload = json!({
        "thread_id": thread.thread_id.as_str(),
        "status": thread_status_label(thread.status),
        "terminal": thread.status.is_terminal(),
        "reason": &thread.reason,
        "result_available": result.is_some(),
        "result_ref": result.map(|result| json!({
            "thread_id": result.thread_id.as_str(),
            "status": terminal_status_label(result.status),
            "session_ref": result.session_ref.as_path().display().to_string(),
            "summary_truncated": result.summary_truncated,
            "original_summary_chars": result.original_summary_chars,
            "changed_paths_count": result.changed_paths.len(),
            "artifact_count": result.artifacts.len(),
            "output_hash": result.output_hash,
            "read_tool": READ_AGENT_RESULT_TOOL_NAME,
            "read_args": {
                "thread_id": result.thread_id.as_str(),
                "offset_chars": 0,
                "max_chars": DEFAULT_RESULT_PAGE_LIMIT,
                "max_page_chars": MAX_RESULT_PAGE_LIMIT
            }
        })),
    });
    ToolResult::ok(
        call.id.clone(),
        call.name.clone(),
        serde_json::to_string(&payload)
            .unwrap_or_else(|error| format!("failed to serialize agent status: {error}")),
        ToolResultMeta {
            details: json!({
                "thread_id": thread.thread_id.as_str(),
                "status": thread_status_label(thread.status),
                "result_available": result.is_some(),
            }),
            ..ToolResultMeta::default()
        },
    )
}

fn agent_result_page_tool_result(
    call: &ToolCall,
    result: &AgentThreadResult,
    page: &ResultPage,
) -> ToolResult {
    let persistent_payload = json!({
        "thread_id": result.thread_id.as_str(),
        "status": terminal_status_label(result.status),
        "session_ref": result.session_ref.as_path().display().to_string(),
        "output_hash": result.output_hash,
        "page": {
            "offset_chars": page.offset_chars,
            "returned_chars": page.returned_chars,
            "total_chars": page.total_chars,
            "next_offset_chars": page.next_offset_chars,
            "truncated": page.truncated,
            "text_omitted": true,
            "text_delivery": "transient_context"
        }
    });
    let transient_payload = json!({
        "thread_id": result.thread_id.as_str(),
        "status": terminal_status_label(result.status),
        "session_ref": result.session_ref.as_path().display().to_string(),
        "output_hash": result.output_hash,
        "page": {
            "text": page.text.as_str(),
            "offset_chars": page.offset_chars,
            "returned_chars": page.returned_chars,
            "total_chars": page.total_chars,
            "next_offset_chars": page.next_offset_chars,
            "truncated": page.truncated
        }
    });
    ToolResult::ok(
        call.id.clone(),
        call.name.clone(),
        serde_json::to_string(&persistent_payload)
            .unwrap_or_else(|error| format!("failed to serialize agent result page: {error}")),
        ToolResultMeta {
            truncated: page.truncated,
            limit_bytes: Some(page.returned_chars as u64),
            details: json!({
                "thread_id": result.thread_id.as_str(),
                "status": terminal_status_label(result.status),
                "output_hash": result.output_hash,
                "offset_chars": page.offset_chars,
                "returned_chars": page.returned_chars,
                "total_chars": page.total_chars,
            }),
            ..ToolResultMeta::default()
        },
    )
    .with_transient_context(vec![ModelMessage::user(format!(
        "Transient read_agent_result page for tool_call_id={}:\n{}",
        call.id,
        serde_json::to_string(&transient_payload).unwrap_or_else(|error| format!(
            "failed to serialize transient agent result page: {error}"
        ))
    ))])
}

fn bounded_summary(summary: &str, max_chars: usize) -> String {
    summary.chars().take(max_chars).collect()
}

fn terminal_status_label(status: AgentThreadTerminalStatus) -> &'static str {
    match status {
        AgentThreadTerminalStatus::Completed => "completed",
        AgentThreadTerminalStatus::Failed => "failed",
        AgentThreadTerminalStatus::Cancelled => "cancelled",
        AgentThreadTerminalStatus::Interrupted => "interrupted",
        AgentThreadTerminalStatus::Unknown => "unknown",
    }
}

fn thread_status_label(status: AgentThreadStatus) -> &'static str {
    match status {
        AgentThreadStatus::Started => "started",
        AgentThreadStatus::Running => "running",
        AgentThreadStatus::Blocked => "blocked",
        AgentThreadStatus::Completed => "completed",
        AgentThreadStatus::Failed => "failed",
        AgentThreadStatus::Cancelled => "cancelled",
        AgentThreadStatus::Interrupted => "interrupted",
        AgentThreadStatus::Closed => "closed",
        AgentThreadStatus::Unavailable => "unavailable",
        AgentThreadStatus::Unknown => "unknown",
    }
}

fn tool_scope_summary(scope: &sigil_kernel::ToolRegistryScope) -> String {
    if scope.allow_all {
        return "all tools".to_owned();
    }
    let names = scope.names.iter().cloned().collect::<Vec<_>>().join(",");
    let prefixes = scope.prefixes.join(",");
    if names.is_empty() && prefixes.is_empty() {
        "no tools".to_owned()
    } else if prefixes.is_empty() {
        format!("names={names}")
    } else if names.is_empty() {
        format!("prefixes={prefixes}")
    } else {
        format!("names={names}; prefixes={prefixes}")
    }
}

fn agent_route_id_for_call(thread_id: &AgentThreadId, call_id: &str) -> Result<AgentRouteId> {
    AgentRouteId::new(format!(
        "agent_route_{}",
        short_digest(&hash_text(&format!("{}:{}", thread_id.as_str(), call_id)))
    ))
}

fn hash_text(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn short_digest(hash: &str) -> &str {
    hash.get(..12).unwrap_or(hash)
}

#[cfg(test)]
#[path = "tests/agent_tools_tests.rs"]
mod tests;
