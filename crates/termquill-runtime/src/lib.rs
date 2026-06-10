use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use sha2::{Digest, Sha256};
use termquill_kernel::{
    AgentRunOptions, InteractionMode, Provider, ProviderCapabilities, ReasoningEffort, RootConfig,
    ToolRegistry,
};
use termquill_provider_deepseek::{DeepSeekProvider, DeepSeekProviderConfig};

/// Builds the configured model provider for runtime entrypoints.
///
/// # Errors
///
/// Returns an error when the configured provider is unsupported or its provider-specific
/// configuration cannot be parsed or initialized.
pub fn build_provider(root_config: &RootConfig) -> Result<Box<dyn Provider>> {
    match root_config.agent.provider.as_str() {
        "deepseek" => Ok(Box::new(DeepSeekProvider::new(load_deepseek_config(
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
    let mut registry = ToolRegistry::new();
    termquill_tools_builtin::register_builtin_tools(&mut registry);
    termquill_code_intel::register_code_intelligence_tools(
        &mut registry,
        &root_config.code_intelligence,
        workspace_root.clone(),
    );
    termquill_mcp::register_mcp_tools_with_capabilities_and_roots(
        &mut registry,
        &root_config.mcp_servers,
        provider_capabilities,
        vec![canonical_workspace_root(workspace_root)],
    )
    .await?;
    Ok(registry)
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
        .ok_or_else(|| anyhow!("missing [providers.deepseek] in termquill.toml"))?;
    serde_json::from_value(provider_config_value).context("invalid deepseek provider config")
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
