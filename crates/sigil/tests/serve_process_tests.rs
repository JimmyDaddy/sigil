use std::{
    fs::{self, File},
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Output, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

mod common;

fn test_workspace(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "sigil-serve-process-{name}-{}",
        uuid::Uuid::new_v4()
    ));
    fs::create_dir_all(&path).expect("test workspace should create");
    path
}

fn http_server_state_root(workspace: &Path, state_version: &str) -> PathBuf {
    workspace
        .join("state/workspaces")
        .join(sigil_runtime::workspace_id_for_root(workspace))
        .join(state_version)
}

fn write_config(path: &Path, base_url: &str) {
    let workspace = path.parent().expect("config should have a parent");
    let config = format!(
        r#"config_version = 2

[workspace]
root = "."

[storage]
state_root = "{}"
cache_root = "{}"

[agent]
connection = "local-test"
model = "gpt-4.1"
tool_timeout_secs = 5

[model_request]
request_timeout_secs = 5

[task]
routing_policy = "manual"

[connections.local-test]
label = "Local test"
provider = "custom"
protocol = "chat_completions"
base_url = "{base_url}"
credential = {{ source = "none" }}
"#,
        workspace.join("state").display(),
        workspace.join("cache").display()
    );
    fs::write(path, config).expect("test config should write");
}

fn write_auto_task_config(path: &Path, base_url: &str) {
    write_config(path, base_url);
    let config = fs::read_to_string(path).expect("base config should read");
    let config = config.replace(
        "[task]\nrouting_policy = \"manual\"",
        "[task]\nrouting_policy = \"auto\"\nmulti_agent_mode = \"proactive\"",
    );
    let config = config.replace("request_timeout_secs = 5", "request_timeout_secs = 60");
    fs::write(path, config).expect("auto Task config should write");
}

fn spawn_provider_fixture(answer: &'static str) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("provider fixture should bind");
    let address = listener.local_addr().expect("provider fixture address");
    (
        format!("http://{address}"),
        spawn_provider_fixture_with_listener(listener, answer),
    )
}

fn spawn_paused_preview_fixture() -> (String, thread::JoinHandle<()>, std::sync::mpsc::Sender<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("preview fixture should bind");
    let address = listener.local_addr().expect("preview fixture address");
    let (proceed, gate) = std::sync::mpsc::channel();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("provider request");
        stream
            .set_read_timeout(Some(Duration::from_secs(15)))
            .expect("timeout");
        read_http_message(&mut stream);
        let deltas =
            "data: {\"choices\":[{\"delta\":{\"content\":\"x\"},\"finish_reason\":null}]}\n\n"
                .repeat(100_000);
        let finish =
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", deltas.len() + finish.len()).expect("headers");
        stream.flush().expect("flush headers");
        gate.recv_timeout(Duration::from_secs(45))
            .expect("first native subscriber");
        stream
            .write_all(deltas.as_bytes())
            .expect("provider deltas");
        stream.flush().expect("flush deltas");
        gate.recv_timeout(Duration::from_secs(45))
            .expect("reconnected snapshot consumed");
        stream
            .write_all(finish.as_bytes())
            .expect("provider terminal");
        stream.flush().expect("flush terminal");
    });
    (format!("http://{address}"), handle, proceed)
}

fn spawn_model_catalog_fixture(model_id: &'static str) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("model catalog fixture should bind");
    let address = listener
        .local_addr()
        .expect("model catalog fixture address");
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener
            .accept()
            .expect("catalog request should reach fixture");
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .expect("catalog read timeout should configure");
        read_http_message(&mut stream);
        let body = serde_json::json!({ "data": [{ "id": model_id }] }).to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream
            .write_all(response.as_bytes())
            .expect("catalog response should write");
    });
    (format!("http://{address}/v1"), handle)
}

fn restart_provider_fixture(base_url: &str, answer: &'static str) -> thread::JoinHandle<()> {
    let address = base_url
        .strip_prefix("http://")
        .expect("fixture URL should use http")
        .parse::<std::net::SocketAddr>()
        .expect("fixture URL should contain a socket address");
    let listener = TcpListener::bind(address).expect("provider fixture should rebind");
    spawn_provider_fixture_with_listener(listener, answer)
}

fn spawn_provider_fixture_with_listener(
    listener: TcpListener,
    answer: &'static str,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
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
    })
}

struct VisionProviderFixture {
    base_url: String,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<anyhow::Result<Vec<serde_json::Value>>>>,
}

struct ProviderRequestGate {
    run_request_index: usize,
    entered: tokio::sync::oneshot::Sender<()>,
    release: std::sync::mpsc::Receiver<()>,
}

struct ProviderGateRelease(Option<std::sync::mpsc::Sender<()>>);

impl ProviderGateRelease {
    fn release(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}

impl Drop for ProviderGateRelease {
    fn drop(&mut self) {
        self.release();
    }
}

impl VisionProviderFixture {
    fn start() -> anyhow::Result<Self> {
        Self::start_with_first_response(None)
    }

    fn start_with_first_response(first_response: Option<String>) -> anyhow::Result<Self> {
        Self::start_with_responses(vec![first_response])
    }

    fn start_with_responses(responses: Vec<Option<String>>) -> anyhow::Result<Self> {
        Self::start_with_options(responses, None)
    }

    fn start_with_options(
        responses: Vec<Option<String>>,
        mut request_gate: Option<ProviderRequestGate>,
    ) -> anyhow::Result<Self> {
        use anyhow::Context as _;
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(60);
            let mut requests = Vec::new();
            let mut run_request_count = 0;
            while !worker_stop.load(Ordering::Acquire) && Instant::now() < deadline {
                let (mut stream, _) = match listener.accept() {
                    Ok(stream) => stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => return Err(error.into()),
                };
                stream.set_read_timeout(Some(Duration::from_secs(5)))?;
                stream.set_write_timeout(Some(Duration::from_secs(5)))?;
                // A cancelled title request may leave a connected socket with no HTTP bytes.
                // Once any byte arrived, keep the strict complete-request/body checks below.
                match stream.peek(&mut [0_u8; 1]) {
                    Ok(0) => continue,
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) =>
                    {
                        continue;
                    }
                    Err(error) => return Err(error.into()),
                    Ok(_) => {}
                }
                let request = read_http_message(&mut stream);
                let (_, body) = request.split_once("\r\n\r\n").context("provider body")?;
                anyhow::ensure!(requests.len() < 16, "unexpected provider request loop");
                let request = serde_json::from_str(body)?;
                let is_title = is_session_title_fixture_request(&request);
                if !is_title {
                    run_request_count += 1;
                }
                requests.push(request);
                if !is_title
                    && request_gate
                        .as_ref()
                        .is_some_and(|gate| gate.run_request_index == run_request_count)
                {
                    let gate = request_gate.take().context("provider gate")?;
                    let _ = gate.entered.send(());
                    gate.release
                        .recv_timeout(Duration::from_secs(20))
                        .context("provider request gate was not released")?;
                }
                let final_answer = concat!(
                    "event: response.output_text.delta\n",
                    "data: {\"delta\":\"Image received.\"}\n\n",
                    "event: response.completed\n",
                    "data: {\"response\":{\"id\":\"resp_image\",\"status\":\"completed\",\"output\":[{\"id\":\"msg_image\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"Image received.\"}]}]}}\n\n"
                );
                let body = if is_title {
                    final_answer
                } else {
                    responses
                        .get(run_request_count - 1)
                        .and_then(Option::as_deref)
                        .unwrap_or(final_answer)
                };
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )?;
            }
            Ok(requests)
        });
        Ok(Self {
            base_url: format!("http://{address}"),
            stop,
            worker: Some(worker),
        })
    }

    fn finish(mut self) -> anyhow::Result<Vec<serde_json::Value>> {
        self.stop.store(true, Ordering::Release);
        self.worker
            .take()
            .ok_or_else(|| anyhow::anyhow!("provider fixture already joined"))?
            .join()
            .map_err(|_| anyhow::anyhow!("provider fixture panicked"))?
    }
}

impl Drop for VisionProviderFixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

// Only this exact, tool-free maintenance request is excluded from explicit run counts.
// Any unexpected provider traffic remains a run request and fails the callers' exact totals.
fn is_session_title_fixture_request(request: &serde_json::Value) -> bool {
    let instruction = concat!(
        "Generate a concise semantic title for a coding-agent conversation. ",
        "Use the same language as the user's request. Capture the concrete goal, ",
        "component, or bug. Return only one plain-text title, without quotes, markdown, ",
        "labels, explanation, or trailing punctuation. Keep it within 12 words."
    );
    request["max_output_tokens"] == 64
        && request["store"] == false
        && request.get("tools").is_none_or(|tools| tools.as_array().is_some_and(Vec::is_empty))
        && request["input"].as_array().is_some_and(|input| {
            input.len() == 2
                && input[0] == serde_json::json!({"role":"developer", "content":[{"type":"input_text", "text":instruction}]})
                && input[1]["role"] == "user"
                && input[1]["content"].as_array().is_some_and(|content| {
                    content.len() == 1 && content[0]["type"] == "input_text" && content[0]["text"].is_string()
                })
        })
}

fn explicit_provider_requests(requests: &[serde_json::Value]) -> Vec<&serde_json::Value> {
    requests
        .iter()
        .filter(|request| !is_session_title_fixture_request(request))
        .collect()
}

fn read_http_message(stream: &mut TcpStream) -> String {
    const MAX_REQUEST_BYTES: usize = 8 * 1024 * 1024;
    let mut request = Vec::new();
    let mut buffer = [0_u8; 16 * 1024];
    let mut sent_continue = false;
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
            return String::from_utf8_lossy(&request).into_owned();
        }
        if !sent_continue
            && headers
                .lines()
                .any(|line| line.eq_ignore_ascii_case("expect: 100-continue"))
        {
            stream
                .write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
                .expect("provider fixture should accept an Expect request body");
            stream
                .flush()
                .expect("provider fixture should flush 100 Continue");
            sent_continue = true;
        }
    }
}

struct DirectTaskProviderFixture {
    base_url: String,
    requests: std::sync::mpsc::Receiver<(String, serde_json::Value)>,
    stop: std::sync::mpsc::Sender<()>,
    release_background_child: std::sync::mpsc::Sender<()>,
    recovery_mode: Arc<AtomicBool>,
    worker: thread::JoinHandle<()>,
}

fn spawn_direct_task_provider_fixture() -> DirectTaskProviderFixture {
    let listener = TcpListener::bind("127.0.0.1:0").expect("provider fixture should bind");
    let address = listener.local_addr().expect("provider fixture address");
    let (request_tx, request_rx) = std::sync::mpsc::channel();
    let (stop_tx, stop_rx) = std::sync::mpsc::channel();
    let (child_release_tx, child_release_rx) = std::sync::mpsc::channel();
    let child_release_rx = Arc::new(Mutex::new(child_release_rx));
    let recovery_mode = Arc::new(AtomicBool::new(false));
    let fixture_recovery_mode = Arc::clone(&recovery_mode);
    let task_turns = Arc::new(AtomicUsize::new(0));
    let provider = thread::spawn(move || {
        listener
            .set_nonblocking(true)
            .expect("fixture listener should be nonblocking");
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut responders = Vec::new();
        while Instant::now() < deadline && stop_rx.try_recv().is_err() {
            match listener.accept() {
                Ok((stream, _)) => {
                    let request_tx = request_tx.clone();
                    let task_turns = Arc::clone(&task_turns);
                    let child_release_rx = Arc::clone(&child_release_rx);
                    let recovery_mode = Arc::clone(&fixture_recovery_mode);
                    responders.push(thread::spawn(move || {
                        respond_to_direct_task_provider_request(
                            stream,
                            request_tx,
                            task_turns,
                            child_release_rx,
                            recovery_mode,
                        );
                    }));
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("provider fixture accept failed: {error}"),
            }
        }
        for responder in responders {
            responder
                .join()
                .expect("provider request responder should not panic");
        }
    });
    DirectTaskProviderFixture {
        base_url: format!("http://{address}"),
        requests: request_rx,
        stop: stop_tx,
        release_background_child: child_release_tx,
        recovery_mode,
        worker: provider,
    }
}

fn respond_to_direct_task_provider_request(
    mut stream: TcpStream,
    request_tx: std::sync::mpsc::Sender<(String, serde_json::Value)>,
    task_turns: Arc<AtomicUsize>,
    child_release_rx: Arc<Mutex<std::sync::mpsc::Receiver<()>>>,
    recovery_mode: Arc<AtomicBool>,
) {
    stream
        .set_nonblocking(false)
        .expect("accepted provider socket should be blocking");
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .expect("provider read timeout should configure");
    let request_text = read_http_message(&mut stream);
    let request: serde_json::Value = serde_json::from_str(
        request_text
            .split_once("\r\n\r\n")
            .expect("provider request should contain headers")
            .1,
    )
    .expect("provider request body should be JSON");
    let tools = request["tools"].as_array().cloned().unwrap_or_default();
    let tool_names = tools
        .iter()
        .filter_map(|tool| tool["function"]["name"].as_str())
        .collect::<Vec<_>>();
    let messages = request["messages"].as_array().cloned().unwrap_or_default();
    let message_text = messages
        .iter()
        .filter_map(|message| message["content"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let category = if message_text
        .contains("Generate a concise semantic title for a coding-agent conversation.")
    {
        "session_title"
    } else if message_text.contains("Inspect the repository with read-only tools")
        && recovery_mode.load(Ordering::SeqCst)
    {
        "replayed_background_child"
    } else if message_text.contains("Inspect the repository with read-only tools") {
        "background_child"
    } else if recovery_mode.load(Ordering::SeqCst) {
        "recovery_turn"
    } else if tool_names.contains(&"start_task") {
        "routing"
    } else if tool_names.contains(&"spawn_agent") {
        match task_turns.fetch_add(1, Ordering::SeqCst) {
            0 => "task_spawn",
            1 => "task_after_spawn",
            2 => "task_continuation_read_result",
            _ => "task_continuation_final",
        }
    } else if tool_names.contains(&"read_agent_result") {
        "task_continuation_read_result"
    } else {
        "unexpected"
    };
    let recorded_category = if category == "unexpected" {
        format!(
            "unexpected tools={tool_names:?}; prompt={}",
            message_text.chars().take(500).collect::<String>()
        )
    } else {
        category.to_owned()
    };
    request_tx
        .send((recorded_category, request))
        .expect("test should still collect provider requests");

    let (status, response_data) = match category {
        "session_title" => (
            "stop",
            serde_json::json!({"content": "Direct Task background E2E"}),
        ),
        "recovery_turn" => (
            "stop",
            serde_json::json!({"content": "The new session turn completed after restart."}),
        ),
        "replayed_background_child" => (
            "stop",
            serde_json::json!({"content": "A replayed child request must fail the test."}),
        ),
        "routing" => (
            "tool_calls",
            serde_json::json!({
                "tool_calls": [{
                    "index": 0,
                    "id": "call-direct-task-handoff",
                    "type": "function",
                    "function": {"name": "start_task", "arguments": "{}"}
                }]
            }),
        ),
        "task_spawn" => (
            "tool_calls",
            serde_json::json!({
                "tool_calls": [{
                    "index": 0,
                    "id": "call-background-child",
                    "type": "function",
                    "function": {
                        "name": "spawn_agent",
                        "arguments": serde_json::json!({
                            "profile_id": "explore",
                            "objective": "Inspect the fixture workspace",
                            "prompt": "Inspect the repository read-only and return the words child research result.",
                            "mode": "background",
                            "isolation": "shared_read_only"
                        }).to_string()
                    }
                }]
            }),
        ),
        "task_continuation_read_result" => (
            "tool_calls",
            serde_json::json!({
                "tool_calls": [{
                    "index": 0,
                    "id": "call-read-child-result",
                    "type": "function",
                    "function": {
                        "name": "read_agent_result",
                        "arguments": serde_json::json!({
                            "thread_id": sigil_runtime::chat_agent_thread_id_for_call(
                                "call-background-child",
                                &sigil_kernel::AgentProfileId::new("explore")
                                    .expect("explore profile id should be valid")
                            )
                            .expect("child thread id should be deterministic")
                            .as_str()
                        }).to_string()
                    }
                }]
            }),
        ),
        "background_child" => {
            child_release_rx
                .lock()
                .expect("child response gate should not be poisoned")
                .recv_timeout(Duration::from_secs(60))
                .expect("test should release the background child response");
            (
                "stop",
                serde_json::json!({"content": "child research result: fixture inspection completed"}),
            )
        }
        "task_after_spawn" => (
            "stop",
            serde_json::json!({"content": "The background child is still working; I will resume this Task when its result is ready."}),
        ),
        "task_continuation_final" => (
            "stop",
            serde_json::json!({"content": "Completed after reviewing the child research result."}),
        ),
        _ => (
            "stop",
            serde_json::json!({"content": "unexpected scripted provider request"}),
        ),
    };
    let delta = if status == "tool_calls" {
        response_data
    } else {
        serde_json::json!({"content": response_data["content"]})
    };
    let event = serde_json::json!({
        "choices": [{"delta": delta, "finish_reason": status}]
    });
    let body = format!("data: {event}\n\ndata: [DONE]\n\n");
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    // A process-restart test intentionally kills the serve process while this
    // response is gated, so a closed client connection is an expected fixture
    // outcome after the test releases the child response.
    let _ = stream.write_all(response.as_bytes());
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
    let mut command = Command::new(env!("CARGO_BIN_EXE_sigil"));
    common::isolated_child_environment(workspace)
        .expect("serve child environment should create")
        .apply_to_command(&mut command);
    let child = command
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
    spawn_desktop_serve_with_home(workspace, config_path, token, None)
}

fn spawn_desktop_serve_with_home(
    workspace: &Path,
    config_path: &Path,
    token: &str,
    user_home: Option<&Path>,
) -> DesktopServeProcess {
    let stdout_path = workspace.join("desktop-serve.stdout");
    let stderr_path = workspace.join("desktop-serve.stderr");
    let stdout = File::create(&stdout_path).expect("desktop serve stdout should create");
    let stderr = File::create(&stderr_path).expect("desktop serve stderr should create");
    let mut command = Command::new(env!("CARGO_BIN_EXE_sigil"));
    command
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
        .stderr(Stdio::from(stderr));
    let child_environment = match user_home {
        Some(home) => common::isolated_child_environment_for_home(home),
        None => common::isolated_child_environment(workspace),
    }
    .expect("desktop serve child environment should create");
    child_environment.apply_to_command(&mut command);
    let mut child = command.spawn().expect("desktop sigil serve should spawn");
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
    http_request_with_headers(address, method, path, token, body, None, None)
}

fn http_request_with_headers(
    address: SocketAddr,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<&str>,
    last_event_id: Option<&str>,
    run_owner: Option<(&str, &str)>,
) -> (u16, String) {
    let body = body.unwrap_or_default();
    let authorization = token
        .map(|token| format!("Authorization: Bearer {token}\r\n"))
        .unwrap_or_default();
    let replay_cursor = last_event_id
        .map(|cursor| format!("Last-Event-ID: {cursor}\r\n"))
        .unwrap_or_default();
    let run_owner = run_owner
        .map(|(session_id, owner_revision)| {
            format!(
                "x-sigil-session-id: {session_id}\r\nx-sigil-owner-revision: {owner_revision}\r\n"
            )
        })
        .unwrap_or_default();
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\n{authorization}{replay_cursor}{run_owner}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
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

    assert_eq!(
        server.server_info["schema_version"],
        sigil_http::HTTP_SERVER_INFO_SCHEMA_VERSION
    );
    assert_eq!(server.server_info["protocol_version"], 2);
    assert_eq!(server.server_info["authentication"], "bearer");
    assert_eq!(server.server_info["shutdown_on_stdin_close"], true);
    assert_eq!(
        server.server_info["capabilities"]["durable_session_reopen"],
        true
    );
    assert_eq!(
        server.server_info["capabilities"]["bounded_transcript_replay"],
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

#[test]
fn desktop_server_starts_first_run_without_config_and_exposes_empty_provider_setup() {
    let workspace = test_workspace("desktop-first-run");
    let config_path = workspace.join("missing-sigil.toml");
    let token = "desktop-first-run-token";

    let server = spawn_desktop_serve(&workspace, &config_path, token);

    assert_eq!(server.server_info["capabilities"]["provider_setup"], true);
    assert_eq!(
        server.server_info["capabilities"]["image_attachments"],
        true
    );
    let (status, body) = http_request(
        server.address,
        "GET",
        "/settings/provider-connections",
        Some(token),
        None,
    );
    assert_eq!(status, 200);
    let inventory =
        serde_json::from_str::<serde_json::Value>(&body).expect("inventory should be JSON");
    assert_eq!(inventory["config_mode"], "v2");
    assert_eq!(inventory["connections"], serde_json::json!([]));
    assert!(inventory["default_model"].is_null());
    assert!(!config_path.exists());

    let output = close_desktop_owner_and_wait(server);
    assert_eq!(output.status.code(), Some(0));
    assert!(!String::from_utf8_lossy(&output.stdout).contains(token));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(token));
    fs::remove_dir_all(workspace).expect("test workspace should remove");
}

#[test]
fn desktop_server_ignores_incompatible_previous_protocol_state_namespace() {
    let workspace = test_workspace("desktop-incompatible-previous-protocol-state");
    let config_path = workspace.join("sigil.toml");
    let token = "desktop-incompatible-previous-state-token";
    write_config(&config_path, "http://127.0.0.1:1");
    let previous_root = http_server_state_root(&workspace, "http-server-v3");
    fs::create_dir_all(&previous_root).expect("previous state root should create");
    let previous_journal = previous_root.join("protocol-events.json");
    let incompatible_state = br#"{"schema_version":3,"events":[{"legacy":true}]}"#;
    fs::write(&previous_journal, incompatible_state)
        .expect("incompatible previous journal should write");

    let server = spawn_desktop_serve(&workspace, &config_path, token);

    let current_root = http_server_state_root(&workspace, "http-server-v4");
    assert!(
        current_root.is_dir(),
        "current server state should use a fresh namespace"
    );
    assert!(
        !current_root.join("protocol-events.json").exists(),
        "an empty legacy journal should not be materialized before its first event"
    );
    assert_eq!(
        fs::read(&previous_journal).expect("previous journal should remain readable"),
        incompatible_state,
        "starting the current server must not migrate or rewrite incompatible state"
    );

    let output = close_desktop_owner_and_wait(server);
    assert_eq!(output.status.code(), Some(0));
    fs::remove_dir_all(workspace).expect("test workspace should remove");
}

#[test]
fn desktop_server_isolates_invalid_current_protocol_replay_state() {
    let workspace = test_workspace("desktop-invalid-current-protocol-state");
    let config_path = workspace.join("sigil.toml");
    let token = "desktop-invalid-current-state-token";
    write_config(&config_path, "http://127.0.0.1:1");
    let server_root = http_server_state_root(&workspace, "http-server-v4");
    fs::create_dir_all(&server_root).expect("current state root should create");
    let journal_path = server_root.join("protocol-events.json");
    let invalid_state = br#"{"schema_version":3,"events":[{"schema_version":3,"invalid":true}],"high_watermarks":[]}"#;
    fs::write(&journal_path, invalid_state).expect("invalid current journal should write");

    let server = spawn_desktop_serve(&workspace, &config_path, token);

    assert!(
        !journal_path.exists(),
        "the quarantined current journal should not be recreated before its first event"
    );
    let quarantined = fs::read_dir(&server_root)
        .expect("server state directory should remain readable")
        .filter_map(Result::ok)
        .find(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("protocol-events.json.invalid-")
        })
        .expect("invalid replay journal should be isolated");
    assert_eq!(
        fs::read(quarantined.path()).expect("isolated replay journal should remain readable"),
        invalid_state
    );

    let output = close_desktop_owner_and_wait(server);
    assert_eq!(output.status.code(), Some(0));
    fs::remove_dir_all(workspace).expect("test workspace should remove");
}

#[test]
fn desktop_server_rejects_noncurrent_protocol_data_without_rewriting_it() {
    for version in [None, Some(2)] {
        let workspace = test_workspace("desktop-noncurrent-protocol-state");
        let config_path = workspace.join("sigil.toml");
        write_config(&config_path, "http://127.0.0.1:1");
        let server_root = http_server_state_root(&workspace, "http-server-v4");
        fs::create_dir_all(&server_root).expect("state root should create");
        let journal_path = server_root.join("protocol-events.json");
        let mut event = serde_json::json!({"unsupported": true});
        if let Some(version) = version {
            event["schema_version"] = serde_json::json!(version);
        }
        let original = serde_json::to_vec(&serde_json::json!({
            "schema_version": 3,
            "events": [event],
            "high_watermarks": [],
        }))
        .expect("unsupported journal fixture should encode");
        fs::write(&journal_path, &original).expect("unsupported journal should write");
        let mut command = Command::new(env!("CARGO_BIN_EXE_sigil"));
        common::isolated_child_environment(&workspace)
            .expect("serve environment should create")
            .apply_to_command(&mut command);
        command
            .current_dir(&workspace)
            .env("SIGIL_HTTP_TOKEN", "noncurrent-protocol-test-token")
            .args([
                "--config",
                config_path.to_str().expect("UTF-8 config path"),
                "serve",
                "--startup-output",
                "json",
                "--shutdown-on-stdin-close",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = command.spawn().expect("serve process should spawn");
        let output = wait_for_child_output(child, Duration::from_secs(10));
        assert!(!output.status.success());
        assert!(
            output.stdout.is_empty(),
            "unsupported data cannot expose a listener"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("unsupported http event envelope schema")
        );
        assert_eq!(
            fs::read(&journal_path).expect("original should remain"),
            original
        );
        assert!(
            !fs::read_dir(&server_root)
                .expect("state root should remain")
                .filter_map(Result::ok)
                .any(|entry| entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("protocol-events.json.invalid-"))
        );
        fs::remove_dir_all(workspace).expect("test workspace should remove");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_launcher_supervises_real_server_and_closes_owner_channel() {
    let workspace = test_workspace("desktop-launcher");
    let config_path = workspace.join("sigil.toml");
    write_config(&config_path, "http://127.0.0.1:1");
    let request = sigil_desktop::DesktopLaunchRequest::new(
        env!("CARGO_BIN_EXE_sigil"),
        &config_path,
        &workspace,
    );

    let process = sigil_desktop::DesktopLauncher::default()
        .launch(request)
        .await
        .expect("desktop launcher should authenticate the real server");

    assert_eq!(
        process.server_info().schema_version,
        sigil_http::HTTP_SERVER_INFO_SCHEMA_VERSION
    );
    assert_eq!(process.server_info().protocol_version, 2);
    assert!(process.server_info().capabilities.durable_session_reopen);
    assert!(process.server_info().capabilities.bounded_transcript_replay);
    assert!(process.address().ip().is_loopback());
    assert_eq!(
        http_request(process.address(), "GET", "/server-info", None, None).0,
        401
    );
    let debug = format!("{process:?}");
    assert!(debug.contains("bearer: \"<redacted>\""));
    assert!(!debug.contains(workspace.to_string_lossy().as_ref()));
    assert!(!debug.contains(config_path.to_string_lossy().as_ref()));

    let report = process
        .shutdown()
        .await
        .expect("owner pipe should gracefully stop the real server");
    assert_eq!(report.kind, sigil_desktop::DesktopShutdownKind::Graceful);
    assert_eq!(report.exit_code, Some(0));
    assert!(report.success);
    fs::remove_dir_all(workspace).expect("test workspace should remove");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_launcher_classifies_a_workspace_service_lease_conflict() {
    let workspace = test_workspace("desktop-launcher-busy-state");
    let config_path = workspace.join("sigil.toml");
    write_config(&config_path, "http://127.0.0.1:1");
    let request = sigil_desktop::DesktopLaunchRequest::new(
        env!("CARGO_BIN_EXE_sigil"),
        &config_path,
        &workspace,
    );
    let launcher = sigil_desktop::DesktopLauncher::default();
    let first = launcher
        .launch(request.clone())
        .await
        .expect("first desktop server should own workspace state");

    let second = launcher
        .launch(request)
        .await
        .expect_err("second desktop server should not share workspace state leases");
    assert!(matches!(
        second,
        sigil_desktop::DesktopLaunchError::StartupRejected(
            sigil_desktop::DesktopStartupFailure::WorkspaceBusy
        )
    ));

    let report = first
        .shutdown()
        .await
        .expect("first desktop server should stop cleanly");
    assert!(report.success);
    fs::remove_dir_all(workspace).expect("test workspace should remove");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_launcher_uses_bounded_fallback_after_zero_grace_deadline() {
    let workspace = test_workspace("desktop-launcher-forced");
    let config_path = workspace.join("sigil.toml");
    write_config(&config_path, "http://127.0.0.1:1");
    let launcher =
        sigil_desktop::DesktopLauncher::with_timeouts(Duration::from_secs(15), Duration::ZERO);
    let process = launcher
        .launch(sigil_desktop::DesktopLaunchRequest::new(
            env!("CARGO_BIN_EXE_sigil"),
            &config_path,
            &workspace,
        ))
        .await
        .expect("desktop launcher should start the real server");

    let report = process
        .shutdown()
        .await
        .expect("fallback should terminate and reap the real server tree");

    assert!(matches!(
        report.kind,
        sigil_desktop::DesktopShutdownKind::Forced
            | sigil_desktop::DesktopShutdownKind::GracefulAfterDeadline
    ));
    fs::remove_dir_all(workspace).expect("test workspace should remove");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_launcher_opens_a_recovery_server_for_invalid_config() {
    let workspace = test_workspace("desktop-launcher-config-recovery");
    let config_path = workspace.join("sigil.toml");
    fs::write(&config_path, "[invalid").expect("invalid config fixture should write");

    let process = sigil_desktop::DesktopLauncher::default()
        .launch(sigil_desktop::DesktopLaunchRequest::new(
            env!("CARGO_BIN_EXE_sigil"),
            &config_path,
            &workspace,
        ))
        .await
        .expect("invalid config should start a bounded recovery server");
    let inventory = process
        .client()
        .provider_connections()
        .await
        .expect("recovery server should expose a safe provider inventory");
    assert_eq!(
        inventory.config_mode,
        sigil_desktop::DesktopProviderConfigMode::Invalid
    );
    assert!(inventory.connections.is_empty());
    assert_eq!(inventory.issues[0].code, "config_invalid_current_schema");
    process
        .shutdown()
        .await
        .expect("recovery server should shut down cleanly");
    fs::remove_dir_all(workspace).expect("test workspace should remove");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_workspace_manager_reuses_one_real_server_and_routes_typed_http() {
    let workspace = test_workspace("desktop-workspace-manager");
    let config_path = workspace.join("sigil.toml");
    write_config(&config_path, "http://127.0.0.1:1");
    let launch = sigil_desktop::DesktopLaunchRequest::new(
        env!("CARGO_BIN_EXE_sigil"),
        &config_path,
        &workspace,
    );
    let manager = sigil_desktop::DesktopWorkspaceManager::default();

    let first = manager
        .open(sigil_desktop::DesktopWorkspaceOpenRequest::new(
            launch.clone(),
            "workspace",
        ))
        .await
        .expect("manager should launch the real server");
    let duplicate = manager
        .open(sigil_desktop::DesktopWorkspaceOpenRequest::new(
            launch,
            "workspace",
        ))
        .await
        .expect("manager should reuse the canonical workspace");

    assert_eq!(duplicate, first);
    assert_eq!(
        manager.list().expect("list should succeed"),
        vec![first.clone()]
    );
    let client = manager
        .client(&first.id)
        .expect("ready workspace should expose an opaque typed client");
    assert!(
        client
            .list_sessions()
            .await
            .expect("typed list route should authenticate")
            .sessions
            .is_empty()
    );
    let session = client
        .create_session(sigil_desktop::DesktopSessionCreateRequest {
            label: Some("desktop smoke".to_owned()),
            model_ref: None,
        })
        .await
        .expect("typed create route should use the production runtime binding");
    assert_eq!(session.label.as_deref(), Some("desktop smoke"));
    assert_eq!(
        client
            .list_sessions()
            .await
            .expect("typed list route should remain available")
            .sessions
            .len(),
        1
    );
    let catalog = client
        .catalog(&sigil_desktop::DesktopCatalogQuery::default())
        .await
        .expect("typed catalog route should reconcile durable history");
    assert_eq!(catalog.workspace_id, first.id);
    let historical = catalog
        .entries
        .iter()
        .find(|entry| {
            entry.session_id.as_deref() == Some(session.durable_session_scope_id.as_str())
        })
        .expect("new durable session should enter the catalog");
    let reopened = client
        .open_session(sigil_desktop::DesktopSessionOpenRequest {
            session_ref: historical.session_ref.clone(),
            session_id: historical
                .session_id
                .clone()
                .expect("ready catalog row should have an identity"),
            label: Some("desktop reopened".to_owned()),
            recovery_binding: None,
        })
        .await
        .expect("typed open route should revalidate durable history");
    assert_eq!(reopened.id, session.id);
    assert_eq!(reopened.label.as_deref(), Some("desktop smoke"));

    let report = manager
        .close(&first.id)
        .await
        .expect("manager should gracefully close the real server");
    assert!(report.success);
    assert!(
        manager
            .list()
            .expect("closed manager should list")
            .is_empty()
    );
    fs::remove_dir_all(workspace).expect("test workspace should remove");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_typed_client_completes_first_run_provider_setup_against_real_server() {
    let workspace = test_workspace("desktop-provider-first-run");
    let config_path = workspace.join("missing-sigil.toml");
    let (provider_endpoint, provider) = spawn_model_catalog_fixture("local-first-run-coder");
    let manager = sigil_desktop::DesktopWorkspaceManager::default();
    let opened = manager
        .open(sigil_desktop::DesktopWorkspaceOpenRequest::new(
            sigil_desktop::DesktopLaunchRequest::new(
                env!("CARGO_BIN_EXE_sigil"),
                &config_path,
                &workspace,
            ),
            "workspace",
        ))
        .await
        .expect("manager should launch setup-capable sigil serve");
    let client = manager
        .client(&opened.id)
        .expect("ready workspace should expose a typed client");

    let inventory = client
        .provider_connections()
        .await
        .expect("missing config should return setup inventory");
    assert!(inventory.connections.is_empty());
    assert!(inventory.default_model.is_none());
    let catalog = client
        .provider_setup_catalog(sigil_desktop::DesktopProviderSetupCatalogRequest {
            template: sigil_desktop::DesktopProviderSetupTemplate::OpenAiCompatible,
            protocol: Some(sigil_desktop::DesktopProviderSetupProtocol::ChatCompletions),
            endpoint: Some(provider_endpoint.clone()),
            credential_source: sigil_desktop::DesktopProviderSetupCredentialSource::None,
            api_key: None,
            replace_invalid_config: false,
        })
        .await
        .expect("typed client should load the exact real-server catalog");
    assert_eq!(catalog.state, "remote");
    assert_eq!(catalog.models[0].model_id, "local-first-run-coder");

    let saved = client
        .save_provider_setup(sigil_desktop::DesktopProviderSetupSaveRequest {
            template: sigil_desktop::DesktopProviderSetupTemplate::OpenAiCompatible,
            protocol: Some(sigil_desktop::DesktopProviderSetupProtocol::ChatCompletions),
            endpoint: Some(provider_endpoint),
            credential_source: sigil_desktop::DesktopProviderSetupCredentialSource::None,
            api_key: None,
            model_id: "local-first-run-coder".to_owned(),
            context_window_tokens: Some(262_144),
            label: Some("Local first run".to_owned()),
            replace_invalid_config: false,
        })
        .await
        .expect("typed client should atomically save the selected real-server route");
    assert_eq!(
        saved.default_model.model_id.as_str(),
        "local-first-run-coder"
    );
    assert_eq!(saved.inventory.connections.len(), 1);
    assert_eq!(saved.inventory.connections[0].label, "Local first run");
    let persisted = fs::read_to_string(&config_path).expect("first-run config should persist");
    assert!(persisted.contains("local-first-run-coder"));
    assert!(persisted.contains("262144"));
    assert!(!persisted.contains("api_key"));

    let restarted = manager
        .restart(&opened.id)
        .await
        .expect("saved provider setup should reload the workspace server");
    assert_eq!(restarted.id, opened.id);
    let reloaded_client = manager
        .client(&restarted.id)
        .expect("restarted workspace should expose a typed client");
    let reloaded_inventory = reloaded_client
        .provider_connections()
        .await
        .expect("restarted server should load the saved provider configuration");
    assert_eq!(
        reloaded_inventory
            .default_model
            .as_ref()
            .map(|model| model.model_id.as_str()),
        Some("local-first-run-coder")
    );
    reloaded_client
        .create_session(sigil_desktop::DesktopSessionCreateRequest {
            label: Some("after provider setup".to_owned()),
            model_ref: None,
        })
        .await
        .expect("restarted server should admit the first durable session");

    let report = manager
        .close(&opened.id)
        .await
        .expect("manager should gracefully close setup server");
    assert!(report.success);
    provider.join().expect("catalog fixture should join");
    fs::remove_dir_all(workspace).expect("test workspace should remove");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_typed_client_explicitly_replaces_invalid_config_against_real_server() {
    let workspace = test_workspace("desktop-provider-config-repair");
    let config_path = workspace.join("sigil.toml");
    fs::write(&config_path, "[legacy\nprovider = \"unsupported\"\n")
        .expect("invalid config fixture should write");
    let (provider_endpoint, provider) = spawn_model_catalog_fixture("local-repair-coder");
    let manager = sigil_desktop::DesktopWorkspaceManager::default();
    let opened = manager
        .open(sigil_desktop::DesktopWorkspaceOpenRequest::new(
            sigil_desktop::DesktopLaunchRequest::new(
                env!("CARGO_BIN_EXE_sigil"),
                &config_path,
                &workspace,
            ),
            "workspace",
        ))
        .await
        .expect("manager should launch the recovery server");
    let client = manager
        .client(&opened.id)
        .expect("recovery workspace should expose a typed client");

    let rejected = client
        .provider_setup_catalog(sigil_desktop::DesktopProviderSetupCatalogRequest {
            template: sigil_desktop::DesktopProviderSetupTemplate::OpenAiCompatible,
            protocol: Some(sigil_desktop::DesktopProviderSetupProtocol::ChatCompletions),
            endpoint: Some(provider_endpoint.clone()),
            credential_source: sigil_desktop::DesktopProviderSetupCredentialSource::None,
            api_key: None,
            replace_invalid_config: false,
        })
        .await;
    assert!(
        rejected.is_err(),
        "invalid config replacement must require explicit intent"
    );

    let catalog = client
        .provider_setup_catalog(sigil_desktop::DesktopProviderSetupCatalogRequest {
            template: sigil_desktop::DesktopProviderSetupTemplate::OpenAiCompatible,
            protocol: Some(sigil_desktop::DesktopProviderSetupProtocol::ChatCompletions),
            endpoint: Some(provider_endpoint.clone()),
            credential_source: sigil_desktop::DesktopProviderSetupCredentialSource::None,
            api_key: None,
            replace_invalid_config: true,
        })
        .await
        .expect("explicit repair should load a current-schema catalog");
    assert_eq!(catalog.models[0].model_id, "local-repair-coder");

    let saved = client
        .save_provider_setup(sigil_desktop::DesktopProviderSetupSaveRequest {
            template: sigil_desktop::DesktopProviderSetupTemplate::OpenAiCompatible,
            protocol: Some(sigil_desktop::DesktopProviderSetupProtocol::ChatCompletions),
            endpoint: Some(provider_endpoint),
            credential_source: sigil_desktop::DesktopProviderSetupCredentialSource::None,
            api_key: None,
            model_id: "local-repair-coder".to_owned(),
            context_window_tokens: None,
            label: Some("Recovered local provider".to_owned()),
            replace_invalid_config: true,
        })
        .await
        .expect("explicit repair should atomically replace the invalid config");
    assert_eq!(
        saved.inventory.config_mode,
        sigil_desktop::DesktopProviderConfigMode::V2
    );
    assert_eq!(saved.inventory.connections.len(), 1);
    let persisted = fs::read_to_string(&config_path).expect("repaired config should persist");
    assert!(persisted.contains("config_version = 2"));
    assert!(persisted.contains("local-repair-coder"));
    assert!(!persisted.contains("[legacy"));

    let report = manager
        .close(&opened.id)
        .await
        .expect("recovery server should close");
    assert!(report.success);
    provider.join().expect("catalog fixture should join");
    fs::remove_dir_all(workspace).expect("test workspace should remove");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_config_repair_refuses_to_overwrite_a_concurrently_valid_config() {
    let workspace = test_workspace("desktop-provider-config-repair-race");
    let config_path = workspace.join("sigil.toml");
    fs::write(&config_path, "[invalid").expect("invalid config fixture should write");
    let (provider_endpoint, provider) = spawn_model_catalog_fixture("local-race-coder");
    let manager = sigil_desktop::DesktopWorkspaceManager::default();
    let opened = manager
        .open(sigil_desktop::DesktopWorkspaceOpenRequest::new(
            sigil_desktop::DesktopLaunchRequest::new(
                env!("CARGO_BIN_EXE_sigil"),
                &config_path,
                &workspace,
            ),
            "workspace",
        ))
        .await
        .expect("manager should launch the recovery server");
    let client = manager
        .client(&opened.id)
        .expect("recovery workspace should expose a typed client");
    client
        .provider_setup_catalog(sigil_desktop::DesktopProviderSetupCatalogRequest {
            template: sigil_desktop::DesktopProviderSetupTemplate::OpenAiCompatible,
            protocol: Some(sigil_desktop::DesktopProviderSetupProtocol::ChatCompletions),
            endpoint: Some(provider_endpoint.clone()),
            credential_source: sigil_desktop::DesktopProviderSetupCredentialSource::None,
            api_key: None,
            replace_invalid_config: true,
        })
        .await
        .expect("explicit repair should load its catalog");

    write_config(&config_path, &provider_endpoint);
    let valid_before = fs::read_to_string(&config_path).expect("valid config should read");
    let rejected = client
        .save_provider_setup(sigil_desktop::DesktopProviderSetupSaveRequest {
            template: sigil_desktop::DesktopProviderSetupTemplate::OpenAiCompatible,
            protocol: Some(sigil_desktop::DesktopProviderSetupProtocol::ChatCompletions),
            endpoint: Some(provider_endpoint),
            credential_source: sigil_desktop::DesktopProviderSetupCredentialSource::None,
            api_key: None,
            model_id: "local-race-coder".to_owned(),
            context_window_tokens: None,
            label: Some("Must not replace".to_owned()),
            replace_invalid_config: true,
        })
        .await;
    assert!(
        rejected.is_err(),
        "repair intent must not replace a config that became valid"
    );
    assert_eq!(
        fs::read_to_string(&config_path).expect("valid config should remain readable"),
        valid_before
    );

    let report = manager
        .close(&opened.id)
        .await
        .expect("recovery server should close");
    assert!(report.success);
    provider.join().expect("catalog fixture should join");
    fs::remove_dir_all(workspace).expect("test workspace should remove");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_typed_client_streams_and_replays_real_run_events() {
    let workspace = test_workspace("desktop-run-events");
    let config_path = workspace.join("sigil.toml");
    let (base_url, provider, proceed) = spawn_paused_preview_fixture();
    write_config(&config_path, &base_url);
    let config = fs::read_to_string(&config_path)
        .expect("preview config")
        .replace("request_timeout_secs = 5", "request_timeout_secs = 60");
    fs::write(&config_path, config).expect("preview request timeout");
    let manager = sigil_desktop::DesktopWorkspaceManager::default();
    let opened = manager
        .open(sigil_desktop::DesktopWorkspaceOpenRequest::new(
            sigil_desktop::DesktopLaunchRequest::new(
                env!("CARGO_BIN_EXE_sigil"),
                &config_path,
                &workspace,
            ),
            "workspace",
        ))
        .await
        .expect("manager should launch production sigil serve");
    let client = manager
        .client(&opened.id)
        .expect("ready workspace should expose a client");
    let session = client
        .create_session(sigil_desktop::DesktopSessionCreateRequest {
            label: Some("desktop run".to_owned()),
            model_ref: None,
        })
        .await
        .expect("session should create");
    let receipt = client
        .start_run(
            &session.id,
            sigil_desktop::DesktopRunStartRequest {
                review_annotations: Vec::new(),
                image_attachments: Vec::new(),
                prompt: "answer from the fixture".to_owned(),
                permission_mode: sigil_desktop::DesktopPermissionMode::ReadOnly,
                model_ref: None,
                model_selection_binding: None,
                route_recovery_binding: None,
                reasoning_effort: None,
                reasoning_effort_binding: None,
                skill_binding: None,
                agent_binding: None,
                task_continuation: None,
            },
        )
        .await
        .expect("run should start");
    let owner = receipt
        .foreground_owner
        .as_ref()
        .expect("started run should return exact foreground ownership");
    let mut stream = client
        .run_events(
            &session.id,
            &session.durable_session_scope_id,
            &receipt.run.id,
            &owner.owner_revision,
            None,
        )
        .await
        .expect("authenticated SSE should connect");
    proceed
        .send(())
        .expect("release streaming deltas after native attach");
    let mut kinds = Vec::new();
    let mut latest_cursor = None;
    let mut reconnected = false;
    let mut resumed = false;
    let mut full_message = None;
    let mut last_durable_sequence = 0;
    let mut live_frames = 0;
    let mut latest_live_revision = 0;
    let mut latest_preview_bytes = 0;
    let mut first_cursor = None;
    loop {
        let event = tokio::time::timeout(Duration::from_secs(40), stream.next_event())
            .await
            .unwrap_or_else(|error| panic!(
                "real event timed out: {error}; live_frames={live_frames}, latest_revision={latest_live_revision}, preview_bytes={latest_preview_bytes}, reconnected={reconnected}, durable_sequence={last_durable_sequence}, kinds={kinds:?}"
            ))
            .expect("real run event should decode");
        let Some(event) = event else {
            break;
        };
        if first_cursor.is_none() {
            first_cursor = event.replay_id.clone();
        }
        if let Some(public) = &event.run_event {
            assert!(public.sequence > last_durable_sequence);
            last_durable_sequence = public.sequence;
            latest_cursor = event.replay_id.clone();
        }
        if let Some(update) = &event.live_update {
            assert!(event.run_event.is_none());
            assert!(event.replay_id.is_none());
            assert!(update.base_durable_sequence <= last_durable_sequence);
            live_frames += 1;
            latest_live_revision = update.live_revision;
            latest_preview_bytes = update.preview.as_str().len();
            if update.preview.as_str() == "x".repeat(65_536)
                && update.truncated
                && update.live_revision >= 100_000
            {
                if !reconnected {
                    drop(stream);
                    stream = client
                        .run_events(
                            &session.id,
                            &session.durable_session_scope_id,
                            &receipt.run.id,
                            &owner.owner_revision,
                            latest_cursor.as_deref(),
                        )
                        .await
                        .expect("active owner should reconnect at durable cursor");
                    reconnected = true;
                    continue;
                }
                if !resumed {
                    proceed
                        .send(())
                        .expect("finish after current snapshot was restored");
                    resumed = true;
                }
            }
        }
        let timeline = event
            .into_timeline(
                &opened.id,
                &session.durable_session_scope_id,
                &receipt.run.id,
                &session.id,
            )
            .expect("real event should narrow for renderer");
        if timeline.kind == sigil_desktop::DesktopTimelineEventKind::AssistantMessage
            && timeline.assistant_kind.as_deref() != Some("reasoning_trace")
        {
            full_message = timeline.text.clone();
        }
        kinds.push(timeline.kind);
    }
    assert!(
        kinds.contains(&sigil_desktop::DesktopTimelineEventKind::RunStarted),
        "run stream omitted start event: {kinds:?}"
    );
    assert!(
        reconnected && resumed,
        "reconnect must restore the paused current snapshot"
    );
    assert!(
        (2..100_000).contains(&live_frames),
        "preview frames must be coalesced"
    );
    assert_eq!(full_message.as_deref(), Some("x".repeat(100_000).as_str()));
    assert!(
        last_durable_sequence < 1000,
        "live revision must not consume durable sequence"
    );
    assert!(kinds.contains(&sigil_desktop::DesktopTimelineEventKind::AssistantMessage));
    assert!(kinds.contains(&sigil_desktop::DesktopTimelineEventKind::RunFinished));

    let first_cursor = first_cursor.expect("run start should provide a durable cursor");
    let replay_error = client
        .run_events(
            &session.id,
            &session.durable_session_scope_id,
            &receipt.run.id,
            &owner.owner_revision,
            Some(&first_cursor),
        )
        .await
        .expect_err("terminal run must no longer admit a foreground live follower");
    assert!(matches!(
        replay_error,
        sigil_desktop::DesktopClientError::Rejected {
            status: 409,
            code: Some(code),
            ..
        } if code == "run_no_longer_foreground"
    ));

    provider.join().expect("provider fixture should stop");
    assert!(
        manager
            .close(&opened.id)
            .await
            .expect("desktop server should close")
            .success
    );
    fs::remove_dir_all(workspace).expect("test workspace should remove");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_image_only_serve_contract_reaches_provider_and_survives_restart()
-> anyhow::Result<()> {
    use anyhow::Context as _;
    use sigil_desktop::{DesktopConversationDisplayContent, DesktopConversationDisplayMessageRole};

    const PNG_BASE64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAIAAAADCAIAAAA2iEnWAAAAEElEQVR4nGP4z8AARAwoFABE0AX7pM/egAAAAABJRU5ErkJggg==";
    const PNG: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 2, 0, 0, 0, 3, 8, 2,
        0, 0, 0, 54, 136, 73, 214, 0, 0, 0, 16, 73, 68, 65, 84, 120, 156, 99, 248, 207, 192, 0, 68,
        12, 40, 20, 0, 68, 208, 5, 251, 164, 207, 222, 128, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66,
        96, 130,
    ];
    let workspace = tempfile::tempdir()?;
    let config_path = workspace.path().join("sigil.toml");
    let provider = VisionProviderFixture::start()?;
    write_config(&config_path, &provider.base_url);
    let config = fs::read_to_string(&config_path)?.replace("chat_completions", "responses");
    fs::write(&config_path, config)?;
    let launch = sigil_desktop::DesktopLaunchRequest::new(
        env!("CARGO_BIN_EXE_sigil"),
        &config_path,
        workspace.path(),
    );
    let manager = sigil_desktop::DesktopWorkspaceManager::default();
    let result: anyhow::Result<()> = async {
        let opened = manager
            .open(sigil_desktop::DesktopWorkspaceOpenRequest::new(
                launch.clone(),
                "images",
            ))
            .await?;
        let client = manager.client(&opened.id)?;
        let image = client.ingest_image(PNG.to_vec()).await?;
        anyhow::ensure!(
            image.width == 2 && image.height == 3 && image.byte_len == PNG.len() as u64
        );
        let session = client
            .create_session(sigil_desktop::DesktopSessionCreateRequest {
                label: Some("image-only contract".to_owned()),
                model_ref: None,
            })
            .await?;
        let receipt = client
            .start_run(
                &session.id,
                sigil_desktop::DesktopRunStartRequest {
                    review_annotations: Vec::new(),
                    image_attachments: vec![image.clone()],
                    prompt: String::new(),
                    permission_mode: sigil_desktop::DesktopPermissionMode::ReadOnly,
                    model_ref: None,
                    model_selection_binding: None,
                    route_recovery_binding: None,
                    reasoning_effort: None,
                    reasoning_effort_binding: None,
                    skill_binding: None,
                    agent_binding: None,
                    task_continuation: None,
                },
            )
            .await?;
        let terminal = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let run = client.run(&receipt.run.id).await?;
                if run.status.is_terminal() {
                    break Ok::<_, sigil_desktop::DesktopClientError>(run);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await??;
        if terminal.status != sigil_desktop::DesktopRunStatus::Finished {
            let display = client
                .conversation_display(&session.id, &Default::default())
                .await;
            anyhow::bail!("image run did not finish: {terminal:?}; display: {display:?}");
        }
        anyhow::ensure!(
            terminal.prompt_preview.is_empty(),
            "image-only request acquired fabricated prompt text"
        );
        let display = client
            .conversation_display(&session.id, &Default::default())
            .await?;
        let user = display
            .items
            .iter()
            .find(|item| {
                matches!(
                    &item.content,
                    DesktopConversationDisplayContent::Message {
                        role: DesktopConversationDisplayMessageRole::User,
                        image_attachments, ..
                    } if image_attachments == &vec![image.clone()]
                )
            })
            .context("durable image message")?;
        anyhow::ensure!(
            display.items.iter().any(|item| matches!(
                &item.content,
                DesktopConversationDisplayContent::Message {
                    role: DesktopConversationDisplayMessageRole::Assistant,
                    text: Some(text), ..
                } if text == "Image received."
            )),
            "real provider answer missing from durable display"
        );
        let display_id = user.display_id.clone();
        let content = client
            .message_image(&session.id, &display_id, &image.attachment_id)
            .await?;
        anyhow::ensure!(content.mime_type == "image/png" && content.data_base64 == PNG_BASE64);
        let catalog = client.catalog(&Default::default()).await?;
        let historical = catalog
            .entries
            .iter()
            .find(|entry| {
                entry.session_id.as_deref() == Some(session.durable_session_scope_id.as_str())
            })
            .context("durable catalog entry")?;
        let session_ref = historical.session_ref.clone();
        drop(client);
        anyhow::ensure!(
            manager.close(&opened.id).await?.success,
            "first server did not close"
        );
        let restarted = manager
            .open(sigil_desktop::DesktopWorkspaceOpenRequest::new(
                launch, "images",
            ))
            .await?;
        let client = manager.client(&restarted.id)?;
        let reopened = client
            .open_session(sigil_desktop::DesktopSessionOpenRequest {
                session_ref,
                session_id: session.durable_session_scope_id,
                label: None,
                recovery_binding: None,
            })
            .await?;
        let restored = client
            .message_image(&reopened.id, &display_id, &image.attachment_id)
            .await?;
        anyhow::ensure!(
            restored.mime_type == "image/png" && restored.data_base64 == PNG_BASE64,
            "restarted server lost exact image content"
        );
        Ok(())
    }
    .await;
    let cleanup = manager.close_all().await;
    let requests = provider.finish()?;
    if let Err(error) = result {
        // This isolated fixture contains no credentials. Still use the production redactor and
        // bounded tails so diagnostics cannot accidentally expose future secret-bearing fields.
        let logs = walkdir::WalkDir::new(workspace.path().join("state/managed/session-log"))
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file() && entry.file_name() == "records.jsonl")
            .take(4)
            .filter_map(|entry| fs::read_to_string(entry.path()).ok())
            .map(|log| {
                let tail = log
                    .char_indices()
                    .map(|(index, _)| index)
                    .find(|index| log.len().saturating_sub(*index) <= 16_000)
                    .unwrap_or(0);
                sigil_kernel::safe_persistence_text(&log[tail..])
            })
            .collect::<Vec<_>>();
        let request_diagnostics =
            sigil_kernel::safe_persistence_text(&serde_json::to_string(&requests)?);
        anyhow::bail!(
            "{error:#}; actual provider requests: {request_diagnostics}; durable events: {logs:?}"
        );
    }
    for (_, report) in cleanup {
        anyhow::ensure!(report?.success, "image server cleanup failed");
    }
    let expected = format!("data:image/png;base64,{PNG_BASE64}");
    let image_blocks = requests
        .iter()
        .filter_map(|request| request["input"].as_array())
        .flatten()
        .filter_map(|item| item["content"].as_array())
        .flatten()
        .filter(|block| block["type"] == "input_image")
        .collect::<Vec<_>>();
    anyhow::ensure!(
        image_blocks.len() == 1,
        "provider did not receive exactly one image: {}",
        image_blocks.len()
    );
    anyhow::ensure!(
        image_blocks[0]["image_url"] == expected,
        "actual provider image bytes changed"
    );
    Ok(())
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

fn approve_direct_task_background_child(
    server: &ServeProcess,
    token: &str,
    run_id: &str,
    session_id: &str,
) {
    let approval_deadline = Instant::now() + Duration::from_secs(10);
    let (pending, stream_sequence) = loop {
        let (status, body) = http_request(
            server.address,
            "GET",
            &format!("/runs/{run_id}"),
            Some(token),
            None,
        );
        assert_eq!(status, 200, "run snapshot should be readable: {body}");
        let snapshot: serde_json::Value =
            serde_json::from_str(&body).expect("run snapshot should be JSON");
        if let Some(pending) = snapshot["pending_approvals"].as_array().and_then(|items| {
            items
                .iter()
                .find(|item| item["call_id"].as_str() == Some("call-background-child"))
        }) {
            let stream_sequence = snapshot["stream_sequence"]
                .as_u64()
                .expect("approval snapshot should carry the current stream sequence");
            break (pending.clone(), stream_sequence);
        }
        assert!(
            Instant::now() < approval_deadline,
            "background child approval did not become visible: {snapshot}"
        );
        thread::sleep(Duration::from_millis(25));
    };
    let approval_body = serde_json::json!({
        "protocol_version": 2,
        "command_id": "approve-direct-task-background-child",
        "client_id": "direct-task-background-e2e",
        "session_id": session_id,
        "expected_stream_sequence": stream_sequence,
        "correlation_id": "direct-task-background-approval",
        "payload": {
            "approval_request_id": pending["approval_request_id"],
            "tool_call_hash": pending["tool_call_hash"],
            "policy_version": pending["policy_version"],
            "expires_at_ms": pending["expires_at_ms"],
            "decision": "approve"
        }
    })
    .to_string();
    let (status, receipt) = http_request(
        server.address,
        "POST",
        &format!("/runs/{run_id}/approvals/call-background-child"),
        Some(token),
        Some(&approval_body),
    );
    assert_eq!(status, 200, "background child approval failed: {receipt}");
    let approval: serde_json::Value =
        serde_json::from_str(&receipt).expect("approval receipt should be JSON");
    assert_eq!(approval["decision"]["decision"], "approved");
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
        "protocol_version": 2,
        "command_id": "start-process-1",
        "client_id": "process-e2e",
        "session_id": session_id,
        "payload": {
            "prompt": "Return the deterministic fixture answer",
            "permission_mode": "read-only"
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
    let owner_revision = run["foreground_owner"]["owner_revision"]
        .as_str()
        .expect("run receipt should include exact foreground ownership")
        .to_owned();

    let (events_status, events_body) = http_request_with_headers(
        server.address,
        "GET",
        &format!("/runs/{run_id}/events"),
        Some(token),
        None,
        None,
        Some((&session_id, &owner_revision)),
    );
    assert_eq!(events_status, 200);
    assert!(events_body.contains("event: run_event"));
    assert!(
        events_body.contains("\"type\":\"run_finished\""),
        "run stream omitted terminal event: {events_body}"
    );
    assert!(events_body.contains("serve process answer"));
    let last_event_id = events_body
        .lines()
        .filter_map(|line| line.strip_prefix("id: "))
        .next_back()
        .expect("terminal durable SSE should include a replay cursor");
    let (reconnect_status, reconnect_body) = http_request_with_headers(
        server.address,
        "GET",
        &format!("/runs/{run_id}/events"),
        Some(token),
        None,
        Some(last_event_id),
        Some((&session_id, &owner_revision)),
    );
    assert_eq!(reconnect_status, 409);
    assert!(
        reconnect_body.contains("run_no_longer_foreground"),
        "terminal live followers must use the durable transcript instead: {reconnect_body}"
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

    let restart_provider = restart_provider_fixture(&base_url, "reopened process answer");
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
    let (transcript_status, transcript_body) = http_request(
        restarted.address,
        "GET",
        &format!("/sessions/{reopened_session_id}/transcript?limit=1"),
        Some(token),
        None,
    );
    assert_eq!(
        transcript_status, 200,
        "transcript response: {transcript_body}"
    );
    let transcript: serde_json::Value =
        serde_json::from_str(&transcript_body).expect("transcript body should be JSON");
    assert!(
        transcript["total_messages"]
            .as_u64()
            .is_some_and(|count| count >= 2)
    );
    assert_eq!(transcript["messages"].as_array().map(Vec::len), Some(1));
    assert_eq!(transcript["messages"][0]["role"], "assistant");
    assert_eq!(transcript["messages"][0]["content"], "serve process answer");
    assert!(transcript["messages"][0].get("args_json").is_none());
    assert!(transcript.get("session_log_path").is_none());
    let resumed_command = serde_json::json!({
        "protocol_version": 2,
        "command_id": "start-process-reopened",
        "client_id": "process-e2e",
        "session_id": reopened_session_id,
        "payload": {
            "prompt": "Continue the durable session with the fixture answer",
            "permission_mode": "read-only"
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
    let resumed_owner_revision = resumed["foreground_owner"]["owner_revision"]
        .as_str()
        .expect("resumed receipt should include exact foreground ownership");
    let (events_status, events_body) = http_request_with_headers(
        restarted.address,
        "GET",
        &format!("/runs/{resumed_run_id}/events"),
        Some(token),
        None,
        None,
        Some((&reopened_session_id, resumed_owner_revision)),
    );
    assert_eq!(events_status, 200);
    assert!(events_body.contains("reopened process answer"));
    restart_provider
        .join()
        .expect("restart provider fixture should join");
    assert_eq!(stop_serve(restarted).status.code(), Some(0));

    fs::remove_dir_all(workspace).expect("test workspace should remove");
}

#[cfg(unix)]
#[test]
fn serve_process_auto_continues_direct_task_after_background_child_result() {
    let workspace = test_workspace("direct-task-background");
    let config_path = workspace.join("sigil.toml");
    let token = "direct-task-background-token";
    let DirectTaskProviderFixture {
        base_url,
        requests: provider_requests,
        stop: stop_provider,
        release_background_child,
        recovery_mode: _,
        worker: provider,
    } = spawn_direct_task_provider_fixture();
    write_auto_task_config(&config_path, &base_url);

    let mut server = spawn_serve(&workspace, &config_path, token);
    let (session_status, session_body) = http_request(
        server.address,
        "POST",
        "/sessions",
        Some(token),
        Some(r#"{"label":"Direct Task background E2E"}"#),
    );
    assert_eq!(session_status, 201, "session response: {session_body}");
    let session: serde_json::Value =
        serde_json::from_str(&session_body).expect("session response should be JSON");
    let session_id = session["id"]
        .as_str()
        .expect("session id should exist")
        .to_owned();

    let run_command = serde_json::json!({
        "protocol_version": 2,
        "command_id": "start-direct-task-background-e2e",
            "client_id": "direct-task-background-e2e",
            "session_id": session_id.clone(),
        "payload": {
            "prompt": "Use a delegated read-only child agent to inspect this workspace, then incorporate its result and report completion.",
            "permission_mode": "read-only"
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
    let initial_run: serde_json::Value =
        serde_json::from_str(&run_body).expect("run receipt should be JSON");
    let initial_run_id = initial_run["run"]["id"]
        .as_str()
        .expect("initial run id should exist")
        .to_owned();

    let deadline = Instant::now() + Duration::from_secs(45);
    let mut provider_turns = Vec::new();
    let mut approved_background_spawn = false;
    while provider_turns.len() < 6 {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let (category, request) = match provider_requests.recv_timeout(remaining) {
            Ok(turn) => turn,
            Err(error) => {
                let server_exit = server.child.try_wait().ok().flatten();
                let session_logs = walkdir::WalkDir::new(&workspace)
                    .into_iter()
                    .filter_map(Result::ok)
                    .filter(|entry| {
                        entry.file_type().is_file()
                            && entry.file_name().to_str() == Some("records.jsonl")
                    })
                    .filter_map(|entry| {
                        let path = entry.path().to_path_buf();
                        let contents = fs::read_to_string(&path).ok()?;
                        let start = contents
                            .char_indices()
                            .map(|(index, _)| index)
                            .find(|index| contents.len().saturating_sub(*index) <= 12_000)
                            .unwrap_or(0);
                        Some(format!("{}\n{}", path.display(), &contents[start..]))
                    })
                    .collect::<Vec<_>>()
                    .join("\n--- log ---\n");
                panic!(
                    "serve did not run all Task and continuation turns: {error}; workspace: {}; observed categories: {:?}; server exit: {server_exit:?}; stdout: {}; stderr: {}; recent durable logs: {session_logs}",
                    workspace.display(),
                    provider_turns
                        .iter()
                        .map(|(category, _)| category)
                        .collect::<Vec<_>>(),
                    fs::read_to_string(&server.stdout_path).unwrap_or_default(),
                    fs::read_to_string(&server.stderr_path).unwrap_or_default()
                );
            }
        };
        if category == "session_title" {
            continue;
        }
        if category == "background_child" {
            release_background_child
                .send(())
                .expect("scripted provider should release the completed E2E child");
        }
        let should_approve_background_spawn = category == "task_spawn";
        provider_turns.push((category, request));
        if should_approve_background_spawn && !approved_background_spawn {
            approve_direct_task_background_child(&server, token, &initial_run_id, &session_id);
            approved_background_spawn = true;
        }
    }
    assert_eq!(provider_turns.len(), 6);
    stop_provider
        .send(())
        .expect("scripted provider should accept its stop signal");
    provider
        .join()
        .expect("scripted provider should serve all Direct Task requests");

    let categories = provider_turns
        .iter()
        .map(|(category, _)| category.as_str())
        .collect::<Vec<_>>();
    for expected in [
        "routing",
        "task_spawn",
        "task_after_spawn",
        "background_child",
        "task_continuation_read_result",
        "task_continuation_final",
    ] {
        assert!(
            categories.contains(&expected),
            "serve skipped {expected}; observed provider turns: {categories:?}"
        );
    }
    let result_read_turn = provider_turns
        .iter()
        .find(|(category, _)| category == "task_continuation_final")
        .expect("Task should make a provider turn after reading the child result");
    assert!(
        result_read_turn
            .1
            .to_string()
            .contains("child research result"),
        "continuation should receive the result read from the background child: {}",
        result_read_turn.1
    );

    let session_log_root = workspace.join("state/managed/session-log");
    let session_path = walkdir::WalkDir::new(&session_log_root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry.file_type().is_file() && entry.file_name().to_str() == Some("records.jsonl")
        })
        .map(|entry| entry.path().to_path_buf())
        .find(|path| {
            sigil_kernel::JsonlSessionStore::read_entries(path)
                .ok()
                .is_some_and(|entries| {
                    sigil_kernel::Session::from_entries("openai_compat", "gpt-4.1", entries)
                        .task_state_projection()
                        .latest_task()
                        .is_some()
                })
        })
        .expect("parent session JSONL should exist");
    let task_deadline = Instant::now() + Duration::from_secs(15);
    let completed = loop {
        if let Ok(entries) = sigil_kernel::JsonlSessionStore::read_entries(&session_path) {
            let session = sigil_kernel::Session::from_entries("openai_compat", "gpt-4.1", entries);
            let tasks = session.task_state_projection();
            if let Some(task) = tasks.latest_task()
                && task.status == sigil_kernel::TaskRunStatus::Completed
            {
                let children = tasks.direct_task_background_agents(&task.task_id);
                let agent_threads = session.agent_thread_state_projection();
                let child_results_complete = !children.is_empty()
                    && children.iter().all(|thread_id| {
                        agent_threads.threads.get(thread_id).is_some_and(|thread| {
                            thread.status == sigil_kernel::AgentThreadStatus::Completed
                                && thread.result.is_some()
                        })
                    });
                if child_results_complete && task.direct_execution_attempts.len() >= 2 {
                    break true;
                }
            }
        }
        assert!(
            Instant::now() < task_deadline,
            "Task did not reach a completed state after the serve continuation"
        );
        thread::sleep(Duration::from_millis(50));
    };
    assert!(completed);

    assert_eq!(stop_serve(server).status.code(), Some(0));
    fs::remove_dir_all(workspace).expect("test workspace should remove");
}

#[cfg(unix)]
#[test]
fn serve_process_restart_interrupts_ownerless_direct_task_background_child() {
    let workspace = test_workspace("direct-task-background-restart");
    let config_path = workspace.join("sigil.toml");
    let token = "direct-task-background-restart-token";
    let DirectTaskProviderFixture {
        base_url,
        requests: provider_requests,
        stop: stop_provider,
        release_background_child,
        recovery_mode,
        worker: provider,
    } = spawn_direct_task_provider_fixture();
    write_auto_task_config(&config_path, &base_url);

    let mut server = spawn_serve(&workspace, &config_path, token);
    let (session_status, session_body) = http_request(
        server.address,
        "POST",
        "/sessions",
        Some(token),
        Some(r#"{"label":"Direct Task background restart"}"#),
    );
    assert_eq!(session_status, 201, "session response: {session_body}");
    let session: serde_json::Value =
        serde_json::from_str(&session_body).expect("session response should be JSON");
    let session_id = session["id"]
        .as_str()
        .expect("session id should exist")
        .to_owned();
    let durable_session_id = session["durable_session_scope_id"]
        .as_str()
        .expect("durable session id should exist")
        .to_owned();
    let run_command = serde_json::json!({
        "protocol_version": 2,
        "command_id": "start-direct-task-background-restart",
        "client_id": "direct-task-background-restart",
        "session_id": session_id,
        "payload": {
            "prompt": "Delegate a read-only child agent and use its result before completing.",
            "permission_mode": "read-only"
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
    let run: serde_json::Value = serde_json::from_str(&run_body).expect("run receipt JSON");
    let run_id = run["run"]["id"]
        .as_str()
        .expect("run id should exist")
        .to_owned();

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut saw_child_request = false;
    let mut saw_parent_wait = false;
    let mut approved_child = false;
    while !saw_child_request || !saw_parent_wait {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let (category, _) = provider_requests
            .recv_timeout(remaining)
            .expect("serve should start a child and let the Direct Task yield");
        if category == "session_title" {
            continue;
        }
        if category == "task_spawn" && !approved_child {
            approve_direct_task_background_child(&server, token, &run_id, &session_id);
            approved_child = true;
        }
        saw_child_request |= category == "background_child";
        saw_parent_wait |= category == "task_after_spawn";
        assert!(
            [
                "routing",
                "task_spawn",
                "task_after_spawn",
                "background_child",
            ]
            .contains(&category.as_str()),
            "unexpected provider turn before process restart: {category}"
        );
    }
    assert!(
        approved_child,
        "background child must be explicitly approved"
    );

    let blocked_deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (status, body) = http_request(
            server.address,
            "GET",
            &format!("/runs/{run_id}"),
            Some(token),
            None,
        );
        assert_eq!(status, 200, "run snapshot response: {body}");
        let snapshot: serde_json::Value =
            serde_json::from_str(&body).expect("run snapshot should be JSON");
        if snapshot["status"] == "blocked" {
            break;
        }
        assert!(
            Instant::now() < blocked_deadline,
            "parent run did not yield while the child response was held: {snapshot}"
        );
        thread::sleep(Duration::from_millis(25));
    }

    let session_log_root = workspace.join("state/managed/session-log");
    let session_path = walkdir::WalkDir::new(&session_log_root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry.file_type().is_file() && entry.file_name().to_str() == Some("records.jsonl")
        })
        .map(|entry| entry.path().to_path_buf())
        .find(|path| {
            sigil_kernel::JsonlSessionStore::read_entries(path)
                .ok()
                .is_some_and(|entries| {
                    sigil_kernel::Session::from_entries("openai_compat", "gpt-4.1", entries)
                        .task_state_projection()
                        .latest_task()
                        .is_some()
                })
        })
        .expect("parent session log should exist before process crash");
    let initial_entries = sigil_kernel::JsonlSessionStore::read_entries(&session_path)
        .expect("parent session should remain readable before process crash");
    let initial_session =
        sigil_kernel::Session::from_entries("openai_compat", "gpt-4.1", initial_entries);
    let interrupted_task_id = initial_session
        .task_state_projection()
        .latest_task()
        .expect("parent Direct Task should be durable before process crash")
        .task_id
        .clone();

    // The provider has received the child model request but cannot complete it
    // until the test opens the gate. Kill serve in this exact ownerless window.
    server
        .child
        .kill()
        .expect("serve process should accept crash simulation");
    let crash_status = server
        .child
        .wait()
        .expect("crashed serve process should be reaped");
    assert!(!crash_status.success(), "crash simulation must be abnormal");
    drop(server);
    release_background_child
        .send(())
        .expect("scripted provider should release the abandoned child request");

    recovery_mode.store(true, Ordering::SeqCst);
    let restarted = spawn_serve(&workspace, &config_path, token);
    let (catalog_status, catalog_body) = http_request(
        restarted.address,
        "GET",
        "/session-catalog",
        Some(token),
        None,
    );
    assert_eq!(catalog_status, 200, "session catalog: {catalog_body}");
    let catalog: serde_json::Value =
        serde_json::from_str(&catalog_body).expect("session catalog should be JSON");
    let historical = catalog["entries"]
        .as_array()
        .and_then(|entries| {
            entries
                .iter()
                .find(|entry| entry["session_id"].as_str() == Some(&durable_session_id))
        })
        .expect("interrupted durable session should be discoverable after restart");
    let open_body = serde_json::json!({
        "session_ref": historical["session_ref"],
        "session_id": durable_session_id,
        "label": "Recovered Direct Task"
    })
    .to_string();
    let (open_status, open_response) = http_request(
        restarted.address,
        "POST",
        "/sessions/open",
        Some(token),
        Some(&open_body),
    );
    assert_eq!(open_status, 200, "session reopen response: {open_response}");
    let reopened: serde_json::Value =
        serde_json::from_str(&open_response).expect("reopened session should be JSON");
    let reopened_session_id = reopened["id"]
        .as_str()
        .expect("reopened adapter session id should exist");
    let recovery_command = serde_json::json!({
        "protocol_version": 2,
        "command_id": "start-after-direct-task-background-restart",
        "client_id": "direct-task-background-restart",
        "session_id": reopened_session_id,
        "payload": {
            "prompt": "Finish this new session turn after recovering prior work.",
            "permission_mode": "read-only"
        }
    })
    .to_string();
    let (recovery_start_status, recovery_start_body) = http_request(
        restarted.address,
        "POST",
        &format!("/sessions/{reopened_session_id}/runs"),
        Some(token),
        Some(&recovery_command),
    );
    assert_eq!(
        recovery_start_status, 201,
        "post-restart run should trigger owner rescan: {recovery_start_body}"
    );
    let recovery_run: serde_json::Value =
        serde_json::from_str(&recovery_start_body).expect("recovery run receipt should be JSON");
    let recovery_run_id = recovery_run["run"]["id"]
        .as_str()
        .expect("post-restart run id should exist");
    let recovery_turn_deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let remaining = recovery_turn_deadline.saturating_duration_since(Instant::now());
        let (category, _) = provider_requests
            .recv_timeout(remaining)
            .expect("post-restart session run should reach its scripted provider turn");
        if category == "session_title" {
            continue;
        }
        assert_eq!(
            category, "recovery_turn",
            "reopening may start the explicit new turn, but must not replay the old child"
        );
        break;
    }
    let recovery_run_deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (status, body) = http_request(
            restarted.address,
            "GET",
            &format!("/runs/{recovery_run_id}"),
            Some(token),
            None,
        );
        assert_eq!(status, 200, "post-restart run snapshot: {body}");
        let snapshot: serde_json::Value =
            serde_json::from_str(&body).expect("post-restart run snapshot should be JSON");
        if snapshot["status"] == "finished" {
            break;
        }
        assert!(
            Instant::now() < recovery_run_deadline,
            "explicit post-restart run did not finish: {snapshot}"
        );
        thread::sleep(Duration::from_millis(25));
    }

    let child_id = sigil_runtime::chat_agent_thread_id_for_call(
        "call-background-child",
        &sigil_kernel::AgentProfileId::new("explore").expect("explore profile id"),
    )
    .expect("child thread id should be deterministic");
    let recovery_deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let entries = sigil_kernel::JsonlSessionStore::read_entries(&session_path)
            .expect("parent session should remain readable after restart");
        let recovered = sigil_kernel::Session::from_entries("openai_compat", "gpt-4.1", entries);
        let tasks = recovered.task_state_projection();
        let agents = recovered.agent_thread_state_projection();
        let child = agents.threads.get(&child_id);
        let task_state = tasks.tasks.get(&interrupted_task_id).map(|task| {
            (
                task.task_id.clone(),
                task.status,
                task.direct_execution_admission.is_some(),
                task.latest_plan_version,
            )
        });
        let direct_children = tasks.direct_task_background_agents(&interrupted_task_id);
        let direct_child_statuses = direct_children
            .iter()
            .map(|thread_id| (thread_id.clone(), tasks.agent_thread_status(thread_id)))
            .collect::<Vec<_>>();
        let all_task_states = tasks
            .tasks
            .values()
            .map(|task| (task.task_id.clone(), task.status))
            .collect::<Vec<_>>();
        let thread_states = agents
            .threads
            .iter()
            .map(|(thread_id, thread)| (thread_id.clone(), thread.status, thread.result.is_some()))
            .collect::<Vec<_>>();
        let child_status_history = recovered
            .entries()
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| match entry {
                sigil_kernel::SessionLogEntry::Control(
                    sigil_kernel::ControlEntry::AgentThreadStarted(entry),
                ) if entry.thread_id == child_id => Some((index, "started".to_owned())),
                sigil_kernel::SessionLogEntry::Control(
                    sigil_kernel::ControlEntry::AgentThreadStatusChanged(entry),
                ) if entry.thread_id == child_id => {
                    Some((index, format!("status:{:?}", entry.status)))
                }
                sigil_kernel::SessionLogEntry::Control(
                    sigil_kernel::ControlEntry::AgentThreadResultRecorded(entry),
                ) if entry.result.thread_id == child_id => {
                    Some((index, format!("result:{:?}", entry.result.status)))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        if task_state
            .as_ref()
            .is_some_and(|(_, status, _, _)| *status == sigil_kernel::TaskRunStatus::Interrupted)
            && child.is_some_and(|thread| {
                thread.status == sigil_kernel::AgentThreadStatus::Interrupted
                    && thread.result.is_none()
            })
        {
            break;
        }
        assert!(
            Instant::now() < recovery_deadline,
            "restart must durably interrupt the ownerless Task and child; target_task={task_state:?}, direct_children={direct_children:?}, child_task_statuses={direct_child_statuses:?}, all_tasks={all_task_states:?}, threads={thread_states:?}, child_history={child_status_history:?}, session_log={}",
            session_path.display()
        );
        thread::sleep(Duration::from_millis(25));
    }

    let no_replay_deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < no_replay_deadline {
        if let Ok((category, _)) = provider_requests.recv_timeout(Duration::from_millis(25)) {
            assert_eq!(
                category, "session_title",
                "restart must not rerun an ownerless Direct Task or child"
            );
        }
    }
    stop_provider
        .send(())
        .expect("scripted provider should stop after recovery verification");
    provider
        .join()
        .expect("scripted provider fixture should finish");
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
        common::isolated_child_environment(&workspace)
            .expect("serve child environment should create")
            .apply_to_command(&mut command);
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
            !http_server_state_root(&workspace, "http-server-v4").exists(),
            "{name} must fail before creating listener state"
        );
    }

    fs::remove_dir_all(workspace).expect("test workspace should remove");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_review_serve_contract_sends_outdated_exact_diff_without_restoring_files()
-> anyhow::Result<()> {
    use anyhow::Context as _;
    async fn finish(client: &sigil_desktop::DesktopHttpClient, run_id: &str) -> anyhow::Result<()> {
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let run = client.run(run_id).await?;
                if run.status.is_terminal() {
                    anyhow::ensure!(
                        run.status == sigil_desktop::DesktopRunStatus::Finished,
                        "review fixture run failed: {run:?}"
                    );
                    return Ok::<_, anyhow::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await??;
        Ok(())
    }
    let workspace = tempfile::tempdir()?;
    let config_path = workspace.path().join("sigil.toml");
    let item = serde_json::json!({ "id": "review-function-item", "type": "function_call", "call_id": "review-write-call", "name": "write_file", "arguments": serde_json::to_string(&serde_json::json!({"path":"note.txt","content":"recorded line one\nrecorded line two\n"}))? });
    let first_response = format!(
        "event: response.output_item.added\ndata: {}\n\nevent: response.output_item.done\ndata: {}\n\nevent: response.completed\ndata: {}\n\n",
        serde_json::json!({"item":item}),
        serde_json::json!({"item":item}),
        serde_json::json!({"response":{"id":"review-write","status":"completed","output":[item]}})
    );
    let provider = VisionProviderFixture::start_with_first_response(Some(first_response))?;
    write_config(&config_path, &provider.base_url);
    fs::write(
        &config_path,
        fs::read_to_string(&config_path)?.replace("chat_completions", "responses"),
    )?;
    let manager = sigil_desktop::DesktopWorkspaceManager::default();
    let result: anyhow::Result<()> = async {
        let opened = manager.open(sigil_desktop::DesktopWorkspaceOpenRequest::new(sigil_desktop::DesktopLaunchRequest::new(env!("CARGO_BIN_EXE_sigil"), &config_path, workspace.path()), "review contract")).await?;
        let client = manager.client(&opened.id)?;
        let session = client.create_session(sigil_desktop::DesktopSessionCreateRequest { label: Some("recorded change".to_owned()), model_ref: None }).await?;
        let mut request = sigil_desktop::DesktopRunStartRequest {
            review_annotations: Vec::new(), image_attachments: Vec::new(), prompt: "Create note.txt with two lines".to_owned(),
            permission_mode: sigil_desktop::DesktopPermissionMode::AutoEdit, model_ref: None,
            model_selection_binding: None, route_recovery_binding: None, reasoning_effort: None,
            reasoning_effort_binding: None, skill_binding: None, agent_binding: None, task_continuation: None,
        };
        let started = client.start_run(&session.id, request.clone()).await?;
        finish(&client, &started.run.id).await?;
        assert_eq!(fs::read_to_string(workspace.path().join("note.txt"))?, "recorded line one\nrecorded line two\n");
        let checkpoint = client.conversation_recovery(&session.id).await?.checkpoints.into_iter().next().context("real write checkpoint")?;
        let selector = sigil_desktop::DesktopCheckpointRestoreRequest { checkpoint_id: checkpoint.checkpoint_id, checkpoint_digest: checkpoint.checkpoint_digest };
        let original = client.checkpoint_review(&session.id, selector.clone()).await?;
        let recorded = original.diffs.iter().find(|diff| diff.path == "note.txt").context("recorded forward diff")?;
        assert_eq!(recorded.file_state, sigil_desktop::DesktopReviewFileState::Current);
        fs::write(workspace.path().join("note.txt"), "unrelated newer content\n")?;
        let outdated = client.checkpoint_review(&session.id, selector.clone()).await?;
        assert_eq!(outdated.diffs.iter().find(|diff| diff.path == "note.txt").context("outdated diff")?.file_state, sigil_desktop::DesktopReviewFileState::Changed);
        assert!(!client.checkpoint_restore_review(&session.id, selector).await?.ready);
        request.prompt = "Explain this original line".to_owned();
        request.permission_mode = sigil_desktop::DesktopPermissionMode::ReadOnly;
        request.review_annotations = vec![sigil_desktop::DesktopReviewAnnotation {
            checkpoint_id: original.checkpoint_id, checkpoint_digest: original.checkpoint_digest,
            source_call_id: recorded.source_call_id.clone(), diff_digest: recorded.diff_digest.clone(), path: recorded.path.clone(),
            side: sigil_desktop::DesktopReviewDiffSide::New, start_line: 1, end_line: 1,
            comment: sigil_desktop::DesktopReviewComment::new("Explain this original line")?,
        }];
        let reviewed = client.start_run(&session.id, request).await?;
        finish(&client, &reviewed.run.id).await?;
        assert_eq!(fs::read_to_string(workspace.path().join("note.txt"))?, "unrelated newer content\n");
        let display = client.conversation_display(&session.id, &Default::default()).await?;
        assert!(display.items.iter().any(|item| matches!(&item.content, sigil_desktop::DesktopConversationDisplayContent::Message { text: Some(text), .. } if text.contains("User review of recorded change") && text.contains("recorded line one"))));
        Ok(())
    }.await;
    let cleanup = manager.close_all().await;
    let requests = provider.finish()?;
    result?;
    anyhow::ensure!(
        cleanup
            .iter()
            .all(|(_, result)| result.as_ref().is_ok_and(|report| report.success)),
        "review serve cleanup failed"
    );
    let run_requests = explicit_provider_requests(&requests);
    assert_eq!(
        run_requests.len(),
        3,
        "one write/tool-result turn then one explicitly sent review"
    );
    assert!(
        requests.len() - run_requests.len() <= 1,
        "at most one title maintenance request"
    );
    let review_request =
        serde_json::to_string(run_requests.last().context("provider review request")?)?;
    assert!(review_request.contains("User review of recorded change"));
    assert!(review_request.contains("recorded line one"));
    assert!(review_request.contains("Explain this original line"));
    assert!(review_request.contains("no file-write authority"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_message_fork_uses_managed_catalog_identity_without_starting_a_run()
-> anyhow::Result<()> {
    use anyhow::Context as _;
    let workspace = tempfile::tempdir()?;
    let config_path = workspace.path().join("sigil.toml");
    let provider = VisionProviderFixture::start()?;
    write_config(&config_path, &provider.base_url);
    fs::write(
        &config_path,
        fs::read_to_string(&config_path)?.replace("chat_completions", "responses"),
    )?;
    let manager = sigil_desktop::DesktopWorkspaceManager::default();
    let result: anyhow::Result<()> = async {
        let opened = manager.open(sigil_desktop::DesktopWorkspaceOpenRequest::new(
            sigil_desktop::DesktopLaunchRequest::new(env!("CARGO_BIN_EXE_sigil"), &config_path, workspace.path()), "branch contract",
        )).await?;
        let client = manager.client(&opened.id)?;
        let session = client.create_session(sigil_desktop::DesktopSessionCreateRequest { label: Some("parent".to_owned()), model_ref: None }).await?;
        let receipt = client.start_run(&session.id, sigil_desktop::DesktopRunStartRequest {
            review_annotations: Vec::new(), image_attachments: Vec::new(), prompt: "Inspect this branch source".to_owned(),
            permission_mode: sigil_desktop::DesktopPermissionMode::ReadOnly,
            model_ref: None, model_selection_binding: None, route_recovery_binding: None,
            reasoning_effort: None, reasoning_effort_binding: None, skill_binding: None,
            agent_binding: None, task_continuation: None,
        }).await?;
        let point = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let run = client.run(&receipt.run.id).await?;
                if run.status.is_terminal() {
                    anyhow::ensure!(run.status == sigil_desktop::DesktopRunStatus::Finished, "fork source run failed: {run:?}");
                    let recovery = client.conversation_recovery(&session.id).await?;
                    if let Some(point) = recovery.fork_points.into_iter().next() { break Ok::<_, anyhow::Error>(point); }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await??;
        anyhow::ensure!(point.prompt_preview.as_deref() == Some("Inspect this branch source"));
        let catalog = client.catalog(&Default::default()).await?;
        let source = catalog.entries.iter().find(|entry| entry.session_id.as_deref() == Some(&session.durable_session_scope_id)).context("managed parent catalog entry")?;
        anyhow::ensure!(source.session_ref != "records.jsonl", "fixture must exercise managed logical refs");
        anyhow::ensure!(walkdir::WalkDir::new(workspace.path().join("state/managed/session-log")).into_iter().filter_map(Result::ok).any(|entry| entry.file_name() == "records.jsonl"), "fixture never used managed records.jsonl layout");
        let model_ref = client.run_context(&session.id).await?.model_ref;
        let forked = client.command_conversation_recovery(&session.id, sigil_desktop::DesktopConversationRecoveryCommandAction::ForkConversation {
            source_turn_digest: point.source_turn_digest, model_ref,
        }).await?.fork.context("fork receipt")?;
        anyhow::ensure!(forked.session_id != session.durable_session_scope_id && forked.copied_message_count >= 2);
        let branch = client.open_session(sigil_desktop::DesktopSessionOpenRequest {
            session_ref: forked.session_ref, session_id: forked.session_id, label: Some("branch".to_owned()), recovery_binding: None,
        }).await?;
        anyhow::ensure!(branch.run_ids.is_empty(), "opening a branch must not start a model run");
        let display = client.conversation_display(&branch.id, &Default::default()).await?;
        anyhow::ensure!(display.items.iter().any(|item| matches!(&item.content, sigil_desktop::DesktopConversationDisplayContent::Message { text: Some(text), .. } if text == "Inspect this branch source")));
        Ok(())
    }.await;
    let cleanup = manager.close_all().await;
    let requests = provider.finish()?;
    result?;
    anyhow::ensure!(
        cleanup
            .iter()
            .all(|(_, result)| result.as_ref().is_ok_and(|report| report.success)),
        "managed fork server did not settle"
    );
    let run_requests = explicit_provider_requests(&requests);
    anyhow::ensure!(
        run_requests.len() == 1,
        "fork/open unexpectedly invoked the provider: {} explicit requests; {} title requests",
        run_requests.len(),
        requests.len() - run_requests.len()
    );
    assert!(
        requests.len() - run_requests.len() <= 1,
        "at most one parent title maintenance request"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_branch_knowledge_serve_contract_preserves_lineage_and_imports_once()
-> anyhow::Result<()> {
    use anyhow::Context as _;
    async fn start_turn(
        client: &sigil_desktop::DesktopHttpClient,
        session_id: &str,
        prompt: &str,
    ) -> anyhow::Result<String> {
        let receipt = client
            .start_run(
                session_id,
                sigil_desktop::DesktopRunStartRequest {
                    review_annotations: Vec::new(),
                    image_attachments: Vec::new(),
                    prompt: prompt.to_owned(),
                    permission_mode: sigil_desktop::DesktopPermissionMode::ReadOnly,
                    model_ref: None,
                    model_selection_binding: None,
                    route_recovery_binding: None,
                    reasoning_effort: None,
                    reasoning_effort_binding: None,
                    skill_binding: None,
                    agent_binding: None,
                    task_continuation: None,
                },
            )
            .await?;
        Ok(receipt.run.id)
    }
    async fn finish_turn(
        client: &sigil_desktop::DesktopHttpClient,
        run_id: &str,
    ) -> anyhow::Result<()> {
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let run = client.run(run_id).await?;
                if run.status.is_terminal() {
                    anyhow::ensure!(
                        run.status == sigil_desktop::DesktopRunStatus::Finished,
                        "branch fixture run failed: {run:?}"
                    );
                    return Ok::<_, anyhow::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await??;
        Ok(())
    }
    async fn turn(
        client: &sigil_desktop::DesktopHttpClient,
        session_id: &str,
        prompt: &str,
    ) -> anyhow::Result<()> {
        let run_id = start_turn(client, session_id, prompt).await?;
        finish_turn(client, &run_id).await
    }
    let workspace = tempfile::tempdir()?;
    let config_path = workspace.path().join("sigil.toml");
    let (entered_sender, entered) = tokio::sync::oneshot::channel();
    let (release_sender, release_receiver) = std::sync::mpsc::channel();
    let provider = VisionProviderFixture::start_with_options(
        Vec::new(),
        Some(ProviderRequestGate {
            run_request_index: 3,
            entered: entered_sender,
            release: release_receiver,
        }),
    )?;
    write_config(&config_path, &provider.base_url);
    fs::write(
        &config_path,
        fs::read_to_string(&config_path)?.replace("chat_completions", "responses"),
    )?;
    let manager = sigil_desktop::DesktopWorkspaceManager::default();
    // On assertion failure this releases the provider before manager/provider teardown.
    let mut gate_release = ProviderGateRelease(Some(release_sender));
    let result: anyhow::Result<()> = async {
        let opened = manager.open(sigil_desktop::DesktopWorkspaceOpenRequest::new(
            sigil_desktop::DesktopLaunchRequest::new(env!("CARGO_BIN_EXE_sigil"), &config_path, workspace.path()), "branch knowledge contract",
        )).await?;
        let client = manager.client(&opened.id)?;
        let parent = client.create_session(sigil_desktop::DesktopSessionCreateRequest { label: Some("parent".to_owned()), model_ref: None }).await?;
        turn(&client, &parent.id, "Explore the parent approach").await?;
        let point = client.conversation_recovery(&parent.id).await?.fork_points.into_iter().next().context("source turn")?;
        let model_ref = client.run_context(&parent.id).await?.model_ref;
        let forked = client.command_conversation_recovery(&parent.id, sigil_desktop::DesktopConversationRecoveryCommandAction::ForkConversation { source_turn_digest: point.source_turn_digest, model_ref }).await?.fork.context("branch receipt")?;
        let branch = client.open_session(sigil_desktop::DesktopSessionOpenRequest { session_ref: forked.session_ref.clone(), session_id: forked.session_id.clone(), label: Some("alternative".to_owned()), recovery_binding: None }).await?;
        turn(&client, &branch.id, "Conclude the branch investigation").await?;
        let parent_lineage = client.branch_lineage(&parent.id).await?;
        assert!(parent_lineage.children.iter().any(|link| link.session_ref == forked.session_ref && link.session_id == forked.session_id));
        let branch_lineage = client.branch_lineage(&branch.id).await?;
        let source_parent = branch_lineage.parent.context("parent link")?;
        assert_eq!(source_parent.session_id, parent.durable_session_scope_id);
        let preview = client.branch_knowledge_preview(&parent.id, sigil_desktop::DesktopBranchKnowledgeSource { source_session_ref: forked.session_ref.clone(), source_session_id: forked.session_id }).await?;
        let conclusion = preview.points.last().context("completed branch conclusion")?;
        assert_eq!(conclusion.summary, "Image received.");
        let selection = sigil_desktop::DesktopBranchKnowledgeImport {
            source_session_ref: preview.source_session_ref, source_session_id: preview.source_session_id,
            source_turn_digest: conclusion.source_turn_digest.clone(), source_message_id: conclusion.source_message_id.clone(),
            source_text_sha256: conclusion.source_text_sha256.clone(), summary_sha256: conclusion.summary_sha256.clone(),
        };
        // Hold a real provider request so import executes through an active session owner.
        let active_run = start_turn(&client, &parent.id, "An unrelated update in the parent").await?;
        tokio::time::timeout(Duration::from_secs(10), entered).await??;
        assert_eq!(client.run(&active_run).await?.status, sigil_desktop::DesktopRunStatus::Running);
        let run_ids = client.session(&parent.id).await?.run_ids;
        let first = client.command_conversation_recovery(&parent.id, sigil_desktop::DesktopConversationRecoveryCommandAction::ImportBranchKnowledge { selection: selection.clone() }).await?.branch_knowledge.context("import receipt")?;
        assert!(!first.already_imported);
        let replay = client.command_conversation_recovery(&parent.id, sigil_desktop::DesktopConversationRecoveryCommandAction::ImportBranchKnowledge { selection: selection.clone() }).await?.branch_knowledge.context("duplicate receipt")?;
        assert!(replay.already_imported);
        assert_eq!(replay.import_id, first.import_id);
        assert_eq!(client.session(&parent.id).await?.run_ids, run_ids, "import must not start a run");
        let mut altered = selection.clone();
        altered.summary_sha256 = format!("sha256:{}", "0".repeat(64));
        assert!(client.command_conversation_recovery(&parent.id, sigil_desktop::DesktopConversationRecoveryCommandAction::ImportBranchKnowledge { selection: altered }).await.is_err(), "unrelated target activity is allowed; changed source binding is not");
        assert_eq!(client.run(&active_run).await?.status, sigil_desktop::DesktopRunStatus::Running, "import must neither stop nor replace the held run");
        gate_release.release();
        finish_turn(&client, &active_run).await?;
        manager.restart(&opened.id).await?;
        let client = manager.client(&opened.id)?;
        let restored = client.open_session(sigil_desktop::DesktopSessionOpenRequest { session_ref: source_parent.session_ref, session_id: source_parent.session_id, label: None, recovery_binding: None }).await?;
        let recovered = client.command_conversation_recovery(&restored.id, sigil_desktop::DesktopConversationRecoveryCommandAction::ImportBranchKnowledge { selection }).await?.branch_knowledge.context("recovered import receipt")?;
        assert!(recovered.already_imported);
        assert_eq!(recovered.import_id, first.import_id);
        turn(&client, &restored.id, "Use the selected branch conclusion as reference").await?;
        Ok(())
    }.await;
    gate_release.release();
    let cleanup = manager.close_all().await;
    let requests = provider.finish()?;
    result?;
    anyhow::ensure!(
        cleanup
            .iter()
            .all(|(_, result)| result.as_ref().is_ok_and(|report| report.success)),
        "branch knowledge server cleanup failed"
    );
    let run_requests = explicit_provider_requests(&requests);
    assert_eq!(
        run_requests.len(),
        4,
        "only the four explicitly requested turns may call the provider"
    );
    assert!(
        requests.len() - run_requests.len() <= 2,
        "at most one title per parent and branch"
    );
    let last = serde_json::to_string(run_requests.last().context("final explicit request")?)?;
    assert_eq!(
        last.matches("User-selected branch conclusion from session")
            .count(),
        1,
        "one durable import must produce one untrusted context snippet"
    );
    assert!(last.contains("Unverified external knowledge"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_review_serve_contract_applies_approved_edit_and_passes_independent_check()
-> anyhow::Result<()> {
    use anyhow::Context as _;
    fn tool_response(call_id: &str, name: &str, args: serde_json::Value) -> anyhow::Result<String> {
        let item = serde_json::json!({"id":format!("item-{call_id}"),"type":"function_call","call_id":call_id,"name":name,"arguments":serde_json::to_string(&args)?});
        Ok(format!(
            "event: response.output_item.added\ndata: {}\n\nevent: response.output_item.done\ndata: {}\n\nevent: response.completed\ndata: {}\n\n",
            serde_json::json!({"item":item}),
            serde_json::json!({"item":item}),
            serde_json::json!({"response":{"id":format!("response-{call_id}"),"status":"completed","output":[item]}})
        ))
    }
    fn check_sum(workspace: &Path) -> anyhow::Result<Output> {
        let mut command = Command::new("python3");
        common::isolated_child_environment(workspace)?.apply_to_command(&mut command);
        command.current_dir(workspace).args(["-I", "-c", "import runpy; total=runpy.run_path('sum_values.py')['total']; assert total([])==0; assert total([2,3])==5; print('independent sum checks passed')"]);
        Ok(command.output()?)
    }
    async fn finish_with_exact_approval(
        client: &sigil_desktop::DesktopHttpClient,
        session_id: &str,
        run_id: &str,
        workspace: &Path,
    ) -> anyhow::Result<Vec<String>> {
        let mut approved = Vec::new();
        tokio::time::timeout(Duration::from_secs(25), async {
            loop {
                let run = client.run(run_id).await?;
                if run.status.is_terminal() {
                    anyhow::ensure!(
                        run.status == sigil_desktop::DesktopRunStatus::Finished,
                        "review repair run failed: {run:?}"
                    );
                    break Ok::<_, anyhow::Error>(());
                }
                for pending in run.pending_approvals {
                    if approved.contains(&pending.call_id) {
                        continue;
                    }
                    let expected = match pending.call_id.as_str() {
                        "review-create-bug" => "write_file",
                        "review-read-current" => "read_file",
                        "review-apply-fix" => "edit_file",
                        _ => anyhow::bail!("unexpected review fixture approval: {pending:?}"),
                    };
                    anyhow::ensure!(pending.tool_name == expected, "approval call changed tools");
                    if pending.call_id == "review-apply-fix" {
                        anyhow::ensure!(
                            !check_sum(workspace)?.status.success(),
                            "the file changed before explicit edit approval"
                        );
                    }
                    let receipt = client
                        .resolve_approval(
                            session_id,
                            run_id,
                            &pending.call_id,
                            run.stream_sequence,
                            sigil_desktop::DesktopApprovalDecisionRequest {
                                approval_request_id: pending.approval_request_id,
                                tool_call_hash: pending.tool_call_hash,
                                policy_version: pending.policy_version,
                                expires_at_ms: pending.expires_at_ms,
                                decision: sigil_desktop::DesktopApprovalDecision::Approve,
                                family_pattern: None,
                                reason: Some("exact isolated review fixture operation".to_owned()),
                            },
                        )
                        .await?;
                    assert_eq!(receipt.call_id, pending.call_id);
                    approved.push(pending.call_id);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await??;
        Ok(approved)
    }
    let workspace = tempfile::tempdir()?;
    let config_path = workspace.path().join("sigil.toml");
    let provider = VisionProviderFixture::start_with_responses(vec![
        Some(tool_response(
            "review-create-bug",
            "write_file",
            serde_json::json!({"path":"sum_values.py","content":"def total(values):\n    return sum(values) + 1\n"}),
        )?),
        None,
        Some(tool_response(
            "review-read-current",
            "read_file",
            serde_json::json!({"path":"sum_values.py"}),
        )?),
        Some(tool_response(
            "review-apply-fix",
            "edit_file",
            serde_json::json!({"path":"sum_values.py","old_text":"return sum(values) + 1","new_text":"return sum(values)"}),
        )?),
        None,
    ])?;
    write_config(&config_path, &provider.base_url);
    fs::write(
        &config_path,
        fs::read_to_string(&config_path)?.replace("chat_completions", "responses"),
    )?;
    let manager = sigil_desktop::DesktopWorkspaceManager::default();
    let result: anyhow::Result<()> = async {
        let opened = manager.open(sigil_desktop::DesktopWorkspaceOpenRequest::new(sigil_desktop::DesktopLaunchRequest::new(env!("CARGO_BIN_EXE_sigil"), &config_path, workspace.path()), "review repair contract")).await?;
        let client = manager.client(&opened.id)?;
        let session = client.create_session(sigil_desktop::DesktopSessionCreateRequest { label: Some("review repair".to_owned()), model_ref: None }).await?;
        let mut request = sigil_desktop::DesktopRunStartRequest {
            review_annotations: Vec::new(), image_attachments: Vec::new(), prompt: "Implement total(values) in sum_values.py".to_owned(), permission_mode: sigil_desktop::DesktopPermissionMode::Manual,
            model_ref: None, model_selection_binding: None, route_recovery_binding: None, reasoning_effort: None, reasoning_effort_binding: None, skill_binding: None, agent_binding: None, task_continuation: None,
        };
        let initial = client.start_run(&session.id, request.clone()).await?;
        let approved = finish_with_exact_approval(&client, &session.id, &initial.run.id, workspace.path()).await?;
        assert!(approved.iter().any(|call| call == "review-create-bug"));
        assert!(!check_sum(workspace.path())?.status.success(), "independent baseline must expose the actual bug");
        let checkpoint = client.conversation_recovery(&session.id).await?.checkpoints.into_iter().next().context("created file checkpoint")?;
        let review = client.checkpoint_review(&session.id, sigil_desktop::DesktopCheckpointRestoreRequest { checkpoint_id: checkpoint.checkpoint_id, checkpoint_digest: checkpoint.checkpoint_digest }).await?;
        let diff = review.diffs.iter().find(|diff| diff.path == "sum_values.py").context("actual forward diff")?;
        assert!(diff.lines.iter().any(|line| line.new_line == Some(2) && line.text.contains("+ 1")));
        request.prompt = "Apply the selected review comment after reading the current file".to_owned();
        request.review_annotations = vec![sigil_desktop::DesktopReviewAnnotation {
            checkpoint_id: review.checkpoint_id, checkpoint_digest: review.checkpoint_digest, source_call_id: diff.source_call_id.clone(), diff_digest: diff.diff_digest.clone(), path: diff.path.clone(),
            side: sigil_desktop::DesktopReviewDiffSide::New, start_line: 2, end_line: 2,
            comment: sigil_desktop::DesktopReviewComment::new("The extra +1 is a bug. Return the sum without an offset.")?,
        }];
        let edited = client.start_run(&session.id, request).await?;
        let approved = finish_with_exact_approval(&client, &session.id, &edited.run.id, workspace.path()).await?;
        assert!(approved.iter().any(|call| call == "review-apply-fix"), "the actual edit must require and receive exact user approval");
        let check = check_sum(workspace.path())?;
        anyhow::ensure!(check.status.success(), "independent repaired check failed: {}", String::from_utf8_lossy(&check.stderr));
        assert!(String::from_utf8_lossy(&check.stdout).contains("independent sum checks passed"));
        let catalog = client.catalog(&Default::default()).await?;
        let entry = catalog.entries.iter().find(|entry| entry.session_id.as_deref() == Some(&session.durable_session_scope_id)).context("managed session catalog")?;
        let path = workspace.path().join("state/managed/session-log").join(Path::new(&entry.session_ref).file_stem().context("managed session key")?).join("records.jsonl");
        let records = sigil_kernel::SessionRecordReadHandle::open_existing_observer(path)?.read_event_records()?;
        assert!(records.iter().filter_map(|record| record.session_log_entry().ok().flatten()).any(|entry| matches!(entry, sigil_kernel::SessionLogEntry::Control(sigil_kernel::ControlEntry::ToolExecution(execution)) if execution.call_id == "review-apply-fix" && execution.tool_name == "edit_file" && execution.status == sigil_kernel::ToolExecutionStatus::Completed)));
        assert!(client.conversation_recovery(&session.id).await?.checkpoints.len() >= 2, "real edit must produce its own durable checkpoint");
        Ok(())
    }.await;
    let cleanup = manager.close_all().await;
    let requests = provider.finish()?;
    result?;
    anyhow::ensure!(
        cleanup
            .iter()
            .all(|(_, result)| result.as_ref().is_ok_and(|report| report.success)),
        "review repair serve owner did not settle"
    );
    let run_requests = explicit_provider_requests(&requests);
    assert_eq!(
        run_requests.len(),
        5,
        "create/result, then review/read/edit/result must run exactly once"
    );
    assert!(
        requests.len() - run_requests.len() <= 1,
        "at most one title maintenance request"
    );
    let review = serde_json::to_string(run_requests[2])?;
    assert!(
        review.contains("User review of recorded change")
            && review.contains("The extra +1 is a bug")
            && review.contains("sum(values) + 1")
    );
    let edited = serde_json::to_string(run_requests[4])?;
    assert!(edited.contains("review-apply-fix") && edited.contains("function_call_output"));
    Ok(())
}
