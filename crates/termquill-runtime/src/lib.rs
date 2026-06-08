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
mod tests {
    use std::{collections::BTreeMap, path::Path};

    use anyhow::Result;
    use serde_json::json;
    use termquill_kernel::{
        AgentConfig, InteractionMode, MemoryConfig, PermissionConfig, RootConfig, SessionConfig,
        WorkspaceConfig,
    };

    use super::{build_provider, build_run_options, build_tool_registry, load_deepseek_config};

    fn test_root_config(provider: &str) -> RootConfig {
        RootConfig {
            workspace: WorkspaceConfig {
                root: ".".to_owned(),
            },
            session: SessionConfig {
                log_dir: ".termquill/sessions".to_owned(),
            },
            agent: AgentConfig {
                provider: provider.to_owned(),
                model: "deepseek-v4-flash".to_owned(),
                max_turns: 12,
                tool_timeout_secs: 45,
            },
            permission: PermissionConfig::default(),
            memory: MemoryConfig { enabled: true },
            compaction: termquill_kernel::CompactionConfig::default(),
            providers: BTreeMap::from([(
                "deepseek".to_owned(),
                json!({
                    "base_url": "https://example.com",
                    "beta_base_url": "https://example.com/beta",
                    "anthropic_base_url": "https://example.com/anthropic",
                    "model": "deepseek-v4-flash",
                    "fim_model": "deepseek-v4-pro",
                    "api_key": "test-key",
                    "strict_tools_mode": "auto",
                    "request_timeout_secs": 15
                }),
            )]),
            mcp_servers: Vec::new(),
        }
    }

    #[test]
    fn load_deepseek_config_reads_provider_block() -> Result<()> {
        let config = load_deepseek_config(&test_root_config("deepseek"))?;

        assert_eq!(config.base_url, "https://example.com");
        assert_eq!(config.model, "deepseek-v4-flash");
        assert_eq!(config.fim_model, "deepseek-v4-pro");
        assert_eq!(config.api_key.as_deref(), Some("test-key"));
        Ok(())
    }

    #[test]
    fn build_provider_rejects_unsupported_provider() {
        let error = match build_provider(&test_root_config("other")) {
            Ok(_) => panic!("expected unsupported provider error"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("unsupported provider other"));
    }

    #[test]
    fn build_run_options_carries_shared_runtime_defaults() {
        let workspace_root = Path::new("/tmp/termquill-runtime-test").to_path_buf();
        let options = build_run_options(
            &test_root_config("deepseek"),
            workspace_root.clone(),
            InteractionMode::Interactive,
        );

        assert_eq!(options.workspace_root, workspace_root);
        assert_eq!(options.max_turns, 12);
        assert_eq!(options.tool_timeout_secs, 45);
        assert_eq!(options.traffic_partition_key.as_deref(), Some("local-user"));
        assert_eq!(options.interaction_mode, InteractionMode::Interactive);
    }

    #[tokio::test]
    async fn build_tool_registry_registers_builtin_tools_without_mcp() -> Result<()> {
        let registry = build_tool_registry(&test_root_config("deepseek")).await?;

        assert!(registry.specs().iter().any(|spec| spec.name == "read_file"));
        assert!(registry.specs().iter().any(|spec| spec.name == "bash"));
        Ok(())
    }
}
