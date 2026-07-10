use std::{
    fs,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
    time::Instant,
};

use anyhow::Result;
use sigil_kernel::{ToolCall, ToolErrorKind, ToolRegistry, ToolResultStatus};
use tokio::io::{AsyncRead, ReadBuf};

use super::*;

fn write_server(path: &std::path::Path, body: &str) -> Result<()> {
    fs::write(path, body)?;
    Ok(())
}

fn server_config(
    name: &str,
    script: &std::path::Path,
    startup_timeout_secs: u64,
) -> McpServerConfig {
    McpServerConfig {
        name: name.to_owned(),
        command: "python3".to_owned(),
        args: vec![script.to_string_lossy().into_owned()],
        startup_timeout_secs,
        ..McpServerConfig::default()
    }
}

fn basic_probe_server(call_body: &str, request_log: &std::path::Path) -> String {
    let request_log = serde_json::to_string(&request_log.to_string_lossy())
        .expect("request-log path should serialize");
    r#"#!/usr/bin/env python3
import json, pathlib, sys, time
REQUEST_LOG = pathlib.Path(__REQUEST_LOG__)
def read_message():
    line = sys.stdin.buffer.readline()
    if not line:
        return None
    return json.loads(line.decode())
def write_message(obj):
    sys.stdout.buffer.write(json.dumps(obj).encode() + b"\n")
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
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[{"name":"probe","inputSchema":{"type":"object"}}]}})
    elif method == "tools/call":
        with REQUEST_LOG.open("a") as handle:
            handle.write("call\n")
__CALL_BODY__
"#
    .replace("__REQUEST_LOG__", &request_log)
    .replace("__CALL_BODY__", call_body)
}

#[test]
fn zero_tool_timeout_uses_finite_project_default() {
    let deadline = McpOperationDeadline::from_secs(0);
    assert_eq!(deadline.timeout_ms, 30_000);

    let normal = McpOperationDeadline::from_secs(7);
    assert_eq!(normal.timeout_ms, 7_000);

    let clamped = McpOperationDeadline::from_secs(u64::MAX);
    assert_eq!(clamped.timeout_ms, MAX_MCP_OPERATION_TIMEOUT_SECS * 1_000);
}

struct FailingStderrReader;

struct DropFlag(Arc<AtomicBool>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

impl AsyncRead for FailingStderrReader {
    fn poll_read(
        self: Pin<&mut Self>,
        _context: &mut Context<'_>,
        _buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Poll::Ready(Err(std::io::Error::other("injected stderr reader failure")))
    }
}

#[tokio::test]
async fn mcp_desync_stderr_reader_fault_is_published_before_notification() {
    let (fault_tx, fault_rx) = tokio::sync::oneshot::channel();
    let faulted = Arc::new(AtomicBool::new(false));
    let fault_record = Arc::new(std::sync::Mutex::new(None));
    let summary = super::super::process::drain_mcp_stderr_reader(
        FailingStderrReader,
        fault_tx,
        Arc::clone(&faulted),
        Arc::clone(&fault_record),
    )
    .await;

    assert!(faulted.load(Ordering::Acquire));
    let fault = fault_rx.await.expect("stderr fault notification");
    assert!(matches!(fault, McpStderrFault::ReaderFailed { .. }));
    assert!(matches!(
        fault_record
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref(),
        Some(McpStderrFault::ReaderFailed { reason, .. })
            if reason.contains("injected stderr reader failure")
    ));
    assert_eq!(summary.total_bytes, 0);
}

#[test]
fn stderr_reader_failure_has_typed_bounded_tool_error_details() {
    let result = McpClientError::ConnectionClosed {
        server_name: "reader".to_owned(),
        reason: "stderr reader failed".to_owned(),
        cause: Some(McpTerminalCause::StderrReaderFailed {
            total_bytes: 17,
            reason: "injected reader failure".to_owned(),
        }),
    }
    .with_cleanup(McpCleanupEvidence {
        completed: true,
        reason: "reaped".to_owned(),
    })
    .to_tool_result("call-reader", "mcp__reader__probe", "reader");
    let ToolResultStatus::Error(error) = result.status else {
        panic!("reader failure must project as a tool error");
    };

    assert_eq!(error.details["mcp"]["code"], "stderr_reader_failed");
    assert_eq!(
        error.details["mcp"]["terminal_cause"],
        "stderr_reader_failed"
    );
    assert_eq!(error.details["mcp"]["stderr_total_bytes"], 17);
    assert_eq!(
        error.details["mcp"]["stderr_reader_cause"],
        "injected reader failure"
    );
}

#[test]
fn mcp_limits_project_resource_kind_and_quantitative_details() {
    let cases = [
        (
            McpClientError::Framing {
                operation: "tools/call".to_owned(),
                source: super::super::framing::McpFramingError::FrameTooLarge {
                    limit_bytes: 100,
                    observed_at_least_bytes: 101,
                },
            },
            (None, None, Some(100), Some(101)),
        ),
        (
            McpClientError::MessageLimit {
                operation: "tools/call".to_owned(),
                limit: 10,
                observed_at_least: 11,
            },
            (Some(10), Some(11), None, None),
        ),
        (
            McpClientError::CumulativeBytesLimit {
                operation: "tools/call".to_owned(),
                limit_bytes: 200,
                observed_at_least_bytes: 225,
            },
            (None, None, Some(200), Some(225)),
        ),
    ];

    for (error, expected) in cases {
        let result = error.to_tool_result("call", "mcp__server__probe", "server");
        let ToolResultStatus::Error(error) = result.status else {
            panic!("limit must project as a tool error");
        };
        assert_eq!(error.kind, ToolErrorKind::ResourceLimit);
        assert_eq!(
            error.details["mcp"]["limit"].as_u64(),
            expected.0.map(|value| value as u64)
        );
        assert_eq!(
            error.details["mcp"]["observed_at_least"].as_u64(),
            expected.1.map(|value| value as u64)
        );
        assert_eq!(
            error.details["mcp"]["limit_bytes"].as_u64(),
            expected.2.map(|value| value as u64)
        );
        assert_eq!(
            error.details["mcp"]["observed_at_least_bytes"].as_u64(),
            expected.3.map(|value| value as u64)
        );
    }
}

#[test]
fn first_terminal_winner_publishes_one_consistent_reason_and_cause() {
    let terminal_state = Arc::new(std::sync::atomic::AtomicU8::new(0));
    let terminal_record = Arc::new(std::sync::Mutex::new(None));
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let candidates = [
        ("framing", None),
        (
            "stderr",
            Some(McpTerminalCause::StderrLimit {
                total_bytes: 9,
                limit_bytes: 8,
            }),
        ),
    ];
    let mut threads = Vec::new();
    for (reason, cause) in candidates {
        let terminal_state = Arc::clone(&terminal_state);
        let terminal_record = Arc::clone(&terminal_record);
        let barrier = Arc::clone(&barrier);
        threads.push(std::thread::spawn(move || {
            barrier.wait();
            publish_terminal_record(&terminal_state, &terminal_record, reason.to_owned(), cause)
        }));
    }
    barrier.wait();
    let winners = threads
        .into_iter()
        .map(|thread| thread.join().expect("terminal publisher must not panic"))
        .filter(|won| *won)
        .count();
    let record = terminal_record
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
        .expect("winning terminal record must be published");

    assert_eq!(winners, 1);
    match (record.reason.as_str(), record.cause) {
        ("framing", None) | ("stderr", Some(McpTerminalCause::StderrLimit { .. })) => {}
        observed => panic!("terminal reason/cause must come from one winner: {observed:?}"),
    }
}

#[tokio::test]
async fn mcp_cleanup_stderr_drain_timeout_aborts_and_joins_task() {
    let dropped = Arc::new(AtomicBool::new(false));
    let task_dropped = Arc::clone(&dropped);
    let task = tokio::spawn(async move {
        let _drop_flag = DropFlag(task_dropped);
        std::future::pending::<()>().await;
        McpStderrSummary::default()
    });

    let completion = finish_stderr_task(Some(task), Duration::from_millis(10)).await;
    assert!(
        completion.failure.as_deref().is_some_and(|reason| {
            reason.contains("bounded grace") && reason.contains("aborted")
        })
    );
    assert!(dropped.load(Ordering::Acquire));
    let mut cleanup = McpProcessCleanupSummary {
        completed: true,
        reason: "process group reaped".to_owned(),
    };
    merge_stderr_capture_into_cleanup(&mut cleanup, &completion);
    assert!(!cleanup.completed);
    assert!(cleanup.reason.contains("stderr drain"));
}

async fn execute_tool(
    registry: &ToolRegistry,
    root: &std::path::Path,
    timeout_secs: u64,
    name: &str,
    call_id: &str,
) -> Result<ToolResult> {
    registry
        .execute(
            ToolContext::new(root.to_path_buf(), timeout_secs),
            ToolCall {
                id: call_id.to_owned(),
                name: name.to_owned(),
                args_json: "{}".to_owned(),
            },
        )
        .await
}

#[cfg(unix)]
#[tokio::test]
async fn mcp_timeout_closes_old_client_reaps_process_group_and_refreshes_cleanly() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let request_log = temp.path().join("requests.log");
    let grandchild_pid = temp.path().join("grandchild.pid");
    let slow_script = temp.path().join("slow.py");
    write_server(
        &slow_script,
        &format!(
            r#"#!/usr/bin/env python3
import json, pathlib, subprocess, sys, time
REQUEST_LOG = pathlib.Path({request_log:?})
GRANDCHILD_PID = pathlib.Path({grandchild_pid:?})
def read_message():
    line = sys.stdin.buffer.readline()
    if not line:
        return None
    return json.loads(line.decode())
def write_message(obj):
    sys.stdout.buffer.write(json.dumps(obj).encode() + b"\n")
    sys.stdout.buffer.flush()
while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        write_message({{"jsonrpc":"2.0","id":message["id"],"result":{{"capabilities":{{}}}}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        write_message({{"jsonrpc":"2.0","id":message["id"],"result":{{"tools":[{{"name":"hang","inputSchema":{{"type":"object"}}}}]}}}})
    elif method == "tools/call":
        with REQUEST_LOG.open("a") as handle:
            handle.write("call\n")
        child = subprocess.Popen(["sleep", "30"])
        GRANDCHILD_PID.write_text(str(child.pid))
        time.sleep(30)
"#,
            request_log = request_log.to_string_lossy(),
            grandchild_pid = grandchild_pid.to_string_lossy(),
        ),
    )?;

    let mut old_registry = ToolRegistry::new();
    register_mcp_tools(
        &mut old_registry,
        &[server_config("deadline", &slow_script, 5)],
    )
    .await?;
    let first = execute_tool(
        &old_registry,
        temp.path(),
        1,
        "mcp__deadline__hang",
        "timeout-1",
    )
    .await?;
    match first.status {
        ToolResultStatus::Error(error) => assert_eq!(error.kind, ToolErrorKind::Timeout),
        ToolResultStatus::Ok => panic!("slow MCP call must time out"),
    }

    let started = Instant::now();
    let second = execute_tool(
        &old_registry,
        temp.path(),
        5,
        "mcp__deadline__hang",
        "timeout-2",
    )
    .await?;
    assert!(started.elapsed() < Duration::from_millis(250));
    match second.status {
        ToolResultStatus::Error(error) => assert_eq!(error.kind, ToolErrorKind::Protocol),
        ToolResultStatus::Ok => panic!("closed MCP client must fail fast"),
    }
    assert_eq!(fs::read_to_string(&request_log)?.lines().count(), 1);

    let pid = fs::read_to_string(&grandchild_pid)?.trim().parse::<u32>()?;
    let mut process_gone = false;
    for _ in 0..20 {
        let status = Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await?;
        if !status.success() {
            process_gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        process_gone,
        "MCP process-group grandchild must be terminated"
    );

    let healthy_script = temp.path().join("healthy.py");
    write_server(
        &healthy_script,
        r#"#!/usr/bin/env python3
import json, sys
def read_message():
    line = sys.stdin.buffer.readline()
    if not line:
        return None
    return json.loads(line.decode())
def write_message(obj):
    sys.stdout.buffer.write(json.dumps(obj).encode() + b"\n")
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
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[{"name":"hang","inputSchema":{"type":"object"}}]}})
    elif method == "tools/call":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"content":[{"type":"text","text":"fresh"}]}})
"#,
    )?;
    let mut refreshed_registry = ToolRegistry::new();
    register_mcp_tools(
        &mut refreshed_registry,
        &[server_config("deadline", &healthy_script, 5)],
    )
    .await?;
    let refreshed = execute_tool(
        &refreshed_registry,
        temp.path(),
        5,
        "mcp__deadline__hang",
        "refresh-1",
    )
    .await?;
    assert!(matches!(refreshed.status, ToolResultStatus::Ok));
    assert_eq!(refreshed.content, "fresh");
    Ok(())
}

#[tokio::test]
async fn mcp_framing_error_poisons_connection_and_second_call_writes_nothing() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let request_log = temp.path().join("framing-requests.log");
    let script = temp.path().join("invalid-frame.py");
    write_server(
        &script,
        &format!(
            r#"#!/usr/bin/env python3
import json, pathlib, sys
REQUEST_LOG = pathlib.Path({request_log:?})
def read_message():
    line = sys.stdin.buffer.readline()
    if not line:
        return None
    return json.loads(line.decode())
def write_message(obj):
    sys.stdout.buffer.write(json.dumps(obj).encode() + b"\n")
    sys.stdout.buffer.flush()
while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        write_message({{"jsonrpc":"2.0","id":message["id"],"result":{{"capabilities":{{}}}}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        write_message({{"jsonrpc":"2.0","id":message["id"],"result":{{"tools":[{{"name":"invalid","inputSchema":{{"type":"object"}}}}]}}}})
    elif method == "tools/call":
        with REQUEST_LOG.open("a") as handle:
            handle.write("call\n")
        sys.stdout.buffer.write(b"not-json\n")
        sys.stdout.buffer.flush()
"#,
            request_log = request_log.to_string_lossy(),
        ),
    )?;
    let mut registry = ToolRegistry::new();
    register_mcp_tools(&mut registry, &[server_config("framing", &script, 5)]).await?;
    for call_id in ["framing-1", "framing-2"] {
        let result =
            execute_tool(&registry, temp.path(), 5, "mcp__framing__invalid", call_id).await?;
        match result.status {
            ToolResultStatus::Error(error) => assert_eq!(error.kind, ToolErrorKind::Protocol),
            ToolResultStatus::Ok => panic!("invalid or closed MCP framing must fail"),
        }
    }
    assert_eq!(fs::read_to_string(request_log)?.lines().count(), 1);
    Ok(())
}

#[tokio::test]
async fn mcp_startup_deadline_includes_first_tools_list() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("slow-list.py");
    write_server(
        &script,
        r#"#!/usr/bin/env python3
import json, sys, time
def read_message():
    line = sys.stdin.buffer.readline()
    if not line:
        return None
    return json.loads(line.decode())
def write_message(obj):
    sys.stdout.buffer.write(json.dumps(obj).encode() + b"\n")
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
        time.sleep(2)
"#,
    )?;
    let mut registry = ToolRegistry::new();
    let error = register_mcp_tools(&mut registry, &[server_config("slow-list", &script, 1)])
        .await
        .expect_err("tools/list must share the startup deadline");
    assert!(error.to_string().contains("tools/list"));
    assert!(format!("{error:#}").contains("timed out"));
    assert!(format!("{error:#}").contains("cleanup_completed=true"));
    Ok(())
}

#[tokio::test]
async fn mcp_resource_and_prompt_adapters_apply_tool_context_timeout() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let cases = [
        (
            "resource-deadline",
            "resources",
            "resources/read",
            "mcp__resource_deadline__resources_read",
            r#"{"uri":"file:///slow"}"#,
        ),
        (
            "prompt-deadline",
            "prompts",
            "prompts/get",
            "mcp__prompt_deadline__prompts_get",
            r#"{"name":"slow"}"#,
        ),
    ];
    for (server_name, capability, blocked_method, tool_name, args_json) in cases {
        let script = temp.path().join(format!("{server_name}.py"));
        let body = r#"#!/usr/bin/env python3
import json, sys, time
CAPABILITY = "__CAPABILITY__"
BLOCKED_METHOD = "__BLOCKED_METHOD__"
def read_message():
    line = sys.stdin.buffer.readline()
    if not line:
        return None
    return json.loads(line.decode())
def write_message(obj):
    sys.stdout.buffer.write(json.dumps(obj).encode() + b"\n")
    sys.stdout.buffer.flush()
while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"capabilities":{CAPABILITY:{}}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[]}})
    elif method == BLOCKED_METHOD:
        time.sleep(30)
"#
        .replace("__CAPABILITY__", capability)
        .replace("__BLOCKED_METHOD__", blocked_method);
        write_server(&script, &body)?;
        let mut registry = ToolRegistry::new();
        register_mcp_tools(&mut registry, &[server_config(server_name, &script, 5)]).await?;
        let result = registry
            .execute(
                ToolContext::new(temp.path().to_path_buf(), 1),
                ToolCall {
                    id: format!("{server_name}-call"),
                    name: tool_name.to_owned(),
                    args_json: args_json.to_owned(),
                },
            )
            .await?;
        match result.status {
            ToolResultStatus::Error(error) => assert_eq!(error.kind, ToolErrorKind::Timeout),
            ToolResultStatus::Ok => panic!("{blocked_method} must honor ToolContext timeout"),
        }
    }
    Ok(())
}

#[tokio::test]
async fn mcp_stderr_hard_limit_closes_and_reaps_during_registration() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let script = temp.path().join("stderr-limit.py");
    write_server(
        &script,
        r#"#!/usr/bin/env python3
import json, sys, time
sys.stderr.write("x" * (9 * 1024 * 1024))
sys.stderr.flush()
def read_message():
    line = sys.stdin.buffer.readline()
    if not line:
        return None
    return json.loads(line.decode())
def write_message(obj):
    sys.stdout.buffer.write(json.dumps(obj).encode() + b"\n")
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
        time.sleep(30)
"#,
    )?;
    let mut registry = ToolRegistry::new();
    let started = Instant::now();
    let error = register_mcp_tools(&mut registry, &[server_config("stderr-limit", &script, 5)])
        .await
        .expect_err("stderr hard limit must terminate registration");
    assert!(started.elapsed() < Duration::from_secs(5));
    let error_text = format!("{error:#}");
    assert!(
        error_text.contains("stderr exceeded hard limit"),
        "unexpected registration error: {error_text}"
    );
    assert!(registry.specs().is_empty());
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn multi_server_registration_failure_rolls_back_prior_generation() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let pid_file = temp.path().join("transactional-registration.pid");
    let script = temp.path().join("transactional-registration.py");
    let pid_path = serde_json::to_string(&pid_file.to_string_lossy())?;
    write_server(
        &script,
        &r#"#!/usr/bin/env python3
import json, os, pathlib, sys
pathlib.Path(__PID_FILE__).write_text(str(os.getpid()))
while True:
    line = sys.stdin.buffer.readline()
    if not line:
        sys.exit(0)
    message = json.loads(line.decode())
    method = message.get("method")
    if method == "initialize":
        result = {"protocolVersion":"2025-06-18","serverInfo":{"name":"first","version":"1.0.0"},"capabilities":{}}
    elif method == "tools/list":
        result = {"tools":[{"name":"probe","inputSchema":{"type":"object"}}]}
    else:
        continue
    sys.stdout.buffer.write(json.dumps({"jsonrpc":"2.0","id":message["id"],"result":result}).encode() + b"\n")
    sys.stdout.buffer.flush()
"#
        .replace("__PID_FILE__", &pid_path),
    )?;
    let first = server_config("first", &script, 5);
    let second = McpServerConfig {
        name: "second".to_owned(),
        command: "/definitely/missing/second-mcp-server".to_owned(),
        startup_timeout_secs: 5,
        ..McpServerConfig::default()
    };
    let mut registry = ToolRegistry::new();

    register_mcp_tools(&mut registry, &[first, second])
        .await
        .expect_err("second server failure must fail the registration transaction");

    assert!(registry.specs().is_empty());
    let pid = fs::read_to_string(&pid_file)?.trim().parse::<u32>()?;
    let mut reaped = false;
    for _ in 0..40 {
        let status = Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await?;
        if !status.success() {
            reaped = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(reaped, "failed registration must reap the prior generation");
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn mcp_zero_surface_registration_reaps_process_group_descendant() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let descendant_pid_file = temp.path().join("zero-surface-descendant.pid");
    let script = temp.path().join("zero-surface.py");
    let descendant_path = serde_json::to_string(&descendant_pid_file.to_string_lossy())?;
    write_server(
        &script,
        &r#"#!/usr/bin/env python3
import json, pathlib, subprocess, sys
child = subprocess.Popen(["sh", "-c", "trap '' TERM; while :; do sleep 1; done"])
pathlib.Path(__DESCENDANT_PID_FILE__).write_text(str(child.pid))
def read_message():
    line = sys.stdin.buffer.readline()
    if not line:
        return None
    return json.loads(line.decode())
def write_message(obj):
    sys.stdout.buffer.write(json.dumps(obj).encode() + b"\n")
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
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"tools":[]}})
"#
        .replace("__DESCENDANT_PID_FILE__", &descendant_path),
    )?;

    let mut registry = ToolRegistry::new();
    let mut zero_surface = server_config("zero-surface", &script, 5);
    zero_surface.required = false;
    register_mcp_tools(&mut registry, &[zero_surface]).await?;
    assert!(registry.specs().is_empty());
    let descendant_pid = fs::read_to_string(&descendant_pid_file)?
        .trim()
        .parse::<u32>()?;
    let status = Command::new("kill")
        .args(["-0", &descendant_pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await?;
    assert!(
        !status.success(),
        "zero-surface registration must not orphan its process-group descendant"
    );
    Ok(())
}

#[tokio::test]
async fn mcp_desync_matching_id_invalid_jsonrpc_shape_closes_before_second_write() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let request_log = temp.path().join("invalid-shape.log");
    let script = temp.path().join("invalid-shape.py");
    write_server(
        &script,
        &basic_probe_server(
            "        write_message({\"jsonrpc\":\"2.0\",\"id\":message[\"id\"]})",
            &request_log,
        ),
    )?;
    let mut registry = ToolRegistry::new();
    register_mcp_tools(&mut registry, &[server_config("invalid-shape", &script, 5)]).await?;

    for call_id in ["invalid-shape-1", "invalid-shape-2"] {
        let result = execute_tool(
            &registry,
            temp.path(),
            5,
            "mcp__invalid_shape__probe",
            call_id,
        )
        .await?;
        let ToolResultStatus::Error(error) = &result.status else {
            panic!("invalid JSON-RPC response shape must fail");
        };
        assert_eq!(error.kind, ToolErrorKind::Protocol);
        if call_id.ends_with('1') {
            assert_eq!(error.details["mcp"]["code"], "invalid_jsonrpc_envelope");
            assert_eq!(error.details["mcp"]["cleanup_completed"], true);
            assert!(error.details["mcp"]["cleanup_reason"].is_string());
        }
    }
    assert_eq!(fs::read_to_string(request_log)?.lines().count(), 1);
    Ok(())
}

#[tokio::test]
async fn mcp_desync_invalid_error_and_server_request_shapes_close_before_second_write() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let cases = [
        (
            "rpc-error-null",
            "        write_message({\"jsonrpc\":\"2.0\",\"id\":message[\"id\"],\"error\":None})",
            "response error must be an object",
        ),
        (
            "rpc-error-code",
            "        write_message({\"jsonrpc\":\"2.0\",\"id\":message[\"id\"],\"error\":{\"code\":\"bad\",\"message\":\"failed\"}})",
            "response error code must be an integer",
        ),
        (
            "rpc-request-id",
            "        write_message({\"jsonrpc\":\"2.0\",\"id\":{},\"method\":\"roots/list\",\"params\":{}})",
            "request id must be a string or integer",
        ),
        (
            "rpc-request-float-id",
            "        write_message({\"jsonrpc\":\"2.0\",\"id\":1.5,\"method\":\"roots/list\",\"params\":{}})",
            "request id must be a string or integer",
        ),
        (
            "rpc-request-params",
            "        write_message({\"jsonrpc\":\"2.0\",\"method\":\"notifications/test\",\"params\":\"bad\"})",
            "request params must be an object",
        ),
        (
            "rpc-request-array-params",
            "        write_message({\"jsonrpc\":\"2.0\",\"method\":\"notifications/test\",\"params\":[]})",
            "request params must be an object",
        ),
        (
            "rpc-success-scalar-result",
            "        write_message({\"jsonrpc\":\"2.0\",\"id\":message[\"id\"],\"result\":True})",
            "success response result must be an object",
        ),
        (
            "rpc-batch",
            "        write_message([{\"jsonrpc\":\"2.0\",\"id\":message[\"id\"],\"result\":{}}])",
            "top-level JSON-RPC message must be an object",
        ),
    ];
    for (server_name, call_body, expected_reason) in cases {
        let request_log = temp.path().join(format!("{server_name}.log"));
        let script = temp.path().join(format!("{server_name}.py"));
        write_server(&script, &basic_probe_server(call_body, &request_log))?;
        let mut registry = ToolRegistry::new();
        register_mcp_tools(&mut registry, &[server_config(server_name, &script, 5)]).await?;
        let tool_name = format!("mcp__{}__probe", server_name.replace('-', "_"));
        let first = execute_tool(
            &registry,
            temp.path(),
            5,
            &tool_name,
            &format!("{server_name}-first"),
        )
        .await?;
        let ToolResultStatus::Error(error) = &first.status else {
            panic!("invalid JSON-RPC shape for {server_name} must fail");
        };
        assert_eq!(error.kind, ToolErrorKind::Protocol);
        assert_eq!(
            error.details["mcp"]["code"], "invalid_jsonrpc_envelope",
            "unexpected code for {server_name}: {error:?}"
        );
        assert!(
            error.message.contains(expected_reason),
            "unexpected message for {server_name}: {}",
            error.message
        );

        let second = execute_tool(
            &registry,
            temp.path(),
            5,
            &tool_name,
            &format!("{server_name}-second"),
        )
        .await?;
        assert!(second.is_error());
        assert_eq!(fs::read_to_string(request_log)?.lines().count(), 1);
    }
    Ok(())
}

#[tokio::test]
async fn mcp_desync_unexpected_response_id_has_bounded_preview_and_zero_second_write() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let request_log = temp.path().join("unexpected-id.log");
    let script = temp.path().join("unexpected-id.py");
    write_server(
        &script,
        &basic_probe_server(
            "        write_message({\"jsonrpc\":\"2.0\",\"id\":\"do-not-leak-secret-prefix-\" + \"x\" * 1000000,\"result\":{}})",
            &request_log,
        ),
    )?;
    let mut registry = ToolRegistry::new();
    register_mcp_tools(&mut registry, &[server_config("unexpected-id", &script, 5)]).await?;

    let first = execute_tool(
        &registry,
        temp.path(),
        5,
        "mcp__unexpected_id__probe",
        "unexpected-id-1",
    )
    .await?;
    let ToolResultStatus::Error(error) = &first.status else {
        panic!("unexpected response id must fail");
    };
    assert_eq!(error.kind, ToolErrorKind::Protocol);
    assert_eq!(error.details["mcp"]["code"], "unexpected_response_id");
    assert!(first.to_model_content().len() < 2_000);
    assert!(
        !first
            .to_model_content()
            .contains("do-not-leak-secret-prefix")
    );

    let second = execute_tool(
        &registry,
        temp.path(),
        5,
        "mcp__unexpected_id__probe",
        "unexpected-id-2",
    )
    .await?;
    assert!(second.is_error());
    assert_eq!(fs::read_to_string(request_log)?.lines().count(), 1);
    Ok(())
}

#[tokio::test]
async fn mcp_remote_error_message_and_data_use_bounded_safe_projection_without_poisoning()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let request_log = temp.path().join("large-remote-errors.log");
    let script = temp.path().join("large-remote-errors.py");
    write_server(
        &script,
        &basic_probe_server(
            r#"        call_number = len(REQUEST_LOG.read_text().splitlines())
        if call_number == 1:
            write_message({"jsonrpc":"2.0","id":message["id"],"error":{"code":-32001,"message":"M" * (3 * 1024 * 1024),"data":{"kind":"small"}}})
        else:
            write_message({"jsonrpc":"2.0","id":message["id"],"error":{"code":-32002,"message":"second remote error","data":"D" * (3 * 1024 * 1024)}})"#,
            &request_log,
        ),
    )?;
    let mut registry = ToolRegistry::new();
    register_mcp_tools(&mut registry, &[server_config("large-errors", &script, 5)]).await?;

    let first = execute_tool(
        &registry,
        temp.path(),
        10,
        "mcp__large_errors__probe",
        "large-error-message",
    )
    .await?;
    let second = execute_tool(
        &registry,
        temp.path(),
        10,
        "mcp__large_errors__probe",
        "large-error-data",
    )
    .await?;

    for result in [&first, &second] {
        let ToolResultStatus::Error(error) = &result.status else {
            panic!("valid remote JSON-RPC error must surface as a protocol ToolResult");
        };
        assert_eq!(error.kind, ToolErrorKind::Protocol);
        assert!(result.content.len() <= MCP_OUTPUT_LIMIT_BYTES);
        assert!(error.message.len() <= MCP_OUTPUT_LIMIT_BYTES);
        assert!(serde_json::to_vec(&error.details)?.len() < 4 * 1024);
        assert!(result.to_model_content().len() < 96 * 1024);
        assert!(serde_json::to_vec(result)?.len() < 160 * 1024);
        assert!(!error.details.to_string().contains(&"D".repeat(100)));
    }
    let ToolResultStatus::Error(first_error) = &first.status else {
        unreachable!();
    };
    assert_eq!(
        first_error.details["remote_error"]["message_truncated"],
        true
    );
    let ToolResultStatus::Error(second_error) = &second.status else {
        unreachable!();
    };
    assert!(
        second_error.details["remote_error"]["data"]["wire_bytes"]
            .as_u64()
            .is_some_and(|bytes| bytes > 3 * 1024 * 1024)
    );
    assert_eq!(fs::read_to_string(request_log)?.lines().count(), 2);
    Ok(())
}

#[tokio::test]
async fn mcp_desync_operation_message_caps_accept_256_and_reject_257() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let request_log = temp.path().join("message-cap.log");
    let script = temp.path().join("message-cap.py");
    write_server(
        &script,
        &basic_probe_server(
            r#"        within = message["params"]["arguments"].get("within")
        count = 255 if within else 256
        for index in range(count):
            write_message({"jsonrpc":"2.0","method":"notifications/test","params":{"index":index}})
        if within:
            write_message({"jsonrpc":"2.0","id":message["id"],"result":{"content":[{"type":"text","text":"ok"}]}})
        else:
            sys.stdout.buffer.write(b'{"jsonrpc":"2.0","id":')
            sys.stdout.buffer.flush()
            time.sleep(30)"#,
            &request_log,
        ),
    )?;
    let mut registry = ToolRegistry::new();
    register_mcp_tools(&mut registry, &[server_config("message-cap", &script, 5)]).await?;

    let within = registry
        .execute(
            ToolContext::new(temp.path().to_path_buf(), 5),
            ToolCall {
                id: "message-cap-256".to_owned(),
                name: "mcp__message_cap__probe".to_owned(),
                args_json: r#"{"within":true}"#.to_owned(),
            },
        )
        .await?;
    assert!(matches!(within.status, ToolResultStatus::Ok));

    let started = Instant::now();
    let over = execute_tool(
        &registry,
        temp.path(),
        5,
        "mcp__message_cap__probe",
        "message-cap-257",
    )
    .await?;
    assert!(started.elapsed() < Duration::from_secs(3));
    let ToolResultStatus::Error(error) = over.status else {
        panic!("257 inbound messages must exceed the operation cap");
    };
    assert_eq!(error.kind, ToolErrorKind::ResourceLimit);
    assert_eq!(error.details["mcp"]["code"], "message_limit");
    assert_eq!(error.details["mcp"]["limit"], 256);
    assert_eq!(error.details["mcp"]["observed_at_least"], 256);
    Ok(())
}

#[tokio::test]
async fn mcp_desync_operation_cumulative_eight_mib_cap_is_enforced() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let request_log = temp.path().join("cumulative-cap.log");
    let script = temp.path().join("cumulative-cap.py");
    write_server(
        &script,
        &basic_probe_server(
            r#"        for index in range(3):
            write_message({"jsonrpc":"2.0","method":"notifications/test","params":{"index":index,"value":"x" * 3000000}})
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"content":[]}})"#,
            &request_log,
        ),
    )?;
    let mut registry = ToolRegistry::new();
    register_mcp_tools(
        &mut registry,
        &[server_config("cumulative-cap", &script, 5)],
    )
    .await?;

    let result = execute_tool(
        &registry,
        temp.path(),
        10,
        "mcp__cumulative_cap__probe",
        "cumulative-cap",
    )
    .await?;
    let ToolResultStatus::Error(error) = result.status else {
        panic!("cumulative frames above 8 MiB must fail");
    };
    assert_eq!(error.kind, ToolErrorKind::ResourceLimit);
    assert_eq!(error.details["mcp"]["code"], "cumulative_bytes_limit");
    assert_eq!(error.details["mcp"]["limit_bytes"], 8 * 1024 * 1024);
    assert!(
        error.details["mcp"]["observed_at_least_bytes"]
            .as_u64()
            .is_some_and(|observed| {
                observed > 8 * 1024 * 1024 && observed <= 8 * 1024 * 1024 + 16 * 1024
            })
    );
    Ok(())
}

#[tokio::test]
async fn mcp_desync_slowloris_partial_frame_obeys_absolute_deadline() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let request_log = temp.path().join("slowloris.log");
    let script = temp.path().join("slowloris.py");
    write_server(
        &script,
        &basic_probe_server(
            r#"        data = json.dumps({"jsonrpc":"2.0","id":message["id"],"result":{"content":[]}}).encode() + b"\n"
        for byte in data:
            sys.stdout.buffer.write(bytes([byte]))
            sys.stdout.buffer.flush()
            time.sleep(0.05)"#,
            &request_log,
        ),
    )?;
    let mut registry = ToolRegistry::new();
    register_mcp_tools(&mut registry, &[server_config("slowloris", &script, 5)]).await?;

    let started = Instant::now();
    let result = execute_tool(
        &registry,
        temp.path(),
        1,
        "mcp__slowloris__probe",
        "slowloris",
    )
    .await?;
    assert!(started.elapsed() < Duration::from_secs(4));
    let ToolResultStatus::Error(error) = result.status else {
        panic!("slowloris response must time out");
    };
    assert_eq!(error.kind, ToolErrorKind::Timeout);
    assert_eq!(error.details["mcp"]["cleanup_completed"], true);
    Ok(())
}

#[tokio::test]
async fn mcp_desync_stderr_fault_wins_over_later_valid_response() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let request_log = temp.path().join("stderr-race.log");
    let script = temp.path().join("stderr-race.py");
    write_server(
        &script,
        &basic_probe_server(
            r#"        sys.stderr.write("x" * (9 * 1024 * 1024))
        sys.stderr.flush()
        write_message({"jsonrpc":"2.0","id":message["id"],"result":{"content":[{"type":"text","text":"must-not-succeed"}]}})"#,
            &request_log,
        ),
    )?;
    let mut registry = ToolRegistry::new();
    register_mcp_tools(&mut registry, &[server_config("stderr-race", &script, 5)]).await?;

    let result = execute_tool(
        &registry,
        temp.path(),
        10,
        "mcp__stderr_race__probe",
        "stderr-race",
    )
    .await?;
    let ToolResultStatus::Error(error) = &result.status else {
        panic!("stderr hard fault must defeat the later valid response");
    };
    assert_eq!(error.kind, ToolErrorKind::ResourceLimit);
    assert!(error.message.contains("stderr exceeded hard limit"));
    assert_eq!(error.details["mcp"]["code"], "stderr_limit");
    assert_eq!(error.details["mcp"]["terminal_cause"], "stderr_limit");
    assert_eq!(error.details["mcp"]["stderr_limit_bytes"], 8 * 1024 * 1024);
    assert!(
        error.details["mcp"]["stderr_total_bytes"]
            .as_u64()
            .is_some_and(|total| total > 8 * 1024 * 1024)
    );
    assert!(!result.to_model_content().contains("must-not-succeed"));
    Ok(())
}

#[tokio::test]
async fn mcp_desync_queued_deadline_interrupts_lock_holder_without_second_write() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let request_log = temp.path().join("queued-deadline.log");
    let script = temp.path().join("queued-deadline.py");
    write_server(
        &script,
        &basic_probe_server("        time.sleep(30)", &request_log),
    )?;
    let mut registry = ToolRegistry::new();
    register_mcp_tools(&mut registry, &[server_config("queued", &script, 5)]).await?;

    let first_registry = registry.clone();
    let first_root = temp.path().to_path_buf();
    let first = tokio::spawn(async move {
        execute_tool(
            &first_registry,
            &first_root,
            20,
            "mcp__queued__probe",
            "queued-first",
        )
        .await
    });
    for _ in 0..100 {
        if request_log.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        request_log.exists(),
        "first request must hold the connection lock"
    );

    let started = Instant::now();
    let second = execute_tool(
        &registry,
        temp.path(),
        1,
        "mcp__queued__probe",
        "queued-second",
    )
    .await?;
    assert!(started.elapsed() < Duration::from_secs(5));
    let ToolResultStatus::Error(second_error) = second.status else {
        panic!("queued request must time out");
    };
    assert_eq!(second_error.kind, ToolErrorKind::Timeout);

    let first = tokio::time::timeout(Duration::from_secs(5), first)
        .await
        .expect("lock holder must be interrupted by teardown")??;
    assert!(first.is_error());
    assert_eq!(fs::read_to_string(request_log)?.lines().count(), 1);
    Ok(())
}
