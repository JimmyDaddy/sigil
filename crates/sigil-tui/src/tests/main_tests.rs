use std::{collections::BTreeMap, path::Path, path::PathBuf, sync::mpsc, time::Duration};

use anyhow::{Result, anyhow};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use sigil_kernel::{
    AgentConfig, CompactionConfig, MemoryConfig, ModelMessage, PermissionConfig, RootConfig,
    SessionConfig, SessionLogEntry, WorkspaceConfig,
};
use sigil_tui::{
    app::{AppAction, AppState},
    runner::{WorkerCommand, WorkerMessage},
};

use super::{
    AppMouseOutcome, BUSY_POLL_INTERVAL, IDLE_POLL_INTERVAL, SCROLLBACK_SEED_POLL_INTERVAL,
    ScrollbackSeedProgress, ScrollbackSyncPlan, ScrollbackSyncState, WorkerRuntime,
    apply_key_action, apply_mouse_outcome, build_initial_app, drain_worker_messages,
    next_poll_interval, plan_scrollback_sync, plan_scrollback_sync_with_chunk_size, poll_interval,
    prepare_scrollback_sync, process_app_action, process_app_action_with_spawner,
    render_scrollback_rows, scrollback_plain_line, scrollback_row_style, scrollback_separator,
    scrollback_wrapped_rows, should_sync_terminal_scrollback, wrap_scrollback_text,
};

fn test_config() -> RootConfig {
    RootConfig {
        workspace: WorkspaceConfig {
            root: ".".to_owned(),
        },
        session: SessionConfig {
            log_dir: ".sigil/sessions".to_owned(),
        },
        agent: AgentConfig {
            provider: "deepseek".to_owned(),
            model: "deepseek-v4-flash".to_owned(),
            max_turns: None,
            tool_timeout_secs: 30,
        },
        permission: PermissionConfig::default(),
        memory: MemoryConfig { enabled: true },
        compaction: CompactionConfig::default(),
        code_intelligence: Default::default(),
        providers: BTreeMap::new(),
        mcp_servers: Vec::new(),
    }
}

#[test]
fn initial_sync_seeds_without_replaying_history() {
    let state = ScrollbackSyncState::default();

    let plan = plan_scrollback_sync(&state, "session-a", 2, 0);

    assert_eq!(
        plan,
        ScrollbackSyncPlan::Seed {
            insert_separator: false,
            from_index: 0,
            to_index: 2,
            total_line_count: 2,
        }
    );
}

#[test]
fn initial_sync_seeds_only_first_chunk_for_large_history() {
    let state = ScrollbackSyncState::default();

    let plan = plan_scrollback_sync_with_chunk_size(&state, "session-a", 5, 0, 2);

    assert_eq!(
        plan,
        ScrollbackSyncPlan::Seed {
            insert_separator: false,
            from_index: 0,
            to_index: 2,
            total_line_count: 5,
        }
    );
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
fn zero_chunk_size_still_seeds_one_line() {
    let state = ScrollbackSyncState::default();

    let plan = plan_scrollback_sync_with_chunk_size(&state, "session-a", 3, 0, 0);

    assert_eq!(
        plan,
        ScrollbackSyncPlan::Seed {
            insert_separator: false,
            from_index: 0,
            to_index: 1,
            total_line_count: 3,
        }
    );
}

#[test]
fn stale_pending_seed_restarts_seed_for_current_session() {
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

    assert_eq!(
        plan,
        ScrollbackSyncPlan::Seed {
            insert_separator: true,
            from_index: 0,
            to_index: 2,
            total_line_count: 4,
        }
    );
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
fn switching_sessions_without_existing_scrollback_skips_separator() {
    let state = ScrollbackSyncState {
        session_id: Some("session-a".to_owned()),
        revision: 2,
        line_count: 0,
        sequence_hash: 0,
        pending_seed: None,
    };

    let plan = plan_scrollback_sync_with_chunk_size(&state, "session-b", 3, 0, 2);

    assert_eq!(
        plan,
        ScrollbackSyncPlan::Seed {
            insert_separator: false,
            from_index: 0,
            to_index: 2,
            total_line_count: 3,
        }
    );
}

#[test]
fn restored_or_switched_session_reseeds_without_replaying_old_lines() {
    let state = ScrollbackSyncState {
        session_id: Some("session-a".to_owned()),
        revision: 2,
        line_count: 1,
        sequence_hash: 9,
        pending_seed: None,
    };

    let plan = plan_scrollback_sync(&state, "session-b", 2, 3);

    assert_eq!(
        plan,
        ScrollbackSyncPlan::Seed {
            insert_separator: true,
            from_index: 0,
            to_index: 2,
            total_line_count: 2,
        }
    );
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

    app.is_busy = true;

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

    app.is_busy = true;
    assert_eq!(
        next_poll_interval(&app, &ScrollbackSyncState::default()),
        BUSY_POLL_INTERVAL
    );

    app.is_busy = false;
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
fn process_app_action_ignores_worker_command_without_runtime() -> anyhow::Result<()> {
    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    let mut worker = None;

    process_app_action(&mut app, &mut worker, AppAction::CancelRun)?;

    assert!(worker.is_none());
    Ok(())
}

fn fake_worker_runtime() -> (
    WorkerRuntime,
    mpsc::Receiver<sigil_tui::runner::WorkerCommand>,
) {
    let (worker_tx, worker_rx) = mpsc::channel();
    let (_message_tx, message_rx) = mpsc::channel::<WorkerMessage>();
    (
        WorkerRuntime {
            worker_tx,
            worker_rx: message_rx,
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
    app
}

#[test]
fn poll_interval_prefers_busy_then_seed_then_idle() {
    let mut busy_app = AppState::from_root_config(Path::new("sigil.toml"), &test_config());
    busy_app.is_busy = true;
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
fn prepare_scrollback_sync_reseeds_and_appends_expected_batches() {
    let app = app_with_scrollback();
    let line_count = app.scrollback_line_count();
    assert!(line_count > 0);

    let reseed = prepare_scrollback_sync(
        &app,
        &ScrollbackSyncState {
            session_id: Some("previous-session".to_owned()),
            revision: 1,
            line_count: 1,
            sequence_hash: 7,
            pending_seed: None,
        },
    )
    .expect("expected reseed");
    assert!(!reseed.line_batches.is_empty());
    assert_eq!(
        scrollback_plain_line(&reseed.line_batches[0][0]),
        scrollback_plain_line(&scrollback_separator(&app))
    );
    assert_eq!(reseed.next_state.session_id, Some(app.session_id.clone()));

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
    assert!(separator.contains(&app.provider_name));
    assert!(separator.contains(&app.model_name));
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
fn build_initial_app_spawns_worker_for_loaded_config() -> Result<()> {
    let (app, worker) = build_initial_app(
        PathBuf::from("."),
        PathBuf::from("sigil.toml"),
        Ok(test_config()),
        |_root_config, _app| Ok(fake_worker_runtime().0),
    )?;

    assert!(!app.is_setup_mode());
    assert!(worker.is_some());
    Ok(())
}

#[test]
fn process_app_action_restarts_worker_for_config_save() -> Result<()> {
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
    assert!(matches!(
        shutdown,
        sigil_tui::runner::WorkerCommand::Shutdown
    ));
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
        sigil_tui::runner::WorkerCommand::SubmitPrompt { ref prompt, reasoning_effort: _ }
            if prompt == "hello"
    ));
    Ok(())
}

#[test]
fn process_app_action_bootstraps_app_after_setup_completion() -> Result<()> {
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
    assert_eq!(app.provider_name, "deepseek");
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
    });
    message_tx.send(WorkerMessage::RunStarted {
        prompt: "hello".to_owned(),
    })?;

    assert!(drain_worker_messages(&mut app, &mut worker)?);
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
        sigil_tui::runner::WorkerCommand::CheckChangedFilesDiagnostics
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
    assert!(matches!(
        command,
        sigil_tui::runner::WorkerCommand::CancelRun
    ));
    Ok(())
}
