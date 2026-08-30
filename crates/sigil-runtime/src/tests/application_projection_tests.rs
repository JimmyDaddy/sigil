use super::*;

fn terminal_record(
    sequence: u64,
    generation: u64,
    status: sigil_kernel::TerminalTaskStatus,
) -> sigil_kernel::SessionStreamRecord {
    let task = sigil_kernel::TerminalTaskEntry {
        schema_version: sigil_kernel::terminal_task::TERMINAL_TASK_SCHEMA_VERSION,
        handle: sigil_kernel::TerminalTaskHandle {
            task_id: sigil_kernel::TerminalTaskId::new("terminal-projection-test")
                .expect("valid task id"),
            command_sha256: "0".repeat(64),
            cwd_label: ".".to_owned(),
            shell_label: "zsh".to_owned(),
            shell_sha256: "1".repeat(64),
            log_ref: "terminal-log:terminal-projection-test".to_owned(),
            created_at_ms: 1,
            execution_backend: None,
            execution_backend_capabilities: None,
            enforcement_backend: None,
            enforcement_backend_capabilities: None,
            sandbox_profile: None,
        },
        generation,
        status,
        readiness: sigil_kernel::TerminalReadinessStatus::None,
        output_preview: None,
        output_hash: None,
        output_truncated: false,
        output_total_bytes: generation * 10,
        output_limit_bytes: None,
        output_termination_reason: None,
        cleanup: None,
        updated_at_ms: generation,
    };
    let event = sigil_kernel::StoredEvent::new(
        sigil_kernel::DurableEventType::SessionEntryRecorded,
        sigil_kernel::EventClass::NonCritical,
        format!("event-{sequence}"),
        "session-1".to_owned(),
        sequence,
        serde_json::json!({
            "session_log_entry": sigil_kernel::SessionLogEntry::Control(
                sigil_kernel::ControlEntry::TerminalTask(task),
            ),
        }),
    )
    .expect("valid stored terminal event");
    sigil_kernel::SessionStreamRecord::Stored(event)
}

fn outbox_entry(
    sequence: u64,
    event: PublicRunEventKind,
) -> sigil_kernel::PublicEventOutboxEntryV1 {
    let event = sigil_kernel::PublicRunEvent::new("session-1", "run-1", sequence, event);
    sigil_kernel::PublicEventOutboxEntryV1 {
        schema_version: sigil_kernel::PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
        public_event_id: format!("event-{sequence}"),
        domain_event_id: format!("domain-{sequence}"),
        run_id: event.run_id.clone(),
        sequence,
        payload_digest: sigil_kernel::stable_event_hash(
            serde_json::to_vec(&event).expect("public event encodes"),
        ),
        event,
    }
}

#[test]
fn generated_before_cursor_is_strictly_positive() {
    let cursor = StablePageCursor::new("before:4").expect("cursor");
    assert_eq!(parse_before_cursor(&cursor).expect("parse"), 4);
    let invalid = StablePageCursor::new("before:0").expect("cursor");
    assert!(parse_before_cursor(&invalid).is_err());
}

#[test]
fn projection_state_rebuilds_from_delivered_history() {
    let started = outbox_entry(
        1,
        PublicRunEventKind::RunStarted {
            prompt: "run".into(),
        },
    );
    let notice = outbox_entry(
        2,
        PublicRunEventKind::Notice {
            message: "checkpoint".into(),
        },
    );
    let finished = outbox_entry(
        3,
        PublicRunEventKind::RunFinished {
            final_text: "done".into(),
        },
    );
    let mut entries = vec![&finished, &started, &notice];
    entries.sort_by_key(|entry| entry.sequence);

    let state = ProjectionEventState::from_events(&entries);
    assert_eq!(state.run_status, "finished");
    assert!(!state.run_active);
    assert_eq!(
        state.last_notice.as_ref().map(SafeText::as_str),
        Some("checkpoint")
    );
}

#[test]
fn terminal_surface_projection_replays_latest_bounded_task_state() {
    let records = vec![
        terminal_record(1, 1, sigil_kernel::TerminalTaskStatus::Starting),
        terminal_record(2, 2, sigil_kernel::TerminalTaskStatus::Running),
    ];

    let projection = terminal_surface_projection(&records).expect("terminal projection");

    assert_eq!(projection.active_task_count, 1);
    assert_eq!(
        projection.latest_task_id.as_ref().map(SafeText::as_str),
        Some("terminal-projection-test")
    );
    assert_eq!(projection.tasks.len(), 1);
    assert_eq!(projection.tasks[0].generation, 2);
    assert_eq!(projection.tasks[0].status.as_str(), "running");
    assert_eq!(projection.tasks[0].output_total_bytes, 20);
    assert!(projection.tasks[0].output_hash.is_none());
}
