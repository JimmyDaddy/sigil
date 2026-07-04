use std::{fs, sync::OnceLock, time::Duration};

use sigil_kernel::{CodeIntelStartup, CodeIntelligenceConfig, LanguageServerConfig};

use super::*;
use crate::context::LspContextSnapshotStatus;
use crate::tests::common::{
    fake_lsp_server_config, fake_lsp_server_config_with_env, fake_server, python3_available,
    read_counter, write_fake_lsp_scenario, write_fake_lsp_server,
};

fn fake_config() -> CodeIntelligenceConfig {
    CodeIntelligenceConfig {
        enabled: true,
        server_startup: CodeIntelStartup::Lazy,
        default_timeout_ms: 50,
        max_results: 20,
        max_payload_bytes: 64 * 1024,
        auto_discover: false,
        report_missing: true,
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

fn legacy_fake_lsp_server_config(script_path: &std::path::Path) -> CodeIntelligenceConfig {
    CodeIntelligenceConfig {
        enabled: true,
        server_startup: CodeIntelStartup::Lazy,
        default_timeout_ms: 5_000,
        max_results: 20,
        max_payload_bytes: 64 * 1024,
        auto_discover: false,
        report_missing: true,
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

fn missing_server_config() -> CodeIntelligenceConfig {
    fake_config()
}

#[tokio::test]
async fn warm_lsp_context_snapshot_times_out_when_cache_read_blocks() {
    let temp = tempfile::tempdir().expect("tempdir");
    let service = CodeIntelligenceService::new(temp.path().to_path_buf(), fake_config());
    let _held_cache = service.inner.symbol_cache.lock().await;

    let snapshot = service
        .warm_lsp_context_snapshot("parse_config", 10, Duration::from_millis(1))
        .await;

    assert_eq!(
        snapshot.status,
        LspContextSnapshotStatus::TimedOut { timeout_ms: 1 }
    );
    assert!(snapshot.symbols.is_empty());
    assert!(snapshot.diagnostics.is_empty());
    assert!(snapshot.references.is_empty());
}

async fn fake_lsp_test_guard() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await
}

fn write_legacy_fake_lsp_server(path: &std::path::Path) {
    fs::write(
        path,
        r#"
import json
import sys


def read_message():
    headers = {}
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
    sys.stdout.buffer.write(f"Content-Length: {len(body)}\r\n\r\n".encode("ascii"))
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()


while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    request_id = message.get("id")
    if method == "initialize":
        write_message({
            "jsonrpc": "2.0",
            "id": request_id,
            "result": {"capabilities": {"documentSymbolProvider": True}},
        })
    elif method == "textDocument/documentSymbol":
        write_message({
            "jsonrpc": "2.0",
            "id": request_id,
            "result": [{
                "name": "hello",
                "kind": 12,
                "range": {
                    "start": {"line": 0, "character": 0},
                    "end": {"line": 0, "character": 17},
                },
                "selectionRange": {
                    "start": {"line": 0, "character": 7},
                    "end": {"line": 0, "character": 12},
                },
            }],
        })
    elif method == "shutdown":
        write_message({"jsonrpc": "2.0", "id": request_id, "result": None})
    elif method == "exit":
        break
"#,
    )
    .expect("fake LSP server script should write");
}

fn write_full_feature_lsp_server(
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
                    "documentSymbolProvider": True,
                    "definitionProvider": True,
                    "referencesProvider": True,
                    "workspaceSymbolProvider": True,
                    "diagnosticProvider": {{}}
                }}
            }},
        }})
    elif method == "textDocument/documentSymbol":
        write_message({{
            "jsonrpc": "2.0",
            "id": request_id,
            "result": [{{
                "name": "hello",
                "kind": 12,
                "range": {{
                    "start": {{"line": 0, "character": 0}},
                    "end": {{"line": 0, "character": 17}},
                }},
                "children": [{{
                    "name": "inner_helper",
                    "kind": 6,
                    "range": {{
                        "start": {{"line": 1, "character": 0}},
                        "end": {{"line": 1, "character": 5}},
                    }},
                }}]
            }}],
        }})
    elif method == "textDocument/definition":
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
                    "targetUri": uri,
                    "targetSelectionRange": {{
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
                {{
                    "range": {{
                        "start": {{"line": 0, "character": 0}},
                        "end": {{"line": 0, "character": 1}},
                    }},
                }},
            ],
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
                    "uri": uri,
                    "range": {{
                        "start": {{"line": 2, "character": 7}},
                        "end": {{"line": 2, "character": 12}},
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
    elif method == "workspace/symbol":
        write_message({{
            "jsonrpc": "2.0",
            "id": request_id,
            "result": [
                {{
                    "name": "hello",
                    "kind": 12,
                    "containerName": "module",
                    "location": {{
                        "uri": WORKSPACE_SYMBOL_URI,
                        "range": {{
                            "start": {{"line": 0, "character": 7}},
                            "end": {{"line": 0, "character": 12}},
                        }},
                    }},
                }},
                {{
                    "name": "skip-external",
                    "kind": 13,
                    "location": {{
                        "uri": EXTERNAL_URI,
                        "range": {{
                            "start": {{"line": 0, "character": 0}},
                            "end": {{"line": 0, "character": 1}},
                        }},
                    }},
                }},
                {{
                    "name": "missing-range",
                    "kind": 13,
                    "location": {{
                        "uri": WORKSPACE_SYMBOL_URI
                    }},
                }},
            ],
        }})
    elif method == "textDocument/diagnostic":
        uri = message["params"]["textDocument"]["uri"]
        write_message({{
            "jsonrpc": "2.0",
            "id": request_id,
            "result": {{"kind": "full", "items": []}},
        }})
        write_message({{
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {{
                "uri": uri,
                "diagnostics": [{{
                    "range": {{
                        "start": {{"line": 0, "character": 0}},
                        "end": {{"line": 0, "character": 3}},
                    }},
                    "severity": 2,
                    "message": "warn from publish diagnostics",
                    "source": "fake-lsp"
                }}]
            }}
        }})
    elif method == "shutdown":
        write_message({{"jsonrpc": "2.0", "id": request_id, "result": None}})
    elif method == "exit":
        break
"#
        ),
    )
    .expect("full-feature LSP server script should write");
}

fn full_feature_lsp_server_config(script_path: &std::path::Path) -> CodeIntelligenceConfig {
    CodeIntelligenceConfig {
        enabled: true,
        server_startup: CodeIntelStartup::Lazy,
        default_timeout_ms: 5_000,
        max_results: 20,
        max_payload_bytes: 64 * 1024,
        auto_discover: false,
        report_missing: true,
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
            server_startup: CodeIntelStartup::Lazy,
            auto_discover: true,
            report_missing: true,
            ..CodeIntelligenceConfig::default()
        },
    );

    let plan = service.server_plan_snapshot();
    assert!(!plan.discovery_loaded);
    assert!(plan.servers.is_empty());
    assert!(plan.discovery_statuses.is_empty());
}

#[tokio::test]
async fn lazy_discovery_loads_plan_on_first_query() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    fs::write(
        temp.path().join("Cargo.toml"),
        "[package]\nname='x'\nversion='0.1.0'\n",
    )
    .expect("cargo file should write");
    fs::write(temp.path().join("lib.rs"), "pub fn hello() {}\n").expect("source should write");
    let service = CodeIntelligenceService::new(
        temp.path().to_path_buf(),
        CodeIntelligenceConfig {
            enabled: true,
            server_startup: CodeIntelStartup::Lazy,
            auto_discover: true,
            report_missing: true,
            ..CodeIntelligenceConfig::default()
        },
    );

    let _ = service
        .workspace_symbols("hello", 10)
        .await
        .expect("tree-sitter fallback should succeed");

    assert!(service.server_plan_snapshot().discovery_loaded);
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
async fn document_symbols_uses_configured_lsp_server_when_available() {
    if std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let _guard = fake_lsp_test_guard().await;
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
    write_legacy_fake_lsp_server(&server_script);
    let service = CodeIntelligenceService::new(
        temp.path().to_path_buf(),
        legacy_fake_lsp_server_config(&server_script),
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
            server_startup: CodeIntelStartup::Lazy,
            default_timeout_ms: 10_000,
            max_results: 20,
            max_payload_bytes: 64 * 1024,
            auto_discover: true,
            report_missing: true,
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

#[tokio::test]
async fn diagnostics_skips_unsupported_files_without_rust_fallback() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    fs::write(temp.path().join("rustfmt.toml"), "edition = \"2024\"\n")
        .expect("config should write");
    let service = CodeIntelligenceService::new(temp.path().to_path_buf(), fake_config());

    let result = service
        .diagnostics(&["rustfmt.toml".to_owned()], Some("error"), 10)
        .await
        .expect("unsupported diagnostics should not fail");

    assert!(result.results.is_empty());
    assert_eq!(result.server, "code-intel");
    assert_eq!(result.capability, "diagnostics/unsupported");
    assert!(
        !result
            .server_statuses
            .iter()
            .any(|status| status.server == "tree-sitter-rust")
    );
    assert!(result.server_statuses.iter().any(|status| {
        status.server == "code-intel" && status.status == "unsupported rustfmt.toml"
    }));
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

#[test]
fn code_action_selection_helpers_cover_filters_and_errors() {
    let summary = parse_code_action_summary(&serde_json::json!({
        "title": "Fix it",
        "kind": "quickfix",
        "isPreferred": true,
        "diagnostics": [{ "message": "broken" }],
        "edit": {},
        "command": { "command": "do" }
    }))
    .expect("summary should parse");
    let selected = select_code_action(
        vec![
            serde_json::json!({ "title": "Command only", "kind": "quickfix" }),
            serde_json::json!({ "title": "Editable", "kind": "quickfix", "edit": {} }),
        ],
        None,
        None,
    )
    .expect("single editable action should be selected");
    let kind_selected = select_code_action(
        vec![serde_json::json!({
            "title": "Rename",
            "kind": "refactor.rename",
            "edit": {}
        })],
        None,
        Some("refactor"),
    )
    .expect("kind prefix should match");

    assert_eq!(summary.title, "Fix it");
    assert_eq!(summary.kind.as_deref(), Some("quickfix"));
    assert!(summary.is_preferred);
    assert_eq!(summary.diagnostics, 1);
    assert!(summary.has_edit);
    assert!(summary.has_command);
    assert!(parse_code_action_summary(&serde_json::json!({ "kind": "quickfix" })).is_none());
    assert_eq!(selected["title"], "Editable");
    assert_eq!(kind_selected["title"], "Rename");
    assert!(
        select_code_action(Vec::new(), None, None)
            .expect_err("empty actions should fail")
            .to_string()
            .contains("no code action")
    );
    assert!(
        select_code_action(
            vec![
                serde_json::json!({ "title": "One", "kind": "quickfix", "edit": {} }),
                serde_json::json!({ "title": "Two", "kind": "quickfix", "edit": {} }),
            ],
            None,
            None,
        )
        .expect_err("ambiguous actions should fail")
        .to_string()
        .contains("multiple")
    );
}

#[tokio::test]
async fn code_actions_filter_malformed_and_only_kind_results() {
    if !python3_available() {
        return;
    }
    let _guard = fake_lsp_test_guard().await;
    let temp = tempfile::tempdir().expect("tempdir should build");
    fs::create_dir(temp.path().join("src")).expect("src dir should build");
    fs::write(temp.path().join("Cargo.toml"), "[package]\nname='x'\n").expect("cargo file");
    fs::write(
        temp.path().join("src").join("lib.rs"),
        "pub fn hello() {}\n",
    )
    .expect("source should write");
    let script = temp.path().join("fake_lsp.py");
    let scenario = temp.path().join("code-actions.json");
    write_fake_lsp_server(&script);
    write_fake_lsp_scenario(
        &scenario,
        &serde_json::json!({
            "methods": {
                "initialize": {
                    "result": { "capabilities": { "codeActionProvider": true } }
                },
                "textDocument/codeAction": {
                    "result": [
                        { "title": "Rename", "kind": "refactor.rename", "edit": { "changes": {} } },
                        { "title": "Quick fix", "kind": "quickfix" },
                        { "kind": "broken" }
                    ]
                },
                "shutdown": { "result": null }
            }
        }),
    );
    let service = CodeIntelligenceService::new(
        temp.path().to_path_buf(),
        fake_lsp_server_config(&script, &scenario),
    );

    let result = service
        .code_actions("src/lib.rs", 1, 7, Some(1), Some(12), Some("refactor"), 10)
        .await
        .expect("code actions should return");

    assert_eq!(result.results.len(), 1);
    assert_eq!(result.results[0].title, "Rename");
    assert_eq!(result.metadata.external_results_filtered, 1);
    service.shutdown().await.expect("shutdown should succeed");
}

#[tokio::test]
async fn code_action_edit_plan_resolves_actions_and_unsupported_capabilities() {
    if !python3_available() {
        return;
    }
    let _guard = fake_lsp_test_guard().await;
    let temp = tempfile::tempdir().expect("tempdir should build");
    fs::create_dir(temp.path().join("src")).expect("src dir should build");
    fs::write(temp.path().join("Cargo.toml"), "[package]\nname='x'\n").expect("cargo file");
    let source = temp.path().join("src").join("lib.rs");
    fs::write(&source, "pub fn hello() {}\n").expect("source should write");
    let uri = crate::workspace::file_uri_from_path(&source);
    let script = temp.path().join("fake_lsp.py");
    let scenario = temp.path().join("resolve-action.json");
    write_fake_lsp_server(&script);
    write_fake_lsp_scenario(
        &scenario,
        &serde_json::json!({
            "methods": {
                "initialize": {
                    "result": {
                        "capabilities": {
                            "codeActionProvider": { "resolveProvider": true },
                            "renameProvider": false
                        }
                    }
                },
                "textDocument/codeAction": {
                    "result": [{
                        "title": "Resolve me",
                        "kind": "quickfix",
                        "data": { "id": 1 }
                    }]
                },
                "codeAction/resolve": {
                    "result": {
                        "title": "Resolve me",
                        "kind": "quickfix",
                        "edit": {
                            "changes": {
                                uri: [{
                                    "range": {
                                        "start": { "line": 0, "character": 7 },
                                        "end": { "line": 0, "character": 12 }
                                    },
                                    "newText": "greet"
                                }]
                            }
                        }
                    }
                },
                "shutdown": { "result": null }
            }
        }),
    );
    let service = CodeIntelligenceService::new(
        temp.path().to_path_buf(),
        fake_lsp_server_config(&script, &scenario),
    );

    let plan = service
        .code_action_edit_plan("src/lib.rs", 1, 7, None, None, Some("Resolve me"), None)
        .await
        .expect("resolved action should produce edit plan");
    let rename_error = service
        .rename_edit_plan("src/lib.rs", 1, 7, "greet")
        .await
        .expect_err("rename should be unsupported");

    assert_eq!(plan.edit.files[0].edits[0].new_text, "greet");
    assert!(rename_error.to_string().contains("textDocument/rename"));
    service.shutdown().await.expect("shutdown should succeed");
}

#[tokio::test]
async fn code_actions_report_unsupported_capability() {
    if !python3_available() {
        return;
    }
    let _guard = fake_lsp_test_guard().await;
    let temp = tempfile::tempdir().expect("tempdir should build");
    fs::create_dir(temp.path().join("src")).expect("src dir should build");
    fs::write(temp.path().join("Cargo.toml"), "[package]\nname='x'\n").expect("cargo file");
    fs::write(
        temp.path().join("src").join("lib.rs"),
        "pub fn hello() {}\n",
    )
    .expect("source should write");
    let script = temp.path().join("fake_lsp.py");
    let scenario = temp.path().join("unsupported-actions.json");
    write_fake_lsp_server(&script);
    write_fake_lsp_scenario(
        &scenario,
        &serde_json::json!({
            "methods": {
                "initialize": { "result": { "capabilities": {} } },
                "shutdown": { "result": null }
            }
        }),
    );
    let service = CodeIntelligenceService::new(
        temp.path().to_path_buf(),
        fake_lsp_server_config(&script, &scenario),
    );

    let error = service
        .code_actions("src/lib.rs", 1, 7, None, None, None, 10)
        .await
        .expect_err("code actions should be unsupported");

    assert!(error.to_string().contains("textDocument/codeAction"));
    service.shutdown().await.expect("shutdown should succeed");
}

#[test]
fn code_intel_status_line_formats_all_variants() {
    assert_eq!(CodeIntelStatus::Off.line(), "off");
    assert_eq!(
        CodeIntelStatus::Starting {
            server: "rust-analyzer".to_owned(),
        }
        .line(),
        "starting rust-analyzer"
    );
    assert_eq!(
        CodeIntelStatus::Indexing {
            server: "rust-analyzer".to_owned(),
            detail: Some("workspace".to_owned()),
        }
        .line(),
        "indexing rust-analyzer workspace"
    );
    assert_eq!(
        CodeIntelStatus::Indexing {
            server: "rust-analyzer".to_owned(),
            detail: None,
        }
        .line(),
        "indexing rust-analyzer"
    );
    assert_eq!(
        CodeIntelStatus::Ready { servers: 2 }.line(),
        "ready 2 server(s)"
    );
    assert_eq!(
        CodeIntelStatus::Degraded {
            reason: "lazy".to_owned(),
        }
        .line(),
        "degraded lazy"
    );
    assert_eq!(
        CodeIntelStatus::Error {
            reason: "boom".to_owned(),
        }
        .line(),
        "error boom"
    );
}

#[tokio::test]
async fn workspace_symbols_falls_back_to_tree_sitter_when_lsp_is_unavailable() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    fs::write(temp.path().join("Cargo.toml"), "[package]\nname='x'\n").expect("cargo file");
    fs::create_dir(temp.path().join("src")).expect("src dir");
    fs::write(
        temp.path().join("src").join("lib.rs"),
        "pub fn hello_workspace() {}\n",
    )
    .expect("source should write");
    let service = CodeIntelligenceService::new(temp.path().to_path_buf(), fake_config());

    let result = service
        .workspace_symbols("hello_workspace", 10)
        .await
        .expect("fallback workspace symbols should succeed");

    assert_eq!(result.server, "tree-sitter-rust");
    assert_eq!(result.capability, "tree_sitter/workspace_symbols");
    assert!(
        result
            .results
            .iter()
            .any(|symbol| symbol.name == "hello_workspace")
    );
    assert!(
        result
            .server_statuses
            .iter()
            .any(|status| { status.server == "tree-sitter-rust" && status.status == "fallback" })
    );
}

#[tokio::test]
async fn workspace_symbols_tree_sitter_fallback_stops_after_limit() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    fs::write(temp.path().join("Cargo.toml"), "[package]\nname='x'\n").expect("cargo file");
    fs::create_dir(temp.path().join("src")).expect("src dir");
    for index in 0..4 {
        fs::write(
            temp.path().join("src").join(format!("file_{index}.rs")),
            format!("pub fn shared_symbol_{index}() {{}}\n"),
        )
        .expect("source should write");
    }
    let service = CodeIntelligenceService::new(temp.path().to_path_buf(), fake_config());

    let result = service
        .workspace_symbols("shared_symbol", 1)
        .await
        .expect("fallback workspace symbols should succeed");

    assert_eq!(result.server, "tree-sitter-rust");
    assert_eq!(result.results.len(), 1);
    assert!(result.metadata.truncated);
}

#[tokio::test]
async fn lsp_service_caches_document_symbols_and_supports_shutdown() {
    if std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let _guard = fake_lsp_test_guard().await;

    let temp = tempfile::tempdir().expect("tempdir should build");
    fs::create_dir(temp.path().join("src")).expect("src dir should build");
    fs::write(temp.path().join("Cargo.toml"), "[package]\nname='x'\n").expect("cargo file");
    let inside = temp.path().join("src").join("lib.rs");
    fs::write(&inside, "pub fn hello() {}\npub fn inner_helper() {}\n").expect("source write");
    let outside = tempfile::NamedTempFile::new().expect("outside file should build");
    let server_script = temp.path().join("full_feature_lsp.py");
    write_full_feature_lsp_server(
        &server_script,
        &file_uri_from_path(&inside),
        &file_uri_from_path(outside.path()),
    );
    let service = CodeIntelligenceService::new(
        temp.path().to_path_buf(),
        full_feature_lsp_server_config(&server_script),
    );

    let first = service
        .document_symbols("src/lib.rs", Some("hello"), 10)
        .await
        .expect("first symbols call should succeed");
    let second = service
        .document_symbols("src/lib.rs", Some("hello"), 10)
        .await
        .expect("cached symbols call should succeed");

    assert_eq!(first, second);
    assert_eq!(service.inner.clients.lock().await.len(), 1);

    service.shutdown().await.expect("shutdown should succeed");
    assert_eq!(service.status().await, CodeIntelStatus::Off);
}

#[tokio::test]
async fn definition_references_workspace_symbols_and_diagnostics_use_lsp() {
    if std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let _guard = fake_lsp_test_guard().await;

    let temp = tempfile::tempdir().expect("tempdir should build");
    fs::create_dir(temp.path().join("src")).expect("src dir should build");
    fs::write(temp.path().join("Cargo.toml"), "[package]\nname='x'\n").expect("cargo file");
    let inside = temp.path().join("src").join("lib.rs");
    fs::write(
        &inside,
        "pub fn hello() {}\nfn helper() {}\npub fn hello_again() { hello(); }\n",
    )
    .expect("source write");
    let outside = tempfile::NamedTempFile::new().expect("outside file should build");
    let server_script = temp.path().join("full_feature_lsp.py");
    write_full_feature_lsp_server(
        &server_script,
        &file_uri_from_path(&inside),
        &file_uri_from_path(outside.path()),
    );
    let service = CodeIntelligenceService::new(
        temp.path().to_path_buf(),
        full_feature_lsp_server_config(&server_script),
    );

    let definition = service
        .definition("src/lib.rs", 1, 7, 10)
        .await
        .expect("definition should succeed");
    assert_eq!(definition.server, "rust-analyzer");
    assert_eq!(definition.results.len(), 1);
    assert_eq!(definition.metadata.external_results_filtered, 2);
    assert_eq!(definition.results[0].path, "src/lib.rs");
    assert_eq!(
        definition.results[0].preview.as_deref(),
        Some("pub fn hello() {}")
    );

    let references = service
        .references("src/lib.rs", 1, 7, true, 10)
        .await
        .expect("references should succeed");
    assert_eq!(references.server, "rust-analyzer");
    assert_eq!(references.results.len(), 2);
    assert_eq!(references.metadata.external_results_filtered, 1);
    assert!(
        references
            .server_statuses
            .iter()
            .any(|status| status.server == "rust-analyzer" && status.status == "ready")
    );

    let workspace = service
        .workspace_symbols("hello", 10)
        .await
        .expect("workspace symbols should succeed");
    assert_eq!(workspace.server, "rust-analyzer");
    assert_eq!(workspace.results.len(), 1);
    assert_eq!(workspace.metadata.external_results_filtered, 2);
    assert_eq!(
        workspace.results[0].container_name.as_deref(),
        Some("module")
    );

    let diagnostics = service
        .diagnostics(&["src/lib.rs".to_owned()], Some("warning"), 10)
        .await
        .expect("diagnostics should succeed");
    assert_eq!(diagnostics.server, "rust-analyzer");
    assert_eq!(diagnostics.capability, "textDocument/diagnostic");
    assert_eq!(diagnostics.results.len(), 1);
    assert_eq!(diagnostics.results[0].severity, "warning");
    assert_eq!(diagnostics.results[0].source.as_deref(), Some("fake-lsp"));

    service.shutdown().await.expect("shutdown should succeed");
}

#[tokio::test]
async fn ensure_client_by_name_reports_disabled_and_unknown_server() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    let disabled = CodeIntelligenceService::new(temp.path().to_path_buf(), Default::default());
    let disabled_error = match disabled.ensure_client_by_name("rust-analyzer").await {
        Ok(_) => panic!("disabled service should reject clients"),
        Err(error) => error,
    };
    assert!(disabled_error.to_string().contains("disabled"));

    let enabled = CodeIntelligenceService::new(temp.path().to_path_buf(), fake_config());
    let unknown_error = match enabled.ensure_client_by_name("other").await {
        Ok(_) => panic!("unknown server should fail"),
        Err(error) => error,
    };
    assert!(
        unknown_error
            .to_string()
            .contains("unknown language server other")
    );
}

#[test]
fn initial_server_plan_marks_configured_servers_while_lazy_discovery_is_pending() {
    let config = CodeIntelligenceConfig {
        enabled: true,
        auto_discover: true,
        report_missing: true,
        ..fake_config()
    };

    let plan = initial_server_plan(&config, std::path::Path::new("."));

    assert_eq!(plan.servers, config.servers);
    assert!(!plan.discovery_loaded);
    assert_eq!(plan.discovery_statuses.len(), 1);
    assert_eq!(plan.discovery_statuses[0].server, "missing-rust-analyzer");
    assert_eq!(plan.discovery_statuses[0].status, "configured");
    assert_eq!(plan.discovery_statuses[0].languages, vec!["rust"]);
}

#[tokio::test]
async fn service_helper_functions_cover_defaults_and_truncation() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    let file = temp.path().join("src.rs");
    fs::write(&file, "first line\nsecond line\n").expect("source write");

    let started = Instant::now();
    let response = response(
        "rust-analyzer".to_owned(),
        vec!["rust".to_owned()],
        "documentSymbol".to_owned(),
        vec![1_u8, 2, 3],
        2,
        started,
        1,
    );
    assert_eq!(response.metadata.returned, 2);
    assert!(response.metadata.truncated);
    assert_eq!(response.metadata.external_results_filtered, 1);

    let response = response_with_statuses(
        "tree-sitter-rust".to_owned(),
        "fallback".to_owned(),
        vec![1_u8],
        Vec::new(),
        5,
        Instant::now(),
        0,
    );
    assert_eq!(response.server_statuses.len(), 1);
    assert_eq!(
        response_with_filtered(
            "rust-analyzer".to_owned(),
            vec!["rust".to_owned()],
            "definition".to_owned(),
            vec![1_u8],
            5,
            Instant::now(),
            2,
        )
        .metadata
        .external_results_filtered,
        2
    );

    let service = CodeIntelligenceService::new(temp.path().to_path_buf(), fake_config());
    let (locations, filtered) = service
        .parse_locations(vec![
            json!({
                "uri": file_uri_from_path(&file),
                "range": {
                    "start": { "line": 0, "character": 0 },
                    "end": { "line": 0, "character": 5 }
                }
            }),
            json!({
                "targetUri": file_uri_from_path(&file),
                "targetSelectionRange": {
                    "start": { "line": 0, "character": 0 },
                    "end": { "line": 0, "character": 5 }
                }
            }),
            json!({
                "uri": file_uri_from_path(&temp.path().join("missing.rs"))
            }),
        ])
        .await
        .expect("locations should parse");
    assert_eq!(locations.len(), 1);
    assert_eq!(filtered, 1);
    assert_eq!(locations[0].preview.as_deref(), Some("first line"));

    let parsed = service
        .parse_symbol_information(&json!({
            "name": "hello",
            "kind": 12,
            "containerName": "module",
            "location": {
                "uri": file_uri_from_path(&file),
                "range": {
                    "start": { "line": 0, "character": 0 },
                    "end": { "line": 0, "character": 5 }
                }
            }
        }))
        .await
        .expect("symbol should parse");
    assert_eq!(parsed.container_name.as_deref(), Some("module"));
    assert!(
        service
            .parse_symbol_information(&json!({ "name": "broken" }))
            .await
            .is_none()
    );

    let mut symbols = Vec::new();
    collect_lsp_symbols(
        &json!([{
            "name": "hello",
            "kind": 12,
            "children": [{
                "name": "child",
                "kind": 6,
                "range": {
                    "start": { "line": 1, "character": 0 },
                    "end": { "line": 1, "character": 3 }
                }
            }]
        }]),
        "src.rs",
        None,
        &mut symbols,
    );
    assert_eq!(symbols.len(), 2);
    assert_eq!(symbols[0].range.start_line, 1);
    assert_eq!(symbols[1].container_name.as_deref(), Some("hello"));
    let mut ignored = Vec::new();
    collect_lsp_symbols(
        &json!({"name":"not-an-array"}),
        "src.rs",
        None,
        &mut ignored,
    );
    collect_lsp_symbols(&json!([{ "kind": 12 }]), "src.rs", None, &mut ignored);
    assert!(ignored.is_empty());

    let diagnostic = parse_diagnostic_value(
        temp.path(),
        &file,
        &json!({
            "severity": 4,
            "message": "x".repeat(600),
        }),
    )
    .expect("diagnostic should parse");
    assert_eq!(diagnostic.path, "src.rs");
    assert_eq!(diagnostic.severity, "hint");
    assert_eq!(diagnostic.message.len(), 500);
    assert_eq!(
        pull_diagnostics_from_response(json!([{"message":"array"}])).len(),
        1
    );
    assert_eq!(
        parse_range(&json!({
            "start": { "line": 0, "character": 1 },
            "end": { "line": 1, "character": 2 }
        }))
        .expect("range should parse")
        .end_line,
        2
    );
    assert!(parse_range(&json!({ "start": {} })).is_none());
    let symbol_kinds = [
        (1, "file"),
        (2, "module"),
        (3, "namespace"),
        (4, "package"),
        (5, "class"),
        (6, "method"),
        (7, "property"),
        (8, "field"),
        (9, "constructor"),
        (10, "enum"),
        (11, "interface"),
        (12, "function"),
        (13, "variable"),
        (14, "constant"),
        (15, "string"),
        (16, "number"),
        (17, "boolean"),
        (18, "array"),
        (19, "object"),
        (20, "key"),
        (21, "null"),
        (22, "enum_member"),
        (23, "struct"),
        (24, "event"),
        (25, "operator"),
        (26, "type_parameter"),
    ];
    for (kind, label) in symbol_kinds {
        assert_eq!(lsp_symbol_kind(Some(kind)), label);
    }
    assert_eq!(lsp_symbol_kind(None), "symbol");
    assert_eq!(lsp_diagnostic_severity(Some(1)), "error");
    assert_eq!(lsp_diagnostic_severity(Some(2)), "warning");
    assert_eq!(lsp_diagnostic_severity(Some(3)), "information");
    assert_eq!(lsp_diagnostic_severity(Some(4)), "hint");
    assert_eq!(lsp_diagnostic_severity(None), "unknown");
    assert_eq!(preview_line(&file, 2).await.as_deref(), Some("second line"));
    assert!(preview_line(&file, 99).await.is_none());
    assert_eq!(service.limit(0), service.inner.config.max_results);
    assert_eq!(service.limit(1), 1);
    assert_eq!(service.request_timeout(), Duration::from_millis(50));
}

#[tokio::test]
async fn service_basic_accessors_status_and_shutdown_cover_idle_paths() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    let source = temp.path().join("src.rs");
    fs::write(&source, "pub fn hello() {}\n").expect("source should write");
    let service = CodeIntelligenceService::new(temp.path().to_path_buf(), fake_config());

    assert!(service.enabled());
    assert_eq!(service.config().servers.len(), 1);
    assert_eq!(service.workspace_root(), temp.path());
    let plan = service.server_plan_snapshot();
    assert_eq!(plan.servers.len(), 1);
    assert_eq!(plan.servers[0].name, "missing-rust-analyzer");
    assert!(plan.discovery_loaded);
    assert_eq!(
        service
            .resolve_file("src.rs")
            .expect("file should resolve")
            .canonicalize()
            .expect("resolved source should canonicalize"),
        source
            .canonicalize()
            .expect("expected source should canonicalize")
    );
    assert!(service.resolve_file("../outside.rs").is_err());
    assert_eq!(
        CodeIntelligenceService::configured_status_line(service.config()),
        "lazy"
    );
    assert_eq!(
        CodeIntelligenceService::configured_status_line(&CodeIntelligenceConfig::default()),
        "off"
    );
    assert_eq!(
        CodeIntelligenceService::configured_status_line(&CodeIntelligenceConfig {
            enabled: true,
            server_startup: CodeIntelStartup::Off,
            ..CodeIntelligenceConfig::default()
        }),
        "off"
    );

    service
        .shutdown()
        .await
        .expect("idle shutdown should succeed");
}

#[tokio::test]
async fn lsp_helpers_report_unsupported_capabilities_and_missing_servers() {
    if !python3_available() {
        return;
    }
    let _guard = fake_lsp_test_guard().await;
    let temp = tempfile::tempdir().expect("tempdir should build");
    fs::create_dir(temp.path().join("src")).expect("src dir should build");
    fs::write(
        temp.path().join("Cargo.toml"),
        "[package]\nname='x'\nversion='0.1.0'\nedition='2024'\n",
    )
    .expect("cargo file should write");
    let rust_path = temp.path().join("src").join("lib.rs");
    let python_path = temp.path().join("script.py");
    fs::write(&rust_path, "pub fn hello() {}\n").expect("rust source should write");
    fs::write(&python_path, "print('hi')\n").expect("python source should write");
    let server_script = temp.path().join("fake_lsp.py");
    let scenario_path = temp.path().join("unsupported-scenario.json");
    write_fake_lsp_server(&server_script);
    write_fake_lsp_scenario(
        &scenario_path,
        &serde_json::json!({
            "methods": {
                "initialize": { "result": { "capabilities": {} } },
                "shutdown": { "result": null }
            }
        }),
    );
    let service = CodeIntelligenceService::new(
        temp.path().to_path_buf(),
        fake_lsp_server_config(&server_script, &scenario_path),
    );

    let doc_error = service
        .lsp_document_symbols(&rust_path, None, 10, Instant::now())
        .await
        .expect_err("document symbols should require capability");
    assert!(doc_error.to_string().contains("documentSymbol"));

    let definition_error = service
        .definition("src/lib.rs", 1, 0, 10)
        .await
        .expect_err("definition should require capability");
    assert!(
        definition_error
            .to_string()
            .contains("textDocument/definition")
    );

    let references_error = service
        .references("src/lib.rs", 1, 0, true, 10)
        .await
        .expect_err("references should require capability");
    assert!(
        references_error
            .to_string()
            .contains("textDocument/references")
    );

    let workspace_error = service
        .lsp_workspace_symbols_for_server("rust-analyzer", "hello")
        .await
        .expect_err("workspace symbols should require capability");
    assert!(workspace_error.to_string().contains("workspace/symbol"));

    let no_server_error = match service.ensure_client(&python_path).await {
        Ok(_) => panic!("non-rust files should not match rust-analyzer"),
        Err(error) => error,
    };
    assert!(no_server_error.to_string().contains("script.py"));

    service.shutdown().await.expect("shutdown should succeed");
}

#[tokio::test]
async fn lsp_workspace_symbols_errors_when_no_servers_are_configured() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    let service = CodeIntelligenceService::new(temp.path().to_path_buf(), Default::default());

    let error = service
        .lsp_workspace_symbols("hello", 10, Instant::now())
        .await
        .expect_err("private lsp query should fail without server configs");

    assert!(error.to_string().contains("disabled"));
}

#[tokio::test]
async fn definition_filters_malformed_and_external_locations() {
    if !python3_available() {
        return;
    }
    let _guard = fake_lsp_test_guard().await;
    let temp = tempfile::tempdir().expect("tempdir should build");
    fs::create_dir(temp.path().join("src")).expect("src dir should build");
    fs::write(
        temp.path().join("Cargo.toml"),
        "[package]\nname='x'\nversion='0.1.0'\nedition='2024'\n",
    )
    .expect("cargo file should write");
    let source_path = temp.path().join("src").join("lib.rs");
    fs::write(&source_path, "pub fn hello() {}\n").expect("source should write");
    let external = tempfile::NamedTempFile::new().expect("external file should build");
    fs::write(external.path(), "pub fn external() {}\n").expect("external source should write");
    let server_script = temp.path().join("fake_lsp.py");
    let scenario_path = temp.path().join("definition-scenario.json");
    write_fake_lsp_server(&server_script);
    write_fake_lsp_scenario(
        &scenario_path,
        &serde_json::json!({
            "methods": {
                "initialize": {
                    "result": { "capabilities": { "definitionProvider": true } }
                },
                "textDocument/definition": {
                    "result": [
                        {
                            "uri": file_uri_from_path(&source_path),
                            "range": {
                                "start": { "line": 0, "character": 0 },
                                "end": { "line": 0, "character": 5 }
                            }
                        },
                        {
                            "uri": file_uri_from_path(&source_path),
                            "range": {
                                "start": { "line": 0, "character": 0 },
                                "end": { "line": 0, "character": 5 }
                            }
                        },
                        {
                            "targetUri": file_uri_from_path(external.path()),
                            "targetSelectionRange": {
                                "start": { "line": 0, "character": 0 },
                                "end": { "line": 0, "character": 8 }
                            }
                        },
                        {
                            "uri": file_uri_from_path(&source_path),
                            "range": { "start": { "line": 0 } }
                        },
                        {
                            "range": {
                                "start": { "line": 0, "character": 0 },
                                "end": { "line": 0, "character": 3 }
                            }
                        }
                    ]
                },
                "shutdown": { "result": null }
            }
        }),
    );
    let service = CodeIntelligenceService::new(
        temp.path().to_path_buf(),
        fake_lsp_server_config(&server_script, &scenario_path),
    );

    let result = service
        .definition("src/lib.rs", 1, 0, 10)
        .await
        .expect("definition query should succeed");

    assert_eq!(result.server, "rust-analyzer");
    assert_eq!(result.results.len(), 1);
    assert_eq!(result.metadata.external_results_filtered, 3);
    assert_eq!(result.results[0].path, "src/lib.rs");
    assert_eq!(
        result.results[0].preview.as_deref(),
        Some("pub fn hello() {}")
    );
    service.shutdown().await.expect("shutdown should succeed");
}

#[tokio::test]
async fn definition_startup_failure_marks_service_degraded() {
    if !python3_available() {
        return;
    }
    let _guard = fake_lsp_test_guard().await;
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
        fake_lsp_server_config_with_env(
            &server_script,
            "initialize_malformed",
            100,
            BTreeMap::new(),
        ),
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
async fn diagnostics_uses_publish_diagnostics_when_pull_is_unavailable() {
    if !python3_available() {
        return;
    }
    let _guard = fake_lsp_test_guard().await;
    let temp = tempfile::tempdir().expect("tempdir should build");
    fs::create_dir(temp.path().join("src")).expect("src dir should build");
    fs::write(
        temp.path().join("Cargo.toml"),
        "[package]\nname='x'\nversion='0.1.0'\nedition='2024'\n",
    )
    .expect("cargo file should write");
    let source_path = temp.path().join("src").join("lib.rs");
    fs::write(&source_path, "pub fn hello() {}\n").expect("source should write");
    let canonical_source = fs::canonicalize(&source_path).expect("source path should canonicalize");
    let server_script = temp.path().join("fake_lsp.py");
    let scenario_path = temp.path().join("diagnostics-scenario.json");
    write_fake_lsp_server(&server_script);
    write_fake_lsp_scenario(
        &scenario_path,
        &serde_json::json!({
            "methods": {
                "initialize": {
                    "result": { "capabilities": {} }
                },
                "shutdown": { "result": null }
            },
            "notifications": {
                "textDocument/didOpen": {
                    "publish_diagnostics": {
                        "uri": file_uri_from_path(&canonical_source),
                        "diagnostics": [
                            {
                                "range": {
                                    "start": { "line": 0, "character": 0 },
                                    "end": { "line": 0, "character": 2 }
                                },
                                "severity": 1,
                                "message": "first error"
                            },
                            {
                                "range": {
                                    "start": { "line": 0, "character": 3 },
                                    "end": { "line": 0, "character": 5 }
                                },
                                "severity": 2,
                                "message": "warning detail"
                            }
                        ]
                    }
                }
            }
        }),
    );
    let service = CodeIntelligenceService::new(
        temp.path().to_path_buf(),
        fake_lsp_server_config(&server_script, &scenario_path),
    );

    let result = service
        .diagnostics(&["src/lib.rs".to_owned()], Some("WARNING"), 10)
        .await
        .expect("diagnostics query should succeed");

    assert_eq!(result.server, "rust-analyzer");
    assert_eq!(result.capability, "textDocument/publishDiagnostics");
    assert_eq!(result.results.len(), 1);
    assert_eq!(result.results[0].severity, "warning");
    assert_eq!(result.results[0].message, "warning detail");
    service.shutdown().await.expect("shutdown should succeed");
}

#[tokio::test]
async fn diagnostics_wait_for_publish_notifications_when_pull_response_is_empty() {
    if !python3_available() {
        return;
    }
    let _guard = fake_lsp_test_guard().await;
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
        fake_lsp_server_config_with_env(
            &server_script,
            "diagnostics_publish_only",
            5_000,
            BTreeMap::new(),
        ),
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

#[tokio::test]
async fn document_symbols_cache_key_changes_when_query_changes() {
    if !python3_available() {
        return;
    }
    let _guard = fake_lsp_test_guard().await;
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
    let _guard = fake_lsp_test_guard().await;
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
        fake_lsp_server_config_with_env(
            &server_script,
            "document_symbols_malformed",
            5_000,
            BTreeMap::new(),
        ),
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
async fn document_symbols_returns_cached_result_for_repeat_query() {
    if !python3_available() {
        return;
    }
    let _guard = fake_lsp_test_guard().await;
    let temp = tempfile::tempdir().expect("tempdir should build");
    fs::write(
        temp.path().join("Cargo.toml"),
        "[package]\nname='x'\nversion='0.1.0'\n",
    )
    .expect("cargo file should write");
    let source_path = temp.path().join("lib.rs");
    fs::write(&source_path, "pub fn hello() {}\n").expect("source should write");
    let server_script = temp.path().join("fake_lsp.py");
    let scenario_path = temp.path().join("cache-scenario.json");
    write_fake_lsp_server(&server_script);
    write_fake_lsp_scenario(
        &scenario_path,
        &serde_json::json!({
            "methods": {
                "initialize": {
                    "result": { "capabilities": { "documentSymbolProvider": true } }
                },
                "textDocument/documentSymbol": {
                    "result_sequence": [
                        [{
                            "name": "hello",
                            "kind": 12,
                            "range": {
                                "start": { "line": 0, "character": 0 },
                                "end": { "line": 0, "character": 5 }
                            }
                        }],
                        [{
                            "name": "goodbye",
                            "kind": 12,
                            "range": {
                                "start": { "line": 0, "character": 0 },
                                "end": { "line": 0, "character": 7 }
                            }
                        }]
                    ]
                },
                "shutdown": { "result": null }
            }
        }),
    );
    let service = CodeIntelligenceService::new(
        temp.path().to_path_buf(),
        fake_lsp_server_config(&server_script, &scenario_path),
    );

    let first = service
        .document_symbols("lib.rs", None, 10)
        .await
        .expect("first query should succeed");
    fs::write(&source_path, "pub fn goodbye() {}\n").expect("updated source should write");
    let second = service
        .document_symbols("lib.rs", None, 10)
        .await
        .expect("cached query should succeed");

    assert!(first.results.iter().any(|symbol| symbol.name == "hello"));
    assert_eq!(first.results, second.results);
    service.shutdown().await.expect("shutdown should succeed");
}

#[tokio::test]
async fn document_symbols_uses_lsp_cache_for_identical_requests() {
    if !python3_available() {
        return;
    }
    let _guard = fake_lsp_test_guard().await;
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
async fn workspace_symbols_fall_back_when_lsp_workspace_symbol_is_unavailable() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    fs::create_dir(temp.path().join("src")).expect("src dir should build");
    fs::write(
        temp.path().join("Cargo.toml"),
        "[package]\nname='x'\nversion='0.1.0'\n",
    )
    .expect("cargo file should write");
    fs::write(
        temp.path().join("src").join("lib.rs"),
        "pub fn hello() {}\n",
    )
    .expect("source should write");
    let service = CodeIntelligenceService::new(temp.path().to_path_buf(), missing_server_config());

    let result = service
        .workspace_symbols("hello", 10)
        .await
        .expect("workspace symbol fallback should succeed");

    assert_eq!(result.server, "tree-sitter-rust");
    assert_eq!(result.capability, "tree_sitter/workspace_symbols");
    assert!(result.results.iter().any(|symbol| symbol.name == "hello"));
    assert!(result.server_statuses.iter().any(|status| {
        status.server == "missing-rust-analyzer"
            && status.status == "degraded workspace/symbol unavailable"
    }));
}

#[tokio::test]
async fn workspace_symbols_filter_external_and_malformed_entries() {
    if !python3_available() {
        return;
    }
    let _guard = fake_lsp_test_guard().await;
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
async fn workspace_symbols_labels_multiple_successful_lsp_servers() {
    if !python3_available() {
        return;
    }
    let _guard = fake_lsp_test_guard().await;
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
    let scenario_path = temp.path().join("multi-workspace-symbols.json");
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
                        "name": "hello",
                        "kind": 12,
                        "location": {
                            "uri": file_uri_from_path(&workspace_file),
                            "range": {
                                "start": { "line": 0, "character": 0 },
                                "end": { "line": 0, "character": 5 }
                            }
                        }
                    }]
                },
                "shutdown": { "result": null }
            }
        }),
    );
    let mut config = fake_lsp_server_config(&server_script, &scenario_path);
    config.servers.push(fake_server(
        "second-rust-analyzer",
        &["rust"],
        &["rs"],
        &["Cargo.toml"],
        &server_script,
        &scenario_path,
        BTreeMap::new(),
        2_000,
    ));
    let service = CodeIntelligenceService::new(temp.path().to_path_buf(), config);

    let result = service
        .workspace_symbols("hello", 10)
        .await
        .expect("workspace symbols should succeed");

    assert_eq!(result.server, "multiple");
    assert_eq!(result.results.len(), 2);
    assert!(
        result
            .server_statuses
            .iter()
            .any(|status| { status.server == "rust-analyzer" && status.status == "ready" })
    );
    assert!(
        result
            .server_statuses
            .iter()
            .any(|status| { status.server == "second-rust-analyzer" && status.status == "ready" })
    );
    service.shutdown().await.expect("shutdown should succeed");
}
