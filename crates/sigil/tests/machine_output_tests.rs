use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::{
    ffi::CString,
    os::unix::{ffi::OsStrExt, fs::OpenOptionsExt},
};

fn test_workspace(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("sigil-process-{name}-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&path).expect("test workspace should create");
    path
}

fn write_config(path: &Path, base_url: &str) {
    let workspace = path.parent().expect("config should have a parent");
    let config = format!(
        r#"[workspace]
root = "."

[storage]
state_root = "{}"
cache_root = "{}"

[agent]
provider = "deepseek"
model = "deepseek-v4-flash"
tool_timeout_secs = 5

[model_request]
request_timeout_secs = 5

[providers.deepseek]
base_url = "{base_url}"
beta_base_url = "{base_url}"
anthropic_base_url = "{base_url}"
api_key = "test-key"
strict_tools_mode = "auto"
"#,
        workspace.join("state").display(),
        workspace.join("cache").display()
    );
    fs::write(path, config).expect("test config should write");
}

fn spawn_sse_server(answer: &'static str) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("test server should bind");
    listener
        .set_nonblocking(true)
        .expect("test listener should be nonblocking");
    let address = listener.local_addr().expect("test server address");
    let server = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "provider request did not arrive");
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("test server accept failed: {error}"),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("test request timeout should configure");
        read_http_request(&mut stream);
        let body = format!(
            "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{answer}\"}},\"finish_reason\":\"stop\"}}]}}\n\ndata: [DONE]\n\n"
        );
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream
            .write_all(response.as_bytes())
            .expect("provider response should write");
    });
    (format!("http://{address}"), server)
}

fn read_http_request(stream: &mut std::net::TcpStream) {
    const MAX_REQUEST_BYTES: usize = 8 * 1024 * 1024;
    let mut request = Vec::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = stream
            .read(&mut buffer)
            .expect("provider request should read");
        assert!(read > 0, "provider request ended before its body arrived");
        request.extend_from_slice(&buffer[..read]);
        assert!(
            request.len() <= MAX_REQUEST_BYTES,
            "provider request too large"
        );
        let Some(header_end) = request.windows(4).position(|part| part == b"\r\n\r\n") else {
            continue;
        };
        let header_end = header_end + 4;
        let headers = String::from_utf8_lossy(&request[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .expect("provider request should carry content-length");
        if request.len() >= header_end.saturating_add(content_length) {
            return;
        }
    }
}

fn spawn_hanging_server() -> (String, mpsc::Receiver<()>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("test server should bind");
    let address = listener.local_addr().expect("test server address");
    let (request_started, started) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("provider request should arrive");
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .expect("test request timeout should configure");
        read_http_request(&mut stream);
        request_started
            .send(())
            .expect("request start should be observed");
        let mut buffer = [0_u8; 1024];
        loop {
            match stream.read(&mut buffer) {
                Ok(0) => return,
                Ok(_) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe
                    ) =>
                {
                    return;
                }
                Err(error) => panic!("hanging provider connection did not close: {error}"),
            }
        }
    });
    (format!("http://{address}"), started, server)
}

fn wait_for_child_output(mut child: Child, timeout: Duration) -> Output {
    let deadline = Instant::now() + timeout;
    let (status, timed_out) = loop {
        match child.try_wait().expect("child status should be readable") {
            Some(status) => break (status, false),
            None if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            None => {
                child.kill().expect("timed-out child should be killed");
                break (
                    child.wait().expect("timed-out child should be reaped"),
                    true,
                );
            }
        }
    };
    let mut stdout = Vec::new();
    if let Some(mut pipe) = child.stdout.take() {
        pipe.read_to_end(&mut stdout)
            .expect("child stdout should drain");
    }
    let mut stderr = Vec::new();
    if let Some(mut pipe) = child.stderr.take() {
        pipe.read_to_end(&mut stderr)
            .expect("child stderr should drain");
    }
    assert!(
        !timed_out,
        "sigil child did not exit before deadline; stderr={}",
        String::from_utf8_lossy(&stderr)
    );
    Output {
        status,
        stdout,
        stderr,
    }
}

#[test]
fn json_process_stdout_is_one_parseable_result_and_exit_zero() {
    let workspace = test_workspace("json-success");
    let config_path = workspace.join("sigil.toml");
    let (base_url, server) = spawn_sse_server("process answer");
    write_config(&config_path, &base_url);

    let output = Command::new(env!("CARGO_BIN_EXE_sigil"))
        .current_dir(&workspace)
        .args([
            "--config",
            config_path.to_str().expect("UTF-8 config path"),
            "run",
            "Say hi",
            "--output",
            "json",
        ])
        .output()
        .expect("sigil process should run");

    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8(output.stdout).expect("machine stdout should be UTF-8");
    assert_eq!(stdout.lines().count(), 1);
    let record: serde_json::Value = serde_json::from_str(&stdout).expect("stdout should be JSON");
    assert_eq!(record["record_type"], "result");
    assert_eq!(record["result"]["status"], "succeeded");
    assert_eq!(record["result"]["final_text"], "process answer");
    assert!(!stdout.contains("session log:"));
    server.join().expect("test server should join");
    fs::remove_dir_all(workspace).expect("test workspace should remove");
}

#[test]
fn json_process_configuration_error_is_structured_and_exits_two() {
    let workspace = test_workspace("json-config-error");
    let config_path = workspace.join("missing.toml");

    let output = Command::new(env!("CARGO_BIN_EXE_sigil"))
        .current_dir(&workspace)
        .args([
            "--config",
            config_path.to_str().expect("UTF-8 config path"),
            "run",
            "Say hi",
            "--output",
            "json",
        ])
        .output()
        .expect("sigil process should run");

    assert_eq!(output.status.code(), Some(2));
    let stdout = String::from_utf8(output.stdout).expect("machine stdout should be UTF-8");
    assert_eq!(stdout.lines().count(), 1);
    let record: serde_json::Value = serde_json::from_str(&stdout).expect("stdout should be JSON");
    assert_eq!(record["record_type"], "error");
    assert_eq!(record["error"]["code"], "configuration_invalid");
    assert!(!stdout.contains("missing.toml"));
    fs::remove_dir_all(workspace).expect("test workspace should remove");
}

#[cfg(unix)]
#[test]
fn jsonl_process_sigint_persists_cancelled_terminal_and_exits_130() {
    let workspace = test_workspace("jsonl-cancel");
    let config_path = workspace.join("sigil.toml");
    let (base_url, request_started, server) = spawn_hanging_server();
    write_config(&config_path, &base_url);
    let child = Command::new(env!("CARGO_BIN_EXE_sigil"))
        .current_dir(&workspace)
        .args([
            "--config",
            config_path.to_str().expect("UTF-8 config path"),
            "run",
            "Wait for SIGINT",
            "--output",
            "jsonl",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("sigil process should start");
    request_started
        .recv_timeout(Duration::from_secs(10))
        .expect("provider request should start before SIGINT");
    let pid = i32::try_from(child.id()).expect("child pid should fit pid_t");
    // SAFETY: `pid` belongs to the live child process spawned immediately above.
    let result = unsafe { libc::kill(pid, libc::SIGINT) };
    assert_eq!(result, 0, "SIGINT should be delivered to the child");
    let output = wait_for_child_output(child, Duration::from_secs(15));
    server.join().expect("hanging test server should join");

    assert_eq!(
        output.status.code(),
        Some(130),
        "stdout={} stderr={} status={:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
        output.status
    );
    let records = String::from_utf8(output.stdout)
        .expect("machine stdout should be UTF-8")
        .lines()
        .map(serde_json::from_str::<serde_json::Value>)
        .collect::<serde_json::Result<Vec<_>>>()
        .expect("every JSONL line should parse");
    assert!(records.len() >= 2);
    assert!(records.iter().any(|record| {
        record["record_type"] == "event" && record["event"]["event"]["type"] == "run_cancelled"
    }));
    let terminal = records.last().expect("terminal machine result");
    assert_eq!(terminal["record_type"], "result");
    assert_eq!(terminal["result"]["status"], "cancelled");
    let session_path = terminal["result"]["session_log_path"]
        .as_str()
        .expect("cancelled result should retain its session path");
    let session =
        fs::read_to_string(session_path).expect("cancelled session should remain durable");
    assert!(session.contains("\"record\":\"requested\""));
    assert!(session.contains("\"record\":\"finalized\""));
    assert!(session.contains("\"outcome\":\"cancelled\""));
    fs::remove_dir_all(workspace).expect("test workspace should remove");
}

#[cfg(unix)]
#[test]
fn json_process_sigint_during_blocking_preparation_exits_130_by_deadline() {
    let workspace = test_workspace("json-preparation-cancel");
    let config_path = workspace.join("blocked-config.fifo");
    let fifo = CString::new(config_path.as_os_str().as_bytes()).expect("FIFO path has no NUL");
    // SAFETY: `fifo` is a valid NUL-terminated path inside the owned test workspace.
    let created = unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) };
    assert_eq!(created, 0, "test config FIFO should create");
    let child = Command::new(env!("CARGO_BIN_EXE_sigil"))
        .current_dir(&workspace)
        .args([
            "--config",
            config_path.to_str().expect("UTF-8 config path"),
            "run",
            "Cancel blocked preparation",
            "--output",
            "json",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("sigil process should start");
    let (writer_ready, ready) = mpsc::channel();
    let (release_writer, release) = mpsc::channel();
    let writer_path = config_path.clone();
    let fifo_writer = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let writer = loop {
            match OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&writer_path)
            {
                Ok(writer) => break writer,
                Err(error)
                    if error.raw_os_error() == Some(libc::ENXIO) && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("test FIFO writer could not observe the reader: {error}"),
            }
        };
        writer_ready
            .send(())
            .expect("FIFO reader readiness should publish");
        let _writer = writer;
        let _ = release.recv_timeout(Duration::from_secs(10));
    });
    ready
        .recv_timeout(Duration::from_secs(5))
        .expect("sigil should enter blocking config preparation");
    let pid = i32::try_from(child.id()).expect("child pid should fit pid_t");
    // SAFETY: `pid` belongs to the live child process spawned immediately above.
    let result = unsafe { libc::kill(pid, libc::SIGINT) };
    assert_eq!(result, 0, "SIGINT should be delivered to the child");

    let started_waiting = Instant::now();
    let output = wait_for_child_output(child, Duration::from_secs(5));
    release_writer
        .send(())
        .expect("FIFO writer should be released");
    fifo_writer.join().expect("FIFO writer should join");
    assert!(started_waiting.elapsed() < Duration::from_secs(5));
    assert_eq!(
        output.status.code(),
        Some(130),
        "exe={} stdout={} stderr={} status={:?}",
        env!("CARGO_BIN_EXE_sigil"),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
        output.status
    );
    let stdout = String::from_utf8(output.stdout).expect("machine stdout should be UTF-8");
    assert_eq!(stdout.lines().count(), 1);
    let record: serde_json::Value = serde_json::from_str(&stdout).expect("stdout should be JSON");
    assert_eq!(record["record_type"], "error");
    assert_eq!(record["error"]["code"], "cancelled");
    assert_eq!(
        record["error"]["message"],
        "application run was cancelled before startup completed"
    );
    fs::remove_dir_all(workspace).expect("test workspace should remove");
}
