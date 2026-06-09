use std::fs;

use anyhow::Result;
use termquill_kernel::{ToolContext, ToolRegistry};

use super::register_mcp_tools;

fn write_fake_server_script(path: &std::path::Path, body: &str) -> Result<()> {
    fs::write(path, body)?;
    Ok(())
}

#[tokio::test]
async fn registers_and_calls_fake_stdio_tool() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("fake_mcp_server.py");
    write_fake_server_script(
        &script,
        r#"#!/usr/bin/env python3
import json, sys

def read_message():
    headers = {}
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        if line in (b"\r\n", b"\n"):
            break
        key, value = line.decode().split(":", 1)
        headers[key.lower()] = value.strip()
    length = int(headers["content-length"])
    body = sys.stdin.buffer.read(length)
    return json.loads(body.decode())

def write_message(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(f"Content-Length: {len(body)}\r\n\r\n".encode())
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"capabilities":{}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[{"name":"echo","description":"Echo","inputSchema":{"type":"object","properties":{"value":{"type":"string"}},"required":["value"]}}]}})
    elif method == "tools/call":
        value = message["params"]["arguments"]["value"]
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"content":[{"type":"text","text":value}]}})
"#,
    )?;
    let mut registry = ToolRegistry::new();
    register_mcp_tools(
        &mut registry,
        &[termquill_kernel::McpServerConfig {
            name: "fake".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().to_string()],
            startup_timeout_secs: 5,
        }],
    )
    .await?;
    let result = registry
        .execute(
            termquill_kernel::ToolContext {
                workspace_root: temp.path().to_path_buf(),
                timeout_secs: 5,
            },
            termquill_kernel::ToolCall {
                id: "call-1".to_owned(),
                name: "echo".to_owned(),
                args_json: r#"{"value":"hello from mcp"}"#.to_owned(),
            },
        )
        .await?;
    assert_eq!(result.content, "hello from mcp");
    Ok(())
}

#[tokio::test]
async fn register_mcp_tools_errors_when_initialize_times_out() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("slow_init_mcp_server.py");
    write_fake_server_script(
        &script,
        r#"#!/usr/bin/env python3
import json, sys, time

def read_message():
    headers = {}
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        if line in (b"\r\n", b"\n"):
            break
        key, value = line.decode().split(":", 1)
        headers[key.lower()] = value.strip()
    length = int(headers["content-length"])
    body = sys.stdin.buffer.read(length)
    return json.loads(body.decode())

message = read_message()
time.sleep(2)
if message and message.get("method") == "initialize":
    sys.exit(0)
"#,
    )?;

    let mut registry = ToolRegistry::new();
    let error = register_mcp_tools(
        &mut registry,
        &[termquill_kernel::McpServerConfig {
            name: "slow".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().to_string()],
            startup_timeout_secs: 1,
        }],
    )
    .await
    .expect_err("expected MCP initialize timeout");

    assert!(error.to_string().contains("MCP initialize timed out"));
    Ok(())
}

#[tokio::test]
async fn register_mcp_tools_errors_when_tools_list_payload_is_invalid() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("invalid_tools_list_mcp_server.py");
    write_fake_server_script(
        &script,
        r#"#!/usr/bin/env python3
import json, sys

def read_message():
    headers = {}
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        if line in (b"\r\n", b"\n"):
            break
        key, value = line.decode().split(":", 1)
        headers[key.lower()] = value.strip()
    length = int(headers["content-length"])
    body = sys.stdin.buffer.read(length)
    return json.loads(body.decode())

def write_message(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(f"Content-Length: {len(body)}\r\n\r\n".encode())
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"capabilities":{}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{}})
"#,
    )?;

    let mut registry = ToolRegistry::new();
    let error = register_mcp_tools(
        &mut registry,
        &[termquill_kernel::McpServerConfig {
            name: "invalid-tools-list".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().to_string()],
            startup_timeout_secs: 5,
        }],
    )
    .await
    .expect_err("expected tools/list payload validation error");

    assert!(
        error
            .to_string()
            .contains("MCP tools/list missing tools array")
    );
    Ok(())
}

#[tokio::test]
async fn mcp_tool_execute_surfaces_remote_call_errors() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("erroring_call_mcp_server.py");
    write_fake_server_script(
        &script,
        r#"#!/usr/bin/env python3
import json, sys

def read_message():
    headers = {}
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        if line in (b"\r\n", b"\n"):
            break
        key, value = line.decode().split(":", 1)
        headers[key.lower()] = value.strip()
    length = int(headers["content-length"])
    body = sys.stdin.buffer.read(length)
    return json.loads(body.decode())

def write_message(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(f"Content-Length: {len(body)}\r\n\r\n".encode())
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"capabilities":{}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[{"name":"echo","description":"Echo","inputSchema":{"type":"object","properties":{"value":{"type":"string"}},"required":["value"]}}]}})
    elif method == "tools/call":
        write_message({"jsonrpc":"2.0","id":message["id"],"error":{"code":-32000,"message":"remote boom"}})
"#,
    )?;

    let mut registry = ToolRegistry::new();
    register_mcp_tools(
        &mut registry,
        &[termquill_kernel::McpServerConfig {
            name: "error-call".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().to_string()],
            startup_timeout_secs: 5,
        }],
    )
    .await?;

    let error = registry
        .execute(
            ToolContext {
                workspace_root: temp.path().to_path_buf(),
                timeout_secs: 5,
            },
            termquill_kernel::ToolCall {
                id: "call-1".to_owned(),
                name: "echo".to_owned(),
                args_json: r#"{"value":"hello from mcp"}"#.to_owned(),
            },
        )
        .await
        .expect_err("expected remote tools/call error");

    assert!(error.to_string().contains("MCP request tools/call failed"));
    Ok(())
}
