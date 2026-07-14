#![allow(dead_code)]

use std::{collections::BTreeMap, fs, path::Path};

use serde_json::Value;
use sigil_kernel::{CodeIntelStartup, CodeIntelligenceConfig, LanguageServerConfig};

pub fn python3_available() -> bool {
    std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_ok()
}

pub fn write_fake_lsp_server(path: &Path) {
    fs::write(
        path,
        r#"
import json
import os
import sys
import time


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


def maybe_record(record_file, record_env, messages):
    if not record_file:
        return
    payload = {
        "cwd": os.getcwd(),
        "env": {key: os.environ.get(key) for key in record_env},
        "messages": messages,
    }
    temp_file = f"{record_file}.{os.getpid()}.tmp"
    try:
        with open(temp_file, "w", encoding="utf-8") as handle:
            json.dump(payload, handle)
        os.replace(temp_file, record_file)
    finally:
        if os.path.exists(temp_file):
            os.remove(temp_file)


def run_scenario_mode(scenario_path):
    with open(scenario_path, "r", encoding="utf-8") as handle:
        scenario = json.load(handle)

    methods = scenario.get("methods", {})
    notifications = scenario.get("notifications", {})
    record_file = scenario.get("record_file")
    record_env = scenario.get("record_env", [])
    messages = []
    method_counts = {}

    try:
        while True:
            message = read_message()
            if message is None:
                break
            method = message.get("method")
            request_id = message.get("id")
            messages.append(
                {
                    "method": method,
                    "id": request_id,
                    "params": message.get("params"),
                }
            )
            maybe_record(record_file, record_env, messages)

            notification_behavior = notifications.get(method, {})
            publish = notification_behavior.get("publish_diagnostics")
            if publish is not None:
                write_message(
                    {
                        "jsonrpc": "2.0",
                        "method": "textDocument/publishDiagnostics",
                        "params": publish,
                    }
                )

            if request_id is None:
                if method == "exit":
                    break
                continue

            behavior = methods.get(method, {})
            call_count = method_counts.get(method, 0)
            method_counts[method] = call_count + 1
            sleep_ms = behavior.get("sleep_ms")
            if sleep_ms:
                time.sleep(float(sleep_ms) / 1000.0)
            if "raw" in behavior:
                write_raw(behavior["raw"].encode("utf-8"))
                continue
            if "error" in behavior:
                write_message(
                    {
                        "jsonrpc": "2.0",
                        "id": request_id,
                        "error": {"code": -32603, "message": behavior["error"]},
                    }
                )
                continue

            if "result_sequence" in behavior:
                sequence = behavior["result_sequence"]
                result = sequence[min(call_count, len(sequence) - 1)] if sequence else None
            else:
                result = behavior.get("result")
            write_message({"jsonrpc": "2.0", "id": request_id, "result": result})
    except BrokenPipeError:
        pass
    finally:
        maybe_record(record_file, record_env, messages)


def bump_counter(counter_file):
    if not counter_file:
        return
    current = 0
    if os.path.exists(counter_file):
        with open(counter_file, "r", encoding="utf-8") as handle:
            text = handle.read().strip()
            if text:
                current = int(text)
    with open(counter_file, "w", encoding="utf-8") as handle:
        handle.write(str(current + 1))


def run_legacy_env_mode():
    mode = os.environ.get("SIGIL_FAKE_LSP_MODE", "document_symbols_success")
    counter_file = os.environ.get("SIGIL_FAKE_LSP_COUNTER_FILE")
    valid_path = os.environ.get("SIGIL_FAKE_LSP_VALID_PATH")
    external_path = os.environ.get("SIGIL_FAKE_LSP_EXTERNAL_PATH")

    while True:
        message = read_message()
        if message is None:
            break
        method = message.get("method")
        request_id = message.get("id")
        if method == "initialize":
            if mode == "initialize_malformed":
                write_raw(b"Content-Length: 1\r\n\r\n{")
                continue
            capabilities = {}
            if mode in ("document_symbols_success", "document_symbols_malformed"):
                capabilities["documentSymbolProvider"] = True
            elif mode == "workspace_symbols_mixed":
                capabilities["workspaceSymbolProvider"] = True
            elif mode == "diagnostics_publish_only":
                capabilities["diagnosticProvider"] = True
            elif mode == "definition_timeout":
                capabilities["definitionProvider"] = True
            write_message({
                "jsonrpc": "2.0",
                "id": request_id,
                "result": {"capabilities": capabilities},
            })
        elif method == "textDocument/documentSymbol":
            bump_counter(counter_file)
            if mode == "document_symbols_malformed":
                write_raw(b"Content-Length: 7\r\n\r\nnotjson")
                continue
            query = (
                message.get("params", {})
                .get("textDocument", {})
                .get("uri", "hello")
            )
            name = "goodbye" if "goodbye" in query else "hello"
            write_message({
                "jsonrpc": "2.0",
                "id": request_id,
                "result": [{
                    "name": name,
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
                        "uri": file_uri(valid_path),
                        "range": {
                            "start": {"line": 0, "character": 0},
                            "end": {"line": 0, "character": 5},
                        },
                    },
                }, {
                    "name": "outside",
                    "kind": 12,
                    "location": {
                        "uri": file_uri(external_path),
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
                    "uri": file_uri(valid_path),
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
        elif method == "textDocument/didOpen" and mode == "diagnostics_publish_only":
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


if len(sys.argv) > 1:
    run_scenario_mode(sys.argv[1])
else:
    run_legacy_env_mode()
"#,
    )
    .expect("fake LSP server script should write");
}

pub fn write_fake_lsp_scenario(path: &Path, scenario: &Value) {
    fs::write(
        path,
        serde_json::to_vec(scenario).expect("scenario should serialize"),
    )
    .expect("fake LSP scenario should write");
}

pub fn fake_lsp_server_config(script_path: &Path, scenario_path: &Path) -> CodeIntelligenceConfig {
    CodeIntelligenceConfig {
        enabled: true,
        server_startup: CodeIntelStartup::Lazy,
        default_timeout_ms: 250,
        max_results: 20,
        max_payload_bytes: 64 * 1024,
        auto_discover: false,
        report_missing: true,
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
    }
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
        server_startup: CodeIntelStartup::Lazy,
        default_timeout_ms,
        max_results: 20,
        max_payload_bytes: 64 * 1024,
        auto_discover: false,
        report_missing: true,
        servers: vec![LanguageServerConfig {
            name: "rust-analyzer".to_owned(),
            languages: vec!["rust".to_owned()],
            command: "python3".to_owned(),
            args: vec![script_path.to_string_lossy().to_string()],
            env,
            root_markers: vec!["Cargo.toml".to_owned()],
            file_extensions: vec!["rs".to_owned()],
            initialization_options: Value::Null,
            trust_required: true,
            startup_timeout_ms: 5_000,
        }],
    }
}

pub fn fake_server(
    name: &str,
    languages: &[&str],
    file_extensions: &[&str],
    root_markers: &[&str],
    script_path: &Path,
    scenario_path: &Path,
    env: BTreeMap<String, String>,
    startup_timeout_ms: u64,
) -> LanguageServerConfig {
    LanguageServerConfig {
        name: name.to_owned(),
        languages: languages.iter().map(|value| (*value).to_owned()).collect(),
        command: "python3".to_owned(),
        args: vec![
            script_path.to_string_lossy().to_string(),
            scenario_path.to_string_lossy().to_string(),
        ],
        env,
        root_markers: root_markers
            .iter()
            .map(|value| (*value).to_owned())
            .collect(),
        file_extensions: file_extensions
            .iter()
            .map(|value| (*value).to_owned())
            .collect(),
        initialization_options: Value::Null,
        trust_required: true,
        startup_timeout_ms,
    }
}

pub fn read_counter(path: &Path) -> usize {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| text.trim().parse::<usize>().ok())
        .unwrap_or(0)
}
