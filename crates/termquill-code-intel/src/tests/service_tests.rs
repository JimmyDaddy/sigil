use std::fs;

use termquill_kernel::{CodeIntelStartup, CodeIntelligenceConfig, LanguageServerConfig};

use super::*;

fn fake_config() -> CodeIntelligenceConfig {
    CodeIntelligenceConfig {
        enabled: true,
        startup: CodeIntelStartup::Lazy,
        default_timeout_ms: 50,
        max_results: 20,
        max_payload_bytes: 64 * 1024,
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
    assert!(result.results.iter().any(|symbol| symbol.name == "hello"));
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
