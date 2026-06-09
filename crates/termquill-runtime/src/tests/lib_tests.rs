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
            max_turns: Some(12),
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
    assert_eq!(options.max_turns, Some(12));
    assert_eq!(options.tool_timeout_secs, 45);
    assert_eq!(options.traffic_partition_key.as_deref(), Some("local-user"));
    assert_eq!(options.interaction_mode, InteractionMode::Interactive);
}

#[tokio::test]
async fn build_tool_registry_registers_builtin_tools_without_mcp() -> Result<()> {
    let provider = build_provider(&test_root_config("deepseek"))?;
    let registry = build_tool_registry(
        &test_root_config("deepseek"),
        &provider.capabilities(),
        std::env::current_dir()?,
    )
    .await?;

    assert!(registry.specs().iter().any(|spec| spec.name == "read_file"));
    assert!(registry.specs().iter().any(|spec| spec.name == "bash"));
    Ok(())
}
