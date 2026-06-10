use std::fs;

use termquill_kernel::{
    CodeIntelStartup, CodeIntelligenceConfig, CodeIntelligenceDiscoveryConfig, LanguageServerConfig,
};

use super::*;

fn fake_config() -> CodeIntelligenceConfig {
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
            command: "definitely-missing-termquill-lsp".to_owned(),
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
    let service = CodeIntelligenceService::new(temp.path().to_path_buf(), fake_config());

    let result = service
        .document_symbols("lib.rs", Some("hello"), 10)
        .await
        .expect("fallback symbols should succeed");

    assert_eq!(result.server, "tree-sitter-rust");
    assert!(
        result
            .server_statuses
            .iter()
            .any(|status| status.server == "tree-sitter-rust" && status.status == "fallback")
    );
    assert!(result.server_statuses.iter().any(|status| {
        status.server == "missing-rust-analyzer" && status.status.starts_with("degraded ")
    }));
    assert!(result.results.iter().any(|symbol| symbol.name == "hello"));
}

#[tokio::test]
async fn document_symbols_uses_discovered_rust_analyzer_when_available() {
    if std::process::Command::new("rust-analyzer")
        .arg("--version")
        .output()
        .is_err()
    {
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
        .document_symbols("src/lib.rs", Some("hello"), 10)
        .await
        .expect("rust-analyzer symbols should succeed");

    assert_eq!(result.server, "rust-analyzer");
    assert!(
        result
            .server_statuses
            .iter()
            .any(|status| status.server == "rust-analyzer" && status.status == "ready")
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
    let service = CodeIntelligenceService::new(temp.path().to_path_buf(), fake_config());

    let result = service
        .diagnostics(&["broken.rs".to_owned()], Some("error"), 10)
        .await
        .expect("fallback diagnostics should succeed");

    assert_eq!(result.server, "tree-sitter-rust");
    assert!(
        result
            .server_statuses
            .iter()
            .any(|status| status.server == "tree-sitter-rust" && status.status == "fallback")
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
