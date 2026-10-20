//! A real root Session owns both the user-input decision and its application commit evidence.

use super::*;
use sigil_application::UserInputCommand;
use sigil_kernel::{
    AgentThreadId, ApplicationOperationTargetV1, LogicalRunId, ModelMessage, SessionScopeId,
    UserInputActionV1, UserInputContinuationBindingV1, UserInputDecisionV1,
    UserInputDurableDecisionV1, UserInputIdentityV1, UserInputLifecycleEntryV1, UserInputPurposeV1,
    UserInputQuestionV1, UserInputRequestId, UserInputRequestV1, UserInputRequestedV1,
    UserInputResolutionV1, UserInputSourceV1,
};

fn seed_root_input_request(
    owner: &sigil_kernel::SessionApplicationOperationOwner,
) -> Result<UserInputRequestedV1> {
    let mut session = owner.attach_for_control()?;
    let call = ToolCall {
        id: "http-root-input-call".to_owned(),
        name: sigil_kernel::REQUEST_USER_INPUT_TOOL_NAME.to_owned(),
        args_json: "{}".to_owned(),
    };
    let source_binding_hash = format!(
        "sha256:{}",
        sigil_kernel::sha256_hex(&serde_json::to_vec(&call)?)
    );
    let mut assistant = ModelMessage::assistant(None, vec![call.clone()]);
    assistant.id = "http-root-input-assistant".to_owned();
    let continuation = UserInputContinuationBindingV1 {
        assistant_message_id: assistant.id.clone(),
        tool_call_id: call.id,
        provider_name: session.provider_name().to_owned(),
        model_name: session.model_name().to_owned(),
    };
    session.append_assistant_message(assistant)?;
    let requested = UserInputRequestedV1::new(UserInputRequestV1 {
        schema_version: sigil_kernel::USER_INPUT_SCHEMA_VERSION,
        identity: UserInputIdentityV1 {
            session_scope_id: SessionScopeId::new(session.session_scope_id())?,
            root_logical_run_id: LogicalRunId::new("http-root-input-run")?,
            source_thread_id: AgentThreadId::new("main")?,
            request_id: UserInputRequestId::new("http-root-input-request")?,
            generation: 1,
            source_binding_hash,
        },
        source: UserInputSourceV1::Agent,
        purpose: UserInputPurposeV1::Clarification,
        prompt: "Select the migration mode".to_owned(),
        questions: vec![UserInputQuestionV1 {
            id: "mode".to_owned(),
            question: "Which migration mode should be used?".to_owned(),
            description: None,
            required: true,
            options: Vec::new(),
            multiple: false,
        }],
        allowed_actions: vec![
            UserInputActionV1::Submit,
            UserInputActionV1::Decline,
            UserInputActionV1::CancelRun,
        ],
        requested_at_unix_ms: 10,
        continuation: Some(continuation),
    })?;
    // Seed only a legitimate pending request. The actual HTTP/runtime decision path must
    // prepare and commit its own operation; the fixture does not manufacture either marker.
    session.append_user_input_lifecycle(vec![UserInputLifecycleEntryV1::Requested(Box::new(
        requested.clone(),
    ))])?;
    Ok(requested)
}

fn seed_waiting_root_input(
    owner: &sigil_kernel::SessionApplicationOperationOwner,
) -> Result<UserInputRequestedV1> {
    let session = owner.attach_for_control()?;
    let lifecycle = session.conversation_run_lifecycle_recorder()?;
    lifecycle.append_started(&sigil_kernel::ConversationRunStartedEntryV1::new(
        "http-root-input-run",
        1,
    )?)?;
    drop(session);
    let requested = seed_root_input_request(owner)?;
    let event = PublicRunEvent::new(
        requested.request.identity.session_scope_id.as_str(),
        "http-root-input-run",
        1,
        PublicRunEventKind::RunAwaitingUserInput {
            request_id: requested.request.identity.request_id.as_str().to_owned(),
            generation: 1,
            request_hash: requested.request_hash.clone(),
        },
    );
    lifecycle.append_finalized_with_outbox(
        &sigil_kernel::ConversationRunFinalizedEntryV1::new(
            "http-root-input-run",
            sigil_kernel::ConversationRunTerminalStatusV1::AwaitingUserInput,
            None,
            None,
            10,
            &sigil_kernel::SecretRedactor::empty(),
        )?,
        &sigil_kernel::PublicEventOutboxEntryV1 {
            schema_version: sigil_kernel::PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
            public_event_id: "http-root-awaiting-public".to_owned(),
            domain_event_id: "http-root-awaiting-domain".to_owned(),
            run_id: "http-root-input-run".to_owned(),
            sequence: 1,
            payload_digest: sigil_kernel::stable_event_hash(serde_json::to_vec(&event)?),
            event,
        },
    )?;
    Ok(requested)
}

async fn post_input_decision(
    address: std::net::SocketAddr,
    path: &str,
    body: &str,
) -> Result<(u16, serde_json::Value)> {
    let mut socket = tokio::net::TcpStream::connect(address).await?;
    socket.write_all(format!("POST {path} HTTP/1.1\r\nhost: localhost\r\nauthorization: Bearer input-test-token\r\ncontent-type: application/json\r\nconnection: close\r\ncontent-length: {}\r\n\r\n{body}", body.len()).as_bytes()).await?;
    let mut bytes = Vec::new();
    socket.read_to_end(&mut bytes).await?;
    let response = String::from_utf8(bytes)?;
    let (headers, body) = response
        .split_once("\r\n\r\n")
        .context("HTTP response separator")?;
    let status = headers
        .split_whitespace()
        .nth(1)
        .context("HTTP status")?
        .parse()?;
    Ok((status, serde_json::from_str(body)?))
}

async fn get_input_display(
    address: std::net::SocketAddr,
    session_id: &str,
) -> Result<crate::HttpConversationDisplayPage> {
    let mut socket = tokio::net::TcpStream::connect(address).await?;
    socket
        .write_all(
            format!(
                "GET /sessions/{session_id}/display?limit=100 HTTP/1.1\r\nhost: localhost\r\nauthorization: Bearer input-test-token\r\nconnection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .await?;
    let mut bytes = Vec::new();
    socket.read_to_end(&mut bytes).await?;
    let response = String::from_utf8(bytes)?;
    let (headers, body) = response
        .split_once("\r\n\r\n")
        .context("display HTTP response separator")?;
    let status: u16 = headers
        .split_whitespace()
        .nth(1)
        .context("display HTTP status")?
        .parse()?;
    assert_eq!(status, 200, "{body}");
    serde_json::from_str(body).context("typed canonical display response")
}

async fn serve_input_terminal_retires_waiting(
    decision: UserInputDecisionV1,
    terminal: HttpRunStatus,
    display_status: crate::HttpConversationDisplayStatus,
) -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let config_path = fixture.path().join("sigil.toml");
    write_production_test_config(&config_path, ".");
    let sessions = fixture.path().join("sessions");
    std::fs::create_dir_all(&sessions)?;
    let protocol = Arc::new(crate::HttpDurableProtocolJournal::open(
        fixture.path().join("protocol.json"),
        32,
    )?);
    let events = Arc::new(HttpLiveEventBus::with_durable_journal(32, protocol));
    let disclosures = Arc::new(HttpDurableEgressDisclosureJournal::open(
        fixture.path().join("disclosures.json"),
        32,
    )?);
    let driver = Arc::new(HttpProductionRunDriver::new(
        HttpProductionRunDriverOptions::new(&config_path, fixture.path()).with_session_lifecycle(
            sigil_runtime::LocalSessionLifecycleService::new(
                "input-cancellation-fixture",
                &sessions,
                fixture.path().join("exports"),
            ),
        ),
        disclosures.clone(),
        events.clone(),
        tokio::runtime::Handle::current(),
    )?);
    let registry = driver.build_registry(Arc::new(HttpDurableCommandStore::open(
        fixture.path().join("input-commands.json"),
        32,
    )?))?;
    let session = registry.create_session(HttpSessionCreateRequest::default())?;
    let owner = driver.application_operation_owner(&session)?;
    let requested = seed_waiting_root_input(&owner.owner)?;
    let waiting = sigil_kernel::PublicEventOutboxProjectionV1::from_records(
        &owner.owner.read_handle().read_event_records()?,
    )?
    .events_in_order()
    .last()
    .context("original waiting outbox")?
    .event
    .clone();
    let waiting_bytes = serde_json::to_value(&waiting)?;
    publish_exact_http_outbox_event(
        &driver.event_bus,
        &session.durable_session_scope_id,
        "http-root-input-run",
        waiting,
    )?;
    let mutation = registry.reserve_durable_session_mutation(&session.durable_session_scope_id)?;
    registry.register_supervised_session_run(
        &session.id,
        "http-root-input-run",
        HttpPermissionMode::Manual,
        "clarification",
    )?;
    drop(mutation);
    record_run_terminal_and_reconcile_stream(
        &registry,
        &driver.event_bus,
        &session.durable_session_scope_id,
        "http-root-input-run",
        HttpRunTerminalOutcome::Paused,
    )?;
    assert!(
        !driver
            .event_bus
            .run_stream_accepts_events(&session.durable_session_scope_id, "http-root-input-run")?
    );
    assert!(
        driver
            .event_bus
            .publish_run_event_with_stream_continuation(PublicRunEvent::new(
                &session.durable_session_scope_id,
                "http-root-input-run",
                2,
                PublicRunEventKind::RunCancelled,
            ))
            .is_err(),
        "a sealed stream cannot be reopened by an unbound generic terminal event"
    );
    let client = registry.application_client(&session.id, "input-test-client")?;
    assert!(client.refresh()?.user_input.pending);
    let catalog = Arc::new(sigil_runtime::SessionCatalogProjectionService::new(
        driver
            .session_lifecycle()
            .context("production managed lifecycle")?
            .clone(),
        fixture.path().join("catalog.sqlite3"),
    ));
    let server = crate::HttpLocalServer::bind_production(
        crate::HttpServerConfig::default(),
        Some("input-test-token"),
        registry.clone(),
        driver.event_bus.clone(),
        disclosures,
        catalog,
        "input-cancellation-fixture",
        false,
    )
    .await?;
    let address = server.local_addr()?;
    let (shutdown, shutdown_rx) = tokio::sync::oneshot::channel();
    let serving = tokio::spawn(async move {
        server
            .serve_until_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await
    });
    let envelope = HttpCommandEnvelope::new(
        "ordinary-cancel-once",
        "input-test-client",
        &session.id,
        HttpUserInputDecisionRequest {
            generation: 1,
            expected_request_hash: requested.request_hash.clone(),
            decision: decision.clone(),
            permission_mode: None,
        },
    );
    let path = format!(
        "/sessions/{}/user-input/{}/decision",
        session.id,
        requested.request.identity.request_id.as_str()
    );
    let body = serde_json::to_string(&envelope)?;
    let (status, receipt) = post_input_decision(address, &path, &body).await?;
    assert_eq!(status, 200, "{receipt}");
    assert!(
        !driver
            .event_bus
            .run_stream_accepts_events(&session.durable_session_scope_id, "http-root-input-run")?
    );
    let waiting = driver
        .event_bus
        .retained_run_event_at(&session.durable_session_scope_id, "http-root-input-run", 1)?
        .context("original waiting event is retained")?;
    assert_eq!(
        serde_json::to_value(waiting.run_event.context("waiting payload")?)?,
        waiting_bytes,
        "admitting the verified successor preserves the old event's exact public bytes"
    );
    assert_eq!(registry.get_run("http-root-input-run")?.status, terminal);
    assert!(registry.session_foreground_owner(&session.id)?.is_none());
    assert!(!driver.has_unresolved_user_input(&session)?);
    let projection = client.refresh()?;
    assert!(!projection.user_input.pending);
    assert!(projection.run.active_binding.is_none());
    let records = owner.owner.read_handle().read_event_records()?;
    let outboxes = sigil_kernel::PublicEventOutboxProjectionV1::from_records(&records)?;
    let terminal_outboxes = outboxes
        .events_in_order()
        .into_iter()
        .filter(|entry| {
            entry.run_id == "http-root-input-run"
                && matches!(
                    entry.event.event,
                    PublicRunEventKind::RunCancelled | PublicRunEventKind::RunBlocked { .. }
                )
        })
        .collect::<Vec<_>>();
    assert_eq!(terminal_outboxes.len(), 1, "one exact public successor");
    let terminal_outbox = terminal_outboxes[0];
    let domain_source = records
        .iter()
        .find(|record| record.event_id() == terminal_outbox.domain_event_id)
        .context("terminal's exact durable domain source")?;
    let Some(SessionLogEntry::Control(ControlEntry::UserInputResolved(resolved))) =
        domain_source.session_log_entry()?
    else {
        anyhow::bail!("ordinary input terminal must originate from its typed resolution")
    };
    assert_eq!(resolved.identity, requested.request.identity);
    assert_eq!(resolved.request_hash, requested.request_hash);
    let expected_summary = match (&decision, &terminal_outbox.event.event) {
        (UserInputDecisionV1::RunCancelled, PublicRunEventKind::RunCancelled) => {
            assert_eq!(resolved.resolution, UserInputResolutionV1::RunCancelled);
            None
        }
        (UserInputDecisionV1::Declined, PublicRunEventKind::RunBlocked { reason }) => {
            assert_eq!(resolved.resolution, UserInputResolutionV1::Declined);
            assert!(!reason.is_empty());
            Some(reason.as_str())
        }
        _ => anyhow::bail!("input decision did not publish its typed terminal successor"),
    };
    let terminal_record = records
        .iter()
        .find(|record| record.event_id() == terminal_outbox.public_event_id)
        .context("terminal's exact public source record")?;
    // ApplicationClient::page is a role/text transcript, not the canonical lifecycle display.
    // Exercise the real HTTP display endpoint and bind its typed terminal to the durable source.
    let page = get_input_display(address, &session.id).await?;
    assert_eq!(page.request_scope, session.id);
    assert!(page.user_inputs.is_empty());
    assert!(page.user_input.is_none());
    assert!(!page.has_more);
    let frontier = page
        .terminal_frontier
        .as_ref()
        .context("canonical terminal frontier")?;
    assert_eq!(frontier.run_id, "http-root-input-run");
    assert_eq!(frontier.status, display_status);
    assert_eq!(
        frontier.session_stream_sequence,
        terminal_record.stream_sequence().to_string()
    );
    let terminal_items = page
        .items
        .iter()
        .filter(|item| {
            item.kind == crate::HttpConversationDisplayItemKind::Terminal
                && item.run_id.as_deref() == Some("http-root-input-run")
        })
        .collect::<Vec<_>>();
    assert_eq!(
        terminal_items
            .iter()
            .map(|item| item.status)
            .collect::<Vec<_>>(),
        vec![
            crate::HttpConversationDisplayStatus::AwaitingUserInput,
            display_status
        ],
        "immutable Awaiting history followed by its exact typed successor"
    );
    let terminal_item = terminal_items[1];
    assert_eq!(
        terminal_item.source,
        crate::HttpConversationDisplaySource::DurableRunEvent
    );
    assert_eq!(
        terminal_item.source_event_id,
        terminal_outbox.public_event_id
    );
    assert_eq!(
        terminal_item.display_order.session_stream_sequence,
        terminal_record.stream_sequence().to_string()
    );
    let crate::HttpConversationDisplayContent::Terminal {
        final_message_id,
        safe_summary,
        summary_truncated,
    } = &terminal_item.content
    else {
        anyhow::bail!("canonical terminal item must carry terminal content")
    };
    assert!(final_message_id.is_none());
    assert_eq!(safe_summary.as_deref(), expected_summary);
    assert!(!summary_truncated);
    let commit = records
        .iter()
        .find_map(|record| match record.session_log_entry() {
            Ok(Some(SessionLogEntry::Control(ControlEntry::ApplicationOperationCommittedV1(
                commit,
            )))) => Some(commit),
            _ => None,
        })
        .context("real application commit")?;
    assert!(commit.domain_events.iter().any(|entry| records.iter().any(
        |record| record.event_id() == entry.event_id
            && record.stored_event().event_kind()
                == Some(sigil_kernel::DurableEventType::PublicEventOutbox)
    )));
    assert!(
        sigil_kernel::session::reconcile_application_operation(
            &owner.owner.read_handle(),
            &commit.binding
        )?
        .is_some()
    );
    let bytes = std::fs::read(&session.session_log_path)?;
    let (status, replay) = post_input_decision(address, &path, &body).await?;
    assert_eq!(status, 200, "{replay}");
    assert_eq!(replay["replayed"], true);
    assert_eq!(std::fs::read(&session.session_log_path)?, bytes);
    let mut drifted = envelope;
    drifted.payload.decision = if matches!(decision, UserInputDecisionV1::RunCancelled) {
        UserInputDecisionV1::Declined
    } else {
        UserInputDecisionV1::RunCancelled
    };
    let (status, _) =
        post_input_decision(address, &path, &serde_json::to_string(&drifted)?).await?;
    assert_eq!(status, 409);
    assert_eq!(std::fs::read(&session.session_log_path)?, bytes);
    let catalog = driver.session_lifecycle().context("lifecycle")?.catalog()?;
    let entry = catalog
        .entries
        .iter()
        .find(|entry| entry.session_id.as_deref() == Some(&session.durable_session_scope_id))
        .context("original session in catalog")?;
    let open = crate::HttpSessionOpenRequest {
        session_ref: entry
            .session_ref
            .as_path()
            .to_str()
            .context("session ref")?
            .to_owned(),
        session_id: session.durable_session_scope_id.clone(),
        label: None,
        recovery_binding: None,
    };
    let registry_reopen = registry.clone();
    assert_eq!(
        tokio::task::spawn_blocking(move || registry_reopen.open_session(open))
            .await??
            .id,
        session.id
    );
    assert_eq!(registry.get_run("http-root-input-run")?.status, terminal);
    let reconnected = registry.application_client(&session.id, "input-after-reopen")?;
    assert!(!reconnected.refresh()?.user_input.pending);
    let start = HttpCommandEnvelope::new(
        "next-after-cancel",
        "input-after-reopen",
        &session.id,
        HttpRunStartRequest {
            prompt: "explain the existing files".to_owned(),
            image_attachments: Vec::new(),
            review_annotations: Vec::new(),
            permission_mode: Some(HttpPermissionMode::ReadOnly),
            model_ref: None,
            model_selection_binding: None,
            route_recovery_binding: None,
            reasoning_effort: None,
            reasoning_effort_binding: None,
            skill_binding: None,
            agent_binding: None,
            task_continuation: None,
        },
    );
    let registry_start = registry.clone();
    let session_id = session.id.clone();
    let admitted =
        tokio::task::spawn_blocking(move || registry_start.start_run_command(&session_id, start))
            .await??;
    assert_ne!(admitted.run.id, "http-root-input-run");
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if registry.get_run(&admitted.run.id)?.status.is_terminal()
                && driver.active_runs.lock().expect("active runs").is_empty()
            {
                break anyhow::Ok(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    let _ = shutdown.send(());
    serving.await??;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn production_serve_cancel_clarification_retires_waiting_and_accepts_next_input() -> Result<()>
{
    serve_input_terminal_retires_waiting(
        UserInputDecisionV1::RunCancelled,
        HttpRunStatus::Cancelled,
        crate::HttpConversationDisplayStatus::Cancelled,
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn production_serve_decline_clarification_blocks_without_cancelling_and_accepts_next_input()
-> Result<()> {
    serve_input_terminal_retires_waiting(
        UserInputDecisionV1::Declined,
        HttpRunStatus::Blocked,
        crate::HttpConversationDisplayStatus::Blocked,
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn production_user_input_receipt_replays_actual_root_decision_after_client_reconnect()
-> Result<()> {
    let fixture = tempfile::tempdir()?;
    let driver = production_queue_driver(&fixture, "user-input-operation");
    let registry = driver.build_registry(Arc::new(HttpDurableCommandStore::open(
        fixture.path().join("commands-user-input-operation.json"),
        16,
    )?))?;
    let session = registry.create_session(HttpSessionCreateRequest::default())?;
    let owner = driver.application_operation_owner(&session)?;
    let requested = seed_root_input_request(&owner.owner)?;
    let reader = owner.owner.read_handle();
    assert!(reader.read_event_records()?.iter().all(|record| !matches!(
        record.session_log_entry().expect("valid seeded session"),
        Some(SessionLogEntry::Control(
            ControlEntry::ApplicationOperationPreparedV1(_)
                | ControlEntry::ApplicationOperationCommittedV1(_)
        ))
    )));

    let command_id = "http-root-input-cancel";
    let command = ApplicationCommand::UserInput(UserInputCommand::Resolve {
        binding: requested.request.identity.request_id.as_str().to_owned(),
        generation: requested.request.identity.generation,
        expected_request_hash: SafeText::new(requested.request_hash.clone())?,
        decision: UserInputDecisionV1::RunCancelled,
        permission_mode: None,
    });
    let client = registry.application_client(&session.id, "root-input-client")?;
    client.refresh()?;
    let journal = client.command_journal_binding()?;
    let receipt = client.execute_in_journal(command_id, Some(journal.clone()), command.clone())?;
    let ApplicationCommandReceipt::Settled(settled) = receipt else {
        panic!("the real decision must settle from its source event: {receipt:?}");
    };
    let records = reader.read_event_records()?;
    let committed_bytes = std::fs::read(&session.session_log_path)?;
    let source = records
        .iter()
        .find(|record| record.event_id() == settled.domain_commit.source_event_id)
        .expect("receipt references a real durable source event");
    assert_eq!(
        source.stream_sequence(),
        settled.domain_commit.source_sequence
    );
    assert_eq!(
        sigil_kernel::sha256_hex(source.record_checksum().as_bytes()),
        settled.domain_commit.source_digest
    );
    let Some(SessionLogEntry::Control(ControlEntry::ApplicationOperationCommittedV1(commit))) =
        source.session_log_entry()?
    else {
        panic!("receipt must name the owning Session's causal commit marker");
    };
    assert!(matches!(
        &commit.binding.target,
        ApplicationOperationTargetV1::UserInputDecision {
            request_id,
            generation: 1,
            request_hash,
            command_id: bound_command,
        } if request_id == requested.request.identity.request_id.as_str()
            && request_hash == &requested.request_hash
            && bound_command == command_id
    ));
    let decisions = records
        .iter()
        .filter(|record| matches!(
            record.session_log_entry().expect("valid decision record"),
            Some(SessionLogEntry::Control(ControlEntry::UserInputDecisionAccepted(ref decision)))
                if decision.identity == requested.request.identity
        ))
        .collect::<Vec<_>>();
    assert_eq!(decisions.len(), 1, "one actual accepted decision");
    assert!(
        commit
            .domain_events
            .iter()
            .any(|event| event.event_id == decisions[0].event_id())
    );
    assert!(
        sigil_kernel::session::reconcile_application_operation(&reader, &commit.binding)?.is_some(),
        "the actual owner validates the decision and commit as one contiguous batch"
    );

    client.refresh()?;
    assert_eq!(
        client.execute_in_journal(command_id, Some(journal.clone()), command.clone())?,
        ApplicationCommandReceipt::Replayed(settled.clone())
    );
    drop(client);
    // A replacement transport client has no in-memory prepared command. It retains the
    // original client ID, command ID and journal binding as a response-lost retry would.
    let reconnected = registry.application_client(&session.id, "root-input-client")?;
    reconnected.refresh()?;
    assert_eq!(
        reconnected.execute_in_journal(command_id, Some(journal.clone()), command.clone())?,
        ApplicationCommandReceipt::Replayed(settled)
    );
    let mut changed = command;
    if let ApplicationCommand::UserInput(UserInputCommand::Resolve { decision, .. }) = &mut changed
    {
        *decision = UserInputDecisionV1::Declined;
    }
    assert!(matches!(
        reconnected.execute_in_journal(command_id, Some(journal), changed)?,
        ApplicationCommandReceipt::PayloadConflict(_)
    ));
    assert_eq!(
        std::fs::read(&session.session_log_path)?,
        committed_bytes,
        "retries and a changed payload cannot append a second decision or continuation"
    );
    let reopened = owner.owner.attach_for_control()?;
    let projection = reopened.user_input_projection()?;
    let state = projection
        .request(&requested.request.identity)
        .expect("the request remains durably addressable");
    assert!(matches!(
        state.decision.as_ref().expect("accepted decision").decision,
        UserInputDurableDecisionV1::RunCancelled
    ));
    assert_eq!(
        state.public_view().resolution,
        Some(UserInputResolutionV1::RunCancelled)
    );
    assert!(state.claim.is_none());
    assert!(state.continuation.is_none());
    assert!(registry.get_session(&session.id)?.run_ids.is_empty());
    assert!(driver.active_runs.lock().expect("active runs").is_empty());
    Ok(())
}
