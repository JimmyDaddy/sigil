use std::{
    fs::{self, File},
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

fn test_workspace(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "sigil-serve-process-{name}-{}",
        uuid::Uuid::new_v4()
    ));
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

fn spawn_provider_fixture(answer: &'static str) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("provider fixture should bind");
    let address = listener.local_addr().expect("provider fixture address");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener
            .accept()
            .expect("provider request should reach fixture");
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .expect("provider read timeout should configure");
        read_http_message(&mut stream);
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

fn read_http_message(stream: &mut TcpStream) {
    const MAX_REQUEST_BYTES: usize = 8 * 1024 * 1024;
    let mut request = Vec::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = stream.read(&mut buffer).expect("HTTP request should read");
        assert!(read > 0, "HTTP request ended before its body arrived");
        request.extend_from_slice(&buffer[..read]);
        assert!(
            request.len() <= MAX_REQUEST_BYTES,
            "HTTP request exceeded fixture limit"
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
            .unwrap_or(0);
        if request.len() >= header_end.saturating_add(content_length) {
            return;
        }
    }
}

struct ServeProcess {
    child: Child,
    address: SocketAddr,
    stdout_path: PathBuf,
    stderr_path: PathBuf,
}

struct DesktopServeProcess {
    child: Child,
    owner_stdin: Option<ChildStdin>,
    address: SocketAddr,
    server_info: serde_json::Value,
    stdout_path: PathBuf,
    stderr_path: PathBuf,
}

impl Drop for ServeProcess {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

impl Drop for DesktopServeProcess {
    fn drop(&mut self) {
        self.owner_stdin.take();
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn spawn_serve(workspace: &Path, config_path: &Path, token: &str) -> ServeProcess {
    let stdout_path = workspace.join("serve.stdout");
    let stderr_path = workspace.join("serve.stderr");
    let stdout = File::create(&stdout_path).expect("serve stdout should create");
    let stderr = File::create(&stderr_path).expect("serve stderr should create");
    let child = Command::new(env!("CARGO_BIN_EXE_sigil"))
        .current_dir(workspace)
        .env("SIGIL_HTTP_TOKEN", token)
        .args([
            "--config",
            config_path.to_str().expect("UTF-8 config path"),
            "serve",
        ])
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .expect("sigil serve should spawn");
    let deadline = Instant::now() + Duration::from_secs(15);
    let address = loop {
        let output = fs::read_to_string(&stdout_path).unwrap_or_default();
        if let Some(address) = output.lines().find_map(|line| line.strip_prefix("bind: ")) {
            break address
                .parse()
                .expect("serve bind should be a socket address");
        }
        assert!(
            Instant::now() < deadline,
            "sigil serve did not report a bind address; stdout={output}; stderr={}",
            fs::read_to_string(&stderr_path).unwrap_or_default()
        );
        thread::sleep(Duration::from_millis(20));
    };
    ServeProcess {
        child,
        address,
        stdout_path,
        stderr_path,
    }
}

fn spawn_desktop_serve(workspace: &Path, config_path: &Path, token: &str) -> DesktopServeProcess {
    let stdout_path = workspace.join("desktop-serve.stdout");
    let stderr_path = workspace.join("desktop-serve.stderr");
    let stdout = File::create(&stdout_path).expect("desktop serve stdout should create");
    let stderr = File::create(&stderr_path).expect("desktop serve stderr should create");
    let mut child = Command::new(env!("CARGO_BIN_EXE_sigil"))
        .current_dir(workspace)
        .env("SIGIL_HTTP_TOKEN", token)
        .args([
            "--config",
            config_path.to_str().expect("UTF-8 config path"),
            "serve",
            "--startup-output",
            "json",
            "--shutdown-on-stdin-close",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .expect("desktop sigil serve should spawn");
    let owner_stdin = child
        .stdin
        .take()
        .expect("desktop owner pipe should be available");
    let deadline = Instant::now() + Duration::from_secs(15);
    let server_info = loop {
        let output = fs::read_to_string(&stdout_path).unwrap_or_default();
        if let Some(line) = output.lines().find(|line| !line.trim().is_empty()) {
            break serde_json::from_str::<serde_json::Value>(line)
                .expect("desktop startup line should be JSON");
        }
        assert!(
            Instant::now() < deadline,
            "desktop sigil serve did not report startup JSON; stdout={output}; stderr={}",
            fs::read_to_string(&stderr_path).unwrap_or_default()
        );
        thread::sleep(Duration::from_millis(20));
    };
    let address = server_info["bind_addr"]
        .as_str()
        .expect("desktop startup should include bind_addr")
        .parse()
        .expect("desktop bind_addr should be a socket address");
    DesktopServeProcess {
        child,
        owner_stdin: Some(owner_stdin),
        address,
        server_info,
        stdout_path,
        stderr_path,
    }
}

fn http_request(
    address: SocketAddr,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<&str>,
) -> (u16, String) {
    http_request_with_last_event_id(address, method, path, token, body, None)
}

fn http_request_with_last_event_id(
    address: SocketAddr,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<&str>,
    last_event_id: Option<&str>,
) -> (u16, String) {
    let body = body.unwrap_or_default();
    let authorization = token
        .map(|token| format!("Authorization: Bearer {token}\r\n"))
        .unwrap_or_default();
    let replay_cursor = last_event_id
        .map(|cursor| format!("Last-Event-ID: {cursor}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\n{authorization}{replay_cursor}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut stream = TcpStream::connect(address).expect("serve endpoint should accept a client");
    stream
        .set_read_timeout(Some(Duration::from_secs(15)))
        .expect("serve response timeout should configure");
    stream
        .write_all(request.as_bytes())
        .expect("serve request should write");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("serve response should complete");
    let status = response
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse::<u16>().ok())
        .expect("serve response should include a status");
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_owned())
        .unwrap_or_default();
    (status, body)
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
        "unsafe sigil serve unexpectedly remained active; stderr={}",
        String::from_utf8_lossy(&stderr)
    );
    Output {
        status,
        stdout,
        stderr,
    }
}

fn close_desktop_owner_and_wait(mut process: DesktopServeProcess) -> Output {
    process.owner_stdin.take();
    let deadline = Instant::now() + Duration::from_secs(15);
    let (status, timed_out) = loop {
        match process
            .child
            .try_wait()
            .expect("desktop serve child status should be readable")
        {
            Some(status) => break (status, false),
            None if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            None => {
                process
                    .child
                    .kill()
                    .expect("timed-out desktop serve should be killed");
                break (
                    process
                        .child
                        .wait()
                        .expect("timed-out desktop serve should be reaped"),
                    true,
                );
            }
        }
    };
    let stdout = fs::read(&process.stdout_path).expect("desktop serve stdout should read");
    let stderr = fs::read(&process.stderr_path).expect("desktop serve stderr should read");
    assert!(
        !timed_out,
        "desktop sigil serve did not drain after owner pipe closure; stderr={}",
        String::from_utf8_lossy(&stderr)
    );
    Output {
        status,
        stdout,
        stderr,
    }
}

#[test]
fn desktop_owner_channel_json_bootstrap_and_pipe_close_are_secret_free() {
    let workspace = test_workspace("desktop-owner");
    let config_path = workspace.join("sigil.toml");
    let token = "desktop-process-secret-token";
    write_config(&config_path, "http://127.0.0.1:1");

    let server = spawn_desktop_serve(&workspace, &config_path, token);

    assert_eq!(server.server_info["schema_version"], 1);
    assert_eq!(server.server_info["protocol_version"], 1);
    assert_eq!(server.server_info["authentication"], "bearer");
    assert_eq!(server.server_info["shutdown_on_stdin_close"], true);
    assert_eq!(
        server.server_info["capabilities"]["durable_session_reopen"],
        true
    );
    let startup = fs::read_to_string(&server.stdout_path).expect("startup output should read");
    assert_eq!(startup.lines().count(), 1);
    assert!(!startup.contains(token));
    assert!(!startup.contains(workspace.to_string_lossy().as_ref()));
    let (status, metadata) = http_request(server.address, "GET", "/server-info", Some(token), None);
    assert_eq!(status, 200);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&metadata)
            .expect("server metadata should be JSON"),
        server.server_info
    );

    let output = close_desktop_owner_and_wait(server);
    assert_eq!(output.status.code(), Some(0));
    assert!(!String::from_utf8_lossy(&output.stdout).contains(token));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(token));
    fs::remove_dir_all(workspace).expect("test workspace should remove");
}

#[cfg(unix)]
fn stop_serve(mut process: ServeProcess) -> Output {
    let signal_result = unsafe { libc::kill(process.child.id() as i32, libc::SIGINT) };
    assert_eq!(signal_result, 0, "SIGINT should reach sigil serve");
    let deadline = Instant::now() + Duration::from_secs(15);
    let (status, timed_out) = loop {
        match process
            .child
            .try_wait()
            .expect("serve child status should be readable")
        {
            Some(status) => break (status, false),
            None if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            None => {
                process
                    .child
                    .kill()
                    .expect("timed-out serve child should be killed");
                break (
                    process
                        .child
                        .wait()
                        .expect("timed-out serve child should be reaped"),
                    true,
                );
            }
        }
    };
    let stdout = fs::read(&process.stdout_path).expect("serve stdout should read");
    let stderr = fs::read(&process.stderr_path).expect("serve stderr should read");
    assert!(
        !timed_out,
        "sigil serve did not drain before deadline; stderr={}",
        String::from_utf8_lossy(&stderr)
    );
    Output {
        status,
        stdout,
        stderr,
    }
}

#[cfg(unix)]
#[test]
fn serve_process_runs_authenticated_session_to_terminal_and_restarts_with_new_epoch() {
    let workspace = test_workspace("lifecycle");
    let config_path = workspace.join("sigil.toml");
    let token = "process-test-token";
    let (base_url, provider) = spawn_provider_fixture("serve process answer");
    write_config(&config_path, &base_url);

    let server = spawn_serve(&workspace, &config_path, token);
    let (health_status, health_body) = http_request(server.address, "GET", "/health", None, None);
    assert_eq!(health_status, 200);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&health_body)
            .expect("health body should be JSON")["status"],
        "ok"
    );
    assert_eq!(
        http_request(server.address, "GET", "/sessions", None, None).0,
        401
    );

    let (session_status, session_body) = http_request(
        server.address,
        "POST",
        "/sessions",
        Some(token),
        Some(r#"{"label":"process e2e"}"#),
    );
    assert_eq!(session_status, 201);
    let session: serde_json::Value =
        serde_json::from_str(&session_body).expect("session body should be JSON");
    let session_id = session["id"]
        .as_str()
        .expect("session id should exist")
        .to_owned();
    let durable_session_id = session["durable_session_scope_id"]
        .as_str()
        .expect("durable session id should exist")
        .to_owned();
    assert!(session_id.starts_with("http-session-e1-"));

    let run_command = serde_json::json!({
        "protocol_version": 1,
        "command_id": "start-process-1",
        "client_id": "process-e2e",
        "session_id": session_id,
        "payload": {
            "prompt": "Return the deterministic fixture answer",
            "approval_mode": "deny"
        }
    })
    .to_string();
    let (run_status, run_body) = http_request(
        server.address,
        "POST",
        &format!("/sessions/{session_id}/runs"),
        Some(token),
        Some(&run_command),
    );
    assert_eq!(run_status, 201, "run response: {run_body}");
    let run: serde_json::Value =
        serde_json::from_str(&run_body).expect("run receipt should be JSON");
    let run_id = run["run"]["id"]
        .as_str()
        .expect("run id should exist")
        .to_owned();

    let (events_status, events_body) = http_request(
        server.address,
        "GET",
        &format!("/runs/{run_id}/events"),
        Some(token),
        None,
    );
    assert_eq!(events_status, 200);
    assert!(events_body.contains("event: run_event"));
    assert!(events_body.contains("\"type\":\"run_finished\""));
    assert!(events_body.contains("serve process answer"));
    let last_event_id = events_body
        .lines()
        .filter_map(|line| line.strip_prefix("id: "))
        .next_back()
        .expect("terminal durable SSE should include a replay cursor");
    let (reconnect_status, reconnect_body) = http_request_with_last_event_id(
        server.address,
        "GET",
        &format!("/runs/{run_id}/events"),
        Some(token),
        None,
        Some(last_event_id),
    );
    assert_eq!(reconnect_status, 200);
    assert!(
        !reconnect_body.contains("event: run_event"),
        "cursor at the terminal event should replay an empty suffix"
    );
    provider.join().expect("provider fixture should join");

    let (snapshot_status, snapshot_body) = http_request(
        server.address,
        "GET",
        &format!("/runs/{run_id}"),
        Some(token),
        None,
    );
    assert_eq!(snapshot_status, 200);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&snapshot_body)
            .expect("run snapshot should be JSON")["status"],
        "finished"
    );
    let output = stop_serve(server);
    assert_eq!(output.status.code(), Some(0));
    let startup = String::from_utf8(output.stdout).expect("serve stdout should be UTF-8");
    assert!(startup.contains("status: listening; press Ctrl-C for graceful shutdown"));
    assert!(!startup.contains(token));

    let (restart_base_url, restart_provider) = spawn_provider_fixture("reopened process answer");
    write_config(&config_path, &restart_base_url);
    let restarted = spawn_serve(&workspace, &config_path, token);
    let (catalog_status, catalog_body) = http_request(
        restarted.address,
        "GET",
        "/session-catalog",
        Some(token),
        None,
    );
    assert_eq!(catalog_status, 200, "catalog response: {catalog_body}");
    let catalog: serde_json::Value =
        serde_json::from_str(&catalog_body).expect("catalog body should be JSON");
    let historical = catalog["entries"]
        .as_array()
        .and_then(|entries| {
            entries
                .iter()
                .find(|entry| entry["session_id"].as_str() == Some(durable_session_id.as_str()))
        })
        .expect("completed durable session should enter the historical catalog");
    let open_body = serde_json::json!({
        "session_ref": historical["session_ref"],
        "session_id": durable_session_id,
        "label": "Reopened history"
    })
    .to_string();
    let (open_status, open_response) = http_request(
        restarted.address,
        "POST",
        "/sessions/open",
        Some(token),
        Some(&open_body),
    );
    assert_eq!(open_status, 200, "open response: {open_response}");
    let reopened: serde_json::Value =
        serde_json::from_str(&open_response).expect("open response should be JSON");
    let reopened_session_id = reopened["id"]
        .as_str()
        .expect("reopened adapter id should exist")
        .to_owned();
    assert!(
        reopened["id"]
            .as_str()
            .expect("reopened session id should exist")
            .starts_with("http-session-e2-")
    );
    assert_ne!(reopened_session_id, session_id);
    assert_eq!(reopened["durable_session_scope_id"], durable_session_id);
    let resumed_command = serde_json::json!({
        "protocol_version": 1,
        "command_id": "start-process-reopened",
        "client_id": "process-e2e",
        "session_id": reopened_session_id,
        "payload": {
            "prompt": "Continue the durable session with the fixture answer",
            "approval_mode": "deny"
        }
    })
    .to_string();
    let (resumed_status, resumed_body) = http_request(
        restarted.address,
        "POST",
        &format!("/sessions/{reopened_session_id}/runs"),
        Some(token),
        Some(&resumed_command),
    );
    assert_eq!(resumed_status, 201, "resumed response: {resumed_body}");
    let resumed: serde_json::Value =
        serde_json::from_str(&resumed_body).expect("resumed receipt should be JSON");
    let resumed_run_id = resumed["run"]["id"]
        .as_str()
        .expect("resumed run id should exist");
    let (events_status, events_body) = http_request(
        restarted.address,
        "GET",
        &format!("/runs/{resumed_run_id}/events"),
        Some(token),
        None,
    );
    assert_eq!(events_status, 200);
    assert!(events_body.contains("reopened process answer"));
    restart_provider
        .join()
        .expect("restart provider fixture should join");
    assert_eq!(stop_serve(restarted).status.code(), Some(0));

    fs::remove_dir_all(workspace).expect("test workspace should remove");
}

#[test]
fn serve_process_rejects_unsafe_startup_before_creating_listener_state() {
    let workspace = test_workspace("unsafe-startup");
    let config_path = workspace.join("sigil.toml");
    write_config(&config_path, "http://127.0.0.1:1");

    let cases: [(&str, Vec<&str>, Option<&str>); 3] = [
        ("missing-token", vec![], None),
        ("disabled-token", vec!["--no-token"], Some("unused-token")),
        (
            "external-bind",
            vec!["--host", "0.0.0.0"],
            Some("unused-token"),
        ),
    ];
    for (name, serve_args, token) in cases {
        let mut command = Command::new(env!("CARGO_BIN_EXE_sigil"));
        command.current_dir(&workspace).args([
            "--config",
            config_path.to_str().expect("UTF-8 config path"),
            "serve",
        ]);
        command.args(serve_args).env_remove("SIGIL_HTTP_TOKEN");
        if let Some(token) = token {
            command.env("SIGIL_HTTP_TOKEN", token);
        }
        let child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("unsafe serve process should spawn");
        let output = wait_for_child_output(child, Duration::from_secs(10));
        assert!(!output.status.success(), "{name} should fail closed");
        assert!(
            !String::from_utf8_lossy(&output.stdout).contains("status: listening"),
            "{name} must not claim that a listener started"
        );
        assert!(
            !workspace.join("state/http-server-v1").exists(),
            "{name} must fail before creating listener state"
        );
    }

    fs::remove_dir_all(workspace).expect("test workspace should remove");
}
