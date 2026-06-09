use std::{collections::BTreeMap, path::Path};

use super::{
    CompactionConfig, CompactionThresholdStatus, RootConfig, preferred_config_path,
    resolve_workspace_root,
};
use crate::{AgentConfig, WorkspaceConfig};

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
            max_turns: 8,
            tool_timeout_secs: 30,
        },
        permission: Default::default(),
        memory: Default::default(),
        compaction: Default::default(),
        providers: BTreeMap::new(),
        mcp_servers: Vec::new(),
    };

    config.save(&path).expect("config should save");
    let loaded = RootConfig::load(&path).expect("config should reload");
    assert_eq!(loaded.workspace.root, "/tmp/workspace");
    assert_eq!(loaded.agent.provider, "deepseek");
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
