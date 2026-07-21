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
        assistant_kind: None,
        tool_input: None,
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

#[test]
fn attachment_projection_is_bounded_and_marks_evicted_detail_as_a_gap() {
    let mut projection = RunProjection::new(DesktopRunStatus::Running, false);
    for sequence in 1..=(MAX_ATTACHMENT_EVENTS as u64 + 1) {
        projection.push(timeline(sequence, DesktopTimelineEventKind::Notice));
    }

    let snapshot = projection.snapshot();
    assert_eq!(snapshot.events.len(), MAX_ATTACHMENT_EVENTS);
    assert_eq!(snapshot.events[0].sequence, 2);
    assert!(snapshot.has_gap);
}

#[test]
fn pending_approval_survives_timeline_eviction_for_safe_reattach() {
    let mut projection = RunProjection::new(DesktopRunStatus::WaitingForApproval, false);
    let mut approval = timeline(1, DesktopTimelineEventKind::ApprovalRequested);
    approval.item_id = Some("call-1".to_owned());
    approval.approval = Some(sigil_desktop::DesktopTimelineApproval {
        call_id: "call-1".to_owned(),
        tool_name: "write_file".to_owned(),
        approval_request_id: "approval-1".to_owned(),
        tool_call_hash: "hash-1".to_owned(),
        policy_version: "policy-1".to_owned(),
        expires_at_ms: 1,
        session_grant_available: false,
        tool_input: None,
        operation: None,
        risk: None,
        snapshot_required: true,
        preview_title: None,
        preview_summary: None,
        preview_body: None,
    });
    projection.push(approval);
    for sequence in 2..=(MAX_ATTACHMENT_EVENTS as u64 + 2) {
        projection.push(timeline(sequence, DesktopTimelineEventKind::Notice));
    }

    let snapshot = projection.snapshot();
    assert!(snapshot.has_gap);
    assert!(snapshot.events.iter().any(|event| {
        event.kind == DesktopTimelineEventKind::ApprovalRequested
            && event.item_id.as_deref() == Some("call-1")
    }));
}

#[tokio::test]
async fn reconnect_state_records_an_honest_attachment_gap() {
    let owner = DesktopRunStreamOwner::default();
    owner.streams.lock().await.insert(
        stream_key("workspace-1", "run-1"),
        OwnedRunStream {
            workspace_id: "workspace-1".to_owned(),
            renderer_session_id: "session-1".to_owned(),
            durable_session_id: "durable-1".to_owned(),
            task: None,
            projection: RunProjection::new(DesktopRunStatus::Running, false),
        },
    );

    owner
        .record_status(
            "workspace-1",
            "run-1",
            DesktopRunStreamState::Reconnecting,
            Some("test reconnect"),
        )
        .await;

    let streams = owner.streams.lock().await;
    let snapshot = streams
        .get(&stream_key("workspace-1", "run-1"))
        .expect("owned stream")
        .projection
        .snapshot();
    assert!(snapshot.has_gap);
    assert_eq!(snapshot.stream_state, DesktopRunStreamState::Reconnecting);
}

fn timeline(sequence: u64, kind: DesktopTimelineEventKind) -> DesktopTimelineEvent {
    DesktopTimelineEvent {
        workspace_id: "workspace-1".to_owned(),
        session_id: "session-1".to_owned(),
        run_id: "run-1".to_owned(),
        sequence,
        replayable: true,
        replay_id: Some(format!("event-{sequence}")),
        kind,
        text: Some("detail".to_owned()),
        item_id: None,
        tool_name: None,
        status: None,
        assistant_kind: None,
        tool_input: None,
        approval: None,
    }
}
