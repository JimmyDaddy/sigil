use std::{collections::BTreeMap, fs};

use serde_json::json;
use sigil_kernel::{
    CodeIntelStartup, CodeIntelligenceConfig, CodeIntelligenceDiscoveryConfig,
    LanguageServerConfig, ToolCall, ToolContext, ToolErrorKind, ToolRegistry,
};

use super::*;
use crate::tests::common::{
    fake_server, python3_available, write_fake_lsp_scenario, write_fake_lsp_server,
};
use crate::workspace::file_uri_from_path;

fn enabled_config() -> CodeIntelligenceConfig {
    CodeIntelligenceConfig {
        enabled: true,
        startup: CodeIntelStartup::Lazy,
        default_timeout_ms: 50,
        max_results: 10,
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

fn bounded_payload_config() -> CodeIntelligenceConfig {
    CodeIntelligenceConfig {
        max_results: 50,
        max_payload_bytes: 900,
        ..enabled_config()
    }
}

fn fake_tool_lsp_config(
    script_path: &std::path::Path,
    scenario_path: &std::path::Path,
    timeout_ms: u64,
) -> CodeIntelligenceConfig {
    CodeIntelligenceConfig {
        default_timeout_ms: timeout_ms,
        servers: vec![fake_server(
            "rust-analyzer",
            &["rust"],
            &["rs"],
            &["Cargo.toml"],
            script_path,
            scenario_path,
            BTreeMap::new(),
            2_000,
        )],
        ..enabled_config()
    }
}

#[test]
fn register_code_intelligence_tools_skips_disabled_config() {
    let mut registry = ToolRegistry::new();
    let temp = tempfile::tempdir().expect("tempdir should build");

    let service = register_code_intelligence_tools(
        &mut registry,
        &Default::default(),
        temp.path().to_path_buf(),
    );

    assert!(service.is_none());
    assert!(registry.spec_for("code_symbols").is_none());
}

#[tokio::test]
async fn code_symbols_tool_returns_bounded_json_envelope() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    fs::write(temp.path().join("lib.rs"), "pub fn hello() {}\n").expect("source should write");
    let mut registry = ToolRegistry::new();
    register_code_intelligence_tools(&mut registry, &enabled_config(), temp.path().to_path_buf());

    let result = registry
        .execute(
            ToolContext {
                workspace_root: temp.path().to_path_buf(),
                timeout_secs: 1,
            },
            ToolCall {
                id: "call-code".to_owned(),
                name: "code_symbols".to_owned(),
                args_json: json!({ "path": "lib.rs", "query": "hello", "max_results": 5 })
                    .to_string(),
            },
        )
        .await
        .expect("tool should execute");

    assert!(!result.is_error());
    assert_eq!(result.metadata.returned_entries, Some(1));
    let content: serde_json::Value =
        serde_json::from_str(&result.content).expect("content should be json");
    assert_eq!(content["tool"], "code_symbols");
    assert_eq!(content["server"], "tree-sitter-rust");
    let content_servers = content["servers"]
        .as_array()
        .expect("content servers should be an array");
    assert!(content_servers.iter().any(|server| {
        server["server"] == "tree-sitter-rust"
            && server["status"] == "fallback"
            && server["languages"][0] == "rust"
    }));
    assert_eq!(content["symbols"][0]["name"], "hello");
    let metadata_servers = result.metadata.details["code_intelligence"]["servers"]
        .as_array()
        .expect("metadata servers should be an array");
    assert!(
        metadata_servers
            .iter()
            .any(|server| server["server"] == "tree-sitter-rust" && server["status"] == "fallback")
    );
}

#[tokio::test]
async fn code_symbols_tool_enforces_payload_byte_limit() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    let source = (0..40)
        .map(|index| format!("pub fn symbol_{index}_with_long_suffix_name() {{}}\n"))
        .collect::<String>();
    fs::write(temp.path().join("lib.rs"), source).expect("source should write");
    let mut registry = ToolRegistry::new();
    register_code_intelligence_tools(
        &mut registry,
        &bounded_payload_config(),
        temp.path().to_path_buf(),
    );

    let result = registry
        .execute(
            ToolContext {
                workspace_root: temp.path().to_path_buf(),
                timeout_secs: 1,
            },
            ToolCall {
                id: "call-code".to_owned(),
                name: "code_symbols".to_owned(),
                args_json: json!({ "path": "lib.rs", "max_results": 50 }).to_string(),
            },
        )
        .await
        .expect("tool should execute");

    assert!(result.content.len() <= 900);
    assert!(result.metadata.truncated);
    assert!(result.metadata.returned_entries.unwrap_or(50) < 40);
}

#[test]
fn code_symbols_permission_subject_rejects_external_path() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    let outside = tempfile::NamedTempFile::new().expect("outside file should build");
    let mut registry = ToolRegistry::new();
    register_code_intelligence_tools(&mut registry, &enabled_config(), temp.path().to_path_buf());

    let error = registry
        .permission_subjects(
            &ToolContext {
                workspace_root: temp.path().to_path_buf(),
                timeout_secs: 1,
            },
            &ToolCall {
                id: "call-code".to_owned(),
                name: "code_symbols".to_owned(),
                args_json: json!({ "path": outside.path() }).to_string(),
            },
        )
        .expect_err("outside path should fail");

    assert!(error.to_string().contains("outside workspace"));
}

#[test]
fn code_workspace_symbols_permission_subject_targets_workspace_root() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    let mut registry = ToolRegistry::new();
    register_code_intelligence_tools(&mut registry, &enabled_config(), temp.path().to_path_buf());

    let subjects = registry
        .permission_subjects(
            &ToolContext {
                workspace_root: temp.path().to_path_buf(),
                timeout_secs: 1,
            },
            &ToolCall {
                id: "call-workspace".to_owned(),
                name: "code_workspace_symbols".to_owned(),
                args_json: json!({ "query": "hello" }).to_string(),
            },
        )
        .expect("workspace subject should resolve");

    assert_eq!(subjects.len(), 1);
    assert_eq!(subjects[0].original, ".");
    assert_eq!(subjects[0].normalized, ".");
    assert_eq!(subjects[0].scope.as_str(), "workspace");
    assert_eq!(
        subjects[0].canonical_path.as_deref(),
        Some(
            temp.path()
                .canonicalize()
                .expect("root should canonicalize")
                .as_path()
        )
    );
}

#[tokio::test]
async fn code_workspace_symbols_tool_returns_lsp_results() {
    if !python3_available() {
        return;
    }
    let temp = tempfile::tempdir().expect("tempdir should build");
    fs::create_dir(temp.path().join("src")).expect("src dir should build");
    fs::write(
        temp.path().join("Cargo.toml"),
        "[package]\nname='x'\nversion='0.1.0'\n",
    )
    .expect("cargo file should write");
    let source_path = temp.path().join("src").join("lib.rs");
    fs::write(&source_path, "pub fn hello_workspace() {}\n").expect("source should write");
    let canonical_source = fs::canonicalize(&source_path).expect("source should canonicalize");
    let server_script = temp.path().join("fake_lsp.py");
    let scenario_path = temp.path().join("workspace-symbols.json");
    write_fake_lsp_server(&server_script);
    write_fake_lsp_scenario(
        &scenario_path,
        &serde_json::json!({
            "methods": {
                "initialize": {
                    "result": { "capabilities": { "workspaceSymbolProvider": true } }
                },
                "workspace/symbol": {
                    "result": [{
                        "name": "hello_workspace",
                        "kind": 12,
                        "location": {
                            "uri": file_uri_from_path(&canonical_source),
                            "range": {
                                "start": { "line": 0, "character": 0 },
                                "end": { "line": 0, "character": 5 }
                            }
                        },
                        "containerName": "crate"
                    }]
                },
                "shutdown": { "result": null }
            }
        }),
    );
    let mut registry = ToolRegistry::new();
    register_code_intelligence_tools(
        &mut registry,
        &fake_tool_lsp_config(&server_script, &scenario_path, 250),
        temp.path().to_path_buf(),
    );

    let result = registry
        .execute(
            ToolContext {
                workspace_root: temp.path().to_path_buf(),
                timeout_secs: 1,
            },
            ToolCall {
                id: "call-workspace".to_owned(),
                name: "code_workspace_symbols".to_owned(),
                args_json: json!({ "query": "hello" }).to_string(),
            },
        )
        .await
        .expect("workspace symbols tool should execute");

    assert!(!result.is_error());
    let content: serde_json::Value =
        serde_json::from_str(&result.content).expect("content should be json");
    assert_eq!(content["tool"], "code_workspace_symbols");
    assert_eq!(content["server"], "rust-analyzer");
    assert_eq!(content["workspace_symbols"][0]["name"], "hello_workspace");
}

#[tokio::test]
async fn code_definition_tool_maps_timeout_error_kind() {
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
    let scenario_path = temp.path().join("timeout.json");
    write_fake_lsp_server(&server_script);
    write_fake_lsp_scenario(
        &scenario_path,
        &serde_json::json!({
            "methods": {
                "initialize": {
                    "result": { "capabilities": { "definitionProvider": true } }
                },
                "textDocument/definition": {
                    "sleep_ms": 100,
                    "result": []
                },
                "shutdown": { "result": null }
            }
        }),
    );
    let mut registry = ToolRegistry::new();
    register_code_intelligence_tools(
        &mut registry,
        &fake_tool_lsp_config(&server_script, &scenario_path, 10),
        temp.path().to_path_buf(),
    );

    let result = registry
        .execute(
            ToolContext {
                workspace_root: temp.path().to_path_buf(),
                timeout_secs: 1,
            },
            ToolCall {
                id: "call-definition".to_owned(),
                name: "code_definition".to_owned(),
                args_json: json!({ "path": "src/lib.rs", "line": 1, "character": 0 }).to_string(),
            },
        )
        .await
        .expect("definition tool should execute");

    let summary = result.summary();
    assert!(result.is_error());
    assert_eq!(summary.error_kind, Some(ToolErrorKind::Timeout));
    assert!(
        summary
            .error_message
            .expect("timeout message should exist")
            .contains("timed out")
    );
}

#[tokio::test]
async fn code_diagnostics_tool_maps_missing_files_to_not_found() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    let mut registry = ToolRegistry::new();
    register_code_intelligence_tools(&mut registry, &enabled_config(), temp.path().to_path_buf());

    let result = registry
        .execute(
            ToolContext {
                workspace_root: temp.path().to_path_buf(),
                timeout_secs: 1,
            },
            ToolCall {
                id: "call-diagnostics".to_owned(),
                name: "code_diagnostics".to_owned(),
                args_json: json!({ "paths": ["missing.rs"] }).to_string(),
            },
        )
        .await
        .expect("diagnostics tool should execute");

    let summary = result.summary();
    assert!(result.is_error());
    assert_eq!(summary.error_kind, Some(ToolErrorKind::NotFound));
}
