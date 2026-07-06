use std::{collections::BTreeMap, env, path::Path, sync::Mutex, time::Duration};

use super::{
    CodeIntelStartup, CompactionConfig, CompactionThresholdStatus, ConfigPlatform, McpServerConfig,
    McpServerStartup, McpTrustClass, ModelRequestConfig, RootConfig,
    SIGIL_MODEL_REQUEST_TIMEOUT_SECS_ENV, SIGIL_MODEL_STREAM_IDLE_TIMEOUT_SECS_ENV,
    SIGIL_MODEL_STREAM_TOTAL_TIMEOUT_SECS_ENV, SyntaxThemeId, TerminalKeyboardEnhancement, ThemeId,
    UsageCostCurrency, default_user_config_dir, default_user_config_path, preferred_config_path,
    preferred_config_path_for_known_paths, resolve_workspace_root, user_home_dir_from_env,
};
use crate::{
    AgentConfig, AgentRole, ApprovalMode, ExecutionBackendCapabilities, ExecutionBackendKind,
    ExecutionCapability, ExecutionIsolationPolicy, ExecutionSandboxFallback,
    ExecutionSandboxProfile, MultiAgentMode, SkillConfig, StorageConfig, StorageRoot, TaskConfig,
    TaskMode, WorkspaceConfig,
};

static ENV_LOCK: Mutex<()> = Mutex::new(());

struct EnvScope {
    previous: Vec<(&'static str, Option<String>)>,
}

impl EnvScope {
    fn set_many(values: &[(&'static str, &'static str)]) -> Self {
        let previous = values
            .iter()
            .map(|(name, _)| (*name, env::var(name).ok()))
            .collect::<Vec<_>>();
        for (name, value) in values {
            // SAFETY: tests that mutate process environment take ENV_LOCK for their full scope.
            unsafe { env::set_var(name, value) };
        }
        Self { previous }
    }

    fn clear_model_request() -> Self {
        let names = [
            SIGIL_MODEL_REQUEST_TIMEOUT_SECS_ENV,
            SIGIL_MODEL_STREAM_IDLE_TIMEOUT_SECS_ENV,
            SIGIL_MODEL_STREAM_TOTAL_TIMEOUT_SECS_ENV,
        ];
        let previous = names
            .iter()
            .map(|name| (*name, env::var(name).ok()))
            .collect::<Vec<_>>();
        for name in names {
            // SAFETY: tests that mutate process environment take ENV_LOCK for their full scope.
            unsafe { env::remove_var(name) };
        }
        Self { previous }
    }
}

impl Drop for EnvScope {
    fn drop(&mut self) {
        for (name, value) in &self.previous {
            if let Some(value) = value {
                // SAFETY: tests that mutate process environment take ENV_LOCK for their full scope.
                unsafe { env::set_var(name, value) };
            } else {
                // SAFETY: tests that mutate process environment take ENV_LOCK for their full scope.
                unsafe { env::remove_var(name) };
            }
        }
    }
}

#[test]
fn compaction_threshold_status_follows_configured_window() {
    let config = CompactionConfig {
        enabled: true,
        soft_threshold_ratio: 0.5,
        hard_threshold_ratio: 0.8,
        context_window_tokens: Some(100),
        tail_messages: 6,
    };

    assert_eq!(config.threshold_status(0), CompactionThresholdStatus::Ready);
    assert_eq!(config.threshold_status(50), CompactionThresholdStatus::Soft);
    assert_eq!(config.threshold_status(80), CompactionThresholdStatus::Hard);
}

#[test]
fn compaction_window_loads_and_saves_fallback_key() {
    let raw = r#"
[agent]
provider = "deepseek"
model = "deepseek-v4-pro"

[compaction]
fallback_context_window_tokens = 128000
"#;

    let config: RootConfig = toml::from_str(raw).expect("config should load");
    assert_eq!(config.compaction.context_window_tokens, Some(128_000));

    let rendered = toml::to_string_pretty(&config).expect("config should serialize");
    assert!(rendered.contains("fallback_context_window_tokens = 128000"));
    assert!(
        !rendered
            .lines()
            .any(|line| line.trim_start().starts_with("context_window_tokens ="))
    );
}

#[test]
fn compaction_window_rejects_legacy_context_window_key() {
    let raw = r#"
[agent]
provider = "deepseek"
model = "deepseek-v4-pro"

[compaction]
context_window_tokens = 128000
"#;

    let error = toml::from_str::<RootConfig>(raw).expect_err("legacy key should be rejected");
    assert!(error.to_string().contains("context_window_tokens"));
}

fn assert_root_config_rejects(raw: &str, expected: &str) {
    let error = toml::from_str::<RootConfig>(raw).expect_err("config should be rejected");
    let message = error.to_string();
    assert!(
        message.contains(expected),
        "expected error to contain {expected:?}, got {message:?}"
    );
}

#[test]
fn skill_config_defaults_and_toml_overrides_are_stable() {
    let defaults = SkillConfig::default();
    assert!(defaults.enabled);
    assert!(defaults.user_skills);
    assert!(defaults.user_agents);
    assert!(defaults.compatibility_sources.is_empty());

    let raw = r#"
[agent]
provider = "deepseek"
model = "deepseek-v4-pro"

[skills]
enabled = false
user_skills = false
user_agents = false
compatibility_sources = ["opencode"]
"#;

    let config: RootConfig = toml::from_str(raw).expect("skills config should load");
    assert!(!config.skills.enabled);
    assert!(!config.skills.user_skills);
    assert!(!config.skills.user_agents);
    assert_eq!(config.skills.compatibility_sources, vec!["opencode"]);
}

#[test]
fn removed_project_asset_config_keys_are_rejected() {
    assert_root_config_rejects(
        r#"
[agent]
provider = "deepseek"
model = "deepseek-v4-pro"

[storage]
project_assets_root = "project-assets"
"#,
        "project_assets_root",
    );
    assert_root_config_rejects(
        r#"
[agent]
provider = "deepseek"
model = "deepseek-v4-pro"

[skills]
workspace_dir = "skills"
"#,
        "workspace_dir",
    );
    assert_root_config_rejects(
        r#"
[agent]
provider = "deepseek"
model = "deepseek-v4-pro"

[skills]
workspace_agents_dir = "agents"
"#,
        "workspace_agents_dir",
    );
}

#[test]
fn removed_permission_config_keys_are_rejected() {
    assert_root_config_rejects(
        r#"
[agent]
provider = "deepseek"
model = "deepseek-v4-pro"

[permission]
preset = "balanced"
"#,
        "preset",
    );
    assert_root_config_rejects(
        r#"
[agent]
provider = "deepseek"
model = "deepseek-v4-pro"

[permission]
default_mode = "ask"
"#,
        "default_mode",
    );
    assert_root_config_rejects(
        r#"
[agent]
provider = "deepseek"
model = "deepseek-v4-pro"

[permission.access]
write = "allow"
"#,
        "access",
    );
}

#[test]
fn model_request_config_has_user_visible_defaults() {
    let config = ModelRequestConfig::default();
    let timeouts = config.to_timeouts().expect("defaults should resolve");

    assert_eq!(config.request_timeout_secs, 120);
    assert_eq!(config.stream_idle_timeout_secs, 180);
    assert_eq!(config.stream_total_timeout_secs, None);
    assert_eq!(timeouts.request_timeout, Duration::from_secs(120));
    assert_eq!(timeouts.stream_idle_timeout, Duration::from_secs(180));
    assert_eq!(timeouts.stream_total_timeout, None);
}

#[test]
fn root_config_loads_model_request_from_toml() {
    let raw = r#"
[agent]
provider = "deepseek"
model = "deepseek-v4-pro"

[model_request]
request_timeout_secs = 7
stream_idle_timeout_secs = 11
stream_total_timeout_secs = 17
"#;

    let config: RootConfig = toml::from_str(raw).expect("model request config should parse");
    let timeouts = config
        .model_request
        .to_timeouts()
        .expect("model request config should resolve");

    assert_eq!(config.model_request.request_timeout_secs, 7);
    assert_eq!(config.model_request.stream_idle_timeout_secs, 11);
    assert_eq!(config.model_request.stream_total_timeout_secs, Some(17));
    assert_eq!(timeouts.request_timeout, Duration::from_secs(7));
    assert_eq!(timeouts.stream_idle_timeout, Duration::from_secs(11));
    assert_eq!(timeouts.stream_total_timeout, Some(Duration::from_secs(17)));
}

#[test]
fn model_request_config_rejects_zero_values_when_resolved() {
    for config in [
        ModelRequestConfig {
            request_timeout_secs: 0,
            ..ModelRequestConfig::default()
        },
        ModelRequestConfig {
            stream_idle_timeout_secs: 0,
            ..ModelRequestConfig::default()
        },
        ModelRequestConfig {
            stream_total_timeout_secs: Some(0),
            ..ModelRequestConfig::default()
        },
    ] {
        let error = config
            .to_timeouts()
            .expect_err("zero timeout should fail resolution");
        assert!(error.to_string().contains("must be greater than 0"));
    }
}

#[test]
fn root_config_load_applies_provider_neutral_model_request_env_overrides() {
    let _guard = ENV_LOCK.lock().expect("env lock should acquire");
    let _clear = EnvScope::clear_model_request();
    let _scope = EnvScope::set_many(&[
        (SIGIL_MODEL_REQUEST_TIMEOUT_SECS_ENV, "13"),
        (SIGIL_MODEL_STREAM_IDLE_TIMEOUT_SECS_ENV, "29"),
        (SIGIL_MODEL_STREAM_TOTAL_TIMEOUT_SECS_ENV, "31"),
    ]);
    let temp = tempfile::tempdir().expect("tempdir should build");
    let path = temp.path().join("sigil.toml");
    std::fs::write(
        &path,
        r#"
[agent]
provider = "deepseek"
model = "deepseek-v4-pro"

[model_request]
request_timeout_secs = 7
stream_idle_timeout_secs = 11
"#,
    )
    .expect("config should write");

    let config = RootConfig::load(&path).expect("config should load");

    assert_eq!(config.model_request.request_timeout_secs, 13);
    assert_eq!(config.model_request.stream_idle_timeout_secs, 29);
    assert_eq!(config.model_request.stream_total_timeout_secs, Some(31));
}

#[test]
fn root_config_load_rejects_invalid_model_request_env_override() {
    let _guard = ENV_LOCK.lock().expect("env lock should acquire");
    let _clear = EnvScope::clear_model_request();
    let _scope = EnvScope::set_many(&[(SIGIL_MODEL_STREAM_IDLE_TIMEOUT_SECS_ENV, "0")]);
    let temp = tempfile::tempdir().expect("tempdir should build");
    let path = temp.path().join("sigil.toml");
    std::fs::write(
        &path,
        r#"
[agent]
provider = "deepseek"
model = "deepseek-v4-pro"
"#,
    )
    .expect("config should write");

    let error = RootConfig::load(&path).expect_err("invalid override should fail");

    assert!(
        error
            .to_string()
            .contains("SIGIL_MODEL_STREAM_IDLE_TIMEOUT_SECS must be greater than 0")
    );
}

#[test]
fn storage_root_toml_parses_auto_paths_and_rejects_empty_values() {
    #[derive(Debug, serde::Deserialize)]
    struct StorageRootFixture {
        value: StorageRoot,
    }

    let auto: StorageRootFixture = toml::from_str(r#"value = "auto""#).expect("auto should parse");
    assert_eq!(auto.value, StorageRoot::Auto);

    let explicit: StorageRootFixture =
        toml::from_str(r#"value = "/tmp/sigil-state""#).expect("path should parse");
    assert_eq!(
        explicit.value,
        StorageRoot::Path("/tmp/sigil-state".to_owned())
    );

    let error =
        toml::from_str::<StorageRootFixture>(r#"value = """#).expect_err("empty path should fail");
    assert!(
        error
            .to_string()
            .contains("storage root path cannot be empty")
    );
}

#[test]
fn storage_mutation_artifact_retention_has_user_visible_defaults_and_overrides() {
    let config = StorageConfig::default();
    assert_eq!(
        config.mutation_artifact_retention.max_artifacts,
        Some(crate::DEFAULT_MUTATION_ARTIFACT_RETENTION_MAX_ARTIFACTS)
    );
    assert_eq!(
        config.mutation_artifact_retention.max_bytes,
        Some(crate::DEFAULT_MUTATION_ARTIFACT_RETENTION_MAX_BYTES)
    );
    assert_eq!(
        config.mutation_artifact_retention.expire_older_than_ms,
        Some(crate::DEFAULT_MUTATION_ARTIFACT_RETENTION_EXPIRE_OLDER_THAN_MS)
    );
    let policy = config.mutation_artifact_retention.to_policy();
    assert_eq!(
        policy.max_artifacts,
        config.mutation_artifact_retention.max_artifacts
    );
    assert_eq!(
        policy.max_bytes,
        config.mutation_artifact_retention.max_bytes
    );
    assert_eq!(
        policy.expire_older_than_ms,
        config.mutation_artifact_retention.expire_older_than_ms
    );

    let parsed: StorageConfig = toml::from_str(
        r#"
[mutation_artifact_retention]
max_artifacts = 42
max_bytes = 1048576
expire_older_than_ms = 60000
"#,
    )
    .expect("storage retention config should parse");

    assert_eq!(parsed.mutation_artifact_retention.max_artifacts, Some(42));
    assert_eq!(
        parsed.mutation_artifact_retention.max_bytes,
        Some(1_048_576)
    );
    assert_eq!(
        parsed.mutation_artifact_retention.expire_older_than_ms,
        Some(60_000)
    );
}

#[test]
fn preferred_config_path_uses_explicit_or_user_config_file() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    let explicit = temp.path().join("explicit.toml");
    assert_eq!(
        preferred_config_path(Some(&explicit), temp.path()).expect("explicit path should win"),
        explicit
    );

    let local = temp.path().join("sigil.toml");
    std::fs::write(&local, "").expect("local config should write");
    assert_eq!(
        preferred_config_path(None, temp.path()).expect("user config path should win"),
        default_user_config_path().expect("default user config path should resolve")
    );
}

#[test]
fn root_config_save_roundtrips() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    let path = temp.path().join("nested").join("sigil.toml");
    let config = RootConfig {
        workspace: WorkspaceConfig {
            root: "/tmp/workspace".to_owned(),
        },
        storage: Default::default(),
        session: Default::default(),
        agent: AgentConfig {
            provider: "deepseek".to_owned(),
            model: "deepseek-v4-flash".to_owned(),
            max_turns: Some(32),
            tool_timeout_secs: 30,
        },
        model_request: Default::default(),
        permission: Default::default(),
        memory: Default::default(),
        skills: Default::default(),
        compaction: Default::default(),
        code_intelligence: Default::default(),
        terminal: Default::default(),
        execution: Default::default(),
        verification: Default::default(),
        appearance: Default::default(),
        task: Default::default(),
        providers: BTreeMap::new(),
        mcp_servers: Vec::new(),
    };

    config.save(&path).expect("config should save");
    let loaded = RootConfig::load(&path).expect("config should reload");
    assert_eq!(loaded.workspace.root, "/tmp/workspace");
    assert_eq!(loaded.agent.provider, "deepseek");
}

#[test]
fn root_config_save_handles_paths_without_parent() {
    let file_name = format!("sigil-config-test-{}.toml", uuid::Uuid::new_v4());
    let path = Path::new(&file_name);

    let config = RootConfig {
        workspace: WorkspaceConfig::default(),
        storage: Default::default(),
        session: Default::default(),
        agent: AgentConfig {
            provider: "deepseek".to_owned(),
            model: "deepseek-v4-flash".to_owned(),
            max_turns: None,
            tool_timeout_secs: 30,
        },
        model_request: Default::default(),
        permission: Default::default(),
        memory: Default::default(),
        skills: Default::default(),
        compaction: Default::default(),
        code_intelligence: Default::default(),
        terminal: Default::default(),
        execution: Default::default(),
        verification: Default::default(),
        appearance: Default::default(),
        task: Default::default(),
        providers: BTreeMap::new(),
        mcp_servers: Vec::new(),
    };

    config
        .save(path)
        .expect("path without parent should save in cwd");

    assert!(path.exists());
    std::fs::remove_file(path).expect("temporary config should clean up");
}

#[test]
fn root_config_loads_agent_and_session_defaults() {
    let config: RootConfig = toml::from_str(
        r#"
[agent]
provider = "deepseek"
model = "deepseek-v4-flash"
"#,
    )
    .expect("minimal config should parse");

    assert_eq!(config.workspace.root, ".");
    assert_eq!(config.session.log_dir, None);
    assert_eq!(config.storage, Default::default());
    assert_eq!(config.agent.tool_timeout_secs, 30);
    assert_eq!(config.memory, Default::default());
    assert_eq!(config.compaction.tail_messages, 6);
    assert_eq!(config.terminal, Default::default());
    assert_eq!(config.appearance.theme, ThemeId::SigilDark);
    assert_eq!(config.appearance.syntax_theme, SyntaxThemeId::Auto);
    assert_eq!(
        config.appearance.usage_cost_currency,
        UsageCostCurrency::Auto
    );
    assert!(config.appearance.colors.is_empty());
    assert_eq!(config.task.default_mode, TaskMode::Chat);
}

#[test]
fn root_config_loads_appearance_config() {
    let config: RootConfig = toml::from_str(
        r##"
[agent]
provider = "deepseek"
model = "deepseek-v4-flash"

[appearance]
theme = "solarized_dark"
syntax_theme = "solarized_dark"
usage_cost_currency = "cny"

[appearance.colors]
surface_base = "#002b36"
accent_primary = "#b58900"
"##,
    )
    .expect("appearance config should parse");

    assert_eq!(config.appearance.theme, ThemeId::SolarizedDark);
    assert_eq!(config.appearance.syntax_theme, SyntaxThemeId::SolarizedDark);
    assert_eq!(
        config.appearance.usage_cost_currency,
        UsageCostCurrency::Cny
    );
    assert_eq!(config.appearance.colors.len(), 2);
    assert_eq!(
        config.appearance.colors.get("surface_base"),
        Some("#002b36")
    );
    assert_eq!(
        config.appearance.colors.get("accent_primary"),
        Some("#b58900")
    );
}

#[test]
fn syntax_theme_ids_have_stable_labels_and_display_names() {
    let values = SyntaxThemeId::all()
        .iter()
        .map(|theme| (theme.as_str(), theme.display_label()))
        .collect::<Vec<_>>();

    assert_eq!(
        values,
        vec![
            ("auto", "Auto"),
            ("catppuccin_mocha", "Catppuccin Mocha"),
            ("catppuccin_latte", "Catppuccin Latte"),
            ("solarized_dark", "Solarized Dark"),
            ("solarized_light", "Solarized Light"),
            ("gruvbox_dark", "Gruvbox Dark"),
            ("gruvbox_light", "Gruvbox Light"),
            ("nord", "Nord"),
            ("one_half_dark", "One Half Dark"),
            ("one_half_light", "One Half Light"),
            ("monokai", "Monokai"),
        ]
    );
}

#[test]
fn usage_cost_currency_helpers_are_stable() {
    assert_eq!(
        UsageCostCurrency::all(),
        &[
            UsageCostCurrency::Auto,
            UsageCostCurrency::Usd,
            UsageCostCurrency::Cny
        ]
    );
    for (currency, id, label, next) in [
        (
            UsageCostCurrency::Auto,
            "auto",
            "Auto",
            UsageCostCurrency::Usd,
        ),
        (UsageCostCurrency::Usd, "usd", "USD", UsageCostCurrency::Cny),
        (
            UsageCostCurrency::Cny,
            "cny",
            "CNY",
            UsageCostCurrency::Auto,
        ),
    ] {
        assert_eq!(currency.as_str(), id);
        assert_eq!(currency.display_label(), label);
        assert_eq!(currency.next(), next);
    }
}

#[test]
fn config_path_env_helpers_cover_all_platform_rules() {
    assert!(matches!(
        super::current_config_platform(),
        ConfigPlatform::Windows | ConfigPlatform::Macos | ConfigPlatform::Other
    ));
    assert_eq!(
        super::current_config_platform_from_os("windows"),
        ConfigPlatform::Windows
    );
    assert_eq!(
        super::current_config_platform_from_os("macos"),
        ConfigPlatform::Macos
    );
    assert_eq!(
        super::current_config_platform_from_os("linux"),
        ConfigPlatform::Other
    );
    assert_eq!(
        user_home_dir_from_env(
            ConfigPlatform::Windows,
            Some("/home/fallback".into()),
            Some("C:/Users/Alice".into())
        )
        .expect("windows userprofile should win"),
        Path::new("C:/Users/Alice")
    );
    assert_eq!(
        user_home_dir_from_env(ConfigPlatform::Windows, Some("/home/fallback".into()), None)
            .expect("windows HOME should be fallback"),
        Path::new("/home/fallback")
    );
    assert!(user_home_dir_from_env(ConfigPlatform::Windows, None, None).is_err());
    assert_eq!(
        user_home_dir_from_env(ConfigPlatform::Macos, Some("/Users/alice".into()), None)
            .expect("mac HOME should resolve"),
        Path::new("/Users/alice")
    );
    assert!(user_home_dir_from_env(ConfigPlatform::Other, None, None).is_err());
}

#[test]
fn preferred_config_path_known_paths_cover_explicit_and_default_edges() {
    let temp = tempfile::tempdir().expect("tempdir");
    let explicit = temp.path().join("explicit.toml");
    let default_path = temp.path().join("home/.sigil/sigil.toml");
    assert_eq!(
        preferred_config_path_for_known_paths(Some(&explicit), default_path.clone()),
        explicit
    );
    assert_eq!(
        preferred_config_path_for_known_paths(None, default_path.clone()),
        default_path
    );
}

#[test]
fn root_config_rejects_unknown_theme() {
    let error = toml::from_str::<RootConfig>(
        r#"
[agent]
provider = "deepseek"
model = "deepseek-v4-flash"

[appearance]
theme = "dracula"
"#,
    )
    .expect_err("unknown themes should fail config parsing");

    assert!(error.to_string().contains("unknown variant"));
    assert!(error.to_string().contains("dracula"));
}

#[test]
fn root_config_rejects_unknown_syntax_theme() {
    let error = toml::from_str::<RootConfig>(
        r#"
[agent]
provider = "deepseek"
model = "deepseek-v4-flash"

[appearance]
syntax_theme = "dracula"
"#,
    )
    .expect_err("unknown syntax themes should fail config parsing");

    assert!(error.to_string().contains("unknown variant"));
    assert!(error.to_string().contains("dracula"));
}

#[test]
fn root_config_serializes_appearance_theme_and_colors() {
    let mut colors = BTreeMap::new();
    colors.insert("surface_base".to_owned(), "#07080a".to_owned());
    colors.insert("text_primary".to_owned(), "#ecf0f6".to_owned());
    let config = RootConfig {
        workspace: WorkspaceConfig::default(),
        storage: Default::default(),
        session: Default::default(),
        agent: AgentConfig {
            provider: "deepseek".to_owned(),
            model: "deepseek-v4-flash".to_owned(),
            max_turns: None,
            tool_timeout_secs: 30,
        },
        model_request: Default::default(),
        permission: Default::default(),
        memory: Default::default(),
        skills: Default::default(),
        compaction: Default::default(),
        code_intelligence: Default::default(),
        terminal: Default::default(),
        execution: Default::default(),
        verification: Default::default(),
        appearance: crate::AppearanceConfig {
            theme: ThemeId::Nord,
            syntax_theme: SyntaxThemeId::Nord,
            usage_cost_currency: UsageCostCurrency::Usd,
            colors: crate::ThemeColorOverrides::new(colors),
        },
        task: Default::default(),
        providers: BTreeMap::new(),
        mcp_servers: Vec::new(),
    };

    let rendered = toml::to_string_pretty(&config).expect("appearance should serialize");

    assert!(rendered.contains("[appearance]"));
    assert!(rendered.contains("theme = \"nord\""));
    assert!(rendered.contains("syntax_theme = \"nord\""));
    assert!(rendered.contains("usage_cost_currency = \"usd\""));
    assert!(rendered.contains("[appearance.colors]"));
    assert!(rendered.contains("surface_base = \"#07080a\""));
    assert!(rendered.contains("text_primary = \"#ecf0f6\""));
}

#[test]
fn task_config_loads_role_overrides() {
    let raw = r#"
[agent]
provider = "deepseek"
model = "deepseek-v4-pro"

[task]
default_mode = "plan"
max_plan_steps = 8
multi_agent_mode = "proactive"

[task.planner]
model = "deepseek-reasoner"
reasoning_effort = "max"

[task.planner.tools]
names = ["read_file", "grep"]
prefixes = ["code_intel_"]
"#;

    let config: RootConfig = toml::from_str(raw).expect("task config should parse");

    assert_eq!(TaskConfig::default().default_mode, TaskMode::Chat);
    assert_eq!(config.task.default_mode, TaskMode::Plan);
    assert_eq!(config.task.multi_agent_mode, MultiAgentMode::Proactive);
    assert_eq!(config.task.max_plan_steps, 8);
    assert_eq!(
        config.task.planner.model.as_deref(),
        Some("deepseek-reasoner")
    );
    assert_eq!(config.task.planner.tools.names, vec!["read_file", "grep"]);
    assert_eq!(config.task.planner.tools.prefixes, vec!["code_intel_"]);
    assert_eq!(config.task.executor.provider, None);
}

#[test]
fn task_config_rejects_legacy_budget_fields() {
    for legacy_field in [
        "max_child_sessions",
        "allow_parallel_readonly_subagents",
        "max_parallel_readonly",
        "max_parallel_write",
        "max_background_threads",
        "max_spawn_fanout_per_turn",
        "max_agent_tokens_per_task",
    ] {
        let raw = format!(
            r#"
[agent]
provider = "deepseek"
model = "deepseek-v4-pro"

[task]
{legacy_field} = 1
"#
        );

        let error = toml::from_str::<RootConfig>(&raw)
            .expect_err("legacy task budget field should be rejected");
        assert!(
            error.to_string().contains(legacy_field),
            "expected error to mention {legacy_field}, got {error}"
        );
    }
}

#[test]
fn task_config_role_config_and_mode_labels_are_stable() {
    let mut config = TaskConfig::default();
    config.planner.model = Some("planner-model".to_owned());
    config.executor.model = Some("executor-model".to_owned());
    config.subagent_read.model = Some("subagent-read-model".to_owned());
    config.subagent_write.model = Some("subagent-write-model".to_owned());

    assert_eq!(
        config.role_config(AgentRole::Planner).model.as_deref(),
        Some("planner-model")
    );
    assert_eq!(
        config.role_config(AgentRole::Executor).model.as_deref(),
        Some("executor-model")
    );
    assert_eq!(
        config.role_config(AgentRole::SubagentRead).model.as_deref(),
        Some("subagent-read-model")
    );
    assert_eq!(
        config
            .role_config(AgentRole::SubagentWrite)
            .model
            .as_deref(),
        Some("subagent-write-model")
    );
    assert_eq!(TaskMode::Chat.as_str(), "chat");
    assert_eq!(TaskMode::Plan.as_str(), "plan");
    assert_eq!(MultiAgentMode::None.as_str(), "none");
    assert_eq!(
        MultiAgentMode::ExplicitRequestOnly.as_str(),
        "explicit_request_only"
    );
    assert_eq!(MultiAgentMode::Proactive.as_str(), "proactive");
}

#[test]
fn task_config_accepts_codex_multi_agent_mode_alias() {
    let raw = r#"
[agent]
provider = "deepseek"
model = "deepseek-v4-pro"

[task]
multi_agent_mode = "explicitRequestOnly"
"#;

    let config: RootConfig = toml::from_str(raw).expect("task config should parse alias");

    assert_eq!(
        config.task.multi_agent_mode,
        MultiAgentMode::ExplicitRequestOnly
    );
}

#[test]
fn root_config_loads_terminal_config() {
    let config: RootConfig = toml::from_str(
        r#"
[agent]
provider = "deepseek"
model = "deepseek-v4-flash"

[terminal]
keyboard_enhancement = "on"
mouse_capture = false
osc52_clipboard = false
scroll_sensitivity = 5
"#,
    )
    .expect("terminal config should parse");

    assert_eq!(
        config.terminal.keyboard_enhancement,
        TerminalKeyboardEnhancement::On
    );
    assert!(!config.terminal.mouse_capture);
    assert!(!config.terminal.osc52_clipboard);
    assert_eq!(config.terminal.scroll_sensitivity, 5);
}

#[test]
fn root_config_defaults_terminal_keyboard_enhancement_to_auto() {
    let config: RootConfig = toml::from_str(
        r#"
[agent]
provider = "deepseek"
model = "deepseek-v4-flash"
"#,
    )
    .expect("minimal config should parse");

    assert_eq!(
        config.terminal.keyboard_enhancement,
        TerminalKeyboardEnhancement::Auto
    );
}

#[test]
fn root_config_rejects_bool_terminal_keyboard_enhancement() {
    let error = toml::from_str::<RootConfig>(
        r#"
[agent]
provider = "deepseek"
model = "deepseek-v4-flash"

[terminal]
keyboard_enhancement = true
"#,
    )
    .expect_err("bool keyboard enhancement should be rejected");
    assert!(error.to_string().contains("keyboard_enhancement"));

    let error = toml::from_str::<RootConfig>(
        r#"
[agent]
provider = "deepseek"
model = "deepseek-v4-flash"

[terminal]
keyboard_enhancement = false
"#,
    )
    .expect_err("bool keyboard enhancement should be rejected");
    assert!(error.to_string().contains("keyboard_enhancement"));
}

#[test]
fn mcp_server_config_loads_lifecycle_and_trust_policy() {
    let raw = r#"
[agent]
provider = "deepseek"
model = "deepseek-v4-flash"

[[mcp_servers]]
name = "required-filesystem"
command = "node"
args = ["/srv/filesystem.js"]
startup_timeout_secs = 7

[[mcp_servers]]
name = "optional-third-party"
command = "uvx"
args = ["third-party-mcp"]
startup_timeout_secs = 3
required = false
startup = "lazy"

[mcp_servers.trust]
trust_class = "third_party"
approval_default = "ask"
egress_logging = true
allow_secrets = false
pin_version = true

[mcp_servers.trust.pinned]
command_fingerprint = "sha256:abc"
protocol_version = "2024-11-05"
server_name = "third-party"
server_version = "1.2.3"
"#;

    let config: RootConfig = toml::from_str(raw).expect("mcp config should parse");

    assert_eq!(config.agent.max_turns, None);

    let required = &config.mcp_servers[0];
    assert!(required.required);
    assert_eq!(required.startup, McpServerStartup::Eager);
    assert_eq!(required.trust.trust_class, McpTrustClass::SelfHosted);
    assert_eq!(required.trust.approval_default, ApprovalMode::Ask);
    assert!(required.trust.egress_logging);
    assert!(!required.trust.allow_secrets);
    assert!(!required.trust.pin_version);

    let optional = &config.mcp_servers[1];
    assert!(!optional.required);
    assert_eq!(optional.startup, McpServerStartup::Lazy);
    assert_eq!(optional.trust.trust_class, McpTrustClass::ThirdParty);
    assert_eq!(optional.trust.approval_default, ApprovalMode::Ask);
    assert!(optional.trust.egress_logging);
    assert!(!optional.trust.allow_secrets);
    assert!(optional.trust.pin_version);
    let pinned = optional
        .trust
        .pinned
        .as_ref()
        .expect("pinned identity should parse");
    assert_eq!(pinned.command_fingerprint, "sha256:abc");
    assert_eq!(pinned.protocol_version, "2024-11-05");
    assert_eq!(pinned.server_name, "third-party");
    assert_eq!(pinned.server_version, "1.2.3");
}

#[test]
fn root_config_loads_code_intelligence_config() {
    let raw = r#"
[agent]
provider = "deepseek"
model = "deepseek-v4-flash"

	[code_intelligence]
	enabled = true
	server_startup = "lazy"
default_timeout_ms = 2500
max_results = 50
max_payload_bytes = 32768

[[code_intelligence.servers]]
name = "rust-analyzer"
languages = ["rust"]
command = "rust-analyzer"
root_markers = ["Cargo.toml"]
file_extensions = ["rs"]
trust_required = true

[code_intelligence.servers.initialization_options]
check = { command = "check" }
"#;

    let config: RootConfig = toml::from_str(raw).expect("code intelligence config should parse");

    assert!(config.code_intelligence.enabled);
    assert_eq!(
        config.code_intelligence.server_startup,
        CodeIntelStartup::Lazy
    );
    assert_eq!(config.code_intelligence.default_timeout_ms, 2500);
    assert!(config.code_intelligence.auto_discover);
    assert!(config.code_intelligence.report_missing);
    assert_eq!(config.code_intelligence.servers[0].name, "rust-analyzer");
    assert_eq!(
        config.code_intelligence.servers[0].initialization_options["check"]["command"],
        "check"
    );
}

#[test]
fn root_config_loads_code_intelligence_auto_discover_config() {
    let raw = r#"
[agent]
provider = "deepseek"
model = "deepseek-v4-flash"

	[code_intelligence]
	enabled = true
	server_startup = "lazy"
	auto_discover = false
	report_missing = false
	"#;

    let config: RootConfig =
        toml::from_str(raw).expect("code intelligence auto discover config should parse");

    assert!(config.code_intelligence.enabled);
    assert!(!config.code_intelligence.auto_discover);
    assert!(!config.code_intelligence.report_missing);

    let rendered = toml::to_string_pretty(&config).expect("config should serialize");
    assert!(!rendered.contains("[code_intelligence.discovery]"));
    assert!(rendered.contains("auto_discover = false"));
    assert!(rendered.contains("report_missing = false"));
}

#[test]
fn root_config_rejects_legacy_code_intelligence_keys() {
    assert_root_config_rejects(
        r#"
	[agent]
	provider = "deepseek"
	model = "deepseek-v4-flash"

	[code_intelligence]
	feature = true
	"#,
        "feature",
    );

    assert_root_config_rejects(
        r#"
	[agent]
	provider = "deepseek"
	model = "deepseek-v4-flash"

	[code_intelligence]
	enabled = true
	startup = "lazy"
	"#,
        "startup",
    );

    assert_root_config_rejects(
        r#"
	[agent]
	provider = "deepseek"
	model = "deepseek-v4-flash"

	[code_intelligence]
	enabled = true

	[code_intelligence.discovery]
	enabled = true
	"#,
        "discovery",
    );
}

#[test]
fn root_config_loads_verification_config_and_defaults_empty() {
    let default_raw = r#"
[agent]
provider = "deepseek"
model = "deepseek-v4-flash"
"#;
    let default_config: RootConfig =
        toml::from_str(default_raw).expect("default verification config should parse");
    assert!(default_config.verification.checks.is_empty());
    assert_eq!(
        default_config.verification.auto_run,
        crate::VerificationAutoRunPolicy::Manual
    );
    assert_eq!(
        default_config.verification.scope.profile,
        crate::VerificationScopeProfile::Auto
    );
    assert!(default_config.verification.scope.extra_excludes.is_empty());
    assert!(default_config.verification.scope.generated_roots.is_empty());

    let raw = r#"
[agent]
provider = "deepseek"
model = "deepseek-v4-flash"

	[verification]
	auto_run = "trusted_only"

	[verification.scope]
	profile = "node"
	extra_excludes = ["tmp/generated/**"]
	generated_roots = ["generated"]

	[[verification.checks]]
id = "cargo-test"
command = "cargo"
args = ["test", "-p", "sigil-kernel"]
cwd = "."
effect = "read_only"
"#;

    let config: RootConfig = toml::from_str(raw).expect("verification config should parse");

    assert_eq!(
        config.verification.auto_run,
        crate::VerificationAutoRunPolicy::TrustedOnly
    );
    assert_eq!(
        config.verification.scope.profile,
        crate::VerificationScopeProfile::Node
    );
    assert_eq!(
        config.verification.scope.extra_excludes,
        vec!["tmp/generated/**".to_owned()]
    );
    assert_eq!(
        config.verification.scope.generated_roots,
        vec![std::path::PathBuf::from("generated")]
    );
    let scope = config.verification.scope_for_hash("scope-main");
    assert!(scope.exclude.contains(&".next/**".to_owned()));
    assert!(scope.exclude.contains(&"tmp/generated/**".to_owned()));
    assert!(
        scope
            .generated_roots
            .contains(&std::path::PathBuf::from("generated"))
    );
    assert_eq!(config.verification.checks.len(), 1);
    assert_eq!(config.verification.checks[0].id, "cargo-test");
    assert_eq!(config.verification.checks[0].command, "cargo");
    assert_eq!(
        config.verification.checks[0].args,
        vec![
            "test".to_owned(),
            "-p".to_owned(),
            "sigil-kernel".to_owned()
        ]
    );
    assert_eq!(config.verification.checks[0].effect.as_str(), "read_only");
}

#[test]
fn root_config_rejects_legacy_verification_scope_keys() {
    assert_root_config_rejects(
        r#"
	[agent]
	provider = "deepseek"
	model = "deepseek-v4-flash"

	[verification]
	scope_profile = "node"
	"#,
        "scope_profile",
    );

    assert_root_config_rejects(
        r#"
	[agent]
	provider = "deepseek"
	model = "deepseek-v4-flash"

	[verification]
	extra_scope_excludes = ["tmp/generated/**"]
	"#,
        "extra_scope_excludes",
    );
}

#[test]
fn resolve_workspace_root_uses_launch_cwd_for_default_dot() {
    let config_path = Path::new("/Users/example/.config/sigil/sigil.toml");
    let cwd = Path::new("/Users/example/work/project");

    assert_eq!(resolve_workspace_root(config_path, cwd, "."), cwd);
    assert_eq!(
        resolve_workspace_root(config_path, cwd, "nested/workspace"),
        Path::new("/Users/example/.config/sigil/nested/workspace")
    );
}

#[test]
fn compaction_threshold_status_handles_disabled_and_missing_window() {
    let disabled = CompactionConfig {
        enabled: false,
        ..CompactionConfig::default()
    };
    let missing_window = CompactionConfig {
        enabled: true,
        context_window_tokens: None,
        ..CompactionConfig::default()
    };
    let zero_window = CompactionConfig {
        enabled: true,
        context_window_tokens: Some(0),
        ..CompactionConfig::default()
    };

    assert_eq!(
        disabled.threshold_status(100),
        CompactionThresholdStatus::Off
    );
    assert_eq!(
        missing_window.threshold_status(100),
        CompactionThresholdStatus::NotAvailable
    );
    assert_eq!(
        zero_window.threshold_status(100),
        CompactionThresholdStatus::NotAvailable
    );
}

#[test]
fn root_config_defaults_code_intelligence_and_memory() {
    let config: RootConfig = toml::from_str(
        r#"
[agent]
provider = "deepseek"
model = "deepseek-v4-flash"
"#,
    )
    .expect("minimal config should parse");

    assert!(config.memory.enabled);
    assert!(!config.code_intelligence.enabled);
    assert_eq!(
        config.code_intelligence.server_startup,
        CodeIntelStartup::Lazy
    );
    assert_eq!(config.code_intelligence.default_timeout_ms, 5_000);
    assert_eq!(config.code_intelligence.max_results, 100);
    assert_eq!(config.code_intelligence.max_payload_bytes, 64 * 1024);
    assert!(config.code_intelligence.auto_discover);
    assert!(config.code_intelligence.report_missing);
    assert_eq!(config.execution.backend(), ExecutionBackendKind::Local);
    assert_eq!(
        config.execution.isolation(),
        ExecutionIsolationPolicy::AllowLocal
    );
    assert_eq!(
        config.execution.profile(),
        ExecutionSandboxProfile::Unconfined
    );
    assert_eq!(config.execution.fallback(), ExecutionSandboxFallback::Deny);
    assert_eq!(config.execution.container_image(), None);
}

#[test]
fn root_config_loads_execution_config() {
    let config: RootConfig = toml::from_str(
        r#"
[agent]
provider = "deepseek"
model = "deepseek-v4-flash"

	[execution]
	strategy = "sandbox"

	[execution.sandbox]
	backend = "macos_seatbelt"
	profile = "build_offline"
	fallback = "prompt"
	"#,
    )
    .expect("execution config should parse");

    assert_eq!(
        config.execution.backend(),
        ExecutionBackendKind::MacosSeatbelt
    );
    assert_eq!(
        config.execution.isolation(),
        ExecutionIsolationPolicy::RequireSandbox
    );
    assert_eq!(
        config.execution.profile(),
        ExecutionSandboxProfile::BuildOffline
    );
    assert_eq!(
        config.execution.fallback(),
        ExecutionSandboxFallback::Prompt
    );
}

#[test]
fn root_config_loads_macos_seatbelt_execution_backend() {
    let config: RootConfig = toml::from_str(
        r#"
[agent]
provider = "deepseek"
model = "deepseek-v4-flash"

	[execution]
	strategy = "sandbox"

	[execution.sandbox]
	backend = "macos_seatbelt"
	"#,
    )
    .expect("macos seatbelt execution config should parse");

    assert_eq!(
        config.execution.backend(),
        ExecutionBackendKind::MacosSeatbelt
    );
    assert_eq!(
        config.execution.isolation(),
        ExecutionIsolationPolicy::RequireSandbox
    );
    assert_eq!(
        config.execution.profile(),
        ExecutionSandboxProfile::WorkspaceWrite
    );
}

#[test]
fn root_config_loads_linux_bubblewrap_execution_backend() {
    let config: RootConfig = toml::from_str(
        r#"
[agent]
provider = "deepseek"
model = "deepseek-v4-flash"

	[execution]
	strategy = "sandbox"

	[execution.sandbox]
	backend = "linux_bubblewrap"
	profile = "build_offline"
	"#,
    )
    .expect("linux bubblewrap execution config should parse");

    assert_eq!(
        config.execution.backend(),
        ExecutionBackendKind::LinuxBubblewrap
    );
    assert_eq!(
        config.execution.isolation(),
        ExecutionIsolationPolicy::RequireSandbox
    );
    assert_eq!(
        config.execution.profile(),
        ExecutionSandboxProfile::BuildOffline
    );
}

#[test]
fn root_config_loads_docker_execution_backend() {
    let config: RootConfig = toml::from_str(
        r#"
[agent]
provider = "deepseek"
model = "deepseek-v4-flash"

	[execution]
	strategy = "sandbox"

	[execution.sandbox]
	backend = "docker"
	profile = "build_networked"
	container_image = "rust:1.94.1"
	"#,
    )
    .expect("docker execution config should parse");

    assert_eq!(config.execution.backend(), ExecutionBackendKind::Docker);
    assert_eq!(
        config.execution.isolation(),
        ExecutionIsolationPolicy::RequireSandbox
    );
    assert_eq!(
        config.execution.profile(),
        ExecutionSandboxProfile::BuildNetworked
    );
    assert_eq!(config.execution.container_image(), Some("rust:1.94.1"));
}

#[test]
fn root_config_rejects_legacy_and_illegal_execution_strategy_config() {
    assert_root_config_rejects(
        r#"
	[agent]
	provider = "deepseek"
	model = "deepseek-v4-flash"

	[execution]
	backend = "docker"
	isolation = "require_sandbox"
	"#,
        "backend",
    );

    assert_root_config_rejects(
        r#"
	[agent]
	provider = "deepseek"
	model = "deepseek-v4-flash"

	[execution]
	strategy = "sandbox"
	"#,
        "[execution.sandbox]",
    );

    assert_root_config_rejects(
        r#"
	[agent]
	provider = "deepseek"
	model = "deepseek-v4-flash"

	[execution]
	strategy = "local"

	[execution.sandbox]
	backend = "macos_seatbelt"
	"#,
        "only valid when execution.strategy is \"sandbox\"",
    );

    assert_root_config_rejects(
        r#"
	[agent]
	provider = "deepseek"
	model = "deepseek-v4-flash"

	[execution]
	strategy = "sandbox"

	[execution.sandbox]
	backend = "local"
	profile = "workspace_write"
	"#,
        "cannot use execution.sandbox.backend \"local\"",
    );

    assert_root_config_rejects(
        r#"
	[agent]
	provider = "deepseek"
	model = "deepseek-v4-flash"

	[execution]
	strategy = "sandbox"

	[execution.sandbox]
	backend = "docker"
	profile = "workspace_write"
	"#,
        "requires execution.sandbox.container_image",
    );

    assert_root_config_rejects(
        r#"
	[agent]
	provider = "deepseek"
	model = "deepseek-v4-flash"

	[execution]
	strategy = "sandbox"

	[execution.sandbox]
	backend = "macos_seatbelt"
	profile = "workspace_write"
	container_image = "rust:1.94.1"
	"#,
        "container_image is only valid for docker",
    );
}

#[test]
fn execution_config_profiles_validate_backend_capabilities() {
    let local_capabilities = ExecutionBackendCapabilities::default();
    let sandbox_capabilities = ExecutionBackendCapabilities {
        filesystem_isolation: true,
        process_isolation: true,
        ..ExecutionBackendCapabilities::default()
    };
    let offline_capabilities = ExecutionBackendCapabilities {
        filesystem_isolation: true,
        process_isolation: true,
        network_isolation: true,
        ..ExecutionBackendCapabilities::default()
    };

    let workspace_write = execution_sandbox_config(ExecutionSandboxProfile::WorkspaceWrite);
    assert!(
        workspace_write
            .validate_profile_capabilities(local_capabilities)
            .expect_err("workspace_write requires sandbox")
            .contains("filesystem and process isolation")
    );
    workspace_write
        .validate_profile_capabilities(sandbox_capabilities)
        .expect("workspace_write accepts basic sandbox capabilities");

    let build_offline = execution_sandbox_config(ExecutionSandboxProfile::BuildOffline);
    assert!(
        build_offline
            .validate_profile_capabilities(sandbox_capabilities)
            .expect_err("build_offline requires network isolation")
            .contains("network isolation")
    );
    build_offline
        .validate_profile_capabilities(offline_capabilities)
        .expect("build_offline accepts network-isolating sandbox");

    let build_networked = execution_sandbox_config(ExecutionSandboxProfile::BuildNetworked);
    assert!(build_networked.profile_spec().network_allowed);
    assert!(build_networked.profile_spec().dependency_caches_read_only);
    build_networked
        .validate_profile_capabilities(sandbox_capabilities)
        .expect("build_networked does not require network isolation");
}

#[test]
fn execution_capability_requirements_report_missing_capabilities() {
    let build_offline = execution_sandbox_config(ExecutionSandboxProfile::BuildOffline);
    let requirements = build_offline.required_capabilities();

    assert!(requirements.filesystem_isolation);
    assert!(requirements.network_isolation);
    assert!(requirements.process_isolation);

    let missing = ExecutionBackendCapabilities::default().missing_requirements(requirements);
    assert_eq!(
        missing,
        vec![
            ExecutionCapability::FilesystemIsolation,
            ExecutionCapability::NetworkIsolation,
            ExecutionCapability::ProcessIsolation
        ]
    );
}

fn execution_sandbox_config(profile: ExecutionSandboxProfile) -> crate::ExecutionConfig {
    let mut sandbox =
        crate::ExecutionSandboxStrategyConfig::new(ExecutionBackendKind::MacosSeatbelt);
    sandbox.profile = profile;
    crate::ExecutionConfig::sandbox(sandbox)
}

#[test]
fn preferred_config_path_falls_back_to_user_config_path() {
    let temp = tempfile::tempdir().expect("tempdir should build");

    assert_eq!(
        preferred_config_path(None, temp.path()).expect("user config path should resolve"),
        default_user_config_path().expect("default user config path should resolve")
    );
}

#[test]
fn resolve_workspace_root_handles_blank_and_absolute_paths() {
    let config_path = Path::new("/Users/example/.config/sigil/sigil.toml");
    let cwd = Path::new("/Users/example/work/project");

    assert_eq!(resolve_workspace_root(config_path, cwd, "   "), cwd);
    assert_eq!(
        resolve_workspace_root(config_path, cwd, "/tmp/absolute-workspace"),
        Path::new("/tmp/absolute-workspace")
    );
    assert_eq!(
        resolve_workspace_root(config_path, cwd, "/tmp/explicit"),
        Path::new("/tmp/explicit")
    );
}

#[test]
fn config_default_user_paths_and_preferred_fallback_are_stable() {
    let config_dir = default_user_config_dir().expect("user config dir should resolve");
    let config_path = default_user_config_path().expect("user config path should resolve");
    assert_eq!(config_path, config_dir.join("sigil.toml"));

    assert!(config_dir.ends_with(".sigil"));

    let temp = tempfile::tempdir().expect("tempdir should build");
    assert_eq!(
        preferred_config_path(None, temp.path()).expect("fallback config should resolve"),
        config_path
    );
}

#[test]
fn config_labels_and_defaults_are_stable() {
    assert_eq!(CodeIntelStartup::Off.as_str(), "off");
    assert_eq!(CodeIntelStartup::Lazy.as_str(), "lazy");
    assert_eq!(CodeIntelStartup::Eager.as_str(), "eager");

    assert_eq!(CompactionThresholdStatus::Off.as_str(), "off");
    assert_eq!(CompactionThresholdStatus::NotAvailable.as_str(), "n/a");
    assert_eq!(CompactionThresholdStatus::Ready.as_str(), "ready");
    assert_eq!(CompactionThresholdStatus::Soft.as_str(), "soft");
    assert_eq!(CompactionThresholdStatus::Hard.as_str(), "hard");

    assert_eq!(McpServerStartup::Eager.as_str(), "eager");
    assert_eq!(McpServerStartup::Lazy.as_str(), "lazy");

    assert_eq!(McpTrustClass::Official.as_str(), "official");
    assert_eq!(McpTrustClass::SelfHosted.as_str(), "self_hosted");
    assert_eq!(McpTrustClass::ThirdParty.as_str(), "third_party");

    let server = McpServerConfig::default();
    assert_eq!(server.startup_timeout_secs, 10);
    assert!(server.required);
    assert_eq!(server.startup, McpServerStartup::Eager);
    assert_eq!(server.trust.trust_class, McpTrustClass::SelfHosted);
    assert_eq!(server.trust.approval_default, ApprovalMode::Ask);
    assert!(server.trust.egress_logging);
    assert!(!server.trust.allow_secrets);
    assert!(!server.trust.pin_version);
}

#[test]
fn config_compaction_threshold_status_handles_off_and_missing_windows() {
    let disabled = CompactionConfig {
        enabled: false,
        ..CompactionConfig::default()
    };
    assert_eq!(
        disabled.threshold_status(10),
        CompactionThresholdStatus::Off
    );

    let unavailable = CompactionConfig {
        enabled: true,
        context_window_tokens: None,
        ..CompactionConfig::default()
    };
    assert_eq!(
        unavailable.threshold_status(10),
        CompactionThresholdStatus::NotAvailable
    );

    let zero_window = CompactionConfig {
        enabled: true,
        context_window_tokens: Some(0),
        ..CompactionConfig::default()
    };
    assert_eq!(
        zero_window.threshold_status(10),
        CompactionThresholdStatus::NotAvailable
    );
}

#[test]
fn resolve_workspace_root_uses_launch_cwd_for_empty_and_absolute_paths() {
    let config_path = Path::new("/Users/example/.config/sigil/sigil.toml");
    let cwd = Path::new("/Users/example/work/project");
    let absolute = Path::new("/tmp/sigil-workspace");

    assert_eq!(resolve_workspace_root(config_path, cwd, "  "), cwd);
    assert_eq!(
        resolve_workspace_root(config_path, cwd, absolute.to_str().expect("utf8 path")),
        absolute
    );
}

#[test]
fn root_config_load_reports_missing_paths_with_context() {
    let missing = Path::new("/tmp").join(format!(
        "sigil-config-missing-{}.toml",
        uuid::Uuid::new_v4()
    ));

    let error = RootConfig::load(&missing).expect_err("missing config should fail");

    assert!(error.to_string().contains("failed to read config at"));
    assert!(
        error.to_string().contains(
            missing
                .file_name()
                .and_then(|name| name.to_str())
                .expect("file name should be utf-8")
        )
    );
}

#[test]
fn root_config_save_reports_parent_creation_and_write_errors() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    let config = RootConfig {
        workspace: WorkspaceConfig {
            root: "/tmp/workspace".to_owned(),
        },
        storage: Default::default(),
        session: Default::default(),
        agent: AgentConfig {
            provider: "deepseek".to_owned(),
            model: "deepseek-v4-flash".to_owned(),
            max_turns: Some(32),
            tool_timeout_secs: 30,
        },
        model_request: Default::default(),
        permission: Default::default(),
        memory: Default::default(),
        skills: Default::default(),
        compaction: Default::default(),
        code_intelligence: Default::default(),
        terminal: Default::default(),
        execution: Default::default(),
        verification: Default::default(),
        appearance: Default::default(),
        task: Default::default(),
        providers: BTreeMap::new(),
        mcp_servers: Vec::new(),
    };

    let blocking_parent = temp.path().join("blocking-parent");
    std::fs::write(&blocking_parent, "file").expect("blocking parent should write");
    let create_error = config
        .save(&blocking_parent.join("sigil.toml"))
        .expect_err("file parent should fail directory creation");
    assert!(create_error.to_string().contains("failed to create"));

    let output_dir = temp.path().join("output-dir");
    std::fs::create_dir(&output_dir).expect("output dir should create");
    let write_error = config
        .save(&output_dir)
        .expect_err("writing to a directory should fail");
    assert!(
        write_error
            .to_string()
            .contains("failed to write config at")
    );
}

#[test]
fn language_server_config_defaults_trust_and_timeout() {
    let config: RootConfig = toml::from_str(
        r#"
[agent]
provider = "deepseek"
model = "deepseek-v4-flash"

[[code_intelligence.servers]]
name = "rust-analyzer"
command = "rust-analyzer"
"#,
    )
    .expect("minimal language server config should parse");

    assert!(config.code_intelligence.servers[0].trust_required);
    assert_eq!(
        config.code_intelligence.servers[0].startup_timeout_ms,
        10_000
    );
}

#[test]
fn resolve_workspace_root_uses_current_directory_when_config_has_no_parent() {
    let cwd = Path::new("/Users/example/work/project");

    assert_eq!(
        resolve_workspace_root(Path::new("sigil.toml"), cwd, "nested/workspace"),
        Path::new("nested/workspace")
    );
}
