use std::{env, path::PathBuf, sync::Arc};

use anyhow::{Context, Result, anyhow};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    AgentRunOptions, InteractionMode, McpServerStartup, Provider, ProviderCapabilities,
    ReasoningEffort, RootConfig, SecretRedactor, ToolRegistry,
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
    sigil_mcp::register_mcp_tools_with_capabilities_roots_secrets_and_elicitation(
        &mut registry,
        &root_config.mcp_servers,
        provider_capabilities,
        vec![canonical_workspace_root(workspace_root)],
        secret_redactor_for_root_config(root_config),
        elicitation_handler,
    )
    .await?;
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
    sigil_mcp::activate_lazy_mcp_tools_with_capabilities_roots_secrets_and_elicitation(
        registry,
        &servers,
        provider_capabilities,
        vec![canonical_workspace_root(workspace_root)],
        secret_redactor_for_root_config(root_config),
        elicitation_handler,
    )
    .await?;
    Ok(LazyMcpActivationResult {
        matched_servers: servers.len(),
        added_tools: registry.specs().len().saturating_sub(before),
    })
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
