use ratatui::{buffer::Buffer, layout::Rect, style::Style};

use super::{
    ScrollbackSyncPlan, ScrollbackSyncState, plan_scrollback_sync, render_scrollback_rows,
    wrap_scrollback_text,
};

#[test]
fn initial_sync_seeds_without_replaying_history() {
    let state = ScrollbackSyncState::default();

    let plan = plan_scrollback_sync(&state, "session-a", 2, 0);

    assert_eq!(
        plan,
        ScrollbackSyncPlan::Seed {
            insert_separator: false
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
    };

    let plan = plan_scrollback_sync(&state, "session-b", 2, 3);

    assert_eq!(
        plan,
        ScrollbackSyncPlan::Seed {
            insert_separator: true
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
    };

    let plan = plan_scrollback_sync(&state, "session-a", 2, 12);

    assert_eq!(plan, ScrollbackSyncPlan::Noop);
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
