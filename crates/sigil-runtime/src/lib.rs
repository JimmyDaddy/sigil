use std::{env, path::PathBuf, sync::Arc};

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
pub use sigil_kernel::ExtensionProcessNetworkAdmission;
use sigil_kernel::{
    AgentRole, AgentRunOptions, ApprovalMode, ExecutionBackend, InteractionMode, McpServerConfig,
    McpServerStartup, MutationEventRecorder, NetworkEffect, NetworkPolicy,
    PermissionEvaluationContext, Provider, ProviderCapabilities, ReasoningEffort, RoleModelConfig,
    RootConfig, ScopedToolRegistry, SecretRedactor, SkillDescriptor, Tool, ToolAccess,
    ToolAllowlistConfig, ToolCategory, ToolContext, ToolEgressAudit, ToolErrorKind, ToolOperation,
    ToolPreviewCapability, ToolRegistry, ToolRegistryScope, ToolResult, ToolResultMeta, ToolSpec,
    ToolSubject, ToolSubjectKind, ToolSubjectScope, WorkspaceTrust, default_user_config_dir,
};
pub use sigil_mcp::{
    McpDeclarationLaunchMetadata, McpElicitationAction, McpElicitationHandler,
    McpElicitationRequest, McpElicitationResponse, McpListChangedKind, McpListChangedNotification,
    McpOAuthRevocationOutcome, McpProcessCoverage, McpProcessLaunch, McpProcessLaunchReceipt,
    McpProcessLaunchRequest, McpProcessLauncher, McpProgressNotification, McpRuntimeEventHandler,
    McpToolRegistrationReport, mcp_transport_static_fingerprint,
    unsupported_mcp_elicitation_handler, unsupported_mcp_runtime_event_handler,
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
use sigil_provider_openai_responses::{
    OPENAI_RESPONSES_API_KEY_ENV, OpenAiResponsesProvider, OpenAiResponsesProviderConfig,
    openai_responses_capabilities,
};
use tokio::process::Command;

#[cfg(test)]
#[macro_use]
#[path = "tests/mcp_config_macros.rs"]
mod mcp_config_macros;

#[cfg(test)]
#[path = "tests/test_env.rs"]
pub(crate) mod test_env;

#[cfg(test)]
#[path = "tests/reasoning_effort_tests.rs"]
mod reasoning_effort_tests;

#[cfg(test)]
#[path = "tests/application_catalog_tests.rs"]
mod application_catalog_tests;

mod mcp_registry; // local/MCP tool registry construction and activation.
mod plugin_manifest_io; // bounded regular-file reads shared by discovery and activation.
mod provider_factory; // provider construction, capabilities, and secrets.
mod reasoning_effort; // exact provider+model effort admission and stale bindings.
mod remote_mcp; // user-root Streamable HTTP activation and raw tool adapters.
mod run_options; // shared run options and scoped tool registry views.

pub mod agent_profile_registry;
pub mod agent_supervisor;
pub mod agent_tools;
pub mod application_catalog;
pub mod application_run;
pub mod context;
pub mod context_window;
pub mod doctor;
pub mod egress_ordering;
mod exa_text_v1;
pub mod hosted_finalizer;
mod hosted_web_search;
pub mod image_attachment;
pub mod machine_protocol;
pub mod mcp_declaration;
pub mod mcp_oauth;
pub mod mcp_oauth_flow;
pub mod mcp_oauth_http;
pub mod model_eval;
pub mod paths;
pub mod plugins;
pub mod portable_compaction;
pub mod product_view;
pub mod provider_config;
pub mod provider_debug;
pub mod provider_status;
pub mod session_control;
pub mod session_lifecycle;
pub mod skills;
#[allow(dead_code)] // E21.15 runtime-private route is intentionally dormant before E21.17.
mod stable_mcp_search;
pub mod streamable_http;
pub mod support;
pub mod url_capability;
pub mod web_destination;
mod web_fetch_tool;
pub mod web_search_connector;
mod web_search_tool;
pub mod webfetch;
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
pub use application_catalog::{
    APPLICATION_COMMANDS, ApplicationAgentBinding, ApplicationAgentCatalogEntry,
    ApplicationClientAction, ApplicationCommandCatalogEntry, ApplicationCommandSpec,
    ApplicationExtensionCatalogView, ApplicationSkillBinding, ApplicationSkillCatalogEntry,
    application_extension_catalog_view,
};
pub use context::{
    ContextSourcePolicy, ContextSourceProvider, ContextSourceRequest, McpResourceContextItem,
    McpResourceContextProvider, PluginHookContextProvider, RequestContextResolver,
    collect_context_from_source_provider, collect_context_from_source_providers,
    context_candidates_from_repo_query, context_candidates_from_safe_sources,
    context_items_from_plugin_hook_output, context_items_from_task_memory,
};
pub use context_window::{
    ContextWindowSource, ResolvedContextWindow, effective_compaction_config,
    resolve_context_window_tokens,
};
pub use egress_ordering::{
    ActiveHostedEgress, ActiveQueryEgress, AuthorizedHostedEgress, AuthorizedQueryEgress,
    AuthorizedTransportEgress, EgressOrderingCoordinator, EgressOrderingError,
};
pub use hosted_finalizer::{HostedEvidenceFinalizer, hosted_terminal_status};
pub use image_attachment::{ControlledImageAttachmentCache, image_path_from_pasted_text};
pub use mcp_declaration::{
    McpConfigOrigin, McpConfigOriginKind, McpExecutionBase, McpExecutionBaseKind,
    McpRegistrationError, McpRegistrationErrorCode, McpServerDeclarationProjection,
    PluginManifestAttestation, ResolvedMcpServerDeclaration, ResolvedMcpStdioLaunch,
    resolve_user_root_mcp_declarations,
};
pub use mcp_oauth::{McpOAuthCredentialManager, RuntimeMcpOAuthBearerProvider};
pub use mcp_oauth_flow::{
    McpOAuthAuthErrorCode, McpOAuthAuthPhase, McpOAuthAuthStatus, McpOAuthFlowControl,
    McpOAuthFlowError, McpOAuthPreparedFlow, McpOAuthRuntimeService,
};
pub use mcp_oauth_http::{RuntimeMcpOAuthHttpExecutor, runtime_mcp_oauth_executor_for_user_action};
pub use paths::{
    DEFAULT_ARTIFACTS_DIR, DEFAULT_ATTACHMENTS_DIR, DEFAULT_CHANGESETS_DIR,
    DEFAULT_PROJECT_ASSETS_DIR, DEFAULT_PROJECTIONS_DIR, DEFAULT_SCRATCH_DIR,
    DEFAULT_SESSION_CATALOG_DB_FILE, DEFAULT_SESSION_EXPORTS_DIR,
    DEFAULT_SESSION_LIFECYCLE_JOURNAL_FILE, DEFAULT_SESSIONS_DIR, DEFAULT_TERMINAL_TASKS_DIR,
    DEFAULT_WORKSPACE_AGENTS_LEAF, DEFAULT_WORKSPACE_COMMANDS_LEAF, DEFAULT_WORKSPACE_PLUGINS_LEAF,
    DEFAULT_WORKSPACE_SKILLS_LEAF, INPUT_HISTORY_FILE, PathResolverEnv, SIGIL_CACHE_HOME_ENV,
    SIGIL_STATE_HOME_ENV, SigilPaths, StoragePlatform, resolve_sigil_paths,
    resolve_sigil_paths_with_env, workspace_id_for_root,
};
pub use plugins::{
    PluginDiscoveryReport, PluginDiscoveryWarning, PluginDiscoveryWarningKind,
    PluginHookExecutionOutcome, PluginHookExecutionRequest, PluginHookExecutionRunner,
    PluginHookRegistration, PluginMcpServerRegistration, PluginRegistrations,
    discover_workspace_plugins, merge_mcp_server_declarations, merge_plugin_skill_descriptors,
};
pub use portable_compaction::{
    DeepSeekV4FlashPortableTargetPressure, deepseek_v4_flash_portable_target_material,
    deepseek_v4_flash_portable_target_material_with_economics,
    deepseek_v4_flash_portable_target_output_tokens, deepseek_v4_flash_portable_target_pressure,
    deepseek_v4_flash_portable_target_proof, install_default_deepseek_v4_flash_tokenizer,
    is_deepseek_v4_flash_portable_target_profile, is_openai_responses_portable_target_profile,
    portable_compaction_target_output_tokens, require_default_deepseek_v4_flash_portable_transport,
};
pub use product_view::{
    AgentGraphProductSummary, ApplicationAgentActivityItem, ApplicationAgentActivityStatus,
    ApplicationAgentActivityView, ApplicationAgentHandoffStatus, ApplicationAgentUsageSummary,
    agent_activity_product_view_from_entries, agent_graph_product_summary_from_entries,
    agent_graph_product_summary_from_session_log,
};
pub use provider_config::{
    ANTHROPIC_PROVIDER_KEY, DEEPSEEK_PROVIDER_KEY, DEFAULT_SETUP_API_KEY_ENV,
    DEFAULT_SETUP_PROVIDER_KEY, DeepSeekProviderConfigFields, GEMINI_PROVIDER_KEY,
    ModelRequestConfigFields, OPENAI_COMPAT_PROVIDER_KEY, OPENAI_RESPONSES_PROVIDER_KEY,
    PROVIDER_KEYS, ProviderConfigFields, ProviderStatusConfig, ProviderStrictToolsMode,
    deepseek_provider_config_fields, deepseek_provider_status_config,
    deepseek_provider_value_for_setup, default_provider_config_fields,
    default_setup_provider_model, model_request_config_fields, next_provider_name,
    normalize_provider_model_alias, normalize_provider_name, provider_api_key_env_name,
    provider_balance_status_config, provider_config_fields, provider_model_status_config,
    provider_model_status_config_from_fields, provider_status_config_from_fields,
    set_active_provider_model, set_model_request_config_fields, set_provider_config_fields,
    supported_provider_name,
};
pub use provider_debug::{
    DeepSeekFimDebugRequest, DeepSeekPrefixDebugRequest, ProviderDebugStream,
    stream_deepseek_fim_debug, stream_deepseek_prefix_debug,
};
pub use provider_status::{
    BalanceSnapshot, ProviderStatusTaskManager, ProviderStatusTaskResult,
    fetch_provider_balance_snapshot, fetch_remote_model_ids,
};
pub use remote_mcp::{
    activate_eager_remote_mcp_server, activate_or_refresh_configured_remote_mcp_server,
    activate_remote_mcp_server, deactivate_configured_remote_mcp_server,
};
pub use session_control::{
    append_session_control_entries, append_session_control_entries_and_track_detached,
    current_unix_time_ms,
};
pub use session_lifecycle::{
    DEFAULT_SESSION_CATALOG_MAX_ENTRIES, DEFAULT_SESSION_CATALOG_MAX_STREAM_BYTES,
    DEFAULT_SESSION_CATALOG_MAX_TOTAL_VALIDATION_BYTES, DEFAULT_SESSION_CATALOG_PAGE_SIZE,
    DEFAULT_SESSION_EXPORT_MAX_BYTES, DEFAULT_SESSION_EXPORT_MAX_MESSAGES, LocalSessionCatalog,
    LocalSessionCatalogEntry, LocalSessionCatalogState, LocalSessionDeleteJournalBinding,
    LocalSessionDisplayNameJournalBinding, LocalSessionExportJournalBinding,
    LocalSessionLifecycleEvent, LocalSessionLifecycleLimits, LocalSessionLifecycleOperationKind,
    LocalSessionLifecycleRecord, LocalSessionLifecycleRecoveryEntry,
    LocalSessionLifecycleRecoveryStatus, LocalSessionLifecycleService, LocalSessionMutationError,
    LocalSessionPinJournalBinding, LocalSessionReopenBinding, LocalSessionReopenError,
    LocalSessionRetentionJournalBinding, MAX_SESSION_CATALOG_PAGE_SIZE,
    SESSION_CATALOG_APPLICATION_ID, SESSION_CATALOG_SCHEMA_VERSION, SESSION_EXPORT_SCHEMA_VERSION,
    SessionCatalogInvalidSourceDeleteReceipt, SessionCatalogMutationReceipt,
    SessionCatalogProjectionEntry, SessionCatalogProjectionError, SessionCatalogProjectionPage,
    SessionCatalogProjectionQuery, SessionCatalogProjectionRebuildReport,
    SessionCatalogProjectionReconcileReport, SessionCatalogProjectionRecoveryReport,
    SessionCatalogProjectionService, SessionCatalogQuarantineReceipt, SessionDeleteOutput,
    SessionDeletePreview, SessionExportMessageV1, SessionExportOutput, SessionExportPayloadV1,
    SessionExportV1, SessionRetentionCandidate, SessionRetentionOutput, SessionRetentionPolicy,
    SessionRetentionPreview, SessionRetentionReason,
};
pub use skills::{
    LOAD_SKILL_TOOL_NAME, LoadedSkillContext, SkillDiscoveryReport, SkillDiscoveryWarning,
    SkillDiscoveryWarningKind, discover_skill_index, discover_skill_index_with_user_dir,
    load_user_invoked_skill, namespaced_plugin_skill_id, register_skill_tools,
};
pub use stable_mcp_search::{
    BundledExaAuthorizerFactory, RuntimeStableSearchQueryAttempt,
    RuntimeStableSearchQueryPermitFactory, StableSearchQueryAttemptFactory,
};
pub use streamable_http::{
    QueuedRuntimeMcpStreamableHttpAttemptFactory, RuntimeMcpStreamableHttpAttempt,
    RuntimeMcpStreamableHttpAttemptFactory, RuntimeMcpStreamableHttpDestinationAuthorizer,
    RuntimeMcpTransportAttemptFactory,
};
pub use url_capability::{
    DEFAULT_URL_CAPABILITY_CAPACITY, DEFAULT_URL_CAPABILITY_TTL, UrlCapabilityLookupError,
    WebUrlCapability, WebUrlCapabilityStore, attach_session_url_capability_store,
};
pub use web_destination::{
    IpCidr, ProxyEnvironment, SystemWebDestinationResolver, WebDestinationError,
    WebDestinationGuard, WebDestinationGuardPolicy, WebDestinationPreview, WebDestinationResolver,
};
pub use web_search_connector::{
    ConfiguredMcpSearchBindingState, McpSearchBindingOrigin, McpSearchBindingRegistry,
    McpSearchBindingRegistryError, PendingMcpSearchBinding, PreparedMcpSearchBinding,
    PreparedMcpSearchLease, SourceProjection, SourceProjectionUnavailableReason,
    StableMcpRouteSelection, WebSearchConnector, WebSearchConnectorError,
    WebSearchConnectorIdentity, WebSearchFailure, WebSearchProtocolFailureKind, WebSearchRequest,
    WebSearchResponse, WebSearchSourceCapability, generic_query_arguments,
    normalize_web_search_query,
};
pub use webfetch::{
    WebFetchExecutionError, WebFetchExecutionOutcome, WebFetchExecutionRequest, WebFetchExecutor,
    WebFetchHopTransport,
};

pub use mcp_registry::{
    LazyMcpActivationResult, McpDeclarationRegistrationOptions, McpPluginTrustSource,
    McpRefreshResult, RuntimeToolSurface, SessionMcpPluginTrustSource, activate_lazy_mcp_tools,
    activate_lazy_mcp_tools_detailed, activate_lazy_mcp_tools_detailed_with_mcp_elicitation,
    activate_lazy_mcp_tools_detailed_with_mcp_handlers,
    activate_lazy_mcp_tools_detailed_with_mcp_handlers_and_mutation_recorder,
    activate_lazy_mcp_tools_detailed_with_mcp_handlers_and_mutation_recorder_and_network_admission,
    activate_mcp_tools_from_product_surface, attach_remote_mcp_activation_presenter,
    build_configured_execution_backend, build_tool_registry,
    build_tool_registry_with_mcp_elicitation, build_tool_registry_with_mcp_handlers,
    build_tool_registry_with_mutation_recorder,
    build_tool_registry_with_mutation_recorder_and_workspace_trust,
    build_tool_registry_with_mutation_recorder_and_workspace_trust_and_network_admission,
    build_tool_registry_without_eager_mcp,
    build_tool_registry_without_eager_mcp_with_workspace_trust,
    build_tool_surface_with_mutation_recorder_and_workspace_trust_and_network_admission,
    build_tool_surface_without_eager_mcp_with_workspace_trust, mcp_process_receipts_summary,
    mcp_stdio_boundary_summary, refresh_mcp_server_tools_from_product_surface,
    refresh_mcp_server_tools_with_mcp_handlers,
    refresh_mcp_server_tools_with_mcp_handlers_and_mutation_recorder,
    refresh_mcp_server_tools_with_mcp_handlers_and_mutation_recorder_and_network_admission,
    register_mcp_server_declarations,
};
pub use provider_factory::{
    ProviderCapabilityRow, ProviderCapabilityStatus, ProviderCapabilityView, SecretResolution,
    SecretSource, build_provider, build_role_provider, load_anthropic_config, load_deepseek_config,
    load_gemini_config, load_openai_compat_config, load_openai_responses_config,
    provider_capabilities_for_name, provider_capability_view, provider_config_key,
    resolve_anthropic_api_key, resolve_anthropic_api_key_with_session, resolve_anthropic_config,
    resolve_deepseek_api_key, resolve_deepseek_api_key_with_session, resolve_deepseek_config,
    resolve_gemini_api_key, resolve_gemini_api_key_with_session, resolve_gemini_config,
    resolve_openai_compat_api_key, resolve_openai_compat_api_key_with_session,
    resolve_openai_compat_config, resolve_openai_responses_api_key,
    resolve_openai_responses_api_key_with_session, resolve_openai_responses_config,
    secret_redactor_for_root_config,
};
pub use run_options::{
    build_plan_prompt_tool_registry, build_role_run_options, build_role_skill_tool_registry,
    build_role_tool_registry, build_run_options, build_skill_tool_registry,
};

use run_options::canonical_workspace_root;

#[cfg(test)]
use mcp_registry::{
    ConfiguredMcpProcessLauncher, launch_planned_mcp_process, register_lazy_mcp_activation_tool,
    shutdown_registered_tools,
};

#[cfg(test)]
#[path = "tests/lib_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "tests/model_eval_tests.rs"]
mod model_eval_tests;
