use std::{env, path::PathBuf, sync::Arc};

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    AgentRole, AgentRunOptions, ApprovalMode, ExecutionBackend, InteractionMode, McpServerConfig,
    McpServerStartup, MutationEventRecorder, PermissionEvaluationContext, Provider,
    ProviderCapabilities, ReasoningEffort, RoleModelConfig, RootConfig, ScopedToolRegistry,
    SecretRedactor, SkillDescriptor, Tool, ToolAccess, ToolAllowlistConfig, ToolCategory,
    ToolContext, ToolEgressAudit, ToolErrorKind, ToolPreviewCapability, ToolRegistry,
    ToolRegistryScope, ToolResult, ToolResultMeta, ToolSpec, ToolSubject, ToolSubjectKind,
    ToolSubjectScope, default_user_config_dir,
};
pub use sigil_mcp::{
    McpElicitationAction, McpElicitationHandler, McpElicitationRequest, McpElicitationResponse,
    McpListChangedKind, McpListChangedNotification, McpProgressNotification,
    McpRuntimeEventHandler,
};
use sigil_provider_anthropic::{
    ANTHROPIC_API_KEY_ENV, AnthropicProvider, AnthropicProviderConfig, SIGIL_ANTHROPIC_API_KEY_ENV,
    anthropic_capabilities,
};
use sigil_provider_deepseek::{
    DeepSeekProvider, DeepSeekProviderConfig, LEGACY_DEEPSEEK_API_KEY_ENV, SIGIL_API_KEY_ENV,
    deepseek_capabilities,
};
use sigil_provider_gemini::{
    GEMINI_API_KEY_ENV, GOOGLE_API_KEY_ENV, GeminiProvider, GeminiProviderConfig,
    SIGIL_GEMINI_API_KEY_ENV, gemini_capabilities,
};
use sigil_provider_openai_compat::{
    OPENAI_API_KEY_ENV, OPENAI_COMPATIBLE_API_KEY_ENV, OpenAiCompatibleProvider,
    OpenAiCompatibleProviderConfig, openai_compatible_capabilities,
};

pub mod agent_profile_registry;
pub mod agent_supervisor;
pub mod agent_tools;
pub mod context;
pub mod context_window;
pub mod doctor;
pub mod paths;
pub mod plugins;
pub mod product_view;
pub mod provider_config;
pub mod provider_status;
pub mod session_control;
pub mod skills;
pub use agent_profile_registry::{
    AgentProfileIndexContext, AgentProfileRegistry, BUILD_PROFILE_ID, EXPLORE_PROFILE_ID,
    ModelVisibleAgentIndex, ModelVisibleAgentIndexEntry, PLAN_PROFILE_ID, ResolvedAgentProfile,
    WORKER_PROFILE_ID,
};
pub use agent_supervisor::{
    AgentBudgetPolicy, AgentChatChildStart, AgentChatChildThread, AgentInterruptedThread,
    AgentMailboxMessage, AgentSupervisor, AgentSupervisorTaskChildRunner, AgentTaskChildStart,
    AgentTaskChildThread, ForegroundCancelImpact, chat_agent_thread_id_for_call,
};
pub use agent_tools::{
    AgentToolBackgroundEventSink, AgentToolBackgroundRuns, AgentToolProviderFactory,
    AgentToolRuntime, CLOSE_AGENT_TOOL_NAME, MESSAGE_AGENT_TOOL_NAME, ManualAgentInvocationResult,
    READ_AGENT_RESULT_TOOL_NAME, SPAWN_AGENT_TOOL_NAME, WAIT_AGENT_TOOL_NAME, close_agent_thread,
    register_agent_tools, register_agent_tools_with_registry, register_agent_tools_with_workspace,
    register_agent_tools_with_workspace_and_entries,
};
pub use context::context_items_from_task_memory;
pub use context_window::{
    ContextWindowSource, ResolvedContextWindow, effective_compaction_config,
    resolve_context_window_tokens,
};
pub use paths::{
    DEFAULT_ARTIFACTS_DIR, DEFAULT_CHANGESETS_DIR, DEFAULT_PROJECT_ASSETS_ROOT,
    DEFAULT_SCRATCH_DIR, DEFAULT_SESSIONS_DIR, DEFAULT_TERMINAL_TASKS_DIR,
    DEFAULT_WORKSPACE_AGENTS_DIR, DEFAULT_WORKSPACE_SKILLS_DIR, INPUT_HISTORY_FILE,
    PathResolverEnv, SIGIL_CACHE_HOME_ENV, SIGIL_STATE_HOME_ENV, SigilPaths, StoragePlatform,
    project_asset_dir, resolve_sigil_paths, resolve_sigil_paths_with_env, workspace_id_for_root,
};
pub use plugins::{
    PluginDiscoveryReport, PluginDiscoveryWarning, PluginDiscoveryWarningKind,
    PluginHookRegistration, PluginMcpServerRegistration, PluginRegistrations,
    discover_workspace_plugins, discover_workspace_plugins_with_project_assets_root,
    merge_plugin_mcp_servers, merge_plugin_skill_descriptors,
};
pub use product_view::{
    AgentGraphProductSummary, agent_graph_product_summary_from_entries,
    agent_graph_product_summary_from_session_log,
};
pub use provider_config::{
    ANTHROPIC_PROVIDER_KEY, DEEPSEEK_PROVIDER_KEY, DEFAULT_SETUP_API_KEY_ENV,
    DEFAULT_SETUP_PROVIDER_KEY, DeepSeekProviderConfigFields, GEMINI_PROVIDER_KEY,
    OPENAI_COMPAT_PROVIDER_KEY, ProviderConfigFields, ProviderStatusConfig,
    ProviderStrictToolsMode, deepseek_provider_config_fields, deepseek_provider_status_config,
    deepseek_provider_value_for_setup, default_provider_config_fields,
    default_setup_provider_model, normalize_provider_name, provider_api_key_env_name,
    provider_config_fields, provider_status_config_from_fields, set_active_provider_model,
    set_provider_config_fields,
};
pub use provider_status::{
    BalanceSnapshot, ProviderStatusTaskManager, ProviderStatusTaskResult,
    fetch_provider_balance_snapshot, fetch_remote_model_ids,
};
pub use session_control::{append_session_control_entries, current_unix_time_ms};
pub use skills::{
    LOAD_SKILL_TOOL_NAME, LoadedSkillContext, SkillDiscoveryReport, SkillDiscoveryWarning,
    SkillDiscoveryWarningKind, discover_skill_index, discover_skill_index_with_project_assets_root,
    discover_skill_index_with_user_dir, load_user_invoked_skill, namespaced_plugin_skill_id,
    register_skill_tools, register_skill_tools_with_project_assets_root,
};

/// Builds the configured model provider for runtime entrypoints.
///
/// # Errors
///
/// Returns an error when the configured provider is unsupported or its provider-specific
/// configuration cannot be parsed or initialized.
pub fn build_provider(root_config: &RootConfig) -> Result<Box<dyn Provider>> {
    match provider_config_key(&root_config.agent.provider) {
        "deepseek" => Ok(Box::new(DeepSeekProvider::new(resolve_deepseek_config(
            root_config,
        )?)?)),
        "openai_compat" => Ok(Box::new(OpenAiCompatibleProvider::new(
            resolve_openai_compat_config(root_config)?,
        )?)),
        "anthropic" => Ok(Box::new(AnthropicProvider::new(resolve_anthropic_config(
            root_config,
        )?)?)),
        "gemini" => Ok(Box::new(GeminiProvider::new(resolve_gemini_config(
            root_config,
        )?)?)),
        other => Err(anyhow!("unsupported provider {other}")),
    }
}

/// Product-facing support state for one provider-neutral capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderCapabilityStatus {
    Supported,
    Advanced,
    Unsupported,
}

impl ProviderCapabilityStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Supported => "supported",
            Self::Advanced => "advanced",
            Self::Unsupported => "unsupported",
        }
    }
}

/// One provider capability row suitable for diagnostics and TUI config surfaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCapabilityRow {
    pub key: &'static str,
    pub label: &'static str,
    pub status: ProviderCapabilityStatus,
    pub detail: String,
}

/// Provider-neutral capability view derived from `ProviderCapabilities`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCapabilityView {
    pub provider_name: String,
    pub rows: Vec<ProviderCapabilityRow>,
}

/// Returns static provider capabilities for a configured provider name or alias.
#[must_use]
pub fn provider_capabilities_for_name(provider_name: &str) -> Option<ProviderCapabilities> {
    match provider_config_key(provider_name) {
        "deepseek" => Some(deepseek_capabilities()),
        "openai_compat" => Some(openai_compatible_capabilities()),
        "anthropic" => Some(anthropic_capabilities()),
        "gemini" => Some(gemini_capabilities()),
        _ => None,
    }
}

/// Builds a provider-neutral capability view for diagnostics and UI display.
#[must_use]
pub fn provider_capability_view(
    provider_name: &str,
    capabilities: &ProviderCapabilities,
) -> ProviderCapabilityView {
    let mut rows = Vec::new();
    rows.push(capability_row(
        "text_stream",
        "Streaming text",
        ProviderCapabilityStatus::Supported,
        "provider stream emits text deltas",
    ));
    rows.push(capability_row(
        "tool_calls",
        "Tool calls",
        if capabilities.supports_schema_constrained_tools || capabilities.supports_tool_stream {
            ProviderCapabilityStatus::Supported
        } else {
            ProviderCapabilityStatus::Unsupported
        },
        if capabilities.supports_schema_constrained_tools {
            "schema-constrained tools enabled"
        } else {
            "basic tool calls only"
        },
    ));
    rows.push(capability_row(
        "tool_args_stream",
        "Tool arg stream",
        status_for_bool(capabilities.supports_tool_stream),
        "incremental tool arguments",
    ));
    rows.push(capability_row(
        "reasoning_stream",
        "Reasoning stream",
        if capabilities.can_surface_reasoning_stream() {
            ProviderCapabilityStatus::Supported
        } else {
            ProviderCapabilityStatus::Unsupported
        },
        capabilities.reasoning_stream.as_str(),
    ));
    rows.push(capability_row(
        "reasoning_effort",
        "Reasoning effort",
        status_for_bool(capabilities.supports_reasoning_effort),
        "generic low/medium/high/max control",
    ));
    rows.push(capability_row(
        "reasoning_artifacts",
        "Reasoning artifacts",
        status_for_bool(capabilities.supports_reasoning_artifacts),
        "durable reasoning artifact handles",
    ));
    rows.push(capability_row(
        "structured_output",
        "Structured output",
        status_for_bool(capabilities.supports_structured_output),
        "provider-native structured response mode",
    ));
    rows.push(capability_row(
        "assistant_prefix_seed",
        "Assistant prefix seed",
        status_for_bool(capabilities.supports_assistant_prefix_seed),
        "assistant-prefix seed accepted",
    ));
    rows.push(capability_row(
        "background_tasks",
        "Background tasks",
        status_for_bool(capabilities.supports_background_tasks),
        "provider-managed async work",
    ));
    rows.push(capability_row(
        "agent_background_resume",
        "Agent background resume",
        status_for_bool(capabilities.supports_agent_background_resume),
        "provider-backed child thread resume",
    ));
    rows.push(capability_row(
        "agent_thread_usage",
        "Agent thread usage",
        status_for_bool(capabilities.supports_agent_thread_usage),
        "per-agent usage replay",
    ));
    rows.push(capability_row(
        "agent_result_replay",
        "Agent result replay",
        status_for_bool(capabilities.supports_agent_result_replay),
        "provider-backed child result replay",
    ));
    rows.push(capability_row(
        "response_handles",
        "Response handles",
        status_for_bool(capabilities.supports_response_handles),
        "provider resumable response handle",
    ));
    rows.push(capability_row(
        "cache_reporting",
        "Cache telemetry",
        if capabilities.exact_prefix_cache && capabilities.reports_cache_tokens {
            ProviderCapabilityStatus::Supported
        } else if capabilities.reports_cache_tokens {
            ProviderCapabilityStatus::Advanced
        } else {
            ProviderCapabilityStatus::Unsupported
        },
        if capabilities.exact_prefix_cache {
            "exact prefix cache tokens"
        } else if capabilities.reports_cache_tokens {
            "provider cache token reporting"
        } else {
            "not reported"
        },
    ));
    rows.push(capability_row(
        "system_fingerprint",
        "System fingerprint",
        status_for_bool(capabilities.supports_system_fingerprint),
        "system fingerprint telemetry",
    ));
    rows.push(capability_row(
        "infill",
        "Infill completion",
        status_for_bool(capabilities.supports_infill_completion),
        "provider-native infill completion",
    ));
    rows.push(capability_row(
        "tool_name_limit",
        "Tool name budget",
        status_for_bool(capabilities.tool_name_max_chars > 0),
        format!(
            "provider-visible tool names up to {} chars",
            capabilities.tool_name_max_chars
        ),
    ));

    ProviderCapabilityView {
        provider_name: provider_config_key(provider_name).to_owned(),
        rows,
    }
}

fn status_for_bool(supported: bool) -> ProviderCapabilityStatus {
    if supported {
        ProviderCapabilityStatus::Supported
    } else {
        ProviderCapabilityStatus::Unsupported
    }
}

fn capability_row(
    key: &'static str,
    label: &'static str,
    status: ProviderCapabilityStatus,
    detail: impl Into<String>,
) -> ProviderCapabilityRow {
    ProviderCapabilityRow {
        key,
        label,
        status,
        detail: detail.into(),
    }
}

/// Builds the configured model provider for one task role.
///
/// # Errors
///
/// Returns an error when the resolved role provider is unsupported or malformed.
pub fn build_role_provider(root_config: &RootConfig, role: AgentRole) -> Result<Box<dyn Provider>> {
    let role_config = root_config.task.role_config(role);
    let resolved = root_config_with_role_agent(root_config, role_config);
    build_provider(&resolved)
}

/// Builds the complete runtime tool registry from built-ins and configured MCP servers.
///
/// # Errors
///
/// Returns an error when one configured MCP server cannot be started or queried.
pub async fn build_tool_registry(
    root_config: &RootConfig,
    provider_capabilities: &ProviderCapabilities,
    workspace_root: PathBuf,
) -> Result<ToolRegistry> {
    build_tool_registry_with_mcp_elicitation(
        root_config,
        provider_capabilities,
        workspace_root,
        sigil_mcp::unsupported_mcp_elicitation_handler(),
    )
    .await
}

/// Builds the runtime tool registry using a caller-provided MCP elicitation handler.
///
/// # Errors
///
/// Returns an error when one configured MCP server cannot be started or queried.
pub async fn build_tool_registry_with_mcp_elicitation(
    root_config: &RootConfig,
    provider_capabilities: &ProviderCapabilities,
    workspace_root: PathBuf,
    elicitation_handler: Arc<dyn McpElicitationHandler>,
) -> Result<ToolRegistry> {
    build_tool_registry_with_mcp_handlers(
        root_config,
        provider_capabilities,
        workspace_root,
        elicitation_handler,
        sigil_mcp::unsupported_mcp_runtime_event_handler(),
    )
    .await
}

/// Builds the runtime tool registry using caller-provided MCP handlers.
///
/// # Errors
///
/// Returns an error when one configured MCP server cannot be started or queried.
pub async fn build_tool_registry_with_mcp_handlers(
    root_config: &RootConfig,
    provider_capabilities: &ProviderCapabilities,
    workspace_root: PathBuf,
    elicitation_handler: Arc<dyn McpElicitationHandler>,
    runtime_event_handler: Arc<dyn McpRuntimeEventHandler>,
) -> Result<ToolRegistry> {
    let mut registry = ToolRegistry::new();
    register_local_tools(&mut registry, root_config, workspace_root.clone())?;
    sigil_mcp::register_mcp_tools_with_options(
        &mut registry,
        &root_config.mcp_servers,
        sigil_mcp::McpToolRegistrationOptions::eager()?
            .with_capabilities(provider_capabilities)
            .with_roots(vec![canonical_workspace_root(workspace_root.clone())])
            .with_working_dir(workspace_root.clone())
            .with_secret_redactor(secret_redactor_for_root_config(root_config))
            .with_elicitation_handler(Arc::clone(&elicitation_handler))
            .with_runtime_event_handler(Arc::clone(&runtime_event_handler)),
    )
    .await?;
    register_lazy_mcp_activation_tool(
        &mut registry,
        root_config,
        provider_capabilities,
        workspace_root,
        elicitation_handler,
        runtime_event_handler,
    );
    Ok(registry)
}

/// Builds the local tool surface and lazy MCP activation tool without starting eager MCP servers.
///
/// TUI entrypoints use this to keep the agent worker available when an external MCP server is
/// slow or broken. Eager MCP servers can then be activated asynchronously against the returned
/// shared registry.
///
/// # Errors
///
/// Returns an error when local tool construction fails, including execution backend policies that
/// cannot be satisfied by the configured backend.
pub fn build_tool_registry_without_eager_mcp(
    root_config: &RootConfig,
    provider_capabilities: &ProviderCapabilities,
    workspace_root: PathBuf,
    elicitation_handler: Arc<dyn McpElicitationHandler>,
    runtime_event_handler: Arc<dyn McpRuntimeEventHandler>,
) -> Result<ToolRegistry> {
    let mut registry = ToolRegistry::new();
    register_local_tools(&mut registry, root_config, workspace_root.clone())?;
    register_lazy_mcp_activation_tool(
        &mut registry,
        root_config,
        provider_capabilities,
        workspace_root,
        elicitation_handler,
        runtime_event_handler,
    );
    Ok(registry)
}

/// Activates lazy MCP servers against an existing runtime tool registry.
///
/// Returns the number of tools added to the registry. When `server_name` is set, only the
/// matching lazy server is activated.
///
/// # Errors
///
/// Returns an error when a required lazy MCP server cannot be started, initialized, or queried.
pub async fn activate_lazy_mcp_tools(
    registry: &mut ToolRegistry,
    root_config: &RootConfig,
    provider_capabilities: &ProviderCapabilities,
    workspace_root: PathBuf,
    server_name: Option<&str>,
) -> Result<usize> {
    Ok(activate_lazy_mcp_tools_detailed(
        registry,
        root_config,
        provider_capabilities,
        workspace_root,
        server_name,
    )
    .await?
    .added_tools)
}

/// Detailed result for one lazy MCP activation attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LazyMcpActivationResult {
    pub matched_servers: usize,
    pub added_tools: usize,
}

/// Activates lazy MCP servers and reports both matched server and added tool counts.
///
/// # Errors
///
/// Returns an error when a required lazy MCP server cannot be started, initialized, or queried.
pub async fn activate_lazy_mcp_tools_detailed(
    registry: &mut ToolRegistry,
    root_config: &RootConfig,
    provider_capabilities: &ProviderCapabilities,
    workspace_root: PathBuf,
    server_name: Option<&str>,
) -> Result<LazyMcpActivationResult> {
    activate_lazy_mcp_tools_detailed_with_mcp_elicitation(
        registry,
        root_config,
        provider_capabilities,
        workspace_root,
        server_name,
        sigil_mcp::unsupported_mcp_elicitation_handler(),
    )
    .await
}

/// Activates lazy MCP servers using a caller-provided MCP elicitation handler.
///
/// # Errors
///
/// Returns an error when a required lazy MCP server cannot be started, initialized, or queried.
pub async fn activate_lazy_mcp_tools_detailed_with_mcp_elicitation(
    registry: &mut ToolRegistry,
    root_config: &RootConfig,
    provider_capabilities: &ProviderCapabilities,
    workspace_root: PathBuf,
    server_name: Option<&str>,
    elicitation_handler: Arc<dyn McpElicitationHandler>,
) -> Result<LazyMcpActivationResult> {
    activate_lazy_mcp_tools_detailed_with_mcp_handlers(
        registry,
        root_config,
        provider_capabilities,
        workspace_root,
        server_name,
        elicitation_handler,
        sigil_mcp::unsupported_mcp_runtime_event_handler(),
    )
    .await
}

/// Activates lazy MCP servers using caller-provided MCP handlers.
///
/// # Errors
///
/// Returns an error when a required lazy MCP server cannot be started, initialized, or queried.
pub async fn activate_lazy_mcp_tools_detailed_with_mcp_handlers(
    registry: &mut ToolRegistry,
    root_config: &RootConfig,
    provider_capabilities: &ProviderCapabilities,
    workspace_root: PathBuf,
    server_name: Option<&str>,
    elicitation_handler: Arc<dyn McpElicitationHandler>,
    runtime_event_handler: Arc<dyn McpRuntimeEventHandler>,
) -> Result<LazyMcpActivationResult> {
    activate_lazy_mcp_tools_detailed_with_mcp_handlers_and_mutation_recorder(
        registry,
        root_config,
        provider_capabilities,
        workspace_root,
        server_name,
        elicitation_handler,
        runtime_event_handler,
        None,
    )
    .await
}

/// Activates lazy MCP servers while recording conservative external-process mutation evidence.
///
/// # Errors
///
/// Returns an error when a required lazy MCP server cannot be started, initialized, or queried.
pub async fn activate_lazy_mcp_tools_detailed_with_mcp_handlers_and_mutation_recorder(
    registry: &mut ToolRegistry,
    root_config: &RootConfig,
    provider_capabilities: &ProviderCapabilities,
    workspace_root: PathBuf,
    server_name: Option<&str>,
    elicitation_handler: Arc<dyn McpElicitationHandler>,
    runtime_event_handler: Arc<dyn McpRuntimeEventHandler>,
    mutation_recorder: Option<MutationEventRecorder>,
) -> Result<LazyMcpActivationResult> {
    let servers = root_config
        .mcp_servers
        .iter()
        .filter(|server| server.startup == McpServerStartup::Lazy)
        .filter(|server| server_name.is_none_or(|name| server.name == name))
        .cloned()
        .collect::<Vec<_>>();
    if servers.is_empty() {
        return Ok(LazyMcpActivationResult {
            matched_servers: 0,
            added_tools: 0,
        });
    }

    let before = registry.specs().len();
    let mut registration_options = sigil_mcp::McpToolRegistrationOptions::lazy()?
        .with_capabilities(provider_capabilities)
        .with_roots(vec![canonical_workspace_root(workspace_root.clone())])
        .with_working_dir(workspace_root.clone())
        .with_secret_redactor(secret_redactor_for_root_config(root_config))
        .with_elicitation_handler(elicitation_handler)
        .with_runtime_event_handler(runtime_event_handler);
    if let Some(recorder) = mutation_recorder {
        registration_options =
            registration_options.with_mutation_recorder(workspace_root.clone(), recorder);
    }
    sigil_mcp::register_mcp_tools_with_options(registry, &servers, registration_options).await?;
    Ok(LazyMcpActivationResult {
        matched_servers: servers.len(),
        added_tools: registry.specs().len().saturating_sub(before),
    })
}

/// Detailed result for one MCP server refresh.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpRefreshResult {
    pub matched_servers: usize,
    pub removed_tools: usize,
    pub added_tools: usize,
}

fn register_local_tools(
    registry: &mut ToolRegistry,
    root_config: &RootConfig,
    workspace_root: PathBuf,
) -> Result<()> {
    let paths = resolve_sigil_paths(&root_config.storage, &root_config.session, &workspace_root);
    let execution_backend = build_configured_execution_backend(root_config)?;
    sigil_tools_builtin::register_builtin_tools_with_paths_and_execution_backend(
        registry,
        sigil_tools_builtin::BuiltinToolPaths {
            changesets_root: paths.changesets_root.clone(),
            changesets_label_root: PathBuf::from("state/artifacts/changesets"),
            terminal_tasks_root: paths.terminal_tasks_root.clone(),
            terminal_tasks_label_root: PathBuf::from("state/artifacts/tasks"),
            scratch_root: paths.scratch_root.clone(),
            scratch_label: "cache/tmp".to_owned(),
        },
        execution_backend,
    );
    sigil_code_intel::register_code_intelligence_tools(
        registry,
        &root_config.code_intelligence,
        workspace_root.clone(),
    );
    let user_config_dir = default_user_config_dir().ok();
    let _ = skills::register_skill_tools_with_project_assets_root(
        registry,
        &workspace_root,
        &paths.project_assets_root,
        user_config_dir.as_deref(),
        &root_config.skills,
    );
    Ok(())
}

/// Builds the execution backend configured for tools and verification checks.
///
/// # Errors
///
/// Returns an error when the configured backend cannot satisfy the requested isolation policy.
pub fn build_configured_execution_backend(
    root_config: &RootConfig,
) -> Result<Arc<dyn ExecutionBackend>> {
    sigil_tools_builtin::build_execution_backend(&root_config.execution)
}

/// Refreshes provider-visible tools for one configured MCP server.
///
/// # Errors
///
/// Returns an error when a required MCP server cannot be restarted, initialized, or queried.
pub async fn refresh_mcp_server_tools_with_mcp_handlers(
    registry: &mut ToolRegistry,
    root_config: &RootConfig,
    provider_capabilities: &ProviderCapabilities,
    workspace_root: PathBuf,
    server_name: &str,
    elicitation_handler: Arc<dyn McpElicitationHandler>,
    runtime_event_handler: Arc<dyn McpRuntimeEventHandler>,
) -> Result<McpRefreshResult> {
    refresh_mcp_server_tools_with_mcp_handlers_and_mutation_recorder(
        registry,
        root_config,
        provider_capabilities,
        workspace_root,
        server_name,
        elicitation_handler,
        runtime_event_handler,
        None,
    )
    .await
}

/// Refreshes one MCP server while recording conservative external-process mutation evidence.
///
/// # Errors
///
/// Returns an error when a required MCP server cannot be restarted, initialized, or queried.
pub async fn refresh_mcp_server_tools_with_mcp_handlers_and_mutation_recorder(
    registry: &mut ToolRegistry,
    root_config: &RootConfig,
    provider_capabilities: &ProviderCapabilities,
    workspace_root: PathBuf,
    server_name: &str,
    elicitation_handler: Arc<dyn McpElicitationHandler>,
    runtime_event_handler: Arc<dyn McpRuntimeEventHandler>,
    mutation_recorder: Option<MutationEventRecorder>,
) -> Result<McpRefreshResult> {
    let servers = root_config
        .mcp_servers
        .iter()
        .filter(|server| server.name == server_name)
        .cloned()
        .collect::<Vec<_>>();
    let Some(server) = servers.first() else {
        return Ok(McpRefreshResult {
            matched_servers: 0,
            removed_tools: 0,
            added_tools: 0,
        });
    };

    let prefix = sigil_mcp::mcp_provider_tool_name_prefix(server_name);
    let removed = registry.drain_by_name_prefix(&prefix);
    let removed_tools = removed.len();
    let before = registry.specs().len();
    let mut registration_options =
        sigil_mcp::McpToolRegistrationOptions::for_startup(server.startup)?
            .with_capabilities(provider_capabilities)
            .with_roots(vec![canonical_workspace_root(workspace_root.clone())])
            .with_working_dir(workspace_root.clone())
            .with_secret_redactor(secret_redactor_for_root_config(root_config))
            .with_elicitation_handler(elicitation_handler)
            .with_runtime_event_handler(runtime_event_handler);
    if let Some(recorder) = mutation_recorder {
        registration_options =
            registration_options.with_mutation_recorder(workspace_root.clone(), recorder);
    }
    if let Err(error) =
        sigil_mcp::register_mcp_tools_with_options(registry, &servers, registration_options).await
    {
        for tool in removed {
            registry.register(tool);
        }
        return Err(error);
    }
    Ok(McpRefreshResult {
        matched_servers: servers.len(),
        removed_tools,
        added_tools: registry.specs().len().saturating_sub(before),
    })
}

fn register_lazy_mcp_activation_tool(
    registry: &mut ToolRegistry,
    root_config: &RootConfig,
    provider_capabilities: &ProviderCapabilities,
    workspace_root: PathBuf,
    elicitation_handler: Arc<dyn McpElicitationHandler>,
    runtime_event_handler: Arc<dyn McpRuntimeEventHandler>,
) {
    if !root_config
        .mcp_servers
        .iter()
        .any(|server| server.startup == McpServerStartup::Lazy)
    {
        return;
    }
    registry.register(Arc::new(McpActivateServerTool {
        registry: registry.clone(),
        root_config: root_config.clone(),
        provider_capabilities: provider_capabilities.clone(),
        workspace_root,
        elicitation_handler,
        runtime_event_handler,
    }));
}

#[derive(Clone)]
struct McpActivateServerTool {
    registry: ToolRegistry,
    root_config: RootConfig,
    provider_capabilities: ProviderCapabilities,
    workspace_root: PathBuf,
    elicitation_handler: Arc<dyn McpElicitationHandler>,
    runtime_event_handler: Arc<dyn McpRuntimeEventHandler>,
}

#[async_trait]
impl Tool for McpActivateServerTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "mcp_activate_server".to_owned(),
            description: "Activate a configured lazy MCP server so its real tools become available on the next model turn."
                .to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "server_name": {
                        "type": "string",
                        "description": "Name of the configured MCP server with startup = lazy."
                    }
                },
                "required": ["server_name"]
            }),
            category: ToolCategory::Mcp,
            access: ToolAccess::Network,
            preview: ToolPreviewCapability::None,
        }
    }

    fn permission_subjects(&self, _ctx: &ToolContext, args: &Value) -> Result<Vec<ToolSubject>> {
        let server_name = required_server_name(args)?;
        let Some(server) = self.lazy_server(server_name) else {
            return Ok(vec![mcp_server_subject(server_name)]);
        };
        Ok(vec![
            mcp_server_subject(server_name),
            ToolSubject::mcp_trust_class(server.name.clone(), server.trust.trust_class.as_str()),
        ])
    }

    fn permission_default_mode(
        &self,
        _ctx: &ToolContext,
        args: &Value,
    ) -> Result<Option<ApprovalMode>> {
        let server_name = required_server_name(args)?;
        Ok(self
            .lazy_server(server_name)
            .map(|server| server.trust.approval_default))
    }

    fn egress_audit(&self, _ctx: &ToolContext, args: &Value) -> Result<Option<ToolEgressAudit>> {
        let server_name = required_server_name(args)?;
        let Some(server) = self.lazy_server(server_name) else {
            return Ok(None);
        };
        if !server.trust.egress_logging {
            return Ok(None);
        }
        Ok(Some(ToolEgressAudit {
            destination: format!("mcp:{server_name}"),
            operation: "server/activate".to_owned(),
            payload: json!({
                "server": server_name,
                "trust_class": server.trust.trust_class.as_str(),
                "startup": server.startup.as_str(),
            }),
            redacted: false,
        }))
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        let server_name = required_server_name(&args)?;
        if self.lazy_server(server_name).is_none() {
            return Ok(ToolResult::error(
                call_id,
                "mcp_activate_server",
                ToolErrorKind::InvalidInput,
                format!("unknown lazy MCP server {server_name}"),
            ));
        }
        if self.registered_tool_count(server_name) > 0 {
            return Ok(activation_result(
                call_id,
                server_name,
                "already_ready",
                1,
                0,
            ));
        }

        let mut registry = self.registry.clone();
        let result = activate_lazy_mcp_tools_detailed_with_mcp_handlers_and_mutation_recorder(
            &mut registry,
            &self.root_config,
            &self.provider_capabilities,
            self.workspace_root.clone(),
            Some(server_name),
            Arc::clone(&self.elicitation_handler),
            Arc::clone(&self.runtime_event_handler),
            ctx.mutation_recorder.clone(),
        )
        .await?;
        Ok(activation_result(
            call_id,
            server_name,
            "ready",
            result.matched_servers,
            result.added_tools,
        ))
    }
}

impl McpActivateServerTool {
    fn lazy_server(&self, server_name: &str) -> Option<&McpServerConfig> {
        self.root_config
            .mcp_servers
            .iter()
            .find(|server| server.name == server_name && server.startup == McpServerStartup::Lazy)
    }

    fn registered_tool_count(&self, server_name: &str) -> usize {
        let prefix = sigil_mcp::mcp_provider_tool_name_prefix(server_name);
        self.registry
            .specs()
            .into_iter()
            .filter(|spec| spec.name.starts_with(&prefix))
            .count()
    }
}

fn activation_result(
    call_id: String,
    server_name: &str,
    status: &str,
    matched_servers: usize,
    added_tools: usize,
) -> ToolResult {
    ToolResult::ok(
        call_id,
        "mcp_activate_server",
        json!({
            "server_name": server_name,
            "status": status,
            "matched_servers": matched_servers,
            "added_tools": added_tools,
        })
        .to_string(),
        ToolResultMeta::default(),
    )
}

fn required_server_name(args: &Value) -> Result<&str> {
    args.get("server_name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("missing server_name"))
}

fn mcp_server_subject(server_name: &str) -> ToolSubject {
    ToolSubject {
        kind: ToolSubjectKind::McpTool,
        original: server_name.to_owned(),
        normalized: format!("mcp_server:{server_name}"),
        canonical_path: None,
        scope: ToolSubjectScope::Unknown,
    }
}

/// Builds shared agent run options for CLI, TUI, and future entrypoints.
pub fn build_run_options(
    root_config: &RootConfig,
    workspace_root: PathBuf,
    interaction_mode: InteractionMode,
) -> AgentRunOptions {
    let workspace_root = canonical_workspace_root(workspace_root);
    let paths = resolve_sigil_paths(&root_config.storage, &root_config.session, &workspace_root);
    AgentRunOptions {
        traffic_partition_key: Some(workspace_partition_key(&workspace_root)),
        workspace_root,
        max_turns: root_config.agent.max_turns,
        tool_timeout_secs: root_config.agent.tool_timeout_secs,
        reasoning_effort: Some(default_reasoning_effort(root_config)),
        interaction_mode,
        permission_config: root_config.permission.clone(),
        permission_context: permission_evaluation_context(root_config, &paths),
        memory_config: root_config.memory.clone(),
        compaction_config: root_config.compaction.clone(),
    }
}

fn permission_evaluation_context(
    root_config: &RootConfig,
    paths: &SigilPaths,
) -> PermissionEvaluationContext {
    PermissionEvaluationContext {
        workspace_root: paths.workspace_root.clone(),
        project_asset_roots: vec![
            paths.project_assets_root.clone(),
            project_asset_dir(
                &paths.workspace_root,
                &paths.project_assets_root,
                &root_config.skills.workspace_dir,
                DEFAULT_WORKSPACE_SKILLS_DIR,
                "skills",
            ),
            project_asset_dir(
                &paths.workspace_root,
                &paths.project_assets_root,
                &root_config.skills.workspace_agents_dir,
                DEFAULT_WORKSPACE_AGENTS_DIR,
                "agents",
            ),
            paths.project_assets_root.join("plugins"),
        ],
        runtime_state_roots: vec![
            paths.workspace_state_root.clone(),
            paths.session_log_dir.clone(),
            paths.input_history_file.clone(),
            paths.artifacts_root.clone(),
            paths.changesets_root.clone(),
            paths.terminal_tasks_root.clone(),
        ],
        user_state_roots: vec![paths.state_root.clone()],
        user_cache_roots: vec![paths.cache_root.clone(), paths.workspace_cache_root.clone()],
        effective_policy_cap: None,
    }
}

/// Builds shared agent run options for one task role.
pub fn build_role_run_options(
    root_config: &RootConfig,
    workspace_root: PathBuf,
    interaction_mode: InteractionMode,
    role: AgentRole,
) -> AgentRunOptions {
    let mut options = build_run_options(root_config, workspace_root, interaction_mode);
    if let Some(reasoning_effort) = root_config.task.role_config(role).reasoning_effort.clone() {
        options.reasoning_effort = Some(reasoning_effort);
    }
    options
}

/// Builds a role-scoped tool registry view over an existing runtime registry.
pub fn build_role_tool_registry(
    registry: &ToolRegistry,
    root_config: &RootConfig,
    role: AgentRole,
) -> ScopedToolRegistry {
    registry.scoped(role_tool_scope(root_config, role))
}

/// Builds the tool registry used by plan-mode prompts.
///
/// Plan mode uses planner-scoped tools for read-only exploration while keeping agent-thread tools
/// visible so explicit delegation can still run through the same child-session contract as chat.
pub fn build_plan_prompt_tool_registry(
    registry: &ToolRegistry,
    root_config: &RootConfig,
) -> ScopedToolRegistry {
    registry.scoped(role_tool_scope(root_config, AgentRole::Planner).union(&agent_tool_scope()))
}

/// Builds the current agent registry further constrained by a loaded skill descriptor.
pub fn build_skill_tool_registry(
    registry: &ToolRegistry,
    skill: &SkillDescriptor,
) -> ScopedToolRegistry {
    let effective_scope = if skill.allowed_tools.is_empty() {
        ToolRegistryScope {
            allow_all: true,
            ..ToolRegistryScope::default()
        }
    } else {
        skill.allowed_tools.clone()
    };
    registry.scoped_with_denies(
        effective_scope,
        skill.disallowed_tools.union(&agent_tool_deny_scope()),
    )
}

/// Builds a role-scoped registry further constrained by a loaded skill descriptor.
pub fn build_role_skill_tool_registry(
    registry: &ToolRegistry,
    root_config: &RootConfig,
    role: AgentRole,
    skill: &SkillDescriptor,
) -> ScopedToolRegistry {
    let role_scope = role_tool_scope(root_config, role);
    let effective_scope = if skill.allowed_tools.is_empty() {
        role_scope
    } else {
        role_scope.intersection(&skill.allowed_tools)
    };
    registry.scoped_with_denies(
        effective_scope,
        skill.disallowed_tools.union(&agent_tool_deny_scope()),
    )
}

fn agent_tool_deny_scope() -> ToolRegistryScope {
    agent_tool_scope()
}

fn agent_tool_scope() -> ToolRegistryScope {
    ToolRegistryScope::from_names_and_prefixes(
        [
            agent_tools::SPAWN_AGENT_TOOL_NAME,
            agent_tools::WAIT_AGENT_TOOL_NAME,
            agent_tools::READ_AGENT_RESULT_TOOL_NAME,
            agent_tools::MESSAGE_AGENT_TOOL_NAME,
            agent_tools::CLOSE_AGENT_TOOL_NAME,
        ],
        std::iter::empty::<&str>(),
    )
}

fn role_tool_scope(root_config: &RootConfig, role: AgentRole) -> ToolRegistryScope {
    let configured = &root_config.task.role_config(role).tools;
    if configured_allowlist_is_empty(configured) {
        default_role_tool_scope(root_config, role)
    } else {
        tool_scope_from_allowlist(configured)
    }
}

/// Parses the DeepSeek provider block from the shared root config.
///
/// # Errors
///
/// Returns an error when `[providers.deepseek]` is missing or malformed.
pub fn load_deepseek_config(root_config: &RootConfig) -> Result<DeepSeekProviderConfig> {
    let provider_config_value = root_config
        .providers
        .get("deepseek")
        .cloned()
        .ok_or_else(|| anyhow!("missing [providers.deepseek] in sigil.toml"))?;
    serde_json::from_value(provider_config_value).context("invalid deepseek provider config")
}

/// Parses the OpenAI-compatible provider block from the shared root config.
///
/// # Errors
///
/// Returns an error when `[providers.openai_compat]` is missing or malformed.
pub fn load_openai_compat_config(
    root_config: &RootConfig,
) -> Result<OpenAiCompatibleProviderConfig> {
    let provider_config_value = root_config
        .providers
        .get("openai_compat")
        .cloned()
        .ok_or_else(|| anyhow!("missing [providers.openai_compat] in sigil.toml"))?;
    serde_json::from_value(provider_config_value).context("invalid openai_compat provider config")
}

/// Parses the Anthropic provider block from the shared root config.
///
/// # Errors
///
/// Returns an error when `[providers.anthropic]` is missing or malformed.
pub fn load_anthropic_config(root_config: &RootConfig) -> Result<AnthropicProviderConfig> {
    let provider_config_value = root_config
        .providers
        .get("anthropic")
        .cloned()
        .ok_or_else(|| anyhow!("missing [providers.anthropic] in sigil.toml"))?;
    serde_json::from_value(provider_config_value).context("invalid anthropic provider config")
}

/// Parses the Gemini provider block from the shared root config.
///
/// # Errors
///
/// Returns an error when `[providers.gemini]` is missing or malformed.
pub fn load_gemini_config(root_config: &RootConfig) -> Result<GeminiProviderConfig> {
    let provider_config_value = root_config
        .providers
        .get("gemini")
        .cloned()
        .ok_or_else(|| anyhow!("missing [providers.gemini] in sigil.toml"))?;
    serde_json::from_value(provider_config_value).context("invalid gemini provider config")
}

/// Source used for a resolved runtime secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretSource {
    Environment(&'static str),
    ConfigPlaintext,
    Session,
}

/// A resolved secret value and the storage layer it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretResolution {
    pub value: String,
    pub source: SecretSource,
}

/// Resolves DeepSeek configuration with runtime overrides applied.
///
/// # Errors
///
/// Returns an error when provider config is missing, malformed, or an environment override is
/// invalid.
pub fn resolve_deepseek_config(root_config: &RootConfig) -> Result<DeepSeekProviderConfig> {
    load_deepseek_config(root_config)?.resolved()
}

/// Resolves OpenAI-compatible configuration with runtime overrides applied.
///
/// # Errors
///
/// Returns an error when provider config is missing, malformed, or an environment override is
/// invalid.
pub fn resolve_openai_compat_config(
    root_config: &RootConfig,
) -> Result<OpenAiCompatibleProviderConfig> {
    load_openai_compat_config(root_config)?.resolved()
}

/// Resolves Anthropic configuration with runtime overrides applied.
///
/// # Errors
///
/// Returns an error when provider config is missing, malformed, or an environment override is
/// invalid.
pub fn resolve_anthropic_config(root_config: &RootConfig) -> Result<AnthropicProviderConfig> {
    load_anthropic_config(root_config)?.resolved()
}

/// Resolves Gemini configuration with runtime overrides applied.
///
/// # Errors
///
/// Returns an error when provider config is missing, malformed, or an environment override is
/// invalid.
pub fn resolve_gemini_config(root_config: &RootConfig) -> Result<GeminiProviderConfig> {
    load_gemini_config(root_config)?.resolved()
}

#[must_use]
pub fn resolve_deepseek_api_key(config: &DeepSeekProviderConfig) -> Option<SecretResolution> {
    resolve_deepseek_api_key_with_session(config, None)
}

#[must_use]
pub fn resolve_deepseek_api_key_with_session(
    config: &DeepSeekProviderConfig,
    session_value: Option<&str>,
) -> Option<SecretResolution> {
    for name in [SIGIL_API_KEY_ENV, LEGACY_DEEPSEEK_API_KEY_ENV] {
        if let Some(value) = read_secret_env(name) {
            return Some(SecretResolution {
                value,
                source: SecretSource::Environment(name),
            });
        }
    }
    if let Some(value) = session_value
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(SecretResolution {
            value: value.to_owned(),
            source: SecretSource::Session,
        });
    }
    config
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| SecretResolution {
            value: value.to_owned(),
            source: SecretSource::ConfigPlaintext,
        })
}

#[must_use]
pub fn resolve_openai_compat_api_key(
    config: &OpenAiCompatibleProviderConfig,
) -> Option<SecretResolution> {
    resolve_openai_compat_api_key_with_session(config, None)
}

#[must_use]
pub fn resolve_openai_compat_api_key_with_session(
    config: &OpenAiCompatibleProviderConfig,
    session_value: Option<&str>,
) -> Option<SecretResolution> {
    for name in [OPENAI_COMPATIBLE_API_KEY_ENV, OPENAI_API_KEY_ENV] {
        if let Some(value) = read_secret_env(name) {
            return Some(SecretResolution {
                value,
                source: SecretSource::Environment(name),
            });
        }
    }
    if let Some(value) = session_value
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(SecretResolution {
            value: value.to_owned(),
            source: SecretSource::Session,
        });
    }
    config
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| SecretResolution {
            value: value.to_owned(),
            source: SecretSource::ConfigPlaintext,
        })
}

#[must_use]
pub fn resolve_anthropic_api_key(config: &AnthropicProviderConfig) -> Option<SecretResolution> {
    resolve_anthropic_api_key_with_session(config, None)
}

#[must_use]
pub fn resolve_anthropic_api_key_with_session(
    config: &AnthropicProviderConfig,
    session_value: Option<&str>,
) -> Option<SecretResolution> {
    for name in [SIGIL_ANTHROPIC_API_KEY_ENV, ANTHROPIC_API_KEY_ENV] {
        if let Some(value) = read_secret_env(name) {
            return Some(SecretResolution {
                value,
                source: SecretSource::Environment(name),
            });
        }
    }
    if let Some(value) = session_value
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(SecretResolution {
            value: value.to_owned(),
            source: SecretSource::Session,
        });
    }
    config
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| SecretResolution {
            value: value.to_owned(),
            source: SecretSource::ConfigPlaintext,
        })
}

#[must_use]
pub fn resolve_gemini_api_key(config: &GeminiProviderConfig) -> Option<SecretResolution> {
    resolve_gemini_api_key_with_session(config, None)
}

#[must_use]
pub fn resolve_gemini_api_key_with_session(
    config: &GeminiProviderConfig,
    session_value: Option<&str>,
) -> Option<SecretResolution> {
    for name in [
        SIGIL_GEMINI_API_KEY_ENV,
        GEMINI_API_KEY_ENV,
        GOOGLE_API_KEY_ENV,
    ] {
        if let Some(value) = read_secret_env(name) {
            return Some(SecretResolution {
                value,
                source: SecretSource::Environment(name),
            });
        }
    }
    if let Some(value) = session_value
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(SecretResolution {
            value: value.to_owned(),
            source: SecretSource::Session,
        });
    }
    config
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| SecretResolution {
            value: value.to_owned(),
            source: SecretSource::ConfigPlaintext,
        })
}

#[must_use]
pub fn secret_redactor_for_root_config(root_config: &RootConfig) -> SecretRedactor {
    let mut redactor = SecretRedactor::empty();
    if let Ok(config) = load_deepseek_config(root_config)
        && let Some(api_key) = resolve_deepseek_api_key(&config)
    {
        redactor.add_secret(api_key.value);
    }
    if let Ok(config) = load_openai_compat_config(root_config)
        && let Some(api_key) = resolve_openai_compat_api_key(&config)
    {
        redactor.add_secret(api_key.value);
    }
    if let Ok(config) = load_anthropic_config(root_config)
        && let Some(api_key) = resolve_anthropic_api_key(&config)
    {
        redactor.add_secret(api_key.value);
    }
    if let Ok(config) = load_gemini_config(root_config)
        && let Some(api_key) = resolve_gemini_api_key(&config)
    {
        redactor.add_secret(api_key.value);
    }
    redactor
}

fn read_secret_env(name: &'static str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn canonical_workspace_root(workspace_root: PathBuf) -> PathBuf {
    workspace_root.canonicalize().unwrap_or(workspace_root)
}

fn default_reasoning_effort(root_config: &RootConfig) -> ReasoningEffort {
    if provider_config_key(&root_config.agent.provider) == "deepseek"
        && let Ok(config) = load_deepseek_config(root_config)
    {
        return config.profile().default_reasoning_effort;
    }
    ReasoningEffort::Max
}

fn root_config_with_role_agent(
    root_config: &RootConfig,
    role_config: &RoleModelConfig,
) -> RootConfig {
    let mut resolved = root_config.clone();
    if let Some(provider) = role_config.provider.as_deref() {
        resolved.agent.provider = provider.to_owned();
    }
    if let Some(model) = role_config.model.as_deref() {
        resolved.agent.model = model.to_owned();
        let provider_key = provider_config_key(&resolved.agent.provider).to_owned();
        if let Some(provider_config) = resolved.providers.get_mut(&provider_key)
            && let Some(object) = provider_config.as_object_mut()
        {
            object.insert("model".to_owned(), Value::String(model.to_owned()));
        }
    }
    resolved
}

fn configured_allowlist_is_empty(config: &ToolAllowlistConfig) -> bool {
    !config.allow_all && config.names.is_empty() && config.prefixes.is_empty()
}

fn tool_scope_from_allowlist(config: &ToolAllowlistConfig) -> ToolRegistryScope {
    ToolRegistryScope {
        allow_all: config.allow_all,
        names: config.names.iter().cloned().collect(),
        prefixes: config.prefixes.clone(),
    }
}

fn default_role_tool_scope(root_config: &RootConfig, role: AgentRole) -> ToolRegistryScope {
    match role {
        AgentRole::Planner | AgentRole::SubagentRead => read_only_role_tool_scope(),
        AgentRole::Executor => ToolRegistryScope {
            allow_all: true,
            ..ToolRegistryScope::default()
        },
        AgentRole::SubagentWrite if root_config.task.allow_write_subagents => ToolRegistryScope {
            allow_all: true,
            ..ToolRegistryScope::default()
        },
        AgentRole::SubagentWrite => read_only_role_tool_scope(),
    }
}

fn read_only_role_tool_scope() -> ToolRegistryScope {
    ToolRegistryScope::from_names_and_prefixes(
        [
            "read_file",
            "ls",
            "glob",
            "grep",
            "code_symbols",
            "code_workspace_symbols",
            "code_definition",
            "code_references",
            "code_diagnostics",
            LOAD_SKILL_TOOL_NAME,
        ],
        std::iter::empty::<&str>(),
    )
}

#[must_use]
pub fn provider_config_key(provider: &str) -> &str {
    match provider {
        "openai-compatible" | "openai_compatible" => "openai_compat",
        "claude" => "anthropic",
        "google" | "google_gemini" | "google-gemini" => "gemini",
        other => other,
    }
}

fn workspace_partition_key(workspace_root: &std::path::Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(workspace_root.to_string_lossy().as_bytes());
    let digest = hasher.finalize();
    format!("workspace-{digest:x}")
}

#[cfg(test)]
#[path = "tests/lib_tests.rs"]
mod tests;
