use std::{env, path::PathBuf, sync::Arc};

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    AgentRole, AgentRunOptions, ApprovalMode, InteractionMode, McpServerConfig, McpServerStartup,
    Provider, ProviderCapabilities, ReasoningEffort, RoleModelConfig, RootConfig,
    ScopedToolRegistry, SecretRedactor, Tool, ToolAccess, ToolAllowlistConfig, ToolCategory,
    ToolContext, ToolEgressAudit, ToolErrorKind, ToolPreviewCapability, ToolRegistry,
    ToolRegistryScope, ToolResult, ToolResultMeta, ToolSpec, ToolSubject, ToolSubjectKind,
    ToolSubjectScope,
};
pub use sigil_mcp::{
    McpElicitationAction, McpElicitationHandler, McpElicitationRequest, McpElicitationResponse,
    McpListChangedKind, McpListChangedNotification, McpProgressNotification,
    McpRuntimeEventHandler,
};
use sigil_provider_deepseek::{
    DeepSeekProvider, DeepSeekProviderConfig, LEGACY_DEEPSEEK_API_KEY_ENV, SIGIL_API_KEY_ENV,
};
use sigil_provider_openai_compat::{
    OPENAI_API_KEY_ENV, OPENAI_COMPATIBLE_API_KEY_ENV, OpenAiCompatibleProvider,
    OpenAiCompatibleProviderConfig,
};

pub mod doctor;

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
        other => Err(anyhow!("unsupported provider {other}")),
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
    sigil_tools_builtin::register_builtin_tools(&mut registry);
    sigil_code_intel::register_code_intelligence_tools(
        &mut registry,
        &root_config.code_intelligence,
        workspace_root.clone(),
    );
    sigil_mcp::register_mcp_tools_with_options(
        &mut registry,
        &root_config.mcp_servers,
        sigil_mcp::McpToolRegistrationOptions::eager()?
            .with_capabilities(provider_capabilities)
            .with_roots(vec![canonical_workspace_root(workspace_root.clone())])
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
    sigil_mcp::register_mcp_tools_with_options(
        registry,
        &servers,
        sigil_mcp::McpToolRegistrationOptions::lazy()?
            .with_capabilities(provider_capabilities)
            .with_roots(vec![canonical_workspace_root(workspace_root)])
            .with_secret_redactor(secret_redactor_for_root_config(root_config))
            .with_elicitation_handler(elicitation_handler)
            .with_runtime_event_handler(runtime_event_handler),
    )
    .await?;
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
    if let Err(error) = sigil_mcp::register_mcp_tools_with_options(
        registry,
        &servers,
        sigil_mcp::McpToolRegistrationOptions::for_startup(server.startup)?
            .with_capabilities(provider_capabilities)
            .with_roots(vec![canonical_workspace_root(workspace_root)])
            .with_secret_redactor(secret_redactor_for_root_config(root_config))
            .with_elicitation_handler(elicitation_handler)
            .with_runtime_event_handler(runtime_event_handler),
    )
    .await
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

    async fn execute(&self, _ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
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
        let result = activate_lazy_mcp_tools_detailed_with_mcp_handlers(
            &mut registry,
            &self.root_config,
            &self.provider_capabilities,
            self.workspace_root.clone(),
            Some(server_name),
            Arc::clone(&self.elicitation_handler),
            Arc::clone(&self.runtime_event_handler),
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
    AgentRunOptions {
        traffic_partition_key: Some(workspace_partition_key(&workspace_root)),
        workspace_root,
        max_turns: root_config.agent.max_turns,
        tool_timeout_secs: root_config.agent.tool_timeout_secs,
        reasoning_effort: Some(default_reasoning_effort(root_config)),
        interaction_mode,
        permission_config: root_config.permission.clone(),
        memory_config: root_config.memory.clone(),
        compaction_config: root_config.compaction.clone(),
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
    let configured = &root_config.task.role_config(role).tools;
    let scope = if configured_allowlist_is_empty(configured) {
        default_role_tool_scope(root_config, role)
    } else {
        tool_scope_from_allowlist(configured)
    };
    registry.scoped(scope)
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
        ],
        std::iter::empty::<&str>(),
    )
}

fn provider_config_key(provider: &str) -> &str {
    match provider {
        "openai-compatible" | "openai_compatible" => "openai_compat",
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
