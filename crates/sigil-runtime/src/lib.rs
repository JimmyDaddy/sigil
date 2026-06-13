use std::{env, path::PathBuf, sync::Arc};

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    AgentRunOptions, ApprovalMode, InteractionMode, McpServerConfig, McpServerStartup, Provider,
    ProviderCapabilities, ReasoningEffort, RootConfig, SecretRedactor, Tool, ToolAccess,
    ToolCategory, ToolContext, ToolEgressAudit, ToolErrorKind, ToolPreviewCapability, ToolRegistry,
    ToolResult, ToolResultMeta, ToolSpec, ToolSubject, ToolSubjectKind, ToolSubjectScope,
};
pub use sigil_mcp::{
    McpElicitationAction, McpElicitationHandler, McpElicitationRequest, McpElicitationResponse,
};
use sigil_provider_deepseek::{
    DeepSeekProvider, DeepSeekProviderConfig, LEGACY_DEEPSEEK_API_KEY_ENV, SIGIL_API_KEY_ENV,
};

/// Builds the configured model provider for runtime entrypoints.
///
/// # Errors
///
/// Returns an error when the configured provider is unsupported or its provider-specific
/// configuration cannot be parsed or initialized.
pub fn build_provider(root_config: &RootConfig) -> Result<Box<dyn Provider>> {
    match root_config.agent.provider.as_str() {
        "deepseek" => Ok(Box::new(DeepSeekProvider::new(resolve_deepseek_config(
            root_config,
        )?)?)),
        other => Err(anyhow!("unsupported provider {other}")),
    }
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
            .with_elicitation_handler(Arc::clone(&elicitation_handler)),
    )
    .await?;
    register_lazy_mcp_activation_tool(
        &mut registry,
        root_config,
        provider_capabilities,
        workspace_root,
        elicitation_handler,
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
            .with_elicitation_handler(elicitation_handler),
    )
    .await?;
    Ok(LazyMcpActivationResult {
        matched_servers: servers.len(),
        added_tools: registry.specs().len().saturating_sub(before),
    })
}

fn register_lazy_mcp_activation_tool(
    registry: &mut ToolRegistry,
    root_config: &RootConfig,
    provider_capabilities: &ProviderCapabilities,
    workspace_root: PathBuf,
    elicitation_handler: Arc<dyn McpElicitationHandler>,
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
    }));
}

#[derive(Clone)]
struct McpActivateServerTool {
    registry: ToolRegistry,
    root_config: RootConfig,
    provider_capabilities: ProviderCapabilities,
    workspace_root: PathBuf,
    elicitation_handler: Arc<dyn McpElicitationHandler>,
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
        let result = activate_lazy_mcp_tools_detailed_with_mcp_elicitation(
            &mut registry,
            &self.root_config,
            &self.provider_capabilities,
            self.workspace_root.clone(),
            Some(server_name),
            Arc::clone(&self.elicitation_handler),
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
pub fn secret_redactor_for_root_config(root_config: &RootConfig) -> SecretRedactor {
    let mut redactor = SecretRedactor::empty();
    if let Ok(config) = load_deepseek_config(root_config)
        && let Some(api_key) = resolve_deepseek_api_key(&config)
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
    if root_config.agent.provider == "deepseek"
        && let Ok(config) = load_deepseek_config(root_config)
    {
        return config.profile().default_reasoning_effort;
    }
    ReasoningEffort::Max
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
