use std::{collections::BTreeMap, path::Path};

use ratatui::{buffer::Buffer, layout::Rect, style::Style};
use termquill_kernel::{
    AgentConfig, CompactionConfig, MemoryConfig, PermissionConfig, RootConfig, SessionConfig,
    WorkspaceConfig,
};
use termquill_tui::app::AppState;

use super::{
    ScrollbackSeedProgress, ScrollbackSyncPlan, ScrollbackSyncState, plan_scrollback_sync,
    plan_scrollback_sync_with_chunk_size, render_scrollback_rows, should_sync_terminal_scrollback,
    wrap_scrollback_text,
};

fn test_config() -> RootConfig {
    RootConfig {
        workspace: WorkspaceConfig {
            root: ".".to_owned(),
        },
        session: SessionConfig {
            log_dir: ".termquill/sessions".to_owned(),
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
    let mut app = AppState::from_root_config(Path::new("termquill.toml"), &test_config());

    assert!(should_sync_terminal_scrollback(&app));

    app.is_busy = true;

    assert!(!should_sync_terminal_scrollback(&app));
}

#[test]
fn wrap_scrollback_text_respects_display_width_for_cjk() {
    assert_eq!(wrap_scrollback_text("你好", 2), vec!["你", "好"]);
    assert_eq!(wrap_scrollback_text("你好ab", 4), vec!["你好", "ab"]);
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
