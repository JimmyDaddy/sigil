use std::{collections::BTreeMap, path::Path, path::PathBuf, sync::mpsc, time::Duration};

use crate::{
    app::{AppAction, AppState},
    mouse::HitTarget,
    runner::{WorkerCommand, WorkerMessage},
};
use anyhow::{Result, anyhow};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use serde_json::json;
use sigil_kernel::{
    AgentConfig, CompactionConfig, EventHandler, JsonlSessionStore, MemoryConfig, ModelMessage,
    PermissionConfig, RootConfig, RunEvent, SessionConfig, SessionLogEntry, WorkspaceConfig,
};

use super::{
    AppMouseOutcome, BUSY_POLL_INTERVAL, IDLE_POLL_INTERVAL, InitialSessionTarget,
    SCROLLBACK_SEED_POLL_INTERVAL, ScrollbackSeedProgress, ScrollbackSyncPlan, ScrollbackSyncState,
    WorkerRuntime, apply_key_action, apply_mouse_outcome, base64_encode, build_initial_app,
    drain_worker_messages, flush_pending_worker_commands, mouse_layout_snapshot,
    next_mouse_capture_action, next_poll_interval, osc52_clipboard_sequence, plan_scrollback_sync,
    plan_scrollback_sync_with_chunk_size, poll_interval, prepare_scrollback_sync,
    prepare_scrollback_sync_with_chunk_size, process_app_action, process_app_action_with_spawner,
    render_scrollback_rows, render_tui_exit_resume_hint, restore_initial_session_from_disk,
    scrollback_plain_line, scrollback_row_style, scrollback_separator, scrollback_wrapped_rows,
    should_sync_terminal_scrollback, wrap_scrollback_text,
};

fn test_config() -> RootConfig {
    RootConfig {
        workspace: WorkspaceConfig {
            root: ".".to_owned(),
        },
        storage: Default::default(),
        session: SessionConfig {
            log_dir: Some(".sigil/sessions".to_owned()),
            retention: Default::default(),
        },
        agent: AgentConfig {
            provider: "deepseek".to_owned(),
            model: "deepseek-v4-flash".to_owned(),
            max_turns: None,
            tool_timeout_secs: 30,
        },
        model_request: Default::default(),
        permission: PermissionConfig::default(),
        memory: MemoryConfig { enabled: true },
        skills: Default::default(),
        compaction: CompactionConfig::default(),
        code_intelligence: Default::default(),
        terminal: Default::default(),
        execution: Default::default(),
        verification: Default::default(),
        appearance: Default::default(),
        task: Default::default(),
        providers: BTreeMap::new(),
        web: Default::default(),
        mcp_servers: Vec::new(),
    }
}

fn test_config_for_workspace(workspace_root: &Path) -> RootConfig {
    RootConfig {
        workspace: WorkspaceConfig {
            root: workspace_root.display().to_string(),
        },
        ..test_config()
    }
}

#[test]
fn osc52_clipboard_sequence_encodes_text() {
    assert_eq!(base64_encode(b"h"), "aA==");
    assert_eq!(base64_encode(b"hello"), "aGVsbG8=");
    assert_eq!(osc52_clipboard_sequence("hi"), "\x1b]52;c;aGk=\x07");
}

#[test]
fn tui_exit_resume_hint_includes_session_id_and_resume_command() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.session_id = "abc123".to_owned();

    let hint = render_tui_exit_resume_hint(&app, None);

    assert_eq!(
        hint,
        "Sigil session: abc123\nResume with: sigil resume abc123\n"
    );
}

#[test]
fn tui_exit_resume_hint_preserves_explicit_config_path() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    app.session_id = "abc123".to_owned();

    let hint = render_tui_exit_resume_hint(&app, Some(Path::new("configs/my config.toml")));

    assert_eq!(
        hint,
        "Sigil session: abc123\nResume with: sigil --config 'configs/my config.toml' resume abc123\n"
    );
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
    JsonlSessionStore::new(&session_log_path)?.append(&SessionLogEntry::User(
        ModelMessage::user("restore this session"),
    ))?;

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
fn initial_sync_skips_replaying_history() {
    let state = ScrollbackSyncState::default();

    let plan = plan_scrollback_sync(&state, "session-a", 2, 0);

    assert_eq!(plan, ScrollbackSyncPlan::Noop);
}

#[test]
fn initial_sync_skips_large_history_replay() {
    let state = ScrollbackSyncState::default();

    let plan = plan_scrollback_sync_with_chunk_size(&state, "session-a", 5, 0, 2);

    assert_eq!(plan, ScrollbackSyncPlan::Noop);
}

#[test]
fn default_initial_sync_skips_large_history_replay() {
    let state = ScrollbackSyncState::default();

    let plan = plan_scrollback_sync(&state, "session-a", 5_000, 0);

    assert_eq!(plan, ScrollbackSyncPlan::Noop);
}

#[test]
fn pending_seed_continues_from_previous_chunk() {
    let state = ScrollbackSyncState {
        session_id: Some("session-a".to_owned()),
        revision: 1,
        line_count: 2,
        sequence_hash: 42,
        pending_seed: Some(ScrollbackSeedProgress {
            session_id: "session-a".to_owned(),
            next_line_index: 2,
        }),
    };

    let plan = plan_scrollback_sync_with_chunk_size(&state, "session-a", 5, 42, 2);

    assert_eq!(
        plan,
        ScrollbackSyncPlan::Seed {
            insert_separator: false,
            from_index: 2,
            to_index: 4,
            total_line_count: 5,
        }
    );
}

#[test]
fn zero_chunk_size_still_skips_initial_history_replay() {
    let state = ScrollbackSyncState::default();

    let plan = plan_scrollback_sync_with_chunk_size(&state, "session-a", 3, 0, 0);

    assert_eq!(plan, ScrollbackSyncPlan::Noop);
}

#[test]
fn stale_pending_seed_from_previous_session_does_not_replay_history() {
    let state = ScrollbackSyncState {
        session_id: Some("session-a".to_owned()),
        revision: 1,
        line_count: 2,
        sequence_hash: 42,
        pending_seed: Some(ScrollbackSeedProgress {
            session_id: "session-a".to_owned(),
            next_line_index: 2,
        }),
    };

    let plan = plan_scrollback_sync_with_chunk_size(&state, "session-b", 4, 0, 2);

    assert_eq!(plan, ScrollbackSyncPlan::Noop);
}

#[test]
fn mismatched_pending_seed_falls_back_to_append_logic() {
    let state = ScrollbackSyncState {
        session_id: Some("session-a".to_owned()),
        revision: 1,
        line_count: 2,
        sequence_hash: 42,
        pending_seed: Some(ScrollbackSeedProgress {
            session_id: "session-a".to_owned(),
            next_line_index: 1,
        }),
    };

    let plan = plan_scrollback_sync_with_chunk_size(&state, "session-a", 4, 42, 2);

    assert_eq!(plan, ScrollbackSyncPlan::Append { from_index: 2 });
}

#[test]
fn growing_history_appends_only_new_lines() {
    let state = ScrollbackSyncState {
        session_id: Some("session-a".to_owned()),
        revision: 1,
        line_count: 1,
        sequence_hash: 42,
        pending_seed: None,
    };

    let plan = plan_scrollback_sync(&state, "session-a", 2, 42);

    assert_eq!(plan, ScrollbackSyncPlan::Append { from_index: 1 });
}

#[test]
fn switching_sessions_without_existing_scrollback_skips_history_replay() {
    let state = ScrollbackSyncState {
        session_id: Some("session-a".to_owned()),
        revision: 2,
        line_count: 0,
        sequence_hash: 0,
        pending_seed: None,
    };

    let plan = plan_scrollback_sync_with_chunk_size(&state, "session-b", 3, 0, 2);

    assert_eq!(plan, ScrollbackSyncPlan::Noop);
}

#[test]
fn restored_or_switched_session_skips_history_replay() {
    let state = ScrollbackSyncState {
        session_id: Some("session-a".to_owned()),
        revision: 2,
        line_count: 1,
        sequence_hash: 9,
        pending_seed: None,
    };

    let plan = plan_scrollback_sync(&state, "session-b", 2, 3);

    assert_eq!(plan, ScrollbackSyncPlan::Noop);
}

#[test]
fn changing_existing_live_line_does_not_append_scrollback() {
    let state = ScrollbackSyncState {
        session_id: Some("session-a".to_owned()),
        revision: 3,
        line_count: 1,
        sequence_hash: 11,
        pending_seed: None,
    };

    let plan = plan_scrollback_sync(&state, "session-a", 2, 12);

    assert_eq!(plan, ScrollbackSyncPlan::Noop);
}

#[test]
fn busy_run_defers_terminal_scrollback_sync() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());

    assert!(should_sync_terminal_scrollback(&app));

    app.runtime.is_busy = true;

    assert!(!should_sync_terminal_scrollback(&app));
}

#[test]
fn setup_mode_defers_terminal_scrollback_sync() {
    let app = AppState::from_setup(
        Path::new("sigil.toml").to_path_buf(),
        Path::new(".").to_path_buf(),
        Some("missing config".to_owned()),
    );

    assert!(!should_sync_terminal_scrollback(&app));
}

#[test]
fn next_poll_interval_prefers_busy_then_seed_then_idle() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let pending_seed = ScrollbackSyncState {
        pending_seed: Some(ScrollbackSeedProgress {
            session_id: app.session_id.clone(),
            next_line_index: 1,
        }),
        ..ScrollbackSyncState::default()
    };

    app.runtime.is_busy = true;
    assert_eq!(
        next_poll_interval(&app, &ScrollbackSyncState::default()),
        BUSY_POLL_INTERVAL
    );

    app.runtime.is_busy = false;
    assert_eq!(
        next_poll_interval(&app, &pending_seed),
        SCROLLBACK_SEED_POLL_INTERVAL
    );

    assert_eq!(
        next_poll_interval(&app, &ScrollbackSyncState::default()),
        IDLE_POLL_INTERVAL
    );
}

#[test]
fn wrap_scrollback_text_respects_display_width_for_cjk() {
    assert_eq!(wrap_scrollback_text("你好", 2), vec!["你", "好"]);
    assert_eq!(wrap_scrollback_text("你好ab", 4), vec!["你好", "ab"]);
}

#[test]
fn scrollback_plain_line_concatenates_spans() {
    let line = Line::from(vec![Span::raw("hello "), Span::raw("world")]);

    assert_eq!(scrollback_plain_line(&line), "hello world");
}

#[test]
fn scrollback_row_style_uses_first_non_empty_span_style() {
    let line = Line::from(vec![
        Span::raw(" "),
        Span::styled(
            "important",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("ignored", Style::default().fg(Color::Red)),
    ]);

    let style = scrollback_row_style(&line);

    assert_eq!(style.fg, Some(Color::Cyan));
    assert!(style.add_modifier.contains(Modifier::BOLD));
}

#[test]
fn scrollback_separator_includes_session_provider_and_model() {
    let app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let separator = scrollback_separator(&app);

    let text = scrollback_plain_line(&separator);

    assert!(text.contains("---- session "));
    assert!(text.contains("deepseek"));
    assert!(text.contains("deepseek-v4-flash"));
}

#[test]
fn scrollback_separator_uses_configured_theme() {
    let mut config = test_config();
    config.appearance.theme = sigil_kernel::ThemeId::SolarizedLight;
    let app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    let separator = scrollback_separator(&app);
    let expected = crate::ui::theme::Theme::builtin(sigil_kernel::ThemeId::SolarizedLight).palette;

    assert_eq!(separator.spans[0].style.fg, Some(expected.text_muted));
    assert_eq!(separator.spans[1].style.fg, Some(expected.accent_info));
    assert_eq!(separator.spans[2].style.fg, Some(expected.text_muted));
}

#[test]
fn wrap_scrollback_text_preserves_empty_and_zero_width_inputs() {
    assert_eq!(wrap_scrollback_text("", 10), vec![""]);
    assert_eq!(wrap_scrollback_text("hello", 0), vec!["hello"]);
}

#[test]
fn render_scrollback_rows_prints_entire_row_from_single_cell() {
    let mut buffer = Buffer::empty(Rect::new(0, 0, 12, 2));
    let rows = vec![("你好 world".to_owned(), Style::default())];

    render_scrollback_rows(&mut buffer, &rows);

    assert_eq!(buffer[(0, 0)].symbol(), "你好 world");
}

#[test]
fn render_scrollback_rows_does_not_split_cjk_into_adjacent_cells() {
    let mut buffer = Buffer::empty(Rect::new(0, 0, 8, 1));
    let rows = vec![("你好".to_owned(), Style::default())];

    render_scrollback_rows(&mut buffer, &rows);

    assert_eq!(buffer[(0, 0)].symbol(), "你好");
    assert_eq!(buffer[(1, 0)].symbol(), " ");
}

#[test]
fn process_app_action_forwards_worker_command_when_runtime_exists() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let (worker_tx, command_rx) = mpsc::channel();
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
    let (worker_tx, command_rx) = mpsc::channel();
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
    let (closed_tx, closed_rx) = mpsc::channel();
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
            assert_eq!(root_config.agent.provider, "deepseek");
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
    let (closed_tx, closed_rx) = mpsc::channel();
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
            let (retry_tx, retry_rx) = mpsc::channel();
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
        Some("agent worker stopped before accepting command")
    );
    assert!(
        app.timeline
            .iter()
            .any(|entry| entry.text.contains("Run failed: agent worker stopped"))
    );
    Ok(())
}

#[test]
fn process_app_action_reports_restart_failure_without_runtime() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let mut worker = None;

    process_app_action(&mut app, &mut worker, AppAction::CancelRun)?;

    assert!(worker.is_none());
    assert!(
        app.last_notice()
            .is_some_and(|notice| notice.contains("test wrapper should not spawn"))
    );
    Ok(())
}

#[test]
fn send_worker_command_returns_false_when_runtime_is_missing() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let mut worker = None;

    let sent = super::send_worker_command(&mut app, &mut worker, WorkerCommand::Shutdown)?;

    assert!(!sent);
    assert!(worker.is_none());
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
        Some("agent worker stopped; runtime config unavailable")
    );
    Ok(())
}

#[test]
fn process_app_action_handles_clipboard_copy_locally() -> anyhow::Result<()> {
    let _env_guard = crate::test_env::lock();
    let _api_key = crate::test_env::EnvScope::unset("SIGIL_API_KEY");
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let (worker_tx, command_rx) = mpsc::channel();
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
fn process_app_action_reports_disabled_osc52_clipboard() -> anyhow::Result<()> {
    let _env_guard = crate::test_env::lock();
    let _api_key = crate::test_env::EnvScope::unset("SIGIL_API_KEY");
    let mut root_config = test_config();
    root_config.terminal.osc52_clipboard = false;
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &root_config);
    let (worker_tx, command_rx) = mpsc::channel();
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
    assert_eq!(
        app.last_notice(),
        Some("clipboard unavailable: OSC52 disabled")
    );
    Ok(())
}

#[test]
fn flush_pending_worker_commands_handles_empty_missing_and_runtime_paths() -> anyhow::Result<()> {
    let mut app = AppState::from_setup(
        Path::new("sigil.toml").to_path_buf(),
        Path::new(".").to_path_buf(),
        None,
    );
    let mut worker = None;
    assert!(!flush_pending_worker_commands(&mut app, &mut worker)?);

    let mut config = test_config();
    config.model_request.request_timeout_secs = 1;
    config.providers.insert(
        "deepseek".to_owned(),
        json!({
            "base_url": "https://example.com",
            "api_key": "test-key"
        }),
    );
    app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    assert!(app.has_pending_worker_commands());
    assert!(!flush_pending_worker_commands(&mut app, &mut worker)?);
    assert!(app.has_pending_worker_commands());

    let (worker_tx, command_rx) = mpsc::channel();
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
    let mut config = test_config();
    config.model_request.request_timeout_secs = 1;
    config.providers.insert(
        "deepseek".to_owned(),
        json!({
            "base_url": "https://example.com",
            "api_key": "test-key"
        }),
    );
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    assert!(app.has_pending_worker_commands());
    let (worker_tx, command_rx) = mpsc::channel();
    drop(command_rx);
    let (_message_tx, worker_rx) = mpsc::channel();
    let mut worker = Some(WorkerRuntime {
        worker_tx,
        worker_rx,
        ready: true,
    });

    assert!(flush_pending_worker_commands(&mut app, &mut worker)?);

    assert!(worker.is_none());
    assert!(!app.has_pending_worker_commands());
    assert_eq!(
        app.last_notice(),
        Some("agent worker stopped before accepting command")
    );
    Ok(())
}

fn fake_worker_runtime() -> (WorkerRuntime, mpsc::Receiver<WorkerCommand>) {
    let (worker_tx, worker_rx) = mpsc::channel();
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

fn app_with_scrollback() -> AppState {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let _ = app.set_terminal_size(48, 6);
    app.handle_worker_message(WorkerMessage::SessionSwitched {
        session_log_path: PathBuf::from(".sigil/sessions/session-restored.jsonl"),
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        entries: vec![
            SessionLogEntry::User(ModelMessage::user("hello")),
            SessionLogEntry::Assistant(ModelMessage::assistant(
                Some(
                    "restored answer with enough wrapped content to overflow the live panel"
                        .to_owned(),
                ),
                Vec::new(),
            )),
        ],
    })
    .expect("session switch should restore timeline");
    app.handle_worker_message(WorkerMessage::Notice("checking".to_owned()))
        .expect("notice should render");
    app.handle_worker_message(WorkerMessage::RunStarted {
        prompt: "follow-up".to_owned(),
    })
    .expect("run started should render");
    app.runtime.is_busy = false;
    app
}

#[test]
fn poll_interval_prefers_busy_then_seed_then_idle() {
    let mut busy_app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    busy_app.runtime.is_busy = true;
    assert_eq!(
        poll_interval(&busy_app, &ScrollbackSyncState::default()),
        BUSY_POLL_INTERVAL
    );

    let seeded_app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let seeded_state = ScrollbackSyncState {
        pending_seed: Some(ScrollbackSeedProgress {
            session_id: seeded_app.session_id.clone(),
            next_line_index: 1,
        }),
        ..ScrollbackSyncState::default()
    };
    assert_eq!(
        poll_interval(&seeded_app, &seeded_state),
        SCROLLBACK_SEED_POLL_INTERVAL
    );

    let setup_app = AppState::from_setup(
        PathBuf::from("sigil.toml"),
        PathBuf::from("."),
        Some("broken".to_owned()),
    );
    assert_eq!(poll_interval(&setup_app, &seeded_state), IDLE_POLL_INTERVAL);
}

#[test]
fn prepare_scrollback_sync_returns_none_when_scrollback_is_disabled_or_unchanged() {
    let setup_app = AppState::from_setup(
        PathBuf::from("sigil.toml"),
        PathBuf::from("."),
        Some("broken".to_owned()),
    );
    assert!(prepare_scrollback_sync(&setup_app, &ScrollbackSyncState::default()).is_none());

    let app = app_with_scrollback();
    let line_count = app.scrollback_line_count();
    let synced = ScrollbackSyncState {
        session_id: Some(app.session_id.clone()),
        revision: app.timeline_revision(),
        line_count,
        sequence_hash: app.scrollback_prefix_hash(line_count),
        pending_seed: None,
    };
    assert!(prepare_scrollback_sync(&app, &synced).is_none());
}

#[test]
fn prepare_scrollback_sync_skips_reseed_and_appends_expected_batches() {
    let app = app_with_scrollback();
    let line_count = app.scrollback_line_count();
    assert!(line_count > 0);

    let skipped_reseed = prepare_scrollback_sync(
        &app,
        &ScrollbackSyncState {
            session_id: Some("previous-session".to_owned()),
            revision: 1,
            line_count: 1,
            sequence_hash: 7,
            pending_seed: None,
        },
    )
    .expect("expected state sync");
    assert!(skipped_reseed.line_batches.is_empty());
    assert_eq!(
        skipped_reseed.next_state.session_id,
        Some(app.session_id.clone())
    );
    assert_eq!(skipped_reseed.next_state.line_count, line_count);
    assert_eq!(skipped_reseed.next_state.pending_seed, None);

    let append = prepare_scrollback_sync(
        &app,
        &ScrollbackSyncState {
            session_id: Some(app.session_id.clone()),
            revision: app.timeline_revision().saturating_sub(1),
            line_count: line_count.saturating_sub(1),
            sequence_hash: app.scrollback_prefix_hash(line_count.saturating_sub(1)),
            pending_seed: None,
        },
    )
    .expect("expected append");
    assert!(append.line_batches.len() <= 1);
    assert_eq!(append.next_state.line_count, line_count);
    assert!(append.next_state.pending_seed.is_none());
}

#[test]
fn prepare_scrollback_sync_tracks_current_session_without_initial_seed() {
    let app = app_with_scrollback();
    assert!(app.scrollback_line_count() > 1);

    let prepared =
        prepare_scrollback_sync_with_chunk_size(&app, &ScrollbackSyncState::default(), 1)
            .expect("expected state sync");

    assert_eq!(prepared.next_state.line_count, app.scrollback_line_count());
    assert_eq!(prepared.next_state.pending_seed, None);
    assert!(prepared.line_batches.is_empty());
}

#[test]
fn prepare_scrollback_sync_appends_non_empty_batches_from_shared_prefix() {
    let app = app_with_scrollback();

    let prepared = prepare_scrollback_sync(
        &app,
        &ScrollbackSyncState {
            session_id: Some(app.session_id.clone()),
            revision: app.timeline_revision().saturating_sub(1),
            line_count: 0,
            sequence_hash: app.scrollback_prefix_hash(0),
            pending_seed: None,
        },
    )
    .expect("expected append plan");

    assert!(!prepared.line_batches.is_empty());
    assert_eq!(prepared.next_state.line_count, app.scrollback_line_count());
}

#[test]
fn prepare_scrollback_sync_survives_rerender_width_changes_and_append() {
    let mut app = app_with_scrollback();
    let mut state = ScrollbackSyncState {
        session_id: Some(app.session_id.clone()),
        revision: app.timeline_revision(),
        line_count: app.scrollback_line_count(),
        sequence_hash: app.scrollback_prefix_hash(app.scrollback_line_count()),
        pending_seed: None,
    };

    assert!(app.set_terminal_size(32, 8));
    let after_narrow = prepare_scrollback_sync(&app, &state)
        .expect("narrow rerender should produce a scrollback sync plan");
    assert_eq!(
        after_narrow.next_state.line_count,
        app.scrollback_line_count()
    );
    assert_eq!(
        after_narrow.next_state.sequence_hash,
        app.scrollback_prefix_hash(app.scrollback_line_count())
    );
    state = after_narrow.next_state;

    assert!(app.set_terminal_size(90, 8));
    let after_wide = prepare_scrollback_sync(&app, &state)
        .expect("wide rerender should produce a scrollback sync plan");
    assert_eq!(
        after_wide.next_state.line_count,
        app.scrollback_line_count()
    );
    assert_eq!(
        after_wide.next_state.sequence_hash,
        app.scrollback_prefix_hash(app.scrollback_line_count())
    );
    state = after_wide.next_state;

    for index in 0..12 {
        app.handle(RunEvent::AssistantMessage(ModelMessage::assistant(
            Some(format!("after resize {index}")),
            Vec::new(),
        )))
        .expect("assistant message should append timeline entry");
    }
    let after_append =
        prepare_scrollback_sync_with_chunk_size(&app, &state, 2).expect("append should sync");
    assert_eq!(
        after_append.next_state.line_count,
        app.scrollback_line_count()
    );
    assert_eq!(
        after_append.next_state.sequence_hash,
        app.scrollback_prefix_hash(app.scrollback_line_count())
    );
}

#[test]
fn prepare_scrollback_sync_can_return_noop_plan_when_prefix_changed() {
    let app = app_with_scrollback();

    let prepared = prepare_scrollback_sync(
        &app,
        &ScrollbackSyncState {
            session_id: Some(app.session_id.clone()),
            revision: app.timeline_revision().saturating_sub(1),
            line_count: 1,
            sequence_hash: u64::MAX,
            pending_seed: None,
        },
    )
    .expect("expected noop preparation");

    assert!(prepared.line_batches.is_empty());
    assert!(prepared.next_state.pending_seed.is_none());
}

#[test]
fn scrollback_plain_and_wrapped_rows_preserve_style_metadata() {
    let line = Line::from(vec![
        Span::styled(
            "Alert",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" body"),
    ]);

    assert_eq!(scrollback_plain_line(&line), "Alert body");

    let style = scrollback_row_style(&line);
    assert_eq!(style.fg, Some(Color::Yellow));
    assert!(style.add_modifier.contains(Modifier::BOLD));

    let wrapped = scrollback_wrapped_rows(&line, 5);
    assert_eq!(wrapped.len(), 2);
    assert_eq!(wrapped[0].0, "Alert");
    assert_eq!(wrapped[0].1.fg, Some(Color::Yellow));
}

#[test]
fn blank_scrollback_rows_use_default_style() {
    let line = Line::from(vec![Span::raw("   ")]);

    assert_eq!(scrollback_row_style(&line), Style::default());
}

#[test]
fn scrollback_separator_mentions_session_provider_and_model() {
    let app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let separator = scrollback_plain_line(&scrollback_separator(&app));

    assert!(separator.contains("---- session "));
    assert!(separator.contains(&app.runtime.provider_name));
    assert!(separator.contains(&app.runtime.model_name));
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
    let root_config = test_config_for_workspace(temp.path());
    let (app, worker) = build_initial_app(
        temp.path().to_path_buf(),
        temp.path().join("sigil.toml"),
        Ok(root_config),
        |_root_config, _app| Ok(fake_worker_runtime().0),
    )?;

    assert!(!app.is_setup_mode());
    assert!(app.is_workspace_trust_gate_mode());
    assert!(worker.is_none());
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
fn process_app_action_test_wrapper_reports_spawn_attempts_as_errors() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let mut worker = None;

    let error = process_app_action(
        &mut app,
        &mut worker,
        AppAction::ConfigSaved {
            root_config: Box::new(test_config()),
        },
    )
    .expect_err("test wrapper should reject spawn actions");

    assert!(error.to_string().contains("test wrapper should not spawn"));
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
    let mut app = AppState::from_setup(
        PathBuf::from("sigil.toml"),
        PathBuf::from("."),
        Some("missing".to_owned()),
    );
    let mut worker = None;

    process_app_action_with_spawner(
        &mut app,
        &mut worker,
        AppAction::SetupCompleted {
            config_path: PathBuf::from("sigil.toml"),
            root_config: Box::new(test_config()),
        },
        |_root_config, _app| Ok(fake_worker_runtime().0),
    )?;

    assert!(!app.is_setup_mode());
    assert!(worker.is_some());
    assert_eq!(app.runtime.provider_name, "deepseek");
    Ok(())
}

#[test]
fn drain_worker_messages_marks_dirty_when_messages_arrive() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let (worker_tx, _command_rx) = mpsc::channel();
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
    let (worker_tx, _command_rx) = mpsc::channel();
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
fn drain_worker_messages_returns_clean_without_runtime() -> Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let mut worker = None;

    assert!(!drain_worker_messages(&mut app, &mut worker)?);
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
