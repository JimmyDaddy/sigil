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
    McpListChangedKind, McpListChangedNotification, McpProcessCoverage, McpProcessLaunch,
    McpProcessLaunchReceipt, McpProcessLaunchRequest, McpProcessLauncher, McpProgressNotification,
    McpRuntimeEventHandler, McpToolRegistrationReport,
};
use sigil_provider_anthropic::{
    AnthropicProvider, AnthropicProviderConfig, SIGIL_ANTHROPIC_API_KEY_ENV, anthropic_capabilities,
};
use sigil_provider_deepseek::{
    DeepSeekProvider, DeepSeekProviderConfig, SIGIL_API_KEY_ENV, deepseek_capabilities,
};
use sigil_provider_gemini::{
    GeminiProvider, GeminiProviderConfig, SIGIL_GEMINI_API_KEY_ENV, gemini_capabilities,
};
use sigil_provider_openai_compat::{
    OPENAI_COMPATIBLE_API_KEY_ENV, OpenAiCompatibleProvider, OpenAiCompatibleProviderConfig,
    openai_compatible_capabilities,
};
use tokio::process::Command;

mod mcp_registry; // local/MCP tool registry construction and activation.
mod provider_factory; // provider construction, capabilities, and secrets.
mod run_options; // shared run options and scoped tool registry views.

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
pub mod provider_debug;
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
    AgentToolRuntime, CANCEL_AGENT_TOOL_NAME, CLOSE_AGENT_TOOL_NAME, LIST_AGENTS_TOOL_NAME,
    MESSAGE_AGENT_TOOL_NAME, ManualAgentInvocationResult, READ_AGENT_RESULT_TOOL_NAME,
    SPAWN_AGENT_TOOL_NAME, WAIT_AGENT_TOOL_NAME, close_agent_thread, register_agent_tools,
    register_agent_tools_with_registry, register_agent_tools_with_workspace,
    register_agent_tools_with_workspace_and_entries,
};
pub use context::{
    ContextSourcePolicy, ContextSourceProvider, ContextSourceRequest, McpResourceContextItem,
    McpResourceContextProvider, PluginHookContextProvider, collect_context_from_source_provider,
    collect_context_from_source_providers, context_candidates_from_repo_query,
    context_candidates_from_safe_sources, context_items_from_plugin_hook_output,
    context_items_from_task_memory,
};
pub use context_window::{
    ContextWindowSource, ResolvedContextWindow, effective_compaction_config,
    resolve_context_window_tokens,
};
pub use paths::{
    DEFAULT_ARTIFACTS_DIR, DEFAULT_CHANGESETS_DIR, DEFAULT_PROJECT_ASSETS_DIR, DEFAULT_SCRATCH_DIR,
    DEFAULT_SESSIONS_DIR, DEFAULT_TERMINAL_TASKS_DIR, DEFAULT_WORKSPACE_AGENTS_LEAF,
    DEFAULT_WORKSPACE_COMMANDS_LEAF, DEFAULT_WORKSPACE_PLUGINS_LEAF, DEFAULT_WORKSPACE_SKILLS_LEAF,
    INPUT_HISTORY_FILE, PathResolverEnv, SIGIL_CACHE_HOME_ENV, SIGIL_STATE_HOME_ENV, SigilPaths,
    StoragePlatform, resolve_sigil_paths, resolve_sigil_paths_with_env, workspace_id_for_root,
};
pub use plugins::{
    PluginDiscoveryReport, PluginDiscoveryWarning, PluginDiscoveryWarningKind,
    PluginHookExecutionOutcome, PluginHookExecutionRequest, PluginHookExecutionRunner,
    PluginHookRegistration, PluginMcpServerRegistration, PluginRegistrations,
    discover_workspace_plugins, merge_plugin_mcp_servers, merge_plugin_skill_descriptors,
};
pub use product_view::{
    AgentGraphProductSummary, agent_graph_product_summary_from_entries,
    agent_graph_product_summary_from_session_log,
};
pub use provider_config::{
    ANTHROPIC_PROVIDER_KEY, DEEPSEEK_PROVIDER_KEY, DEFAULT_SETUP_API_KEY_ENV,
    DEFAULT_SETUP_PROVIDER_KEY, DeepSeekProviderConfigFields, GEMINI_PROVIDER_KEY,
    ModelRequestConfigFields, OPENAI_COMPAT_PROVIDER_KEY, PROVIDER_KEYS, ProviderConfigFields,
    ProviderStatusConfig, ProviderStrictToolsMode, deepseek_provider_config_fields,
    deepseek_provider_status_config, deepseek_provider_value_for_setup,
    default_provider_config_fields, default_setup_provider_model, model_request_config_fields,
    next_provider_name, normalize_provider_model_alias, normalize_provider_name,
    provider_api_key_env_name, provider_balance_status_config, provider_config_fields,
    provider_model_status_config, provider_model_status_config_from_fields,
    provider_status_config_from_fields, set_active_provider_model, set_model_request_config_fields,
    set_provider_config_fields, supported_provider_name,
};
pub use provider_debug::{
    DeepSeekFimDebugRequest, DeepSeekPrefixDebugRequest, ProviderDebugStream,
    stream_deepseek_fim_debug, stream_deepseek_prefix_debug,
};
pub use provider_status::{
    BalanceSnapshot, ProviderStatusTaskManager, ProviderStatusTaskResult,
    fetch_provider_balance_snapshot, fetch_remote_model_ids,
};
pub use session_control::{append_session_control_entries, current_unix_time_ms};
pub use skills::{
    LOAD_SKILL_TOOL_NAME, LoadedSkillContext, SkillDiscoveryReport, SkillDiscoveryWarning,
    SkillDiscoveryWarningKind, discover_skill_index, discover_skill_index_with_user_dir,
    load_user_invoked_skill, namespaced_plugin_skill_id, register_skill_tools,
};

pub use mcp_registry::{
    LazyMcpActivationResult, McpRefreshResult, activate_lazy_mcp_tools,
    activate_lazy_mcp_tools_detailed, activate_lazy_mcp_tools_detailed_with_mcp_elicitation,
    activate_lazy_mcp_tools_detailed_with_mcp_handlers,
    activate_lazy_mcp_tools_detailed_with_mcp_handlers_and_mutation_recorder,
    build_configured_execution_backend, build_tool_registry,
    build_tool_registry_with_mcp_elicitation, build_tool_registry_with_mcp_handlers,
    build_tool_registry_without_eager_mcp, mcp_process_receipts_summary,
    mcp_stdio_boundary_summary, refresh_mcp_server_tools_with_mcp_handlers,
    refresh_mcp_server_tools_with_mcp_handlers_and_mutation_recorder,
};
pub use provider_factory::{
    ProviderCapabilityRow, ProviderCapabilityStatus, ProviderCapabilityView, SecretResolution,
    SecretSource, build_provider, build_role_provider, load_anthropic_config, load_deepseek_config,
    load_gemini_config, load_openai_compat_config, provider_capabilities_for_name,
    provider_capability_view, provider_config_key, resolve_anthropic_api_key,
    resolve_anthropic_api_key_with_session, resolve_anthropic_config, resolve_deepseek_api_key,
    resolve_deepseek_api_key_with_session, resolve_deepseek_config, resolve_gemini_api_key,
    resolve_gemini_api_key_with_session, resolve_gemini_config, resolve_openai_compat_api_key,
    resolve_openai_compat_api_key_with_session, resolve_openai_compat_config,
    secret_redactor_for_root_config,
};
pub use run_options::{
    build_plan_prompt_tool_registry, build_role_run_options, build_role_skill_tool_registry,
    build_role_tool_registry, build_run_options, build_skill_tool_registry,
};

use run_options::canonical_workspace_root;

#[cfg(test)]
use mcp_registry::{ConfiguredMcpProcessLauncher, register_lazy_mcp_activation_tool};

#[cfg(test)]
#[path = "tests/lib_tests.rs"]
mod tests;
