use std::{collections::BTreeMap, path::Path};

use super::{
    CodeIntelStartup, CompactionConfig, CompactionThresholdStatus, McpServerConfig,
    McpServerStartup, McpTrustClass, RootConfig, default_user_config_dir, default_user_config_path,
    preferred_config_path, resolve_workspace_root,
};
use crate::{
    AgentConfig, AgentRole, ApprovalMode, SkillConfig, TaskConfig, TaskMode, WorkspaceConfig,
};

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
fn compaction_window_loads_legacy_key_and_saves_fallback_key() {
    let raw = r#"
[agent]
provider = "deepseek"
model = "deepseek-v4-pro"

[compaction]
context_window_tokens = 128000
"#;

    let config: RootConfig = toml::from_str(raw).expect("legacy config should load");
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
fn skill_config_defaults_and_toml_overrides_are_stable() {
    let defaults = SkillConfig::default();
    assert!(defaults.enabled);
    assert_eq!(defaults.workspace_dir, ".sigil/skills");
    assert_eq!(defaults.workspace_agents_dir, ".sigil/agents");
    assert!(defaults.user_skills);
    assert!(defaults.user_agents);
    assert_eq!(defaults.compatibility_sources, vec!["claude"]);

    let raw = r#"
[agent]
provider = "deepseek"
model = "deepseek-v4-pro"

[skills]
enabled = false
workspace_dir = "skills"
workspace_agents_dir = "agents"
user_skills = false
user_agents = false
compatibility_sources = ["opencode"]
"#;

    let config: RootConfig = toml::from_str(raw).expect("skills config should load");
    assert!(!config.skills.enabled);
    assert_eq!(config.skills.workspace_dir, "skills");
    assert_eq!(config.skills.workspace_agents_dir, "agents");
    assert!(!config.skills.user_skills);
    assert!(!config.skills.user_agents);
    assert_eq!(config.skills.compatibility_sources, vec!["opencode"]);
}

#[test]
fn preferred_config_path_uses_explicit_or_local_file() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    let explicit = temp.path().join("explicit.toml");
    assert_eq!(
        preferred_config_path(Some(&explicit), temp.path()).expect("explicit path should win"),
        explicit
    );

    let local = temp.path().join("sigil.toml");
    std::fs::write(&local, "").expect("local config should write");
    assert_eq!(
        preferred_config_path(None, temp.path()).expect("local path should win"),
        local
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
        session: Default::default(),
        agent: AgentConfig {
            provider: "deepseek".to_owned(),
            model: "deepseek-v4-flash".to_owned(),
            max_turns: Some(32),
            tool_timeout_secs: 30,
        },
        permission: Default::default(),
        memory: Default::default(),
        skills: Default::default(),
        compaction: Default::default(),
        code_intelligence: Default::default(),
        terminal: Default::default(),
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
        session: Default::default(),
        agent: AgentConfig {
            provider: "deepseek".to_owned(),
            model: "deepseek-v4-flash".to_owned(),
            max_turns: None,
            tool_timeout_secs: 30,
        },
        permission: Default::default(),
        memory: Default::default(),
        skills: Default::default(),
        compaction: Default::default(),
        code_intelligence: Default::default(),
        terminal: Default::default(),
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
    assert_eq!(config.session.log_dir, ".sigil/sessions");
    assert_eq!(config.agent.tool_timeout_secs, 30);
    assert_eq!(config.memory, Default::default());
    assert_eq!(config.compaction.tail_messages, 6);
    assert_eq!(config.terminal, Default::default());
    assert_eq!(config.task.default_mode, TaskMode::Chat);
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
}

#[test]
fn root_config_loads_terminal_config() {
    let config: RootConfig = toml::from_str(
        r#"
[agent]
provider = "deepseek"
model = "deepseek-v4-flash"

[terminal]
mouse_capture = false
osc52_clipboard = false
scroll_sensitivity = 5
"#,
    )
    .expect("terminal config should parse");

    assert!(!config.terminal.mouse_capture);
    assert!(!config.terminal.osc52_clipboard);
    assert_eq!(config.terminal.scroll_sensitivity, 5);
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
startup = "lazy"
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
    assert_eq!(config.code_intelligence.startup, CodeIntelStartup::Lazy);
    assert_eq!(config.code_intelligence.default_timeout_ms, 2500);
    assert!(config.code_intelligence.discovery.enabled);
    assert!(config.code_intelligence.discovery.report_missing);
    assert_eq!(config.code_intelligence.servers[0].name, "rust-analyzer");
    assert_eq!(
        config.code_intelligence.servers[0].initialization_options["check"]["command"],
        "check"
    );
}

#[test]
fn root_config_loads_code_intelligence_discovery_config() {
    let raw = r#"
[agent]
provider = "deepseek"
model = "deepseek-v4-flash"

[code_intelligence]
enabled = true
startup = "lazy"

[code_intelligence.discovery]
enabled = false
report_missing = false
"#;

    let config: RootConfig =
        toml::from_str(raw).expect("code intelligence discovery config should parse");

    assert!(config.code_intelligence.enabled);
    assert!(!config.code_intelligence.discovery.enabled);
    assert!(!config.code_intelligence.discovery.report_missing);

    let rendered = toml::to_string_pretty(&config).expect("config should serialize");
    assert!(rendered.contains("[code_intelligence.discovery]"));
    assert!(rendered.contains("enabled = false"));
    assert!(rendered.contains("report_missing = false"));
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
    assert_eq!(config.code_intelligence.startup, CodeIntelStartup::Lazy);
    assert_eq!(config.code_intelligence.default_timeout_ms, 5_000);
    assert_eq!(config.code_intelligence.max_results, 100);
    assert_eq!(config.code_intelligence.max_payload_bytes, 64 * 1024);
    assert!(config.code_intelligence.discovery.enabled);
    assert!(config.code_intelligence.discovery.report_missing);
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

    if cfg!(target_os = "macos") {
        assert!(config_dir.ends_with("Library/Application Support/sigil"));
    } else if cfg!(target_os = "windows") {
        assert!(config_dir.ends_with("sigil"));
    } else {
        assert!(config_dir.ends_with(".config/sigil"));
    }

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
        session: Default::default(),
        agent: AgentConfig {
            provider: "deepseek".to_owned(),
            model: "deepseek-v4-flash".to_owned(),
            max_turns: Some(32),
            tool_timeout_secs: 30,
        },
        permission: Default::default(),
        memory: Default::default(),
        skills: Default::default(),
        compaction: Default::default(),
        code_intelligence: Default::default(),
        terminal: Default::default(),
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
