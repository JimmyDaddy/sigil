use std::{
    collections::BTreeMap,
    env,
    ffi::OsString,
    path::Path,
    sync::{Mutex, OnceLock},
};

use anyhow::Result;
use serde_json::json;
use sigil_kernel::{
    AgentConfig, CodeIntelStartup, CodeIntelligenceConfig, InteractionMode, LanguageServerConfig,
    McpServerConfig, McpServerStartup, MemoryConfig, PermissionConfig, ReasoningEffort, RootConfig,
    SessionConfig, ToolRegistry, WorkspaceConfig,
};
use sigil_provider_deepseek::{LEGACY_DEEPSEEK_API_KEY_ENV, SIGIL_API_KEY_ENV};

use super::{
    SecretSource, activate_lazy_mcp_tools, activate_lazy_mcp_tools_detailed, build_provider,
    build_run_options, build_tool_registry, load_deepseek_config, resolve_deepseek_api_key,
    resolve_deepseek_api_key_with_session, secret_redactor_for_root_config,
};

static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn test_root_config(provider: &str) -> RootConfig {
    RootConfig {
        workspace: WorkspaceConfig {
            root: ".".to_owned(),
        },
        session: SessionConfig {
            log_dir: ".sigil/sessions".to_owned(),
        },
        agent: AgentConfig {
            provider: provider.to_owned(),
            model: "deepseek-v4-flash".to_owned(),
            max_turns: Some(12),
            tool_timeout_secs: 45,
        },
        permission: PermissionConfig::default(),
        memory: MemoryConfig { enabled: true },
        compaction: sigil_kernel::CompactionConfig::default(),
        code_intelligence: sigil_kernel::CodeIntelligenceConfig::default(),
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
fn resolve_deepseek_api_key_uses_env_before_plaintext_config() -> Result<()> {
    let _guard = ENV_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("env lock poisoned");
    let _scope = EnvScope::set_many(&[(SIGIL_API_KEY_ENV, "env-key")]);
    let config = load_deepseek_config(&test_root_config("deepseek"))?;

    let resolved = resolve_deepseek_api_key(&config).expect("expected api key");

    assert_eq!(resolved.value, "env-key");
    assert_eq!(
        resolved.source,
        SecretSource::Environment(SIGIL_API_KEY_ENV)
    );
    Ok(())
}

#[test]
fn resolve_deepseek_api_key_supports_deepseek_env_and_config_fallback() -> Result<()> {
    let _guard = ENV_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("env lock poisoned");
    let config = load_deepseek_config(&test_root_config("deepseek"))?;

    {
        let _scope = EnvScope::set_many(&[(LEGACY_DEEPSEEK_API_KEY_ENV, "deepseek-env-key")]);
        let resolved = resolve_deepseek_api_key(&config).expect("expected deepseek api key");
        assert_eq!(resolved.value, "deepseek-env-key");
        assert_eq!(
            resolved.source,
            SecretSource::Environment(LEGACY_DEEPSEEK_API_KEY_ENV)
        );
    }

    let resolved = resolve_deepseek_api_key(&config).expect("expected config api key");
    assert_eq!(resolved.value, "test-key");
    assert_eq!(resolved.source, SecretSource::ConfigPlaintext);
    Ok(())
}

#[test]
fn secret_redactor_for_root_config_redacts_resolved_api_key() {
    let _guard = ENV_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("env lock poisoned");
    let _scope = EnvScope::set_many(&[(SIGIL_API_KEY_ENV, "env-secret-key")]);

    let redactor = secret_redactor_for_root_config(&test_root_config("deepseek"));

    assert_eq!(
        redactor.redact_text("Authorization: Bearer env-secret-key"),
        "Authorization: [redacted] [redacted]"
    );
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
fn build_provider_supports_deepseek_and_missing_provider_config_errors() -> Result<()> {
    let provider = build_provider(&test_root_config("deepseek"))?;
    assert_eq!(provider.name(), "deepseek");

    let mut missing = test_root_config("deepseek");
    missing.providers.clear();
    let error = load_deepseek_config(&missing).expect_err("missing provider config should fail");
    assert!(error.to_string().contains("missing [providers.deepseek]"));
    Ok(())
}

#[test]
fn build_run_options_carries_shared_runtime_defaults() {
    let workspace_root = Path::new("/tmp/sigil-runtime-test").to_path_buf();
    let options = build_run_options(
        &test_root_config("deepseek"),
        workspace_root.clone(),
        InteractionMode::Interactive,
    );

    assert_eq!(options.workspace_root, workspace_root);
    assert_eq!(options.max_turns, Some(12));
    assert_eq!(options.tool_timeout_secs, 45);
    assert_eq!(options.reasoning_effort, Some(ReasoningEffort::Max));
    assert!(
        options
            .traffic_partition_key
            .as_deref()
            .is_some_and(|key| key.starts_with("workspace-"))
    );
    assert_eq!(options.interaction_mode, InteractionMode::Interactive);
}

#[test]
fn build_run_options_uses_max_reasoning_for_non_deepseek() {
    let options = build_run_options(
        &test_root_config("other"),
        Path::new("/tmp/sigil-runtime-test").to_path_buf(),
        InteractionMode::Headless,
    );

    assert_eq!(options.reasoning_effort, Some(ReasoningEffort::Max));
}

#[test]
fn resolve_deepseek_api_key_prefers_session_over_plaintext_and_skips_blank_values() -> Result<()> {
    let _guard = ENV_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("env lock poisoned");
    let _scope = EnvScope::set_many(&[
        (SIGIL_API_KEY_ENV, "   "),
        (LEGACY_DEEPSEEK_API_KEY_ENV, "   "),
    ]);
    let config = load_deepseek_config(&test_root_config("deepseek"))?;

    let resolved = resolve_deepseek_api_key_with_session(&config, Some("  session-secret  "))
        .expect("session api key should resolve");
    assert_eq!(resolved.value, "session-secret");
    assert_eq!(resolved.source, SecretSource::Session);

    let resolved = resolve_deepseek_api_key_with_session(&config, Some("   "))
        .expect("config fallback should resolve");
    assert_eq!(resolved.source, SecretSource::ConfigPlaintext);
    Ok(())
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

#[tokio::test]
async fn build_tool_registry_registers_code_intelligence_tools_when_enabled() -> Result<()> {
    let provider = build_provider(&test_root_config("deepseek"))?;
    let mut config = test_root_config("deepseek");
    config.code_intelligence = CodeIntelligenceConfig {
        enabled: true,
        startup: CodeIntelStartup::Lazy,
        servers: vec![LanguageServerConfig {
            name: "rust-analyzer".to_owned(),
            languages: vec!["rust".to_owned()],
            command: "rust-analyzer".to_owned(),
            args: Vec::new(),
            env: Default::default(),
            root_markers: vec!["Cargo.toml".to_owned()],
            file_extensions: vec!["rs".to_owned()],
            initialization_options: serde_json::Value::Null,
            trust_required: true,
            startup_timeout_ms: 100,
        }],
        ..CodeIntelligenceConfig::default()
    };

    let registry =
        build_tool_registry(&config, &provider.capabilities(), std::env::current_dir()?).await?;

    assert!(registry.spec_for("code_symbols").is_some());
    assert!(registry.spec_for("code_diagnostics").is_some());
    Ok(())
}

#[tokio::test]
async fn activate_lazy_mcp_tools_ignores_nonmatching_server_name() -> Result<()> {
    let provider = build_provider(&test_root_config("deepseek"))?;
    let mut config = test_root_config("deepseek");
    config.mcp_servers.push(McpServerConfig {
        name: "lazy".to_owned(),
        command: "/definitely/missing/sigil-mcp-server".to_owned(),
        startup: McpServerStartup::Lazy,
        ..McpServerConfig::default()
    });
    let mut registry = ToolRegistry::new();

    let added = activate_lazy_mcp_tools(
        &mut registry,
        &config,
        &provider.capabilities(),
        std::env::current_dir()?,
        Some("other"),
    )
    .await?;

    assert_eq!(added, 0);
    assert!(registry.specs().is_empty());
    Ok(())
}

#[tokio::test]
async fn activate_lazy_mcp_tools_returns_zero_when_optional_server_is_skipped() -> Result<()> {
    let provider = build_provider(&test_root_config("deepseek"))?;
    let mut config = test_root_config("deepseek");
    config.mcp_servers.push(McpServerConfig {
        name: "optional-lazy".to_owned(),
        command: "/definitely/missing/sigil-mcp-server".to_owned(),
        required: false,
        startup: McpServerStartup::Lazy,
        ..McpServerConfig::default()
    });
    let mut registry = ToolRegistry::new();

    let added = activate_lazy_mcp_tools(
        &mut registry,
        &config,
        &provider.capabilities(),
        std::env::current_dir()?,
        Some("optional-lazy"),
    )
    .await?;

    assert_eq!(added, 0);
    assert!(registry.specs().is_empty());
    Ok(())
}

#[tokio::test]
async fn activate_lazy_mcp_tools_detailed_reports_matched_servers() -> Result<()> {
    let provider = build_provider(&test_root_config("deepseek"))?;
    let mut config = test_root_config("deepseek");
    config.mcp_servers.push(McpServerConfig {
        name: "optional-lazy".to_owned(),
        command: "/definitely/missing/sigil-mcp-server".to_owned(),
        required: false,
        startup: McpServerStartup::Lazy,
        ..McpServerConfig::default()
    });
    let mut registry = ToolRegistry::new();

    let result = activate_lazy_mcp_tools_detailed(
        &mut registry,
        &config,
        &provider.capabilities(),
        std::env::current_dir()?,
        Some("optional-lazy"),
    )
    .await?;

    assert_eq!(result.matched_servers, 1);
    assert_eq!(result.added_tools, 0);
    Ok(())
}

struct EnvScope {
    saved: Vec<(&'static str, Option<OsString>)>,
}

impl EnvScope {
    fn set_many(values: &[(&'static str, &'static str)]) -> Self {
        let mut saved = Vec::with_capacity(values.len());
        for (name, value) in values {
            saved.push((*name, env::var_os(name)));
            // SAFETY: tests serialize process-wide env mutation with ENV_LOCK.
            unsafe { env::set_var(name, value) };
        }
        Self { saved }
    }
}

impl Drop for EnvScope {
    fn drop(&mut self) {
        for (name, value) in self.saved.drain(..).rev() {
            match value {
                Some(value) => {
                    // SAFETY: tests serialize process-wide env mutation with ENV_LOCK.
                    unsafe { env::set_var(name, value) };
                }
                None => {
                    // SAFETY: tests serialize process-wide env mutation with ENV_LOCK.
                    unsafe { env::remove_var(name) };
                }
            }
        }
    }
}
