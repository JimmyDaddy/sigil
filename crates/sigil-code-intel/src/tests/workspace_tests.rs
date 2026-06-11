use std::{collections::BTreeMap, fs};

use sigil_kernel::{CodeIntelligenceConfig, CodeIntelligenceDiscoveryConfig, LanguageServerConfig};

use crate::discovery::{DiscoveredLanguageServer, DiscoverySource, ServerAvailability};

use super::*;

#[test]
fn effective_servers_defaults_to_rust_analyzer_when_discovery_is_disabled() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    let config = CodeIntelligenceConfig {
        discovery: CodeIntelligenceDiscoveryConfig {
            enabled: false,
            report_missing: true,
        },
        ..CodeIntelligenceConfig::default()
    };
    let servers = effective_servers(&config, temp.path());

    assert_eq!(servers[0].name, "rust-analyzer");
    assert!(servers[0].file_extensions.contains(&"rs".to_owned()));
}

#[test]
fn effective_server_plan_keeps_missing_servers_status_only() {
    let config = CodeIntelligenceConfig::default();
    let rust = discovered_server(
        default_rust_analyzer_server(),
        ServerAvailability::Installed,
    );
    let typescript = discovered_server(
        test_server(
            "typescript-language-server",
            &["typescript", "javascript"],
            &["ts", "tsx", "js"],
        ),
        ServerAvailability::Missing,
    );

    let plan = effective_server_plan_from_discovered(&config, vec![rust, typescript]);

    assert_eq!(plan.servers.len(), 1);
    assert_eq!(plan.servers[0].name, "rust-analyzer");
    assert_eq!(
        plan.statuses
            .iter()
            .find(|status| status.server == "rust-analyzer")
            .map(|status| status.status.as_str()),
        Some("installed")
    );
    assert_eq!(
        plan.statuses
            .iter()
            .find(|status| status.server == "typescript-language-server")
            .map(|status| status.status.as_str()),
        Some("missing")
    );
}

#[test]
fn configured_servers_override_discovered_profile_by_name() {
    let configured = LanguageServerConfig {
        command: "custom-ts-lsp".to_owned(),
        startup_timeout_ms: 123,
        ..test_server(
            "typescript-language-server",
            &["typescript", "javascript"],
            &["ts", "tsx", "js"],
        )
    };
    let config = CodeIntelligenceConfig {
        servers: vec![configured.clone()],
        ..CodeIntelligenceConfig::default()
    };
    let discovered = discovered_server(
        test_server(
            "typescript-language-server",
            &["typescript", "javascript"],
            &["ts", "tsx", "js"],
        ),
        ServerAvailability::Missing,
    );

    let plan = effective_server_plan_from_discovered(&config, vec![discovered]);

    assert_eq!(plan.servers.len(), 1);
    assert_eq!(plan.servers[0].command, "custom-ts-lsp");
    assert_eq!(plan.servers[0].startup_timeout_ms, 123);
    assert_eq!(
        plan.statuses
            .iter()
            .find(|status| status.server == "typescript-language-server")
            .map(|status| status.status.as_str()),
        Some("configured")
    );
}

fn discovered_server(
    config: LanguageServerConfig,
    availability: ServerAvailability,
) -> DiscoveredLanguageServer {
    DiscoveredLanguageServer {
        config,
        source: DiscoverySource::BuiltIn,
        availability,
        install_hint: None,
    }
}

fn test_server(name: &str, languages: &[&str], file_extensions: &[&str]) -> LanguageServerConfig {
    LanguageServerConfig {
        name: name.to_owned(),
        languages: languages
            .iter()
            .map(|language| (*language).to_owned())
            .collect(),
        command: name.to_owned(),
        args: Vec::new(),
        env: BTreeMap::new(),
        root_markers: Vec::new(),
        file_extensions: file_extensions
            .iter()
            .map(|extension| (*extension).to_owned())
            .collect(),
        initialization_options: serde_json::Value::Null,
        trust_required: true,
        startup_timeout_ms: 10_000,
    }
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
        ("SIGIL_API_KEY".to_owned(), "secret".to_owned()),
    ]));

    assert_eq!(env.get("SAFE_FLAG").map(String::as_str), Some("1"));
    assert!(!env.contains_key("SIGIL_API_KEY"));
}

#[test]
fn file_uri_roundtrips_paths_with_spaces() {
    let path = std::path::Path::new("/tmp/sigil space/src/main.rs");
    let uri = file_uri_from_path(path);

    assert_eq!(path_from_file_uri(&uri), Some(path.to_path_buf()));
}
