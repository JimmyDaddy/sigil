use std::{collections::BTreeMap, fs};

use termquill_kernel::CodeIntelligenceConfig;

use super::*;

#[test]
fn effective_servers_defaults_to_rust_analyzer() {
    let servers = effective_servers(&CodeIntelligenceConfig::default());

    assert_eq!(servers[0].name, "rust-analyzer");
    assert!(servers[0].file_extensions.contains(&"rs".to_owned()));
}

#[test]
fn resolve_workspace_file_rejects_paths_outside_workspace() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    let inside = temp.path().join("src.rs");
    let outside_dir = tempfile::tempdir().expect("outside tempdir should build");
    let outside = outside_dir.path().join("secret.rs");
    fs::write(&inside, "fn main() {}").expect("inside should write");
    fs::write(&outside, "fn secret() {}").expect("outside should write");

    assert!(resolve_workspace_file(temp.path(), "src.rs").is_ok());

    let error = resolve_workspace_file(temp.path(), outside.to_str().expect("utf8 path"))
        .expect_err("outside path should be rejected");
    assert!(error.to_string().contains("outside workspace"));
}

#[test]
fn safe_lsp_command_allows_pathless_command_and_blocks_escape() {
    let temp = tempfile::tempdir().expect("tempdir should build");

    assert_eq!(
        safe_lsp_command(temp.path(), "rust-analyzer").expect("pathless command should pass"),
        std::path::PathBuf::from("rust-analyzer")
    );
    assert!(safe_lsp_command(temp.path(), "../bin/lsp").is_err());
}

#[test]
fn sanitize_lsp_env_filters_secret_like_configured_values() {
    let env = sanitize_lsp_env(&BTreeMap::from([
        ("SAFE_FLAG".to_owned(), "1".to_owned()),
        ("TERMQUILL_API_KEY".to_owned(), "secret".to_owned()),
    ]));

    assert_eq!(env.get("SAFE_FLAG").map(String::as_str), Some("1"));
    assert!(!env.contains_key("TERMQUILL_API_KEY"));
}

#[test]
fn file_uri_roundtrips_paths_with_spaces() {
    let path = std::path::Path::new("/tmp/termquill space/src/main.rs");
    let uri = file_uri_from_path(path);

    assert_eq!(path_from_file_uri(&uri), Some(path.to_path_buf()));
}
