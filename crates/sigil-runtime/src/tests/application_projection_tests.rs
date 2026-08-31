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

    let state = ProjectionEventState::from_events(&entries, &BTreeSet::new());
    assert_eq!(state.run_status, "finished");
    assert!(!state.run_active);
    assert_eq!(
        state.last_notice.as_ref().map(SafeText::as_str),
        Some("checkpoint")
    );
}

#[test]
fn resumed_revision_run_started_clears_historical_waiting_projection() {
    let waiting = outbox_entry(
        1,
        PublicRunEventKind::RunAwaitingUserInput {
            request_id: "revision-question".to_owned(),
            generation: 1,
            request_hash: "sha256:revision-question".to_owned(),
        },
    );
    let resumed = outbox_entry(
        2,
        PublicRunEventKind::RunStarted {
            prompt: "plan review revision".to_owned(),
        },
    );
    let entries = vec![&waiting, &resumed];
    let revision_waiting_public_event_ids = BTreeSet::from([waiting.public_event_id.clone()]);

    let state = ProjectionEventState::from_events(&entries, &revision_waiting_public_event_ids);

    assert_eq!(state.run_status, "running");
    assert!(state.run_active);
    assert_eq!(state.plan_status, "started");
    assert!(!state.user_input_pending);
    assert!(state.user_input_binding.is_none());
}

#[test]
fn awaiting_user_input_rebuild_is_inactive_and_preserves_durable_request() {
    let request = sigil_kernel::PublicUserInputRequestV1 {
        identity: sigil_kernel::UserInputIdentityV1 {
            session_scope_id: sigil_kernel::SessionScopeId::new("projection-session")
                .expect("valid session scope"),
            root_logical_run_id: sigil_kernel::LogicalRunId::new("projection-root")
                .expect("valid root logical run"),
            source_thread_id: sigil_kernel::AgentThreadId::new("main")
                .expect("valid source thread"),
            request_id: sigil_kernel::UserInputRequestId::new("input-1").expect("valid request id"),
            generation: 7,
            source_binding_hash: format!("sha256:{}", "a".repeat(64)),
        },
        request_hash: format!("sha256:{}", "b".repeat(64)),
        source: sigil_kernel::UserInputSourceV1::Agent,
        purpose: sigil_kernel::UserInputPurposeV1::Clarification,
        prompt: "Choose the deployment target.".to_owned(),
        questions: Vec::new(),
        allowed_actions: vec![sigil_kernel::UserInputActionV1::Submit],
        requested_at_unix_ms: 10,
        status: sigil_kernel::UserInputStatusV1::Requested,
        answer_receipt: None,
        resolution: None,
    };
    let expected_binding = format!(
        "{}:{}:{}",
        request.identity.request_id.as_str(),
        request.identity.generation,
        request.request_hash
    );
    let started = outbox_entry(
        1,
        PublicRunEventKind::RunStarted {
            prompt: "run".into(),
        },
    );
    let changed = outbox_entry(
        2,
        PublicRunEventKind::UserInputChanged {
            request_id: request.identity.request_id.as_str().to_owned(),
            generation: request.identity.generation,
            request_hash: request.request_hash.clone(),
            status: request.status,
            request: Box::new(request.clone()),
        },
    );
    let awaiting = outbox_entry(
        3,
        PublicRunEventKind::RunAwaitingUserInput {
            request_id: request.identity.request_id.as_str().to_owned(),
            generation: request.identity.generation,
            request_hash: request.request_hash.clone(),
        },
    );
    let entries = vec![&started, &changed, &awaiting];

    let state = ProjectionEventState::from_events(&entries, &BTreeSet::new());

    assert_eq!(state.run_status, "awaiting-user-input");
    assert!(!state.run_active);
    assert!(state.run_binding.is_none());
    assert!(state.user_input_pending);
    assert_eq!(
        state.user_input_binding.as_deref(),
        Some(expected_binding.as_str())
    );
    assert_eq!(
        state.user_input_prompt.as_ref().map(SafeText::as_str),
        Some("Choose the deployment target.")
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

#[tokio::test]
async fn tui_outbox_ack_uses_the_exact_projection_cut_and_rejects_foreign_frontiers()
-> anyhow::Result<()> {
    fn entry(
        session_id: &str,
        run_id: &str,
        sequence: u64,
    ) -> sigil_kernel::PublicEventOutboxEntryV1 {
        let event = sigil_kernel::PublicRunEvent::new(
            session_id,
            run_id,
            sequence,
            PublicRunEventKind::Notice {
                message: format!("notice-{sequence}"),
            },
        );
        let public_event_id = format!("application-public:{session_id}:{run_id}:{sequence}");
        sigil_kernel::PublicEventOutboxEntryV1 {
            schema_version: sigil_kernel::PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
            domain_event_id: public_event_id.clone(),
            public_event_id,
            run_id: run_id.to_owned(),
            sequence,
            payload_digest: sigil_kernel::stable_event_hash(
                serde_json::to_vec(&event).expect("test public event must serialize"),
            ),
            event,
        }
    }

    let temp = tempfile::tempdir()?;
    let session_path = temp.path().join("session.jsonl");
    let store = sigil_kernel::JsonlSessionStore::new(&session_path)?;
    let session = sigil_kernel::Session::load_from_store("deepseek", "model", store)?;
    let session_id = session.session_scope_id().to_owned();
    let recorder = sigil_kernel::PublicEventOutboxRecorder::new(
        sigil_kernel::JsonlSessionStore::new(&session_path)?,
    );
    recorder.append_outbox(&entry(&session_id, "run-ack", 1))?;
    recorder.append_outbox(&entry(&session_id, "run-ack", 2))?;
    let records = sigil_kernel::JsonlSessionStore::read_event_records(&session_path)?;
    let outbox_stream_sequences = records
        .iter()
        .filter(|record| {
            record.stored_event().event_kind()
                == Some(sigil_kernel::DurableEventType::PublicEventOutbox)
        })
        .map(sigil_kernel::SessionStreamRecord::stream_sequence)
        .collect::<Vec<_>>();
    assert_eq!(outbox_stream_sequences.len(), 2);
    let binding = RuntimeSessionProjectionBinding::new(
        temp.path().join("config.json"),
        temp.path().to_path_buf(),
        session_path.clone(),
        session_id.clone(),
        sigil_application::ApplicationInstanceId::new("projection-ack-test")?,
        sigil_application::AuthenticatedSubject::new("local-user")?,
        None,
        1,
        1,
        1,
        1,
    )?;
    let frontier = ApplicationFrontier {
        schema_version: sigil_application::APPLICATION_CONTRACT_SCHEMA_VERSION,
        scope: binding.scope.clone(),
        writer_generation: 1,
        stream_generation: 1,
        through_sequence: outbox_stream_sequences[0],
        durable_cursor: format!("session-stream:{}", outbox_stream_sequences[0]),
    };

    assert_eq!(
        binding
            .acknowledge_tui_public_outbox_through(&frontier)
            .await?,
        1
    );
    let after_first = sigil_kernel::PublicEventOutboxProjectionV1::from_records(
        &sigil_kernel::JsonlSessionStore::read_event_records(&session_path)?,
    )?;
    assert_eq!(after_first.pending_for_adapter("tui").len(), 1);
    assert_eq!(
        after_first.pending_for_adapter("tui")[0].sequence,
        2,
        "the later outbox entry is beyond the committed projection cut"
    );

    let mut foreign_scope = frontier.clone();
    foreign_scope.scope.session = Some(sigil_application::SessionScopeId::new("foreign")?);
    assert!(matches!(
        binding
            .acknowledge_tui_public_outbox_through(&foreign_scope)
            .await,
        Err(ApplicationError::ScopeMismatch)
    ));
    let mut invalid_cursor = frontier.clone();
    invalid_cursor.durable_cursor = "session-stream:wrong".to_owned();
    assert!(matches!(
        binding
            .acknowledge_tui_public_outbox_through(&invalid_cursor)
            .await,
        Err(ApplicationError::ResetRequired)
    ));

    let second_frontier = ApplicationFrontier {
        through_sequence: outbox_stream_sequences[1],
        durable_cursor: format!("session-stream:{}", outbox_stream_sequences[1]),
        ..frontier
    };
    assert_eq!(
        binding
            .acknowledge_tui_public_outbox_through(&second_frontier)
            .await?,
        1
    );
    let complete = sigil_kernel::PublicEventOutboxProjectionV1::from_records(
        &sigil_kernel::JsonlSessionStore::read_event_records(&session_path)?,
    )?;
    assert!(complete.pending_for_adapter("tui").is_empty());
    Ok(())
}
