use std::{collections::BTreeMap, fs};

use sigil_kernel::{
    CodeIntelStartup, CodeIntelligenceConfig, CodeIntelligenceDiscoveryConfig, LanguageServerConfig,
};

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
fn effective_server_plan_reports_degraded_status_when_discovery_fails() {
    let missing_root = std::env::temp_dir().join("sigil-code-intel-missing-root");
    let config = CodeIntelligenceConfig {
        enabled: true,
        discovery: CodeIntelligenceDiscoveryConfig {
            enabled: true,
            report_missing: true,
        },
        ..CodeIntelligenceConfig::default()
    };

    let plan = effective_server_plan(&config, &missing_root);

    assert!(plan.servers.contains(&default_rust_analyzer_server()));
    assert!(
        plan.statuses.iter().any(|status| {
            status.server == "discovery" && status.status.starts_with("degraded ")
        })
    );
}

#[test]
fn effective_server_plan_uses_successful_discovery_results() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    fs::write(
        temp.path().join("Cargo.toml"),
        "[package]\nname='x'\nversion='0.1.0'\n",
    )
    .expect("cargo file should write");
    let config = CodeIntelligenceConfig {
        enabled: true,
        discovery: CodeIntelligenceDiscoveryConfig {
            enabled: true,
            report_missing: true,
        },
        ..CodeIntelligenceConfig::default()
    };

    let plan = effective_server_plan(&config, temp.path());

    assert!(
        plan.statuses
            .iter()
            .any(|status| status.server == "rust-analyzer")
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

#[test]
fn effective_server_plan_tracks_disabled_and_extra_configured_servers() {
    let configured_override = LanguageServerConfig {
        command: "custom-ts-lsp".to_owned(),
        ..test_server("typescript-language-server", &["typescript"], &["ts"])
    };
    let configured_extra = test_server("python-lsp", &["python"], &["py"]);
    let config = CodeIntelligenceConfig {
        servers: vec![configured_override.clone(), configured_extra.clone()],
        ..CodeIntelligenceConfig::default()
    };
    let discovered = vec![
        discovered_server(
            test_server("typescript-language-server", &["typescript"], &["ts"]),
            ServerAvailability::Installed,
        ),
        discovered_server(
            test_server("go-lsp", &["go"], &["go"]),
            ServerAvailability::Disabled,
        ),
    ];

    let plan = effective_server_plan_from_discovered(&config, discovered);

    assert!(plan.servers.contains(&configured_override));
    assert!(plan.servers.contains(&configured_extra));
    assert_eq!(
        plan.statuses
            .iter()
            .find(|status| status.server == "typescript-language-server")
            .map(|status| status.status.as_str()),
        Some("configured")
    );
    assert_eq!(
        plan.statuses
            .iter()
            .find(|status| status.server == "go-lsp")
            .map(|status| status.status.as_str()),
        Some("disabled")
    );
    assert_eq!(
        plan.statuses
            .iter()
            .find(|status| status.server == "python-lsp")
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
fn config_enabled_respects_enabled_and_startup_flags() {
    assert!(!config_enabled(&CodeIntelligenceConfig::default()));
    assert!(!config_enabled(&CodeIntelligenceConfig {
        enabled: true,
        startup: CodeIntelStartup::Off,
        ..CodeIntelligenceConfig::default()
    }));
    assert!(config_enabled(&CodeIntelligenceConfig {
        enabled: true,
        startup: CodeIntelStartup::Lazy,
        ..CodeIntelligenceConfig::default()
    }));
}

#[test]
fn fallback_rust_analyzer_server_matches_default_rust_shape() {
    let server = fallback_rust_analyzer_server();

    assert_eq!(server.name, "rust-analyzer");
    assert_eq!(server.languages, vec!["rust"]);
    assert_eq!(server.command, "rust-analyzer");
    assert!(server.root_markers.contains(&"Cargo.toml".to_owned()));
    assert!(server.file_extensions.contains(&"rs".to_owned()));
    assert!(server.trust_required);
}

#[test]
fn canonical_workspace_root_errors_for_missing_directory() {
    let missing = std::env::temp_dir().join("sigil-code-intel-no-root");
    let error = canonical_workspace_root(&missing).expect_err("missing workspace root should fail");

    assert!(
        error
            .to_string()
            .contains("failed to resolve workspace root")
    );
}

#[test]
fn resolve_workspace_file_rejects_empty_and_missing_paths() {
    let temp = tempfile::tempdir().expect("tempdir should build");

    let empty = resolve_workspace_file(temp.path(), "").expect_err("empty path should fail");
    assert!(empty.to_string().contains("path cannot be empty"));

    let missing =
        resolve_workspace_file(temp.path(), "missing.rs").expect_err("missing file should fail");
    assert!(missing.to_string().contains("missing.rs"));
}

#[test]
fn resolve_workspace_file_maps_non_not_found_io_errors() {
    let temp = tempfile::tempdir().expect("tempdir should build");

    let error = resolve_workspace_file(temp.path(), "bad\0path.rs")
        .expect_err("invalid path bytes should surface as io error");

    assert!(error.to_string().contains("bad"));
}

#[test]
fn language_for_path_prefers_matching_extension_then_defaults() {
    let multi = test_server("ts", &["typescript", "javascript"], &["ts", "js"]);
    assert_eq!(
        language_for_path(&multi, std::path::Path::new("src/lib.rs")),
        "rust"
    );
    assert_eq!(
        language_for_path(&multi, std::path::Path::new("src/main.ts")),
        "typescript"
    );

    let empty = LanguageServerConfig {
        languages: Vec::new(),
        ..test_server("empty", &[], &["txt"])
    };
    assert_eq!(
        language_for_path(&empty, std::path::Path::new("notes.txt")),
        "plaintext"
    );
}

#[test]
fn safe_lsp_command_allows_pathless_command_and_blocks_escape() {
    let temp = tempfile::tempdir().expect("tempdir should build");

    assert_eq!(
        safe_lsp_command(temp.path(), "rust-analyzer").expect("pathless command should pass"),
        std::path::PathBuf::from("rust-analyzer")
    );
    assert_eq!(
        safe_lsp_command(temp.path(), "bin/rust-analyzer")
            .expect("nested relative command should stay in workspace"),
        canonical_workspace_root(temp.path())
            .expect("workspace root should canonicalize")
            .join("bin/rust-analyzer")
    );
    assert!(safe_lsp_command(temp.path(), "").is_err());
    assert_eq!(
        safe_lsp_command(temp.path(), "/usr/bin/rust-analyzer")
            .expect("absolute command should pass through"),
        std::path::PathBuf::from("/usr/bin/rust-analyzer")
    );
    assert!(safe_lsp_command(temp.path(), "../bin/lsp").is_err());
}

#[test]
fn find_server_root_uses_markers_and_otherwise_falls_back_to_workspace() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    fs::write(
        temp.path().join("Cargo.toml"),
        "[package]\nname='x'\nversion='0.1.0'\n",
    )
    .expect("cargo file should write");
    let marker_server = test_server("rust-analyzer", &["rust"], &["rs"]);
    let fallback_server = LanguageServerConfig {
        root_markers: vec!["missing.marker".to_owned()],
        ..test_server("other", &["rust"], &["rs"])
    };

    let marker_root = find_server_root(temp.path(), &marker_server).expect("marker root");
    let fallback_root =
        find_server_root(temp.path(), &fallback_server).expect("fallback root should resolve");

    let canonical_root = std::fs::canonicalize(temp.path()).expect("workspace should canonicalize");
    assert_eq!(marker_root, canonical_root);
    assert_eq!(fallback_root, canonical_root);
}

#[test]
fn safe_lsp_command_resolves_relative_paths_inside_workspace() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    fs::create_dir_all(temp.path().join("tools")).expect("tools dir should build");

    let resolved = safe_lsp_command(temp.path(), "./tools/lsp")
        .expect("relative command inside workspace should resolve");

    assert_eq!(
        resolved,
        std::fs::canonicalize(temp.path())
            .expect("workspace should canonicalize")
            .join("tools")
            .join("lsp")
    );
}

#[test]
fn safe_lsp_command_allows_relative_command_without_dot_prefix() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    fs::create_dir_all(temp.path().join("tools")).expect("tools dir should build");

    let command = safe_lsp_command(temp.path(), "tools/custom-lsp")
        .expect("workspace-relative command should pass");

    assert_eq!(
        command,
        temp.path()
            .canonicalize()
            .expect("root should canonicalize")
            .join("tools")
            .join("custom-lsp")
    );
}

#[test]
fn language_and_server_matching_cover_multi_language_variants() {
    let server = LanguageServerConfig {
        languages: vec!["typescript".to_owned(), "javascript".to_owned()],
        file_extensions: vec![".tsx".to_owned(), "js".to_owned()],
        ..test_server("typescript-language-server", &["typescript"], &["ts"])
    };
    let servers = vec![server.clone()];

    assert_eq!(
        language_for_path(&server, std::path::Path::new("src/lib.rs")),
        "rust"
    );
    assert_eq!(
        language_for_path(&server, std::path::Path::new("src/index.tsx")),
        "typescript"
    );
    assert_eq!(
        server_for_path(&servers, std::path::Path::new("src/COMPONENT.TSX"))
            .map(|value| value.name.as_str()),
        Some("typescript-language-server")
    );
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

#[test]
fn file_uri_encodes_relative_paths_and_invalid_hex_is_left_literal() {
    let uri = file_uri_from_path(std::path::Path::new("relative dir/main.rs"));

    assert!(uri.starts_with("file://"));
    assert!(uri.contains("relative%20dir/main.rs"));
    assert_eq!(
        path_from_file_uri("file:///tmp/%ZZ/path"),
        Some(std::path::PathBuf::from("/tmp/%ZZ/path"))
    );
}

#[test]
fn lexical_normalize_handles_curdir_and_parent_before_any_root() {
    assert_eq!(
        super::lexical_normalize(std::path::Path::new("./../tools/../lsp"))
            .expect("relative paths should normalize"),
        std::path::PathBuf::from("../lsp")
    );
}
