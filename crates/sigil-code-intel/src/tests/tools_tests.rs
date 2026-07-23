use std::{collections::BTreeMap, fs};

use anyhow::anyhow;
use serde_json::json;
use sigil_kernel::{
    CodeIntelStartup, CodeIntelligenceConfig, JsonlSessionStore, LanguageServerConfig,
    MutationEventRecorder, ToolCall, ToolContext, ToolErrorKind, ToolRegistry, WorkspaceTrust,
    write_file_with_mutation,
};

use super::*;
use crate::tests::common::{
    fake_server, python3_available, write_fake_lsp_scenario, write_fake_lsp_server,
};
use crate::workspace::file_uri_from_path;

async fn prepare_mutation_for_test(
    service: &CodeIntelligenceService,
    ctx: ToolContext,
    call: &ToolCall,
) -> anyhow::Result<(
    crate::prepared_mutation::PreparedMutation,
    Vec<sigil_kernel::ToolSubject>,
)> {
    let args: serde_json::Value = serde_json::from_str(&call.args_json)?;
    let (plan, label) = match call.name.as_str() {
        "code_rename" => {
            let tool = CodeRenameTool {
                service: std::sync::Arc::new(service.clone()),
            };
            (tool.rename_plan(&args).await?, "Rename symbol")
        }
        "code_action" => {
            let tool = CodeActionTool {
                service: std::sync::Arc::new(service.clone()),
            };
            (tool.code_action_plan(&args).await?, "Apply code action")
        }
        name => anyhow::bail!("unsupported prepared mutation test tool {name}"),
    };
    materialize_prepared_mutation(ctx, plan, label).await
}

async fn execute_mutation_for_test(
    ctx: ToolContext,
    call: &ToolCall,
    policy_fingerprint: &str,
    prepared: crate::prepared_mutation::PreparedMutation,
) -> anyhow::Result<ToolResult> {
    execute_prepared_mutation_for_test(
        ctx,
        match call.name.as_str() {
            "code_rename" => "code_rename",
            "code_action" => "code_action",
            name => anyhow::bail!("unsupported prepared mutation test tool {name}"),
        },
        call.id.clone(),
        policy_fingerprint,
        prepared,
    )
    .await
}

fn enabled_config() -> CodeIntelligenceConfig {
    CodeIntelligenceConfig {
        enabled: true,
        server_startup: CodeIntelStartup::Lazy,
        default_timeout_ms: 50,
        max_results: 10,
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

fn register_trusted_code_intelligence_tools(
    registry: &mut ToolRegistry,
    config: &CodeIntelligenceConfig,
    workspace_root: std::path::PathBuf,
) -> Option<CodeIntelligenceService> {
    register_code_intelligence_tools_with_workspace_trust(
        registry,
        config,
        workspace_root,
        WorkspaceTrust::Trusted,
    )
}

fn mutation_context(workspace_root: &std::path::Path, state_root: &std::path::Path) -> ToolContext {
    let recorder = MutationEventRecorder::new(
        JsonlSessionStore::new(state_root.join("session.jsonl"))
            .expect("session store should build"),
    );
    ToolContext::new(workspace_root.to_path_buf(), 1).with_mutation_recorder(recorder)
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
        server_startup: CodeIntelStartup::Lazy,
        default_timeout_ms: 5_000,
        max_results: 10,
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

#[test]
fn code_intel_tools_expose_permission_subjects_for_file_scoped_calls() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    fs::write(temp.path().join("lib.rs"), "pub fn hello() {}\n").expect("source should write");
    let mut registry = ToolRegistry::new();
    register_code_intelligence_tools(&mut registry, &enabled_config(), temp.path().to_path_buf());
    let ctx = ToolContext::new(temp.path().to_path_buf(), 1);

    for tool_name in ["code_definition", "code_references", "code_diagnostics"] {
        let args_json = if tool_name == "code_diagnostics" {
            json!({ "paths": ["lib.rs"], "max_results": 5 }).to_string()
        } else {
            json!({ "path": "lib.rs", "line": 1, "character": 0 }).to_string()
        };
        let subjects = registry
            .permission_subjects(
                &ctx,
                &ToolCall {
                    id: format!("call-{tool_name}"),
                    name: tool_name.to_owned(),
                    args_json,
                },
            )
            .expect("subjects should resolve");

        assert_eq!(subjects.len(), 1);
        assert_eq!(subjects[0].original, "lib.rs");
    }
}

#[tokio::test]
async fn code_symbols_tool_returns_bounded_json_envelope() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    fs::write(temp.path().join("lib.rs"), "pub fn hello() {}\n").expect("source should write");
    let mut registry = ToolRegistry::new();
    register_code_intelligence_tools(&mut registry, &enabled_config(), temp.path().to_path_buf());

    let result = registry
        .execute(
            ToolContext::new(temp.path().to_path_buf(), 1),
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
    )
    .expect("code intelligence should register");

    let result = registry
        .execute(
            ToolContext::new(temp.path().to_path_buf(), 1),
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
            &ToolContext::new(temp.path().to_path_buf(), 1),
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
    register_trusted_code_intelligence_tools(
        &mut registry,
        &tooling_lsp_config(&script),
        temp.path().to_path_buf(),
    );
    let ctx = ToolContext::new(temp.path().to_path_buf(), 1);

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
    register_trusted_code_intelligence_tools(
        &mut registry,
        &fake_tool_lsp_config(&server_script, &scenario_path, 10),
        temp.path().to_path_buf(),
    );

    let result = registry
        .execute(
            ToolContext::new(temp.path().to_path_buf(), 1),
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
            ToolContext::new(temp.path().to_path_buf(), 1),
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
            &ToolContext::new(temp.path().to_path_buf(), 1),
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

#[test]
fn code_action_and_rename_tools_are_registered_as_previewed_write_tools() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    let mut registry = ToolRegistry::new();
    register_code_intelligence_tools(&mut registry, &enabled_config(), temp.path().to_path_buf());

    for name in ["code_action", "code_rename"] {
        let spec = registry.spec_for(name).expect("tool should be registered");
        assert_eq!(spec.access, sigil_kernel::ToolAccess::Write);
        assert_eq!(spec.preview, ToolPreviewCapability::Required);
    }

    let actions = registry
        .spec_for("code_actions")
        .expect("list tool should be registered");
    assert_eq!(actions.access, sigil_kernel::ToolAccess::Read);
    assert_eq!(actions.preview, ToolPreviewCapability::None);
}

#[test]
fn read_only_code_intelligence_tools_declare_no_workspace_mutation_tracking() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    let mut registry = ToolRegistry::new();
    register_code_intelligence_tools(&mut registry, &enabled_config(), temp.path().to_path_buf());
    let contracts = registry
        .contracts()
        .into_iter()
        .map(|(spec, tracking)| (spec.name, tracking))
        .collect::<BTreeMap<_, _>>();

    for name in [
        "code_symbols",
        "code_workspace_symbols",
        "code_definition",
        "code_references",
        "code_diagnostics",
    ] {
        assert_eq!(
            contracts.get(name),
            Some(&ToolMutationTracking::None),
            "{name} should not trigger workspace mutation scans"
        );
    }
}

#[test]
fn code_action_and_rename_permission_subjects_use_expected_scopes() {
    let temp = tempfile::tempdir().expect("tempdir should build");
    fs::write(temp.path().join("lib.rs"), "pub fn hello() {}\n").expect("source should write");
    let mut registry = ToolRegistry::new();
    register_code_intelligence_tools(&mut registry, &enabled_config(), temp.path().to_path_buf());
    let ctx = ToolContext::new(temp.path().to_path_buf(), 1);
    let actions = registry
        .permission_subjects(
            &ctx,
            &ToolCall {
                id: "actions".to_owned(),
                name: "code_actions".to_owned(),
                args_json: json!({ "path": "lib.rs", "line": 1, "character": 7 }).to_string(),
            },
        )
        .expect("code actions subject should resolve");

    assert_eq!(actions[0].normalized, "lib.rs");

    for name in ["code_action", "code_rename"] {
        let subjects = registry
            .permission_subjects(
                &ctx,
                &ToolCall {
                    id: name.to_owned(),
                    name: name.to_owned(),
                    args_json: json!({ "path": "lib.rs", "line": 1, "character": 7 }).to_string(),
                },
            )
            .expect("write code-intel subject should resolve");
        assert_eq!(subjects.len(), 1);
        assert_eq!(subjects[0].normalized, ".");
        assert_eq!(subjects[0].scope, ToolSubjectScope::Workspace);
    }
}

#[tokio::test]
async fn code_actions_tool_returns_lsp_action_summaries() {
    if !python3_available() {
        return;
    }
    let temp = tempfile::tempdir().expect("tempdir should build");
    fs::create_dir(temp.path().join("src")).expect("src dir should build");
    fs::write(temp.path().join("Cargo.toml"), "[package]\nname='x'\n").expect("cargo file");
    fs::write(
        temp.path().join("src").join("lib.rs"),
        "pub fn hello() {}\n",
    )
    .expect("source should write");
    let server_script = temp.path().join("fake_lsp.py");
    let scenario_path = temp.path().join("actions.json");
    write_fake_lsp_server(&server_script);
    write_fake_lsp_scenario(
        &scenario_path,
        &serde_json::json!({
            "methods": {
                "initialize": {
                    "result": {
                        "capabilities": {
                            "codeActionProvider": { "resolveProvider": true }
                        }
                    }
                },
                "textDocument/codeAction": {
                    "result": [{
                        "title": "Replace hello",
                        "kind": "quickfix",
                        "isPreferred": true,
                        "edit": { "changes": {} }
                    }]
                },
                "shutdown": { "result": null }
            }
        }),
    );
    let mut registry = ToolRegistry::new();
    register_trusted_code_intelligence_tools(
        &mut registry,
        &fake_tool_lsp_config(&server_script, &scenario_path, 250),
        temp.path().to_path_buf(),
    );

    let result = registry
        .execute(
            ToolContext::new(temp.path().to_path_buf(), 1),
            ToolCall {
                id: "actions".to_owned(),
                name: "code_actions".to_owned(),
                args_json: json!({ "path": "src/lib.rs", "line": 1, "character": 7 }).to_string(),
            },
        )
        .await
        .expect("code actions should execute");

    assert!(!result.is_error());
    let content: serde_json::Value =
        serde_json::from_str(&result.content).expect("content should be json");
    assert_eq!(content["code_actions"][0]["title"], "Replace hello");
    assert_eq!(content["code_actions"][0]["has_edit"], true);
}

#[tokio::test]
async fn code_rename_tool_previews_and_applies_workspace_edit() {
    if !python3_available() {
        return;
    }
    let temp = tempfile::tempdir().expect("tempdir should build");
    fs::create_dir(temp.path().join("src")).expect("src dir should build");
    fs::write(temp.path().join("Cargo.toml"), "[package]\nname='x'\n").expect("cargo file");
    let source_path = temp.path().join("src").join("lib.rs");
    fs::write(
        &source_path,
        "pub fn hello() {}\npub fn call() { hello(); }\n",
    )
    .expect("source should write");
    let source_uri = file_uri_from_path(&source_path);
    let server_script = temp.path().join("fake_lsp.py");
    let scenario_path = temp.path().join("rename.json");
    write_fake_lsp_server(&server_script);
    write_fake_lsp_scenario(
        &scenario_path,
        &serde_json::json!({
            "methods": {
                "initialize": {
                    "result": { "capabilities": { "renameProvider": true } }
                },
                "textDocument/rename": {
                    "result": {
                        "changes": {
                            source_uri: [
                                {
                                    "range": {
                                        "start": { "line": 0, "character": 7 },
                                        "end": { "line": 0, "character": 12 }
                                    },
                                    "newText": "greet"
                                },
                                {
                                    "range": {
                                        "start": { "line": 1, "character": 16 },
                                        "end": { "line": 1, "character": 21 }
                                    },
                                    "newText": "greet"
                                }
                            ]
                        }
                    }
                },
                "shutdown": { "result": null }
            }
        }),
    );
    let mut registry = ToolRegistry::new();
    let service = register_trusted_code_intelligence_tools(
        &mut registry,
        &fake_tool_lsp_config(&server_script, &scenario_path, 250),
        temp.path().to_path_buf(),
    )
    .expect("code intelligence should register");
    let state = tempfile::tempdir().expect("state tempdir should build");
    let ctx = mutation_context(temp.path(), state.path());
    let call = ToolCall {
        id: "rename".to_owned(),
        name: "code_rename".to_owned(),
        args_json: json!({
            "path": "src/lib.rs",
            "line": 1,
            "character": 7,
            "new_name": "greet"
        })
        .to_string(),
    };

    let (prepared, subjects) = prepare_mutation_for_test(&service, ctx.clone(), &call)
        .await
        .expect("prepared mutation should build");
    let preview = prepared.preview("Rename symbol");
    assert_eq!(preview.changed_files, vec!["src/lib.rs"]);
    assert!(preview.file_diffs[0].diff.contains("+pub fn greet()"));
    assert_eq!(subjects[0].normalized, "src/lib.rs");
    let result = execute_mutation_for_test(ctx.clone(), &call, "sha256:test-policy", prepared)
        .await
        .expect("rename should execute");

    assert!(!result.is_error());
    assert_eq!(
        fs::read_to_string(&source_path).expect("source should read"),
        "pub fn greet() {}\npub fn call() { greet(); }\n"
    );
    assert_eq!(result.metadata.changed_files, vec!["src/lib.rs"]);
}

#[tokio::test]
async fn code_action_tool_previews_and_applies_selected_edit() {
    if !python3_available() {
        return;
    }
    let temp = tempfile::tempdir().expect("tempdir should build");
    fs::create_dir(temp.path().join("src")).expect("src dir should build");
    fs::write(temp.path().join("Cargo.toml"), "[package]\nname='x'\n").expect("cargo file");
    let source_path = temp.path().join("src").join("lib.rs");
    fs::write(&source_path, "pub fn hello() {}\n").expect("source should write");
    let source_uri = file_uri_from_path(&source_path);
    let server_script = temp.path().join("fake_lsp.py");
    let scenario_path = temp.path().join("code-action.json");
    write_fake_lsp_server(&server_script);
    write_fake_lsp_scenario(
        &scenario_path,
        &serde_json::json!({
            "methods": {
                "initialize": {
                    "result": {
                        "capabilities": {
                            "codeActionProvider": { "resolveProvider": false }
                        }
                    }
                },
                "textDocument/codeAction": {
                    "result": [{
                        "title": "Make public greeting explicit",
                        "kind": "quickfix",
                        "edit": {
                            "changes": {
                                source_uri: [{
                                    "range": {
                                        "start": { "line": 0, "character": 7 },
                                        "end": { "line": 0, "character": 12 }
                                    },
                                    "newText": "greet"
                                }]
                            }
                        }
                    }]
                },
                "shutdown": { "result": null }
            }
        }),
    );
    let mut registry = ToolRegistry::new();
    let service = register_trusted_code_intelligence_tools(
        &mut registry,
        &fake_tool_lsp_config(&server_script, &scenario_path, 250),
        temp.path().to_path_buf(),
    )
    .expect("code intelligence should register");
    let state = tempfile::tempdir().expect("state tempdir should build");
    let ctx = mutation_context(temp.path(), state.path());
    let call = ToolCall {
        id: "action".to_owned(),
        name: "code_action".to_owned(),
        args_json: json!({
            "path": "src/lib.rs",
            "line": 1,
            "character": 7,
            "title": "Make public greeting explicit"
        })
        .to_string(),
    };

    let (prepared, _subjects) = prepare_mutation_for_test(&service, ctx.clone(), &call)
        .await
        .expect("prepared mutation should build");
    let preview = prepared.preview("Apply code action");
    assert!(preview.file_diffs[0].diff.contains("+pub fn greet()"));
    let result = execute_mutation_for_test(ctx, &call, "sha256:test-policy", prepared)
        .await
        .expect("action should execute");

    assert!(!result.is_error());
    assert_eq!(
        fs::read_to_string(&source_path).expect("source should read"),
        "pub fn greet() {}\n"
    );
}

#[tokio::test]
async fn approved_mutation_consumes_first_lsp_plan_without_second_request() {
    if !python3_available() {
        return;
    }
    let workspace = tempfile::tempdir().expect("workspace tempdir should build");
    let state = tempfile::tempdir().expect("state tempdir should build");
    fs::create_dir(workspace.path().join("src")).expect("src dir should build");
    fs::write(workspace.path().join("Cargo.toml"), "[package]\nname='x'\n")
        .expect("cargo file should write");
    let source_path = workspace.path().join("src/lib.rs");
    fs::write(&source_path, "pub fn hello() {}\n").expect("source should write");
    let source_uri = file_uri_from_path(&source_path);
    let server_script = workspace.path().join("fake_lsp.py");
    let scenario_path = workspace.path().join("scenario.json");
    let record_path = state.path().join("lsp-record.json");
    write_fake_lsp_server(&server_script);
    write_fake_lsp_scenario(
        &scenario_path,
        &json!({
            "record_file": record_path,
            "methods": {
                "initialize": {
                    "result": { "capabilities": { "renameProvider": true } }
                },
                "textDocument/rename": {
                    "result_sequence": [
                        { "changes": { source_uri.clone(): [{
                            "range": {
                                "start": { "line": 0, "character": 7 },
                                "end": { "line": 0, "character": 12 }
                            },
                            "newText": "approved"
                        }] } },
                        { "changes": { source_uri: [{
                            "range": {
                                "start": { "line": 0, "character": 7 },
                                "end": { "line": 0, "character": 12 }
                            },
                            "newText": "unapproved"
                        }] } }
                    ]
                },
                "shutdown": { "result": null }
            }
        }),
    );
    let mut registry = ToolRegistry::new();
    let service = register_trusted_code_intelligence_tools(
        &mut registry,
        &fake_tool_lsp_config(&server_script, &scenario_path, 2_000),
        workspace.path().to_path_buf(),
    )
    .expect("code intelligence should register");
    let ctx = mutation_context(workspace.path(), state.path());
    let call = ToolCall {
        id: "approved-once".to_owned(),
        name: "code_rename".to_owned(),
        args_json: json!({
            "path": "src/lib.rs",
            "line": 1,
            "character": 7,
            "new_name": "approved"
        })
        .to_string(),
    };

    let (prepared, subjects) = prepare_mutation_for_test(&service, ctx.clone(), &call)
        .await
        .expect("prepare should succeed");
    assert_eq!(subjects[0].normalized, "src/lib.rs");
    let result = execute_mutation_for_test(ctx, &call, "sha256:policy-once", prepared)
        .await
        .expect("prepared execute should return");
    assert!(!result.is_error());
    assert_eq!(
        fs::read_to_string(&source_path).expect("source should read"),
        "pub fn approved() {}\n"
    );

    service.shutdown().await.expect("service should shut down");
    let record: serde_json::Value = serde_json::from_slice(
        &fs::read(&record_path).expect("LSP record should exist after shutdown"),
    )
    .expect("LSP record should decode");
    let rename_requests = record["messages"]
        .as_array()
        .expect("messages should be an array")
        .iter()
        .filter(|message| message["method"] == "textDocument/rename")
        .count();
    assert_eq!(rename_requests, 1);
}

#[tokio::test]
async fn approved_mutation_rejects_source_drift_with_zero_mutation() {
    if !python3_available() {
        return;
    }
    let workspace = tempfile::tempdir().expect("workspace tempdir should build");
    let state = tempfile::tempdir().expect("state tempdir should build");
    fs::create_dir(workspace.path().join("src")).expect("src dir should build");
    fs::write(workspace.path().join("Cargo.toml"), "[package]\nname='x'\n")
        .expect("cargo file should write");
    let source_path = workspace.path().join("src/lib.rs");
    fs::write(&source_path, "pub fn hello() {}\n").expect("source should write");
    let source_uri = file_uri_from_path(&source_path);
    let server_script = workspace.path().join("fake_lsp.py");
    let scenario_path = workspace.path().join("scenario.json");
    write_fake_lsp_server(&server_script);
    write_fake_lsp_scenario(
        &scenario_path,
        &json!({
            "methods": {
                "initialize": { "result": { "capabilities": { "renameProvider": true } } },
                "textDocument/rename": { "result": { "changes": { source_uri: [{
                    "range": {
                        "start": { "line": 0, "character": 7 },
                        "end": { "line": 0, "character": 12 }
                    },
                    "newText": "greet"
                }] } } },
                "shutdown": { "result": null }
            }
        }),
    );
    let mut registry = ToolRegistry::new();
    let service = register_trusted_code_intelligence_tools(
        &mut registry,
        &fake_tool_lsp_config(&server_script, &scenario_path, 250),
        workspace.path().to_path_buf(),
    )
    .expect("code intelligence should register");
    let ctx = mutation_context(workspace.path(), state.path());
    let call = ToolCall {
        id: "stale-source".to_owned(),
        name: "code_rename".to_owned(),
        args_json: json!({
            "path": "src/lib.rs",
            "line": 1,
            "character": 7,
            "new_name": "greet"
        })
        .to_string(),
    };
    let (prepared, _subjects) = prepare_mutation_for_test(&service, ctx.clone(), &call)
        .await
        .expect("prepare should succeed");
    fs::write(&source_path, "pub fn externally_changed() {}\n")
        .expect("external change should write");

    let result = execute_mutation_for_test(ctx.clone(), &call, "sha256:policy-stale", prepared)
        .await
        .expect("stale execute should return");
    assert_eq!(
        result.summary().error_kind,
        Some(ToolErrorKind::StalePreparedMutation)
    );
    assert_eq!(
        fs::read_to_string(&source_path).expect("source should read"),
        "pub fn externally_changed() {}\n"
    );
    let durable = fs::read_to_string(state.path().join("session.jsonl")).unwrap_or_default();
    assert!(!durable.contains("mutation_batch_started"));

    fs::write(&source_path, "pub fn hello() {}\n").expect("source should restore");
    let (prepared, _subjects) = prepare_mutation_for_test(&service, ctx.clone(), &call)
        .await
        .expect("second prepare should succeed");
    let recorder = ctx
        .mutation_recorder
        .clone()
        .expect("test context should have recorder");
    write_file_with_mutation(
        Some(&recorder),
        workspace.path(),
        "other-controlled-write",
        "other.rs",
        workspace.path().join("other.rs"),
        b"pub fn other() {}\n",
    )
    .expect("other controlled write should succeed");
    let result =
        execute_mutation_for_test(ctx, &call, "sha256:policy-workspace-revision", prepared)
            .await
            .expect("revision-stale execute should return");
    assert_eq!(
        result.summary().error_kind,
        Some(ToolErrorKind::StalePreparedMutation)
    );
    assert_eq!(
        result.metadata.details["prepared_mutation_result"]["reason"],
        "workspace_mutation_epoch_changed"
    );
    assert_eq!(
        fs::read_to_string(&source_path).expect("source should read"),
        "pub fn hello() {}\n"
    );
}

#[tokio::test]
async fn approved_mutation_requires_durable_recorder_before_write() {
    if !python3_available() {
        return;
    }
    let workspace = tempfile::tempdir().expect("workspace tempdir should build");
    fs::create_dir(workspace.path().join("src")).expect("src dir should build");
    fs::write(workspace.path().join("Cargo.toml"), "[package]\nname='x'\n")
        .expect("cargo file should write");
    let source_path = workspace.path().join("src/lib.rs");
    fs::write(&source_path, "pub fn hello() {}\n").expect("source should write");
    let source_uri = file_uri_from_path(&source_path);
    let server_script = workspace.path().join("fake_lsp.py");
    let scenario_path = workspace.path().join("scenario.json");
    write_fake_lsp_server(&server_script);
    write_fake_lsp_scenario(
        &scenario_path,
        &json!({
            "methods": {
                "initialize": { "result": { "capabilities": { "renameProvider": true } } },
                "textDocument/rename": { "result": { "changes": { source_uri: [{
                    "range": {
                        "start": { "line": 0, "character": 7 },
                        "end": { "line": 0, "character": 12 }
                    },
                    "newText": "greet"
                }] } } },
                "shutdown": { "result": null }
            }
        }),
    );
    let mut registry = ToolRegistry::new();
    let service = register_trusted_code_intelligence_tools(
        &mut registry,
        &fake_tool_lsp_config(&server_script, &scenario_path, 250),
        workspace.path().to_path_buf(),
    )
    .expect("code intelligence should register");
    let ctx = ToolContext::new(workspace.path().to_path_buf(), 1);
    let call = ToolCall {
        id: "no-recorder".to_owned(),
        name: "code_rename".to_owned(),
        args_json: json!({
            "path": "src/lib.rs",
            "line": 1,
            "character": 7,
            "new_name": "greet"
        })
        .to_string(),
    };
    let (prepared, _subjects) = prepare_mutation_for_test(&service, ctx.clone(), &call)
        .await
        .expect("prepare should succeed");
    let result = execute_mutation_for_test(ctx, &call, "sha256:no-recorder-policy", prepared)
        .await
        .expect("execute should return typed failure");
    assert_eq!(
        result.summary().error_kind,
        Some(ToolErrorKind::DurabilityRequired)
    );
    assert_eq!(
        fs::read_to_string(&source_path).expect("source should read"),
        "pub fn hello() {}\n"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn approved_mutation_rolls_back_first_file_when_second_apply_fails() {
    use std::os::unix::fs::PermissionsExt;

    if !python3_available() {
        return;
    }
    let workspace = tempfile::tempdir().expect("workspace tempdir should build");
    let state = tempfile::tempdir().expect("state tempdir should build");
    fs::create_dir(workspace.path().join("src")).expect("src dir should build");
    fs::create_dir(workspace.path().join("zzz_locked")).expect("locked dir should build");
    fs::write(workspace.path().join("Cargo.toml"), "[package]\nname='x'\n")
        .expect("cargo file should write");
    let first_path = workspace.path().join("src/a.rs");
    let second_path = workspace.path().join("zzz_locked/b.rs");
    fs::write(&first_path, "pub fn hello() {}\n").expect("first source should write");
    fs::write(&second_path, "pub fn call() { hello(); }\n").expect("second source should write");
    let mut executable_permissions = fs::metadata(&first_path)
        .expect("first source metadata should read")
        .permissions();
    executable_permissions.set_mode(0o755);
    fs::set_permissions(&first_path, executable_permissions)
        .expect("first source should become executable");
    let first_uri = file_uri_from_path(&first_path);
    let second_uri = file_uri_from_path(&second_path);
    let server_script = workspace.path().join("fake_lsp.py");
    let scenario_path = workspace.path().join("scenario.json");
    write_fake_lsp_server(&server_script);
    write_fake_lsp_scenario(
        &scenario_path,
        &json!({
            "methods": {
                "initialize": { "result": { "capabilities": { "renameProvider": true } } },
                "textDocument/rename": { "result": { "changes": {
                    first_uri: [{
                        "range": {
                            "start": { "line": 0, "character": 7 },
                            "end": { "line": 0, "character": 12 }
                        },
                        "newText": "greet"
                    }],
                    second_uri: [{
                        "range": {
                            "start": { "line": 0, "character": 16 },
                            "end": { "line": 0, "character": 21 }
                        },
                        "newText": "greet"
                    }]
                } } },
                "shutdown": { "result": null }
            }
        }),
    );
    let mut registry = ToolRegistry::new();
    let service = register_trusted_code_intelligence_tools(
        &mut registry,
        &fake_tool_lsp_config(&server_script, &scenario_path, 250),
        workspace.path().to_path_buf(),
    )
    .expect("code intelligence should register");
    let ctx = mutation_context(workspace.path(), state.path());
    let call = ToolCall {
        id: "rollback-second-file".to_owned(),
        name: "code_rename".to_owned(),
        args_json: json!({
            "path": "src/a.rs",
            "line": 1,
            "character": 7,
            "new_name": "greet"
        })
        .to_string(),
    };
    let (prepared, _subjects) = prepare_mutation_for_test(&service, ctx.clone(), &call)
        .await
        .expect("prepare should succeed");
    let mut locked_permissions = fs::metadata(workspace.path().join("zzz_locked"))
        .expect("locked dir metadata should read")
        .permissions();
    locked_permissions.set_mode(0o555);
    fs::set_permissions(workspace.path().join("zzz_locked"), locked_permissions)
        .expect("locked dir should become read-only");

    let result = execute_mutation_for_test(ctx, &call, "sha256:rollback-policy", prepared)
        .await
        .expect("execute should return rollback result");
    let mut restored_permissions = fs::metadata(workspace.path().join("zzz_locked"))
        .expect("locked dir metadata should read")
        .permissions();
    restored_permissions.set_mode(0o755);
    fs::set_permissions(workspace.path().join("zzz_locked"), restored_permissions)
        .expect("locked dir permissions should restore");

    assert_eq!(result.summary().error_kind, Some(ToolErrorKind::Io));
    assert_eq!(
        result.metadata.details["prepared_mutation_result"]["status"],
        "rolled_back"
    );
    assert_eq!(
        fs::read_to_string(&first_path).expect("first source should read"),
        "pub fn hello() {}\n"
    );
    assert_eq!(
        fs::read_to_string(&second_path).expect("second source should read"),
        "pub fn call() { hello(); }\n"
    );
    assert_eq!(
        fs::metadata(&first_path)
            .expect("first source metadata should read")
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
    let durable =
        fs::read_to_string(state.path().join("session.jsonl")).expect("session log should read");
    assert!(durable.contains("\"status\":\"rolled_back\""));
    assert!(durable.contains("sha256:rollback-policy"));
    let records = JsonlSessionStore::read_event_records(state.path().join("session.jsonl"))
        .expect("durable records should decode");
    let prepared_ids = records
        .iter()
        .filter_map(|record| match record {
            sigil_kernel::SessionStreamRecord::Stored(event)
                if event.event_type == "mutation_prepared" =>
            {
                event.payload["operation_id"].as_str().map(str::to_owned)
            }
            _ => None,
        })
        .collect::<std::collections::BTreeSet<_>>();
    let terminal_ids = records
        .iter()
        .filter_map(|record| match record {
            sigil_kernel::SessionStreamRecord::Stored(event)
                if matches!(
                    event.event_type.as_str(),
                    "mutation_committed" | "mutation_reconciled"
                ) =>
            {
                event.payload["operation_id"].as_str().map(str::to_owned)
            }
            _ => None,
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert!(prepared_ids.is_subset(&terminal_ids));
}

#[tokio::test]
async fn approved_mutation_records_residual_revision_when_rollback_fails() {
    if !python3_available() {
        return;
    }
    let workspace = tempfile::tempdir().expect("workspace tempdir should build");
    let state = tempfile::tempdir().expect("state tempdir should build");
    fs::create_dir(workspace.path().join("src")).expect("src dir should build");
    fs::write(workspace.path().join("Cargo.toml"), "[package]\nname='x'\n")
        .expect("cargo file should write");
    let first_path = workspace.path().join("src/a.rs");
    let second_path = workspace.path().join("src/b.rs");
    fs::write(&first_path, "pub fn hello() {}\n").expect("first source should write");
    fs::write(&second_path, "pub fn call() { hello(); }\n").expect("second source should write");
    let first_uri = file_uri_from_path(&first_path);
    let second_uri = file_uri_from_path(&second_path);
    let server_script = workspace.path().join("fake_lsp.py");
    let scenario_path = workspace.path().join("scenario.json");
    write_fake_lsp_server(&server_script);
    write_fake_lsp_scenario(
        &scenario_path,
        &json!({
            "methods": {
                "initialize": { "result": { "capabilities": { "renameProvider": true } } },
                "textDocument/rename": { "result": { "changes": {
                    first_uri: [{
                        "range": {
                            "start": { "line": 0, "character": 7 },
                            "end": { "line": 0, "character": 12 }
                        },
                        "newText": "greet"
                    }],
                    second_uri: [{
                        "range": {
                            "start": { "line": 0, "character": 16 },
                            "end": { "line": 0, "character": 21 }
                        },
                        "newText": "greet"
                    }]
                } } },
                "shutdown": { "result": null }
            }
        }),
    );
    let mut registry = ToolRegistry::new();
    let service = register_trusted_code_intelligence_tools(
        &mut registry,
        &fake_tool_lsp_config(&server_script, &scenario_path, 250),
        workspace.path().to_path_buf(),
    )
    .expect("code intelligence should register");
    let cancellation_owner = sigil_kernel::RunCancellationOwner::new();
    let ctx = mutation_context(workspace.path(), state.path())
        .with_cancellation(cancellation_owner.handle());
    let recorder = ctx
        .mutation_recorder
        .clone()
        .expect("test context should have recorder");
    recorder.inject_commit_write_fault_for_test(2, true);
    recorder.inject_commit_write_fault_for_test(3, false);
    let call = ToolCall {
        id: "rollback-failure".to_owned(),
        name: "code_rename".to_owned(),
        args_json: json!({
            "path": "src/a.rs",
            "line": 1,
            "character": 7,
            "new_name": "greet"
        })
        .to_string(),
    };
    let (prepared, _subjects) = prepare_mutation_for_test(&service, ctx.clone(), &call)
        .await
        .expect("prepare should succeed");
    let result = execute_mutation_for_test(ctx, &call, "sha256:rollback-failure-policy", prepared)
        .await
        .expect("execute should return rollback failure");

    assert_eq!(result.summary().error_kind, Some(ToolErrorKind::Io));
    assert_eq!(
        result.metadata.details["prepared_mutation_result"]["status"],
        "rollback_failed"
    );
    assert!(!cancellation_owner.cleanup_complete());
    assert_eq!(
        fs::read_to_string(&first_path).expect("first source should read"),
        "pub fn hello() {}\n"
    );
    assert_eq!(
        fs::read_to_string(&second_path).expect("second source should read"),
        "pub fn call() { greet(); }\n"
    );
    let records = JsonlSessionStore::read_event_records(state.path().join("session.jsonl"))
        .expect("durable records should decode");
    let residual_reconcile = records.iter().find_map(|record| match record {
        sigil_kernel::SessionStreamRecord::Stored(event)
            if event.event_type == "mutation_reconciled"
                && event.payload["resolution"] == "mark_committed" =>
        {
            Some(&event.payload)
        }
        _ => None,
    });
    let residual_reconcile = residual_reconcile.expect("residual write should be reconciled");
    assert!(residual_reconcile["workspace_revision"].as_u64().is_some());
    assert!(
        residual_reconcile["workspace_snapshot_id"]
            .as_str()
            .is_some_and(|snapshot| !snapshot.is_empty())
    );
    assert!(records.iter().any(|record| matches!(
        record,
        sigil_kernel::SessionStreamRecord::Stored(event)
            if event.event_type == "mutation_batch_finished"
                && event.payload["status"] == "rollback_failed"
                && event.payload["rollback_failed_operations"]
                    .as_array()
                    .is_some_and(|operations| !operations.is_empty())
    )));
    assert!(
        recorder
            .current_workspace_revision(workspace.path())
            .expect("workspace revision should read")
            >= 2
    );
}

#[tokio::test]
async fn approved_mutation_treats_reconciled_reverse_write_as_rolled_back() {
    if !python3_available() {
        return;
    }
    let workspace = tempfile::tempdir().expect("workspace tempdir should build");
    let state = tempfile::tempdir().expect("state tempdir should build");
    fs::create_dir(workspace.path().join("src")).expect("src dir should build");
    fs::write(workspace.path().join("Cargo.toml"), "[package]\nname='x'\n")
        .expect("cargo file should write");
    let source_path = workspace.path().join("src/lib.rs");
    fs::write(&source_path, "pub fn hello() {}\n").expect("source should write");
    let source_uri = file_uri_from_path(&source_path);
    let server_script = workspace.path().join("fake_lsp.py");
    let scenario_path = workspace.path().join("scenario.json");
    write_fake_lsp_server(&server_script);
    write_fake_lsp_scenario(
        &scenario_path,
        &json!({
            "methods": {
                "initialize": { "result": { "capabilities": { "renameProvider": true } } },
                "textDocument/rename": { "result": { "changes": { source_uri: [{
                    "range": {
                        "start": { "line": 0, "character": 7 },
                        "end": { "line": 0, "character": 12 }
                    },
                    "newText": "greet"
                }] } } },
                "shutdown": { "result": null }
            }
        }),
    );
    let mut registry = ToolRegistry::new();
    let service = register_trusted_code_intelligence_tools(
        &mut registry,
        &fake_tool_lsp_config(&server_script, &scenario_path, 250),
        workspace.path().to_path_buf(),
    )
    .expect("code intelligence should register");
    let ctx = mutation_context(workspace.path(), state.path());
    let recorder = ctx
        .mutation_recorder
        .clone()
        .expect("test context should have recorder");
    recorder.inject_commit_write_fault_for_test(1, true);
    recorder.inject_commit_write_fault_for_test(2, true);
    let call = ToolCall {
        id: "rollback-reconciled".to_owned(),
        name: "code_rename".to_owned(),
        args_json: json!({
            "path": "src/lib.rs",
            "line": 1,
            "character": 7,
            "new_name": "greet"
        })
        .to_string(),
    };
    let (prepared, _subjects) = prepare_mutation_for_test(&service, ctx.clone(), &call)
        .await
        .expect("prepare should succeed");
    let result =
        execute_mutation_for_test(ctx, &call, "sha256:rollback-reconciled-policy", prepared)
            .await
            .expect("execute should return reconciled rollback");

    assert_eq!(result.summary().error_kind, Some(ToolErrorKind::Io));
    assert_eq!(
        result.metadata.details["prepared_mutation_result"]["status"],
        "rolled_back"
    );
    assert_eq!(
        result.metadata.details["prepared_mutation_result"]["residual_files"],
        json!([])
    );
    assert_eq!(
        fs::read_to_string(&source_path).expect("source should read"),
        "pub fn hello() {}\n"
    );
    let records = JsonlSessionStore::read_event_records(state.path().join("session.jsonl"))
        .expect("durable records should decode");
    assert!(records.iter().any(|record| matches!(
        record,
        sigil_kernel::SessionStreamRecord::Stored(event)
            if event.event_type == "mutation_batch_finished"
                && event.payload["status"] == "rolled_back"
                && event.payload
                    .get("rollback_failed_operations")
                    .is_none_or(|operations| operations.as_array().is_some_and(Vec::is_empty))
    )));
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
    register_trusted_code_intelligence_tools(
        &mut registry,
        &fake_tool_lsp_config(&server_script, &scenario_path, 250),
        temp.path().to_path_buf(),
    );

    let result = registry
        .execute(
            ToolContext::new(temp.path().to_path_buf(), 1),
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
