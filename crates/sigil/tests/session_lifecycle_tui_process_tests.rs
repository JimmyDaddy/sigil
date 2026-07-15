#![cfg(unix)]

use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use serde_json::json;
use sigil_kernel::{
    AssistantMessageKind, ControlEntry, DurableEventType, EventClass, JsonlSessionStore,
    ModelMessage, RootConfig, Session, WorkspaceTrust, WorkspaceTrustDecisionEntry,
    stable_workspace_id,
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
        r#"[workspace]
root = "{}"

[storage]
state_root = "{}"
cache_root = "{}"

[session]
log_dir = "{}"

[agent]
provider = "deepseek"
model = "deepseek-v4-flash"
tool_timeout_secs = 5

[model_request]
request_timeout_secs = 2

[terminal]
keyboard_enhancement = "off"
mouse_capture = false
osc52_clipboard = false

[providers.deepseek]
base_url = "http://127.0.0.1:9"
beta_base_url = "http://127.0.0.1:9"
anthropic_base_url = "http://127.0.0.1:9"
api_key = "test-key"
strict_tools_mode = "auto"
"#,
        workspace.display(),
        workspace.join("state").display(),
        workspace.join("cache").display(),
        session_dir.display()
    );
    fs::write(path, config)?;
    Ok(())
}

fn write_trusted_finalized_session(path: &Path, workspace: &Path) -> Result<()> {
    let store = JsonlSessionStore::new(path)?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(store);
    session.append_control(ControlEntry::SessionIdentity {
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
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

fn run_tui_process(
    config_path: &Path,
    workspace: &Path,
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
    command.args([
        "--config",
        config_path.to_str().context("UTF-8 config path")?,
    ]);
    command.cwd(workspace);
    command.env("TERM", "xterm-256color");
    command.env("SIGIL_BASE_URL", "https://api.deepseek.com");
    command.env("SIGIL_BETA_BASE_URL", "https://api.deepseek.com/beta");
    command.env(
        "SIGIL_ANTHROPIC_BASE_URL",
        "https://api.deepseek.com/anthropic",
    );
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
    result
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
        run_tui_process(&config_path, &workspace, |output, writer| {
            write_input(writer, b"/resume release compaction acceptance objective\r")?;
            wait_for_text(output, "verified-state")?;
            write_input(writer, b"/compact\r")?;
            wait_for_text(output, "verified locally")?;
            write_input(writer, b"\r")?;
            wait_for_text(output, "Context compacted:")?;
            Ok(())
        })?;

        run_tui_process(&config_path, &workspace, |output, writer| {
            write_input(writer, b"/resume release compaction acceptance objective\r")?;
            wait_for_text(output, "verified-state")?;
            write_input(writer, b"/compact\r")?;
            wait_for_text(output, "no newly foldable history:")?;
            Ok(())
        })?;

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
    write_trusted_finalized_session(&session_dir.join("session-process-e2e.jsonl"), &workspace)?;
    let root_config = RootConfig::load(&config_path)?;
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
    command.env("TERM", "xterm-256color");
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
