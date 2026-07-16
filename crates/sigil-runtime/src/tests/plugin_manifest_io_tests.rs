use std::{fs, os::unix::fs::symlink};

use super::*;

#[test]
fn bounded_manifest_read_rejects_terminal_symlink() {
    let workspace = tempfile::tempdir().expect("workspace should create");
    let target = workspace.path().join("target.toml");
    let link = workspace.path().join("plugin.toml");
    fs::write(&target, "id = \"fixture\"\n").expect("target should write");
    symlink(&target, &link).expect("symlink should create");

    assert_eq!(
        read_bounded_plugin_manifest(&link),
        Err(BoundedPluginManifestReadError::Unavailable)
    );
}
