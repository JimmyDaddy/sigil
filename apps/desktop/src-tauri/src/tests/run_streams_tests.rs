use super::*;

fn preview(
    revision: u64,
    base: u64,
    attempt: &str,
    slot: &str,
    text: &str,
) -> DesktopTimelineEvent {
    let mut event = timeline(base, DesktopTimelineEventKind::AssistantDelta);
    event.replayable = false;
    event.replay_id = None;
    event.text = Some(text.to_owned());
    event.item_id = Some(slot.to_owned());
    event.live_preview = Some(sigil_desktop::DesktopTimelineLivePreview {
        attempt_id: Some(attempt.to_owned()),
        slot_id: slot.to_owned(),
        revision: revision.to_string(),
        base_sequence: base.to_string(),
        truncated: false,
    });
    event
}

fn completed_call(sequence: u64, call: &str) -> DesktopTimelineEvent {
    let mut event = timeline(sequence, DesktopTimelineEventKind::ToolCompleted);
    event.item_id = Some(call.to_owned());
    event.tool_name = Some("exec_command".to_owned());
    event.tool_input = Some("printf visible".to_owned());
    event
}

fn execution_preview(
    revision: u64,
    base: u64,
    call: &str,
    execution: &str,
    text: &str,
) -> DesktopTimelineEvent {
    let mut event = preview(revision, base, "unused", execution, text);
    event.kind = DesktopTimelineEventKind::ToolProgress;
    event.item_id = Some(call.to_owned());
    event.execution_id = Some(execution.to_owned());
    event.tool_name = Some("exec_command".to_owned());
    event.live_preview.as_mut().expect("preview").attempt_id = None;
    event
}

#[test]
fn native_execution_snapshots_survive_provider_retry_and_retire_only_their_call() {
    let mut cursor = RunEventCursor::default();
    let mut projection = RunProjection::new(DesktopRunStatus::Running, false);
    for event in [
        timeline(1, DesktopTimelineEventKind::RunStarted),
        completed_call(2, "call-a"),
        completed_call(3, "call-b"),
    ] {
        assert!(cursor.accept(&event));
        projection.push(event);
    }
    for event in [
        preview(100, 3, "attempt-a", "execution-a", "provider"),
        execution_preview(1000, 3, "call-a", "execution-a", "a"),
        execution_preview(1001, 3, "call-b", "execution-b", "b"),
        preview(101, 3, "attempt-b", "text", "retry"),
    ] {
        assert!(cursor.accept(&event));
        projection.push(event);
    }
    assert_eq!(cursor.attempt.as_deref(), Some("attempt-b"));
    assert_eq!(cursor.latest_live_revision, 101);
    assert_eq!(
        projection
            .events
            .iter()
            .filter(|event| is_execution_preview(event))
            .count(),
        2
    );
    let discarded = timeline(
        4,
        DesktopTimelineEventKind::ProviderTurnPartialOutputDiscarded,
    );
    assert!(cursor.accept(&discarded));
    projection.push(discarded);
    let message = timeline(5, DesktopTimelineEventKind::AssistantMessage);
    assert!(cursor.accept(&message));
    projection.push(message);
    let sibling = execution_preview(1002, 3, "call-b", "execution-b", "still running");
    assert!(
        cursor.accept(&sibling),
        "provider frontier does not retire execution snapshots"
    );
    projection.push(sibling);
    assert_eq!(
        projection
            .events
            .iter()
            .filter(|event| is_execution_preview(event))
            .count(),
        2
    );
    let mut result = timeline(6, DesktopTimelineEventKind::ToolResult);
    result.item_id = Some("call-a".to_owned());
    assert!(cursor.accept(&result));
    projection.push(result);
    assert!(!cursor.accept(&execution_preview(2000, 6, "call-a", "execution-a", "late")));
    assert!(!cursor.accept(&execution_preview(
        2000,
        6,
        "call-b",
        "execution-other",
        "wrong binding"
    )));
    let next = execution_preview(1003, 6, "call-b", "execution-b", "next");
    assert!(cursor.accept(&next));
    projection.push(next);
    assert_eq!(
        projection
            .events
            .iter()
            .filter(|event| is_execution_preview(event))
            .count(),
        1
    );
    let terminal = timeline(7, DesktopTimelineEventKind::RunFinished);
    assert!(cursor.accept(&terminal));
    projection.push(terminal);
    assert!(
        projection
            .events
            .iter()
            .all(|event| event.live_preview.is_none())
    );
    assert!(!cursor.accept(&execution_preview(
        2001,
        7,
        "call-b",
        "execution-b",
        "post terminal"
    )));
}

#[test]
fn native_provider_and_execution_snapshots_have_independent_bounded_budgets() {
    let mut cursor = RunEventCursor::default();
    let mut projection = RunProjection::new(DesktopRunStatus::Running, false);
    let started = timeline(1, DesktopTimelineEventKind::RunStarted);
    assert!(cursor.accept(&started));
    projection.push(started);
    for index in 0..8 {
        let completed = completed_call(index + 2, &format!("call-{index}"));
        assert!(cursor.accept(&completed));
        projection.push(completed);
    }
    for index in 0..8 {
        for event in [
            execution_preview(
                index + 1,
                9,
                &format!("call-{index}"),
                &format!("execution-{index}"),
                "output",
            ),
            preview(
                index + 1,
                9,
                "attempt",
                &format!("provider-{index}"),
                "text",
            ),
        ] {
            assert!(cursor.accept(&event));
            projection.push(event);
        }
    }
    assert_eq!(
        projection
            .events
            .iter()
            .filter(|event| is_execution_preview(event))
            .count(),
        4
    );
    assert_eq!(
        projection
            .events
            .iter()
            .filter(|event| event.live_preview.is_some() && !is_execution_preview(event))
            .count(),
        4
    );
    assert_eq!(projection.last_sequence, 9);
    let mut malformed = execution_preview(20, 9, "call-7", "execution-7", "bad");
    malformed.live_preview.as_mut().expect("preview").attempt_id = Some("fake-provider".to_owned());
    assert!(!cursor.accept(&malformed));
    assert!(!cursor.accept(&execution_preview(
        20,
        9,
        "unknown-call",
        "execution-unknown",
        "unadmitted"
    )));
}

#[test]
fn native_execution_binding_rejects_alias_wrong_tool_old_source_and_terminal_reopen() {
    let mut cursor = RunEventCursor::default();
    for event in [
        timeline(1, DesktopTimelineEventKind::RunStarted),
        completed_call(2, "call-a"),
        completed_call(3, "call-b"),
    ] {
        assert!(cursor.accept(&event));
    }
    assert!(cursor.accept(&execution_preview(1, 3, "call-a", "execution-a", "a")));
    assert!(!cursor.accept(&execution_preview(2, 3, "call-b", "execution-a", "alias")));
    let mut wrong_tool = execution_preview(2, 3, "call-b", "execution-b", "wrong tool");
    wrong_tool.tool_name = Some("read_file".to_owned());
    assert!(!cursor.accept(&wrong_tool));
    let mut terminal = terminal_timeline(4, 1, "exited");
    terminal.terminal_task.as_mut().expect("task").task_id = "execution-b".to_owned();
    assert!(cursor.accept(&terminal));
    assert!(!cursor.accept(&execution_preview(
        3,
        4,
        "call-b",
        "execution-b",
        "already exited before first preview"
    )));
    assert!(cursor.accept(&timeline(5, DesktopTimelineEventKind::RunStarted)));
    assert!(cursor.accept(&completed_call(6, "call-a")));
    assert!(!cursor.accept(&execution_preview(
        100_000,
        3,
        "call-a",
        "execution-old",
        "old source"
    )));
    assert!(cursor.accept(&execution_preview(
        1,
        6,
        "call-a",
        "execution-new",
        "new source"
    )));
}

#[test]
fn native_terminal_snapshot_retires_execution_cursor_and_queued_preview() {
    let mut projection = RunProjection::new(DesktopRunStatus::Running, false);
    for event in [
        timeline(1, DesktopTimelineEventKind::RunStarted),
        completed_call(2, "call-a"),
        execution_preview(1, 2, "call-a", "terminal-1", "running"),
    ] {
        assert!(projection.event_cursor.accept(&event));
        projection.push(event);
    }
    let mut run = run_snapshot(3, vec![]);
    run.terminal_tasks = vec![terminal_snapshot(2, "exited")];
    assert!(projection.reconcile_run_snapshot(&run, "workspace-1", "session-1"));
    assert!(
        projection
            .events
            .iter()
            .all(|event| !is_execution_preview(event))
    );
    assert!(!projection.accept_stream_event(&execution_preview(
        10,
        2,
        "call-a",
        "terminal-1",
        "late"
    )));
}

#[test]
fn native_historical_terminal_snapshot_does_not_consume_new_execution_admission() {
    let mut projection = RunProjection::new(DesktopRunStatus::Running, false);
    for event in [
        timeline(1, DesktopTimelineEventKind::RunStarted),
        completed_call(2, "new-call"),
    ] {
        assert!(projection.accept_stream_event(&event));
        projection.push(event);
    }
    let mut run = run_snapshot(3, vec![]);
    run.terminal_tasks = (0..(MAX_ATTACHMENT_EVENTS + 16))
        .map(|index| {
            let mut task = terminal_snapshot(1, "exited");
            task.task_id = format!("old-execution-{index}");
            task
        })
        .collect();
    assert!(projection.reconcile_run_snapshot(&run, "workspace-1", "session-1"));
    assert!(
        projection.event_cursor.terminal_execution_ids.is_empty(),
        "canonical history is not copied into unresolved admission tombstones"
    );
    assert!(!projection.accept_stream_event(&execution_preview(
        1,
        2,
        "new-call",
        "old-execution-0",
        "old"
    )));
    assert!(projection.accept_stream_event(&execution_preview(
        1,
        2,
        "new-call",
        "new-execution",
        "current command stays visible"
    )));
}

#[test]
fn native_serial_execution_retirement_does_not_limit_later_commands() {
    let mut cursor = RunEventCursor::default();
    assert!(cursor.accept(&timeline(1, DesktopTimelineEventKind::RunStarted)));
    for index in 0..(MAX_ATTACHMENT_EVENTS as u64 + 16) {
        let sequence = 2 + index * 3;
        let call = format!("call-{index}");
        let execution = format!("execution-{index}");
        assert!(cursor.accept(&completed_call(sequence, &call)));
        // Fast exit may precede both the first ephemeral preview and the durable receipt.
        let mut terminal = terminal_timeline(sequence + 1, 1, "exited");
        terminal.terminal_task.as_mut().expect("task").task_id = execution.clone();
        assert!(cursor.accept(&terminal));
        assert!(!cursor.accept(&execution_preview(
            1,
            sequence + 1,
            &call,
            &execution,
            "late"
        )));
        let mut result = timeline(sequence + 2, DesktopTimelineEventKind::ToolResult);
        result.item_id = Some(call);
        result.execution_id = Some(execution);
        assert!(cursor.accept(&result));
        assert!(cursor.terminal_execution_ids.is_empty());
        assert!(cursor.execution_calls.is_empty());
    }
    let next_sequence = cursor.durable_sequence + 1;
    assert!(cursor.accept(&completed_call(next_sequence, "next-call")));
    assert!(cursor.accept(&execution_preview(
        1,
        next_sequence,
        "next-call",
        "next-execution",
        "still visible"
    )));
}

#[test]
fn native_attachment_command_bodies_count_towards_text_budget() {
    let mut projection = RunProjection::new(DesktopRunStatus::Running, false);
    for sequence in 1..=80 {
        let mut event = completed_call(sequence, &format!("call-{sequence}"));
        event.tool_input = Some("x".repeat(65_536));
        projection.push(event);
    }
    assert!(projection.event_text_bytes <= MAX_ATTACHMENT_TEXT_BYTES);
    assert!(projection.events.len() < 80);
    assert!(projection.has_gap);
    assert_eq!(
        projection.event_text_bytes,
        projection
            .events
            .iter()
            .map(event_text_bytes)
            .sum::<usize>()
    );
}

#[tokio::test]
async fn native_owned_cursor_survives_follower_replacement_and_full_gap_reset() {
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
    for event in [
        timeline(1, DesktopTimelineEventKind::RunStarted),
        completed_call(2, "call-a"),
        execution_preview(1, 2, "call-a", "execution-a", "before reconnect"),
    ] {
        assert_eq!(owner.record_stream_event(event).await, Some(false));
    }
    // Replacing the follower task changes no cursor facts retained by the stream owner.
    owner
        .record_status(
            "workspace-1",
            "run-1",
            DesktopRunStreamState::Reconnecting,
            None,
        )
        .await;
    assert_eq!(
        owner
            .record_stream_event(execution_preview(
                2,
                2,
                "call-a",
                "execution-a",
                "after reconnect"
            ))
            .await,
        Some(false)
    );
    let mut result = timeline(3, DesktopTimelineEventKind::ToolResult);
    result.item_id = Some("call-a".to_owned());
    assert_eq!(owner.record_stream_event(result).await, Some(false));
    assert_eq!(
        owner
            .record_stream_event(execution_preview(3, 3, "call-a", "execution-a", "late"))
            .await,
        None
    );
    owner.reset_event_cursor("workspace-1", "run-1").await;
    assert_eq!(
        owner
            .record_stream_event(timeline(1, DesktopTimelineEventKind::RunStarted))
            .await,
        Some(false)
    );
    assert_eq!(
        owner.record_stream_event(completed_call(2, "call-a")).await,
        Some(false)
    );
    assert_eq!(
        owner
            .record_stream_event(execution_preview(1, 2, "call-a", "execution-a", "replayed"))
            .await,
        Some(false)
    );
    let streams = owner.streams.lock().await;
    let snapshot = streams
        .get(&stream_key("workspace-1", "run-1"))
        .expect("stream")
        .projection
        .snapshot();
    assert_eq!(
        snapshot
            .events
            .iter()
            .filter(|event| is_execution_preview(event))
            .count(),
        1
    );
    let projection = &streams
        .get(&stream_key("workspace-1", "run-1"))
        .expect("stream")
        .projection;
    assert_eq!(
        projection.last_sequence, 2,
        "replayed duplicate records advance the follower frontier"
    );
    assert_eq!(projection.last_replay_id.as_deref(), Some("event-2"));
    assert_eq!(projection.event_cursor.durable_sequence, 2);
}

#[test]
fn native_tool_result_retires_progress_without_reopening_it() {
    let mut cursor = RunEventCursor::default();
    let mut projection = RunProjection::new(DesktopRunStatus::Running, false);
    let started = timeline(3, DesktopTimelineEventKind::RunStarted);
    assert!(cursor.accept(&started));
    projection.push(started);
    let completed = completed_call(4, "call-1");
    assert!(cursor.accept(&completed));
    projection.push(completed);
    let mut progress = execution_preview(1000, 4, "call-1", "execution-1", "output");
    assert!(cursor.accept(&progress));
    projection.push(progress.clone());
    let mut result = timeline(5, DesktopTimelineEventKind::ToolResult);
    result.item_id = Some("call-1".to_owned());
    assert!(cursor.accept(&result));
    projection.push(result);
    assert!(
        projection
            .events
            .iter()
            .all(|event| event.live_preview.is_none())
    );
    progress.live_preview.as_mut().expect("preview").revision = "100000".to_owned();
    assert!(!cursor.accept(&progress));
    progress.sequence = 5;
    progress.run_sequence = "5".to_owned();
    progress
        .live_preview
        .as_mut()
        .expect("preview")
        .base_sequence = "5".to_owned();
    assert!(
        !cursor.accept(&progress),
        "durable tool result closes the semantic slot"
    );
}

#[test]
fn native_live_revisions_never_skip_durable_message_approval_or_terminal() {
    let mut cursor = RunEventCursor::default();
    assert!(cursor.accept(&timeline(3, DesktopTimelineEventKind::RunStarted)));
    for revision in 1..=100_000 {
        assert!(cursor.accept(&preview(revision, 3, "attempt-1", "text", "snapshot")));
    }
    assert_eq!(cursor.durable_sequence, 3);
    assert!(cursor.accept(&timeline(4, DesktopTimelineEventKind::AssistantMessage)));
    assert!(!cursor.accept(&preview(100_001, 3, "attempt-1", "text", "late")));
    assert!(!cursor.accept(&preview(100_001, 4, "attempt-1", "text", "late equal base")));
    assert!(cursor.accept(&preview(
        100_002,
        4,
        "attempt-2",
        "text",
        "next provider turn"
    )));
    assert!(cursor.accept(&timeline(5, DesktopTimelineEventKind::ApprovalRequested)));
    assert!(cursor.accept(&timeline(6, DesktopTimelineEventKind::RunFinished)));
    assert!(!cursor.accept(&preview(100_002, 6, "attempt-1", "text", "post-terminal")));
    assert_eq!(cursor.durable_sequence, 6);
}

#[test]
fn native_waiting_retires_painted_preview_and_resumed_source_can_restart_revision() {
    let mut cursor = RunEventCursor::default();
    let mut projection = RunProjection::new(DesktopRunStatus::Running, false);
    assert!(cursor.accept(&timeline(3, DesktopTimelineEventKind::RunStarted)));
    let partial = preview(100_000, 3, "old-attempt", "text", "partial");
    assert!(cursor.accept(&partial));
    projection.push(partial);
    let mut waiting = timeline(4, DesktopTimelineEventKind::UserInputChanged);
    waiting.status = Some("requested".into());
    assert!(cursor.accept(&waiting));
    projection.push(waiting);
    assert!(
        projection
            .events
            .iter()
            .all(|event| event.live_preview.is_none())
    );
    assert!(!cursor.accept(&preview(100_001, 4, "old-attempt", "text", "late")));
    assert!(cursor.accept(&timeline(5, DesktopTimelineEventKind::RunStarted)));
    assert!(!cursor.accept(&preview(100_001, 4, "old-attempt", "text", "old source")));
    assert!(cursor.accept(&preview(1, 5, "resumed-attempt", "text", "resumed")));
}

#[test]
fn native_live_cursor_rejects_future_base_stale_attempt_and_duplicate_revision() {
    let mut cursor = RunEventCursor::default();
    assert!(!cursor.accept(&preview(1, 1, "attempt-1", "text", "future")));
    assert!(cursor.accept(&timeline(1, DesktopTimelineEventKind::RunStarted)));
    assert!(cursor.accept(&preview(10, 1, "attempt-1", "text", "first")));
    assert!(!cursor.accept(&preview(10, 1, "attempt-1", "text", "duplicate")));
    assert!(cursor.accept(&preview(11, 1, "attempt-2", "text", "next")));
    assert!(!cursor.accept(&preview(10, 1, "attempt-1", "text", "old attempt")));
    for revision in 12..1000 {
        assert!(cursor.accept(&preview(
            revision,
            1,
            "attempt-2",
            &format!("slot-{revision}"),
            "bounded"
        )));
        assert!(cursor.live_revisions.len() <= 4);
    }
}

#[test]
fn native_attachment_replaces_snapshots_and_final_content_without_growing_with_deltas() {
    for updates in [1, 1_000, 100_000] {
        let mut projection = RunProjection::new(DesktopRunStatus::Running, false);
        projection.push(timeline(3, DesktopTimelineEventKind::RunStarted));
        for revision in 1..=updates {
            projection.push(preview(
                revision,
                3,
                "attempt",
                "text",
                "latest full snapshot",
            ));
        }
        assert_eq!(projection.events.len(), 2);
        assert_eq!(projection.last_sequence, 3);
        let mut final_event = timeline(4, DesktopTimelineEventKind::AssistantMessage);
        final_event.text = Some("exact final bytes".to_owned());
        projection.push(final_event);
        assert_eq!(projection.events.len(), 2);
        assert!(
            projection
                .events
                .iter()
                .all(|event| event.live_preview.is_none())
        );
        assert_eq!(
            projection
                .events
                .back()
                .and_then(|event| event.text.as_deref()),
            Some("exact final bytes")
        );
    }
}

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
fn healthy_idle_streams_do_not_exhaust_the_reconnect_budget() {
    assert_eq!(
        next_reconnect_attempt(7, MIN_HEALTHY_STREAM_LIFETIME, false),
        1
    );
    assert_eq!(next_reconnect_attempt(7, Duration::from_millis(1), true), 1);
    assert_eq!(
        next_reconnect_attempt(7, Duration::from_millis(1), false),
        8
    );
}

#[test]
fn foreground_terminal_waits_for_owned_terminal_tasks_to_settle() {
    let mut projection = RunProjection::new(DesktopRunStatus::Running, false);
    projection.push(terminal_timeline(1, 1, "running"));
    projection.push(timeline(2, DesktopTimelineEventKind::RunFinished));

    assert_eq!(projection.run_status, DesktopRunStatus::Finished);
    assert!(!projection.is_settled());

    projection.push(terminal_timeline(3, 2, "exited"));
    assert!(projection.is_settled());
}

#[test]
fn canonical_snapshot_keeps_terminal_follower_live_until_later_exit() {
    let mut projection = RunProjection::new(DesktopRunStatus::Running, false);
    let mut foreground_done = run_snapshot(12, Vec::new());
    foreground_done.status = DesktopRunStatus::Finished;
    foreground_done.terminal_tasks = vec![terminal_snapshot(1, "running")];

    assert!(projection.reconcile_run_snapshot(&foreground_done, "workspace-1", "session-1"));
    assert!(!projection.is_settled());

    let mut task_done = foreground_done;
    task_done.stream_sequence = 13;
    task_done.terminal_tasks = vec![terminal_snapshot(2, "exited")];
    assert!(projection.reconcile_run_snapshot(&task_done, "workspace-1", "session-1"));
    assert!(projection.is_settled());
}

#[test]
fn settled_terminal_reattach_does_not_reenter_the_live_stream_owner() {
    let mut foreground_done = run_snapshot(12, Vec::new());
    foreground_done.status = DesktopRunStatus::Finished;

    let snapshot =
        settled_terminal_reattach_snapshot(&foreground_done).expect("settled terminal snapshot");
    assert_eq!(snapshot.stream_state, DesktopRunStreamState::Terminal);
    assert!(snapshot.has_gap);
    assert!(snapshot.events.is_empty());

    foreground_done.terminal_tasks = vec![terminal_snapshot(1, "running")];
    assert!(settled_terminal_reattach_snapshot(&foreground_done).is_none());

    foreground_done.terminal_tasks = vec![terminal_snapshot(2, "exited")];
    assert!(settled_terminal_reattach_snapshot(&foreground_done).is_some());
}

#[test]
fn terminal_snapshot_projection_preserves_interrupted_status() {
    assert_eq!(
        terminal_timeline_projection(DesktopRunStatus::Interrupted),
        Some((DesktopTimelineEventKind::RunInterrupted, "interrupted"))
    );
    assert_eq!(
        terminal_timeline_projection(DesktopRunStatus::Failed),
        Some((DesktopTimelineEventKind::RunFailed, "failed"))
    );
    assert_eq!(
        terminal_timeline_projection(DesktopRunStatus::Running),
        None
    );
    assert_eq!(
        terminal_timeline_projection(DesktopRunStatus::Paused),
        Some((DesktopTimelineEventKind::RunPaused, "paused"))
    );
}

#[test]
fn paused_terminal_event_preserves_the_run_status() {
    let mut projection = RunProjection::new(DesktopRunStatus::Running, false);
    let mut task_terminal = timeline(1, DesktopTimelineEventKind::TaskRunFinished);
    task_terminal.status = Some("paused".to_owned());
    projection.push(task_terminal);

    projection.push(timeline(2, DesktopTimelineEventKind::RunCancelled));

    assert_eq!(projection.run_status, DesktopRunStatus::Paused);
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
        session_grant_unavailable_reason: Some(
            sigil_desktop::DesktopSessionGrantUnavailableReason {
                code:
                    sigil_desktop::DesktopSessionGrantUnavailableReasonCode::OperationNotGrantable,
            },
        ),
        effects: Vec::new(),
        subjects: Vec::new(),
        analysis_status: "complete".to_owned(),
        analysis_reason_codes: Vec::new(),
        analysis_reasons: Vec::new(),
        containment: Vec::new(),
        decision_reasons: Vec::new(),
        safe_summary_title: "Write a file".to_owned(),
        safe_summary_detail: "write_file operation".to_owned(),
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

#[test]
fn canonical_run_snapshot_rebuilds_clears_and_rejects_stale_pending_approvals() {
    let mut projection = RunProjection::new(DesktopRunStatus::Running, true);
    let waiting = run_snapshot(12, vec![pending_snapshot("request-1", 7)]);
    assert!(projection.reconcile_run_snapshot(&waiting, "workspace-1", "session-1"));
    assert_eq!(
        projection
            .pending_approvals
            .get("call-1")
            .and_then(|event| event.approval.as_ref())
            .map(|approval| approval.approval_request_id.as_str()),
        Some("request-1")
    );

    let resolved = run_snapshot(13, Vec::new());
    assert!(projection.reconcile_run_snapshot(&resolved, "workspace-1", "session-1"));
    assert!(projection.pending_approvals.is_empty());

    assert!(!projection.reconcile_run_snapshot(&waiting, "workspace-1", "session-1"));
    assert!(projection.pending_approvals.is_empty());
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
        run_sequence: sequence.to_string(),
        replayable: true,
        replay_id: Some(format!("event-{sequence}")),
        provisional_id: None,
        live_preview: None,
        kind,
        text: Some("detail".to_owned()),
        item_id: None,
        tool_name: None,
        status: None,
        assistant_kind: None,
        tool_input: None,
        execution_id: None,
        execution_started_at_ms: None,
        execution_updated_at_ms: None,
        display_call_id: None,
        approval: None,
        approval_request_id: None,
        tool_execution: None,
        task: None,
        terminal_task: None,
        provider_turn_recovery: None,
        route_recovery: None,
        route_transition: None,
    }
}

fn terminal_timeline(sequence: u64, generation: u64, status: &str) -> DesktopTimelineEvent {
    let mut event = timeline(sequence, DesktopTimelineEventKind::TerminalLifecycle);
    event.item_id = Some("terminal-1".to_owned());
    event.status = Some(status.to_owned());
    event.terminal_task = Some(DesktopTimelineTerminalTask {
        task_id: "terminal-1".to_owned(),
        generation,
        status: status.to_owned(),
        exit_code: (status == "exited").then_some(0),
        failure_reason: None,
        readiness: "ready".to_owned(),
        readiness_kind: Some("output_contains".to_owned()),
        readiness_failure_reason: None,
        ready_at_ms: Some(10),
        total_output_bytes: 16,
        emitted_at_ms: generation * 10,
        execution_backend: None,
        sandbox_profile: None,
    });
    event
}

fn terminal_snapshot(generation: u64, status: &str) -> sigil_desktop::DesktopTerminalLifecycleView {
    sigil_desktop::DesktopTerminalLifecycleView {
        task_id: "terminal-1".to_owned(),
        generation,
        status: match status {
            "running" => sigil_desktop::DesktopTerminalTaskStatus::Running,
            "exited" => sigil_desktop::DesktopTerminalTaskStatus::Exited { exit_code: Some(0) },
            _ => panic!("unsupported terminal test status"),
        },
        readiness: sigil_desktop::DesktopTerminalReadinessStatus::Ready {
            kind: sigil_desktop::DesktopTerminalReadinessKind::OutputContains,
            ready_at_ms: 10,
        },
        total_output_bytes: 16,
        emitted_at_ms: generation * 10,
        execution_backend: None,
        sandbox_profile: None,
    }
}

fn run_snapshot(
    stream_sequence: u64,
    pending_approvals: Vec<sigil_desktop::DesktopPendingApproval>,
) -> DesktopRunSnapshot {
    DesktopRunSnapshot {
        id: "run-1".to_owned(),
        session_id: "durable-1".to_owned(),
        status: if pending_approvals.is_empty() {
            DesktopRunStatus::Running
        } else {
            DesktopRunStatus::WaitingForApproval
        },
        permission_mode: sigil_desktop::DesktopPermissionMode::Manual,
        reasoning_effort: None,
        prompt_preview: String::new(),
        pending_approvals,
        approval_lifecycles: Vec::new(),
        terminal_tasks: Vec::new(),
        stream_sequence,
    }
}

fn pending_snapshot(
    approval_request_id: &str,
    event_sequence: u64,
) -> sigil_desktop::DesktopPendingApproval {
    sigil_desktop::DesktopPendingApproval {
        call_id: "call-1".to_owned(),
        tool_name: "bash".to_owned(),
        approval_request_id: approval_request_id.to_owned(),
        tool_call_hash: "a".repeat(64),
        policy_version: "permission-policy-v2".to_owned(),
        expires_at_ms: u64::MAX,
        session_grant_available: false,
        session_grant_unavailable_reason: Some(
            sigil_desktop::DesktopSessionGrantUnavailableReason {
                code:
                    sigil_desktop::DesktopSessionGrantUnavailableReasonCode::OperationNotGrantable,
            },
        ),
        display: sigil_desktop::DesktopPendingApprovalDisplay {
            event_sequence,
            effects: vec!["execute_workspace_code".to_owned()],
            subjects: Vec::new(),
            analysis_status: "complete".to_owned(),
            analysis_reason_codes: Vec::new(),
            analysis_reasons: Vec::new(),
            containment: vec!["network=deny".to_owned()],
            decision_reasons: vec!["explicit_ask".to_owned()],
            safe_summary_title: "Run validation".to_owned(),
            safe_summary_detail: "Runs workspace validation".to_owned(),
            operation: Some("execute_workspace_check_command".to_owned()),
            risk: Some("medium".to_owned()),
            snapshot_required: false,
        },
    }
}
