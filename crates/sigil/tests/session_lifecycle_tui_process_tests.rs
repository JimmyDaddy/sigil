#![cfg(unix)]

use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use serde_json::json;
use sigil_kernel::{
    AssistantMessageKind, ControlEntry, DurableEventType, EventClass, JsonlSessionStore,
    ModelMessage, ResolvedModelRoute, RootConfig, Session, WorkspaceTrust,
    WorkspaceTrustDecisionEntry, stable_workspace_id,
};
use sigil_runtime::{SessionExportV1, resolve_sigil_paths};

const PROCESS_TIMEOUT: Duration = Duration::from_secs(15);

fn test_workspace() -> Result<PathBuf> {
    let path = std::env::temp_dir().join(format!(
        "sigil-session-lifecycle-tui-{}",
        uuid::Uuid::new_v4()
    ));
    fs::create_dir_all(&path)?;
    Ok(path)
}

fn write_config(path: &Path, workspace: &Path, session_dir: &Path) -> Result<()> {
    let config = format!(
        r#"config_version = 2

[workspace]
root = "{}"

[storage]
state_root = "{}"
cache_root = "{}"

[session]
log_dir = "{}"

[agent]
connection = "deepseek-test"
model = "deepseek-v4-flash"
tool_timeout_secs = 5

[model_request]
request_timeout_secs = 2

[terminal]
keyboard_enhancement = "off"
mouse_capture = false
osc52_clipboard = false

[connections.deepseek-test]
label = "DeepSeek test"
provider = "deepseek"
protocol = "deepseek"
base_url = "https://api.deepseek.com"
credential = {{ source = "environment", name = "SIGIL_API_KEY" }}
"#,
        workspace.display(),
        workspace.join("state").display(),
        workspace.join("cache").display(),
        session_dir.display()
    );
    fs::write(path, config)?;
    Ok(())
}

fn write_trusted_finalized_session(
    path: &Path,
    workspace: &Path,
    resolved_model_route: Option<ResolvedModelRoute>,
) -> Result<()> {
    let store = JsonlSessionStore::new(path)?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(store);
    session.append_control(ControlEntry::SessionIdentity {
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        resolved_model_route,
    })?;
    let workspace_id = stable_workspace_id(workspace)?;
    session.append_control(ControlEntry::WorkspaceTrustDecision(
        WorkspaceTrustDecisionEntry {
            workspace_id: workspace_id.clone(),
            workspace_trust_snapshot_id: format!("workspace-trust:{workspace_id}"),
            trust: WorkspaceTrust::Trusted,
            decided_by_event_id: Some("session-lifecycle-process-e2e".to_owned()),
            reason: Some("trusted process fixture".to_owned()),
        },
    ))?;
    session.append_user_message(ModelMessage::user("process lifecycle fixture"))?;
    let assistant = ModelMessage::assistant_with_kind(
        Some("fixture completed".to_owned()),
        Vec::new(),
        AssistantMessageKind::FinalAnswer,
    );
    session.append_assistant_message(assistant.clone())?;
    session.append_durable_event(
        DurableEventType::RunFinalized,
        EventClass::Critical,
        json!({
            "run_status": "completed",
            "terminal_reason": "final_answer",
            "final_message_id": assistant.id,
            "tool_calls": 0,
            "error": null
        }),
    )?;
    Ok(())
}

fn write_compaction_config(path: &Path, workspace: &Path, session_dir: &Path) -> Result<()> {
    let config = format!(
        r#"[workspace]
root = "{}"

[storage]
state_root = "{}"
cache_root = "auto"

[session]
log_dir = "{}"

[agent]
provider = "deepseek"
model = "deepseek-v4-flash"
tool_timeout_secs = 5

[compaction]
enabled = true
tail_messages = 2

[terminal]
keyboard_enhancement = "off"
mouse_capture = false
osc52_clipboard = false

[providers.deepseek]
api_key = "test-key"
strict_tools_mode = "auto"
"#,
        workspace.display(),
        workspace.join("state").display(),
        session_dir.display()
    );
    fs::write(path, config)?;
    Ok(())
}

fn write_compaction_session(path: &Path, workspace: &Path) -> Result<()> {
    let store = JsonlSessionStore::new(path)?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(store);
    session.append_control(ControlEntry::SessionIdentity {
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        resolved_model_route: None,
    })?;
    let workspace_id = stable_workspace_id(workspace)?;
    session.append_control(ControlEntry::WorkspaceTrustDecision(
        WorkspaceTrustDecisionEntry {
            workspace_id: workspace_id.clone(),
            workspace_trust_snapshot_id: format!("workspace-trust:{workspace_id}"),
            trust: WorkspaceTrust::Trusted,
            decided_by_event_id: Some("compaction-process-e2e".to_owned()),
            reason: Some("trusted compaction process fixture".to_owned()),
        },
    ))?;
    let mut final_message_id = None;
    for turn in 0..4 {
        session.append_user_message(ModelMessage::user(format!(
            "release compaction acceptance objective turn {turn}"
        )))?;
        let assistant = ModelMessage::assistant_with_kind(
            Some(format!(
                "completed durable implementation evidence for turn {turn}: {}",
                "verified-state ".repeat(400)
            )),
            Vec::new(),
            AssistantMessageKind::FinalAnswer,
        );
        final_message_id = Some(assistant.id.clone());
        session.append_assistant_message(assistant)?;
    }
    session.append_durable_event(
        DurableEventType::RunFinalized,
        EventClass::Critical,
        json!({
            "run_status": "completed",
            "terminal_reason": "final_answer",
            "final_message_id": final_message_id,
            "tool_calls": 0,
            "error": null
        }),
    )?;
    Ok(())
}

fn require_installed_compaction_tokenizer(cache_root: &Path) -> Result<()> {
    let profile_root = cache_root.join("provider-profiles/deepseek-v4-flash");
    let installed = fs::read_dir(&profile_root)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(std::result::Result::ok)
        .any(|entry| entry.path().join("tokenizer.json").is_file());
    if !installed {
        bail!(
            "release acceptance requires `sigil tokenizer install deepseek-v4-flash` under {}",
            cache_root.display()
        );
    }
    Ok(())
}

fn captured_text(output: &Arc<Mutex<Vec<u8>>>) -> String {
    output
        .lock()
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_else(|_| "<captured output unavailable>".to_owned())
}

fn captured_len(output: &Arc<Mutex<Vec<u8>>>) -> usize {
    output.lock().map_or(0, |bytes| bytes.len())
}

fn wait_for_text_after(output: &Arc<Mutex<Vec<u8>>>, offset: usize, needle: &str) -> Result<()> {
    let deadline = Instant::now() + PROCESS_TIMEOUT;
    loop {
        let captured = output
            .lock()
            .map(|bytes| {
                let offset = offset.min(bytes.len());
                String::from_utf8_lossy(&bytes[offset..]).into_owned()
            })
            .unwrap_or_else(|_| "<captured output unavailable>".to_owned());
        if captured.contains(needle) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            let tail = captured
                .chars()
                .rev()
                .take(2_000)
                .collect::<String>()
                .chars()
                .rev()
                .collect::<String>();
            return Err(anyhow!(
                "timed out waiting for fresh {needle:?}; captured tail={tail:?}"
            ));
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn wait_for_text(output: &Arc<Mutex<Vec<u8>>>, needle: &str) -> Result<()> {
    let deadline = Instant::now() + PROCESS_TIMEOUT;
    loop {
        let captured = captured_text(output);
        if captured.contains(needle) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            let tail = captured
                .chars()
                .rev()
                .take(2_000)
                .collect::<String>()
                .chars()
                .rev()
                .collect::<String>();
            return Err(anyhow!(
                "timed out waiting for {needle:?}; captured tail={tail:?}"
            ));
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn write_input(writer: &mut dyn Write, bytes: &[u8]) -> Result<()> {
    writer.write_all(bytes)?;
    writer.flush()?;
    Ok(())
}

fn read_http_request(stream: &mut impl Read) -> Result<Vec<u8>> {
    let mut request = Vec::new();
    let mut chunk = [0_u8; 8 * 1024];
    loop {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        request.extend_from_slice(&chunk[..read]);
        let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let body_start = header_end + 4;
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
        if request.len() >= body_start.saturating_add(content_length) {
            break;
        }
    }
    Ok(request)
}

struct NoCatalogProviderFixture {
    base_url: String,
    catalog_responded: Arc<AtomicBool>,
    generation_responded: Arc<AtomicBool>,
    server: thread::JoinHandle<Result<()>>,
}

fn spawn_openai_compatible_without_catalog_fixture() -> Result<NoCatalogProviderFixture> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    listener.set_nonblocking(true)?;
    let responded = Arc::new(AtomicBool::new(false));
    let server_responded = Arc::clone(&responded);
    let generated = Arc::new(AtomicBool::new(false));
    let server_generated = Arc::clone(&generated);
    let server = thread::spawn(move || -> Result<()> {
        let deadline = Instant::now() + PROCESS_TIMEOUT;
        let mut generation_requests = 0_u8;
        loop {
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if Instant::now() >= deadline {
                            bail!("catalog fixture did not receive a request");
                        }
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => return Err(error.into()),
                }
            };
            stream.set_nonblocking(false)?;
            stream.set_read_timeout(Some(PROCESS_TIMEOUT))?;
            let request = read_http_request(&mut stream)?;
            let request = String::from_utf8_lossy(&request);
            if request.starts_with("GET /v1/models ") {
                let body = r#"{"error":"model discovery is not implemented"}"#;
                write!(
                    stream,
                    "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )?;
                stream.flush()?;
                server_responded.store(true, Ordering::Release);
                continue;
            }
            assert!(
                request.starts_with("POST /v1/chat/completions "),
                "unexpected provider request: {request}"
            );
            let body = concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"FIRST-RUN-CANARY\"},\"finish_reason\":\"stop\"}]}\n\n",
                "data: [DONE]\n\n"
            );
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )?;
            stream.flush()?;
            server_generated.store(true, Ordering::Release);
            generation_requests = generation_requests.saturating_add(1);
            if generation_requests >= 2 {
                return Ok(());
            }
        }
    });
    Ok(NoCatalogProviderFixture {
        base_url: format!("http://{address}/v1"),
        catalog_responded: responded,
        generation_responded: generated,
        server,
    })
}

fn configure_isolated_process_home(command: &mut CommandBuilder, workspace: &Path) -> Result<()> {
    let home = workspace.join(".process-home");
    let config_home = home.join(".config");
    let cache_home = home.join(".cache");
    let state_home = home.join(".local").join("state");
    let runtime_home = home.join(".runtime");
    for path in [&home, &config_home, &cache_home, &state_home, &runtime_home] {
        fs::create_dir_all(path)?;
    }
    command.env("HOME", home.as_os_str());
    command.env("XDG_CONFIG_HOME", config_home.as_os_str());
    command.env("XDG_CACHE_HOME", cache_home.as_os_str());
    command.env("XDG_STATE_HOME", state_home.as_os_str());
    command.env("XDG_RUNTIME_DIR", runtime_home.as_os_str());
    Ok(())
}

fn run_tui_process(
    config_path: &Path,
    workspace: &Path,
    ready_text: &str,
    interact: impl FnOnce(&Arc<Mutex<Vec<u8>>>, &mut dyn Write) -> Result<()>,
) -> Result<()> {
    run_tui_process_with_optional_config(Some(config_path), workspace, ready_text, true, interact)
}

fn run_tui_process_with_default_config(
    workspace: &Path,
    ready_text: &str,
    interact: impl FnOnce(&Arc<Mutex<Vec<u8>>>, &mut dyn Write) -> Result<()>,
) -> Result<()> {
    run_tui_process_with_optional_config(None, workspace, ready_text, false, interact)
}

fn run_tui_process_with_optional_config(
    config_path: Option<&Path>,
    workspace: &Path,
    ready_text: &str,
    inject_deepseek_test_env: bool,
    interact: impl FnOnce(&Arc<Mutex<Vec<u8>>>, &mut dyn Write) -> Result<()>,
) -> Result<()> {
    let pty_system = native_pty_system();
    let pair = pty_system.openpty(PtySize {
        rows: 40,
        cols: 120,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    let master = pair.master;
    let slave = pair.slave;
    let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_sigil"));
    if let Some(config_path) = config_path {
        command.args([
            "--config",
            config_path.to_str().context("UTF-8 config path")?,
        ]);
    }
    command.cwd(workspace);
    configure_isolated_process_home(&mut command, workspace)?;
    command.env("TERM", "xterm-256color");
    if inject_deepseek_test_env {
        command.env("SIGIL_API_KEY", "test-key");
        command.env("SIGIL_BASE_URL", "https://api.deepseek.com");
        command.env("SIGIL_BETA_BASE_URL", "https://api.deepseek.com/beta");
        command.env(
            "SIGIL_ANTHROPIC_BASE_URL",
            "https://api.deepseek.com/anthropic",
        );
    } else {
        command.env_remove("SIGIL_API_KEY");
        command.env_remove("SIGIL_OPENAI_COMPATIBLE_API_KEY");
        command.env_remove("SIGIL_OPENAI_RESPONSES_API_KEY");
        command.env_remove("SIGIL_ANTHROPIC_API_KEY");
        command.env_remove("SIGIL_GEMINI_API_KEY");
    }
    let mut child = slave.spawn_command(command)?;
    drop(slave);

    let output = Arc::new(Mutex::new(Vec::new()));
    let reader_output = Arc::clone(&output);
    let mut reader = master.try_clone_reader()?;
    let reader_thread = thread::spawn(move || {
        let mut chunk = [0_u8; 8 * 1024];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    if let Ok(mut captured) = reader_output.lock() {
                        captured.extend_from_slice(&chunk[..read]);
                    }
                }
            }
        }
    });
    let mut writer = master.take_writer()?;

    let result = (|| -> Result<()> {
        wait_for_text(&output, ready_text)?;
        interact(&output, writer.as_mut())?;
        write_input(writer.as_mut(), &[0x01, 0x0b])?;
        write_input(writer.as_mut(), b"/quit\r")?;
        let deadline = Instant::now() + PROCESS_TIMEOUT;
        loop {
            if let Some(status) = child.try_wait()? {
                if !status.success() {
                    return Err(anyhow!(
                        "sigil TUI process exited with {}: {}",
                        status.exit_code(),
                        captured_text(&output)
                    ));
                }
                break;
            }
            if Instant::now() >= deadline {
                return Err(anyhow!(
                    "sigil TUI process did not exit after /quit: {}",
                    captured_text(&output)
                ));
            }
            thread::sleep(Duration::from_millis(25));
        }
        Ok(())
    })();

    if child.try_wait()?.is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    drop(writer);
    drop(master);
    let _ = reader_thread.join();
    result
}

#[test]
fn real_tui_first_run_without_model_catalog_completes_the_first_request() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let workspace = test_workspace()?;
    let config_path = workspace
        .join(".process-home")
        .join(".sigil")
        .join("sigil.toml");
    let fixture = spawn_openai_compatible_without_catalog_fixture()?;

    let result = (|| -> Result<()> {
        run_tui_process_with_default_config(
            &workspace,
            "Set up a model connection",
            |output, writer| {
                wait_for_text(output, "Set up a model connection")?;

                // Use an explicit custom connection so this real-process fixture can exercise a
                // loopback Chat Completions endpoint without weakening hosted-provider HTTPS
                // policy or requiring a credential.
                for _ in 0..4 {
                    write_input(writer, b"\x1b[B")?;
                    thread::sleep(Duration::from_millis(50));
                }
                write_input(writer, b"\r")?;
                thread::sleep(Duration::from_millis(150));

                let setup_complete_offset = captured_len(output);
                write_input(writer, b"\x1b[B\r")?;
                thread::sleep(Duration::from_millis(100));
                write_input(writer, &[0x7f; 64])?;
                write_input(writer, fixture.base_url.as_bytes())?;
                let endpoint_apply_offset = captured_len(output);
                write_input(writer, b"\r")?;
                wait_for_text_after(output, endpoint_apply_offset, "credential: <not staged>")?;

                let authentication_offset = captured_len(output);
                write_input(writer, b"\x1b[B\x1b[C")?;
                wait_for_text_after(output, authentication_offset, "no authentication")?;

                let catalog_loading_offset = captured_len(output);
                write_input(writer, b"\x1b[B\r")?;
                wait_for_text_after(
                    output,
                    catalog_loading_offset,
                    "refreshing optional remote list",
                )?;
                let deadline = Instant::now() + PROCESS_TIMEOUT;
                while !fixture.catalog_responded.load(Ordering::Acquire) {
                    if Instant::now() >= deadline {
                        return Err(anyhow!(
                            "model picker did not query the loopback catalog; captured={}",
                            captured_text(output)
                        ));
                    }
                    thread::sleep(Duration::from_millis(25));
                }
                wait_for_text_after(output, catalog_loading_offset, " · unverified]")?;
                let model_apply_offset = captured_len(output);
                write_input(writer, b"\r")?;
                wait_for_text_after(
                    output,
                    model_apply_offset,
                    "Custom endpoint · Chat Completions",
                )?;

                write_input(writer, b"\x1b[B\r")?;

                let deadline = Instant::now() + PROCESS_TIMEOUT;
                while !config_path.exists() {
                    if Instant::now() >= deadline {
                        return Err(anyhow!(
                            "first-run setup did not publish {}; captured={}",
                            config_path.display(),
                            captured_text(output)
                        ));
                    }
                    thread::sleep(Duration::from_millis(25));
                }
                wait_for_text_after(output, setup_complete_offset, "sigil ready.")?;

                let first_request_offset = captured_len(output);
                write_input(writer, b"reply with the first-run canary\r")?;
                wait_for_text_after(output, first_request_offset, "FIRST-RUN-CANARY")?;
                assert!(fixture.generation_responded.load(Ordering::Acquire));

                let root = RootConfig::load(&config_path)?;
                assert_eq!(root.config_version, sigil_kernel::CONFIG_VERSION_V2);
                assert_eq!(
                    root.agent.connection.as_ref().map(|id| id.as_str()),
                    Some("custom-default")
                );
                assert_eq!(root.agent.model, "gpt-4.1");
                assert!(root.agent.runtime_provider.is_empty());
                assert!(root.connections.contains_key("custom-default"));
                assert_eq!(
                    fs::metadata(&config_path)?.permissions().mode() & 0o777,
                    0o600
                );
                assert_eq!(
                    fs::metadata(config_path.parent().expect("config parent"))?
                        .permissions()
                        .mode()
                        & 0o777,
                    0o700
                );
                Ok(())
            },
        )?;
        Ok(())
    })();

    let catalog_result = fixture
        .server
        .join()
        .map_err(|_| anyhow!("catalog fixture thread panicked"))?;
    let cleanup = fs::remove_dir_all(&workspace);
    result?;
    catalog_result?;
    cleanup?;
    Ok(())
}

#[test]
#[ignore = "release acceptance requires an installed checksum-pinned DeepSeek V4 tokenizer"]
fn real_tui_process_compacts_and_reloads_the_durable_boundary() -> Result<()> {
    let workspace = test_workspace()?;
    let config_path = workspace.join("sigil-compaction.toml");
    let session_dir = workspace.join("sessions");
    fs::create_dir(&session_dir)?;
    write_compaction_config(&config_path, &workspace, &session_dir)?;
    let session_path = session_dir.join("session-compaction-process-e2e.jsonl");
    write_compaction_session(&session_path, &workspace)?;
    let root_config = RootConfig::load(&config_path)?;
    let paths = resolve_sigil_paths(&root_config.storage, &root_config.session, &workspace);
    require_installed_compaction_tokenizer(&paths.cache_root)?;

    let result = (|| -> Result<()> {
        run_tui_process(
            &config_path,
            &workspace,
            "deepseek-v4-flash",
            |output, writer| {
                write_input(writer, b"/resume release compaction acceptance objective\r")?;
                wait_for_text(output, "verified-state")?;
                write_input(writer, b"/compact\r")?;
                wait_for_text(output, "verified locally")?;
                write_input(writer, b"\r")?;
                wait_for_text(output, "Context compacted:")?;
                Ok(())
            },
        )?;

        run_tui_process(
            &config_path,
            &workspace,
            "deepseek-v4-flash",
            |output, writer| {
                write_input(writer, b"/resume release compaction acceptance objective\r")?;
                wait_for_text(output, "verified-state")?;
                write_input(writer, b"/compact\r")?;
                wait_for_text(output, "no newly foldable history:")?;
                Ok(())
            },
        )?;

        let records = JsonlSessionStore::read_event_records(&session_path)?;
        assert_eq!(
            records
                .iter()
                .filter(|record| {
                    record.stored_event().event_type
                        == DurableEventType::CompactionAppliedV2.as_str()
                })
                .count(),
            1
        );
        Ok(())
    })();

    let cleanup = fs::remove_dir_all(&workspace);
    result?;
    cleanup?;
    Ok(())
}

#[test]
fn real_tui_process_opens_session_actions_and_exports_safe_transcript() -> Result<()> {
    let workspace = test_workspace()?;
    let config_path = workspace.join("sigil.toml");
    let session_dir = workspace.join("sessions");
    fs::create_dir(&session_dir)?;
    write_config(&config_path, &workspace, &session_dir)?;
    let root_config = RootConfig::load(&config_path)?;
    let (_, model_route) =
        sigil_runtime::provider_connections::resolve_default_model_route(&root_config)
            .map_err(anyhow::Error::new)?;
    write_trusted_finalized_session(
        &session_dir.join("session-process-e2e.jsonl"),
        &workspace,
        Some(model_route),
    )?;
    let paths = resolve_sigil_paths(&root_config.storage, &root_config.session, &workspace);

    let pty_system = native_pty_system();
    let pair = pty_system.openpty(PtySize {
        rows: 40,
        cols: 120,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    let master = pair.master;
    let slave = pair.slave;
    let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_sigil"));
    command.args([
        "--config",
        config_path.to_str().context("UTF-8 config path")?,
    ]);
    command.cwd(&workspace);
    configure_isolated_process_home(&mut command, &workspace)?;
    command.env("TERM", "xterm-256color");
    command.env("SIGIL_API_KEY", "test-key");
    let mut child = slave.spawn_command(command)?;
    drop(slave);

    let output = Arc::new(Mutex::new(Vec::new()));
    let reader_output = Arc::clone(&output);
    let mut reader = master.try_clone_reader()?;
    let reader_thread = thread::spawn(move || {
        let mut chunk = [0_u8; 8 * 1024];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    if let Ok(mut captured) = reader_output.lock() {
                        captured.extend_from_slice(&chunk[..read]);
                    }
                }
            }
        }
    });
    let mut writer = master.take_writer()?;

    let result = (|| -> Result<()> {
        wait_for_text(&output, "deepseek-v4-flash")?;
        write_input(writer.as_mut(), b"/resume")?;
        wait_for_text(&output, "Ctrl-O actions")?;
        write_input(writer.as_mut(), &[0x0f])?;
        wait_for_text(&output, "Session Actions")?;
        wait_for_text(&output, "Export safe transcript")?;
        write_input(writer.as_mut(), b"e")?;
        wait_for_text(&output, "exported 2 safe message(s) to")?;

        let exports =
            fs::read_dir(&paths.session_exports_root)?.collect::<std::io::Result<Vec<_>>>()?;
        assert_eq!(exports.len(), 1);
        let artifact: SessionExportV1 = serde_json::from_slice(&fs::read(exports[0].path())?)?;
        artifact.validate_digest()?;
        assert_eq!(artifact.payload.messages.len(), 2);

        write_input(writer.as_mut(), &[0x1b])?;
        thread::sleep(Duration::from_millis(100));
        write_input(writer.as_mut(), &[0x01, 0x0b])?;
        write_input(writer.as_mut(), b"/quit\r")?;
        let deadline = Instant::now() + PROCESS_TIMEOUT;
        loop {
            if let Some(status) = child.try_wait()? {
                if !status.success() {
                    return Err(anyhow!(
                        "sigil TUI process exited with {}: {}",
                        status.exit_code(),
                        captured_text(&output)
                    ));
                }
                break;
            }
            if Instant::now() >= deadline {
                return Err(anyhow!("sigil TUI process did not exit after /quit"));
            }
            thread::sleep(Duration::from_millis(25));
        }
        Ok(())
    })();

    if child.try_wait()?.is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    drop(writer);
    drop(master);
    let _ = reader_thread.join();
    let cleanup = fs::remove_dir_all(&workspace);
    result?;
    cleanup?;
    Ok(())
}
