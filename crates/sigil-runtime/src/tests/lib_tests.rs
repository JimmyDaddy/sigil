use std::{
    collections::BTreeMap,
    env,
    ffi::OsString,
    path::Path,
    process::Command,
    sync::{Arc, Mutex, OnceLock},
};

use anyhow::Result;
use async_trait::async_trait;
use serde_json::json;
use sigil_kernel::{
    AgentConfig, AgentRole, ApprovalMode, CodeIntelStartup, CodeIntelligenceConfig,
    InteractionMode, LanguageServerConfig, McpServerConfig, McpServerStartup, MemoryConfig,
    PermissionConfig, ProviderCapabilities, ReasoningEffort, ReasoningStreamSupport,
    RoleModelConfig, RootConfig, SessionConfig, SkillDescriptor, SkillRunMode, SkillSource,
    SkillTrustState, TaskConfig, Tool, ToolAccess, ToolAllowlistConfig, ToolCall, ToolCategory,
    ToolContext, ToolPreviewCapability, ToolRegistry, ToolRegistryScope, ToolResult,
    ToolResultMeta, ToolSpec, WorkspaceConfig,
};
use sigil_provider_anthropic::{ANTHROPIC_API_KEY_ENV, SIGIL_ANTHROPIC_API_KEY_ENV};
use sigil_provider_deepseek::{LEGACY_DEEPSEEK_API_KEY_ENV, SIGIL_API_KEY_ENV};
use sigil_provider_gemini::{GEMINI_API_KEY_ENV, GOOGLE_API_KEY_ENV, SIGIL_GEMINI_API_KEY_ENV};
use sigil_provider_openai_compat::{OPENAI_API_KEY_ENV, OPENAI_COMPATIBLE_API_KEY_ENV};

use super::{
    SecretSource, activate_lazy_mcp_tools, activate_lazy_mcp_tools_detailed, build_provider,
    build_role_provider, build_role_run_options, build_role_skill_tool_registry,
    build_role_tool_registry, build_run_options, build_skill_tool_registry, build_tool_registry,
    build_tool_registry_without_eager_mcp, load_anthropic_config, load_deepseek_config,
    load_gemini_config, load_openai_compat_config, provider_capabilities_for_name,
    provider_capability_view, refresh_mcp_server_tools_with_mcp_handlers,
    register_lazy_mcp_activation_tool, resolve_anthropic_api_key, resolve_deepseek_api_key,
    resolve_deepseek_api_key_with_session, resolve_gemini_api_key,
    resolve_gemini_api_key_with_session, resolve_openai_compat_api_key,
    resolve_openai_compat_api_key_with_session, secret_redactor_for_root_config,
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
        skills: Default::default(),
        compaction: sigil_kernel::CompactionConfig::default(),
        code_intelligence: sigil_kernel::CodeIntelligenceConfig::default(),
        terminal: Default::default(),
        task: TaskConfig::default(),
        providers: BTreeMap::from([
            (
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
            ),
            (
                "openai_compat".to_owned(),
                json!({
                    "base_url": "https://openai.example.com/v1",
                    "model": "gpt-test",
                    "api_key": "openai-config-key",
                    "organization": "org-test",
                    "project": "project-test",
                    "request_timeout_secs": 20
                }),
            ),
            (
                "anthropic".to_owned(),
                json!({
                    "base_url": "https://anthropic.example.com",
                    "model": "claude-test",
                    "api_key": "anthropic-config-key",
                    "anthropic_version": "2023-06-01",
                    "max_tokens": 1024,
                    "request_timeout_secs": 21
                }),
            ),
            (
                "gemini".to_owned(),
                json!({
                    "base_url": "https://gemini.example.com/v1beta",
                    "model": "gemini-test",
                    "api_key": "gemini-config-key",
                    "request_timeout_secs": 22
                }),
            ),
        ]),
        mcp_servers: Vec::new(),
    }
}

struct ExistingMcpTool;

#[async_trait]
impl Tool for ExistingMcpTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "mcp__lazy__echo".to_owned(),
            description: "already registered MCP tool".to_owned(),
            input_schema: json!({"type": "object"}),
            category: ToolCategory::Mcp,
            access: ToolAccess::Network,
            preview: ToolPreviewCapability::None,
        }
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        call_id: String,
        _args: serde_json::Value,
    ) -> Result<ToolResult> {
        Ok(ToolResult::ok(
            call_id,
            "mcp__lazy__echo",
            "ok",
            ToolResultMeta::default(),
        ))
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
fn load_openai_compat_config_reads_provider_block() -> Result<()> {
    let config = load_openai_compat_config(&test_root_config("openai_compat"))?;

    assert_eq!(config.base_url, "https://openai.example.com/v1");
    assert_eq!(config.model, "gpt-test");
    assert_eq!(config.api_key.as_deref(), Some("openai-config-key"));
    assert_eq!(config.organization.as_deref(), Some("org-test"));
    assert_eq!(config.project.as_deref(), Some("project-test"));
    assert_eq!(config.request_timeout_secs, 20);
    Ok(())
}

#[test]
fn load_anthropic_and_gemini_config_read_provider_blocks() -> Result<()> {
    let anthropic = load_anthropic_config(&test_root_config("anthropic"))?;
    assert_eq!(anthropic.base_url, "https://anthropic.example.com");
    assert_eq!(anthropic.model, "claude-test");
    assert_eq!(anthropic.api_key.as_deref(), Some("anthropic-config-key"));
    assert_eq!(anthropic.max_tokens, 1024);
    assert_eq!(anthropic.request_timeout_secs, 21);

    let gemini = load_gemini_config(&test_root_config("gemini"))?;
    assert_eq!(gemini.base_url, "https://gemini.example.com/v1beta");
    assert_eq!(gemini.model, "gemini-test");
    assert_eq!(gemini.api_key.as_deref(), Some("gemini-config-key"));
    assert_eq!(gemini.request_timeout_secs, 22);
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
fn resolve_openai_compat_api_key_prefers_env_session_then_config() -> Result<()> {
    let _guard = ENV_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("env lock poisoned");
    let config = load_openai_compat_config(&test_root_config("openai_compat"))?;

    {
        let _scope = EnvScope::set_many(&[(OPENAI_COMPATIBLE_API_KEY_ENV, "sigil-openai-key")]);
        let resolved =
            resolve_openai_compat_api_key(&config).expect("expected OpenAI-compatible api key");
        assert_eq!(resolved.value, "sigil-openai-key");
        assert_eq!(
            resolved.source,
            SecretSource::Environment(OPENAI_COMPATIBLE_API_KEY_ENV)
        );
    }

    {
        let _scope = EnvScope::set_many(&[
            (OPENAI_COMPATIBLE_API_KEY_ENV, "   "),
            (OPENAI_API_KEY_ENV, "openai-env-key"),
        ]);
        let resolved =
            resolve_openai_compat_api_key(&config).expect("expected OpenAI-compatible api key");
        assert_eq!(resolved.value, "openai-env-key");
        assert_eq!(
            resolved.source,
            SecretSource::Environment(OPENAI_API_KEY_ENV)
        );
    }

    let resolved = resolve_openai_compat_api_key_with_session(&config, Some(" session-key "))
        .expect("expected session api key");
    assert_eq!(resolved.value, "session-key");
    assert_eq!(resolved.source, SecretSource::Session);

    let resolved = resolve_openai_compat_api_key_with_session(&config, Some("   "))
        .expect("expected config fallback");
    assert_eq!(resolved.value, "openai-config-key");
    assert_eq!(resolved.source, SecretSource::ConfigPlaintext);
    Ok(())
}

#[test]
fn resolve_anthropic_and_gemini_api_keys_prefer_env_session_then_config() -> Result<()> {
    let _guard = ENV_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("env lock poisoned");
    let anthropic = load_anthropic_config(&test_root_config("anthropic"))?;
    let gemini = load_gemini_config(&test_root_config("gemini"))?;

    {
        let _scope = EnvScope::set_many(&[(SIGIL_ANTHROPIC_API_KEY_ENV, "anthropic-env")]);
        let resolved = resolve_anthropic_api_key(&anthropic).expect("expected Anthropic api key");
        assert_eq!(resolved.value, "anthropic-env");
        assert_eq!(
            resolved.source,
            SecretSource::Environment(SIGIL_ANTHROPIC_API_KEY_ENV)
        );
    }

    {
        let _scope = EnvScope::set_many(&[
            (SIGIL_ANTHROPIC_API_KEY_ENV, "   "),
            (ANTHROPIC_API_KEY_ENV, "anthropic-provider-env"),
        ]);
        let resolved = resolve_anthropic_api_key(&anthropic).expect("expected Anthropic api key");
        assert_eq!(resolved.value, "anthropic-provider-env");
        assert_eq!(
            resolved.source,
            SecretSource::Environment(ANTHROPIC_API_KEY_ENV)
        );
    }

    let resolved = super::resolve_anthropic_api_key_with_session(&anthropic, Some(" session-key "))
        .expect("expected Anthropic session key");
    assert_eq!(resolved.value, "session-key");
    assert_eq!(resolved.source, SecretSource::Session);

    {
        let _scope = EnvScope::set_many(&[(SIGIL_GEMINI_API_KEY_ENV, "gemini-env")]);
        let resolved = resolve_gemini_api_key(&gemini).expect("expected Gemini api key");
        assert_eq!(resolved.value, "gemini-env");
        assert_eq!(
            resolved.source,
            SecretSource::Environment(SIGIL_GEMINI_API_KEY_ENV)
        );
    }

    {
        let _scope = EnvScope::set_many(&[
            (SIGIL_GEMINI_API_KEY_ENV, "   "),
            (GEMINI_API_KEY_ENV, "gemini-provider-env"),
            (GOOGLE_API_KEY_ENV, "google-env"),
        ]);
        let resolved = resolve_gemini_api_key(&gemini).expect("expected Gemini api key");
        assert_eq!(resolved.value, "gemini-provider-env");
        assert_eq!(
            resolved.source,
            SecretSource::Environment(GEMINI_API_KEY_ENV)
        );
    }

    let resolved = resolve_gemini_api_key_with_session(&gemini, Some(" gemini-session "))
        .expect("expected Gemini session key");
    assert_eq!(resolved.value, "gemini-session");
    assert_eq!(resolved.source, SecretSource::Session);
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
fn secret_redactor_for_root_config_redacts_openai_compat_api_key() {
    let _guard = ENV_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("env lock poisoned");
    let _scope = EnvScope::set_many(&[(OPENAI_COMPATIBLE_API_KEY_ENV, "openai-env-secret")]);

    let redactor = secret_redactor_for_root_config(&test_root_config("openai_compat"));

    assert_eq!(
        redactor.redact_text("Authorization: Bearer openai-env-secret"),
        "Authorization: [redacted] [redacted]"
    );
}

#[test]
fn secret_redactor_for_root_config_redacts_anthropic_and_gemini_api_keys() {
    let _guard = ENV_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("env lock poisoned");
    let _scope = EnvScope::set_many(&[
        (SIGIL_ANTHROPIC_API_KEY_ENV, "   "),
        (ANTHROPIC_API_KEY_ENV, "   "),
        (SIGIL_GEMINI_API_KEY_ENV, "   "),
        (GEMINI_API_KEY_ENV, "   "),
        (GOOGLE_API_KEY_ENV, "   "),
    ]);

    let redactor = secret_redactor_for_root_config(&test_root_config("anthropic"));

    assert_eq!(
        redactor.redact_text("x-api-key: anthropic-config-key; key=gemini-config-key"),
        "x-api-key: [redacted]; key=[redacted]"
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
fn build_provider_supports_openai_compat_aliases_and_missing_config_errors() -> Result<()> {
    for provider_name in ["openai_compat", "openai-compatible", "openai_compatible"] {
        let provider = build_provider(&test_root_config(provider_name))?;
        assert_eq!(provider.name(), "openai_compat");
    }

    let mut missing = test_root_config("openai_compat");
    missing.providers.remove("openai_compat");
    let error = load_openai_compat_config(&missing)
        .expect_err("missing OpenAI-compatible provider config should fail");
    assert!(
        error
            .to_string()
            .contains("missing [providers.openai_compat]")
    );
    Ok(())
}

#[test]
fn build_provider_supports_anthropic_and_gemini_and_missing_config_errors() -> Result<()> {
    let anthropic = build_provider(&test_root_config("anthropic"))?;
    assert_eq!(anthropic.name(), "anthropic");

    let claude_alias = build_provider(&test_root_config("claude"))?;
    assert_eq!(claude_alias.name(), "anthropic");

    let gemini = build_provider(&test_root_config("gemini"))?;
    assert_eq!(gemini.name(), "gemini");

    for provider_name in ["google", "google_gemini", "google-gemini"] {
        let provider = build_provider(&test_root_config(provider_name))?;
        assert_eq!(provider.name(), "gemini");
    }

    let mut missing_anthropic = test_root_config("anthropic");
    missing_anthropic.providers.remove("anthropic");
    let error = load_anthropic_config(&missing_anthropic)
        .expect_err("missing Anthropic provider config should fail");
    assert!(error.to_string().contains("missing [providers.anthropic]"));

    let mut missing_gemini = test_root_config("gemini");
    missing_gemini.providers.remove("gemini");
    let error = load_gemini_config(&missing_gemini)
        .expect_err("missing Gemini provider config should fail");
    assert!(error.to_string().contains("missing [providers.gemini]"));
    Ok(())
}

#[test]
fn provider_capability_view_uses_provider_neutral_rows() {
    let capabilities =
        provider_capabilities_for_name("anthropic").expect("Anthropic capabilities should exist");
    let view = provider_capability_view("anthropic", &capabilities);
    let alias_capabilities =
        provider_capabilities_for_name("claude").expect("Claude alias should resolve");
    let alias_view = provider_capability_view("claude", &alias_capabilities);

    assert_eq!(view.provider_name, "anthropic");
    assert_eq!(alias_view.provider_name, "anthropic");
    assert_eq!(
        view.rows.iter().map(|row| row.key).collect::<Vec<_>>(),
        vec![
            "text_stream",
            "tool_calls",
            "tool_args_stream",
            "reasoning_stream",
            "reasoning_effort",
            "reasoning_artifacts",
            "structured_output",
            "assistant_prefix_seed",
            "background_tasks",
            "agent_background_resume",
            "agent_thread_usage",
            "agent_result_replay",
            "response_handles",
            "cache_reporting",
            "system_fingerprint",
            "infill",
            "tool_name_limit",
        ]
    );
    assert!(
        view.rows
            .iter()
            .any(|row| { row.key == "tool_calls" && row.status.as_str() == "supported" })
    );
    assert!(
        view.rows
            .iter()
            .any(|row| { row.key == "reasoning_effort" && row.status.as_str() == "unsupported" })
    );
    assert!(view.rows.iter().any(|row| {
        row.key == "agent_background_resume" && row.status.as_str() == "unsupported"
    }));
    assert!(provider_capabilities_for_name("unknown").is_none());
}

#[test]
fn provider_capability_view_projects_every_capability_field() {
    let view = provider_capability_view(
        "custom",
        &ProviderCapabilities {
            exact_prefix_cache: true,
            reports_cache_tokens: true,
            reasoning_stream: ReasoningStreamSupport::Native,
            supports_reasoning_effort: true,
            supports_tool_stream: true,
            supports_background_tasks: true,
            supports_response_handles: true,
            supports_reasoning_artifacts: true,
            supports_structured_output: true,
            supports_assistant_prefix_seed: true,
            supports_schema_constrained_tools: true,
            supports_agent_background_resume: false,
            supports_agent_thread_usage: false,
            supports_agent_result_replay: false,
            supports_infill_completion: true,
            supports_system_fingerprint: true,
            tool_name_max_chars: 48,
        },
    );
    let row = |key: &str| {
        view.rows
            .iter()
            .find(|row| row.key == key)
            .expect("capability row should exist")
    };

    assert_eq!(row("reasoning_stream").detail, "native");
    assert_eq!(row("reasoning_artifacts").status.as_str(), "supported");
    assert_eq!(row("assistant_prefix_seed").status.as_str(), "supported");
    assert_eq!(row("system_fingerprint").status.as_str(), "supported");
    assert_eq!(row("infill").detail, "provider-native infill completion");
    assert_eq!(row("tool_name_limit").status.as_str(), "supported");
    assert!(row("tool_name_limit").detail.contains("48 chars"));
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
fn build_role_run_options_applies_reasoning_override() {
    let mut config = test_root_config("deepseek");
    config.task.planner.reasoning_effort = Some(ReasoningEffort::Low);

    let options = build_role_run_options(
        &config,
        Path::new("/tmp/sigil-runtime-test").to_path_buf(),
        InteractionMode::Interactive,
        AgentRole::Planner,
    );

    assert_eq!(options.reasoning_effort, Some(ReasoningEffort::Low));
    assert_eq!(options.max_turns, Some(12));
}

#[test]
fn build_role_provider_uses_role_provider_override() -> Result<()> {
    let mut config = test_root_config("deepseek");
    config.task.planner = RoleModelConfig {
        provider: Some("openai_compat".to_owned()),
        model: Some("gpt-role".to_owned()),
        ..RoleModelConfig::default()
    };

    let provider = build_role_provider(&config, AgentRole::Planner)?;

    assert_eq!(provider.name(), "openai_compat");
    Ok(())
}

#[tokio::test]
async fn build_role_tool_registry_applies_default_and_configured_scopes() -> Result<()> {
    let mut registry = ToolRegistry::new();
    sigil_tools_builtin::register_builtin_tools(&mut registry);

    let config = test_root_config("deepseek");
    let planner = build_role_tool_registry(&registry, &config, AgentRole::Planner);
    assert!(planner.spec_for("read_file").is_some());
    assert!(planner.spec_for("write_file").is_none());

    let executor = build_role_tool_registry(&registry, &config, AgentRole::Executor);
    assert!(executor.spec_for("write_file").is_some());

    let subagent_write = build_role_tool_registry(&registry, &config, AgentRole::SubagentWrite);
    assert!(subagent_write.spec_for("write_file").is_some());

    let mut write_disabled = config.clone();
    write_disabled.task.allow_write_subagents = false;
    let subagent_write =
        build_role_tool_registry(&registry, &write_disabled, AgentRole::SubagentWrite);
    assert!(subagent_write.spec_for("read_file").is_some());
    assert!(subagent_write.spec_for("write_file").is_none());

    let mut configured = config.clone();
    configured.task.planner.tools = ToolAllowlistConfig {
        allow_all: false,
        names: vec!["write_file".to_owned()],
        prefixes: Vec::new(),
    };
    let planner = build_role_tool_registry(&registry, &configured, AgentRole::Planner);
    assert!(planner.spec_for("write_file").is_some());
    assert!(planner.spec_for("read_file").is_none());
    Ok(())
}

#[tokio::test]
async fn build_skill_tool_registry_never_expands_base_or_role_scope() -> Result<()> {
    let mut registry = ToolRegistry::new();
    sigil_tools_builtin::register_builtin_tools(&mut registry);
    let config = test_root_config("deepseek");
    super::register_agent_tools(&mut registry, &config)?;

    let skill = SkillDescriptor {
        id: "readonly".to_owned(),
        name: "Readonly".to_owned(),
        description: String::new(),
        when_to_use: None,
        root: ".sigil/skills/readonly".into(),
        entrypoint: ".sigil/skills/readonly/SKILL.md".into(),
        source: SkillSource::Workspace,
        sha256: String::new(),
        enabled: true,
        trust: SkillTrustState::Trusted,
        model_invocable: true,
        user_invocable: true,
        run_as: SkillRunMode::Inline,
        agent: None,
        argument_hint: None,
        allowed_tools: ToolRegistryScope::from_names_and_prefixes(
            ["read_file", "write_file"],
            std::iter::empty::<&str>(),
        ),
        disallowed_tools: ToolRegistryScope::from_names_and_prefixes(
            ["write_file"],
            std::iter::empty::<&str>(),
        ),
        path_patterns: Vec::new(),
    };

    let direct = build_skill_tool_registry(&registry, &skill);
    assert!(direct.spec_for("read_file").is_some());
    assert!(direct.spec_for("write_file").is_none());

    let mut allow_all_skill = skill.clone();
    allow_all_skill.allowed_tools = ToolRegistryScope::default();
    allow_all_skill.disallowed_tools = ToolRegistryScope::default();
    let direct_allow_all = build_skill_tool_registry(&registry, &allow_all_skill);
    assert!(direct_allow_all.spec_for("read_file").is_some());
    assert!(direct_allow_all.spec_for("spawn_agent").is_none());
    assert!(direct_allow_all.spec_for("wait_agent").is_none());
    assert!(direct_allow_all.spec_for("close_agent").is_none());

    let mut write_allowed_skill = skill.clone();
    write_allowed_skill.disallowed_tools = ToolRegistryScope::default();
    let base_read_only = registry
        .scoped(ToolRegistryScope::from_names_and_prefixes(
            ["read_file"],
            std::iter::empty::<&str>(),
        ))
        .into_registry();
    let direct_on_read_only_base = build_skill_tool_registry(&base_read_only, &write_allowed_skill);
    assert!(direct_on_read_only_base.spec_for("read_file").is_some());
    assert!(direct_on_read_only_base.spec_for("write_file").is_none());

    let base_denied_write = registry
        .scoped_with_denies(
            ToolRegistryScope {
                allow_all: true,
                ..ToolRegistryScope::default()
            },
            ToolRegistryScope::from_names_and_prefixes(["write_file"], std::iter::empty::<&str>()),
        )
        .into_registry();
    let direct_on_denied_base = build_skill_tool_registry(&base_denied_write, &write_allowed_skill);
    assert!(direct_on_denied_base.spec_for("read_file").is_some());
    assert!(direct_on_denied_base.spec_for("write_file").is_none());

    let planner = build_role_skill_tool_registry(&registry, &config, AgentRole::Planner, &skill);
    assert!(planner.spec_for("read_file").is_some());
    assert!(planner.spec_for("write_file").is_none());
    assert!(planner.spec_for("bash").is_none());

    let read_child = build_role_skill_tool_registry(
        &registry,
        &config,
        AgentRole::SubagentRead,
        &write_allowed_skill,
    );
    assert!(read_child.spec_for("read_file").is_some());
    assert!(read_child.spec_for("write_file").is_none());

    let mut write_config = config.clone();
    write_config.task.allow_write_subagents = true;
    let write_child = build_role_skill_tool_registry(
        &registry,
        &write_config,
        AgentRole::SubagentWrite,
        &write_allowed_skill,
    );
    assert!(write_child.spec_for("read_file").is_some());
    assert!(write_child.spec_for("write_file").is_some());

    let mut inheriting_skill = skill.clone();
    inheriting_skill.allowed_tools = ToolRegistryScope::default();
    inheriting_skill.disallowed_tools = ToolRegistryScope::default();
    let inherited =
        build_role_skill_tool_registry(&registry, &config, AgentRole::Planner, &inheriting_skill);
    assert!(inherited.spec_for("read_file").is_some());
    assert!(inherited.spec_for("ls").is_some());
    assert!(inherited.spec_for("bash").is_none());
    assert!(inherited.spec_for("write_file").is_none());
    Ok(())
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
async fn mcp_activate_server_tool_registers_lazy_tools_for_model_turns() -> Result<()> {
    if Command::new("python3").arg("--version").output().is_err() {
        return Ok(());
    }
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("lazy_mcp_server.py");
    std::fs::write(
        &script,
        r#"
import json
import sys

def read_message():
    headers = {}
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            sys.exit(0)
        if line in (b"\r\n", b"\n"):
            break
        key, value = line.decode().split(":", 1)
        headers[key.lower()] = value.strip()
    body = sys.stdin.buffer.read(int(headers["content-length"]))
    return json.loads(body)

def write_message(message):
    data = json.dumps(message).encode()
    sys.stdout.buffer.write(b"Content-Length: " + str(len(data)).encode() + b"\r\n\r\n" + data)
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    method = message.get("method")
    if method == "initialize":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"protocolVersion":"2025-06-18","serverInfo":{"name":"lazy","version":"1.0.0"},"capabilities":{}}})
    elif method == "tools/list":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[{"name":"echo","description":"echo","inputSchema":{"type":"object","properties":{"value":{"type":"string"}},"required":["value"]}}]}})
    elif method == "tools/call":
        value = message.get("params", {}).get("arguments", {}).get("value", "")
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"content":[{"type":"text","text":"lazy:" + value}]}})
    elif "id" in message:
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{}})
"#,
    )?;
    let provider = build_provider(&test_root_config("deepseek"))?;
    let mut config = test_root_config("deepseek");
    config.mcp_servers.push(McpServerConfig {
        name: "lazy".to_owned(),
        command: "python3".to_owned(),
        args: vec![script.display().to_string()],
        startup: McpServerStartup::Lazy,
        startup_timeout_secs: 5,
        ..McpServerConfig::default()
    });

    let registry =
        build_tool_registry(&config, &provider.capabilities(), temp.path().to_path_buf()).await?;

    assert!(registry.spec_for("mcp_activate_server").is_some());
    assert!(registry.spec_for("mcp__lazy__echo").is_none());

    let activation = registry
        .execute(
            ToolContext {
                workspace_root: temp.path().to_path_buf(),
                timeout_secs: 5,
            },
            ToolCall {
                id: "activate-lazy".to_owned(),
                name: "mcp_activate_server".to_owned(),
                args_json: json!({ "server_name": "lazy" }).to_string(),
            },
        )
        .await?;

    assert!(!activation.is_error());
    assert!(registry.spec_for("mcp__lazy__echo").is_some());
    Ok(())
}

#[tokio::test]
async fn refresh_mcp_server_tools_replaces_existing_server_tool_surface() -> Result<()> {
    if Command::new("python3").arg("--version").output().is_err() {
        return Ok(());
    }
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("refresh_mcp_server.py");
    std::fs::write(
        &script,
        r#"
import json
import sys

def read_message():
    headers = {}
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            sys.exit(0)
        if line in (b"\r\n", b"\n"):
            break
        key, value = line.decode().split(":", 1)
        headers[key.lower()] = value.strip()
    body = sys.stdin.buffer.read(int(headers["content-length"]))
    return json.loads(body)

def write_message(message):
    data = json.dumps(message).encode()
    sys.stdout.buffer.write(b"Content-Length: " + str(len(data)).encode() + b"\r\n\r\n" + data)
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    method = message.get("method")
    if method == "initialize":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"protocolVersion":"2025-06-18","serverInfo":{"name":"refresh","version":"1.0.0"},"capabilities":{"prompts":{}}}})
    elif method == "tools/list":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[{"name":"echo","description":"echo","inputSchema":{"type":"object"}}]}})
    elif method == "prompts/list":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"prompts":[]}})
    elif "id" in message:
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{}})
"#,
    )?;

    let provider = build_provider(&test_root_config("deepseek"))?;
    let mut config = test_root_config("deepseek");
    config.mcp_servers.push(McpServerConfig {
        name: "lazy".to_owned(),
        command: "python3".to_owned(),
        args: vec![script.display().to_string()],
        startup_timeout_secs: 5,
        ..McpServerConfig::default()
    });
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ExistingMcpTool));

    let result = refresh_mcp_server_tools_with_mcp_handlers(
        &mut registry,
        &config,
        &provider.capabilities(),
        temp.path().to_path_buf(),
        "lazy",
        sigil_mcp::unsupported_mcp_elicitation_handler(),
        sigil_mcp::unsupported_mcp_runtime_event_handler(),
    )
    .await?;

    assert_eq!(result.matched_servers, 1);
    assert_eq!(result.removed_tools, 1);
    assert_eq!(result.added_tools, 3);
    assert!(registry.spec_for("mcp__lazy__echo").is_some());
    assert!(registry.spec_for("mcp__lazy__prompts_list").is_some());
    assert!(registry.spec_for("mcp__lazy__prompts_get").is_some());
    Ok(())
}

#[tokio::test]
async fn refresh_mcp_server_tools_restores_existing_tools_when_refresh_fails() -> Result<()> {
    let provider = build_provider(&test_root_config("deepseek"))?;
    let mut config = test_root_config("deepseek");
    config.mcp_servers.push(McpServerConfig {
        name: "lazy".to_owned(),
        command: "/definitely/missing/sigil-mcp-server".to_owned(),
        startup_timeout_secs: 5,
        ..McpServerConfig::default()
    });
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ExistingMcpTool));

    let error = refresh_mcp_server_tools_with_mcp_handlers(
        &mut registry,
        &config,
        &provider.capabilities(),
        std::env::current_dir()?,
        "lazy",
        sigil_mcp::unsupported_mcp_elicitation_handler(),
        sigil_mcp::unsupported_mcp_runtime_event_handler(),
    )
    .await
    .expect_err("missing required server should fail refresh");

    assert!(error.to_string().contains("failed to spawn MCP server"));
    assert!(registry.spec_for("mcp__lazy__echo").is_some());
    Ok(())
}

#[tokio::test]
async fn refresh_mcp_server_tools_returns_zero_for_unknown_server() -> Result<()> {
    let provider = build_provider(&test_root_config("deepseek"))?;
    let config = test_root_config("deepseek");
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ExistingMcpTool));

    let result = refresh_mcp_server_tools_with_mcp_handlers(
        &mut registry,
        &config,
        &provider.capabilities(),
        std::env::current_dir()?,
        "missing",
        sigil_mcp::unsupported_mcp_elicitation_handler(),
        sigil_mcp::unsupported_mcp_runtime_event_handler(),
    )
    .await?;

    assert_eq!(result.matched_servers, 0);
    assert_eq!(result.removed_tools, 0);
    assert_eq!(result.added_tools, 0);
    assert!(registry.spec_for("mcp__lazy__echo").is_some());
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

#[test]
fn lazy_mcp_activation_tool_is_not_registered_without_lazy_servers() -> Result<()> {
    let provider = build_provider(&test_root_config("deepseek"))?;
    let config = test_root_config("deepseek");
    let mut registry = ToolRegistry::new();

    register_lazy_mcp_activation_tool(
        &mut registry,
        &config,
        &provider.capabilities(),
        std::env::current_dir()?,
        sigil_mcp::unsupported_mcp_elicitation_handler(),
        sigil_mcp::unsupported_mcp_runtime_event_handler(),
    );

    assert!(registry.spec_for("mcp_activate_server").is_none());
    Ok(())
}

#[test]
fn build_tool_registry_without_eager_mcp_keeps_local_tools_when_required_eager_is_missing()
-> Result<()> {
    let provider = build_provider(&test_root_config("deepseek"))?;
    let mut config = test_root_config("deepseek");
    config.mcp_servers.push(McpServerConfig {
        name: "required-eager".to_owned(),
        command: "/definitely/missing/sigil-mcp-server".to_owned(),
        startup: McpServerStartup::Eager,
        ..McpServerConfig::default()
    });

    let registry = build_tool_registry_without_eager_mcp(
        &config,
        &provider.capabilities(),
        std::env::current_dir()?,
        sigil_mcp::unsupported_mcp_elicitation_handler(),
        sigil_mcp::unsupported_mcp_runtime_event_handler(),
    );

    assert!(registry.spec_for("read_file").is_some());
    assert!(registry.spec_for("mcp__required_eager__echo").is_none());
    Ok(())
}

#[tokio::test]
async fn mcp_activate_server_tool_reports_unknown_and_already_ready_states() -> Result<()> {
    let provider = build_provider(&test_root_config("deepseek"))?;
    let mut config = test_root_config("deepseek");
    config.mcp_servers.push(McpServerConfig {
        name: "lazy".to_owned(),
        command: "/definitely/missing/sigil-mcp-server".to_owned(),
        startup: McpServerStartup::Lazy,
        ..McpServerConfig::default()
    });
    let mut registry = ToolRegistry::new();
    register_lazy_mcp_activation_tool(
        &mut registry,
        &config,
        &provider.capabilities(),
        std::env::current_dir()?,
        sigil_mcp::unsupported_mcp_elicitation_handler(),
        sigil_mcp::unsupported_mcp_runtime_event_handler(),
    );

    let spec = registry
        .spec_for("mcp_activate_server")
        .expect("activation tool should register");
    assert_eq!(spec.category, ToolCategory::Mcp);
    assert_eq!(spec.access, ToolAccess::Network);
    assert_eq!(spec.preview, ToolPreviewCapability::None);

    let missing_name = registry.permission_subjects(
        &ToolContext {
            workspace_root: std::env::current_dir()?,
            timeout_secs: 5,
        },
        &ToolCall {
            id: "activate-missing-name".to_owned(),
            name: "mcp_activate_server".to_owned(),
            args_json: "{}".to_owned(),
        },
    );
    assert!(
        missing_name
            .expect_err("missing server name should fail permission subjects")
            .to_string()
            .contains("missing server_name")
    );

    let unknown_default = registry.permission_default_mode(
        &ToolContext {
            workspace_root: std::env::current_dir()?,
            timeout_secs: 5,
        },
        &ToolCall {
            id: "activate-unknown-default".to_owned(),
            name: "mcp_activate_server".to_owned(),
            args_json: json!({"server_name": "other"}).to_string(),
        },
    )?;
    assert_eq!(unknown_default, None);

    let unknown_audit = registry.egress_audit(
        &ToolContext {
            workspace_root: std::env::current_dir()?,
            timeout_secs: 5,
        },
        &ToolCall {
            id: "activate-unknown-audit".to_owned(),
            name: "mcp_activate_server".to_owned(),
            args_json: json!({"server_name": "other"}).to_string(),
        },
    )?;
    assert!(unknown_audit.is_none());

    let unknown = registry
        .execute(
            ToolContext {
                workspace_root: std::env::current_dir()?,
                timeout_secs: 5,
            },
            ToolCall {
                id: "activate-unknown".to_owned(),
                name: "mcp_activate_server".to_owned(),
                args_json: json!({"server_name": "other"}).to_string(),
            },
        )
        .await?;
    assert!(unknown.is_error());
    assert!(unknown.content.contains("unknown lazy MCP server other"));

    registry.register(Arc::new(ExistingMcpTool));
    let subjects = registry.permission_subjects(
        &ToolContext {
            workspace_root: std::env::current_dir()?,
            timeout_secs: 5,
        },
        &ToolCall {
            id: "activate-lazy-subjects".to_owned(),
            name: "mcp_activate_server".to_owned(),
            args_json: json!({"server_name": "lazy"}).to_string(),
        },
    )?;
    assert!(
        subjects
            .iter()
            .any(|subject| subject.normalized == "mcp_server:lazy")
    );
    assert!(
        subjects
            .iter()
            .any(|subject| subject.normalized == "mcp_trust_class:self_hosted")
    );

    let default_mode = registry.permission_default_mode(
        &ToolContext {
            workspace_root: std::env::current_dir()?,
            timeout_secs: 5,
        },
        &ToolCall {
            id: "activate-lazy-default".to_owned(),
            name: "mcp_activate_server".to_owned(),
            args_json: json!({"server_name": "lazy"}).to_string(),
        },
    )?;
    assert_eq!(default_mode, Some(ApprovalMode::Ask));

    let audit = registry.egress_audit(
        &ToolContext {
            workspace_root: std::env::current_dir()?,
            timeout_secs: 5,
        },
        &ToolCall {
            id: "activate-lazy-audit".to_owned(),
            name: "mcp_activate_server".to_owned(),
            args_json: json!({"server_name": "lazy"}).to_string(),
        },
    )?;
    let audit = audit.expect("activation should expose egress audit");
    assert_eq!(audit.destination, "mcp:lazy");
    assert_eq!(audit.payload["startup"], "lazy");

    let ready = registry
        .execute(
            ToolContext {
                workspace_root: std::env::current_dir()?,
                timeout_secs: 5,
            },
            ToolCall {
                id: "activate-lazy".to_owned(),
                name: "mcp_activate_server".to_owned(),
                args_json: json!({"server_name": "lazy"}).to_string(),
            },
        )
        .await?;
    assert!(!ready.is_error());
    let payload: serde_json::Value = serde_json::from_str(&ready.content)?;
    assert_eq!(payload["status"], "already_ready");
    assert_eq!(payload["matched_servers"], 1);
    assert_eq!(payload["added_tools"], 0);
    Ok(())
}

#[test]
fn mcp_activate_server_tool_respects_disabled_egress_logging() -> Result<()> {
    let provider = build_provider(&test_root_config("deepseek"))?;
    let mut config = test_root_config("deepseek");
    config.mcp_servers.push(McpServerConfig {
        name: "quiet-lazy".to_owned(),
        command: "/definitely/missing/sigil-mcp-server".to_owned(),
        startup: McpServerStartup::Lazy,
        trust: sigil_kernel::McpServerTrustPolicy {
            egress_logging: false,
            ..sigil_kernel::McpServerTrustPolicy::default()
        },
        ..McpServerConfig::default()
    });
    let mut registry = ToolRegistry::new();
    register_lazy_mcp_activation_tool(
        &mut registry,
        &config,
        &provider.capabilities(),
        std::env::current_dir()?,
        sigil_mcp::unsupported_mcp_elicitation_handler(),
        sigil_mcp::unsupported_mcp_runtime_event_handler(),
    );

    let audit = registry.egress_audit(
        &ToolContext {
            workspace_root: std::env::current_dir()?,
            timeout_secs: 5,
        },
        &ToolCall {
            id: "quiet-audit".to_owned(),
            name: "mcp_activate_server".to_owned(),
            args_json: json!({"server_name": "quiet-lazy"}).to_string(),
        },
    )?;

    assert!(audit.is_none());
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
