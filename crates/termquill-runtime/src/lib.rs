use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use termquill_kernel::{
    AgentRunOptions, InteractionMode, Provider, ReasoningEffort, RootConfig, ToolRegistry,
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
pub async fn build_tool_registry(root_config: &RootConfig) -> Result<ToolRegistry> {
    let mut registry = ToolRegistry::new();
    termquill_tools_builtin::register_builtin_tools(&mut registry);
    termquill_mcp::register_mcp_tools(&mut registry, &root_config.mcp_servers).await?;
    Ok(registry)
}

/// Builds shared agent run options for CLI, TUI, and future entrypoints.
pub fn build_run_options(
    root_config: &RootConfig,
    workspace_root: PathBuf,
    interaction_mode: InteractionMode,
) -> AgentRunOptions {
    AgentRunOptions {
        workspace_root,
        max_turns: root_config.agent.max_turns,
        tool_timeout_secs: root_config.agent.tool_timeout_secs,
        reasoning_effort: Some(ReasoningEffort::Max),
        traffic_partition_key: Some("local-user".to_owned()),
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

#[cfg(test)]
#[path = "tests/lib_tests.rs"]
mod tests;
