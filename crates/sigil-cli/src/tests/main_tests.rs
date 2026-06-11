use anyhow::Result;

use super::{default_session_path, resolve_workspace_root};

#[test]
fn resolve_workspace_root_uses_config_parent() -> Result<()> {
    let config_path = std::env::temp_dir()
        .join("sigil-cli-config-parent")
        .join("sigil.toml");
    let launch_cwd = std::env::temp_dir().join("sigil-cli-launch");
    let resolved = resolve_workspace_root(&config_path, &launch_cwd, "workspace/project");

    assert_eq!(
        resolved,
        config_path
            .parent()
            .expect("config path should have a parent")
            .join("workspace/project")
    );
    Ok(())
}

#[test]
fn resolve_workspace_root_uses_launch_cwd_for_default_dot() {
    let config_path = std::env::temp_dir()
        .join("sigil-cli-config-parent")
        .join("sigil.toml");
    let launch_cwd = std::env::temp_dir().join("sigil-cli-launch");

    let resolved = resolve_workspace_root(&config_path, &launch_cwd, ".");

    assert_eq!(resolved, launch_cwd);
}

#[test]
fn default_session_path_uses_configured_log_dir_and_jsonl_suffix() {
    let workspace_root = std::env::temp_dir().join("sigil-cli-workspace");
    let session_path = default_session_path(&workspace_root, ".sigil/sessions");

    assert!(session_path.starts_with(workspace_root.join(".sigil/sessions")));
    assert_eq!(
        session_path.extension().and_then(|ext| ext.to_str()),
        Some("jsonl")
    );
    assert!(
        session_path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("session-"))
    );
}
