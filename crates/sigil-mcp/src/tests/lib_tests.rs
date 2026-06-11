use std::fs;

use anyhow::Result;
use serde_json::json;
use sigil_kernel::{
    ApprovalMode, McpServerConfig, McpServerStartup, McpServerTrustPolicy, McpTrustClass,
    ProviderCapabilities, SecretRedactor, ToolAccess, ToolCategory, ToolContext, ToolErrorKind,
    ToolRegistry, ToolResultStatus, ToolSubjectKind, ToolSubjectScope,
};

use super::{
    McpElicitationHandler, McpElicitationRequest, McpElicitationResponse, activate_lazy_mcp_tools,
    register_mcp_tools, register_mcp_tools_with_capabilities_roots_and_secrets,
    register_mcp_tools_with_capabilities_roots_secrets_and_elicitation,
};

fn write_fake_server_script(path: &std::path::Path, body: &str) -> Result<()> {
    fs::write(path, body)?;
    Ok(())
}

fn write_identity_server_script(
    path: &std::path::Path,
    server_name: &str,
    server_version: &str,
) -> Result<()> {
    let body = format!(
        r#"#!/usr/bin/env python3
import json, sys

SERVER_NAME = {server_name:?}
SERVER_VERSION = {server_version:?}

def read_message():
    headers = {{}}
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
    sys.stdout.buffer.write(f"Content-Length: {{len(body)}}\r\n\r\n".encode())
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        write_message({{"jsonrpc":"2.0","id":message["id"],"result":{{"protocolVersion":"2024-11-05","serverInfo":{{"name":SERVER_NAME,"version":SERVER_VERSION}},"capabilities":{{}}}}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        write_message({{"jsonrpc":"2.0","id":message["id"],"result":{{"tools":[{{"name":"echo","description":"Echo","inputSchema":{{"type":"object","properties":{{"value":{{"type":"string"}}}},"required":["value"]}}}}]}}}})
    elif method == "tools/call":
        value = message["params"]["arguments"]["value"]
        write_message({{"jsonrpc":"2.0","id":message["id"],"result":{{"content":[{{"type":"text","text":value}}]}}}})
"#
    );
    write_fake_server_script(path, &body)
}

fn test_provider_capabilities() -> ProviderCapabilities {
    ProviderCapabilities {
        exact_prefix_cache: false,
        reports_cache_tokens: false,
        supports_reasoning_stream: false,
        supports_tool_stream: false,
        supports_background_tasks: false,
        supports_response_handles: false,
        supports_reasoning_artifacts: false,
        supports_structured_output: false,
        supports_assistant_prefix_seed: false,
        supports_schema_constrained_tools: false,
        supports_infill_completion: false,
        supports_system_fingerprint: false,
        tool_name_max_chars: 64,
    }
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
        &[McpServerConfig {
            name: "fake".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().to_string()],
            startup_timeout_secs: 5,
            trust: McpServerTrustPolicy {
                trust_class: McpTrustClass::ThirdParty,
                approval_default: ApprovalMode::Allow,
                ..McpServerTrustPolicy::default()
            },
            ..McpServerConfig::default()
        }],
    )
    .await?;
    let spec = registry
        .spec_for("mcp__fake__echo")
        .expect("expected provider-visible MCP tool");
    assert_eq!(spec.category, ToolCategory::Mcp);
    assert_eq!(spec.access, ToolAccess::Network);
    assert!(registry.spec_for("echo").is_none());

    let subjects = registry.permission_subjects(
        &ToolContext {
            workspace_root: temp.path().to_path_buf(),
            timeout_secs: 5,
        },
        &sigil_kernel::ToolCall {
            id: "call-subject".to_owned(),
            name: "mcp__fake__echo".to_owned(),
            args_json: r#"{"value":"hello from mcp"}"#.to_owned(),
        },
    )?;
    assert_eq!(subjects.len(), 2);
    assert_eq!(subjects[0].kind, ToolSubjectKind::McpTool);
    assert_eq!(subjects[0].normalized, "mcp__fake__echo");
    assert_eq!(subjects[0].scope, ToolSubjectScope::Unknown);
    assert_eq!(subjects[1].kind, ToolSubjectKind::McpTrustClass);
    assert_eq!(subjects[1].original, "fake:third_party");
    assert_eq!(subjects[1].normalized, "mcp_trust_class:third_party");
    assert_eq!(subjects[1].scope, ToolSubjectScope::Unknown);

    let default_mode = registry.permission_default_mode(
        &ToolContext {
            workspace_root: temp.path().to_path_buf(),
            timeout_secs: 5,
        },
        &sigil_kernel::ToolCall {
            id: "call-default".to_owned(),
            name: "mcp__fake__echo".to_owned(),
            args_json: r#"{"value":"hello from mcp"}"#.to_owned(),
        },
    )?;
    assert_eq!(default_mode, Some(ApprovalMode::Allow));

    let egress = registry
        .egress_audit(
            &ToolContext {
                workspace_root: temp.path().to_path_buf(),
                timeout_secs: 5,
            },
            &sigil_kernel::ToolCall {
                id: "call-egress".to_owned(),
                name: "mcp__fake__echo".to_owned(),
                args_json: r#"{"value":"hello from mcp"}"#.to_owned(),
            },
        )?
        .expect("mcp trust egress logging should produce an audit summary");
    assert_eq!(egress.destination, "mcp:fake");
    assert_eq!(egress.operation, "tools/call");
    assert!(!egress.redacted);
    let payload = serde_json::to_string(&egress.payload)?;
    assert!(payload.contains(r#""server":"fake""#));
    assert!(payload.contains(r#""remote_tool":"echo""#));
    assert!(payload.contains(r#""top_level_keys":["value"]"#));
    assert!(!payload.contains("hello from mcp"));

    register_mcp_tools(
        &mut registry,
        &[McpServerConfig {
            name: "quiet".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().to_string()],
            startup_timeout_secs: 5,
            trust: McpServerTrustPolicy {
                egress_logging: false,
                ..McpServerTrustPolicy::default()
            },
            ..McpServerConfig::default()
        }],
    )
    .await?;
    let quiet_egress = registry.egress_audit(
        &ToolContext {
            workspace_root: temp.path().to_path_buf(),
            timeout_secs: 5,
        },
        &sigil_kernel::ToolCall {
            id: "call-quiet-egress".to_owned(),
            name: "mcp__quiet__echo".to_owned(),
            args_json: r#"{"value":"hello from mcp"}"#.to_owned(),
        },
    )?;
    assert!(quiet_egress.is_none());

    let result = registry
        .execute(
            sigil_kernel::ToolContext {
                workspace_root: temp.path().to_path_buf(),
                timeout_secs: 5,
            },
            sigil_kernel::ToolCall {
                id: "call-1".to_owned(),
                name: "mcp__fake__echo".to_owned(),
                args_json: r#"{"value":"hello from mcp"}"#.to_owned(),
            },
        )
        .await?;
    assert_eq!(result.content, "hello from mcp");
    Ok(())
}

#[tokio::test]
async fn pinned_mcp_server_registers_when_identity_matches() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("pinned_mcp_server.py");
    write_identity_server_script(&script, "sigil-test-server", "1.2.3")?;
    let args = vec![script.to_string_lossy().to_string()];
    let command_fingerprint = super::mcp_command_fingerprint("python3", &args)?;

    let mut registry = ToolRegistry::new();
    register_mcp_tools(
        &mut registry,
        &[McpServerConfig {
            name: "pinned".to_owned(),
            command: "python3".to_owned(),
            args,
            startup_timeout_secs: 5,
            trust: McpServerTrustPolicy {
                pin_version: true,
                pinned: Some(sigil_kernel::McpServerPinnedIdentity {
                    command_fingerprint,
                    protocol_version: "2024-11-05".to_owned(),
                    server_name: "sigil-test-server".to_owned(),
                    server_version: "1.2.3".to_owned(),
                }),
                ..McpServerTrustPolicy::default()
            },
            ..McpServerConfig::default()
        }],
    )
    .await?;

    assert!(registry.spec_for("mcp__pinned__echo").is_some());
    let egress = registry
        .egress_audit(
            &ToolContext {
                workspace_root: temp.path().to_path_buf(),
                timeout_secs: 5,
            },
            &sigil_kernel::ToolCall {
                id: "call-pin-egress".to_owned(),
                name: "mcp__pinned__echo".to_owned(),
                args_json: r#"{"value":"hello"}"#.to_owned(),
            },
        )?
        .expect("egress audit should include pinned server identity");
    assert_eq!(
        egress.payload["server_identity"]["server_name"],
        "sigil-test-server"
    );
    assert_eq!(egress.payload["server_identity"]["server_version"], "1.2.3");
    Ok(())
}

#[tokio::test]
async fn pinned_mcp_server_errors_when_pin_is_missing() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("unpinned_mcp_server.py");
    write_identity_server_script(&script, "sigil-test-server", "1.2.3")?;

    let mut registry = ToolRegistry::new();
    let error = register_mcp_tools(
        &mut registry,
        &[McpServerConfig {
            name: "unpinned".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().to_string()],
            startup_timeout_secs: 5,
            trust: McpServerTrustPolicy {
                pin_version: true,
                ..McpServerTrustPolicy::default()
            },
            ..McpServerConfig::default()
        }],
    )
    .await
    .expect_err("pin_version without pinned identity should fail");

    let message = error.to_string();
    assert!(message.contains("pin_version = true but no pinned identity"));
    assert!(message.contains("command_fingerprint"));
    assert!(message.contains("sigil-test-server"));
    Ok(())
}

#[tokio::test]
async fn pinned_mcp_server_errors_when_identity_mismatches() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("mismatched_mcp_server.py");
    write_identity_server_script(&script, "sigil-test-server", "1.2.3")?;
    let args = vec![script.to_string_lossy().to_string()];
    let command_fingerprint = super::mcp_command_fingerprint("python3", &args)?;

    let mut registry = ToolRegistry::new();
    let error = register_mcp_tools(
        &mut registry,
        &[McpServerConfig {
            name: "mismatched".to_owned(),
            command: "python3".to_owned(),
            args,
            startup_timeout_secs: 5,
            trust: McpServerTrustPolicy {
                pin_version: true,
                pinned: Some(sigil_kernel::McpServerPinnedIdentity {
                    command_fingerprint,
                    protocol_version: "2024-11-05".to_owned(),
                    server_name: "other-server".to_owned(),
                    server_version: "1.2.3".to_owned(),
                }),
                ..McpServerTrustPolicy::default()
            },
            ..McpServerConfig::default()
        }],
    )
    .await
    .expect_err("mismatched pinned identity should fail");

    let message = error.to_string();
    assert!(message.contains("pinned identity mismatch"));
    assert!(message.contains("server_name expected other-server observed sigil-test-server"));
    Ok(())
}

#[tokio::test]
async fn mcp_tool_blocks_secret_args_when_trust_disallows_secrets() -> Result<()> {
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
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"content":[{"type":"text","text":"should not run"}]}})
"#,
    )?;

    let mut registry = ToolRegistry::new();
    register_mcp_tools_with_capabilities_roots_and_secrets(
        &mut registry,
        &[McpServerConfig {
            name: "fake".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().to_string()],
            startup_timeout_secs: 5,
            trust: McpServerTrustPolicy {
                allow_secrets: false,
                ..McpServerTrustPolicy::default()
            },
            ..McpServerConfig::default()
        }],
        &test_provider_capabilities(),
        vec![temp.path().to_path_buf()],
        SecretRedactor::from_values(["sk-secret"]),
    )
    .await?;

    let result = registry
        .execute(
            ToolContext {
                workspace_root: temp.path().to_path_buf(),
                timeout_secs: 5,
            },
            sigil_kernel::ToolCall {
                id: "call-secret".to_owned(),
                name: "mcp__fake__echo".to_owned(),
                args_json: r#"{"value":"sk-secret"}"#.to_owned(),
            },
        )
        .await?;

    let egress = registry
        .egress_audit(
            &ToolContext {
                workspace_root: temp.path().to_path_buf(),
                timeout_secs: 5,
            },
            &sigil_kernel::ToolCall {
                id: "call-secret-egress".to_owned(),
                name: "mcp__fake__echo".to_owned(),
                args_json: r#"{"value":"sk-secret"}"#.to_owned(),
            },
        )?
        .expect("mcp trust egress logging should summarize blocked attempts");
    assert!(egress.redacted);
    let payload = serde_json::to_string(&egress.payload)?;
    assert!(payload.contains(r#""secret_detected":true"#));
    assert!(payload.contains(r#""top_level_keys":["value"]"#));
    assert!(!payload.contains("sk-secret"));

    match result.status {
        ToolResultStatus::Error(error) => {
            assert_eq!(error.kind, ToolErrorKind::PermissionDenied);
        }
        ToolResultStatus::Ok => panic!("secret egress should be blocked"),
    }
    assert!(!result.content.contains("sk-secret"));
    Ok(())
}

#[test]
fn mcp_egress_json_summary_does_not_include_values() {
    let summary = super::summarize_egress_json(&serde_json::json!({
        "path": "src/main.rs",
        "api_key": "sk-secret",
        "count": 3
    }));
    let rendered = serde_json::to_string(&summary).expect("summary should serialize");

    assert!(rendered.contains(r#""top_level_keys":["api_key","count","path"]"#));
    assert!(rendered.contains(r#""api_key":"string""#));
    assert!(!rendered.contains("sk-secret"));
    assert!(!rendered.contains("src/main.rs"));
}

#[tokio::test]
async fn mcp_tool_redacts_secret_echo_when_trust_allows_secrets() -> Result<()> {
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
    register_mcp_tools_with_capabilities_roots_and_secrets(
        &mut registry,
        &[McpServerConfig {
            name: "fake".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().to_string()],
            startup_timeout_secs: 5,
            trust: McpServerTrustPolicy {
                allow_secrets: true,
                ..McpServerTrustPolicy::default()
            },
            ..McpServerConfig::default()
        }],
        &test_provider_capabilities(),
        vec![temp.path().to_path_buf()],
        SecretRedactor::from_values(["sk-secret"]),
    )
    .await?;

    let result = registry
        .execute(
            ToolContext {
                workspace_root: temp.path().to_path_buf(),
                timeout_secs: 5,
            },
            sigil_kernel::ToolCall {
                id: "call-secret".to_owned(),
                name: "mcp__fake__echo".to_owned(),
                args_json: r#"{"value":"sk-secret"}"#.to_owned(),
            },
        )
        .await?;

    assert!(matches!(result.status, ToolResultStatus::Ok));
    assert_eq!(result.content, sigil_kernel::REDACTED_SECRET);
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
        &[McpServerConfig {
            name: "slow".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().to_string()],
            startup_timeout_secs: 1,
            ..McpServerConfig::default()
        }],
    )
    .await
    .expect_err("expected MCP initialize timeout");

    assert!(
        error
            .to_string()
            .contains("MCP server slow initialize timed out")
    );
    Ok(())
}

#[tokio::test]
async fn required_lazy_mcp_server_is_deferred_until_activation() -> Result<()> {
    let mut registry = ToolRegistry::new();
    register_mcp_tools(
        &mut registry,
        &[McpServerConfig {
            name: "required-lazy".to_owned(),
            command: "/definitely/missing/sigil-mcp-server".to_owned(),
            startup: McpServerStartup::Lazy,
            ..McpServerConfig::default()
        }],
    )
    .await?;

    assert!(registry.specs().is_empty());

    let error = activate_lazy_mcp_tools(
        &mut registry,
        &[McpServerConfig {
            name: "required-lazy".to_owned(),
            command: "/definitely/missing/sigil-mcp-server".to_owned(),
            startup: McpServerStartup::Lazy,
            ..McpServerConfig::default()
        }],
    )
    .await
    .expect_err("required lazy activation should surface startup failure");

    assert!(
        error
            .to_string()
            .contains("failed to spawn MCP server required-lazy")
    );
    Ok(())
}

#[tokio::test]
async fn lazy_mcp_servers_are_not_started_or_registered() -> Result<()> {
    let mut registry = ToolRegistry::new();
    register_mcp_tools(
        &mut registry,
        &[McpServerConfig {
            name: "lazy".to_owned(),
            command: "/definitely/missing/sigil-mcp-server".to_owned(),
            startup: McpServerStartup::Lazy,
            required: false,
            ..McpServerConfig::default()
        }],
    )
    .await?;

    assert!(registry.specs().is_empty());
    Ok(())
}

#[tokio::test]
async fn lazy_mcp_server_activation_registers_and_calls_real_tools() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("lazy_mcp_server.py");
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
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"content":[{"type":"text","text":"lazy:" + value}]}})
"#,
    )?;

    let server = McpServerConfig {
        name: "lazy".to_owned(),
        command: "python3".to_owned(),
        args: vec![script.to_string_lossy().to_string()],
        startup: McpServerStartup::Lazy,
        startup_timeout_secs: 5,
        ..McpServerConfig::default()
    };
    let mut registry = ToolRegistry::new();
    register_mcp_tools(&mut registry, std::slice::from_ref(&server)).await?;
    assert!(
        registry.spec_for("mcp__lazy__echo").is_none(),
        "lazy registration should not expose pseudo tools"
    );

    activate_lazy_mcp_tools(&mut registry, &[server]).await?;
    assert!(registry.spec_for("mcp__lazy__echo").is_some());

    let result = registry
        .execute(
            ToolContext {
                workspace_root: temp.path().to_path_buf(),
                timeout_secs: 5,
            },
            sigil_kernel::ToolCall {
                id: "call-lazy".to_owned(),
                name: "mcp__lazy__echo".to_owned(),
                args_json: r#"{"value":"ok"}"#.to_owned(),
            },
        )
        .await?;

    assert_eq!(result.content, "lazy:ok");
    Ok(())
}

#[tokio::test]
async fn optional_eager_mcp_server_start_failure_is_skipped() -> Result<()> {
    let mut registry = ToolRegistry::new();
    register_mcp_tools(
        &mut registry,
        &[McpServerConfig {
            name: "optional".to_owned(),
            command: "/definitely/missing/sigil-mcp-server".to_owned(),
            required: false,
            ..McpServerConfig::default()
        }],
    )
    .await?;

    assert!(registry.specs().is_empty());
    Ok(())
}

#[tokio::test]
async fn optional_eager_mcp_server_start_failure_does_not_block_other_servers() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("healthy_mcp_server.py");
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
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[{"name":"echo","description":"Echo","inputSchema":{"type":"object"}}]}})
"#,
    )?;

    let mut registry = ToolRegistry::new();
    register_mcp_tools(
        &mut registry,
        &[
            McpServerConfig {
                name: "optional".to_owned(),
                command: "/definitely/missing/sigil-mcp-server".to_owned(),
                required: false,
                ..McpServerConfig::default()
            },
            McpServerConfig {
                name: "healthy".to_owned(),
                command: "python3".to_owned(),
                args: vec![script.to_string_lossy().to_string()],
                startup_timeout_secs: 5,
                ..McpServerConfig::default()
            },
        ],
    )
    .await?;

    assert!(registry.spec_for("mcp__healthy__echo").is_some());
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
        &[McpServerConfig {
            name: "invalid-tools-list".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().to_string()],
            startup_timeout_secs: 5,
            ..McpServerConfig::default()
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
        &[McpServerConfig {
            name: "error-call".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().to_string()],
            startup_timeout_secs: 5,
            ..McpServerConfig::default()
        }],
    )
    .await?;

    let result = registry
        .execute(
            ToolContext {
                workspace_root: temp.path().to_path_buf(),
                timeout_secs: 5,
            },
            sigil_kernel::ToolCall {
                id: "call-1".to_owned(),
                name: "mcp__error_call__echo".to_owned(),
                args_json: r#"{"value":"hello from mcp"}"#.to_owned(),
            },
        )
        .await?;

    let ToolResultStatus::Error(error) = result.status else {
        panic!("expected remote tools/call error result");
    };
    assert_eq!(error.kind, ToolErrorKind::Protocol);
    assert!(error.message.contains("remote boom"));
    Ok(())
}

#[tokio::test]
async fn mcp_tool_execute_falls_back_for_non_text_content() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("non_text_mcp_server.py");
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
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[{"name":"image","description":"Image","inputSchema":{"type":"object"}}]}})
    elif method == "tools/call":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"content":[{"type":"image","data":"abc","mimeType":"image/png"}]}})
"#,
    )?;

    let mut registry = ToolRegistry::new();
    register_mcp_tools(
        &mut registry,
        &[McpServerConfig {
            name: "non-text".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().to_string()],
            startup_timeout_secs: 5,
            ..McpServerConfig::default()
        }],
    )
    .await?;

    let result = registry
        .execute(
            ToolContext {
                workspace_root: temp.path().to_path_buf(),
                timeout_secs: 5,
            },
            sigil_kernel::ToolCall {
                id: "call-1".to_owned(),
                name: "mcp__non_text__image".to_owned(),
                args_json: "{}".to_owned(),
            },
        )
        .await?;

    assert!(result.content.contains(r#""type": "image""#));
    assert!(result.content.contains(r#""mimeType": "image/png""#));
    Ok(())
}

#[tokio::test]
async fn mcp_client_answers_roots_list_while_waiting_for_tools_list() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace_root = temp.path().join("sigil mcp root");
    fs::create_dir_all(&workspace_root)?;
    let script = temp.path().join("roots_request_mcp_server.py");
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
        capabilities = message.get("params", {}).get("capabilities", {})
        if "roots" not in capabilities:
            write_message({"jsonrpc":"2.0","id":message["id"],"error":{"code":-32602,"message":"missing roots capability"}})
        else:
            write_message({"jsonrpc":"2.0","id":message["id"],"result":{"capabilities":{}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        write_message({"jsonrpc":"2.0","method":"notifications/progress","params":{"progressToken":"p1","progress":1}})
        write_message({"jsonrpc":"2.0","id":"server-roots-1","method":"roots/list","params":{}})
        roots_response = read_message()
        roots = roots_response.get("result", {}).get("roots", [])
        uri = roots[0].get("uri", "") if roots else ""
        if roots_response.get("id") != "server-roots-1" or not uri.startswith("file://") or "sigil%20mcp%20root" not in uri:
            write_message({"jsonrpc":"2.0","id":message["id"],"error":{"code":-32000,"message":"invalid roots response"}})
        else:
            write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[{"name":"echo","description":"Echo","inputSchema":{"type":"object"}}]}})
"#,
    )?;

    let mut registry = ToolRegistry::new();
    super::register_mcp_tools_with_name_limit_and_roots(
        &mut registry,
        &[McpServerConfig {
            name: "roots".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().to_string()],
            startup_timeout_secs: 5,
            ..McpServerConfig::default()
        }],
        64,
        vec![workspace_root],
        SecretRedactor::empty(),
        super::unsupported_mcp_elicitation_handler(),
    )
    .await?;

    assert!(registry.spec_for("mcp__roots__echo").is_some());
    Ok(())
}

#[tokio::test]
async fn mcp_roots_list_blocks_secret_when_trust_disallows_secrets() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace_root = temp.path().join("project-sk-secret");
    fs::create_dir_all(&workspace_root)?;
    let script = temp.path().join("secret_roots_request_mcp_server.py");
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
        write_message({"jsonrpc":"2.0","id":"server-roots-secret","method":"roots/list","params":{}})
        roots_response = read_message()
        error_message = roots_response.get("error", {}).get("message", "missing roots error")
        write_message({"jsonrpc":"2.0","id":message["id"],"error":{"code":-32000,"message":error_message}})
"#,
    )?;

    let mut registry = ToolRegistry::new();
    let error = super::register_mcp_tools_with_name_limit_and_roots(
        &mut registry,
        &[McpServerConfig {
            name: "secret_roots".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().to_string()],
            startup_timeout_secs: 5,
            trust: McpServerTrustPolicy {
                allow_secrets: false,
                ..McpServerTrustPolicy::default()
            },
            ..McpServerConfig::default()
        }],
        64,
        vec![workspace_root],
        SecretRedactor::from_values(["sk-secret"]),
        super::unsupported_mcp_elicitation_handler(),
    )
    .await
    .expect_err("secret-bearing roots/list should fail registration");

    let message = error.to_string();
    assert!(message.contains("roots/list would expose a secret"));
    assert!(message.contains("allow_secrets = false"));
    Ok(())
}

#[tokio::test]
async fn mcp_client_rejects_elicitation_requests_without_hanging() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("elicitation_request_mcp_server.py");
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
        write_message({"jsonrpc":"2.0","id":"server-elicitation-1","method":"elicitation/create","params":{"message":"Need input"}})
        elicitation_response = read_message()
        error = elicitation_response.get("error", {})
        if elicitation_response.get("id") != "server-elicitation-1" or error.get("code") != -32601:
            write_message({"jsonrpc":"2.0","id":message["id"],"error":{"code":-32000,"message":"invalid elicitation response"}})
        else:
            write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[{"name":"echo","description":"Echo","inputSchema":{"type":"object"}}]}})
"#,
    )?;

    let mut registry = ToolRegistry::new();
    register_mcp_tools(
        &mut registry,
        &[McpServerConfig {
            name: "elicitation".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().to_string()],
            startup_timeout_secs: 5,
            ..McpServerConfig::default()
        }],
    )
    .await?;

    assert!(registry.spec_for("mcp__elicitation__echo").is_some());
    Ok(())
}

#[derive(Debug)]
struct AcceptingElicitationHandler;

#[async_trait::async_trait]
impl McpElicitationHandler for AcceptingElicitationHandler {
    fn supports_elicitation(&self) -> bool {
        true
    }

    async fn elicit(&self, request: McpElicitationRequest) -> Result<McpElicitationResponse> {
        assert_eq!(request.server_name, "elicitation");
        assert_eq!(request.message, "Need input");
        assert!(request.requested_schema.get("properties").is_some());
        Ok(McpElicitationResponse::accept(serde_json::json!({
            "value": "accepted from test"
        })))
    }
}

#[tokio::test]
async fn mcp_client_answers_elicitation_requests_with_handler() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("elicitation_supported_mcp_server.py");
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
        params = message.get("params", {})
        capabilities = params.get("capabilities", {})
        if params.get("protocolVersion") != "2025-06-18" or "elicitation" not in capabilities:
            write_message({"jsonrpc":"2.0","id":message["id"],"error":{"code":-32000,"message":"elicitation capability missing"}})
        else:
            write_message({"jsonrpc":"2.0","id":message["id"],"result":{"protocolVersion":"2025-06-18","serverInfo":{"name":"elicitation","version":"1.0.0"},"capabilities":{}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        write_message({"jsonrpc":"2.0","id":"server-elicitation-1","method":"elicitation/create","params":{"message":"Need input","requestedSchema":{"type":"object","properties":{"value":{"type":"string","title":"Value"}},"required":["value"]}}})
        elicitation_response = read_message()
        result = elicitation_response.get("result", {})
        content = result.get("content", {})
        if elicitation_response.get("id") != "server-elicitation-1" or result.get("action") != "accept" or content.get("value") != "accepted from test":
            write_message({"jsonrpc":"2.0","id":message["id"],"error":{"code":-32000,"message":"invalid elicitation response"}})
        else:
            write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[{"name":"echo","description":"Echo","inputSchema":{"type":"object"}}]}})
"#,
    )?;

    let mut registry = ToolRegistry::new();
    register_mcp_tools_with_capabilities_roots_secrets_and_elicitation(
        &mut registry,
        &[McpServerConfig {
            name: "elicitation".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().to_string()],
            startup_timeout_secs: 5,
            ..McpServerConfig::default()
        }],
        &test_provider_capabilities(),
        vec![temp.path().to_path_buf()],
        SecretRedactor::empty(),
        std::sync::Arc::new(AcceptingElicitationHandler),
    )
    .await?;

    assert!(registry.spec_for("mcp__elicitation__echo").is_some());
    Ok(())
}

#[tokio::test]
async fn mcp_provider_visible_names_are_sanitized_truncated_and_unique() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("many_tools_mcp_server.py");
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

tools = [
    {"name":"alpha tool","description":"Alpha","inputSchema":{"type":"object"}},
    {"name":"alpha-tool","description":"Alpha collision","inputSchema":{"type":"object"}},
    {"name":"very-long-tool-name-" + "x" * 80,"description":"Long","inputSchema":{"type":"object"}}
]

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
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":tools}})
"#,
    )?;

    let mut registry = ToolRegistry::new();
    super::register_mcp_tools_with_name_limit(
        &mut registry,
        &[McpServerConfig {
            name: "bad server!".to_owned(),
            command: "python3".to_owned(),
            args: vec![script.to_string_lossy().to_string()],
            startup_timeout_secs: 5,
            ..McpServerConfig::default()
        }],
        48,
    )
    .await?;

    let names = registry
        .specs()
        .into_iter()
        .map(|spec| spec.name)
        .collect::<Vec<_>>();
    assert!(names.iter().all(|name| name.len() <= 48));
    assert!(names.iter().all(|name| {
        name.chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    }));
    assert!(
        names
            .iter()
            .any(|name| name == "mcp__bad_server__alpha_tool")
    );
    assert_eq!(
        names
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        names.len()
    );
    assert!(
        names
            .iter()
            .any(|name| name.starts_with("mcp__bad_server__very_long") && name.contains("__"))
    );
    Ok(())
}

#[tokio::test]
async fn same_tool_names_from_different_mcp_servers_stay_isolated() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("same_tool_mcp_server.py");
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
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[{"name":"echo","description":"Echo","inputSchema":{"type":"object"}}]}})
"#,
    )?;

    let mut registry = ToolRegistry::new();
    super::register_mcp_tools_with_name_limit(
        &mut registry,
        &[
            McpServerConfig {
                name: "server-a".to_owned(),
                command: "python3".to_owned(),
                args: vec![script.to_string_lossy().to_string()],
                startup_timeout_secs: 5,
                ..McpServerConfig::default()
            },
            McpServerConfig {
                name: "server-b".to_owned(),
                command: "python3".to_owned(),
                args: vec![script.to_string_lossy().to_string()],
                startup_timeout_secs: 5,
                ..McpServerConfig::default()
            },
        ],
        64,
    )
    .await?;

    assert!(registry.spec_for("mcp__server_a__echo").is_some());
    assert!(registry.spec_for("mcp__server_b__echo").is_some());
    Ok(())
}

#[test]
fn unsupported_elicitation_handler_reports_capability() {
    let handler = super::unsupported_mcp_elicitation_handler();
    assert!(!handler.supports_elicitation());
}

#[tokio::test]
async fn unsupported_elicitation_handler_rejects_requests() {
    let handler = super::unsupported_mcp_elicitation_handler();
    let error = handler
        .elicit(McpElicitationRequest {
            server_name: "fake".to_owned(),
            message: "need input".to_owned(),
            requested_schema: json!({"type":"object"}),
        })
        .await
        .expect_err("unsupported handler should fail");
    assert!(error.to_string().contains("not supported"));
}

#[test]
fn mcp_name_and_uri_helpers_sanitize_and_encode() {
    assert_eq!(
        super::sanitize_provider_name_part("bad name///tool"),
        "bad_name_tool"
    );
    assert_eq!(super::sanitize_provider_name_part("!!!"), "tool");
    assert_eq!(
        super::root_name(std::path::Path::new("/tmp/test-root")),
        "test-root"
    );
    assert_eq!(
        super::file_uri(std::path::Path::new("/tmp/space name.txt")),
        "file:///tmp/space%20name.txt"
    );
}

#[test]
fn mcp_egress_json_summary_handles_arrays_and_scalars() {
    let array = super::summarize_egress_json(&json!(["a", 1, true]));
    assert_eq!(array["type"], "array");
    assert_eq!(array["item_count"], 3);

    let scalar = super::summarize_egress_json(&json!(true));
    assert_eq!(scalar["type"], "bool");
}
