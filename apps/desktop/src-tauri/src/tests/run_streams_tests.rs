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
        Some((DesktopTimelineEventKind::RunFailed, "interrupted"))
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
        Some((DesktopTimelineEventKind::RunCancelled, "paused"))
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
        kind,
        text: Some("detail".to_owned()),
        item_id: None,
        tool_name: None,
        status: None,
        assistant_kind: None,
        tool_input: None,
        approval: None,
        approval_request_id: None,
        tool_execution: None,
        task: None,
        terminal_task: None,
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
