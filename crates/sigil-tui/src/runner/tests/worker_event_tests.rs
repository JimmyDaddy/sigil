use super::*;

#[test]
fn supervisor_wakes_coalesce_until_the_queued_token_is_consumed() {
    let (event_tx, event_rx) = mpsc::channel();
    let coalescer = WorkerWakeCoalescer::new(event_tx, Some("session-a".to_owned()));

    for _ in 0..32 {
        coalescer.notify_supervisor(sigil_runtime::AgentSupervisorChange::ProviderRouteDiagnostics);
        coalescer.notify_supervisor(sigil_runtime::AgentSupervisorChange::TaskCompletionProgress);
    }

    let event = event_rx.recv().expect("one coalesced wake should arrive");
    assert!(event_rx.try_recv().is_err());
    let mut readiness = WorkerReadiness::new();
    readiness.ingest(event);
    assert!(readiness.take_wake_readiness(&coalescer).any);

    coalescer.notify_supervisor(sigil_runtime::AgentSupervisorChange::TaskCompletionProgress);
    assert!(matches!(event_rx.recv(), Ok(WorkerEvent::Wake(_))));
}

#[test]
fn session_switch_clears_stale_projection_wake_payload() {
    let (event_tx, event_rx) = mpsc::channel();
    let coalescer = WorkerWakeCoalescer::new(event_tx, Some("session-a".to_owned()));
    let binding = coalescer
        .current_projection_binding()
        .expect("session binding should exist");
    coalescer.notify_session_projection(
        &binding,
        false,
        &[ActiveProjectionFamily::Queue].into_iter().collect(),
    );
    let stale_token = event_rx.recv().expect("wake token should be queued");

    coalescer.switch_session_scope("session-b".to_owned());
    let mut readiness = WorkerReadiness::new();
    readiness.ingest(stale_token);

    assert!(!readiness.take_wake_readiness(&coalescer).any);
    coalescer.notify_session_projection(
        &binding,
        false,
        &[ActiveProjectionFamily::Queue].into_iter().collect(),
    );
    assert!(event_rx.try_recv().is_err());
}

#[test]
fn same_scope_rebind_fences_stale_generation_and_coalesces_current_families() {
    let (event_tx, event_rx) = mpsc::channel();
    let coalescer = WorkerWakeCoalescer::new(event_tx, Some("session-a".to_owned()));
    let stale_binding = coalescer
        .current_projection_binding()
        .expect("initial binding should exist");
    let current_binding = coalescer.switch_session_scope("session-a".to_owned());
    assert!(current_binding.observer_id > stale_binding.observer_id);

    coalescer.notify_session_projection(
        &stale_binding,
        false,
        &[ActiveProjectionFamily::Queue].into_iter().collect(),
    );
    assert!(event_rx.try_recv().is_err());

    for _ in 0..32 {
        coalescer.notify_session_projection(
            &current_binding,
            false,
            &[ActiveProjectionFamily::Queue].into_iter().collect(),
        );
        coalescer.notify_session_projection(
            &current_binding,
            false,
            &[ActiveProjectionFamily::Task].into_iter().collect(),
        );
    }
    let event = event_rx
        .recv()
        .expect("current generation should wake once");
    assert!(event_rx.try_recv().is_err());
    let mut readiness = WorkerReadiness::new();
    readiness.ingest(event);
    let wake = readiness.take_wake_readiness(&coalescer);
    assert_eq!(
        wake.projection_families,
        [ActiveProjectionFamily::Queue, ActiveProjectionFamily::Task]
            .into_iter()
            .collect()
    );
}

#[test]
fn usage_projection_wake_does_not_dirty_task_guidance_or_queue() {
    let (event_tx, event_rx) = mpsc::channel();
    let coalescer = WorkerWakeCoalescer::new(event_tx, Some("session-a".to_owned()));
    let binding = coalescer
        .current_projection_binding()
        .expect("session binding should exist");
    coalescer.notify_session_projection(
        &binding,
        false,
        &[ActiveProjectionFamily::Usage].into_iter().collect(),
    );

    let mut readiness = WorkerReadiness::new();
    readiness.ingest(event_rx.recv().expect("usage wake should arrive"));
    let wake = readiness.take_wake_readiness(&coalescer);
    assert!(wake.any);
    assert!(!wake.task_guidance_dirty());
    assert!(!wake.conversation_queue_dirty());
}

#[test]
fn background_completion_wakes_are_deduplicated_by_thread() {
    let (event_tx, event_rx) = mpsc::channel();
    let coalescer = WorkerWakeCoalescer::new(event_tx, Some("session-a".to_owned()));
    let thread_id = AgentThreadId::new("agent-1").expect("valid thread id");

    coalescer.notify_background_agent(&thread_id);
    coalescer.notify_background_agent(&thread_id);

    let event = event_rx.recv().expect("one background wake should arrive");
    assert!(event_rx.try_recv().is_err());
    let mut readiness = WorkerReadiness::new();
    readiness.ingest(event);
    assert!(readiness.take_wake_readiness(&coalescer).any);
}

#[test]
fn mcp_runtime_progress_is_latest_only_and_publishes_one_token() {
    let (event_tx, event_rx) = mpsc::channel();
    let sender = WorkerMcpRuntimeEventSender::new(event_tx);
    for ordinal in 0..1_000 {
        sender
            .send(McpRuntimeEvent::Progress(
                sigil_runtime::McpProgressNotification {
                    server_name: "filesystem".to_owned(),
                    progress_token: "scan".to_owned(),
                    progress: Some(f64::from(ordinal)),
                    total: Some(1_000.0),
                    message: Some(format!("step-{ordinal}")),
                },
            ))
            .expect("progress should enter the bounded slot");
    }

    assert_eq!(sender.pending_len(), 1);
    let event = event_rx
        .recv()
        .expect("one MCP readiness token should arrive");
    assert!(event_rx.try_recv().is_err());
    let mut readiness = WorkerReadiness::new();
    readiness.ingest(event);
    assert!(matches!(
        readiness.mcp_runtime_events.pop_front(),
        Some(McpRuntimeEvent::Progress(notification))
            if notification.progress == Some(999.0)
                && notification.message.as_deref() == Some("step-999")
    ));
    assert!(readiness.mcp_runtime_events.is_empty());
}

#[test]
fn mcp_runtime_slot_is_bounded_and_coalesces_list_changes() {
    let (event_tx, event_rx) = mpsc::channel();
    let sender = WorkerMcpRuntimeEventSender::new(event_tx);
    for _ in 0..32 {
        sender
            .send(McpRuntimeEvent::ListChanged(
                sigil_runtime::McpListChangedNotification {
                    server_name: "filesystem".to_owned(),
                    kind: sigil_runtime::McpListChangedKind::Tools,
                },
            ))
            .expect("list-change should coalesce");
    }
    for ordinal in 0..(MAX_PENDING_MCP_RUNTIME_EVENTS * 4) {
        sender
            .send(McpRuntimeEvent::Progress(
                sigil_runtime::McpProgressNotification {
                    server_name: "filesystem".to_owned(),
                    progress_token: format!("scan-{ordinal}"),
                    progress: Some(ordinal as f64),
                    total: None,
                    message: None,
                },
            ))
            .expect("bounded progress should remain accepted");
    }

    assert_eq!(sender.pending_len(), MAX_PENDING_MCP_RUNTIME_EVENTS);
    let event = event_rx
        .recv()
        .expect("one bounded MCP token should arrive");
    assert!(event_rx.try_recv().is_err());
    let mut readiness = WorkerReadiness::new();
    readiness.ingest(event);
    assert_eq!(
        readiness.mcp_runtime_events.len(),
        MAX_PENDING_MCP_RUNTIME_EVENTS
    );
    assert_eq!(
        readiness
            .mcp_runtime_events
            .iter()
            .filter(|event| matches!(event, McpRuntimeEvent::ListChanged(_)))
            .count(),
        1
    );
}

#[test]
fn mcp_runtime_list_change_overflow_requests_conservative_resync() {
    let (event_tx, event_rx) = mpsc::channel();
    let sender = WorkerMcpRuntimeEventSender::new(event_tx);
    for ordinal in 0..=MAX_PENDING_MCP_RUNTIME_EVENTS {
        sender
            .send(McpRuntimeEvent::ListChanged(
                sigil_runtime::McpListChangedNotification {
                    server_name: format!("server-{ordinal}"),
                    kind: sigil_runtime::McpListChangedKind::Tools,
                },
            ))
            .expect("dirty-server signal should enter the bounded slot");
    }

    assert_eq!(sender.pending_len(), MAX_PENDING_MCP_RUNTIME_EVENTS);
    let event = event_rx
        .recv()
        .expect("one MCP readiness token should arrive");
    assert!(event_rx.try_recv().is_err());
    let mut readiness = WorkerReadiness::new();
    readiness.ingest(event);

    assert_eq!(
        readiness.mcp_runtime_events.len(),
        MAX_PENDING_MCP_RUNTIME_EVENTS
    );
    assert_eq!(
        readiness.mcp_resync_servers.len(),
        MAX_PENDING_MCP_RUNTIME_EVENTS + 1
    );
    assert!(readiness.mcp_resync_servers.contains("server-0"));
    assert!(
        !readiness
            .mcp_resync_servers
            .contains("never-activated-lazy"),
        "overflow recovery must not activate declarations that never emitted a runtime event"
    );
}

#[test]
fn active_projection_observer_ignores_unrelated_append_and_wakes_once_for_queue_append()
-> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let store = sigil_kernel::JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = sigil_kernel::Session::new("test", "model").with_store(store);
    session
        .active_projection_snapshot()?
        .expect("store-backed session should seed its active projection");
    let (event_tx, event_rx) = mpsc::channel();
    let coalescer = WorkerWakeCoalescer::new(event_tx, Some(session.session_scope_id().to_owned()));
    let binding = coalescer
        .current_projection_binding()
        .expect("durable session binding should exist");
    let observer: Arc<dyn ActiveProjectionObserver> = Arc::new(
        WorkerActiveProjectionObserver::new(coalescer.clone(), binding),
    );
    let _subscription = session
        .register_active_projection_observer(observer)?
        .expect("store-backed session should register an observer");

    session.append_control(sigil_kernel::ControlEntry::Note {
        kind: "unrelated".to_owned(),
        data: serde_json::Value::Null,
    })?;
    assert!(event_rx.try_recv().is_err());

    session.append_control(sigil_kernel::ControlEntry::ConversationInputQueued(
        sigil_kernel::ConversationInputQueuedEntry {
            queue_id: sigil_kernel::ConversationInputQueueId::new("queue_1")?,
            target: sigil_kernel::ConversationInputTarget::MainThread,
            kind: sigil_kernel::ConversationInputKind::Chat,
            prompt_hash: "safe:sha256:test".to_owned(),
            prompt: "hello".to_owned(),
            reasoning_effort: Some(sigil_kernel::ReasoningEffort::Max),
            created_at_ms: Some(1),
        },
    ))?;
    let event = event_rx.recv()?;
    assert!(event_rx.try_recv().is_err());
    let mut readiness = WorkerReadiness::new();
    readiness.ingest(event);
    let wake = readiness.take_wake_readiness(&coalescer);
    assert!(wake.any);
    assert!(wake.task_guidance_dirty());
    assert!(wake.conversation_queue_dirty());
    Ok(())
}
