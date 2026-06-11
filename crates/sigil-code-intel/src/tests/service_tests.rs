use std::{collections::BTreeMap, fs};

use sigil_kernel::{
    CodeIntelStartup, CodeIntelligenceConfig, CodeIntelligenceDiscoveryConfig, LanguageServerConfig,
};

#[allow(clippy::duplicate_mod)]
#[path = "common.rs"]
mod common;

use super::*;
use common::{
    fake_lsp_server_config, fake_lsp_server_config_with_env, python3_available, read_counter,
    write_fake_lsp_server,
};

fn missing_server_config() -> CodeIntelligenceConfig {
    CodeIntelligenceConfig {
        enabled: true,
        startup: CodeIntelStartup::Lazy,
        default_timeout_ms: 50,
        max_results: 20,
        max_payload_bytes: 64 * 1024,
        discovery: CodeIntelligenceDiscoveryConfig {
            enabled: false,
            report_missing: true,
        },
        servers: vec![LanguageServerConfig {
            name: "missing-rust-analyzer".to_owned(),
            languages: vec!["rust".to_owned()],
            command: "definitely-missing-sigil-lsp".to_owned(),
            args: Vec::new(),
            env: Default::default(),
            root_markers: vec!["Cargo.toml".to_owned()],
            file_extensions: vec!["rs".to_owned()],
            initialization_options: serde_json::Value::Null,
            trust_required: true,
            startup_timeout_ms: 50,
        }],
    }
}

#[tokio::test]
async fn disabled_service_reports_off_status() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    let service = CodeIntelligenceService::new(temp.path().to_path_buf(), Default::default());

    assert_eq!(service.status().await, CodeIntelStatus::Off);
    service
        .shutdown()
        .await
        .expect("shutdown without clients should succeed");
    assert_eq!(service.status().await, CodeIntelStatus::Off);
}

#[test]
fn lazy_discovery_is_deferred_until_first_code_query() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    fs::write(
        temp.path().join("Cargo.toml"),
        "[package]\nname='x'\nversion='0.1.0'\n",
    )
    .expect("cargo file should write");
    let service = CodeIntelligenceService::new(
        temp.path().to_path_buf(),
        CodeIntelligenceConfig {
            enabled: true,
            startup: CodeIntelStartup::Lazy,
            discovery: CodeIntelligenceDiscoveryConfig {
                enabled: true,
                report_missing: true,
            },
            ..CodeIntelligenceConfig::default()
        },
    );

    let plan = service.server_plan_snapshot();
    assert!(!plan.discovery_loaded);
    assert!(plan.servers.is_empty());
    assert!(plan.discovery_statuses.is_empty());
}

#[tokio::test]
async fn document_symbols_falls_back_to_tree_sitter_when_lsp_is_unavailable() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    fs::write(
        temp.path().join("Cargo.toml"),
        "[package]\nname='x'\nversion='0.1.0'\n",
    )
    .expect("cargo file should write");
    fs::write(temp.path().join("lib.rs"), "pub fn hello() {}\n").expect("source should write");
    let service = CodeIntelligenceService::new(temp.path().to_path_buf(), missing_server_config());

    let result = service
        .document_symbols("lib.rs", Some("hello"), 10)
        .await
        .expect("fallback symbols should succeed");

    assert_eq!(result.server, "tree-sitter-rust");
    assert!(
        result
            .server_statuses
            .iter()
            .any(|status| { status.server == "tree-sitter-rust" && status.status == "fallback" })
    );
    assert!(result.server_statuses.iter().any(|status| {
        status.server == "missing-rust-analyzer" && status.status.starts_with("degraded ")
    }));
    assert!(result.results.iter().any(|symbol| symbol.name == "hello"));
}

#[tokio::test]
async fn document_symbols_uses_configured_lsp_server_when_available() {
    if !python3_available() {
        return;
    }
    let temp = tempfile::tempdir().expect("tempdir should build");
    fs::create_dir(temp.path().join("src")).expect("src dir should build");
    fs::write(
        temp.path().join("Cargo.toml"),
        "[package]\nname='x'\nversion='0.1.0'\nedition='2024'\n",
    )
    .expect("cargo file should write");
    fs::write(
        temp.path().join("src").join("lib.rs"),
        "pub fn hello() {}\n",
    )
    .expect("source should write");
    let server_script = temp.path().join("fake_lsp.py");
    write_fake_lsp_server(&server_script);
    let service = CodeIntelligenceService::new(
        temp.path().to_path_buf(),
        fake_lsp_server_config(&server_script, "document_symbols_success", 5_000),
    );

    let result = service
        .document_symbols("src/lib.rs", Some("hello"), 10)
        .await
        .expect("fake LSP symbols should succeed");

    assert_eq!(result.server, "rust-analyzer");
    assert!(
        result
            .server_statuses
            .iter()
            .any(|status| { status.server == "rust-analyzer" && status.status == "ready" })
    );
    assert!(result.results.iter().any(|symbol| symbol.name == "hello"));
    service.shutdown().await.expect("shutdown should succeed");
}

#[tokio::test]
async fn document_symbols_uses_discovered_typescript_server_when_available() {
    if std::process::Command::new("typescript-language-server")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let temp = tempfile::tempdir().expect("tempdir should build");
    fs::create_dir(temp.path().join("src")).expect("src dir should build");
    fs::write(temp.path().join("package.json"), "{}\n").expect("package file should write");
    fs::write(
        temp.path().join("tsconfig.json"),
        "{\"compilerOptions\":{\"target\":\"ES2022\"}}\n",
    )
    .expect("tsconfig file should write");
    fs::write(
        temp.path().join("src").join("index.ts"),
        "export function hello() { return 1; }\n",
    )
    .expect("source should write");
    let service = CodeIntelligenceService::new(
        temp.path().to_path_buf(),
        CodeIntelligenceConfig {
            enabled: true,
            startup: CodeIntelStartup::Lazy,
            default_timeout_ms: 10_000,
            max_results: 20,
            max_payload_bytes: 64 * 1024,
            discovery: Default::default(),
            servers: Vec::new(),
        },
    );

    let result = service
        .document_symbols("src/index.ts", Some("hello"), 10)
        .await
        .expect("typescript-language-server symbols should succeed");

    assert_eq!(result.server, "typescript-language-server");
    assert!(result.server_statuses.iter().any(|status| {
        status.server == "typescript-language-server" && status.status == "ready"
    }));
    assert!(result.results.iter().any(|symbol| symbol.name == "hello"));
    service.shutdown().await.expect("shutdown should succeed");
}

#[tokio::test]
async fn diagnostics_falls_back_to_tree_sitter_syntax_errors() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    fs::write(temp.path().join("broken.rs"), "fn broken( {").expect("source should write");
    let service = CodeIntelligenceService::new(temp.path().to_path_buf(), missing_server_config());

    let result = service
        .diagnostics(&["broken.rs".to_owned()], Some("error"), 10)
        .await
        .expect("fallback diagnostics should succeed");

    assert_eq!(result.server, "tree-sitter-rust");
    assert!(
        result
            .server_statuses
            .iter()
            .any(|status| { status.server == "tree-sitter-rust" && status.status == "fallback" })
    );
    assert!(result.server_statuses.iter().any(|status| {
        status.server == "missing-rust-analyzer" && status.status.starts_with("degraded ")
    }));
    assert!(
        result
            .results
            .iter()
            .any(|diagnostic| diagnostic.severity == "error")
    );
}

#[test]
fn pull_diagnostics_from_response_reads_items_array() {
    let response = serde_json::json!({
        "kind": "full",
        "items": [{
            "range": {
                "start": { "line": 0, "character": 0 },
                "end": { "line": 0, "character": 3 }
            },
            "severity": 1,
            "message": "broken"
        }]
    });

    let diagnostics = pull_diagnostics_from_response(response);

    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0]["message"], "broken");
}

#[tokio::test]
async fn document_symbols_uses_lsp_cache_for_identical_requests() {
    if !python3_available() {
        return;
    }
    let temp = tempfile::tempdir().expect("tempdir should build");
    fs::create_dir(temp.path().join("src")).expect("src dir should build");
    fs::write(
        temp.path().join("Cargo.toml"),
        "[package]\nname='x'\nversion='0.1.0'\nedition='2024'\n",
    )
    .expect("cargo file should write");
    fs::write(
        temp.path().join("src").join("lib.rs"),
        "pub fn hello() {}\n",
    )
    .expect("source should write");
    let server_script = temp.path().join("fake_lsp.py");
    let counter_path = temp.path().join("document_symbol.count");
    write_fake_lsp_server(&server_script);
    let service = CodeIntelligenceService::new(
        temp.path().to_path_buf(),
        fake_lsp_server_config_with_env(
            &server_script,
            "document_symbols_success",
            5_000,
            BTreeMap::from([(
                "SIGIL_FAKE_LSP_COUNTER_FILE".to_owned(),
                counter_path.to_string_lossy().to_string(),
            )]),
        ),
    );

    let first = service
        .document_symbols("src/lib.rs", Some("hello"), 10)
        .await
        .expect("first symbol request should succeed");
    let second = service
        .document_symbols("src/lib.rs", Some("hello"), 10)
        .await
        .expect("second symbol request should hit cache");

    assert_eq!(first.results, second.results);
    assert_eq!(read_counter(&counter_path), 1);
    service.shutdown().await.expect("shutdown should succeed");
}

#[tokio::test]
async fn document_symbols_cache_key_changes_when_query_changes() {
    if !python3_available() {
        return;
    }
    let temp = tempfile::tempdir().expect("tempdir should build");
    fs::create_dir(temp.path().join("src")).expect("src dir should build");
    fs::write(
        temp.path().join("Cargo.toml"),
        "[package]\nname='x'\nversion='0.1.0'\nedition='2024'\n",
    )
    .expect("cargo file should write");
    fs::write(
        temp.path().join("src").join("lib.rs"),
        "pub fn hello() {}\npub fn world() {}\n",
    )
    .expect("source should write");
    let server_script = temp.path().join("fake_lsp.py");
    let counter_path = temp.path().join("document_symbol.count");
    write_fake_lsp_server(&server_script);
    let service = CodeIntelligenceService::new(
        temp.path().to_path_buf(),
        fake_lsp_server_config_with_env(
            &server_script,
            "document_symbols_success",
            5_000,
            BTreeMap::from([(
                "SIGIL_FAKE_LSP_COUNTER_FILE".to_owned(),
                counter_path.to_string_lossy().to_string(),
            )]),
        ),
    );

    let first = service
        .document_symbols("src/lib.rs", Some("hello"), 10)
        .await
        .expect("first symbol request should succeed");
    let second = service
        .document_symbols("src/lib.rs", Some("world"), 10)
        .await
        .expect("second symbol request should bypass cache");

    assert_eq!(first.results.len(), 1);
    assert!(second.results.is_empty());
    assert_eq!(read_counter(&counter_path), 2);
    service.shutdown().await.expect("shutdown should succeed");
}

#[tokio::test]
async fn document_symbols_falls_back_when_lsp_returns_malformed_payload() {
    if !python3_available() {
        return;
    }
    let temp = tempfile::tempdir().expect("tempdir should build");
    fs::create_dir(temp.path().join("src")).expect("src dir should build");
    fs::write(
        temp.path().join("Cargo.toml"),
        "[package]\nname='x'\nversion='0.1.0'\nedition='2024'\n",
    )
    .expect("cargo file should write");
    fs::write(
        temp.path().join("src").join("lib.rs"),
        "pub fn hello() {}\n",
    )
    .expect("source should write");
    let server_script = temp.path().join("fake_lsp.py");
    write_fake_lsp_server(&server_script);
    let service = CodeIntelligenceService::new(
        temp.path().to_path_buf(),
        fake_lsp_server_config(&server_script, "document_symbols_malformed", 5_000),
    );

    let result = service
        .document_symbols("src/lib.rs", Some("hello"), 10)
        .await
        .expect("tree-sitter fallback should succeed");

    assert_eq!(result.server, "tree-sitter-rust");
    assert!(result.results.iter().any(|symbol| symbol.name == "hello"));
    assert!(result.server_statuses.iter().any(|status| {
        status.server == "rust-analyzer" && status.status.contains("body is not valid json")
    }));
    service.shutdown().await.expect("shutdown should succeed");
}

#[tokio::test]
async fn definition_startup_failure_marks_service_degraded() {
    if !python3_available() {
        return;
    }
    let temp = tempfile::tempdir().expect("tempdir should build");
    fs::create_dir(temp.path().join("src")).expect("src dir should build");
    fs::write(
        temp.path().join("Cargo.toml"),
        "[package]\nname='x'\nversion='0.1.0'\nedition='2024'\n",
    )
    .expect("cargo file should write");
    fs::write(
        temp.path().join("src").join("lib.rs"),
        "pub fn hello() {}\n",
    )
    .expect("source should write");
    let server_script = temp.path().join("fake_lsp.py");
    write_fake_lsp_server(&server_script);
    let service = CodeIntelligenceService::new(
        temp.path().to_path_buf(),
        fake_lsp_server_config(&server_script, "initialize_malformed", 100),
    );

    let error = service
        .definition("src/lib.rs", 1, 0, 10)
        .await
        .expect_err("definition request should fail");

    assert!(error.to_string().contains("body is not valid json"));
    match service.status().await {
        CodeIntelStatus::Degraded { reason } => {
            assert!(reason.starts_with("rust-analyzer "));
        }
        status => panic!("expected degraded status, got {status:?}"),
    }
}

#[tokio::test]
async fn workspace_symbols_filter_external_and_malformed_entries() {
    if !python3_available() {
        return;
    }
    let temp = tempfile::tempdir().expect("tempdir should build");
    fs::create_dir(temp.path().join("src")).expect("src dir should build");
    fs::write(
        temp.path().join("Cargo.toml"),
        "[package]\nname='x'\nversion='0.1.0'\nedition='2024'\n",
    )
    .expect("cargo file should write");
    let workspace_file = temp.path().join("src").join("lib.rs");
    fs::write(&workspace_file, "pub fn hello() {}\n").expect("source should write");
    let outside = tempfile::NamedTempFile::new().expect("outside file should build");
    fs::write(outside.path(), "pub fn outside() {}\n").expect("outside source should write");
    let server_script = temp.path().join("fake_lsp.py");
    write_fake_lsp_server(&server_script);
    let service = CodeIntelligenceService::new(
        temp.path().to_path_buf(),
        fake_lsp_server_config_with_env(
            &server_script,
            "workspace_symbols_mixed",
            5_000,
            BTreeMap::from([
                (
                    "SIGIL_FAKE_LSP_VALID_PATH".to_owned(),
                    workspace_file.to_string_lossy().to_string(),
                ),
                (
                    "SIGIL_FAKE_LSP_EXTERNAL_PATH".to_owned(),
                    outside.path().to_string_lossy().to_string(),
                ),
            ]),
        ),
    );

    let result = service
        .workspace_symbols("hello", 10)
        .await
        .expect("workspace symbols should succeed");

    assert_eq!(result.server, "rust-analyzer");
    assert_eq!(result.results.len(), 1);
    assert_eq!(result.results[0].name, "hello");
    assert_eq!(result.metadata.external_results_filtered, 2);
    service.shutdown().await.expect("shutdown should succeed");
}

#[tokio::test]
async fn diagnostics_wait_for_publish_notifications_when_pull_response_is_empty() {
    if !python3_available() {
        return;
    }
    let temp = tempfile::tempdir().expect("tempdir should build");
    fs::create_dir(temp.path().join("src")).expect("src dir should build");
    fs::write(
        temp.path().join("Cargo.toml"),
        "[package]\nname='x'\nversion='0.1.0'\nedition='2024'\n",
    )
    .expect("cargo file should write");
    let workspace_file = temp.path().join("src").join("lib.rs");
    fs::write(&workspace_file, "pub fn hello() {}\n").expect("source should write");
    let server_script = temp.path().join("fake_lsp.py");
    write_fake_lsp_server(&server_script);
    let service = CodeIntelligenceService::new(
        temp.path().to_path_buf(),
        fake_lsp_server_config(&server_script, "diagnostics_publish_only", 5_000),
    );

    let result = service
        .diagnostics(&["src/lib.rs".to_owned()], Some("warning"), 10)
        .await
        .expect("diagnostics should succeed");

    assert_eq!(result.server, "rust-analyzer");
    assert_eq!(result.capability, "textDocument/diagnostic");
    assert_eq!(result.results.len(), 1);
    assert_eq!(result.results[0].message, "from publish diagnostics");
    assert_eq!(result.results[0].severity, "warning");
    assert_eq!(result.results[0].source.as_deref(), Some("fake-lsp"));
    service.shutdown().await.expect("shutdown should succeed");
}
