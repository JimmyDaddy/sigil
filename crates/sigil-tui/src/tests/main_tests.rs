use std::{
    collections::BTreeMap, ffi::OsString, path::Path, path::PathBuf, sync::mpsc, time::Duration,
};

use crate::{
    app::{AppAction, AppState},
    mouse::HitTarget,
    runner::{WorkerCommand, WorkerCommandSender, WorkerMessage},
};
use anyhow::{Result, anyhow};
use ratatui::{
    Terminal,
    backend::{Backend, TestBackend},
    layout::{Position, Rect},
    style::Style,
};
use serde_json::json;
use sigil_kernel::{
    AgentConfig, CONFIG_VERSION_V2, CompactionConfig, ConnectionId, ControlEntry,
    JsonlSessionStore, MemoryConfig, ModelMessage, ModelRef, PermissionConfig, RootConfig,
    SessionConfig, SessionLogEntry, WorkspaceConfig, WorkspaceTrust, stable_workspace_id,
};

use super::{
    AppMouseOutcome, BACKGROUND_TASK_WAKE_INTERVAL, ExternalLaunchPlatform, ExternalLaunchTarget,
    InitialSessionTarget, SPINNER_FRAME_MILLIS, TuiPanicHookGuard, WorkerRuntime, apply_key_action,
    apply_mouse_outcome, build_initial_app, drain_worker_messages, enter_terminal_presentation,
    external_launch_plan, finalize_terminal_presentation, flush_pending_worker_commands,
    leave_terminal_presentation, mouse_layout_snapshot, next_mouse_capture_action,
    next_wake_deadline, process_app_action, process_app_action_with_spawner,
    render_tui_exit_resume_hint, restart_worker_after_session_transition,
    restore_initial_session_from_disk,
};

fn test_config() -> RootConfig {
    RootConfig {
        config_version: 2,
        workspace: WorkspaceConfig {
            root: ".".to_owned(),
        },
        storage: Default::default(),
        session: SessionConfig {
            log_dir: Some(".sigil/sessions".to_owned()),
            retention: Default::default(),
        },
        agent: AgentConfig {
            runtime_provider: "deepseek".to_owned(),
            connection: Some(ConnectionId::new("deepseek-default").expect("valid test connection")),
            model: "deepseek-v4-flash".to_owned(),
            max_turns: None,
            tool_timeout_secs: 30,
        },
        model_request: Default::default(),
        permission: PermissionConfig::default(),
        memory: MemoryConfig::with_enabled(true),
        skills: Default::default(),
        compaction: CompactionConfig::default(),
        code_intelligence: Default::default(),
        terminal: Default::default(),
        execution: Default::default(),
        verification: Default::default(),
        appearance: Default::default(),
        task: Default::default(),
        connections: BTreeMap::from([(
            "deepseek-default".to_owned(),
            json!({
                "label": "DeepSeek",
                "provider": "deepseek",
                "protocol": "deepseek",
                "base_url": "https://api.deepseek.com",
                "credential": {
                    "source": "environment",
                    "name": "SIGIL_API_KEY"
                }
            }),
        )]),
        web: Default::default(),
        mcp_servers: Vec::new(),
    }
}

#[test]
fn background_panic_is_forwarded_without_restoring_the_owner_terminal() -> Result<()> {
    const CHILD_ENV: &str = "SIGIL_TUI_BACKGROUND_PANIC_HOOK_CHILD";
    const TEST_NAME: &str =
        "launcher::tests::background_panic_is_forwarded_without_restoring_the_owner_terminal";
    if std::env::var_os(CHILD_ENV).is_none() {
        let output = std::process::Command::new(std::env::current_exe()?)
            .args(["--exact", TEST_NAME, "--nocapture"])
            .env(CHILD_ENV, "1")
            .output()?;
        assert!(
            output.status.success(),
            "panic-hook subprocess failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return Ok(());
    }

    let restore_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let restore_count_for_hook = std::sync::Arc::clone(&restore_count);
    let (guard, mut reports) = TuiPanicHookGuard::install_with_restore(move || {
        restore_count_for_hook.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    });
    let panicked = std::thread::Builder::new()
        .name("panic-regression-worker".to_owned())
        .spawn(|| panic!("UTF-8 background panic 到"))?;
    assert!(panicked.join().is_err());
    assert_eq!(
        restore_count.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "a background panic must not restore terminal state owned by the launcher thread"
    );
    let report = reports.try_recv()?;
    assert!(report.contains("panic-regression-worker"));
    assert!(report.contains("UTF-8 background panic 到"));
    guard.restore();
    Ok(())
}

fn test_config_for_workspace(workspace_root: &Path) -> RootConfig {
    RootConfig {
        config_version: 2,
        workspace: WorkspaceConfig {
            root: workspace_root.display().to_string(),
        },
        ..test_config()
    }
}

fn v2_test_config(default_connection: &str) -> RootConfig {
    let mut config = test_config();
    config.config_version = CONFIG_VERSION_V2;
    config.agent.runtime_provider.clear();
    config.agent.connection =
        Some(ConnectionId::new(default_connection).expect("valid test connection"));
    config.agent.model = format!("{default_connection}-model");
    for connection_id in ["primary", "secondary"] {
        config.connections.insert(
            connection_id.to_owned(),
            json!({
                "label": connection_id,
                "provider": "deepseek",
                "protocol": "deepseek",
                "base_url": "https://api.deepseek.com",
                "credential": {
                    "source": "environment",
                    "name": "SIGIL_API_KEY"
                }
            }),
        );
    }
    config
}

#[test]
fn tui_exit_resume_hint_includes_session_id_and_resume_command() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.session_id = "abc123".to_owned();
    app.session_browser
        .current_entries
        .push(SessionLogEntry::User(ModelMessage::user("hello")));

    let hint = render_tui_exit_resume_hint(&app, None);

    assert_eq!(
        hint,
        "Sigil session: abc123\nResume with: sigil resume abc123\n"
    );
}

#[test]
fn terminal_finalization_clears_the_frame_and_parks_the_cursor_at_origin() {
    let backend = TestBackend::new(12, 4);
    let mut terminal = Terminal::new(backend).expect("test terminal should initialize");
    terminal
        .draw(|frame| {
            frame
                .buffer_mut()
                .set_string(0, 0, "stale TUI frame", Style::default());
        })
        .expect("test frame should draw");
    terminal
        .set_cursor_position(Position::new(7, 3))
        .expect("test cursor should move");

    finalize_terminal_presentation(&mut terminal).expect("terminal finalization should succeed");

    assert_eq!(
        terminal
            .backend_mut()
            .get_cursor_position()
            .expect("cursor should be readable"),
        Position::ORIGIN
    );
    assert!(
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .all(|cell| cell.symbol() == " "),
        "no stale TUI cells may remain before the resume hint or a fatal error is printed"
    );
}

#[test]
fn terminal_presentation_uses_a_balanced_alternate_screen_lifecycle() {
    let mut output = Vec::new();

    enter_terminal_presentation(&mut output).expect("alternate screen should activate");
    leave_terminal_presentation(&mut output).expect("alternate screen should restore");

    assert_eq!(output, b"\x1b[?1049h\x1b[3J\x1b[2J\x1b[1;1H\x1b[?1049l");
}

#[test]
fn wake_deadline_only_polls_while_runtime_work_is_active() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    assert_eq!(next_wake_deadline(&app), None);

    app.runtime.is_busy = true;
    assert_eq!(
        next_wake_deadline(&app),
        Some(Duration::from_millis(SPINNER_FRAME_MILLIS as u64))
    );

    app.runtime.is_busy = false;
    let (_sender, receiver) = mpsc::channel();
    app.set_pending_model_catalog_for_test(receiver);
    assert_eq!(
        next_wake_deadline(&app),
        Some(BACKGROUND_TASK_WAKE_INTERVAL)
    );
}

#[test]
fn tui_exit_resume_hint_preserves_explicit_config_path() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.session_id = "abc123".to_owned();
    app.session_browser
        .current_entries
        .push(SessionLogEntry::User(ModelMessage::user("hello")));

    let hint = render_tui_exit_resume_hint(&app, Some(Path::new("configs/my config.toml")));

    assert_eq!(
        hint,
        "Sigil session: abc123\nResume with: sigil --config 'configs/my config.toml' resume abc123\n"
    );
}

#[test]
fn tui_exit_resume_hint_is_empty_for_bootstrap_only_session() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.session_browser
        .current_entries
        .push(SessionLogEntry::Control(ControlEntry::SessionIdentity {
            provider_name: "deepseek".to_owned(),
            model_name: "deepseek-v4-flash".to_owned(),
            resolved_model_route: None,
        }));

    assert_eq!(render_tui_exit_resume_hint(&app, None), "");
}

#[test]
fn tui_exit_resume_hint_is_empty_before_session_mode() {
    let app = AppState::from_setup(
        PathBuf::from("sigil.toml"),
        PathBuf::from("."),
        Some("missing config".to_owned()),
    );

    assert_eq!(render_tui_exit_resume_hint(&app, None), "");
}

#[test]
fn restore_initial_session_from_disk_uses_requested_selector() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let config_path = workspace.path().join("sigil.toml");
    let config = test_config_for_workspace(workspace.path());
    let mut app = AppState::from_root_config(&config_path, &config);
    let session_log_path = app.session_log_dir.join("session-target-123.jsonl");
    let store = JsonlSessionStore::new(&session_log_path)?;
    let mut session =
        sigil_kernel::Session::load_from_store("deepseek", "deepseek-v4-flash", store)?;
    session.append_user_message(ModelMessage::user("restore this session"))?;

    restore_initial_session_from_disk(
        &mut app,
        &config,
        InitialSessionTarget::Selector("target-123"),
    )?;

    assert_eq!(app.session_id, "target-123");
    assert_eq!(app.session_log_path, session_log_path);
    assert!(
        app.timeline
            .iter()
            .any(|entry| entry.text.contains("restore this session"))
    );
    Ok(())
}

#[test]
fn restore_initial_session_from_disk_latest_reopens_the_most_recent_session() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let config_path = workspace.path().join("sigil.toml");
    let config = test_config_for_workspace(workspace.path());
    let mut app = AppState::from_root_config(&config_path, &config);
    let session_log_path = app.session_log_dir.join("session-latest-existing.jsonl");
    let store = JsonlSessionStore::new(&session_log_path)?;
    let mut session =
        sigil_kernel::Session::load_from_store("deepseek", "deepseek-v4-flash", store)?;
    session.append_user_message(ModelMessage::user("latest session marker"))?;

    restore_initial_session_from_disk(&mut app, &config, InitialSessionTarget::Latest)?;

    assert_eq!(app.session_id, "latest-existing");
    assert_eq!(app.session_log_path, session_log_path);
    assert!(
        app.timeline
            .iter()
            .any(|entry| entry.text.contains("latest session marker"))
    );
    Ok(())
}

#[test]
fn fresh_initial_session_target_does_not_reopen_existing_history() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let config_path = workspace.path().join("sigil.toml");
    let config = test_config_for_workspace(workspace.path());
    let mut app = AppState::from_root_config(&config_path, &config);
    let fresh_session_id = app.session_id.clone();
    let fresh_session_path = app.session_log_path.clone();
    let existing_path = app.session_log_dir.join("session-existing.jsonl");
    let store = JsonlSessionStore::new(&existing_path)?;
    let mut existing =
        sigil_kernel::Session::load_from_store("deepseek", "deepseek-v4-flash", store)?;
    existing.append_user_message(ModelMessage::user("must stay in history"))?;

    restore_initial_session_from_disk(&mut app, &config, InitialSessionTarget::Fresh)?;

    assert_eq!(app.session_id, fresh_session_id);
    assert_eq!(app.session_log_path, fresh_session_path);
    assert!(
        !app.timeline
            .iter()
            .any(|entry| entry.text.contains("must stay in history"))
    );
    Ok(())
}

#[test]
fn next_mouse_capture_action_tracks_runtime_terminal_config_changes() {
    let mut active = false;

    assert_eq!(next_mouse_capture_action(active, false), None);
    assert!(!active);

    assert_eq!(next_mouse_capture_action(active, true), Some(true));
    assert!(!active);
    active = true;
    assert!(active);

    assert_eq!(next_mouse_capture_action(active, true), None);
    assert!(active);

    assert_eq!(next_mouse_capture_action(active, false), Some(false));
    assert!(active);
    active = false;
    assert!(!active);
}

#[test]
fn mouse_layout_snapshot_tracks_inline_frame_origin() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.set_terminal_size(120, 40);
    app.composer.input = "/".to_owned();
    let frame_area = Rect::new(0, 7, 120, 20);

    let layout = mouse_layout_snapshot(frame_area, Rect::new(0, 0, 120, 40), &app);

    assert_eq!(layout.screen, frame_area);
    let slash = layout
        .slash_overlay
        .expect("slash overlay should be visible");
    assert!(slash.overlay.y >= frame_area.y);
    let candidate_y = slash.content.y.saturating_add(slash.title_rows);
    assert_eq!(
        layout.hit_target(slash.content.x, candidate_y),
        HitTarget::SlashCandidate { index: 0 }
    );
}

#[test]
fn mouse_layout_snapshot_falls_back_to_terminal_size_before_first_frame() {
    let app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    let layout = mouse_layout_snapshot(Rect::default(), Rect::new(0, 0, 100, 30), &app);

    assert_eq!(layout.screen, Rect::new(0, 0, 100, 30));
}

#[test]
fn process_app_action_forwards_worker_command_when_runtime_exists() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let (worker_tx, command_rx) = WorkerCommandSender::test_channel();
    let (_message_tx, worker_rx) = mpsc::channel();
    let mut worker = Some(WorkerRuntime {
        worker_tx,
        worker_rx,
        ready: true,
    });

    process_app_action(
        &mut app,
        &mut worker,
        AppAction::SubmitPrompt("hello".to_owned()),
    )?;

    let command = command_rx.recv_timeout(Duration::from_secs(1))?;
    assert!(matches!(
        command,
        WorkerCommand::SubmitPrompt {
            ref prompt,
            reasoning_effort: sigil_kernel::ReasoningEffort::Max,
        } if prompt == "hello"
    ));
    Ok(())
}

#[test]
fn process_app_action_queues_worker_command_until_runtime_is_ready() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let _ = app.drain_pending_worker_commands();
    let (worker_tx, command_rx) = WorkerCommandSender::test_channel();
    let (_message_tx, worker_rx) = mpsc::channel();
    let mut worker = Some(WorkerRuntime {
        worker_tx,
        worker_rx,
        ready: false,
    });

    process_app_action(
        &mut app,
        &mut worker,
        AppAction::SubmitPrompt("hello".to_owned()),
    )?;

    assert!(command_rx.recv_timeout(Duration::from_millis(10)).is_err());
    assert!(app.has_pending_worker_commands());

    worker.as_mut().expect("worker should exist").ready = true;
    assert!(flush_pending_worker_commands(&mut app, &mut worker)?);

    let command = command_rx.recv_timeout(Duration::from_secs(1))?;
    assert!(matches!(
        command,
        WorkerCommand::SubmitPrompt {
            ref prompt,
            reasoning_effort: sigil_kernel::ReasoningEffort::Max,
        } if prompt == "hello"
    ));
    assert!(!app.has_pending_worker_commands());
    Ok(())
}

#[test]
fn process_app_action_restarts_closed_worker_and_retries_command() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let (closed_tx, closed_rx) = WorkerCommandSender::test_channel();
    drop(closed_rx);
    let (_message_tx, worker_rx) = mpsc::channel();
    let mut worker = Some(WorkerRuntime {
        worker_tx: closed_tx,
        worker_rx,
        ready: true,
    });
    let (next_runtime, commands) = fake_worker_runtime();
    let mut next_runtime = Some(next_runtime);
    let mut spawn_count = 0;

    process_app_action_with_spawner(
        &mut app,
        &mut worker,
        AppAction::SubmitTask("review workspace".to_owned()),
        |root_config, _app| {
            spawn_count += 1;
            assert_eq!(root_config.agent.runtime_provider, "deepseek");
            next_runtime
                .take()
                .ok_or_else(|| anyhow!("worker restarted more than once"))
        },
    )?;

    assert_eq!(spawn_count, 1);
    assert!(worker.is_some());
    let command = commands.recv_timeout(Duration::from_secs(1))?;
    assert!(matches!(
        command,
        WorkerCommand::SubmitTask { ref prompt } if prompt == "review workspace"
    ));
    Ok(())
}

#[test]
fn process_app_action_starts_missing_worker_and_sends_command() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let mut worker = None;
    let (next_runtime, commands) = fake_worker_runtime();
    let mut next_runtime = Some(next_runtime);

    process_app_action_with_spawner(
        &mut app,
        &mut worker,
        AppAction::SubmitTask("review workspace".to_owned()),
        |_root_config, _app| {
            next_runtime
                .take()
                .ok_or_else(|| anyhow!("worker restarted more than once"))
        },
    )?;

    assert!(worker.is_some());
    let command = commands.recv_timeout(Duration::from_secs(1))?;
    assert!(matches!(
        command,
        WorkerCommand::SubmitTask { ref prompt } if prompt == "review workspace"
    ));
    Ok(())
}

#[test]
fn process_app_action_reports_closed_worker_after_restart_without_exiting() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let (closed_tx, closed_rx) = WorkerCommandSender::test_channel();
    drop(closed_rx);
    let (_message_tx, worker_rx) = mpsc::channel();
    let mut worker = Some(WorkerRuntime {
        worker_tx: closed_tx,
        worker_rx,
        ready: true,
    });

    process_app_action_with_spawner(
        &mut app,
        &mut worker,
        AppAction::SubmitTask("review workspace".to_owned()),
        |_root_config, _app| {
            let (retry_tx, retry_rx) = WorkerCommandSender::test_channel();
            drop(retry_rx);
            let (_message_tx, worker_rx) = mpsc::channel();
            Ok(WorkerRuntime {
                worker_tx: retry_tx,
                worker_rx,
                ready: true,
            })
        },
    )?;

    assert!(worker.is_none());
    assert_eq!(
        app.last_notice(),
        Some("provider is temporarily unavailable; retry or repair the connection")
    );
    assert!(app.timeline.iter().any(|entry| {
        entry
            .text
            .contains("Session unavailable: provider is temporarily unavailable")
    }));
    Ok(())
}

#[test]
fn process_app_action_reports_restart_failure_without_runtime() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let mut worker = None;

    process_app_action(&mut app, &mut worker, AppAction::CancelRun)?;

    assert!(worker.is_none());
    assert_eq!(
        app.last_notice(),
        Some("provider is temporarily unavailable; retry or repair the connection")
    );
    Ok(())
}

#[test]
fn send_worker_command_with_restart_reports_missing_runtime_config() -> Result<()> {
    let mut app = AppState::from_setup(
        PathBuf::from("sigil.toml"),
        PathBuf::from("."),
        Some("missing config".to_owned()),
    );
    let mut worker = None;
    let mut spawn_worker = |_root_config: RootConfig, _app: &AppState| -> Result<WorkerRuntime> {
        Err(anyhow!("spawn should not be called without runtime config"))
    };

    super::send_worker_command_with_restart(
        &mut app,
        &mut worker,
        WorkerCommand::CancelRun,
        &mut spawn_worker,
    )?;

    assert!(worker.is_none());
    assert_eq!(
        app.last_notice(),
        Some("provider is temporarily unavailable; retry or repair the connection")
    );
    Ok(())
}

#[test]
fn process_app_action_handles_clipboard_copy_locally() -> anyhow::Result<()> {
    let _env_guard = crate::test_env::lock();
    let _api_key = crate::test_env::EnvScope::unset("SIGIL_API_KEY");
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let (worker_tx, command_rx) = WorkerCommandSender::test_channel();
    let (_message_tx, worker_rx) = mpsc::channel();
    let mut worker = Some(WorkerRuntime {
        worker_tx,
        worker_rx,
        ready: true,
    });

    process_app_action(
        &mut app,
        &mut worker,
        AppAction::CopyToClipboard {
            text: "selected".to_owned(),
        },
    )?;

    assert!(command_rx.recv_timeout(Duration::from_millis(10)).is_err());
    assert_eq!(app.last_notice(), Some("copied 1 line(s), 8 char(s)"));
    Ok(())
}

#[test]
fn process_app_action_uses_system_clipboard_when_osc52_is_disabled() -> anyhow::Result<()> {
    let _env_guard = crate::test_env::lock();
    let _api_key = crate::test_env::EnvScope::unset("SIGIL_API_KEY");
    let mut root_config = test_config();
    root_config.terminal.osc52_clipboard = false;
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &root_config);
    let (worker_tx, command_rx) = WorkerCommandSender::test_channel();
    let (_message_tx, worker_rx) = mpsc::channel();
    let mut worker = Some(WorkerRuntime {
        worker_tx,
        worker_rx,
        ready: true,
    });

    process_app_action(
        &mut app,
        &mut worker,
        AppAction::CopyToClipboard {
            text: "selected".to_owned(),
        },
    )?;

    assert!(command_rx.recv_timeout(Duration::from_millis(10)).is_err());
    assert_eq!(app.last_notice(), Some("copied 1 line(s), 8 char(s)"));
    Ok(())
}

#[test]
fn feedback_external_launch_plans_are_shell_free_and_platform_specific() -> anyhow::Result<()> {
    let issue_url = "https://github.com/JimmyDaddy/sigil/issues/new?template=bug-report.yml";
    let report = Path::new("/tmp/sigil-support.json");

    let mac_url = external_launch_plan(
        ExternalLaunchTarget::Url(issue_url),
        ExternalLaunchPlatform::MacOs,
    )?;
    assert_eq!(mac_url.program, "/usr/bin/open");
    assert_eq!(mac_url.args, vec![OsString::from(issue_url)]);
    let mac_reveal = external_launch_plan(
        ExternalLaunchTarget::RevealFile(report),
        ExternalLaunchPlatform::MacOs,
    )?;
    assert_eq!(
        mac_reveal.args,
        vec![OsString::from("-R"), report.as_os_str().to_owned()]
    );

    let linux_reveal = external_launch_plan(
        ExternalLaunchTarget::RevealFile(report),
        ExternalLaunchPlatform::Freedesktop,
    )?;
    assert_eq!(linux_reveal.program, "xdg-open");
    assert_eq!(linux_reveal.args, vec![OsString::from("/tmp")]);

    let windows_url = external_launch_plan(
        ExternalLaunchTarget::Url(issue_url),
        ExternalLaunchPlatform::Windows,
    )?;
    assert_eq!(windows_url.program, "rundll32.exe");
    assert_eq!(windows_url.args[1], OsString::from(issue_url));

    assert!(
        external_launch_plan(
            ExternalLaunchTarget::Url("http://example.com"),
            ExternalLaunchPlatform::MacOs,
        )
        .is_err()
    );
    assert!(
        external_launch_plan(
            ExternalLaunchTarget::Url(issue_url),
            ExternalLaunchPlatform::Unsupported,
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn process_app_action_handles_feedback_handoff_locally() -> anyhow::Result<()> {
    let _env_guard = crate::test_env::lock();
    let _api_key = crate::test_env::EnvScope::unset("SIGIL_API_KEY");
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let (worker_tx, command_rx) = WorkerCommandSender::test_channel();
    let (_message_tx, worker_rx) = mpsc::channel();
    let mut worker = Some(WorkerRuntime {
        worker_tx,
        worker_rx,
        ready: true,
    });

    process_app_action(
        &mut app,
        &mut worker,
        AppAction::OpenExternalUrl {
            url: "https://github.com/JimmyDaddy/sigil/issues/new?template=bug-report.yml"
                .to_owned(),
        },
    )?;
    assert_eq!(app.last_notice(), Some("opening bug report form"));
    process_app_action(
        &mut app,
        &mut worker,
        AppAction::RevealFile {
            path: PathBuf::from("/tmp/sigil-support.json"),
        },
    )?;
    assert_eq!(app.last_notice(), Some("revealing feedback report"));
    assert!(command_rx.recv_timeout(Duration::from_millis(10)).is_err());
    Ok(())
}

#[test]
fn flush_pending_worker_commands_handles_empty_missing_and_runtime_paths() -> anyhow::Result<()> {
    let _environment_lock = crate::test_env::lock();
    let _api_key = crate::test_env::EnvScope::set("SIGIL_API_KEY", "test-key");
    let mut app = AppState::from_setup(
        Path::new("sigil.toml").to_path_buf(),
        Path::new(".").to_path_buf(),
        None,
    );
    let mut worker = None;
    assert!(!flush_pending_worker_commands(&mut app, &mut worker)?);

    let mut config = test_config();
    config.model_request.request_timeout_secs = 1;
    app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    assert!(app.has_pending_worker_commands());
    assert!(!flush_pending_worker_commands(&mut app, &mut worker)?);
    assert!(app.has_pending_worker_commands());

    let (worker_tx, command_rx) = WorkerCommandSender::test_channel();
    let (_message_tx, worker_rx) = mpsc::channel();
    worker = Some(WorkerRuntime {
        worker_tx,
        worker_rx,
        ready: true,
    });
    assert!(flush_pending_worker_commands(&mut app, &mut worker)?);

    let command = command_rx.recv_timeout(Duration::from_secs(1))?;
    assert!(matches!(
        command,
        WorkerCommand::RefreshProviderBalance { .. }
    ));
    assert!(!app.has_pending_worker_commands());
    assert!(!flush_pending_worker_commands(&mut app, &mut worker)?);
    Ok(())
}

#[test]
fn flush_pending_worker_commands_reports_closed_worker_without_error() -> Result<()> {
    let _environment_lock = crate::test_env::lock();
    let _api_key = crate::test_env::EnvScope::set("SIGIL_API_KEY", "test-key");
    let mut config = test_config();
    config.model_request.request_timeout_secs = 1;
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    assert!(app.has_pending_worker_commands());
    let (worker_tx, command_rx) = WorkerCommandSender::test_channel();
    drop(command_rx);
    let (_message_tx, worker_rx) = mpsc::channel();
    let mut worker = Some(WorkerRuntime {
        worker_tx,
        worker_rx,
        ready: true,
    });

    assert!(flush_pending_worker_commands(&mut app, &mut worker)?);

    assert!(worker.is_none());
    assert!(app.has_pending_worker_commands());
    assert!(matches!(
        app.drain_pending_worker_commands().as_slice(),
        [WorkerCommand::RefreshProviderBalance { .. }]
    ));
    assert_eq!(
        app.last_notice(),
        Some("provider is temporarily unavailable; retry or repair the connection")
    );
    Ok(())
}

fn fake_worker_runtime() -> (WorkerRuntime, mpsc::Receiver<WorkerCommand>) {
    let (worker_tx, worker_rx) = WorkerCommandSender::test_channel();
    let (_message_tx, message_rx) = mpsc::channel::<WorkerMessage>();
    (
        WorkerRuntime {
            worker_tx,
            worker_rx: message_rx,
            ready: true,
        },
        worker_rx,
    )
}

#[test]
fn build_initial_app_enters_setup_mode_when_config_load_fails() -> Result<()> {
    let (app, worker) = build_initial_app(
        PathBuf::from("/tmp/workspace"),
        PathBuf::from("/tmp/workspace/sigil.toml"),
        Err(anyhow!("broken config")),
        |_root_config, _app| Err(anyhow!("spawner should not run")),
    )?;

    assert!(app.is_setup_mode());
    assert!(worker.is_none());
    Ok(())
}

#[test]
fn build_initial_app_enters_trust_gate_for_loaded_untrusted_config() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut root_config = test_config_for_workspace(temp.path());
    root_config.storage.state_root =
        sigil_kernel::StorageRoot::Path(temp.path().join("state").display().to_string());
    root_config.storage.cache_root =
        sigil_kernel::StorageRoot::Path(temp.path().join("cache").display().to_string());
    let config_path = temp.path().join("sigil.toml");
    root_config.save(&config_path)?;
    let (app, worker) = build_initial_app(
        temp.path().to_path_buf(),
        config_path,
        Ok(root_config),
        |_root_config, _app| Ok(fake_worker_runtime().0),
    )?;

    assert!(!app.is_setup_mode());
    assert!(app.is_workspace_trust_gate_mode());
    assert!(worker.is_none());
    Ok(())
}

#[test]
fn new_session_action_uses_launcher_control_path_when_worker_is_unavailable() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root_config = test_config_for_workspace(temp.path());
    let config_path = temp.path().join("sigil.toml");
    let mut app = AppState::from_root_config(&config_path, &root_config);
    let previous_session_id = app.session_id.clone();
    let new_session_path = app.session_log_dir.join("session-control-new.jsonl");
    let mut worker = None;
    let (runtime, _commands) = fake_worker_runtime();
    let mut runtime = Some(runtime);

    process_app_action_with_spawner(
        &mut app,
        &mut worker,
        AppAction::StartNewSession {
            session_log_path: new_session_path.clone(),
        },
        |_root_config, _app| Ok(runtime.take().expect("spawner is called once")),
    )?;

    assert_ne!(app.session_id, previous_session_id);
    assert_eq!(app.session_id, "control-new");
    assert_eq!(app.session_log_path, new_session_path);
    assert!(worker.is_some());
    let entries = JsonlSessionStore::read_entries(&app.session_log_path)?;
    assert!(entries.iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::SessionRouteTrustBound { .. })
    )));
    Ok(())
}

#[test]
fn process_app_action_restarts_worker_for_config_save() -> Result<()> {
    let _env_guard = crate::test_env::lock();
    let _api_key = crate::test_env::EnvScope::unset("SIGIL_API_KEY");
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let (old_runtime, old_commands) = fake_worker_runtime();
    let mut worker = Some(old_runtime);

    process_app_action_with_spawner(
        &mut app,
        &mut worker,
        AppAction::ConfigSaved {
            root_config: Box::new(test_config()),
        },
        |_root_config, _app| Ok(fake_worker_runtime().0),
    )?;

    let shutdown = old_commands.recv()?;
    assert!(matches!(shutdown, WorkerCommand::Shutdown));
    assert!(worker.is_some());
    Ok(())
}

#[test]
fn config_save_restarts_worker_on_active_session_route() -> Result<()> {
    let _env_guard = crate::test_env::lock();
    let _api_key = crate::test_env::EnvScope::unset("SIGIL_API_KEY");
    let current_config = v2_test_config("primary");
    let mut saved_config = v2_test_config("secondary");
    saved_config.agent.model = "secondary-model".to_owned();
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &current_config);
    app.apply_runtime_config_snapshot(&saved_config);
    let (old_runtime, old_commands) = fake_worker_runtime();
    let mut worker = Some(old_runtime);
    let mut spawned_config = None;

    process_app_action_with_spawner(
        &mut app,
        &mut worker,
        AppAction::ConfigSaved {
            root_config: Box::new(saved_config),
        },
        |root_config, _app| {
            spawned_config = Some(root_config);
            Ok(fake_worker_runtime().0)
        },
    )?;

    assert!(matches!(old_commands.recv()?, WorkerCommand::Shutdown));
    let spawned_config = spawned_config.expect("replacement worker config");
    assert_eq!(
        spawned_config
            .agent
            .connection
            .as_ref()
            .map(ConnectionId::as_str),
        Some("primary")
    );
    assert_eq!(spawned_config.agent.model, "primary-model");
    assert_eq!(
        app.root_config_snapshot()
            .and_then(|config| config.agent.connection.as_ref())
            .map(ConnectionId::as_str),
        Some("secondary")
    );
    Ok(())
}

#[test]
fn process_app_action_restarts_worker_for_runtime_config_update() -> Result<()> {
    let _env_guard = crate::test_env::lock();
    let _api_key = crate::test_env::EnvScope::unset("SIGIL_API_KEY");
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let (old_runtime, old_commands) = fake_worker_runtime();
    let mut worker = Some(old_runtime);

    process_app_action_with_spawner(
        &mut app,
        &mut worker,
        AppAction::RuntimeConfigUpdated {
            root_config: Box::new(test_config()),
        },
        |_root_config, _app| Ok(fake_worker_runtime().0),
    )?;

    assert!(matches!(old_commands.recv()?, WorkerCommand::Shutdown));
    assert!(worker.is_some());
    Ok(())
}

#[test]
fn process_app_action_forwards_runtime_commands_to_worker() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let (runtime, commands) = fake_worker_runtime();
    let mut worker = Some(runtime);

    process_app_action_with_spawner(
        &mut app,
        &mut worker,
        AppAction::SubmitPrompt("hello".to_owned()),
        |_root_config, _app| Err(anyhow!("spawner should not run")),
    )?;

    let command = commands.recv()?;
    assert!(matches!(
        command,
        WorkerCommand::SubmitPrompt { ref prompt, reasoning_effort: _ }
            if prompt == "hello"
    ));
    Ok(())
}

#[test]
fn process_app_action_bootstraps_app_after_setup_completion() -> Result<()> {
    let _env_guard = crate::test_env::lock();
    let _api_key = crate::test_env::EnvScope::unset("SIGIL_API_KEY");
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    let root_config = test_config_for_workspace(temp.path());
    let mut app = AppState::from_setup(
        config_path.clone(),
        temp.path().to_path_buf(),
        Some("missing".to_owned()),
    );
    app.set_support_build_info(sigil_runtime::support::SupportBuildInfo::new(
        "1.2.3",
        "setup-commit",
        "test-target",
        "test-profile",
    ));
    let mut worker = None;

    process_app_action_with_spawner(
        &mut app,
        &mut worker,
        AppAction::SetupCompleted {
            config_path,
            root_config: Box::new(root_config),
        },
        |_root_config, _app| Ok(fake_worker_runtime().0),
    )?;

    assert!(!app.is_setup_mode());
    assert!(worker.is_some());
    assert_eq!(app.runtime.provider_name, "deepseek");
    assert_eq!(app.support_build_info().commit, "setup-commit");
    let workspace_id = stable_workspace_id(temp.path())?;
    let entries = JsonlSessionStore::read_entries(&app.session_log_path)?;
    assert!(entries.iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::WorkspaceTrustDecision(decision))
                if decision.workspace_id == workspace_id
                    && decision.trust == WorkspaceTrust::Trusted
                    && decision.reason.as_deref()
                        == Some("trusted by user during quick setup")
        )
    }));
    Ok(())
}

#[test]
fn drain_worker_messages_marks_dirty_when_messages_arrive() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let (worker_tx, _command_rx) = WorkerCommandSender::test_channel();
    let (message_tx, worker_rx) = mpsc::channel();
    let mut worker = Some(WorkerRuntime {
        worker_tx,
        worker_rx,
        ready: true,
    });
    message_tx.send(WorkerMessage::RunStarted {
        prompt: "hello".to_owned(),
    })?;

    assert!(drain_worker_messages(&mut app, &mut worker)?);
    Ok(())
}

#[test]
fn drain_worker_messages_marks_runtime_ready() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let (worker_tx, _command_rx) = WorkerCommandSender::test_channel();
    let (message_tx, worker_rx) = mpsc::channel();
    let mut worker = Some(WorkerRuntime {
        worker_tx,
        worker_rx,
        ready: false,
    });
    message_tx.send(WorkerMessage::WorkerReady)?;

    assert!(drain_worker_messages(&mut app, &mut worker)?);
    assert!(worker.as_ref().expect("worker should exist").ready);
    Ok(())
}

#[test]
fn drain_worker_messages_retires_unready_worker_without_dropping_pending_input() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.enqueue_worker_command(WorkerCommand::SubmitPrompt {
        prompt: "queued while starting".to_owned(),
        reasoning_effort: sigil_kernel::ReasoningEffort::Max,
    });
    let (worker_tx, _command_rx) = WorkerCommandSender::test_channel();
    let (message_tx, worker_rx) = mpsc::channel();
    let mut worker = Some(WorkerRuntime {
        worker_tx,
        worker_rx,
        ready: false,
    });
    message_tx.send(WorkerMessage::RunFailed(
        "native credential store rejected the read".to_owned(),
    ))?;

    assert!(drain_worker_messages(&mut app, &mut worker)?);
    assert!(worker.is_none());
    assert!(app.has_pending_worker_commands());
    assert!(app.timeline.iter().any(|entry| {
        entry
            .text
            .contains("native credential store rejected the read")
            && !entry.text.contains("did not become ready")
    }));
    Ok(())
}

#[test]
fn typed_startup_route_recovery_preserves_pending_input_and_avoids_run_failure_copy() -> Result<()>
{
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.enqueue_worker_command(WorkerCommand::SubmitPrompt {
        prompt: "queued while route recovery is required".to_owned(),
        reasoning_effort: sigil_kernel::ReasoningEffort::Max,
    });
    let (worker_tx, _command_rx) = WorkerCommandSender::test_channel();
    let (message_tx, worker_rx) = mpsc::channel();
    let mut worker = Some(WorkerRuntime {
        worker_tx,
        worker_rx,
        ready: false,
    });
    message_tx.send(WorkerMessage::SessionRouteRecoveryRequired {
        code: sigil_kernel::PublicRouteRecoveryCode::SessionRouteConfirmationRequired,
        actions: vec![
            sigil_kernel::PublicRouteRecoveryAction::ConfirmCurrentRoute,
            sigil_kernel::PublicRouteRecoveryAction::RepairConnection,
        ],
        recovery_binding: "route-binding-exact".to_owned(),
        retryable: true,
        target_session: None,
    })?;

    assert!(drain_worker_messages(&mut app, &mut worker)?);
    assert!(worker.is_none());
    assert!(app.has_pending_worker_commands());
    assert_eq!(
        app.pending_session_route_recovery_binding(),
        Some("route-binding-exact")
    );
    assert!(app.timeline.iter().any(|entry| {
        entry.text.starts_with("Session unavailable:")
            && !entry.text.to_ascii_lowercase().contains("run failed")
    }));
    Ok(())
}

#[test]
fn startup_failure_preserves_a_queued_new_session_command_before_worker_readiness() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.enqueue_worker_command(WorkerCommand::StartNewSession {
        session_log_path: app.session_log_dir.join("session-new.jsonl"),
    });
    let (worker_tx, _command_rx) = WorkerCommandSender::test_channel();
    let (message_tx, worker_rx) = mpsc::channel();
    let mut worker = Some(WorkerRuntime {
        worker_tx,
        worker_rx,
        ready: false,
    });
    message_tx.send(WorkerMessage::RunFailed(
        "session route cannot be restored".to_owned(),
    ))?;

    assert!(drain_worker_messages(&mut app, &mut worker)?);
    assert!(worker.is_none());
    assert!(app.has_pending_worker_commands());
    Ok(())
}

#[test]
fn drain_worker_messages_returns_clean_without_runtime() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let mut worker = None;

    assert!(!drain_worker_messages(&mut app, &mut worker)?);
    Ok(())
}

#[test]
fn session_transition_restarts_worker_against_the_restored_compound_route() -> Result<()> {
    let config = v2_test_config("primary");
    let target_model = ModelRef::new(
        ConnectionId::new("secondary")?,
        "secondary-model".to_owned(),
    )?;
    let (_, target_route) =
        sigil_runtime::provider_connections::resolve_model_route(&config, &target_model)?;
    let target_session = PathBuf::from(".sigil/sessions/session-secondary.jsonl");
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    let (old_worker, _old_commands) = fake_worker_runtime();
    let mut worker = Some(old_worker);

    app.handle_worker_message(WorkerMessage::SessionSwitched {
        session_log_path: target_session.clone(),
        provider_name: "deepseek".to_owned(),
        model_name: target_model.model_id.clone(),
        entries: vec![SessionLogEntry::Control(ControlEntry::SessionIdentity {
            provider_name: "deepseek".to_owned(),
            model_name: target_model.model_id.clone(),
            resolved_model_route: Some(target_route.clone()),
        })],
    })?;

    let mut spawn_count = 0;
    assert!(restart_worker_after_session_transition(
        &mut app,
        &mut worker,
        |root_config, rebound_app| {
            spawn_count += 1;
            assert_eq!(root_config.config_version, CONFIG_VERSION_V2);
            assert_eq!(rebound_app.session_log_path, target_session);
            assert_eq!(
                rebound_app.runtime.model_route.as_ref(),
                Some(&target_route)
            );
            Ok(fake_worker_runtime().0)
        }
    )?);
    assert_eq!(spawn_count, 1);
    assert!(worker.is_some());
    assert!(!restart_worker_after_session_transition(
        &mut app,
        &mut worker,
        |_root_config, _app| Err(anyhow!("spawner must not run twice"))
    )?);
    Ok(())
}

#[test]
fn apply_mouse_outcome_handles_noop_redraw_and_actions() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let (runtime, commands) = fake_worker_runtime();
    let mut worker = Some(runtime);

    assert!(!apply_mouse_outcome(
        &mut app,
        &mut worker,
        AppMouseOutcome::Noop,
        |_root_config, _app| Err(anyhow!("spawner should not run"))
    )?);
    assert!(apply_mouse_outcome(
        &mut app,
        &mut worker,
        AppMouseOutcome::Redraw,
        |_root_config, _app| Err(anyhow!("spawner should not run"))
    )?);
    assert!(apply_mouse_outcome(
        &mut app,
        &mut worker,
        AppMouseOutcome::Action(AppAction::CheckChangedFilesDiagnostics),
        |_root_config, _app| Err(anyhow!("spawner should not run"))
    )?);

    let command = commands.recv()?;
    assert!(matches!(
        command,
        WorkerCommand::CheckChangedFilesDiagnostics
    ));
    Ok(())
}

#[test]
fn apply_key_action_always_requests_render_and_forwards_actions() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let (runtime, commands) = fake_worker_runtime();
    let mut worker = Some(runtime);

    assert!(apply_key_action(
        &mut app,
        &mut worker,
        None,
        |_root_config, _app| Err(anyhow!("spawner should not run"))
    )?);
    assert!(apply_key_action(
        &mut app,
        &mut worker,
        Some(AppAction::CancelRun),
        |_root_config, _app| Err(anyhow!("spawner should not run"))
    )?);

    let command = commands.recv()?;
    assert!(matches!(command, WorkerCommand::CancelRun));
    Ok(())
}

#[test]
fn tab_enter_run_next_reaches_the_worker_command_channel() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let entries = vec![SessionLogEntry::Control(
        ControlEntry::ConversationInputQueued(sigil_kernel::ConversationInputQueuedEntry {
            queue_id: sigil_kernel::ConversationInputQueueId::new("queue_keyboard_next")?,
            target: sigil_kernel::ConversationInputTarget::MainThread,
            kind: sigil_kernel::ConversationInputKind::Chat,
            prompt_hash: "sha256:keyboard-next".to_owned(),
            prompt: "run this follow-up next".to_owned(),
            reasoning_effort: Some(sigil_kernel::ReasoningEffort::High),
            created_at_ms: Some(1),
        }),
    )];
    let items = sigil_kernel::ConversationQueueProjection::from_entries(&entries).items;
    app.handle_worker_message(WorkerMessage::ConversationQueueUpdated {
        items,
        paused: false,
        entries,
    })?;
    let (runtime, commands) = fake_worker_runtime();
    let mut worker = Some(runtime);

    assert!(
        app.handle_key_event(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Tab,
            crossterm::event::KeyModifiers::NONE,
        ))?
        .is_none()
    );
    assert!(app.is_composer_queue_panel_focused());
    let action = app.handle_key_event(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Enter,
        crossterm::event::KeyModifiers::NONE,
    ))?;
    assert!(apply_key_action(
        &mut app,
        &mut worker,
        action,
        |_root_config, _app| Err(anyhow!("spawner should not run"))
    )?);

    let command = commands.recv_timeout(Duration::from_secs(1))?;
    assert!(matches!(
        command,
        WorkerCommand::PromoteQueuedConversationInput { queue_id }
            if queue_id.as_str() == "queue_keyboard_next"
    ));
    Ok(())
}
