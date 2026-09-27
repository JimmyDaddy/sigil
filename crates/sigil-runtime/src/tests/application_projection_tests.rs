use super::*;

struct ProjectionClientTestPort {
    binding: RuntimeSessionProjectionBinding,
    acknowledgements: std::sync::Mutex<Vec<sigil_application::ProjectionDeliveryAck>>,
}

impl sigil_application::ApplicationPort for ProjectionClientTestPort {
    fn delivery_batch(
        &self,
        request: sigil_application::DurableDeliveryRequest,
    ) -> BoxFuture<'static, Result<sigil_application::DurableDeliveryBatch, ApplicationError>> {
        crate::RuntimeApplicationProjectionSource::delivery_batch(&self.binding, request)
    }

    fn open_projection(
        &self,
        request: OpenProjectionRequest,
    ) -> BoxFuture<'static, Result<ProjectionSnapshot, ApplicationError>> {
        crate::RuntimeApplicationProjectionSource::open_projection(&self.binding, request)
    }

    fn page(
        &self,
        request: ProjectionPageRequest,
    ) -> BoxFuture<'static, Result<ProjectionPage, ApplicationError>> {
        crate::RuntimeApplicationProjectionSource::page(&self.binding, request)
    }

    fn cancel_page(
        &self,
        _request: sigil_application::PageRequestId,
    ) -> BoxFuture<'static, sigil_application::PageCancellationReceipt> {
        panic!("projection refresh must not cancel pages")
    }

    fn acknowledge(
        &self,
        acknowledgement: sigil_application::ProjectionDeliveryAck,
    ) -> BoxFuture<'static, Result<(), ApplicationError>> {
        self.acknowledgements
            .lock()
            .expect("test ACK log")
            .push(acknowledgement);
        Box::pin(async { Ok(()) })
    }

    fn execute(
        &self,
        _request: sigil_application::ApplicationCommandRequest,
    ) -> BoxFuture<'static, Result<sigil_application::ApplicationCommandReceipt, ApplicationError>>
    {
        panic!("projection refresh must not execute commands")
    }
}

#[tokio::test]
async fn application_client_refresh_resumes_from_committed_cut_after_durable_append()
-> anyhow::Result<()> {
    use sigil_kernel::EventHandler;
    use std::sync::Arc;

    let fixture = tempfile::tempdir()?;
    let mut config = sigil_kernel::RootConfig::parse_persisted(
        r#"config_version = 2
[composition]
profile = "core"
[agent]
connection = "fixture"
model = "fixture-model"
[connections.fixture]
label = "Projection fixture"
provider = "custom"
protocol = "chat_completions"
base_url = "http://127.0.0.1:1"
credential = { source = "none" }
model_context_windows = { fixture-model = 32768 }
"#,
    )?;
    config.workspace.root = fixture.path().display().to_string();
    config.storage.state_root =
        sigil_kernel::StorageRoot::Path(fixture.path().join("state").display().to_string());
    config.storage.cache_root =
        sigil_kernel::StorageRoot::Path(fixture.path().join("cache").display().to_string());
    config.session.log_dir = Some(fixture.path().join("sessions").display().to_string());
    let config_path = fixture.path().join("sigil.toml");
    config.save(&config_path)?;
    let (provider, route) = crate::provider_connections::resolve_default_model_route(&config)?;
    let session_path = fixture.path().join("sessions/refresh.jsonl");
    let store = JsonlSessionStore::new(&session_path)?;
    let mut session =
        sigil_kernel::Session::new_with_route(provider, route).with_store(store.clone());
    session.ensure_identity_entry()?;
    crate::bind_session_composition(&mut session, &config)?;
    let binding = RuntimeSessionProjectionBinding::new(
        config_path,
        fixture.path().to_path_buf(),
        session_path,
        session.session_scope_id().to_owned(),
        ApplicationInstanceId::new("projection-refresh-test")?,
        sigil_application::AuthenticatedSubject::new("local-user")?,
        None,
        1,
        1,
        1,
        1,
    )?
    .with_owner(crate::RuntimeSessionProjectionOwner::from_store(&store));
    let port = Arc::new(ProjectionClientTestPort {
        binding: binding.clone(),
        acknowledgements: Default::default(),
    });
    let client = sigil_application::ApplicationClient::new(
        port.clone(),
        binding.scope.clone(),
        1,
        1,
        sigil_application::HostConnectionInstanceId::new("projection-refresh-connection")?,
    )?;
    let initial = client.refresh().await?;
    assert_eq!(initial.run.status.as_str(), "idle");

    let mut recorder = crate::ApplicationRunEventRecorder::start(
        &session,
        "projection-refresh-run",
        "inspect the fixture",
    )?;
    recorder.handle(sigil_kernel::RunEvent::Notice(
        "first durable notice".to_owned(),
    ))?;
    let snapshot = binding.build_snapshot(Some(initial.frontier.clone()))?;
    assert!(snapshot.envelope.cut.through_sequence > initial.frontier.through_sequence);
    assert_eq!(snapshot.envelope.projection.run.status.as_str(), "running");
    assert!(matches!(
        snapshot.feed.as_slice(),
        [ProjectionFeedItem::CurrentState]
    ));
    let running = client.refresh().await?;
    assert_eq!(running.run.status.as_str(), "running");
    assert!(client.take_applied_delivery_event_ids()?.is_empty());
    assert!(
        port.acknowledgements
            .lock()
            .expect("test ACK log")
            .is_empty()
    );

    // State is immediately current even while the independent delivery channel has a backlog.
    for index in 0..300 {
        recorder.handle(sigil_kernel::RunEvent::Notice(format!("backlog-{index}")))?;
    }
    let latest = client.refresh().await?;
    assert_eq!(
        latest.attention.last_notice.as_ref().map(SafeText::as_str),
        Some("backlog-299")
    );
    let first = client.refresh_delivery().await?;
    assert!(first.has_more);
    assert_eq!(
        first.notices.first().map(SafeText::as_str),
        Some("first durable notice")
    );
    let first_ids = client.take_applied_delivery_event_ids()?;
    assert_eq!(first_ids.len(), 256);
    assert_eq!(client.current_frontier()?, Some(latest.frontier.clone()));
    let second = client.refresh_delivery().await?;
    assert!(!second.has_more);
    assert_eq!(
        second.notices.last().map(SafeText::as_str),
        Some("backlog-299")
    );
    let second_ids = client.take_applied_delivery_event_ids()?;
    assert!(!second_ids.is_empty());
    assert!(second_ids.iter().all(|id| !first_ids.contains(id)));
    assert_eq!(client.refresh().await?, latest);
    assert!(client.refresh_delivery().await?.notices.is_empty());
    assert!(client.take_applied_delivery_event_ids()?.is_empty());
    assert!(
        port.acknowledgements
            .lock()
            .expect("test ACK log")
            .is_empty()
    );
    Ok(())
}

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
    let session = sigil_kernel::Session::load_from_store("deepseek", "model", store.clone())?;
    let session_id = session.session_scope_id().to_owned();
    let recorder = sigil_kernel::PublicEventOutboxRecorder::new(store.clone());
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
    )?
    .with_owner(crate::RuntimeSessionProjectionOwner::from_store(&store));
    let frontier = ApplicationFrontier {
        schema_version: sigil_application::APPLICATION_CONTRACT_SCHEMA_VERSION,
        scope: binding.scope.clone(),
        writer_generation: 1,
        stream_generation: 1,
        through_sequence: outbox_stream_sequences[0],
        durable_cursor: format!("session-stream:{}", outbox_stream_sequences[0]),
    };

    let mut detached = binding.clone();
    detached.owner = Arc::new(Mutex::new(None));
    let before_detached_ack = std::fs::read(&session_path)?;
    assert!(matches!(
        detached
            .acknowledge_tui_public_events(
                &[entry(&session_id, "run-ack", 1).public_event_id],
                &frontier
            )
            .await,
        Err(ApplicationError::Unavailable)
    ));
    assert_eq!(std::fs::read(&session_path)?, before_detached_ack);

    assert_eq!(
        binding
            .acknowledge_tui_public_events(
                &[entry(&session_id, "run-ack", 1).public_event_id],
                &frontier
            )
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
            .acknowledge_tui_public_events(&[], &foreign_scope)
            .await,
        Err(ApplicationError::ScopeMismatch)
    ));
    let mut invalid_cursor = frontier.clone();
    invalid_cursor.durable_cursor = "session-stream:wrong".to_owned();
    assert!(matches!(
        binding
            .acknowledge_tui_public_events(&[], &invalid_cursor)
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
            .acknowledge_tui_public_events(
                &[entry(&session_id, "run-ack", 2).public_event_id],
                &second_frontier
            )
            .await?,
        1
    );
    let complete = sigil_kernel::PublicEventOutboxProjectionV1::from_records(
        &sigil_kernel::JsonlSessionStore::read_event_records(&session_path)?,
    )?;
    assert!(complete.pending_for_adapter("tui").is_empty());
    assert_eq!(
        binding
            .acknowledge_tui_public_events(
                &[entry(&session_id, "run-ack", 2).public_event_id],
                &second_frontier
            )
            .await?,
        0
    );
    assert!(matches!(
        detached
            .acknowledge_tui_public_events(&[], &second_frontier)
            .await,
        Err(ApplicationError::Unavailable)
    ));
    Ok(())
}

fn owned_projection_fixture() -> anyhow::Result<(
    tempfile::TempDir,
    sigil_kernel::Session,
    JsonlSessionStore,
    RuntimeSessionProjectionBinding,
)> {
    let fixture = tempfile::tempdir()?;
    let mut config = sigil_kernel::RootConfig::parse_persisted(
        r#"config_version = 2
[composition]
profile = "core"
[agent]
connection = "fixture"
model = "fixture-model"
[connections.fixture]
label = "Projection owner fixture"
provider = "custom"
protocol = "chat_completions"
base_url = "http://127.0.0.1:1"
credential = { source = "none" }
model_context_windows = { fixture-model = 32768 }
"#,
    )?;
    config.workspace.root = fixture.path().display().to_string();
    config.storage.state_root =
        sigil_kernel::StorageRoot::Path(fixture.path().join("state").display().to_string());
    config.storage.cache_root =
        sigil_kernel::StorageRoot::Path(fixture.path().join("cache").display().to_string());
    config.session.log_dir = Some(fixture.path().join("sessions").display().to_string());
    let config_path = fixture.path().join("sigil.toml");
    config.save(&config_path)?;
    let (provider, route) = crate::provider_connections::resolve_default_model_route(&config)?;
    let store = JsonlSessionStore::new(fixture.path().join("sessions/owned.jsonl"))?;
    let mut session =
        sigil_kernel::Session::new_with_route(provider, route).with_store(store.clone());
    session.ensure_identity_entry()?;
    crate::bind_session_composition(&mut session, &config)?;
    let binding = RuntimeSessionProjectionBinding::new(
        config_path,
        fixture.path().to_path_buf(),
        store.path().to_path_buf(),
        session.session_scope_id().to_owned(),
        ApplicationInstanceId::new("owned-projection-test")?,
        sigil_application::AuthenticatedSubject::new("local-user")?,
        None,
        1,
        1,
        1,
        1,
    )?
    .with_owner(crate::RuntimeSessionProjectionOwner::from_store(&store));
    Ok((fixture, session, store, binding))
}

#[test]
fn owned_projection_reuses_one_strict_cut_after_the_writer_advances() -> anyhow::Result<()> {
    let (_fixture, mut session, store, binding) = owned_projection_fixture()?;
    session.append_user_message(sigil_kernel::ModelMessage::user("before snapshot"))?;
    let snapshot = binding.build_snapshot(None)?.envelope;
    let owner = binding.owner().expect("owned fixture");
    let metrics = owner.metrics()?;
    let attempts = store.active_projection_metrics().writer_lock_attempt_total;
    assert_eq!(snapshot.projection.conversation.message_count, 1);
    assert_eq!(
        snapshot
            .projection
            .conversation
            .latest_message
            .as_ref()
            .map(SafeText::as_str),
        Some("before snapshot")
    );
    assert_eq!(binding.build_snapshot(None)?.envelope, snapshot);
    assert_eq!(
        store.active_projection_metrics().writer_lock_attempt_total,
        attempts + 1
    );
    assert_eq!(owner.metrics()?.bytes_read, metrics.bytes_read);
    assert_eq!(owner.metrics()?.records_applied, metrics.records_applied);
    session.append_user_message(sigil_kernel::ModelMessage::user("after snapshot"))?;
    session.append_control(ControlEntry::UsageSnapshot(sigil_kernel::UsageStats {
        prompt_tokens: 42,
        ..Default::default()
    }))?;
    let current = binding.build_snapshot(Some(snapshot.cut.clone()))?.envelope;
    assert!(current.cut.through_sequence > snapshot.cut.through_sequence);
    assert_eq!(current.projection.conversation.message_count, 2);
    assert_eq!(snapshot.projection.conversation.message_count, 1);
    assert_eq!(owner.metrics()?.full_prefix_scan_count, 1);
    assert_eq!(
        owner.metrics()?.records_applied,
        metrics.records_applied + 2
    );
    Ok(())
}

#[test]
fn owned_projection_pages_keep_a_fixed_cut_after_tail_growth() -> anyhow::Result<()> {
    let (fixture, mut session, store, mut binding) = owned_projection_fixture()?;
    session.append_user_message(sigil_kernel::ModelMessage::user("first message"))?;
    session.append_user_message(sigil_kernel::ModelMessage::user("second message"))?;
    // An owned page must never acquire an observer by its retained historical path.
    binding.session_path = fixture.path().join("unused-observer.jsonl");
    let snapshot = binding.build_snapshot(None)?.envelope;
    let request = ProjectionPageRequest {
        request_id: sigil_application::PageRequestId::new("owned-page")?,
        scope: binding.scope.clone(),
        source_generation: binding.source_generation,
        at_frontier: snapshot.cut,
        query: sigil_application::PageQueryFingerprint::new("transcript")?,
        anchor: sigil_application::PageAnchor {
            item_id: None,
            intra_item_row: 0,
            cursor: Some(StablePageCursor::new("before:2")?),
        },
        direction: PageDirection::Older,
        limit: std::num::NonZeroUsize::new(1).expect("positive page size"),
        width_bucket: 80,
    };
    let attempts = store.active_projection_metrics().writer_lock_attempt_total;
    let page = binding.page_sync(request.clone())?;
    assert_eq!(
        store.active_projection_metrics().writer_lock_attempt_total,
        attempts + 2
    );
    assert_eq!(page.at_frontier, request.at_frontier);
    assert_eq!(page.total, 2);
    assert_eq!(page.items.len(), 1);
    assert_eq!(
        page.items[0].text.as_ref().map(SafeText::as_str),
        Some("first message")
    );
    assert!(!binding.session_path.exists());

    session.append_user_message(sigil_kernel::ModelMessage::user("later message"))?;
    assert_eq!(binding.page_sync(request.clone())?, page);
    let mut forged = request;
    forged.at_frontier.through_sequence += 1000;
    forged.at_frontier.durable_cursor =
        format!("session-stream:{}", forged.at_frontier.through_sequence);
    assert!(matches!(
        binding.page_sync(forged),
        Err(ApplicationError::ResetRequired)
    ));
    Ok(())
}

#[test]
fn owned_application_projection_reads_through_its_actual_session_owner() -> anyhow::Result<()> {
    let (_fixture, _session, store, binding) = owned_projection_fixture()?;
    let before = std::fs::read(store.path())?;
    let attempts = store.active_projection_metrics().writer_lock_attempt_total;
    let snapshot = binding.build_snapshot(None)?.envelope;
    let records = JsonlSessionStore::read_event_records(store.path())?;
    assert_eq!(
        store.active_projection_metrics().writer_lock_attempt_total,
        attempts + 2
    );
    assert_eq!(
        snapshot.cut.through_sequence,
        records
            .last()
            .expect("initialized session")
            .stream_sequence()
    );
    assert_eq!(snapshot.cut.scope, binding.scope);
    assert_eq!(std::fs::read(store.path())?, before);
    Ok(())
}

#[test]
fn owned_projection_rejects_foreign_or_corrupt_owner_without_path_fallback() -> anyhow::Result<()> {
    let (fixture, _session, store, binding) = owned_projection_fixture()?;
    let foreign = JsonlSessionStore::new(fixture.path().join("foreign.jsonl"))?;
    let mut foreign_session =
        sigil_kernel::Session::new("foreign-provider", "foreign-model").with_store(foreign.clone());
    foreign_session.ensure_identity_entry()?;
    let foreign_binding = binding
        .clone()
        .with_owner(crate::RuntimeSessionProjectionOwner::from_store(&foreign));
    assert!(matches!(
        foreign_binding.build_snapshot(None),
        Err(ApplicationError::ScopeMismatch)
    ));

    let original = std::fs::read(store.path())?;
    let mut corrupt = original.clone();
    corrupt.extend_from_slice(b"{\"partial\":");
    std::fs::write(store.path(), &corrupt)?;
    assert!(matches!(
        binding.build_snapshot(None),
        Err(ApplicationError::CorruptProjection(_))
    ));
    let scans = binding
        .owner()
        .expect("owner")
        .metrics()?
        .full_prefix_scan_count;
    assert!(matches!(
        binding.build_snapshot(None),
        Err(ApplicationError::CorruptProjection(_))
    ));
    assert_eq!(
        binding
            .owner()
            .expect("owner")
            .metrics()?
            .full_prefix_scan_count,
        scans,
        "permanent codec corruption cannot restart a full scan on every refresh"
    );
    assert_eq!(std::fs::read(store.path())?, corrupt);
    Ok(())
}

fn query_page_request(
    binding: &RuntimeSessionProjectionBinding,
    frontier: ApplicationFrontier,
    limit: usize,
) -> anyhow::Result<ProjectionPageRequest> {
    Ok(ProjectionPageRequest {
        request_id: sigil_application::PageRequestId::new("qualification-page")?,
        scope: binding.scope.clone(),
        source_generation: binding.source_generation,
        at_frontier: frontier,
        query: sigil_application::PageQueryFingerprint::new("transcript")?,
        anchor: sigil_application::PageAnchor {
            item_id: None,
            intra_item_row: 0,
            cursor: None,
        },
        direction: PageDirection::Older,
        limit: std::num::NonZeroUsize::new(limit).expect("positive page size"),
        width_bucket: 80,
    })
}

// Build a valid offline log through the public stored-event codec. Qualification measures the
// real attached runtime reader and reducers, independently of writer fsync fixture setup cost.
fn append_large_query_fixture(store: &JsonlSessionStore, target_bytes: u64) -> anyhow::Result<()> {
    use std::io::Write;
    let records = JsonlSessionStore::read_event_records(store.path())?;
    let last = records.last().expect("identity fixture");
    let session_id = last.session_id().to_owned();
    let mut sequence = last.stream_sequence();
    let mut length = std::fs::metadata(store.path())?.len();
    let file = std::fs::OpenOptions::new()
        .append(true)
        .open(store.path())?;
    let mut output = std::io::BufWriter::new(file);
    while length < target_bytes {
        sequence += 1;
        let message =
            sigil_kernel::ModelMessage::user(format!("row-{sequence}:{}", "x".repeat(128 * 1024)));
        let event = sigil_kernel::StoredEvent::new(
            sigil_kernel::DurableEventType::UserMessageRecorded,
            sigil_kernel::EventClass::Critical,
            format!("qualification-{sequence}"),
            session_id.clone(),
            sequence,
            serde_json::json!({ "session_log_entry": SessionLogEntry::User(message) }),
        )?;
        let line = event.to_json_line()?;
        output.write_all(line.as_bytes())?;
        output.write_all(b"\n")?;
        length += line.len() as u64 + 1;
    }
    output.flush()?;
    Ok(())
}

fn qualify_hot_queries(mebibytes: u64) -> anyhow::Result<()> {
    let (_fixture, _session, store, binding) = owned_projection_fixture()?;
    append_large_query_fixture(&store, mebibytes * 1024 * 1024)?;
    let started = std::time::Instant::now();
    let snapshot = binding.build_snapshot(None)?.envelope;
    let cold_ms = started.elapsed().as_millis();
    let owner = binding.owner().expect("owned fixture");
    let cold = owner.metrics()?;
    let started = std::time::Instant::now();
    for _ in 0..20 {
        assert_eq!(binding.build_snapshot(None)?.envelope, snapshot);
    }
    let hot_ms = started.elapsed().as_millis();
    let hot = owner.metrics()?;
    assert_eq!(hot.full_prefix_scan_count, 1);
    assert_eq!(hot.bytes_read, cold.bytes_read);
    assert_eq!(hot.records_applied, cold.records_applied);
    let request = query_page_request(&binding, snapshot.cut, 20)?;
    let started = std::time::Instant::now();
    let page = binding.page_sync(request.clone())?;
    let page_ms = started.elapsed().as_millis();
    assert!(!page.items.is_empty());
    assert_eq!(page.total, snapshot.projection.conversation.message_count);
    assert!(serde_json::to_vec(&page.items)?.len() <= 1024 * 1024);
    let paged = owner.metrics()?;
    assert!(paged.page_bytes_read > 0 && paged.page_bytes_read <= 4 * 1024 * 1024);
    assert_eq!(paged.records_applied, cold.records_applied);
    assert_eq!(paged.full_prefix_scan_count, 1);
    assert_eq!(binding.page_sync(request)?, page);
    println!(
        "R75_QUERY mib={mebibytes} file_bytes={} cold_ms={cold_ms} hot_20_ms={hot_ms} page_ms={page_ms} full_scans={} hot_bytes={} hot_records={} page_bytes={} rows={}",
        std::fs::metadata(store.path())?.len(),
        paged.full_prefix_scan_count,
        hot.bytes_read - cold.bytes_read,
        hot.records_applied - cold.records_applied,
        paged.page_bytes_read,
        page.items.len()
    );
    let display_query = |cursor| ConversationDisplayQuery {
        expected_session_scope_id: &binding.expected_session_scope_id,
        cursor,
        limit: 20,
        current_workspace_snapshot_id: None,
        artifact_store: None,
    };
    let budget = sigil_kernel::SessionReadBudget::default();
    let before_display = owner.metrics()?;
    let display = owner.conversation_display_page(display_query(None), &budget)?;
    assert!(!display.items.is_empty());
    assert!(serde_json::to_vec(&display)?.len() <= 1024 * 1024);
    let display_metrics = owner.metrics()?;
    assert!(display_metrics.page_bytes_read - before_display.page_bytes_read <= 4 * 1024 * 1024);
    assert_eq!(display_metrics.records_applied, cold.records_applied);
    assert_eq!(display_metrics.full_prefix_scan_count, 1);
    let started = std::time::Instant::now();
    for _ in 0..20 {
        assert_eq!(
            owner.conversation_display_page(display_query(None), &budget)?,
            display
        );
    }
    let display_hot_ms = started.elapsed().as_millis();
    let display_hot = owner.metrics()?;
    assert_eq!(display_hot.bytes_read, display_metrics.bytes_read);
    assert_eq!(display_hot.records_applied, cold.records_applied);
    let cursor = display
        .next_cursor
        .as_deref()
        .expect("long fixture has an older page");
    let older = owner.conversation_display_page(display_query(Some(cursor)), &budget)?;
    assert!(!older.items.is_empty());
    assert_eq!(
        older.through_session_stream_sequence,
        display.through_session_stream_sequence
    );
    let older_metrics = owner.metrics()?;
    assert!(older_metrics.page_bytes_read - display_hot.page_bytes_read <= 4 * 1024 * 1024);
    assert_eq!(older_metrics.records_applied, cold.records_applied);
    if mebibytes == 1 {
        let canonical =
            crate::conversation_display::conversation_display_page_with_optional_artifact_store(
                store.path(),
                &binding.expected_session_scope_id,
                None,
                20,
                None,
                None,
            )?;
        assert_eq!(display, canonical);
    }
    println!(
        "R75_DISPLAY mib={mebibytes} hot_20_ms={display_hot_ms} full_scans={} hot_bytes={} hot_records={} latest_page_bytes={} older_page_bytes={} rows={}",
        display_hot.full_prefix_scan_count,
        display_hot.bytes_read - display_metrics.bytes_read,
        display_hot.records_applied - display_metrics.records_applied,
        display_metrics.page_bytes_read - before_display.page_bytes_read,
        older_metrics.page_bytes_read - display_hot.page_bytes_read,
        display.items.len()
    );
    Ok(())
}

#[test]
fn runtime_query_hot_path_smoke() -> anyhow::Result<()> {
    qualify_hot_queries(1)
}

#[test]
#[ignore = "explicit RFC-0075 long-log qualification"]
fn runtime_query_hot_path_20_mib() -> anyhow::Result<()> {
    qualify_hot_queries(20)
}

#[test]
#[ignore = "explicit RFC-0075 long-log qualification"]
fn runtime_query_hot_path_200_mib() -> anyhow::Result<()> {
    qualify_hot_queries(200)
}

#[test]
fn runtime_query_concurrent_catchup_applies_each_record_once() -> anyhow::Result<()> {
    let (_fixture, mut session, _store, binding) = owned_projection_fixture()?;
    binding.build_snapshot(None)?;
    let before = binding.owner().expect("owned").metrics()?;
    for index in 0..300 {
        session.append_user_message(sigil_kernel::ModelMessage::user(format!("tail-{index}")))?;
    }
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    std::thread::scope(|threads| -> anyhow::Result<()> {
        let workers = (0..8)
            .map(|_| {
                let binding = binding.clone();
                let barrier = barrier.clone();
                threads.spawn(move || -> anyhow::Result<_> {
                    barrier.wait();
                    Ok(binding.build_snapshot(None)?.envelope)
                })
            })
            .collect::<Vec<_>>();
        for worker in workers {
            assert_eq!(
                worker
                    .join()
                    .expect("query worker")?
                    .projection
                    .conversation
                    .message_count,
                300
            );
        }
        Ok(())
    })?;
    let after = binding.owner().expect("owned").metrics()?;
    assert_eq!(after.full_prefix_scan_count, 1);
    assert_eq!(after.records_applied - before.records_applied, 300);
    Ok(())
}

#[test]
fn runtime_query_source_replacement_requires_a_new_attachment() -> anyhow::Result<()> {
    let (fixture, _session, store, binding) = owned_projection_fixture()?;
    binding.build_snapshot(None)?;
    let bytes = std::fs::read(store.path())?;
    let replacement = fixture.path().join("replacement.jsonl");
    std::fs::write(&replacement, bytes)?;
    std::fs::rename(replacement, store.path())?;
    for _ in 0..2 {
        assert!(matches!(
            binding.build_snapshot(None),
            Err(ApplicationError::ResetRequired)
        ));
    }
    Ok(())
}

#[test]
fn cancelled_query_remains_retryable_while_corruption_is_permanent() -> anyhow::Result<()> {
    let (_fixture, _session, _store, binding) = owned_projection_fixture()?;
    let cancelled = sigil_kernel::SessionReadBudget::default();
    cancelled.cancel();
    assert!(matches!(
        binding.snapshot_with_budget(None, &cancelled),
        Err(ApplicationError::Unavailable)
    ));
    assert!(binding.build_snapshot(None).is_ok());
    Ok(())
}

#[test]
fn message_content_pages_bind_identity_and_preserve_unicode_under_a_fixed_budget()
-> anyhow::Result<()> {
    use sigil_application::message_content::{MessageContentError, MessageContentQuery};
    let (_fixture, mut session, _store, binding) = owned_projection_fixture()?;
    let body = format!(
        "{}🦀中文{}FINAL_CONTENT_MARKER",
        "x".repeat(65_535),
        "y".repeat(40_000)
    );
    let mut message = sigil_kernel::ModelMessage::assistant_with_kind(
        Some(body.clone()),
        vec![],
        sigil_kernel::AssistantMessageKind::FinalAnswer,
    );
    message.id = format!("token=message-content-private-carrier-{}", "s".repeat(300));
    let raw_message_id = message.id.clone();
    let message_id =
        crate::application_run::safe_application_transcript_message_id(&raw_message_id);
    session.append_assistant_message(message)?;
    let owner = binding.owner().expect("owner");
    let budget = sigil_kernel::SessionReadBudget::default();
    let display = owner.conversation_display_page(
        ConversationDisplayQuery {
            expected_session_scope_id: session.session_scope_id(),
            cursor: None,
            limit: 100,
            current_workspace_snapshot_id: None,
            artifact_store: None,
        },
        &budget,
    )?;
    let query = MessageContentQuery {
        display_id: display.items[0].display_id.clone(),
        offset: 0,
        limit: 65_536,
        content_version: None,
    };
    let initial = owner.metrics()?;
    let first = owner.message_content_page(session.session_scope_id(), &query, &budget)?;
    assert_eq!(first.message_id, message_id);
    assert!(first.message_id.starts_with("message-sha256:"));
    assert!(!serde_json::to_string(&first)?.contains("message-content-private-carrier"));
    assert!(!serde_json::to_string(&first)?.contains(&raw_message_id));
    assert_eq!(first.text.len(), 65_535);
    assert_eq!(first.next_offset, Some(65_535));
    assert_eq!(first.total_bytes, body.len() as u64);
    let second_query = MessageContentQuery {
        offset: first.next_offset.expect("next"),
        content_version: Some(first.content_version.clone()),
        ..query.clone()
    };
    // Appending unrelated durable history does not change this immutable message cut.
    session.append_user_message(sigil_kernel::ModelMessage::user("later turn"))?;
    let second = owner.message_content_page(session.session_scope_id(), &second_query, &budget)?;
    assert_eq!(second.next_offset, None);
    assert_eq!(format!("{}{}", first.text, second.text), body);
    let hot = owner.metrics()?;
    assert_eq!(hot.full_prefix_scan_count, initial.full_prefix_scan_count);
    assert_eq!(hot.records_applied - initial.records_applied, 1);
    assert!(hot.page_bytes_read - initial.page_bytes_read < 2 * 2 * 1024 * 1024);
    for bad in [
        MessageContentQuery {
            offset: 65_536,
            ..second_query.clone()
        },
        MessageContentQuery {
            offset: body.len() as u64 + 1,
            ..second_query.clone()
        },
        MessageContentQuery {
            offset: 1,
            content_version: None,
            ..query.clone()
        },
        MessageContentQuery {
            limit: 65_537,
            ..query.clone()
        },
    ] {
        assert_eq!(
            owner.message_content_page(session.session_scope_id(), &bad, &budget),
            Err(MessageContentError::InvalidQuery)
        );
    }
    assert_eq!(
        owner.message_content_page(
            session.session_scope_id(),
            &MessageContentQuery {
                content_version: Some("0".repeat(64)),
                ..query.clone()
            },
            &budget
        ),
        Err(MessageContentError::Stale)
    );
    assert_eq!(
        owner.message_content_page("wrong-session", &query, &budget),
        Err(MessageContentError::NotFound)
    );
    assert_eq!(
        owner.message_content_page(
            session.session_scope_id(),
            &MessageContentQuery {
                display_id: "wrong-display".into(),
                ..query.clone()
            },
            &budget
        ),
        Err(MessageContentError::NotFound)
    );
    let cancelled = sigil_kernel::SessionReadBudget::default();
    cancelled.cancel();
    assert_eq!(
        owner.message_content_page(session.session_scope_id(), &query, &cancelled),
        Err(MessageContentError::Unavailable)
    );
    assert_eq!(
        owner.message_content_page(session.session_scope_id(), &query, &budget)?,
        first
    );
    Ok(())
}

#[test]
fn message_content_reads_user_and_reasoning_but_rejects_replaced_sources() -> anyhow::Result<()> {
    use sigil_application::message_content::{MessageContentError, MessageContentQuery};
    let (fixture, mut session, store, binding) = owned_projection_fixture()?;
    session.append_user_message(sigil_kernel::ModelMessage::user("user body"))?;
    session.append_assistant_message(sigil_kernel::ModelMessage::assistant_with_kind(
        Some("assistant reasoning body".into()),
        vec![],
        sigil_kernel::AssistantMessageKind::ReasoningTrace,
    ))?;
    session.append_control(ControlEntry::Note {
        kind: "reasoning_trace".into(),
        data: serde_json::json!({"text":"control-note reasoning body"}),
    })?;
    let owner = binding.owner().expect("owner");
    let budget = sigil_kernel::SessionReadBudget::default();
    let display = owner.conversation_display_page(
        ConversationDisplayQuery {
            expected_session_scope_id: session.session_scope_id(),
            cursor: None,
            limit: 100,
            current_workspace_snapshot_id: None,
            artifact_store: None,
        },
        &budget,
    )?;
    assert_eq!(display.items.len(), 3);
    for (item, expected) in display.items.iter().zip([
        "user body",
        "assistant reasoning body",
        "control-note reasoning body",
    ]) {
        let query = MessageContentQuery {
            display_id: item.display_id.clone(),
            offset: 0,
            limit: 65_536,
            content_version: None,
        };
        assert_eq!(
            owner
                .message_content_page(session.session_scope_id(), &query, &budget)?
                .text,
            expected
        );
    }
    let query = MessageContentQuery {
        display_id: display.items[0].display_id.clone(),
        offset: 0,
        limit: 65_536,
        content_version: None,
    };
    let replacement = fixture.path().join("replacement-content.jsonl");
    std::fs::write(&replacement, std::fs::read(store.path())?)?;
    std::fs::rename(replacement, store.path())?;
    assert_eq!(
        owner.message_content_page(session.session_scope_id(), &query, &budget),
        Err(MessageContentError::Stale)
    );
    Ok(())
}

#[test]
fn cancelling_a_coordinated_read_preserves_the_verified_prefix_for_retry() -> anyhow::Result<()> {
    let (_fixture, mut session, store, binding) = owned_projection_fixture()?;
    session.append_user_message(sigil_kernel::ModelMessage::user("verified prefix"))?;
    binding.build_snapshot(None)?;
    let owner = binding.owner().expect("actual owner");
    let before = owner.metrics()?;
    session.append_user_message(sigil_kernel::ModelMessage::user("new suffix"))?;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(store.path())?;
    fs2::FileExt::lock_exclusive(&lock)?;
    let deadline = sigil_kernel::SessionReadBudget::new(Some(
        std::time::Instant::now() + std::time::Duration::from_millis(30),
    ));
    assert!(matches!(
        binding.snapshot_with_budget(None, &deadline),
        Err(ApplicationError::Unavailable)
    ));
    assert_eq!(
        owner.cache.lock().expect("query state").sequence,
        before.records_applied
    );
    fs2::FileExt::unlock(&lock)?;
    let recovered = binding.build_snapshot(None)?;
    assert_eq!(recovered.envelope.projection.conversation.message_count, 2);
    let after = owner.metrics()?;
    assert_eq!(after.full_prefix_scan_count, 1);
    assert_eq!(
        after.records_applied,
        before.records_applied + 1,
        "retry must apply only the unvalidated suffix"
    );
    Ok(())
}

#[tokio::test]
async fn durable_delivery_bounds_notice_text_without_blocking_its_original_ids()
-> anyhow::Result<()> {
    use sigil_kernel::EventHandler;
    let (_fixture, session, store, binding) = owned_projection_fixture()?;
    let mut recorder =
        crate::ApplicationRunEventRecorder::start(&session, "notice-bounds", "inspect")?;
    let port = Arc::new(ProjectionClientTestPort {
        binding: binding.clone(),
        acknowledgements: Default::default(),
    });
    let client = sigil_application::ApplicationClient::new(
        port,
        binding.scope.clone(),
        1,
        1,
        sigil_application::HostConnectionInstanceId::new("bounded-notices")?,
    )?;
    let initial = client.refresh().await?;
    client.refresh_delivery().await?;
    let initial_ids = client.take_applied_delivery_event_ids()?;
    binding
        .acknowledge_tui_public_events(&initial_ids, &initial.frontier)
        .await?;
    recorder.handle(sigil_kernel::RunEvent::Notice(String::new()))?;
    recorder.handle(sigil_kernel::RunEvent::Notice("界".repeat(30_000)))?;
    recorder.handle(sigil_kernel::RunEvent::Notice("tail notice".into()))?;
    let current = client.refresh().await?;
    let batch = client.refresh_delivery().await?;
    assert_eq!(batch.notices.len(), 2);
    assert_eq!(
        batch.notices[0].as_str(),
        "界".repeat(sigil_application::MAX_SAFE_TEXT_BYTES / 3)
    );
    assert_eq!(batch.notices[1].as_str(), "tail notice");
    assert_eq!(client.current_frontier()?, Some(current.frontier.clone()));
    let ids = client.take_applied_delivery_event_ids()?;
    assert_eq!(
        ids.len(),
        3,
        "the empty notice keeps its original delivery identity"
    );
    assert_eq!(
        binding
            .acknowledge_tui_public_events(&ids, &current.frontier)
            .await?,
        3
    );
    assert!(client.refresh_delivery().await?.notices.is_empty());
    assert!(client.take_applied_delivery_event_ids()?.is_empty());
    let pending = sigil_kernel::PublicEventOutboxProjectionV1::from_records(
        &store.read_handle().read_event_records()?,
    )?;
    assert!(pending.pending_for_adapter("tui").is_empty());
    Ok(())
}

#[tokio::test]
async fn aborting_observation_stops_its_blocking_work() -> anyhow::Result<()> {
    use std::sync::atomic::{AtomicBool, Ordering};
    let started = Arc::new(tokio::sync::Notify::new());
    let stopped = Arc::new(AtomicBool::new(false));
    let notify = started.clone();
    let flag = stopped.clone();
    let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (release, blocked) = std::sync::mpsc::channel();
    let worker = tokio::spawn(observation(counter.clone(), move |budget| {
        notify.notify_one();
        blocked
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("release simulated blocking syscall");
        assert!(budget.check().is_err());
        flag.store(true, Ordering::Release);
        Err::<(), _>(ApplicationError::Unavailable)
    }));
    started.notified().await;
    worker.abort();
    assert!(worker.await.expect_err("aborted observer").is_cancelled());
    assert_eq!(
        counter.load(Ordering::Acquire),
        1,
        "outer abort is not physical completion"
    );
    release.send(())?;
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while !stopped.load(Ordering::Acquire) || counter.load(Ordering::Acquire) != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    Ok(())
}

fn append_current_delivery_fixture(
    store: &JsonlSessionStore,
    session_id: &str,
    count: u64,
) -> anyhow::Result<()> {
    use std::io::Write;
    let records = store.read_handle().read_event_records()?;
    let cut = records
        .last()
        .expect("initialized stream")
        .stream_sequence();
    let mut output = std::io::BufWriter::new(
        std::fs::OpenOptions::new()
            .append(true)
            .open(store.path())?,
    );
    for index in 1..=count {
        let event = sigil_kernel::PublicRunEvent::new(
            session_id,
            "current-backlog",
            index,
            PublicRunEventKind::Notice {
                message: format!("current-{index}"),
            },
        );
        let id = format!("application-public:{session_id}:current-backlog:{index}");
        let entry = sigil_kernel::PublicEventOutboxEntryV1 {
            schema_version: sigil_kernel::PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
            public_event_id: id.clone(),
            domain_event_id: id.clone(),
            run_id: "current-backlog".into(),
            sequence: index,
            payload_digest: sigil_kernel::stable_event_hash(serde_json::to_vec(&event)?),
            event,
        };
        let record = sigil_kernel::StoredEvent::new(
            DurableEventType::PublicEventOutbox,
            sigil_kernel::EventClass::Critical,
            id,
            session_id.to_owned(),
            cut + index,
            serde_json::to_value(entry)?,
        )?;
        writeln!(output, "{}", record.to_json_line()?)?;
    }
    output.flush()?;
    drop(output);
    // Opening an existing owner performs its one recovery bootstrap before observers attach.
    // ACK itself must not start writer recovery or replay history.
    drop(store.read_event_records_writer()?);
    Ok(())
}

#[tokio::test]
#[ignore = "explicit RFC-0075 10000-event delivery and cancellation qualification"]
async fn runtime_delivery_10k_backlog_partial_ack_cancel_and_retry() -> anyhow::Result<()> {
    let (_fixture, _session, store, binding) = owned_projection_fixture()?;
    append_current_delivery_fixture(&store, &binding.expected_session_scope_id, 10_000)?;
    let original_prefix = std::fs::read(store.path())?;
    let port = Arc::new(ProjectionClientTestPort {
        binding: binding.clone(),
        acknowledgements: Default::default(),
    });
    let client = sigil_application::ApplicationClient::new(
        port,
        binding.scope.clone(),
        1,
        1,
        sigil_application::HostConnectionInstanceId::new("backlog-client")?,
    )?;
    let current = client.refresh().await?;
    assert_eq!(
        current
            .attention
            .last_notice
            .as_ref()
            .map(|notice| notice.as_str()),
        Some("current-10000")
    );
    assert!(client.take_applied_delivery_event_ids()?.is_empty());
    let owner = binding.owner().expect("owned fixture");
    let before = owner.metrics()?;
    let mut all_ids = Vec::new();
    let mut batches = 0;
    loop {
        let delivered = client.refresh_delivery().await?;
        let ids = client.take_applied_delivery_event_ids()?;
        assert!(ids.len() <= 256);
        all_ids.extend(ids);
        batches += 1;
        assert_eq!(client.current_frontier()?, Some(current.frontier.clone()));
        if !delivered.has_more {
            break;
        }
    }
    assert_eq!(all_ids.len(), 10_000);
    assert_eq!(batches, 40);
    assert_eq!(client.refresh().await?, current);
    assert_eq!(owner.metrics()?.records_applied, before.records_applied);

    struct CancelAfterCommit(sigil_kernel::SessionReadBudget);
    impl sigil_kernel::session::ActiveProjectionObserver for CancelAfterCommit {
        fn active_projection_changed(
            &self,
            _notice: sigil_kernel::session::ActiveProjectionNotice,
        ) {
            self.0.cancel();
        }
    }
    let budget = sigil_kernel::SessionReadBudget::default();
    let observer = Arc::new(CancelAfterCommit(budget.clone()));
    let subscription = store.register_active_projection_observer(observer);
    let acknowledgement = binding.clone();
    let ids = all_ids.clone();
    let frontier = current.frontier.clone();
    let cancelled = tokio::task::spawn_blocking(move || {
        acknowledgement.acknowledge_with_budget(&ids, &frontier, &budget)
    })
    .await?;
    assert!(matches!(cancelled, Err(ApplicationError::Unavailable)));
    drop(subscription);
    let partial = sigil_kernel::PublicEventOutboxProjectionV1::from_records(
        &store.read_handle().read_event_records()?,
    )?;
    assert_eq!(partial.pending_for_adapter("tui").len(), 10_000 - 256);
    let resumed = binding
        .acknowledge_tui_public_events(&all_ids, &current.frontier)
        .await?;
    assert_eq!(resumed, 10_000 - 256);
    assert_eq!(
        binding
            .acknowledge_tui_public_events(&all_ids, &current.frontier)
            .await?,
        0
    );
    let complete = sigil_kernel::PublicEventOutboxProjectionV1::from_records(
        &store.read_handle().read_event_records()?,
    )?;
    assert!(complete.pending_for_adapter("tui").is_empty());
    assert!(
        std::fs::read(store.path())?.starts_with(&original_prefix),
        "delivery and retry must preserve every original ID, sequence and payload byte"
    );
    println!(
        "R75_DELIVERY kind=current_notice events={} batches={batches} first_committed=256 resumed={resumed} duplicate_retry=0 prefix_unchanged=true full_scans={}",
        all_ids.len(),
        owner.metrics()?.full_prefix_scan_count
    );
    Ok(())
}

#[tokio::test]
async fn aborting_actual_ack_preserves_outstanding_work_until_the_committed_batch_returns()
-> anyhow::Result<()> {
    struct BlockCommittedAck {
        entered: tokio::sync::Notify,
        release: Mutex<std::sync::mpsc::Receiver<()>>,
    }
    impl sigil_kernel::session::ActiveProjectionObserver for BlockCommittedAck {
        fn active_projection_changed(
            &self,
            _notice: sigil_kernel::session::ActiveProjectionNotice,
        ) {
            self.entered.notify_one();
            self.release
                .lock()
                .expect("release receiver")
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("release the already committed ACK batch");
        }
    }
    let (_fixture, _session, store, binding) = owned_projection_fixture()?;
    append_current_delivery_fixture(&store, &binding.expected_session_scope_id, 300)?;
    let snapshot = binding.build_snapshot(None)?.envelope;
    let pending = sigil_kernel::PublicEventOutboxProjectionV1::from_records(
        &store.read_handle().read_event_records()?,
    )?;
    let ids = pending
        .pending_for_adapter("tui")
        .into_iter()
        .map(|entry| entry.public_event_id.clone())
        .collect::<Vec<_>>();
    assert_eq!(ids.len(), 300);
    let (release, receiver) = std::sync::mpsc::channel();
    let observer = Arc::new(BlockCommittedAck {
        entered: tokio::sync::Notify::new(),
        release: Mutex::new(receiver),
    });
    let subscription = store.register_active_projection_observer(observer.clone());
    let acknowledgement = binding.clone();
    let worker = tokio::spawn(async move {
        acknowledgement
            .acknowledge_tui_public_events(&ids, &snapshot.cut)
            .await
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        observer.entered.notified(),
    )
    .await?;
    worker.abort();
    assert!(worker.await.expect_err("ACK future aborted").is_cancelled());
    assert_eq!(
        binding.pending_observations(),
        1,
        "committed writer callback is still running"
    );
    release.send(())?;
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while binding.pending_observations() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    drop(subscription);
    let pending = sigil_kernel::PublicEventOutboxProjectionV1::from_records(
        &store.read_handle().read_event_records()?,
    )?;
    assert_eq!(
        pending.pending_for_adapter("tui").len(),
        44,
        "only the already admitted batch may finish after cancellation"
    );
    Ok(())
}

#[tokio::test]
async fn readonly_projection_cannot_ack_even_after_a_valid_snapshot() -> anyhow::Result<()> {
    let (_fixture, _session, store, binding) = owned_projection_fixture()?;
    let readonly = binding.with_owner(RuntimeSessionProjectionOwner::from_read_handle(
        store.read_handle(),
    ));
    let before = std::fs::read(store.path())?;
    let snapshot = readonly.build_snapshot(None)?;
    assert!(matches!(
        readonly
            .acknowledge_tui_public_events(&[], &snapshot.envelope.cut)
            .await,
        Err(ApplicationError::Unavailable)
    ));
    assert_eq!(std::fs::read(store.path())?, before);
    assert_eq!(readonly.pending_observations(), 0);
    Ok(())
}

#[tokio::test]
async fn worker_projection_replacement_waits_for_real_observation_and_keeps_scope()
-> anyhow::Result<()> {
    let (_fixture, _session, store, binding) = owned_projection_fixture()?;
    let shared = binding.clone();
    let entered = Arc::new(tokio::sync::Notify::new());
    let observer_entered = Arc::clone(&entered);
    let (release, blocked) = std::sync::mpsc::channel();
    let counter = binding.observation_counter();
    let observation = tokio::spawn(observation(counter, move |_| {
        observer_entered.notify_one();
        blocked.recv().expect("release old projection observation");
        Ok(())
    }));
    tokio::time::timeout(std::time::Duration::from_secs(2), entered.notified()).await?;
    let next = RuntimeSessionProjectionOwner::from_store(&store);
    assert!(matches!(
        binding.replace_owner(next.clone()),
        Err(ApplicationError::Unavailable)
    ));
    assert_eq!(binding.pending_observations(), 1);
    release.send(())?;
    observation.await??;
    binding.replace_owner(next.clone())?;
    assert!(
        Arc::ptr_eq(&shared.owner()?.cache, &next.cache),
        "service clones share the published owner"
    );
    let foreign_root = tempfile::tempdir()?;
    let foreign_store = JsonlSessionStore::new(foreign_root.path().join("foreign.jsonl"))?;
    let _foreign =
        sigil_kernel::Session::load_from_store("fixture", "model", foreign_store.clone())?;
    assert!(matches!(
        binding.replace_owner(RuntimeSessionProjectionOwner::from_store(&foreign_store)),
        Err(ApplicationError::ScopeMismatch)
    ));
    assert!(Arc::ptr_eq(&binding.owner()?.cache, &next.cache));
    Ok(())
}

#[test]
fn message_image_reference_survives_reopen_and_is_bound_to_exact_session_and_message()
-> anyhow::Result<()> {
    use sigil_application::message_content::MessageContentError;
    let (fixture, mut session, store, binding) = owned_projection_fixture()?;
    let cache = crate::ControlledImageAttachmentCache::new(fixture.path().join("images"));
    let mut encoded = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(2, 3).write_to(&mut encoded, image::ImageFormat::Png)?;
    let image = cache.ingest_encoded_bytes("image-1", encoded.into_inner())?;
    session.try_attach_image_attachment_resolver(std::sync::Arc::new(cache))?;
    let mut message = sigil_kernel::ModelMessage::user("");
    message.image_attachments = vec![image.clone()];
    session.append_user_message(message)?;
    session.append_user_message(sigil_kernel::ModelMessage::user("unrelated message"))?;
    let owner = binding.owner().expect("projection owner");
    let budget = sigil_kernel::SessionReadBudget::default();
    let display = owner.conversation_display_page(
        ConversationDisplayQuery {
            expected_session_scope_id: session.session_scope_id(),
            cursor: None,
            limit: 100,
            current_workspace_snapshot_id: None,
            artifact_store: None,
        },
        &budget,
    )?;
    let selected = &display.items[0];
    let crate::conversation_display::ConversationDisplayContentV1::Message {
        image_attachments,
        ..
    } = &selected.content
    else {
        panic!("user message");
    };
    assert_eq!(image_attachments, &vec![image.without_resolved_bytes()]);
    let reopened = crate::RuntimeSessionProjectionOwner::from_store(&store);
    assert_eq!(
        reopened.message_image_attachment(
            session.session_scope_id(),
            &selected.display_id,
            "image-1",
            &budget
        )?,
        image.without_resolved_bytes()
    );
    assert!(matches!(
        reopened.message_image_attachment(
            "other-session",
            &selected.display_id,
            "image-1",
            &budget
        ),
        Err(MessageContentError::NotFound)
    ));
    assert!(matches!(
        reopened.message_image_attachment(
            session.session_scope_id(),
            &display.items[1].display_id,
            "image-1",
            &budget
        ),
        Err(MessageContentError::NotFound)
    ));
    assert!(matches!(
        reopened.message_image_attachment(
            session.session_scope_id(),
            &selected.display_id,
            "other-image",
            &budget
        ),
        Err(MessageContentError::NotFound)
    ));
    Ok(())
}
