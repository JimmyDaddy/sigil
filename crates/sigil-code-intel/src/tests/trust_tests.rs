use std::{fs, path::Path};

use sigil_kernel::{
    CodeIntelStartup, CodeIntelligenceConfig, LanguageServerConfig, ToolAccess, ToolCall,
    ToolContext, ToolErrorKind, ToolPreviewCapability, ToolRegistry, WorkspaceTrust,
};

use super::*;
use crate::register_code_intelligence_tools_with_workspace_trust;

#[tokio::test]
async fn trust_required_blocks_unknown_restricted_and_denied_before_spawn() {
    for startup in [CodeIntelStartup::Lazy, CodeIntelStartup::Eager] {
        for trust in [
            WorkspaceTrust::Unknown,
            WorkspaceTrust::Restricted,
            WorkspaceTrust::Denied,
        ] {
            let temp = tempfile::tempdir().expect("workspace tempdir");
            let sentinel = prepare_workspace(temp.path());
            let service = CodeIntelligenceService::new_with_workspace_trust(
                temp.path().to_path_buf(),
                sentinel_config(temp.path(), startup, true),
                trust,
            );

            let error = match service.ensure_client_by_name("sentinel-lsp").await {
                Ok(_) => panic!("untrusted workspace must block LSP startup"),
                Err(error) => error,
            };

            assert!(matches!(
                error.downcast_ref::<CodeIntelError>(),
                Some(CodeIntelError::WorkspaceTrustRequired { server })
                    if server == "sentinel-lsp"
            ));
            assert!(!error.to_string().contains("is unavailable"));
            assert!(!sentinel.exists(), "blocked LSP process wrote its sentinel");
        }
    }
}

#[tokio::test]
async fn legacy_service_constructor_defaults_to_unknown_trust() {
    let temp = tempfile::tempdir().expect("workspace tempdir");
    let sentinel = prepare_workspace(temp.path());
    let service = CodeIntelligenceService::new(
        temp.path().to_path_buf(),
        sentinel_config(temp.path(), CodeIntelStartup::Lazy, true),
    );

    let error = match service.ensure_client_by_name("sentinel-lsp").await {
        Ok(_) => panic!("legacy constructor must fail closed"),
        Err(error) => error,
    };

    assert!(matches!(
        error.downcast_ref::<CodeIntelError>(),
        Some(CodeIntelError::WorkspaceTrustRequired { .. })
    ));
    assert!(!sentinel.exists());
}

#[tokio::test]
async fn trusted_workspace_starts_required_server_for_lazy_and_eager_plans() {
    if !python3_available() {
        return;
    }
    for startup in [CodeIntelStartup::Lazy, CodeIntelStartup::Eager] {
        let temp = tempfile::tempdir().expect("workspace tempdir");
        let sentinel = prepare_workspace(temp.path());
        let service = CodeIntelligenceService::new_with_workspace_trust(
            temp.path().to_path_buf(),
            sentinel_config(temp.path(), startup, true),
            WorkspaceTrust::Trusted,
        );

        service
            .ensure_client_by_name("sentinel-lsp")
            .await
            .expect("trusted workspace should start LSP");

        assert!(sentinel.exists(), "started LSP process must write sentinel");
        service.shutdown().await.expect("service should shut down");
    }
}

#[tokio::test]
async fn server_with_disabled_trust_requirement_starts_for_unknown_workspace() {
    if !python3_available() {
        return;
    }
    let temp = tempfile::tempdir().expect("workspace tempdir");
    let sentinel = prepare_workspace(temp.path());
    let service = CodeIntelligenceService::new_with_workspace_trust(
        temp.path().to_path_buf(),
        sentinel_config(temp.path(), CodeIntelStartup::Lazy, false),
        WorkspaceTrust::Unknown,
    );

    service
        .ensure_client_by_name("sentinel-lsp")
        .await
        .expect("trust-optional server should start");

    assert!(sentinel.exists());
    service.shutdown().await.expect("service should shut down");
}

#[tokio::test]
async fn tree_sitter_fallback_remains_available_without_spawning_untrusted_lsp() {
    let temp = tempfile::tempdir().expect("workspace tempdir");
    let sentinel = prepare_workspace(temp.path());
    let service = CodeIntelligenceService::new_with_workspace_trust(
        temp.path().to_path_buf(),
        sentinel_config(temp.path(), CodeIntelStartup::Lazy, true),
        WorkspaceTrust::Unknown,
    );

    let response = service
        .document_symbols("src/lib.rs", Some("hello"), 10)
        .await
        .expect("Tree-sitter fallback should remain available");

    assert_eq!(response.server, "tree-sitter-rust");
    assert_eq!(response.capability, "tree_sitter/document_symbols");
    assert_eq!(response.results.len(), 1);
    assert!(!sentinel.exists());
}

#[tokio::test]
async fn trust_required_tool_error_is_permission_denied_and_write_specs_remain_strong() {
    let temp = tempfile::tempdir().expect("workspace tempdir");
    let sentinel = prepare_workspace(temp.path());
    let mut registry = ToolRegistry::new();
    register_code_intelligence_tools_with_workspace_trust(
        &mut registry,
        &sentinel_config(temp.path(), CodeIntelStartup::Lazy, true),
        temp.path().to_path_buf(),
        WorkspaceTrust::Unknown,
    )
    .expect("code intelligence tools should register");

    let result = registry
        .execute(
            ToolContext::new(temp.path().to_path_buf(), 1),
            ToolCall {
                id: "trust-definition".to_owned(),
                name: "code_definition".to_owned(),
                args_json: serde_json::json!({
                    "path": "src/lib.rs",
                    "line": 1,
                    "character": 0
                })
                .to_string(),
            },
        )
        .await
        .expect("tool execution should return a structured result");

    assert_eq!(
        result.summary().error_kind,
        Some(ToolErrorKind::PermissionDenied)
    );
    assert!(!sentinel.exists());
    for name in ["code_action", "code_rename"] {
        let spec = registry
            .spec_for(name)
            .expect("write tool should remain registered");
        assert_eq!(spec.access, ToolAccess::Write);
        assert_eq!(spec.preview, ToolPreviewCapability::Required);
    }
}

fn prepare_workspace(workspace: &Path) -> std::path::PathBuf {
    fs::create_dir(workspace.join("src")).expect("source directory should build");
    fs::write(
        workspace.join("Cargo.toml"),
        "[package]\nname='trust-test'\n",
    )
    .expect("Cargo manifest should write");
    fs::write(workspace.join("src/lib.rs"), "pub fn hello() {}\n").expect("source should write");
    write_sentinel_lsp_server(&workspace.join("sentinel_lsp.py"));
    workspace.join("lsp-spawned")
}

fn sentinel_config(
    workspace: &Path,
    startup: CodeIntelStartup,
    trust_required: bool,
) -> CodeIntelligenceConfig {
    CodeIntelligenceConfig {
        enabled: true,
        server_startup: startup,
        default_timeout_ms: 1_000,
        max_results: 20,
        max_payload_bytes: 64 * 1024,
        auto_discover: false,
        report_missing: true,
        servers: vec![LanguageServerConfig {
            name: "sentinel-lsp".to_owned(),
            languages: vec!["rust".to_owned()],
            command: "python3".to_owned(),
            args: vec![
                workspace.join("sentinel_lsp.py").display().to_string(),
                workspace.join("lsp-spawned").display().to_string(),
            ],
            env: Default::default(),
            root_markers: vec!["Cargo.toml".to_owned()],
            file_extensions: vec!["rs".to_owned()],
            initialization_options: serde_json::Value::Null,
            trust_required,
            startup_timeout_ms: 2_000,
        }],
    }
}

fn python3_available() -> bool {
    std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn write_sentinel_lsp_server(path: &Path) {
    fs::write(
        path,
        r#"import json
import sys

with open(sys.argv[1], "w", encoding="utf-8") as sentinel:
    sentinel.write("spawned")

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
            "result": {"capabilities": {"definitionProvider": True}},
        })
    elif method == "shutdown":
        write_message({"jsonrpc": "2.0", "id": request_id, "result": None})
    elif method == "exit":
        break
"#,
    )
    .expect("sentinel LSP script should write");
}
