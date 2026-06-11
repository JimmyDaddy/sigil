use super::{
    EventEntry, LiveActivitySummary, RunPhase, SessionHistoryRow, SidebarAgentRow, SidebarCard,
    ThinkingBlockMode, TimelineEntry, TimelineRole, ToolActivityCacheEntry,
};

#[test]
fn sidebar_card_navigation_wraps() {
    assert_eq!(SidebarCard::Permission.label(), "permission");
    assert_eq!(SidebarCard::Agents.label(), "agents");
    assert_eq!(SidebarCard::Usage.label(), "usage");
    assert_eq!(SidebarCard::Permission.next(), SidebarCard::Agents);
    assert_eq!(SidebarCard::Usage.next(), SidebarCard::Permission);
    assert_eq!(SidebarCard::Permission.previous(), SidebarCard::Usage);
    assert_eq!(SidebarCard::Agents.previous(), SidebarCard::Permission);
}

#[test]
fn thinking_block_mode_labels_match_persisted_copy() {
    assert_eq!(ThinkingBlockMode::Collapsed.as_str(), "collapsed");
    assert_eq!(ThinkingBlockMode::Expanded.as_str(), "expanded");
}

#[test]
fn timeline_data_structures_preserve_projection_fields() {
    let entry = TimelineEntry {
        role: TimelineRole::Assistant,
        text: "hello".to_owned(),
    };
    let event = EventEntry {
        label: "run".to_owned(),
        detail: "finished".to_owned(),
    };
    let activity = ToolActivityCacheEntry {
        index: 3,
        key: "tool:3".to_owned(),
        defaults_expanded: true,
    };
    let summary = LiveActivitySummary {
        label: "tool".to_owned(),
        detail: "read_file".to_owned(),
    };
    let agent = SidebarAgentRow {
        label: "deepseek".to_owned(),
        detail: "ready".to_owned(),
        selected: true,
        muted: false,
    };

    assert!(matches!(entry.role, TimelineRole::Assistant));
    assert_eq!(entry.text, "hello");
    assert_eq!(event.detail, "finished");
    assert_eq!(activity.index, 3);
    assert!(activity.defaults_expanded);
    assert_eq!(summary.label, "tool");
    assert!(agent.selected);
    assert!(!agent.muted);
}

#[test]
fn session_history_rows_capture_selector_states() {
    let header = SessionHistoryRow::SessionHeader {
        filter: "abc".to_owned(),
        total: 2,
    };
    let item = SessionHistoryRow::SessionItem {
        index: 1,
        label: "first prompt".to_owned(),
        current: true,
        selected: true,
        meta: "8h ago".to_owned(),
    };
    let empty = SessionHistoryRow::Empty {
        text: "no sessions".to_owned(),
    };

    assert!(matches!(
        header,
        SessionHistoryRow::SessionHeader {
            ref filter,
            total: 2,
        } if filter == "abc"
    ));
    assert!(matches!(
        item,
        SessionHistoryRow::SessionItem {
            index: 1,
            current: true,
            selected: true,
            ..
        }
    ));
    assert!(matches!(
        empty,
        SessionHistoryRow::Empty { ref text } if text == "no sessions"
    ));
}

#[test]
fn run_phase_variants_hold_user_facing_state() {
    assert!(matches!(RunPhase::Idle, RunPhase::Idle));
    assert!(matches!(RunPhase::Thinking, RunPhase::Thinking));
    assert!(matches!(RunPhase::Streaming, RunPhase::Streaming));
    assert!(matches!(
        RunPhase::Tool("read_file".to_owned()),
        RunPhase::Tool(ref name) if name == "read_file"
    ));
}
