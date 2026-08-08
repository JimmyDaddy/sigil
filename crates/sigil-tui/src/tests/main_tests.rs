use std::{
    collections::BTreeMap, ffi::OsString, path::Path, path::PathBuf, sync::mpsc, time::Duration,
};

use crate::{
    app::{AppAction, AppState},
    mouse::HitTarget,
    runner::{WorkerCommand, WorkerCommandSender, WorkerMessage},
    timeline::TimelineRole,
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
    AgentConfig, CONFIG_VERSION_V2, CompactionConfig, ConnectionId, ControlEntry, EventHandler,
    JsonlSessionStore, MemoryConfig, ModelMessage, ModelRef, PermissionConfig, RootConfig,
    RunEvent, SessionConfig, SessionLogEntry, WorkspaceConfig, WorkspaceTrust, stable_workspace_id,
};

use super::{
    AppMouseOutcome, BACKGROUND_TASK_WAKE_INTERVAL, ExternalLaunchPlatform, ExternalLaunchTarget,
    InitialSessionTarget, SCROLLBACK_SEED_POLL_INTERVAL, SPINNER_FRAME_MILLIS,
    ScrollbackSeedProgress, ScrollbackSyncPlan, ScrollbackSyncState, WorkerRuntime,
    apply_key_action, apply_mouse_outcome, build_initial_app, drain_worker_messages,
    external_launch_plan, flush_pending_worker_commands, inline_viewport_growth,
    mouse_layout_snapshot, native_scrollback_available, next_mouse_capture_action,
    next_wake_deadline, padded_scrollback_row, plan_scrollback_sync,
    plan_scrollback_sync_with_chunk_size, prepare_scrollback_sync,
    prepare_scrollback_sync_with_chunk_size, process_app_action, process_app_action_with_spawner,
    render_scrollback_rows, render_tui_exit_resume_hint, restart_worker_after_session_transition,
    restore_initial_session_from_disk, scrollback_frontier_update, scrollback_plain_line,
    scrollback_row_style, scrollback_separator, scrollback_wrapped_rows,
    should_sync_terminal_scrollback, tested_next_wake_deadline, wrap_scrollback_text,
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
fn initial_sync_skips_replaying_history() {
    let state = ScrollbackSyncState::default();

    let plan = plan_scrollback_sync(&state, "session-a", 2, 0);

    assert_eq!(plan, ScrollbackSyncPlan::Noop);
}

#[test]
fn inline_viewport_only_rebuilds_when_the_terminal_exceeds_its_high_water_mark() {
    assert_eq!(inline_viewport_growth(Some(24), 20), None);
    assert_eq!(inline_viewport_growth(Some(24), 24), None);
    assert_eq!(inline_viewport_growth(Some(24), 40), Some(40));
    assert_eq!(inline_viewport_growth(None, 40), None);
}

#[test]
fn native_scrollback_is_disabled_for_fullscreen_fallback() {
    assert!(native_scrollback_available(Some(24)));
    assert!(!native_scrollback_available(None));
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
        entry_count: 0,
        sequence_hash: 42,
        pending_seed: Some(ScrollbackSeedProgress {
            session_id: "session-a".to_owned(),
            next_line_index: 2,
            prefix_hash: 42,
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
        entry_count: 0,
        sequence_hash: 42,
        pending_seed: Some(ScrollbackSeedProgress {
            session_id: "session-a".to_owned(),
            next_line_index: 2,
            prefix_hash: 42,
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
        entry_count: 0,
        sequence_hash: 42,
        pending_seed: Some(ScrollbackSeedProgress {
            session_id: "session-a".to_owned(),
            next_line_index: 1,
            prefix_hash: 42,
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
        entry_count: 0,
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
        entry_count: 0,
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
        entry_count: 0,
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
        entry_count: 0,
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
fn next_wake_deadline_prefers_busy_then_seed_then_event_driven_idle() {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let pending_seed = ScrollbackSyncState {
        pending_seed: Some(ScrollbackSeedProgress {
            session_id: app.session_id.clone(),
            next_line_index: 1,
            prefix_hash: 0,
        }),
        ..ScrollbackSyncState::default()
    };

    app.runtime.is_busy = true;
    assert_eq!(
        tested_next_wake_deadline(&app, &ScrollbackSyncState::default()),
        Some(Duration::from_millis(SPINNER_FRAME_MILLIS as u64))
    );

    app.runtime.is_busy = false;
    assert_eq!(
        tested_next_wake_deadline(&app, &pending_seed),
        Some(SCROLLBACK_SEED_POLL_INTERVAL)
    );

    assert_eq!(
        tested_next_wake_deadline(&app, &ScrollbackSyncState::default()),
        None
    );

    let (_sender, receiver) =
        mpsc::channel::<Result<sigil_runtime::provider_connections::ModelCatalogResult, String>>();
    app.set_pending_model_catalog_for_test(receiver);
    assert_eq!(
        tested_next_wake_deadline(&app, &ScrollbackSyncState::default()),
        Some(BACKGROUND_TASK_WAKE_INTERVAL)
    );
}

#[test]
fn wrap_scrollback_text_respects_display_width_for_cjk() {
    assert_eq!(wrap_scrollback_text("你好", 2), vec!["你", "好"]);
    assert_eq!(wrap_scrollback_text("你好ab", 4), vec!["你好", "ab"]);
}

#[test]
fn scrollback_width_matches_ratatui_for_halfwidth_katakana_marks() {
    assert_eq!(wrap_scrollback_text("ｶﾞx", 2), vec!["ｶﾞ", "x"]);
    assert_eq!(padded_scrollback_row("ｶﾞ", 2), "ｶﾞ");
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
    assert_eq!(wrap_scrollback_text("ab\tcd", 10), vec!["abcd"]);
    assert_eq!(wrap_scrollback_text("你", 1), vec!["?"]);
    assert_eq!(padded_scrollback_row("你", 1), "?");
    assert_eq!(
        padded_scrollback_row(&wrap_scrollback_text("你", 1)[0], 1),
        "?"
    );
}

#[test]
fn render_scrollback_rows_prints_exactly_one_physical_row() {
    let mut buffer = Buffer::empty(Rect::new(0, 0, 12, 2));
    let rows = vec![("你好 world".to_owned(), Style::default())];

    render_scrollback_rows(&mut buffer, &rows);

    assert_eq!(buffer[(0, 0)].symbol(), "你好 world  ");
    for x in 1..12 {
        assert_eq!(buffer[(x, 0)].symbol(), "");
    }
}

#[test]
fn render_scrollback_rows_pads_cjk_by_display_width_without_trailing_cells() {
    let mut buffer = Buffer::empty(Rect::new(0, 0, 8, 1));
    let rows = vec![("你好".to_owned(), Style::default())];

    render_scrollback_rows(&mut buffer, &rows);

    assert_eq!(buffer[(0, 0)].symbol(), "你好    ");
    for x in 1..8 {
        assert_eq!(buffer[(x, 0)].symbol(), "");
    }
}

#[test]
fn padded_scrollback_row_is_width_bounded_and_drops_terminal_controls() {
    assert_eq!(padded_scrollback_row("abc\u{1b}[31mdef", 5), "abc[3");
    assert_eq!(
        crate::ui::terminal_cell_width(&padded_scrollback_row("你好 world", 8)),
        8
    );
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
fn wake_deadline_prefers_busy_then_seed_then_event_driven_idle() {
    let mut busy_app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    busy_app.runtime.is_busy = true;
    assert_eq!(
        next_wake_deadline(&busy_app, &ScrollbackSyncState::default()),
        Some(Duration::from_millis(SPINNER_FRAME_MILLIS as u64))
    );

    let seeded_app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let seeded_state = ScrollbackSyncState {
        pending_seed: Some(ScrollbackSeedProgress {
            session_id: seeded_app.session_id.clone(),
            next_line_index: 1,
            prefix_hash: 0,
        }),
        ..ScrollbackSyncState::default()
    };
    assert_eq!(
        next_wake_deadline(&seeded_app, &seeded_state),
        Some(SCROLLBACK_SEED_POLL_INTERVAL)
    );

    let setup_app = AppState::from_setup(
        PathBuf::from("sigil.toml"),
        PathBuf::from("."),
        Some("broken".to_owned()),
    );
    assert_eq!(next_wake_deadline(&setup_app, &seeded_state), None);
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
        entry_count: app.scrollback_entry_count(),
        sequence_hash: app.timeline_entry_prefix_hash(app.scrollback_entry_count()),
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
            entry_count: 0,
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
            entry_count: app.timeline_entry_count_at_or_before_line(line_count.saturating_sub(1)),
            sequence_hash: app.timeline_entry_prefix_hash(
                app.timeline_entry_count_at_or_before_line(line_count.saturating_sub(1)),
            ),
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
    assert_eq!(scrollback_frontier_update(&prepared), Some((0, true)));
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
            entry_count: 0,
            sequence_hash: app.scrollback_prefix_hash(0),
            pending_seed: None,
        },
    )
    .expect("expected append plan");

    assert!(!prepared.line_batches.is_empty());
    assert_eq!(prepared.next_state.line_count, app.scrollback_line_count());
    assert_eq!(
        scrollback_frontier_update(&prepared),
        Some((prepared.next_state.entry_count, false))
    );
}

#[test]
fn prepare_scrollback_sync_survives_rerender_width_changes_and_append() {
    let mut app = app_with_scrollback();
    let mut state = ScrollbackSyncState {
        session_id: Some(app.session_id.clone()),
        revision: app.timeline_revision(),
        line_count: app.scrollback_line_count(),
        entry_count: app.scrollback_entry_count(),
        sequence_hash: app.timeline_entry_prefix_hash(app.scrollback_entry_count()),
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
        app.timeline_entry_prefix_hash(after_narrow.next_state.entry_count)
    );
    state = after_narrow.next_state;

    assert!(app.set_terminal_size(90, 8));
    let after_wide = prepare_scrollback_sync(&app, &state)
        .expect("wide rerender should produce a scrollback sync plan");
    assert_eq!(
        after_wide.next_state.line_count,
        app.scrollback_line_count_for_entry_count(after_wide.next_state.entry_count)
    );
    assert_eq!(after_wide.next_state.entry_count, state.entry_count);
    assert_eq!(
        after_wide.next_state.sequence_hash,
        app.timeline_entry_prefix_hash(after_wide.next_state.entry_count)
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
        app.timeline_entry_prefix_hash(after_append.next_state.entry_count)
    );
}

#[test]
fn busy_reflow_and_completion_append_from_the_stable_entry_frontier() {
    let mut app = app_with_scrollback();
    let state = ScrollbackSyncState {
        session_id: Some(app.session_id.clone()),
        revision: app.timeline_revision(),
        line_count: app.scrollback_line_count(),
        entry_count: app.scrollback_entry_count(),
        sequence_hash: app.timeline_entry_prefix_hash(app.scrollback_entry_count()),
        pending_seed: None,
    };
    let owned_entries = state.entry_count;

    app.runtime.is_busy = true;
    assert!(app.set_terminal_size(32, 8));
    for index in 0..12 {
        app.handle(RunEvent::AssistantMessage(ModelMessage::assistant(
            Some(format!("completed after busy reflow {index}")),
            Vec::new(),
        )))
        .expect("assistant message should append timeline entry");
    }
    assert!(prepare_scrollback_sync(&app, &state).is_none());

    app.runtime.is_busy = false;
    let prepared = prepare_scrollback_sync(&app, &state)
        .expect("completion after reflow must reconcile native scrollback");

    assert!(prepared.next_state.entry_count > owned_entries);
    assert!(!prepared.line_batches.is_empty());
}

#[test]
fn same_session_projection_replacement_rebases_before_future_appends() {
    let mut app = app_with_scrollback();
    let state = ScrollbackSyncState {
        session_id: Some(app.session_id.clone()),
        revision: app.timeline_revision(),
        line_count: app.scrollback_line_count(),
        entry_count: app.scrollback_entry_count(),
        sequence_hash: app.timeline_entry_prefix_hash(app.scrollback_entry_count()),
        pending_seed: None,
    };

    assert!(state.entry_count > 0);
    app.timeline[0].text.push_str(" replacement");
    assert!(app.set_terminal_size(47, 6));
    let rebased = prepare_scrollback_sync(&app, &state)
        .expect("same-session projection replacement should be observed");
    assert!(rebased.rebase_frontier);
    assert!(!rebased.line_batches.is_empty());
    assert_eq!(
        scrollback_frontier_update(&rebased),
        Some((rebased.next_state.entry_count, true))
    );

    let mut rebased_state = rebased.next_state;
    for index in 0..12 {
        app.handle(RunEvent::AssistantMessage(ModelMessage::assistant(
            Some(format!("after projection replacement {index}")),
            Vec::new(),
        )))
        .expect("assistant message should append timeline entry");
    }
    let appended = prepare_scrollback_sync(&app, &rebased_state)
        .expect("new entries after a projection rebase should remain appendable");
    assert!(!appended.line_batches.is_empty());
    rebased_state = appended.next_state;
    assert_eq!(rebased_state.entry_count, app.scrollback_entry_count());
}

#[test]
fn projection_replacement_and_new_entries_are_seeded_in_the_same_sync_window() {
    let mut app = app_with_scrollback();
    let owned_entry_count = app.scrollback_entry_count();
    let state = ScrollbackSyncState {
        session_id: Some(app.session_id.clone()),
        revision: app.timeline_revision(),
        line_count: app.scrollback_line_count(),
        entry_count: owned_entry_count,
        sequence_hash: app.timeline_entry_prefix_hash(owned_entry_count),
        pending_seed: None,
    };

    app.timeline[0].text.push_str(" replacement in same window");
    assert!(app.set_terminal_size(47, 6));
    for index in 0..12 {
        app.handle(RunEvent::AssistantMessage(ModelMessage::assistant(
            Some(format!("same-window append {index}")),
            Vec::new(),
        )))
        .expect("assistant message should append timeline entry");
    }

    let prepared = prepare_scrollback_sync(&app, &state)
        .expect("replacement and append should start a current projection epoch");

    assert!(prepared.rebase_frontier);
    assert!(!prepared.line_batches.is_empty());
    assert_eq!(
        prepared.next_state.entry_count,
        app.scrollback_entry_count()
    );
    assert_eq!(
        scrollback_frontier_update(&prepared),
        Some((prepared.next_state.entry_count, true))
    );
    assert!(prepare_scrollback_sync(&app, &prepared.next_state).is_none());
}

#[test]
fn projection_reseed_never_rewinds_the_existing_native_frontier() {
    let mut app = app_with_scrollback();
    let owned_entry_count = app.scrollback_entry_count();
    let state = ScrollbackSyncState {
        session_id: Some(app.session_id.clone()),
        revision: app.timeline_revision(),
        line_count: app.scrollback_line_count(),
        entry_count: owned_entry_count,
        sequence_hash: app.timeline_entry_prefix_hash(owned_entry_count),
        pending_seed: None,
    };
    assert!(owned_entry_count > 0);

    app.timeline[0].text.push_str(" replacement");
    assert!(app.set_terminal_size(31, 8));
    let prepared = prepare_scrollback_sync_with_chunk_size(&app, &state, 1)
        .expect("replacement should begin a bounded projection seed");

    assert!(prepared.rebase_frontier);
    assert_eq!(
        scrollback_frontier_update(&prepared),
        Some((owned_entry_count.max(prepared.next_state.entry_count), true))
    );
}

#[test]
fn projection_change_during_seed_restarts_without_rewinding_the_physical_frontier() {
    let mut app = app_with_scrollback();
    let owned_entry_count = app.scrollback_entry_count();
    let state = ScrollbackSyncState {
        session_id: Some(app.session_id.clone()),
        revision: app.timeline_revision(),
        line_count: app.scrollback_line_count(),
        entry_count: owned_entry_count,
        sequence_hash: app.timeline_entry_prefix_hash(owned_entry_count),
        pending_seed: None,
    };

    app.timeline[0].text.push_str(" first replacement");
    assert!(app.set_terminal_size(31, 8));
    let first = prepare_scrollback_sync_with_chunk_size(&app, &state, 1)
        .expect("first replacement should begin a projection seed");
    assert!(first.next_state.pending_seed.is_some());
    let (first_frontier, first_rebase) =
        scrollback_frontier_update(&first).expect("seed should update the frontier");
    assert!(first_rebase);
    app.set_native_scrollback_frontier(app.session_id.clone(), first_frontier, first_rebase);

    app.timeline[0].role = TimelineRole::User;
    assert!(app.set_terminal_size(33, 8));
    let restarted = prepare_scrollback_sync_with_chunk_size(&app, &first.next_state, 1)
        .expect("changed seeded prefix should restart the projection epoch");
    assert!(restarted.rebase_frontier);
    let (restarted_frontier, restarted_rebase) =
        scrollback_frontier_update(&restarted).expect("restarted seed should update the frontier");
    assert!(restarted_rebase);
    assert!(restarted_frontier >= first_frontier);
}

#[test]
fn prepare_scrollback_sync_reseeds_when_owned_prefix_changed() {
    let app = app_with_scrollback();

    let prepared = prepare_scrollback_sync(
        &app,
        &ScrollbackSyncState {
            session_id: Some(app.session_id.clone()),
            revision: app.timeline_revision().saturating_sub(1),
            line_count: 1,
            entry_count: app.timeline_entry_count_at_or_before_line(1),
            sequence_hash: u64::MAX,
            pending_seed: None,
        },
    )
    .expect("expected replacement projection seed");

    assert!(!prepared.line_batches.is_empty());
    assert!(prepared.next_state.pending_seed.is_none());
}

#[test]
fn prepare_scrollback_sync_never_rewinds_the_native_cursor_when_layout_expands() {
    let mut app = app_with_scrollback();
    app.set_terminal_size(48, 8);
    let emitted_line_count = app.scrollback_line_count();
    assert!(emitted_line_count > 0);
    let state = ScrollbackSyncState {
        session_id: Some(app.session_id.clone()),
        revision: app.timeline_revision().saturating_sub(1),
        line_count: emitted_line_count,
        entry_count: app.scrollback_entry_count(),
        sequence_hash: app.timeline_entry_prefix_hash(app.scrollback_entry_count()),
        pending_seed: None,
    };

    app.set_terminal_size(48, 40);
    let prepared = prepare_scrollback_sync(&app, &state).expect("layout change should be observed");

    assert!(prepared.line_batches.is_empty());
    assert_eq!(prepared.next_state.line_count, emitted_line_count);
    assert_eq!(prepared.next_state.sequence_hash, state.sequence_hash);
}

#[test]
fn prepare_scrollback_sync_observes_height_only_cutoff_growth_without_a_revision() {
    let mut app = app_with_scrollback();
    app.set_terminal_size(80, 40);
    let wide_cutoff = app.scrollback_line_count();
    let state = ScrollbackSyncState {
        session_id: Some(app.session_id.clone()),
        revision: app.timeline_revision(),
        line_count: wide_cutoff,
        entry_count: app.scrollback_entry_count(),
        sequence_hash: app.timeline_entry_prefix_hash(app.scrollback_entry_count()),
        pending_seed: None,
    };

    let revision = app.timeline_revision();
    assert!(app.set_terminal_size(80, 8));
    assert_eq!(app.timeline_revision(), revision);
    let narrow_cutoff = app.scrollback_line_count();
    assert!(narrow_cutoff > wide_cutoff);
    let prepared = prepare_scrollback_sync(&app, &state)
        .expect("a larger height-only cutoff must not be hidden by the revision fast path");

    assert!(!prepared.line_batches.is_empty());
    assert_eq!(prepared.next_state.line_count, narrow_cutoff);
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
