use std::{collections::BTreeMap, path::Path};

use super::{
    CodeIntelStartup, CompactionConfig, CompactionThresholdStatus, McpServerStartup, McpTrustClass,
    RootConfig, preferred_config_path, resolve_workspace_root,
};
use crate::{AgentConfig, ApprovalMode, WorkspaceConfig};

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
fn preferred_config_path_uses_explicit_or_local_file() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    let explicit = temp.path().join("explicit.toml");
    assert_eq!(
        preferred_config_path(Some(&explicit), temp.path()).expect("explicit path should win"),
        explicit
    );

    let local = temp.path().join("termquill.toml");
    std::fs::write(&local, "").expect("local config should write");
    assert_eq!(
        preferred_config_path(None, temp.path()).expect("local path should win"),
        local
    );
}

#[test]
fn root_config_save_roundtrips() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    let path = temp.path().join("nested").join("termquill.toml");
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
        compaction: Default::default(),
        code_intelligence: Default::default(),
        providers: BTreeMap::new(),
        mcp_servers: Vec::new(),
    };

    config.save(&path).expect("config should save");
    let loaded = RootConfig::load(&path).expect("config should reload");
    assert_eq!(loaded.workspace.root, "/tmp/workspace");
    assert_eq!(loaded.agent.provider, "deepseek");
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
    let config_path = Path::new("/Users/example/.config/termquill/termquill.toml");
    let cwd = Path::new("/Users/example/work/project");

    assert_eq!(resolve_workspace_root(config_path, cwd, "."), cwd);
    assert_eq!(
        resolve_workspace_root(config_path, cwd, "nested/workspace"),
        Path::new("/Users/example/.config/termquill/nested/workspace")
    );
}
