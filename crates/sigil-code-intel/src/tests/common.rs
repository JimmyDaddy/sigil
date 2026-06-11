#![allow(dead_code)]

use std::{collections::BTreeMap, fs, path::Path};

use sigil_kernel::{
    CodeIntelStartup, CodeIntelligenceConfig, CodeIntelligenceDiscoveryConfig, LanguageServerConfig,
};

pub fn python3_available() -> bool {
    std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_ok()
}

pub fn fake_lsp_server_config(
    script_path: &Path,
    mode: &str,
    default_timeout_ms: u64,
) -> CodeIntelligenceConfig {
    fake_lsp_server_config_with_env(script_path, mode, default_timeout_ms, BTreeMap::new())
}

pub fn fake_lsp_server_config_with_env(
    script_path: &Path,
    mode: &str,
    default_timeout_ms: u64,
    mut env: BTreeMap<String, String>,
) -> CodeIntelligenceConfig {
    env.insert("SIGIL_FAKE_LSP_MODE".to_owned(), mode.to_owned());
    CodeIntelligenceConfig {
        enabled: true,
        startup: CodeIntelStartup::Lazy,
        default_timeout_ms,
        max_results: 20,
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
            env,
            root_markers: vec!["Cargo.toml".to_owned()],
            file_extensions: vec!["rs".to_owned()],
            initialization_options: serde_json::Value::Null,
            trust_required: true,
            startup_timeout_ms: 5_000,
        }],
    }
}

pub fn write_fake_lsp_server(path: &Path) {
    fs::write(
        path,
        r#"
import json
import os
import sys
import time


MODE = os.environ.get("SIGIL_FAKE_LSP_MODE", "document_symbols_success")
COUNTER_FILE = os.environ.get("SIGIL_FAKE_LSP_COUNTER_FILE")
VALID_PATH = os.environ.get("SIGIL_FAKE_LSP_VALID_PATH")
EXTERNAL_PATH = os.environ.get("SIGIL_FAKE_LSP_EXTERNAL_PATH")


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


def write_raw(payload):
    sys.stdout.buffer.write(payload)
    sys.stdout.buffer.flush()


def file_uri(path):
    return f"file://{path}"


def bump_counter():
    if not COUNTER_FILE:
        return
    current = 0
    if os.path.exists(COUNTER_FILE):
        with open(COUNTER_FILE, "r", encoding="utf-8") as handle:
            text = handle.read().strip()
            if text:
                current = int(text)
    with open(COUNTER_FILE, "w", encoding="utf-8") as handle:
        handle.write(str(current + 1))


while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    request_id = message.get("id")
    if method == "initialize":
        if MODE == "initialize_malformed":
            write_raw(b"Content-Length: 1\r\n\r\n{")
            continue
        capabilities = {}
        if MODE in ("document_symbols_success", "document_symbols_malformed"):
            capabilities["documentSymbolProvider"] = True
        elif MODE == "workspace_symbols_mixed":
            capabilities["workspaceSymbolProvider"] = True
        elif MODE == "diagnostics_publish_only":
            capabilities["diagnosticProvider"] = True
        elif MODE == "definition_timeout":
            capabilities["definitionProvider"] = True
        write_message({
            "jsonrpc": "2.0",
            "id": request_id,
            "result": {"capabilities": capabilities},
        })
    elif method == "textDocument/documentSymbol":
        bump_counter()
        if MODE == "document_symbols_malformed":
            write_raw(b"Content-Length: 7\r\n\r\nnotjson")
            continue
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
    elif method == "workspace/symbol":
        write_message({
            "jsonrpc": "2.0",
            "id": request_id,
            "result": [{
                "name": "hello",
                "kind": 12,
                "location": {
                    "uri": file_uri(VALID_PATH),
                    "range": {
                        "start": {"line": 0, "character": 0},
                        "end": {"line": 0, "character": 5},
                    },
                },
            }, {
                "name": "outside",
                "kind": 12,
                "location": {
                    "uri": file_uri(EXTERNAL_PATH),
                    "range": {
                        "start": {"line": 0, "character": 0},
                        "end": {"line": 0, "character": 5},
                    },
                },
            }, {
                "name": "broken",
                "kind": 12,
            }],
        })
    elif method == "textDocument/definition":
        time.sleep(0.2)
        write_message({
            "jsonrpc": "2.0",
            "id": request_id,
            "result": [{
                "uri": file_uri(VALID_PATH),
                "range": {
                    "start": {"line": 0, "character": 0},
                    "end": {"line": 0, "character": 5},
                },
            }],
        })
    elif method == "textDocument/diagnostic":
        write_message({
            "jsonrpc": "2.0",
            "id": request_id,
            "result": {"kind": "full", "items": []},
        })
    elif method == "textDocument/didOpen" and MODE == "diagnostics_publish_only":
        uri = message["params"]["textDocument"]["uri"]
        write_message({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": uri,
                "diagnostics": [{
                    "range": {
                        "start": {"line": 0, "character": 0},
                        "end": {"line": 0, "character": 5},
                    },
                    "severity": 2,
                    "message": "from publish diagnostics",
                    "source": "fake-lsp",
                }],
            },
        })
    elif method == "shutdown":
        write_message({"jsonrpc": "2.0", "id": request_id, "result": None})
    elif method == "exit":
        break
"#,
    )
    .expect("fake LSP server script should write");
}

pub fn read_counter(path: &Path) -> usize {
    fs::read_to_string(path)
        .expect("counter file should exist")
        .trim()
        .parse()
        .expect("counter file should contain an integer")
}
