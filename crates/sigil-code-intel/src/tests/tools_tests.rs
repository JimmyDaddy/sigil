use std::{collections::BTreeMap, fs};

use anyhow::anyhow;
use serde_json::json;
use sigil_kernel::{
    CodeIntelStartup, CodeIntelligenceConfig, CodeIntelligenceDiscoveryConfig,
    LanguageServerConfig, ToolCall, ToolContext, ToolRegistry,
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

fn write_tooling_lsp_server(
    path: &std::path::Path,
    workspace_symbol_uri: &str,
    external_uri: &str,
) {
    fs::write(
        path,
        format!(
            r#"
import json
import sys

WORKSPACE_SYMBOL_URI = {workspace_symbol_uri:?}
EXTERNAL_URI = {external_uri:?}


def read_message():
    headers = {{}}
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        if line in (b"\r\n", b"\n"):
            break
        key, value = line.decode("ascii").split(":", 1)
        headers[key.lower()] = value.strip()
    length = int(headers.get("content-length", "0"))
    if length == 0:
        return None
    return json.loads(sys.stdin.buffer.read(length))


def write_message(message):
    body = json.dumps(message, separators=(",", ":")).encode("utf-8")
    sys.stdout.buffer.write(f"Content-Length: {{len(body)}}\r\n\r\n".encode("ascii"))
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()


while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    request_id = message.get("id")
    if method == "initialize":
        write_message({{
            "jsonrpc": "2.0",
            "id": request_id,
            "result": {{
                "capabilities": {{
                    "definitionProvider": True,
                    "referencesProvider": True,
                    "workspaceSymbolProvider": True,
                    "diagnosticProvider": {{}}
                }}
            }},
        }})
    elif method == "workspace/symbol":
        write_message({{
            "jsonrpc": "2.0",
            "id": request_id,
            "result": [{{
                "name": "hello",
                "kind": 12,
                "location": {{
                    "uri": WORKSPACE_SYMBOL_URI,
                    "range": {{
                        "start": {{"line": 0, "character": 7}},
                        "end": {{"line": 0, "character": 12}},
                    }},
                }},
            }}],
        }})
    elif method == "textDocument/definition":
        uri = message["params"]["textDocument"]["uri"]
        write_message({{
            "jsonrpc": "2.0",
            "id": request_id,
            "result": [{{
                "uri": uri,
                "range": {{
                    "start": {{"line": 0, "character": 7}},
                    "end": {{"line": 0, "character": 12}},
                }},
            }}],
        }})
    elif method == "textDocument/references":
        uri = message["params"]["textDocument"]["uri"]
        write_message({{
            "jsonrpc": "2.0",
            "id": request_id,
            "result": [
                {{
                    "uri": uri,
                    "range": {{
                        "start": {{"line": 0, "character": 7}},
                        "end": {{"line": 0, "character": 12}},
                    }},
                }},
                {{
                    "uri": EXTERNAL_URI,
                    "range": {{
                        "start": {{"line": 0, "character": 0}},
                        "end": {{"line": 0, "character": 1}},
                    }},
                }},
            ],
        }})
    elif method == "textDocument/diagnostic":
        uri = message["params"]["textDocument"]["uri"]
        write_message({{
            "jsonrpc": "2.0",
            "id": request_id,
            "result": {{"kind": "full", "items": [{{
                "uri": uri,
                "range": {{
                    "start": {{"line": 0, "character": 0}},
                    "end": {{"line": 0, "character": 3}},
                }},
                "severity": 1,
                "message": "broken"
            }}]}},
        }})
    elif method == "shutdown":
        write_message({{"jsonrpc": "2.0", "id": request_id, "result": None}})
    elif method == "exit":
        break
"#
        ),
    )
    .expect("tooling LSP server script should write");
}

fn tooling_lsp_config(script_path: &std::path::Path) -> CodeIntelligenceConfig {
    CodeIntelligenceConfig {
        enabled: true,
        startup: CodeIntelStartup::Lazy,
        default_timeout_ms: 5_000,
        max_results: 10,
        max_payload_bytes: 64 * 1024,
        discovery: CodeIntelligenceDiscoveryConfig {
            enabled: false,
            report_missing: true,
        },
        servers: vec![LanguageServerConfig {
            name: "rust-analyzer".to_owned(),
            languages: vec!["rust".to_owned()],
            command: "python3".to_owned(),
            args: vec![script_path.to_string_lossy().to_string()],
            env: Default::default(),
            root_markers: vec!["Cargo.toml".to_owned()],
            file_extensions: vec!["rs".to_owned()],
            initialization_options: serde_json::Value::Null,
            trust_required: true,
            startup_timeout_ms: 5_000,
        }],
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

#[tokio::test]
async fn code_workspace_definition_references_and_diagnostics_tools_use_lsp() {
    if std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }

    let temp = tempfile::tempdir().expect("tempdir should build");
    fs::write(temp.path().join("Cargo.toml"), "[package]\nname='x'\n").expect("cargo file");
    let inside = temp.path().join("lib.rs");
    fs::write(&inside, "pub fn hello() {}\n").expect("source should write");
    let outside = tempfile::NamedTempFile::new().expect("outside file should build");
    let script = temp.path().join("tooling_lsp.py");
    write_tooling_lsp_server(
        &script,
        &crate::workspace::file_uri_from_path(&inside),
        &crate::workspace::file_uri_from_path(outside.path()),
    );
    let mut registry = ToolRegistry::new();
    register_code_intelligence_tools(
        &mut registry,
        &tooling_lsp_config(&script),
        temp.path().to_path_buf(),
    );
    let ctx = ToolContext {
        workspace_root: temp.path().to_path_buf(),
        timeout_secs: 1,
    };

    let workspace = registry
        .execute(
            ctx.clone(),
            ToolCall {
                id: "workspace".to_owned(),
                name: "code_workspace_symbols".to_owned(),
                args_json: json!({ "query": "hello" }).to_string(),
            },
        )
        .await
        .expect("workspace symbols tool should execute");
    assert!(!workspace.is_error());
    assert!(workspace.content.contains("\"workspace_symbols\""));

    let definition = registry
        .execute(
            ctx.clone(),
            ToolCall {
                id: "definition".to_owned(),
                name: "code_definition".to_owned(),
                args_json: json!({
                    "path": "lib.rs",
                    "line": 1,
                    "character": 7
                })
                .to_string(),
            },
        )
        .await
        .expect("definition tool should execute");
    assert!(!definition.is_error());
    assert!(definition.content.contains("\"definition\""));

    let references = registry
        .execute(
            ctx.clone(),
            ToolCall {
                id: "references".to_owned(),
                name: "code_references".to_owned(),
                args_json: json!({
                    "path": "lib.rs",
                    "line": 1,
                    "character": 7,
                    "include_declaration": true
                })
                .to_string(),
            },
        )
        .await
        .expect("references tool should execute");
    assert!(!references.is_error());
    assert_eq!(references.metadata.total_entries, Some(1));

    let diagnostics = registry
        .execute(
            ctx,
            ToolCall {
                id: "diagnostics".to_owned(),
                name: "code_diagnostics".to_owned(),
                args_json: json!({ "paths": ["lib.rs"], "severity": "error" }).to_string(),
            },
        )
        .await
        .expect("diagnostics tool should execute");
    assert!(!diagnostics.is_error());
    assert!(diagnostics.content.contains("\"diagnostics\""));
}

#[test]
fn helper_functions_classify_and_validate_arguments() {
    assert_eq!(
        classify_error("outside workspace"),
        sigil_kernel::ToolErrorKind::PathOutsideWorkspace
    );
    assert_eq!(
        classify_error("file not found"),
        sigil_kernel::ToolErrorKind::NotFound
    );
    assert_eq!(
        classify_error("timed out"),
        sigil_kernel::ToolErrorKind::Timeout
    );
    assert_eq!(
        classify_error("server does not support this"),
        sigil_kernel::ToolErrorKind::Unsupported
    );
    assert_eq!(
        classify_error("other"),
        sigil_kernel::ToolErrorKind::Protocol
    );

    let args = json!({
        "name": "value",
        "line": 7,
        "max_results": 3,
        "paths": ["lib.rs", "src/lib.rs"]
    });
    assert_eq!(required_string(&args, "name").expect("string"), "value");
    assert_eq!(optional_string(&args, "name").as_deref(), Some("value"));
    assert_eq!(required_u64(&args, "line").expect("u64"), 7);
    assert_eq!(optional_usize(&args, "max_results"), Some(3));
    assert_eq!(
        string_array(&args, "paths").expect("paths"),
        vec!["lib.rs".to_owned(), "src/lib.rs".to_owned()]
    );
    assert!(required_string(&json!({}), "name").is_err());
    assert!(required_u64(&json!({}), "line").is_err());
    assert!(string_array(&json!({ "paths": [] }), "paths").is_err());
    assert!(string_array(&json!({ "paths": [""] }), "paths").is_err());
}

#[test]
fn result_from_response_maps_errors_and_subject_helpers_use_workspace_scope() {
    let error = result_from_response::<serde_json::Value>(
        1024,
        "call-1".to_owned(),
        "code_symbols",
        "symbols",
        json!({ "path": "lib.rs" }),
        Err(anyhow!("request timed out")),
        "path=lib.rs".to_owned(),
    )
    .expect("tool result should build");
    assert!(error.is_error());
    let sigil_kernel::ToolResultStatus::Error(tool_error) = &error.status else {
        panic!("expected timeout tool error");
    };
    assert_eq!(tool_error.kind, sigil_kernel::ToolErrorKind::Timeout);

    let temp = tempfile::tempdir().expect("tempdir should build");
    fs::write(temp.path().join("lib.rs"), "pub fn hello() {}\n").expect("source should write");
    let service = CodeIntelligenceService::new(temp.path().to_path_buf(), enabled_config());

    let path = path_subject(&service, "lib.rs").expect("path subject should build");
    assert_eq!(path.scope, ToolSubjectScope::Workspace);
    assert_eq!(path.normalized, "lib.rs");

    let workspace = workspace_subject(temp.path()).expect("workspace subject should build");
    assert_eq!(workspace.scope, ToolSubjectScope::Workspace);

    let mut results = vec![
        json!({"name": "one", "detail": "x".repeat(300)}),
        json!({"name": "two", "detail": "x".repeat(300)}),
        json!({"name": "three", "detail": "x".repeat(300)}),
    ];
    let mut metadata = crate::service::QueryMetadata {
        returned: 0,
        total: 3,
        truncated: false,
        elapsed_ms: 1,
        external_results_filtered: 0,
    };
    let (content, truncated) = bounded_response_content(
        64,
        "code_symbols",
        "symbols",
        &json!({ "path": "lib.rs" }),
        "tree-sitter-rust",
        "fallback",
        &[],
        &mut results,
        &mut metadata,
    )
    .expect("bounded content should build");
    assert!(truncated);
    assert!(content.len() <= 512);
    assert!(metadata.truncated);
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
