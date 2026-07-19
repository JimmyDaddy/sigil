use super::*;

#[test]
fn reconnect_backoff_is_bounded_and_stream_keys_are_workspace_scoped() {
    assert_eq!(reconnect_delay(0), Duration::from_millis(250));
    assert_eq!(reconnect_delay(1), Duration::from_millis(500));
    assert_eq!(reconnect_delay(8), Duration::from_millis(2_000));
    assert_ne!(
        stream_key("workspace-a", "run-1"),
        stream_key("workspace-b", "run-1")
    );
}

#[test]
fn only_server_terminal_events_finish_a_stream() {
    let event = |kind| DesktopTimelineEvent {
        workspace_id: "workspace-1".to_owned(),
        session_id: "session-1".to_owned(),
        run_id: "run-1".to_owned(),
        sequence: 1,
        replayable: true,
        replay_id: None,
        kind,
        text: None,
        item_id: None,
        tool_name: None,
        status: None,
        approval: None,
    };
    assert!(!timeline_is_terminal(&event(
        DesktopTimelineEventKind::AssistantMessage
    )));
    assert!(timeline_is_terminal(&event(
        DesktopTimelineEventKind::RunFinished
    )));
    assert!(timeline_is_terminal(&event(
        DesktopTimelineEventKind::RunCancelled
    )));
}
