use std::{
    collections::BTreeMap,
    env,
    ffi::OsString,
    path::Path,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use anyhow::Result;
use async_trait::async_trait;
use serde_json::json;
use sigil_kernel::{
    AgentConfig, AgentRole, ApprovalMode, CodeIntelStartup, CodeIntelligenceConfig, ControlEntry,
    ExecutionBackendCapabilities, ExecutionBackendKind, ExecutionNetworkReceipt,
    ExecutionSandboxFallback, ExecutionSandboxProfile, ExecutionSandboxStrategyConfig,
    InteractionMode, JsonlSessionStore, LanguageServerConfig, McpServerConfig, McpServerStartup,
    McpServerTrustPolicy, MemoryConfig, MutationEventRecorder, NetworkEffect, PermissionConfig,
    ProviderCapabilities, ReasoningEffort, ReasoningStreamSupport, RoleModelConfig, RootConfig,
    Session, SessionConfig, SessionLogEntry, SkillDescriptor, SkillRunMode, SkillSource,
    SkillTrustState, TaskConfig, Tool, ToolAccess, ToolAllowlistConfig, ToolCall, ToolCategory,
    ToolContext, ToolLifecycleOwner, ToolPreviewCapability, ToolRegistry, ToolRegistryScope,
    ToolResult, ToolResultMeta, ToolSpec, ToolSubjectKind, WorkspaceConfig, WorkspaceTrust,
};
use sigil_provider_anthropic::SIGIL_ANTHROPIC_API_KEY_ENV;
use sigil_provider_deepseek::SIGIL_API_KEY_ENV;
use sigil_provider_gemini::SIGIL_GEMINI_API_KEY_ENV;
use sigil_provider_openai_compat::OPENAI_COMPATIBLE_API_KEY_ENV;
use sigil_provider_openai_responses::OPENAI_RESPONSES_API_KEY_ENV;

use super::{
    ExtensionProcessNetworkAdmission, McpProcessLaunchRequest, McpProcessLauncher, SecretSource,
    activate_eager_remote_mcp_server, activate_lazy_mcp_tools, activate_lazy_mcp_tools_detailed,
    activate_or_refresh_configured_remote_mcp_server, build_plan_prompt_tool_registry,
    build_provider, build_role_provider, build_role_run_options, build_role_skill_tool_registry,
    build_role_tool_registry, build_run_options, build_skill_tool_registry, build_tool_registry,
    build_tool_registry_without_eager_mcp,
    build_tool_registry_without_eager_mcp_with_workspace_trust,
    build_tool_surface_without_eager_mcp_with_workspace_trust, launch_planned_mcp_process,
    load_anthropic_config, load_deepseek_config, load_gemini_config, load_openai_compat_config,
    load_openai_responses_config, provider_capabilities_for_name, provider_capability_view,
    refresh_mcp_server_tools_with_mcp_handlers,
    refresh_mcp_server_tools_with_mcp_handlers_and_mutation_recorder_and_network_admission,
    register_lazy_mcp_activation_tool, require_default_deepseek_v4_flash_portable_transport,
    resolve_anthropic_api_key, resolve_deepseek_api_key, resolve_deepseek_api_key_with_session,
    resolve_gemini_api_key, resolve_gemini_api_key_with_session, resolve_openai_compat_api_key,
    resolve_openai_compat_api_key_with_session, resolve_openai_responses_api_key,
    resolve_openai_responses_api_key_with_session, secret_redactor_for_root_config,
    shutdown_registered_tools,
};

fn sandbox_execution_config(
    backend: ExecutionBackendKind,
    profile: ExecutionSandboxProfile,
    fallback: ExecutionSandboxFallback,
    container_image: Option<String>,
) -> sigil_kernel::ExecutionConfig {
    let mut sandbox = ExecutionSandboxStrategyConfig::new(backend);
    sandbox.profile = profile;
    sandbox.fallback = fallback;
    sandbox.container_image = container_image;
    sigil_kernel::ExecutionConfig::sandbox(sandbox)
}

fn test_root_config(provider: &str) -> RootConfig {
    let model = match provider {
        "openai_compat" | "openai_responses" => "gpt-test",
        "anthropic" => "claude-test",
        "gemini" => "gemini-test",
        _ => "deepseek-v4-flash",
    };
    RootConfig {
        workspace: WorkspaceConfig {
            root: ".".to_owned(),
        },
        storage: Default::default(),
        session: SessionConfig {
            log_dir: Some(".sigil/sessions".to_owned()),
            retention: Default::default(),
        },
        agent: AgentConfig {
            provider: provider.to_owned(),
            model: model.to_owned(),
            max_turns: Some(12),
            tool_timeout_secs: 45,
        },
        permission: PermissionConfig::default(),
        model_request: Default::default(),
        memory: MemoryConfig { enabled: true },
        skills: Default::default(),
        compaction: sigil_kernel::CompactionConfig::default(),
        code_intelligence: sigil_kernel::CodeIntelligenceConfig::default(),
        terminal: Default::default(),
        execution: Default::default(),
        verification: Default::default(),
        appearance: Default::default(),
        task: TaskConfig::default(),
        providers: BTreeMap::from([
            (
                "deepseek".to_owned(),
                json!({
                    "base_url": "https://example.com",
                    "beta_base_url": "https://example.com/beta",
                    "anthropic_base_url": "https://example.com/anthropic",
                    "fim_model": "deepseek-v4-pro",
                    "api_key": "test-key",
                    "strict_tools_mode": "auto"
                }),
            ),
            (
                "openai_compat".to_owned(),
                json!({
                    "base_url": "https://openai.example.com/v1",
                    "api_key": "openai-config-key",
                    "organization": "org-test",
                    "project": "project-test"
                }),
            ),
            (
                "openai_responses".to_owned(),
                json!({
                    "base_url": "https://responses.example.com/v1",
                    "api_key": "responses-config-key"
                }),
            ),
            (
                "anthropic".to_owned(),
                json!({
                    "base_url": "https://anthropic.example.com",
                    "api_key": "anthropic-config-key",
                    "anthropic_version": "2023-06-01",
                    "max_tokens": 1024
                }),
            ),
            (
                "gemini".to_owned(),
                json!({
                    "base_url": "https://gemini.example.com/v1beta",
                    "api_key": "gemini-config-key"
                }),
            ),
        ]),
        web: Default::default(),
        mcp_servers: Vec::new(),
    }
}

fn command_exists_on_path(command: &str) -> bool {
    let Some(path) = env::var_os("PATH") else {
        return false;
    };
    if cfg!(windows) {
        let path_extensions =
            env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_owned());
        return env::split_paths(&path).any(|directory| {
            path_extensions.split(';').any(|extension| {
                !extension.is_empty() && directory.join(format!("{command}{extension}")).is_file()
            })
        });
    }
    env::split_paths(&path).any(|directory| directory.join(command).is_file())
}

#[test]
fn append_session_control_entries_updates_in_memory_session() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let mut current_session = Some(
        Session::new("deepseek", "deepseek-v4-pro")
            .with_store(JsonlSessionStore::new(path.clone())?),
    );

    let entries = super::append_session_control_entries(
        &path,
        &mut current_session,
        [ControlEntry::Note {
            kind: "runtime_test".to_owned(),
            data: json!({"value": 1}),
        }],
        "test note",
    )?;

    assert_eq!(entries.len(), 1);
    assert!(matches!(
        &entries[0],
        SessionLogEntry::Control(ControlEntry::Note { kind, .. }) if kind == "runtime_test"
    ));
    assert_eq!(
        current_session
            .as_ref()
            .map(|session| session.entries().len()),
        Some(1)
    );
    Ok(())
}

#[test]
fn append_session_control_entries_persists_without_in_memory_session() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let mut current_session = None;

    let entries = super::append_session_control_entries(
        &path,
        &mut current_session,
        [ControlEntry::Note {
            kind: "runtime_store_test".to_owned(),
            data: json!({"value": 2}),
        }],
        "test note",
    )?;

    assert!(current_session.is_none());
    assert_eq!(entries.len(), 1);
    let reloaded = JsonlSessionStore::read_entries(&path)?;
    assert_eq!(reloaded.len(), entries.len());
    assert!(matches!(
        &reloaded[0],
        SessionLogEntry::Control(ControlEntry::Note { kind, .. }) if kind == "runtime_store_test"
    ));
    Ok(())
}

#[test]
fn detached_session_control_tracking_records_only_successful_durable_appends() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session-detached.jsonl");
    let mut current_session = None;
    let mut detached_controls = Vec::new();

    let entries = super::append_session_control_entries_and_track_detached(
        &path,
        &mut current_session,
        [ControlEntry::Note {
            kind: "runtime_detached_test".to_owned(),
            data: json!({"value": 2}),
        }],
        &mut detached_controls,
        "detached test note",
    )?;

    assert_eq!(detached_controls.len(), 1);
    assert!(matches!(
        detached_controls.first(),
        Some(ControlEntry::Note { kind, .. }) if kind == "runtime_detached_test"
    ));
    assert_eq!(entries.len(), 1);
    assert!(current_session.is_none());
    Ok(())
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
            access: ToolAccess::Read,
            network_effect: Some(NetworkEffect::Unknown),
            preview: ToolPreviewCapability::None,
        }
    }

    fn lifecycle_owner(&self) -> Option<ToolLifecycleOwner> {
        Some(ToolLifecycleOwner::new(
            sigil_mcp::MCP_TOOL_LIFECYCLE_NAMESPACE,
            "lazy",
            "existing-fixture-generation",
        ))
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

struct ShutdownFailingMcpTool;

struct CountingShutdownTool {
    name: &'static str,
    owner: ToolLifecycleOwner,
    attempts: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for CountingShutdownTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name.to_owned(),
            description: "counting cleanup fixture".to_owned(),
            input_schema: json!({"type": "object"}),
            category: ToolCategory::Mcp,
            access: ToolAccess::Read,
            network_effect: Some(NetworkEffect::Unknown),
            preview: ToolPreviewCapability::None,
        }
    }

    async fn shutdown(&self) -> Result<()> {
        self.attempts.fetch_add(1, Ordering::AcqRel);
        Err(anyhow::anyhow!("injected cleanup failure"))
    }

    fn lifecycle_owner(&self) -> Option<ToolLifecycleOwner> {
        Some(self.owner.clone())
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        call_id: String,
        _args: serde_json::Value,
    ) -> Result<ToolResult> {
        Ok(ToolResult::ok(
            call_id,
            self.name,
            "unused",
            ToolResultMeta::default(),
        ))
    }
}

#[async_trait]
impl Tool for ShutdownFailingMcpTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "mcp__rollback__echo".to_owned(),
            description: "MCP tool with injected retirement failure".to_owned(),
            input_schema: json!({"type": "object"}),
            category: ToolCategory::Mcp,
            access: ToolAccess::Read,
            network_effect: Some(NetworkEffect::Unknown),
            preview: ToolPreviewCapability::None,
        }
    }

    async fn shutdown(&self) -> Result<()> {
        Err(anyhow::anyhow!("injected old generation cleanup failure"))
    }

    fn lifecycle_owner(&self) -> Option<ToolLifecycleOwner> {
        Some(ToolLifecycleOwner::new(
            sigil_mcp::MCP_TOOL_LIFECYCLE_NAMESPACE,
            "rollback",
            "shutdown-failing-fixture-generation",
        ))
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        call_id: String,
        _args: serde_json::Value,
    ) -> Result<ToolResult> {
        Ok(ToolResult::ok(
            call_id,
            "mcp__rollback__echo",
            "old",
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
fn portable_compaction_requires_the_resolved_default_deepseek_transport() -> Result<()> {
    let mut config = test_root_config("deepseek");
    let error = require_default_deepseek_v4_flash_portable_transport(&config)
        .expect_err("a custom DeepSeek route must not inherit the default local token proof");
    assert!(
        error
            .to_string()
            .contains("resolved default DeepSeek V4 Flash transport")
    );

    config.providers.insert(
        "deepseek".to_owned(),
        json!({
            "api_key": "test-key",
        }),
    );
    require_default_deepseek_v4_flash_portable_transport(&config)?;
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
    Ok(())
}

#[test]
fn load_openai_responses_config_reads_provider_block() -> Result<()> {
    let config = load_openai_responses_config(&test_root_config("openai_responses"))?;

    assert_eq!(config.base_url, "https://responses.example.com/v1");
    assert_eq!(config.api_key.as_deref(), Some("responses-config-key"));
    assert_eq!(config.model, "gpt-test");
    Ok(())
}

#[test]
fn load_anthropic_and_gemini_config_read_provider_blocks() -> Result<()> {
    let anthropic = load_anthropic_config(&test_root_config("anthropic"))?;
    assert_eq!(anthropic.base_url, "https://anthropic.example.com");
    assert_eq!(anthropic.model, "claude-test");
    assert_eq!(anthropic.api_key.as_deref(), Some("anthropic-config-key"));
    assert_eq!(anthropic.max_tokens, 1024);

    let gemini = load_gemini_config(&test_root_config("gemini"))?;
    assert_eq!(gemini.base_url, "https://gemini.example.com/v1beta");
    assert_eq!(gemini.model, "gemini-test");
    assert_eq!(gemini.api_key.as_deref(), Some("gemini-config-key"));
    Ok(())
}

#[test]
fn resolve_deepseek_api_key_uses_env_before_plaintext_config() -> Result<()> {
    let _guard = crate::test_env::lock();
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
fn resolve_deepseek_api_key_ignores_deepseek_env_and_uses_config_fallback() -> Result<()> {
    let _guard = crate::test_env::lock();
    let _base_scope =
        EnvScope::set_many(&[(SIGIL_API_KEY_ENV, "   "), ("DEEPSEEK_API_KEY", "   ")]);
    let config = load_deepseek_config(&test_root_config("deepseek"))?;

    {
        let _scope = EnvScope::set_many(&[("DEEPSEEK_API_KEY", "deepseek-env-key")]);
        let resolved = resolve_deepseek_api_key(&config).expect("expected deepseek api key");
        assert_eq!(resolved.value, "test-key");
        assert_eq!(resolved.source, SecretSource::ConfigPlaintext);
    }

    let resolved = resolve_deepseek_api_key(&config).expect("expected config api key");
    assert_eq!(resolved.value, "test-key");
    assert_eq!(resolved.source, SecretSource::ConfigPlaintext);
    Ok(())
}

#[test]
fn resolve_openai_compat_api_key_prefers_env_session_then_config() -> Result<()> {
    let _guard = crate::test_env::lock();
    let _base_scope = EnvScope::set_many(&[
        (OPENAI_COMPATIBLE_API_KEY_ENV, "   "),
        ("OPENAI_API_KEY", "   "),
    ]);
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
            ("OPENAI_API_KEY", "openai-env-key"),
        ]);
        let resolved =
            resolve_openai_compat_api_key(&config).expect("expected OpenAI-compatible api key");
        assert_eq!(resolved.value, "openai-config-key");
        assert_eq!(resolved.source, SecretSource::ConfigPlaintext);
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
fn resolve_openai_responses_api_key_prefers_env_session_then_config() -> Result<()> {
    let _guard = crate::test_env::lock();
    let _base_scope = EnvScope::set_many(&[(OPENAI_RESPONSES_API_KEY_ENV, "   ")]);
    let config = load_openai_responses_config(&test_root_config("openai_responses"))?;

    {
        let _scope = EnvScope::set_many(&[(OPENAI_RESPONSES_API_KEY_ENV, "responses-env-key")]);
        let resolved =
            resolve_openai_responses_api_key(&config).expect("expected OpenAI Responses api key");
        assert_eq!(resolved.value, "responses-env-key");
        assert_eq!(
            resolved.source,
            SecretSource::Environment(OPENAI_RESPONSES_API_KEY_ENV)
        );
    }

    let resolved = resolve_openai_responses_api_key_with_session(&config, Some(" session-key "))
        .expect("expected session api key");
    assert_eq!(resolved.value, "session-key");
    assert_eq!(resolved.source, SecretSource::Session);
    Ok(())
}

#[test]
fn resolve_anthropic_and_gemini_api_keys_prefer_env_session_then_config() -> Result<()> {
    let _guard = crate::test_env::lock();
    let _base_scope = EnvScope::set_many(&[
        (SIGIL_ANTHROPIC_API_KEY_ENV, "   "),
        ("ANTHROPIC_API_KEY", "   "),
        (SIGIL_GEMINI_API_KEY_ENV, "   "),
        ("GEMINI_API_KEY", "   "),
        ("GOOGLE_API_KEY", "   "),
    ]);
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
            ("ANTHROPIC_API_KEY", "anthropic-provider-env"),
        ]);
        let resolved = resolve_anthropic_api_key(&anthropic).expect("expected Anthropic api key");
        assert_eq!(resolved.value, "anthropic-config-key");
        assert_eq!(resolved.source, SecretSource::ConfigPlaintext);
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
            ("GEMINI_API_KEY", "gemini-provider-env"),
            ("GOOGLE_API_KEY", "google-env"),
        ]);
        let resolved = resolve_gemini_api_key(&gemini).expect("expected Gemini api key");
        assert_eq!(resolved.value, "gemini-config-key");
        assert_eq!(resolved.source, SecretSource::ConfigPlaintext);
    }

    let resolved = resolve_gemini_api_key_with_session(&gemini, Some(" gemini-session "))
        .expect("expected Gemini session key");
    assert_eq!(resolved.value, "gemini-session");
    assert_eq!(resolved.source, SecretSource::Session);
    Ok(())
}

#[test]
fn secret_redactor_for_root_config_redacts_resolved_api_key() {
    let _guard = crate::test_env::lock();
    let _scope = EnvScope::set_many(&[(SIGIL_API_KEY_ENV, "env-secret-key")]);

    let redactor = secret_redactor_for_root_config(&test_root_config("deepseek"));

    assert_eq!(
        redactor.redact_text("Authorization: Bearer env-secret-key"),
        "Authorization: [redacted] [redacted]"
    );
}

#[test]
fn secret_redactor_for_root_config_redacts_openai_compat_api_key() {
    let _guard = crate::test_env::lock();
    let _scope = EnvScope::set_many(&[(OPENAI_COMPATIBLE_API_KEY_ENV, "openai-env-secret")]);

    let redactor = secret_redactor_for_root_config(&test_root_config("openai_compat"));

    assert_eq!(
        redactor.redact_text("Authorization: Bearer openai-env-secret"),
        "Authorization: [redacted] [redacted]"
    );
}

#[test]
fn secret_redactor_for_root_config_redacts_openai_responses_api_key() {
    let _guard = crate::test_env::lock();
    let _scope = EnvScope::set_many(&[(OPENAI_RESPONSES_API_KEY_ENV, "responses-env-secret")]);

    let redactor = secret_redactor_for_root_config(&test_root_config("openai_responses"));

    assert_eq!(
        redactor.redact_text("authorization: Bearer responses-env-secret"),
        "authorization: [redacted] [redacted]"
    );
}

#[test]
fn secret_redactor_for_root_config_redacts_anthropic_and_gemini_api_keys() {
    let _guard = crate::test_env::lock();
    let _scope = EnvScope::set_many(&[
        (SIGIL_ANTHROPIC_API_KEY_ENV, "   "),
        ("ANTHROPIC_API_KEY", "   "),
        (SIGIL_GEMINI_API_KEY_ENV, "   "),
        ("GEMINI_API_KEY", "   "),
        ("GOOGLE_API_KEY", "   "),
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
fn build_provider_supports_openai_compat_and_missing_config_errors() -> Result<()> {
    let provider = build_provider(&test_root_config("openai_compat"))?;
    assert_eq!(provider.name(), "openai_compat");

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
fn build_provider_supports_openai_responses_and_missing_config_errors() -> Result<()> {
    let provider = build_provider(&test_root_config("openai_responses"))?;
    assert_eq!(provider.name(), "openai_responses");

    let mut missing = test_root_config("openai_responses");
    missing.providers.remove("openai_responses");
    let error = load_openai_responses_config(&missing)
        .expect_err("missing OpenAI Responses provider config should fail");
    assert!(
        error
            .to_string()
            .contains("missing [providers.openai_responses]")
    );
    Ok(())
}

#[test]
fn build_provider_supports_canonical_anthropic_and_gemini_and_missing_config_errors() -> Result<()>
{
    let anthropic = build_provider(&test_root_config("anthropic"))?;
    assert_eq!(anthropic.name(), "anthropic");

    let gemini = build_provider(&test_root_config("gemini"))?;
    assert_eq!(gemini.name(), "gemini");

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
fn build_provider_rejects_provider_aliases() {
    for provider_name in [
        "openai-compatible",
        "openai_compatible",
        "claude",
        "google",
        "google_gemini",
        "google-gemini",
    ] {
        let error = match build_provider(&test_root_config(provider_name)) {
            Ok(_) => panic!("provider aliases should not be accepted"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains(&format!("unsupported provider {provider_name}"))
        );
    }
}

#[test]
fn provider_capability_view_uses_provider_neutral_rows() {
    let capabilities =
        provider_capabilities_for_name("anthropic").expect("Anthropic capabilities should exist");
    let view = provider_capability_view("anthropic", &capabilities);

    assert_eq!(view.provider_name, "anthropic");
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
            .any(|row| { row.key == "reasoning_stream" && row.status.as_str() == "supported" })
    );
    assert!(
        view.rows
            .iter()
            .any(|row| { row.key == "reasoning_effort" && row.status.as_str() == "unsupported" })
    );
    assert!(view.rows.iter().any(|row| {
        row.key == "agent_background_resume" && row.status.as_str() == "unsupported"
    }));
    assert!(provider_capabilities_for_name("claude").is_none());
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
fn build_run_options_omits_default_reasoning_for_unsupported_providers() {
    let options = build_run_options(
        &test_root_config("openai_compat"),
        Path::new("/tmp/sigil-runtime-test").to_path_buf(),
        InteractionMode::Headless,
    );

    assert_eq!(options.reasoning_effort, None);
}

#[test]
fn build_run_options_uses_supported_openai_responses_reasoning_default() {
    let options = build_run_options(
        &test_root_config("openai_responses"),
        Path::new("/tmp/sigil-runtime-test").to_path_buf(),
        InteractionMode::Headless,
    );

    assert_eq!(options.reasoning_effort, Some(ReasoningEffort::High));
}

#[test]
fn build_run_options_keeps_uncanonical_workspace_root_observable_but_tolerant() {
    let workspace_root = Path::new("/tmp/sigil-runtime-test-missing").to_path_buf();
    let options = build_run_options(
        &test_root_config("deepseek"),
        workspace_root.clone(),
        InteractionMode::Headless,
    );

    assert_eq!(options.workspace_root, workspace_root);
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
async fn build_plan_prompt_tool_registry_keeps_agent_tools_with_readonly_scope() -> Result<()> {
    let mut registry = ToolRegistry::new();
    sigil_tools_builtin::register_builtin_tools(&mut registry);
    let config = test_root_config("deepseek");
    super::register_agent_tools(&mut registry, &config)?;

    let planner = build_plan_prompt_tool_registry(&registry, &config);

    assert!(planner.spec_for("read_file").is_some());
    assert!(planner.spec_for(super::SPAWN_AGENT_TOOL_NAME).is_some());
    assert!(planner.spec_for(super::WAIT_AGENT_TOOL_NAME).is_some());
    assert!(
        planner
            .spec_for(super::READ_AGENT_RESULT_TOOL_NAME)
            .is_some()
    );
    assert!(planner.spec_for("write_file").is_none());
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
    assert!(direct_allow_all.spec_for("read_agent_result").is_none());
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
    let _guard = crate::test_env::lock();
    let _scope = EnvScope::set_many(&[(SIGIL_API_KEY_ENV, "   "), ("DEEPSEEK_API_KEY", "   ")]);
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
    assert!(registry.spec_for("terminal_start").is_some());
    assert!(registry.spec_for("mcp_activate_server").is_none());
    Ok(())
}

#[tokio::test]
async fn build_tool_registry_fails_closed_when_sandbox_is_required() -> Result<()> {
    let provider = build_provider(&test_root_config("deepseek"))?;
    let mut config = test_root_config("deepseek");
    config.execution = sandbox_execution_config(
        ExecutionBackendKind::Local,
        ExecutionSandboxProfile::WorkspaceWrite,
        ExecutionSandboxFallback::Deny,
        None,
    );

    let result =
        build_tool_registry(&config, &provider.capabilities(), std::env::current_dir()?).await;
    let Err(error) = result else {
        panic!("local backend must not satisfy required sandbox policy");
    };

    assert!(
        error
            .to_string()
            .contains("execution profile WorkspaceWrite requires filesystem and process isolation")
    );
    Ok(())
}

#[tokio::test]
async fn build_tool_registry_fails_closed_when_profile_requires_sandbox() -> Result<()> {
    let provider = build_provider(&test_root_config("deepseek"))?;
    let mut config = test_root_config("deepseek");
    config.execution = sandbox_execution_config(
        ExecutionBackendKind::Local,
        ExecutionSandboxProfile::WorkspaceWrite,
        ExecutionSandboxFallback::Deny,
        None,
    );

    let result =
        build_tool_registry(&config, &provider.capabilities(), std::env::current_dir()?).await;
    let Err(error) = result else {
        panic!("local backend must not satisfy sandbox profile");
    };

    assert!(
        error
            .to_string()
            .contains("execution profile WorkspaceWrite requires filesystem and process isolation")
    );
    Ok(())
}

#[tokio::test]
async fn configured_mcp_process_launcher_local_records_outside_sandbox() -> Result<()> {
    if tokio::process::Command::new("sh")
        .arg("-c")
        .arg("true")
        .output()
        .await
        .is_err()
    {
        return Ok(());
    }

    let temp = tempfile::tempdir()?;
    let launcher = super::ConfiguredMcpProcessLauncher {
        execution: sigil_kernel::ExecutionConfig::default(),
    };
    let launch = launcher.launch(McpProcessLaunchRequest {
        server_name: "local".to_owned(),
        command: "sh".to_owned(),
        args: vec!["-c".to_owned(), "sleep 1".to_owned()],
        working_dir: Some(temp.path().to_path_buf()),
        environment: sigil_kernel::resolve_extension_process_environment(&[])?,
        launch_static_fingerprint: "sha256:test-launch".to_owned(),
        startup_timeout_secs: 1,
        classification: sigil_mcp::McpProcessClass::LocalStdioConfigured,
        network_admission: ExtensionProcessNetworkAdmission::default(),
        declaration: None,
    })?;

    assert_eq!(
        launch.receipt.coverage,
        sigil_mcp::McpProcessCoverage::LocalStdioOutsideSandbox
    );
    assert_eq!(launch.receipt.backend, Some(ExecutionBackendKind::Local));
    assert_eq!(
        launch.receipt.sandbox_profile,
        Some(ExecutionSandboxProfile::Unconfined)
    );

    let mut child = launch.child;
    let _ = child.kill().await;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn configured_mcp_process_launcher_network_ask_without_evidence_is_zero_spawn() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let marker = temp.path().join("configured-ask-rejected-spawned");
    let launcher = super::ConfiguredMcpProcessLauncher {
        execution: sigil_kernel::ExecutionConfig::default(),
    };
    let result = launcher.launch(McpProcessLaunchRequest {
        server_name: "configured-ask-rejected".to_owned(),
        command: "sh".to_owned(),
        args: vec![
            "-c".to_owned(),
            "printf spawned > \"$1\"".to_owned(),
            "sh".to_owned(),
            marker.to_string_lossy().into_owned(),
        ],
        working_dir: Some(temp.path().to_path_buf()),
        environment: sigil_kernel::resolve_extension_process_environment(&[])?,
        launch_static_fingerprint: "sha256:configured-ask-rejected".to_owned(),
        startup_timeout_secs: 1,
        classification: sigil_mcp::McpProcessClass::LocalStdioConfigured,
        network_admission: ExtensionProcessNetworkAdmission::new(
            sigil_kernel::NetworkPolicy::Ask,
            false,
        ),
        declaration: None,
    });

    let Err(error) = result else {
        panic!("ask without explicit evidence must fail before configured spawn");
    };
    assert_eq!(
        error
            .downcast_ref::<sigil_kernel::ExtensionProcessLaunchError>()
            .map(|error| error.code),
        Some(sigil_kernel::ExtensionProcessLaunchErrorCode::NetworkApprovalRequired)
    );
    assert!(
        !marker.exists(),
        "configured ask rejection must be zero-spawn"
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn configured_mcp_process_launcher_network_ask_with_evidence_spawns() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let marker = temp.path().join("configured-ask-approved-spawned");
    let launcher = super::ConfiguredMcpProcessLauncher {
        execution: sigil_kernel::ExecutionConfig::default(),
    };
    let mut launch = launcher.launch(McpProcessLaunchRequest {
        server_name: "configured-ask-approved".to_owned(),
        command: "sh".to_owned(),
        args: vec![
            "-c".to_owned(),
            "printf spawned > \"$1\"".to_owned(),
            "sh".to_owned(),
            marker.to_string_lossy().into_owned(),
        ],
        working_dir: Some(temp.path().to_path_buf()),
        environment: sigil_kernel::resolve_extension_process_environment(&[])?,
        launch_static_fingerprint: "sha256:configured-ask-approved".to_owned(),
        startup_timeout_secs: 1,
        classification: sigil_mcp::McpProcessClass::LocalStdioConfigured,
        network_admission: ExtensionProcessNetworkAdmission::new(
            sigil_kernel::NetworkPolicy::Ask,
            true,
        ),
        declaration: None,
    })?;

    assert!(launch.child.wait().await?.success());
    assert!(
        marker.exists(),
        "explicit evidence should admit configured spawn"
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn refresh_mcp_server_network_admission_rejects_before_spawn() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let provider = build_provider(&test_root_config("deepseek"))?;
    for (label, admission, expected_code) in [
        (
            "ask",
            ExtensionProcessNetworkAdmission::new(sigil_kernel::NetworkPolicy::Ask, false),
            sigil_kernel::ExtensionProcessLaunchErrorCode::NetworkApprovalRequired,
        ),
        (
            "deny",
            ExtensionProcessNetworkAdmission::new(sigil_kernel::NetworkPolicy::Deny, true),
            sigil_kernel::ExtensionProcessLaunchErrorCode::NetworkIsolationUnavailable,
        ),
    ] {
        let marker = temp.path().join(format!("refresh-{label}-spawned"));
        let mut config = test_root_config("deepseek");
        config.mcp_servers.push(mcp_server_config! {
            name: format!("refresh-{label}"),
            command: "sh".to_owned(),
            args: vec![
                "-c".to_owned(),
                "printf spawned > \"$1\"".to_owned(),
                "sh".to_owned(),
                marker.to_string_lossy().into_owned(),
            ],
            required: true,
            trust: McpServerTrustPolicy {
                approval_default: ApprovalMode::Allow,
                allow_secrets: true,
                ..McpServerTrustPolicy::default()
            },
            ..McpServerConfig::default()
        });
        let mut registry = ToolRegistry::new();
        let error =
            refresh_mcp_server_tools_with_mcp_handlers_and_mutation_recorder_and_network_admission(
                &mut registry,
                &config,
                &provider.capabilities(),
                temp.path().to_path_buf(),
                &format!("refresh-{label}"),
                sigil_mcp::unsupported_mcp_elicitation_handler(),
                sigil_mcp::unsupported_mcp_runtime_event_handler(),
                None,
                admission,
            )
            .await
            .expect_err("network admission must reject before MCP refresh spawn");

        assert_eq!(
            error
                .downcast_ref::<sigil_kernel::ExtensionProcessLaunchError>()
                .map(|error| error.code),
            Some(expected_code)
        );
        assert!(
            !marker.exists(),
            "{label} rejection must not be authorized by source defaults or secret policy"
        );
    }
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn planned_mcp_process_returns_owned_child_and_denied_receipt() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let environment = sigil_kernel::resolve_extension_process_environment(&[])?;
    let capabilities = ExecutionBackendCapabilities {
        filesystem_isolation: true,
        network_isolation: true,
        process_isolation: true,
        ..ExecutionBackendCapabilities::default()
    };
    let plan = sigil_tools_builtin::LongLivedStdioProcessPlan {
        program: "sh".into(),
        args: vec![OsString::from("-c"), OsString::from("true")],
        cwd: temp.path().canonicalize()?,
        environment: environment.clone(),
        backend: ExecutionBackendKind::LinuxBubblewrap,
        backend_capabilities: capabilities,
        sandbox_profile: ExecutionSandboxProfile::BuildNetworked,
        network: ExecutionNetworkReceipt::denied(
            "deterministic test plan proves isolated process tree",
        ),
        sandboxed: true,
    };
    let mut launch = launch_planned_mcp_process(
        McpProcessLaunchRequest {
            server_name: "configured-deny-proven".to_owned(),
            command: "sh".to_owned(),
            args: vec!["-c".to_owned(), "true".to_owned()],
            working_dir: Some(temp.path().to_path_buf()),
            environment,
            launch_static_fingerprint: "sha256:configured-deny-proven".to_owned(),
            startup_timeout_secs: 1,
            classification: sigil_mcp::McpProcessClass::LocalStdioConfigured,
            network_admission: ExtensionProcessNetworkAdmission::new(
                sigil_kernel::NetworkPolicy::Deny,
                false,
            ),
            declaration: None,
        },
        plan,
    )?;

    assert!(launch.receipt.network.is_denied());
    let receipt_capabilities = launch
        .receipt
        .backend_capabilities
        .expect("configured launcher should report backend capabilities");
    assert_eq!(receipt_capabilities, capabilities);
    assert_eq!(
        launch.receipt.classification,
        sigil_mcp::McpProcessClass::LocalStdioSandboxed
    );
    let _process_owner = launch.process_owner;
    assert!(launch.child.wait().await?.success());
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn configured_mcp_process_launcher_creates_killable_process_group() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let launcher = super::ConfiguredMcpProcessLauncher {
        execution: sigil_kernel::ExecutionConfig::default(),
    };
    let launch = launcher.launch(McpProcessLaunchRequest {
        server_name: "process-group".to_owned(),
        command: "sh".to_owned(),
        args: vec!["-c".to_owned(), "sleep 30".to_owned()],
        working_dir: Some(temp.path().to_path_buf()),
        environment: sigil_kernel::resolve_extension_process_environment(&[])?,
        launch_static_fingerprint: "sha256:test-process-group".to_owned(),
        startup_timeout_secs: 1,
        classification: sigil_mcp::McpProcessClass::LocalStdioConfigured,
        network_admission: ExtensionProcessNetworkAdmission::default(),
        declaration: None,
    })?;
    let mut child = launch.child;
    let process_id = child.id().expect("configured MCP child should have a pid");
    let process_group = tokio::process::Command::new("ps")
        .args(["-o", "pgid=", "-p", &process_id.to_string()])
        .output()
        .await?;
    let observed_group = String::from_utf8_lossy(&process_group.stdout)
        .trim()
        .parse::<u32>();

    let _ = tokio::process::Command::new("kill")
        .args(["-KILL", &format!("-{process_id}")])
        .status()
        .await;
    let _ = child.wait().await;

    assert!(process_group.status.success());
    assert_eq!(observed_group?, process_id);
    Ok(())
}

#[test]
fn configured_mcp_process_launcher_local_required_sandbox_fails_closed() {
    let launcher = super::ConfiguredMcpProcessLauncher {
        execution: sandbox_execution_config(
            ExecutionBackendKind::Local,
            ExecutionSandboxProfile::WorkspaceWrite,
            ExecutionSandboxFallback::Deny,
            None,
        ),
    };
    let result = launcher.launch(McpProcessLaunchRequest {
        server_name: "local".to_owned(),
        command: "sh".to_owned(),
        args: vec!["-c".to_owned(), "true".to_owned()],
        working_dir: None,
        environment: sigil_kernel::resolve_extension_process_environment(&[])
            .expect("test environment should resolve"),
        launch_static_fingerprint: "sha256:test-launch".to_owned(),
        startup_timeout_secs: 1,
        classification: sigil_mcp::McpProcessClass::LocalStdioConfigured,
        network_admission: ExtensionProcessNetworkAdmission::default(),
        declaration: None,
    });

    let Err(error) = result else {
        panic!("local MCP launcher must fail closed when sandbox is required");
    };
    assert_eq!(
        error
            .downcast_ref::<sigil_kernel::ExtensionProcessLaunchError>()
            .map(|error| error.code),
        Some(sigil_kernel::ExtensionProcessLaunchErrorCode::ProcessIsolationUnavailable)
    );
}

#[tokio::test]
#[cfg(target_os = "macos")]
async fn configured_mcp_process_launcher_macos_seatbelt_conformance_denies_external_write()
-> Result<()> {
    if !Path::new("/usr/bin/sandbox-exec").exists() {
        return Ok(());
    }

    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let outside = temp.path().join("outside.txt");
    let launcher = super::ConfiguredMcpProcessLauncher {
        execution: sandbox_execution_config(
            ExecutionBackendKind::MacosSeatbelt,
            ExecutionSandboxProfile::BuildNetworked,
            ExecutionSandboxFallback::Deny,
            None,
        ),
    };
    let launch = launcher.launch(McpProcessLaunchRequest {
        server_name: "seatbelt".to_owned(),
        command: "sh".to_owned(),
        args: vec![
            "-c".to_owned(),
            "printf ok > inside.txt; if printf nope > \"$1\"; then printf outside-wrote; else printf outside-denied; fi".to_owned(),
            "sh".to_owned(),
            outside.to_string_lossy().into_owned(),
        ],
        working_dir: Some(workspace.clone()),
        environment: sigil_kernel::resolve_extension_process_environment(&[])?,
        launch_static_fingerprint: "sha256:test-launch".to_owned(),
        startup_timeout_secs: 1,
        classification: sigil_mcp::McpProcessClass::LocalStdioConfigured,
        network_admission: ExtensionProcessNetworkAdmission::default(),
        declaration: None,
    })?;

    assert_eq!(
        launch.receipt.classification,
        sigil_mcp::McpProcessClass::LocalStdioSandboxed
    );
    assert_eq!(
        launch.receipt.coverage,
        sigil_mcp::McpProcessCoverage::LocalStdioSandboxed
    );
    assert_eq!(
        launch.receipt.backend,
        Some(ExecutionBackendKind::MacosSeatbelt)
    );
    assert_eq!(
        launch.receipt.sandbox_profile,
        Some(ExecutionSandboxProfile::BuildNetworked)
    );

    let output = launch.child.wait_with_output().await?;
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("outside-denied"),
        "stdout: {} stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(std::fs::read_to_string(workspace.join("inside.txt"))?, "ok");
    assert!(
        !outside.exists(),
        "MCP stdio Seatbelt wrapper must deny writes outside the workspace"
    );
    Ok(())
}

#[tokio::test]
#[cfg(target_os = "macos")]
async fn build_tool_registry_accepts_macos_seatbelt_when_sandbox_is_required() -> Result<()> {
    let provider = build_provider(&test_root_config("deepseek"))?;
    let mut config = test_root_config("deepseek");
    config.execution = sandbox_execution_config(
        ExecutionBackendKind::MacosSeatbelt,
        ExecutionSandboxProfile::WorkspaceWrite,
        ExecutionSandboxFallback::Deny,
        None,
    );

    let registry =
        build_tool_registry(&config, &provider.capabilities(), std::env::current_dir()?).await?;

    assert!(registry.specs().iter().any(|spec| spec.name == "bash"));
    Ok(())
}

#[tokio::test]
#[cfg(target_os = "macos")]
async fn build_tool_registry_routes_terminal_pty_through_configured_sandbox_backend() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let provider = build_provider(&test_root_config("deepseek"))?;
    let mut config = test_root_config("deepseek");
    config.execution = sandbox_execution_config(
        ExecutionBackendKind::MacosSeatbelt,
        ExecutionSandboxProfile::WorkspaceWrite,
        ExecutionSandboxFallback::Deny,
        None,
    );
    let registry =
        build_tool_registry(&config, &provider.capabilities(), temp.path().to_path_buf()).await?;

    let result = registry
        .execute(
            ToolContext::new(temp.path().to_path_buf(), 5),
            ToolCall {
                id: "terminal-sandboxed-pty".to_owned(),
                name: "terminal_start".to_owned(),
                args_json: json!({
                    "task_id": "runtime-sandboxed-pty",
                    "command": "printf runtime > runtime.txt",
                    "shell": "/bin/sh",
                    "pty": true
                })
                .to_string(),
            },
        )
        .await?;

    assert!(!result.is_error());
    assert_eq!(
        result.metadata.details["execution_backend"],
        json!("sandboxed_pty")
    );
    assert_eq!(
        result.metadata.details["enforcement_backend"],
        json!("macos_seatbelt")
    );
    assert_eq!(
        result.metadata.details["sandbox_profile"],
        json!("workspace_write")
    );
    assert_eq!(
        result.metadata.details["enforcement_backend_capabilities"]["persistent_pty"],
        json!(true)
    );
    Ok(())
}

#[tokio::test]
async fn build_tool_registry_registers_code_intelligence_tools_when_enabled() -> Result<()> {
    let provider = build_provider(&test_root_config("deepseek"))?;
    let mut config = test_root_config("deepseek");
    config.code_intelligence = CodeIntelligenceConfig {
        enabled: true,
        server_startup: CodeIntelStartup::Lazy,
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
async fn runtime_tool_surface_shares_code_intelligence_with_context_resolver() -> Result<()> {
    let temp = tempfile::tempdir()?;
    std::fs::write(temp.path().join("lib.rs"), "pub fn hello() {}\n")?;
    let provider = build_provider(&test_root_config("deepseek"))?;
    let mut config = test_root_config("deepseek");
    config.code_intelligence.enabled = true;
    config.code_intelligence.server_startup = CodeIntelStartup::Lazy;

    let surface = build_tool_surface_without_eager_mcp_with_workspace_trust(
        &config,
        &provider.capabilities(),
        temp.path().to_path_buf(),
        sigil_mcp::unsupported_mcp_elicitation_handler(),
        sigil_mcp::unsupported_mcp_runtime_event_handler(),
        WorkspaceTrust::Trusted,
    )?;

    assert!(surface.registry.spec_for("code_symbols").is_some());
    assert!(surface.context_resolver.has_shared_code_intelligence());
    let context = surface
        .context_resolver
        .resolve("where is `hello` defined?")
        .await?;
    assert!(
        context
            .items
            .iter()
            .any(|item| item.id == "repo-file:lib.rs")
    );
    assert!(
        context
            .items
            .iter()
            .any(|item| item.id == "lsp-context:unavailable")
    );
    Ok(())
}

#[tokio::test]
async fn explicit_workspace_trust_reaches_code_intelligence_services() -> Result<()> {
    let temp = tempfile::tempdir()?;
    std::fs::write(
        temp.path().join("Cargo.toml"),
        "[package]\nname='runtime-trust-test'\nversion='0.1.0'\n",
    )?;
    std::fs::write(temp.path().join("lib.rs"), "pub fn hello() {}\n")?;
    let provider = build_provider(&test_root_config("deepseek"))?;
    let mut config = test_root_config("deepseek");
    config.code_intelligence = CodeIntelligenceConfig {
        enabled: true,
        server_startup: CodeIntelStartup::Lazy,
        servers: vec![LanguageServerConfig {
            name: "rust-analyzer".to_owned(),
            languages: vec!["rust".to_owned()],
            command: "/definitely/missing/rust-analyzer".to_owned(),
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

    let registry = build_tool_registry_without_eager_mcp_with_workspace_trust(
        &config,
        &provider.capabilities(),
        temp.path().to_path_buf(),
        sigil_mcp::unsupported_mcp_elicitation_handler(),
        sigil_mcp::unsupported_mcp_runtime_event_handler(),
        WorkspaceTrust::Denied,
    )?;
    let result = registry
        .execute(
            ToolContext::new(temp.path().to_path_buf(), 5),
            ToolCall {
                id: "runtime-denied-code-symbols".to_owned(),
                name: "code_symbols".to_owned(),
                args_json: json!({ "path": "lib.rs", "query": "hello" }).to_string(),
            },
        )
        .await?;

    assert!(!result.is_error());
    let content: serde_json::Value = serde_json::from_str(&result.content)?;
    assert_eq!(content["server"], "tree-sitter-rust");
    assert!(content["servers"].as_array().is_some_and(|servers| {
        servers.iter().any(|server| {
            server["server"] == "rust-analyzer"
                && server["status"]
                    .as_str()
                    .is_some_and(|status| status.contains("workspace trust is required"))
        })
    }));
    Ok(())
}

#[tokio::test]
async fn mcp_activate_server_tool_registers_lazy_tools_for_model_turns() -> Result<()> {
    if !command_exists_on_path("python3") {
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
    line = sys.stdin.buffer.readline()
    if not line:
        sys.exit(0)
    return json.loads(line.decode())

def write_message(message):
    data = json.dumps(message).encode()
    sys.stdout.buffer.write(data + b"\n")
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
    config.mcp_servers.push(mcp_server_config! {
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

    let activation_call = ToolCall {
        id: "activate-lazy".to_owned(),
        name: "mcp_activate_server".to_owned(),
        args_json: json!({ "server_name": "lazy" }).to_string(),
    };
    let approved_subjects = registry.permission_subjects(
        &ToolContext::new(temp.path().to_path_buf(), 5),
        &activation_call,
    )?;
    let mut stale_subjects = approved_subjects.clone();
    stale_subjects
        .iter_mut()
        .find(|subject| subject.kind == ToolSubjectKind::McpTrustClass)
        .expect("lazy activation should have a process-bound trust subject")
        .original
        .push_str(":stale");
    let stale_error = registry
        .execute(
            ToolContext::new(temp.path().to_path_buf(), 5).with_approved_subjects(stale_subjects),
            activation_call.clone(),
        )
        .await
        .expect_err("stale approval binding must fail before MCP spawn");
    assert!(
        stale_error
            .to_string()
            .contains("process binding changed after approval")
    );
    assert!(registry.spec_for("mcp__lazy__echo").is_none());

    let activation = registry
        .execute(
            ToolContext::new(temp.path().to_path_buf(), 5)
                .with_approved_subjects(approved_subjects),
            activation_call,
        )
        .await?;

    assert!(!activation.is_error());
    assert!(registry.spec_for("mcp__lazy__echo").is_some());
    let weak_registry = registry.downgrade();
    drop(registry);
    assert!(
        weak_registry.upgrade().is_none(),
        "the activation tool must not retain its containing registry"
    );
    Ok(())
}

#[tokio::test]
async fn explicit_optional_lazy_activation_fails_instead_of_reporting_empty_ready() -> Result<()> {
    let provider = build_provider(&test_root_config("deepseek"))?;
    let mut config = test_root_config("deepseek");
    config.mcp_servers.push(mcp_server_config! {
        name: "optional-missing".to_owned(),
        command: "/definitely/missing/optional-lazy-mcp".to_owned(),
        startup: McpServerStartup::Lazy,
        required: false,
        startup_timeout_secs: 5,
        ..McpServerConfig::default()
    });
    let mut registry = ToolRegistry::new();

    activate_lazy_mcp_tools_detailed(
        &mut registry,
        &config,
        &provider.capabilities(),
        std::env::current_dir()?,
        Some("optional-missing"),
    )
    .await
    .expect_err("explicit optional activation must require a callable generation");

    assert!(registry.specs().is_empty());
    Ok(())
}

#[tokio::test]
async fn refresh_mcp_server_tools_replaces_existing_server_tool_surface() -> Result<()> {
    if !command_exists_on_path("python3") {
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
    line = sys.stdin.buffer.readline()
    if not line:
        sys.exit(0)
    return json.loads(line.decode())

def write_message(message):
    data = json.dumps(message).encode()
    sys.stdout.buffer.write(data + b"\n")
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
    config.mcp_servers.push(mcp_server_config! {
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
async fn refresh_mcp_server_tools_uses_exact_server_scope_and_stable_hashed_names() -> Result<()> {
    if !command_exists_on_path("python3") {
        return Ok(());
    }
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("scoped_refresh_mcp.py");
    std::fs::write(
        &script,
        r#"
import json
import sys

label = sys.argv[1]
while True:
    line = sys.stdin.buffer.readline()
    if not line:
        sys.exit(0)
    message = json.loads(line.decode())
    method = message.get("method")
    if method == "initialize":
        result = {"protocolVersion":"2025-06-18","serverInfo":{"name":label,"version":"1.0.0"},"capabilities":{}}
    elif method == "tools/list":
        result = {"tools":[{"name":"echo","inputSchema":{"type":"object"}}]}
    elif method == "tools/call":
        result = {"content":[{"type":"text","text":label}]}
    else:
        continue
    sys.stdout.buffer.write(json.dumps({"jsonrpc":"2.0","id":message["id"],"result":result}).encode() + b"\n")
    sys.stdout.buffer.flush()
"#,
    )?;

    let provider = build_provider(&test_root_config("deepseek"))?;
    let long_name = format!("long-{}", "server".repeat(40));
    let servers = ["a-b".to_owned(), "a_b".to_owned(), long_name.clone()]
        .into_iter()
        .map(|name| {
            mcp_server_config! {
                args: vec![script.display().to_string(), name.clone()],
                name,
                command: "python3".to_owned(),
                startup_timeout_secs: 5,
                ..McpServerConfig::default()
            }
        })
        .collect::<Vec<_>>();
    let mut config = test_root_config("deepseek");
    config.mcp_servers = servers.clone();
    let mut registry = ToolRegistry::new();
    sigil_mcp::register_mcp_tools_with_options(
        &mut registry,
        &servers,
        sigil_mcp::McpToolRegistrationOptions::eager()?
            .with_capabilities(&provider.capabilities())
            .with_working_dir(temp.path().to_path_buf()),
    )
    .await?;

    let mut initial_names = registry
        .specs()
        .into_iter()
        .map(|spec| spec.name)
        .collect::<Vec<_>>();
    initial_names.sort();
    let initial_contents = mcp_tool_contents(&registry, temp.path()).await?;
    assert_eq!(
        initial_contents
            .values()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>(),
        std::collections::BTreeSet::from(["a-b".to_owned(), "a_b".to_owned(), long_name.clone()])
    );
    assert!(
        initial_names
            .iter()
            .all(|name| name.chars().count() <= provider.capabilities().tool_name_max_chars)
    );
    assert_eq!(
        registry
            .lifecycle_owners_by_scope(sigil_mcp::MCP_TOOL_LIFECYCLE_NAMESPACE, "a-b")
            .len(),
        1
    );
    assert_eq!(
        registry
            .lifecycle_owners_by_scope(sigil_mcp::MCP_TOOL_LIFECYCLE_NAMESPACE, "a_b")
            .len(),
        1
    );

    for server_name in ["a-b", "a_b", long_name.as_str()] {
        let refreshed = refresh_mcp_server_tools_with_mcp_handlers(
            &mut registry,
            &config,
            &provider.capabilities(),
            temp.path().to_path_buf(),
            server_name,
            sigil_mcp::unsupported_mcp_elicitation_handler(),
            sigil_mcp::unsupported_mcp_runtime_event_handler(),
        )
        .await?;
        assert_eq!(refreshed.removed_tools, 1);
        assert_eq!(refreshed.added_tools, 1);
    }

    let mut refreshed_names = registry
        .specs()
        .into_iter()
        .map(|spec| spec.name)
        .collect::<Vec<_>>();
    refreshed_names.sort();
    assert_eq!(refreshed_names, initial_names);
    assert_eq!(
        mcp_tool_contents(&registry, temp.path()).await?,
        initial_contents
    );
    Ok(())
}

async fn mcp_tool_contents(
    registry: &ToolRegistry,
    workspace_root: &Path,
) -> Result<BTreeMap<String, String>> {
    let mut contents = BTreeMap::new();
    for spec in registry.specs() {
        let result = registry
            .execute(
                ToolContext::new(workspace_root.to_path_buf(), 5),
                ToolCall {
                    id: format!("call-{}", spec.name),
                    name: spec.name.clone(),
                    args_json: "{}".to_owned(),
                },
            )
            .await?;
        contents.insert(spec.name, result.content);
    }
    Ok(contents)
}

#[tokio::test]
async fn refresh_mcp_server_tools_replaces_poisoned_generation_before_first_new_call() -> Result<()>
{
    if !command_exists_on_path("python3") {
        return Ok(());
    }
    let temp = tempfile::tempdir()?;
    let generation_file = temp.path().join("generation.txt");
    let script = temp.path().join("refresh_poisoned_mcp.py");
    let generation_path = serde_json::to_string(&generation_file.to_string_lossy())?;
    let script_body = r#"
import json
import pathlib
import sys

generation_file = pathlib.Path(__GENERATION_FILE__)
generation = int(generation_file.read_text()) + 1 if generation_file.exists() else 1
generation_file.write_text(str(generation))

def read_message():
    line = sys.stdin.buffer.readline()
    if not line:
        sys.exit(0)
    return json.loads(line.decode())

def write_message(message):
    sys.stdout.buffer.write(json.dumps(message).encode() + b"\n")
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    method = message.get("method")
    if method == "initialize":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"protocolVersion":"2025-06-18","serverInfo":{"name":"refresh-poisoned","version":"1.0.0"},"capabilities":{}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[{"name":"echo","description":"echo","inputSchema":{"type":"object"}}]}})
    elif method == "tools/call" and generation == 1:
        write_message({"jsonrpc":"2.0","id":message["id"]})
    elif method == "tools/call":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"content":[{"type":"text","text":"fresh-generation"}]}})
"#
    .replace("__GENERATION_FILE__", &generation_path);
    std::fs::write(&script, script_body)?;

    let provider = build_provider(&test_root_config("deepseek"))?;
    let mut config = test_root_config("deepseek");
    let server = mcp_server_config! {
        name: "poisoned".to_owned(),
        command: "python3".to_owned(),
        args: vec![script.display().to_string()],
        startup_timeout_secs: 5,
        ..McpServerConfig::default()
    };
    config.mcp_servers.push(server.clone());
    let mut registry = ToolRegistry::new();
    sigil_mcp::register_mcp_tools(&mut registry, &[server]).await?;

    let poisoned = registry
        .execute(
            ToolContext::new(temp.path().to_path_buf(), 5),
            ToolCall {
                id: "poison-old-generation".to_owned(),
                name: "mcp__poisoned__echo".to_owned(),
                args_json: "{}".to_owned(),
            },
        )
        .await?;
    assert!(poisoned.is_error());

    let refresh = refresh_mcp_server_tools_with_mcp_handlers(
        &mut registry,
        &config,
        &provider.capabilities(),
        temp.path().to_path_buf(),
        "poisoned",
        sigil_mcp::unsupported_mcp_elicitation_handler(),
        sigil_mcp::unsupported_mcp_runtime_event_handler(),
    )
    .await?;
    assert_eq!(refresh.removed_tools, 1);
    assert_eq!(refresh.added_tools, 1);

    let fresh = registry
        .execute(
            ToolContext::new(temp.path().to_path_buf(), 5),
            ToolCall {
                id: "fresh-generation-first-call".to_owned(),
                name: "mcp__poisoned__echo".to_owned(),
                args_json: "{}".to_owned(),
            },
        )
        .await?;
    assert!(!fresh.is_error());
    assert_eq!(fresh.content, "fresh-generation");
    assert_eq!(std::fs::read_to_string(generation_file)?.trim(), "2");
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn refresh_mcp_server_tools_reaps_healthy_retired_generation_process_group() -> Result<()> {
    if !command_exists_on_path("python3") {
        return Ok(());
    }
    let temp = tempfile::tempdir()?;
    let generation_file = temp.path().join("healthy-generation.txt");
    let descendant_pid_file = temp.path().join("retired-descendant.pid");
    let script = temp.path().join("refresh_healthy_mcp.py");
    let generation_path = serde_json::to_string(&generation_file.to_string_lossy())?;
    let descendant_path = serde_json::to_string(&descendant_pid_file.to_string_lossy())?;
    let script_body = r#"
import json
import pathlib
import subprocess
import sys

generation_file = pathlib.Path(__GENERATION_FILE__)
generation = int(generation_file.read_text()) + 1 if generation_file.exists() else 1
generation_file.write_text(str(generation))
if generation == 1:
    child = subprocess.Popen(["sh", "-c", "trap '' TERM; while :; do sleep 1; done"])
    pathlib.Path(__DESCENDANT_PID_FILE__).write_text(str(child.pid))

def read_message():
    line = sys.stdin.buffer.readline()
    if not line:
        sys.exit(0)
    return json.loads(line.decode())

def write_message(message):
    sys.stdout.buffer.write(json.dumps(message).encode() + b"\n")
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    method = message.get("method")
    if method == "initialize":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"capabilities":{}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[{"name":"echo","inputSchema":{"type":"object"}}]}})
    elif method == "tools/call":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"content":[]}})
"#
    .replace("__GENERATION_FILE__", &generation_path)
    .replace("__DESCENDANT_PID_FILE__", &descendant_path);
    std::fs::write(&script, script_body)?;

    let provider = build_provider(&test_root_config("deepseek"))?;
    let mut config = test_root_config("deepseek");
    let server = mcp_server_config! {
        name: "healthy-retired".to_owned(),
        command: "python3".to_owned(),
        args: vec![script.display().to_string()],
        startup_timeout_secs: 5,
        ..McpServerConfig::default()
    };
    config.mcp_servers.push(server.clone());
    let mut registry = ToolRegistry::new();
    sigil_mcp::register_mcp_tools(&mut registry, &[server]).await?;
    let descendant_pid = std::fs::read_to_string(&descendant_pid_file)?
        .trim()
        .parse::<u32>()?;

    let refresh = refresh_mcp_server_tools_with_mcp_handlers(
        &mut registry,
        &config,
        &provider.capabilities(),
        temp.path().to_path_buf(),
        "healthy-retired",
        sigil_mcp::unsupported_mcp_elicitation_handler(),
        sigil_mcp::unsupported_mcp_runtime_event_handler(),
    )
    .await?;
    assert_eq!(refresh.removed_tools, 1);
    assert_eq!(refresh.added_tools, 1);

    let mut descendant_gone = false;
    for _ in 0..40 {
        let status = Command::new("kill")
            .args(["-0", &descendant_pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?;
        if !status.success() {
            descendant_gone = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert!(
        descendant_gone,
        "refresh must reap descendants owned by the healthy retired generation"
    );
    Ok(())
}

#[tokio::test]
async fn refresh_mcp_server_tools_rolls_back_new_generation_when_old_shutdown_fails() -> Result<()>
{
    if !command_exists_on_path("python3") {
        return Ok(());
    }
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("refresh_rollback_mcp.py");
    std::fs::write(
        &script,
        r#"
import json
import sys
def read_message():
    line = sys.stdin.buffer.readline()
    if not line:
        sys.exit(0)
    return json.loads(line.decode())
def write_message(message):
    sys.stdout.buffer.write(json.dumps(message).encode() + b"\n")
    sys.stdout.buffer.flush()
while True:
    message = read_message()
    method = message.get("method")
    if method == "initialize":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"capabilities":{}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[{"name":"echo","inputSchema":{"type":"object"}}]}})
"#,
    )?;

    let provider = build_provider(&test_root_config("deepseek"))?;
    let mut config = test_root_config("deepseek");
    config.mcp_servers.push(mcp_server_config! {
        name: "rollback".to_owned(),
        command: "python3".to_owned(),
        args: vec![script.display().to_string()],
        startup_timeout_secs: 5,
        ..McpServerConfig::default()
    });
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ShutdownFailingMcpTool));

    let error = refresh_mcp_server_tools_with_mcp_handlers(
        &mut registry,
        &config,
        &provider.capabilities(),
        temp.path().to_path_buf(),
        "rollback",
        sigil_mcp::unsupported_mcp_elicitation_handler(),
        sigil_mcp::unsupported_mcp_runtime_event_handler(),
    )
    .await
    .expect_err("old generation cleanup failure must fail closed");
    assert!(
        error
            .to_string()
            .contains("failed to retire previous MCP server")
    );
    assert!(registry.spec_for("mcp__rollback__echo").is_none());
    Ok(())
}

#[tokio::test]
async fn generation_shutdown_attempts_every_distinct_owner_after_failure() {
    let first_attempts = Arc::new(AtomicUsize::new(0));
    let second_attempts = Arc::new(AtomicUsize::new(0));
    let first_owner = ToolLifecycleOwner::new(
        sigil_mcp::MCP_TOOL_LIFECYCLE_NAMESPACE,
        "shared-scope",
        "generation-1",
    );
    let second_owner = ToolLifecycleOwner::new(
        sigil_mcp::MCP_TOOL_LIFECYCLE_NAMESPACE,
        "shared-scope",
        "generation-2",
    );
    let tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(CountingShutdownTool {
            name: "first-surface",
            owner: first_owner.clone(),
            attempts: Arc::clone(&first_attempts),
        }),
        Arc::new(CountingShutdownTool {
            name: "duplicate-first-surface",
            owner: first_owner,
            attempts: Arc::clone(&first_attempts),
        }),
        Arc::new(CountingShutdownTool {
            name: "second-surface",
            owner: second_owner,
            attempts: Arc::clone(&second_attempts),
        }),
    ];

    shutdown_registered_tools(&tools)
        .await
        .expect_err("all injected generation shutdowns fail");

    assert_eq!(first_attempts.load(Ordering::Acquire), 1);
    assert_eq!(second_attempts.load(Ordering::Acquire), 1);
}

#[tokio::test]
async fn refresh_mcp_server_tools_restores_existing_tools_when_refresh_fails() -> Result<()> {
    let provider = build_provider(&test_root_config("deepseek"))?;
    let mut config = test_root_config("deepseek");
    config.mcp_servers.push(mcp_server_config! {
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

    assert!(error.to_string().contains("mcp_command_resolution_failed"));
    assert!(
        error
            .to_string()
            .contains("stdio command does not resolve to an existing file")
    );
    assert!(registry.spec_for("mcp__lazy__echo").is_some());
    Ok(())
}

#[tokio::test]
async fn refresh_optional_mcp_server_failure_preserves_healthy_old_generation() -> Result<()> {
    if !command_exists_on_path("python3") {
        return Ok(());
    }
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("optional_refresh_old_mcp.py");
    std::fs::write(
        &script,
        r#"
import json
import sys
while True:
    line = sys.stdin.buffer.readline()
    if not line:
        sys.exit(0)
    message = json.loads(line.decode())
    method = message.get("method")
    if method == "initialize":
        result = {"protocolVersion":"2025-06-18","serverInfo":{"name":"optional-refresh","version":"1.0.0"},"capabilities":{}}
    elif method == "tools/list":
        result = {"tools":[{"name":"echo","inputSchema":{"type":"object"}}]}
    elif method == "tools/call":
        result = {"content":[{"type":"text","text":"healthy-old-generation"}]}
    else:
        continue
    sys.stdout.buffer.write(json.dumps({"jsonrpc":"2.0","id":message["id"],"result":result}).encode() + b"\n")
    sys.stdout.buffer.flush()
"#,
    )?;
    let provider = build_provider(&test_root_config("deepseek"))?;
    let healthy = mcp_server_config! {
        name: "optional-refresh".to_owned(),
        command: "python3".to_owned(),
        args: vec![script.display().to_string()],
        required: false,
        startup_timeout_secs: 5,
        ..McpServerConfig::default()
    };
    let mut registry = ToolRegistry::new();
    sigil_mcp::register_mcp_tools(&mut registry, std::slice::from_ref(&healthy)).await?;

    let mut config = test_root_config("deepseek");
    config.mcp_servers.push(mcp_server_config! {
        command: "/definitely/missing/optional-mcp-server".to_owned(),
        ..healthy
    });
    refresh_mcp_server_tools_with_mcp_handlers(
        &mut registry,
        &config,
        &provider.capabilities(),
        temp.path().to_path_buf(),
        "optional-refresh",
        sigil_mcp::unsupported_mcp_elicitation_handler(),
        sigil_mcp::unsupported_mcp_runtime_event_handler(),
    )
    .await
    .expect_err("explicit refresh must not downgrade optional replacement failure to success");

    let old = registry
        .execute(
            ToolContext::new(temp.path().to_path_buf(), 5),
            ToolCall {
                id: "optional-old-still-ready".to_owned(),
                name: "mcp__optional_refresh__echo".to_owned(),
                args_json: "{}".to_owned(),
            },
        )
        .await?;
    assert!(!old.is_error());
    assert_eq!(old.content, "healthy-old-generation");
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
    config.mcp_servers.push(mcp_server_config! {
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
    config.mcp_servers.push(mcp_server_config! {
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
        None,
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
    config.mcp_servers.push(mcp_server_config! {
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
        None,
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
    config.mcp_servers.push(mcp_server_config! {
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
    )?;

    assert!(registry.spec_for("read_file").is_some());
    assert!(registry.spec_for("mcp__required_eager__echo").is_none());
    Ok(())
}

#[tokio::test]
async fn mcp_activate_server_tool_reports_unknown_and_already_ready_states() -> Result<()> {
    let provider = build_provider(&test_root_config("deepseek"))?;
    let mut config = test_root_config("deepseek");
    config.mcp_servers.push(mcp_server_config! {
        name: "lazy".to_owned(),
        command: std::env::current_exe()?.display().to_string(),
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
    assert_eq!(spec.access, ToolAccess::Execute);
    assert_eq!(spec.network_effect, Some(NetworkEffect::Unknown));
    assert_eq!(spec.preview, ToolPreviewCapability::None);
    let activation_call = ToolCall {
        id: "activate-permission-contract".to_owned(),
        name: "mcp_activate_server".to_owned(),
        args_json: json!({"server_name": "lazy"}).to_string(),
    };
    let activation_context = ToolContext::new(std::env::current_dir()?, 5);
    assert_eq!(
        registry.permission_operation(&activation_context, &activation_call)?,
        sigil_kernel::ToolOperation::NetworkRequest
    );
    assert_eq!(
        registry.permission_network_effect(&activation_context, &activation_call)?,
        Some(NetworkEffect::Unknown)
    );

    let missing_name = registry.permission_subjects(
        &ToolContext::new(std::env::current_dir()?, 5),
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
        &ToolContext::new(std::env::current_dir()?, 5),
        &ToolCall {
            id: "activate-unknown-default".to_owned(),
            name: "mcp_activate_server".to_owned(),
            args_json: json!({"server_name": "other"}).to_string(),
        },
    )?;
    assert_eq!(unknown_default, None);

    let unknown_audit = registry.egress_audit(
        &ToolContext::new(std::env::current_dir()?, 5),
        &ToolCall {
            id: "activate-unknown-audit".to_owned(),
            name: "mcp_activate_server".to_owned(),
            args_json: json!({"server_name": "other"}).to_string(),
        },
    )?;
    assert!(unknown_audit.is_none());

    let unknown = registry
        .execute(
            ToolContext::new(std::env::current_dir()?, 5),
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
        &ToolContext::new(std::env::current_dir()?, 5),
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
        &ToolContext::new(std::env::current_dir()?, 5),
        &ToolCall {
            id: "activate-lazy-default".to_owned(),
            name: "mcp_activate_server".to_owned(),
            args_json: json!({"server_name": "lazy"}).to_string(),
        },
    )?;
    assert_eq!(default_mode, Some(ApprovalMode::Ask));

    let audit = registry.egress_audit(
        &ToolContext::new(std::env::current_dir()?, 5),
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
            ToolContext::new(std::env::current_dir()?, 5),
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

#[tokio::test]
async fn mcp_activate_server_uses_its_own_lifecycle_mutation_evidence() -> Result<()> {
    let provider = build_provider(&test_root_config("deepseek"))?;
    let mut config = test_root_config("deepseek");
    config.mcp_servers.push(mcp_server_config! {
        name: "lifecycle-owned".to_owned(),
        command: std::env::current_exe()?.display().to_string(),
        startup: McpServerStartup::Lazy,
        ..McpServerConfig::default()
    });
    let workspace = tempfile::tempdir()?;
    let registry = build_tool_registry_without_eager_mcp_with_workspace_trust(
        &config,
        &provider.capabilities(),
        workspace.path().to_path_buf(),
        sigil_mcp::unsupported_mcp_elicitation_handler(),
        sigil_mcp::unsupported_mcp_runtime_event_handler(),
        WorkspaceTrust::Trusted,
    )?;
    let store = JsonlSessionStore::new(workspace.path().join("session.jsonl"))?;
    let context = ToolContext::new(workspace.path(), 5)
        .with_mutation_recorder(MutationEventRecorder::new(store));
    let activation = ToolCall {
        id: "activate-lifecycle-owned".to_owned(),
        name: "mcp_activate_server".to_owned(),
        args_json: json!({ "server_name": "lifecycle-owned" }).to_string(),
    };

    assert!(
        registry
            .execution_mutation_profile(&context, &activation)?
            .is_none()
    );
    Ok(())
}

#[test]
fn mcp_activate_server_tool_respects_disabled_egress_logging() -> Result<()> {
    let provider = build_provider(&test_root_config("deepseek"))?;
    let mut config = test_root_config("deepseek");
    config.mcp_servers.push(mcp_server_config! {
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
        &ToolContext::new(std::env::current_dir()?, 5),
        &ToolCall {
            id: "quiet-audit".to_owned(),
            name: "mcp_activate_server".to_owned(),
            args_json: json!({"server_name": "quiet-lazy"}).to_string(),
        },
    )?;

    assert!(audit.is_none());
    Ok(())
}

struct EagerRemoteReceiptPresenter;

#[async_trait]
impl sigil_kernel::EgressDisclosurePresenter for EagerRemoteReceiptPresenter {
    async fn present(
        &self,
        disclosure: sigil_kernel::PreEgressDisclosure,
    ) -> std::result::Result<
        sigil_kernel::DisclosurePresentationReceipt,
        sigil_kernel::DisclosurePresentationError,
    > {
        disclosure.presentation_receipt("runtime-eager-remote-fixture-v1")
    }
}

async fn read_fixture_http_request(socket: &mut tokio::net::TcpStream) -> Result<String> {
    use tokio::io::AsyncReadExt;

    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let read = socket.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(header_end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            let content_length = String::from_utf8_lossy(&buffer[..header_end])
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            if buffer.len() >= header_end + 4 + content_length {
                break;
            }
        }
    }
    Ok(String::from_utf8_lossy(&buffer).into_owned())
}

async fn write_fixture_http_response(
    socket: &mut tokio::net::TcpStream,
    status: &str,
    content_type: Option<&str>,
    body: &str,
) -> Result<()> {
    use tokio::io::AsyncWriteExt;

    let mut head = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if let Some(content_type) = content_type {
        head.push_str(&format!("Content-Type: {content_type}\r\n"));
    }
    head.push_str("\r\n");
    socket.write_all(head.as_bytes()).await?;
    socket.write_all(body.as_bytes()).await?;
    Ok(())
}

fn eager_remote_config(
    host: &str,
    port: u16,
    network_mode: sigil_kernel::NetworkPolicy,
) -> RootConfig {
    let mut config = test_root_config("deepseek");
    config.web.enabled = true;
    config.web.network_mode = network_mode;
    config.web.allow_http = true;
    config.web.proxy_mode = sigil_kernel::WebProxyMode::Direct;
    config.web.allowed_ports = vec![port];
    config.web.allowed_private_hosts.clear();
    config.web.allowed_private_cidrs.clear();
    config.mcp_servers.push(McpServerConfig {
        name: "remote-eager".to_owned(),
        transport: sigil_kernel::McpServerTransportConfig::StreamableHttp(
            sigil_kernel::McpStreamableHttpConfig {
                url: format!("http://{host}:{port}/mcp"),
                http_headers: BTreeMap::new(),
                env_http_headers: BTreeMap::new(),
                bearer_token_env_var: None,
                client_capabilities: Default::default(),
            },
        ),
        startup: McpServerStartup::Eager,
        required: true,
        trust: McpServerTrustPolicy {
            approval_default: ApprovalMode::Allow,
            ..McpServerTrustPolicy::default()
        },
        ..McpServerConfig::default()
    });
    config
}

fn eager_remote_recorder(temp: &tempfile::TempDir) -> Result<sigil_kernel::EgressAuditRecorder> {
    let store = JsonlSessionStore::new(temp.path().join("eager-remote-session.jsonl"))?;
    Ok(Session::new("deepseek", "test")
        .with_store(store)
        .egress_audit_recorder()?)
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn eager_remote_streamable_http_activates_real_transport_and_registers_tools() -> Result<()> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let proxy_port = listener.local_addr()?.port();
    let server = tokio::spawn(async move {
        let responses = [
            (
                "200 OK",
                Some("application/json"),
                json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": {
                        "protocolVersion": "2025-06-18",
                        "capabilities": { "tools": {} },
                        "serverInfo": { "name": "runtime-eager-fixture", "version": "1.0.0" }
                    }
                })
                .to_string(),
            ),
            ("202 Accepted", None, String::new()),
            (
                "200 OK",
                Some("application/json"),
                json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "result": {
                        "tools": [{
                            "name": "echo",
                            "description": "Echo fixture",
                            "inputSchema": { "type": "object" }
                        }]
                    }
                })
                .to_string(),
            ),
        ];
        let mut requests = Vec::new();
        for (status, content_type, body) in responses {
            let (mut socket, _) = listener.accept().await?;
            requests.push(read_fixture_http_request(&mut socket).await?);
            write_fixture_http_response(&mut socket, status, content_type, &body).await?;
        }
        Result::<Vec<String>>::Ok(requests)
    });

    let temp = tempfile::tempdir()?;
    let _environment_guard = crate::test_env::lock();
    let proxy = format!("http://127.0.0.1:{proxy_port}");
    let _environment = EnvScope::set_owned(&[
        ("HTTP_PROXY", proxy.clone()),
        ("http_proxy", proxy),
        ("NO_PROXY", String::new()),
        ("no_proxy", String::new()),
    ]);
    let mut config = eager_remote_config("fixture.invalid", 80, sigil_kernel::NetworkPolicy::Allow);
    config.web.proxy_mode = sigil_kernel::WebProxyMode::Environment;
    let mut registry = ToolRegistry::new();
    let added = activate_eager_remote_mcp_server(
        &mut registry,
        &config,
        "remote-eager",
        128,
        temp.path().to_path_buf(),
        eager_remote_recorder(&temp)?,
        Arc::new(EagerRemoteReceiptPresenter),
        sigil_mcp::unsupported_mcp_elicitation_handler(),
    )
    .await?;

    assert_eq!(added, 1);
    assert!(registry.spec_for("mcp__remote_eager__echo").is_some());
    let requests = server.await??;
    assert_eq!(requests.len(), 3);
    assert!(requests[0].contains("\"method\":\"initialize\""));
    assert!(requests[1].contains("\"method\":\"notifications/initialized\""));
    assert!(requests[2].contains("\"method\":\"tools/list\""));
    Ok(())
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn configured_remote_refresh_failure_preserves_previous_generation() -> Result<()> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let proxy_port = listener.local_addr()?.port();
    let server = tokio::spawn(async move {
        let responses = [
            (
                "200 OK",
                Some("application/json"),
                json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": {
                        "protocolVersion": "2025-06-18",
                        "capabilities": { "tools": {} },
                        "serverInfo": { "name": "runtime-refresh-fixture", "version": "1.0.0" }
                    }
                })
                .to_string(),
            ),
            ("202 Accepted", None, String::new()),
            (
                "200 OK",
                Some("application/json"),
                json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "result": {
                        "tools": [{
                            "name": "echo",
                            "description": "Echo fixture",
                            "inputSchema": { "type": "object" }
                        }]
                    }
                })
                .to_string(),
            ),
        ];
        for (status, content_type, body) in responses {
            let (mut socket, _) = listener.accept().await?;
            let _ = read_fixture_http_request(&mut socket).await?;
            write_fixture_http_response(&mut socket, status, content_type, &body).await?;
        }
        Result::<()>::Ok(())
    });

    let temp = tempfile::tempdir()?;
    let _environment_guard = crate::test_env::lock();
    let proxy = format!("http://127.0.0.1:{proxy_port}");
    let _environment = EnvScope::set_owned(&[
        ("HTTP_PROXY", proxy.clone()),
        ("http_proxy", proxy),
        ("NO_PROXY", String::new()),
        ("no_proxy", String::new()),
    ]);
    let mut config = eager_remote_config("fixture.invalid", 80, sigil_kernel::NetworkPolicy::Allow);
    config.web.proxy_mode = sigil_kernel::WebProxyMode::Environment;
    let mut registry = ToolRegistry::new();
    let _ = activate_eager_remote_mcp_server(
        &mut registry,
        &config,
        "remote-eager",
        128,
        temp.path().to_path_buf(),
        eager_remote_recorder(&temp)?,
        Arc::new(EagerRemoteReceiptPresenter),
        sigil_mcp::unsupported_mcp_elicitation_handler(),
    )
    .await?;
    server.await??;
    let previous_owners =
        registry.lifecycle_owners_by_scope(sigil_mcp::MCP_TOOL_LIFECYCLE_NAMESPACE, "remote-eager");
    assert_eq!(previous_owners.len(), 1);

    let sigil_kernel::McpServerTransportConfig::StreamableHttp(remote) =
        &mut config.mcp_servers[0].transport
    else {
        panic!("fixture transport is remote");
    };
    remote.url = "not a valid URL".to_owned();
    let error = activate_or_refresh_configured_remote_mcp_server(
        &mut registry,
        &config,
        "remote-eager",
        128,
        temp.path().to_path_buf(),
        eager_remote_recorder(&temp)?,
        Arc::new(EagerRemoteReceiptPresenter),
        sigil_mcp::unsupported_mcp_elicitation_handler(),
    )
    .await
    .expect_err("invalid replacement endpoint must fail before registry mutation");

    assert!(error.to_string().contains("invalid remote MCP endpoint"));
    assert!(registry.spec_for("mcp__remote_eager__echo").is_some());
    assert_eq!(
        registry.lifecycle_owners_by_scope(sigil_mcp::MCP_TOOL_LIFECYCLE_NAMESPACE, "remote-eager"),
        previous_owners
    );
    Ok(())
}

#[tokio::test]
async fn eager_remote_ask_policy_fails_before_socket_activity() -> Result<()> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let temp = tempfile::tempdir()?;
    let config = eager_remote_config("127.0.0.1", port, sigil_kernel::NetworkPolicy::Ask);
    let mut registry = ToolRegistry::new();
    let error = activate_eager_remote_mcp_server(
        &mut registry,
        &config,
        "remote-eager",
        128,
        temp.path().to_path_buf(),
        eager_remote_recorder(&temp)?,
        Arc::new(EagerRemoteReceiptPresenter),
        sigil_mcp::unsupported_mcp_elicitation_handler(),
    )
    .await
    .expect_err("ask must fail closed without an interactive approval surface");

    assert!(error.to_string().contains("web.network_mode = allow"));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), listener.accept())
            .await
            .is_err(),
        "eager ask rejection must happen before socket activity"
    );
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
            // SAFETY: tests serialize process-wide env mutation with crate::test_env.
            unsafe { env::set_var(name, value) };
        }
        Self { saved }
    }

    fn set_owned(values: &[(&'static str, String)]) -> Self {
        let mut saved = Vec::with_capacity(values.len());
        for (name, value) in values {
            saved.push((*name, env::var_os(name)));
            // SAFETY: tests serialize process-wide env mutation with crate::test_env.
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
                    // SAFETY: tests serialize process-wide env mutation with crate::test_env.
                    unsafe { env::set_var(name, value) };
                }
                None => {
                    // SAFETY: tests serialize process-wide env mutation with crate::test_env.
                    unsafe { env::remove_var(name) };
                }
            }
        }
    }
}
