use std::{collections::BTreeMap, path::Path, sync::mpsc, time::Duration};

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use sigil_kernel::{
    AgentConfig, CompactionConfig, MemoryConfig, PermissionConfig, RootConfig, SessionConfig,
    WorkspaceConfig,
};
use sigil_tui::{
    app::{AppAction, AppState},
    runner::WorkerCommand,
};

use super::{
    BUSY_POLL_INTERVAL, IDLE_POLL_INTERVAL, SCROLLBACK_SEED_POLL_INTERVAL, ScrollbackSeedProgress,
    ScrollbackSyncPlan, ScrollbackSyncState, WorkerRuntime, next_poll_interval,
    plan_scrollback_sync, plan_scrollback_sync_with_chunk_size, process_app_action,
    render_scrollback_rows, scrollback_plain_line, scrollback_row_style, scrollback_separator,
    should_sync_terminal_scrollback, wrap_scrollback_text,
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
