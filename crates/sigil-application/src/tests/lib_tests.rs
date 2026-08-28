use super::*;
use std::{
    num::NonZeroUsize,
    sync::atomic::{AtomicUsize, Ordering},
};

fn scope() -> ApplicationScope {
    ApplicationScope {
        application_instance: ApplicationInstanceId::new("app").expect("valid id"),
        authenticated_subject: AuthenticatedSubject::new("subject").expect("valid id"),
        workspace: Some(WorkspaceScopeId::new("workspace").expect("valid id")),
        session: Some(SessionScopeId::new("session").expect("valid id")),
    }
}

fn snapshot() -> ProjectionSnapshotEnvelope {
    let scope = scope();
    let frontier = ApplicationFrontier {
        schema_version: APPLICATION_CONTRACT_SCHEMA_VERSION,
        scope: scope.clone(),
        writer_generation: 1,
        stream_generation: 1,
        through_sequence: 0,
        durable_cursor: "cursor-0".to_owned(),
    };
    let recovery = ResourceRecoverySurfaceContractV1 {
        schema_version: RESOURCE_RECOVERY_SURFACE_SCHEMA_VERSION,
        blocker: None,
        resource_effects: Vec::new(),
        action_envelope: None,
    };
    let projection = ApplicationProjection {
        schema_version: APPLICATION_CONTRACT_SCHEMA_VERSION,
        scope: scope.clone(),
        writer_generation: 1,
        stream_generation: 1,
        observer_generation: 9,
        frontier: frontier.clone(),
        resource_recovery: recovery,
        session: SessionSurfaceProjection {
            session_id: scope.session.clone(),
            title: SafeText::new("test").expect("valid text"),
            status: SafeText::new("idle").expect("valid text"),
        },
        conversation: ConversationSurfaceProjection {
            message_count: 0,
            latest_message: None,
        },
        run: RunSurfaceProjection {
            status: SafeText::new("idle").expect("valid text"),
            active_binding: None,
        },
        plan_task: PlanTaskSurfaceProjection {
            status: SafeText::new("none").expect("valid text"),
            action_binding: None,
        },
        agents: AgentSurfaceProjection {
            active_count: 0,
            summary: Vec::new(),
        },
        approval: ApprovalSurfaceProjection {
            pending: false,
            binding: None,
            summary: None,
        },
        user_input: UserInputSurfaceProjection {
            pending: false,
            binding: None,
            prompt: None,
        },
        capabilities: CapabilitySurfaceProjection {
            can_submit: true,
            can_cancel: false,
            can_configure: true,
        },
        configuration: ConfigurationSurfaceProjection {
            persisted_revision: 1,
            selected_route: None,
            dirty: false,
        },
        attention: AttentionSurfaceProjection { last_notice: None },
        queue: ApplicationQueueSurfaceProjection {
            generation: queue_generation(
                0,
                sigil_kernel::conversation_queue::CONVERSATION_QUEUE_INITIAL_REVISION_EVENT_ID,
            ),
            paused: false,
            items: Vec::new(),
        },
        terminal: TerminalSurfaceProjection {
            tasks: Vec::new(),
            active_task_count: 0,
            latest_task_id: None,
        },
    };
    ProjectionSnapshotEnvelope {
        schema_version: APPLICATION_CONTRACT_SCHEMA_VERSION,
        scope,
        writer_generation: 1,
        stream_generation: 1,
        observer_generation: 9,
        cut: frontier,
        projection,
    }
}

#[test]
fn fake_application_replays_exact_receipt_and_rejects_payload_conflict() {
    let app = FakeApplication::new(snapshot()).expect("valid snapshot");
    let expected = snapshot().cut;
    let command_id = ApplicationCommandId::new("command").expect("valid id");
    let request = || ApplicationCommandRequest {
        envelope: ApplicationCommandEnvelope {
            schema_version: APPLICATION_CONTRACT_SCHEMA_VERSION,
            command_id: command_id.clone(),
            correlation_id: None,
            expected_frontier: ExpectedFrontier {
                scope: expected.scope.clone(),
                writer_generation: expected.writer_generation,
                through_sequence: expected.through_sequence,
            },
            command: ApplicationCommand::Conversation(ConversationCommand::SubmitPrompt {
                prompt: Some(SafeText::new("hello").expect("valid text")),
                options: None,
            }),
        },
        admission: CommandAdmissionContext::host_bound(
            expected.scope.authenticated_subject.clone(),
            1,
            HostConnectionInstanceId::new("connection").expect("valid id"),
            expected.scope.clone(),
        )
        .expect("valid admission"),
    };
    let first = futures::executor::block_on(app.execute(request())).expect("execute");
    assert!(matches!(first, ApplicationCommandReceipt::Settled(_)));
    let replay = futures::executor::block_on(app.execute(request())).expect("replay");
    assert!(matches!(replay, ApplicationCommandReceipt::Replayed(_)));
    let mut conflicting = request();
    conflicting.envelope.command =
        ApplicationCommand::Conversation(ConversationCommand::SubmitPrompt {
            prompt: Some(SafeText::new("different").expect("valid text")),
            options: None,
        });
    assert!(matches!(
        futures::executor::block_on(app.execute(conflicting)).expect("conflict"),
        ApplicationCommandReceipt::PayloadConflict(_)
    ));
}

#[test]
fn reducer_rejects_gap_and_accepts_exact_event_chain() {
    let envelope = snapshot();
    let mut reducer = ProjectionReducer::open(envelope.clone()).expect("valid snapshot");
    let mut next = envelope.projection.clone();
    next.frontier.through_sequence = 1;
    next.frontier.durable_cursor = "cursor-1".to_owned();
    let payload = ApplicationEvent::ProjectionReplaced(Box::new(next.clone()));
    let event = ApplicationEventEnvelope {
        schema_version: APPLICATION_CONTRACT_SCHEMA_VERSION,
        scope: envelope.scope.clone(),
        writer_generation: 1,
        stream_generation: 1,
        observer_generation: 9,
        event_id: "event-1".to_owned(),
        base_frontier: envelope.cut.clone(),
        next_frontier: next.frontier.clone(),
        payload_digest: digest_event(&payload).expect("digest"),
        payload,
    };
    reducer
        .apply(ProjectionFeedItem::Event(Box::new(event)))
        .expect("exact event");
    assert_eq!(reducer.frontier().through_sequence, 1);
    assert_eq!(
        reducer.apply(ProjectionFeedItem::Gap {
            expected: 2,
            observed: 4
        }),
        Err(ApplicationError::ResetRequired)
    );
}

#[test]
fn presenter_capability_is_one_shot() {
    let broker = PresenterBroker::default();
    let session_id = PresenterSessionId::new("tui").expect("valid id");
    let session = broker.register(session_id, 42).expect("register");
    let observation = RendererNeutralPresentationObservation {
        marker_id: PresentationMarkerId::new("marker").expect("valid id"),
        content_revision: 1,
        frame_nonce: 2,
        terminal_epoch: 3,
        sink_completion_nonce: 4,
    };
    let capability = broker
        .arm(&session, observation.marker_id.clone(), 1, 3)
        .expect("arm");
    let attestation = broker
        .attest(&session, &capability, observation.clone())
        .expect("attest");
    broker.complete(attestation).expect("complete");
    assert!(broker.attest(&session, &capability, observation).is_err());
    assert!(!format!("{session:?}").contains("42"));
    assert!(!format!("{broker:?}").contains("42"));
}

#[derive(Clone)]
struct RecordingPort {
    inner: FakeApplication,
    opens: Arc<Mutex<Vec<OpenProjectionRequest>>>,
    acknowledgements: Arc<Mutex<Vec<ProjectionDeliveryAck>>>,
}

impl ApplicationPort for RecordingPort {
    fn open_projection(
        &self,
        request: OpenProjectionRequest,
    ) -> BoxFuture<'static, Result<ProjectionSnapshot, ApplicationError>> {
        let opens = Arc::clone(&self.opens);
        let inner = self.inner.clone();
        Box::pin(async move {
            opens
                .lock()
                .map_err(|_| ApplicationError::Unavailable)?
                .push(request.clone());
            inner.open_projection(request).await
        })
    }

    fn page(
        &self,
        request: ProjectionPageRequest,
    ) -> BoxFuture<'static, Result<ProjectionPage, ApplicationError>> {
        self.inner.page(request)
    }

    fn cancel_page(&self, request: PageRequestId) -> BoxFuture<'static, PageCancellationReceipt> {
        self.inner.cancel_page(request)
    }

    fn acknowledge(
        &self,
        acknowledgement: ProjectionDeliveryAck,
    ) -> BoxFuture<'static, Result<(), ApplicationError>> {
        let acknowledgements = Arc::clone(&self.acknowledgements);
        Box::pin(async move {
            acknowledgements
                .lock()
                .map_err(|_| ApplicationError::Unavailable)?
                .push(acknowledgement.clone());
            acknowledgement.validate()
        })
    }

    fn execute(
        &self,
        request: ApplicationCommandRequest,
    ) -> BoxFuture<'static, Result<ApplicationCommandReceipt, ApplicationError>> {
        self.inner.execute(request)
    }
}

#[test]
fn application_client_resumes_with_the_committed_frontier() {
    let fake = FakeApplication::new(snapshot()).expect("valid snapshot");
    let opens = Arc::new(Mutex::new(Vec::new()));
    let acknowledgements = Arc::new(Mutex::new(Vec::new()));
    let port = Arc::new(RecordingPort {
        inner: fake,
        opens: Arc::clone(&opens),
        acknowledgements,
    });
    let client = ApplicationClient::new(
        port,
        scope(),
        9,
        1,
        HostConnectionInstanceId::new("connection").expect("valid id"),
    )
    .expect("client");

    futures::executor::block_on(client.refresh()).expect("initial refresh");
    futures::executor::block_on(client.refresh()).expect("resumed refresh");

    let opens = opens.lock().expect("open log");
    assert_eq!(opens.len(), 2);
    assert!(opens[0].resume_from.is_none());
    assert_eq!(opens[1].resume_from, Some(snapshot().cut));
}

#[test]
fn application_client_acknowledges_only_reducer_commits() {
    let base = snapshot();
    let mut next_projection = base.projection.clone();
    next_projection.frontier.through_sequence = 1;
    next_projection.frontier.durable_cursor = "cursor-1".to_owned();
    let payload = ApplicationEvent::ProjectionReplaced(Box::new(next_projection.clone()));
    let feed = ProjectionFeedItem::Event(Box::new(ApplicationEventEnvelope {
        schema_version: APPLICATION_CONTRACT_SCHEMA_VERSION,
        scope: base.scope.clone(),
        writer_generation: base.writer_generation,
        stream_generation: base.stream_generation,
        observer_generation: base.observer_generation,
        event_id: "event-1".to_owned(),
        base_frontier: base.cut.clone(),
        next_frontier: next_projection.frontier.clone(),
        payload_digest: event_payload_digest(&payload).expect("digest"),
        payload,
    }));
    let port = Arc::new(FeedPort {
        snapshot: ProjectionSnapshot {
            envelope: base,
            feed: vec![feed],
        },
        acknowledgements: Arc::new(AtomicUsize::new(0)),
    });
    let acknowledgements = Arc::clone(&port.acknowledgements);
    let client = ApplicationClient::new(
        port,
        scope(),
        9,
        1,
        HostConnectionInstanceId::new("connection").expect("valid id"),
    )
    .expect("client");

    let projection = futures::executor::block_on(client.refresh()).expect("refresh");
    assert_eq!(projection.frontier.through_sequence, 1);
    assert_eq!(acknowledgements.load(Ordering::SeqCst), 1);
}

struct FeedPort {
    snapshot: ProjectionSnapshot,
    acknowledgements: Arc<AtomicUsize>,
}

impl ApplicationPort for FeedPort {
    fn open_projection(
        &self,
        _request: OpenProjectionRequest,
    ) -> BoxFuture<'static, Result<ProjectionSnapshot, ApplicationError>> {
        let snapshot = self.snapshot.clone();
        Box::pin(async move { Ok(snapshot) })
    }

    fn page(
        &self,
        _request: ProjectionPageRequest,
    ) -> BoxFuture<'static, Result<ProjectionPage, ApplicationError>> {
        Box::pin(async { Err(ApplicationError::Unavailable) })
    }

    fn cancel_page(&self, _request: PageRequestId) -> BoxFuture<'static, PageCancellationReceipt> {
        Box::pin(async { PageCancellationReceipt::UnknownRequest })
    }

    fn acknowledge(
        &self,
        _acknowledgement: ProjectionDeliveryAck,
    ) -> BoxFuture<'static, Result<(), ApplicationError>> {
        let acknowledgements = Arc::clone(&self.acknowledgements);
        Box::pin(async move {
            acknowledgements.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }

    fn execute(
        &self,
        _request: ApplicationCommandRequest,
    ) -> BoxFuture<'static, Result<ApplicationCommandReceipt, ApplicationError>> {
        Box::pin(async { Err(ApplicationError::Unavailable) })
    }
}

#[test]
fn five_surface_clients_share_frontier_page_and_domain_receipt() {
    let application = FakeApplication::new(snapshot()).expect("valid snapshot");
    let expected_frontier = snapshot().cut;
    let command_id = ApplicationCommandId::new("shared-command").expect("valid id");
    let clients = ["tui-keyboard", "tui-mouse", "desktop", "http", "cli"]
        .into_iter()
        .map(|surface| {
            ApplicationClient::new(
                Arc::new(application.clone()),
                scope(),
                9,
                1,
                HostConnectionInstanceId::new(format!("connection-{surface}")).expect("valid id"),
            )
            .expect("client")
        })
        .collect::<Vec<_>>();
    for client in &clients {
        futures::executor::block_on(client.refresh()).expect("refresh");
    }

    for (index, client) in clients.iter().enumerate() {
        let page = futures::executor::block_on(client.page(
            PageRequestId::new(format!("page-{index}")).expect("valid page request id"),
            1,
            PageQueryFingerprint::new("conversation").expect("valid query"),
            PageAnchor {
                item_id: None,
                intra_item_row: 0,
                cursor: None,
            },
            PageDirection::Older,
            NonZeroUsize::new(8).expect("non-zero page size"),
            80,
        ))
        .expect("page should use the refreshed frontier");
        assert_eq!(page.scope, expected_frontier.scope);
        assert_eq!(page.at_frontier, expected_frontier);
        assert_eq!(
            futures::executor::block_on(client.cancel_page(page.request_id.clone())),
            PageCancellationReceipt::TooLate
        );
    }

    let command = ApplicationCommand::Run(RunCommand::Cancel {
        binding: "run-1".to_owned(),
        reason: None,
    });
    let receipts = clients
        .iter()
        .map(|client| {
            futures::executor::block_on(client.execute_with_id(command_id.clone(), command.clone()))
                .expect("execute")
        })
        .collect::<Vec<_>>();
    let domains = receipts
        .iter()
        .map(|receipt| match receipt {
            ApplicationCommandReceipt::Settled(domain)
            | ApplicationCommandReceipt::Replayed(domain) => domain,
            other => panic!("unexpected receipt: {other:?}"),
        })
        .collect::<Vec<_>>();
    assert!(matches!(receipts[0], ApplicationCommandReceipt::Settled(_)));
    assert!(
        receipts[1..]
            .iter()
            .all(|receipt| matches!(receipt, ApplicationCommandReceipt::Replayed(_)))
    );
    assert!(domains.windows(2).all(|pair| pair[0] == pair[1]));
    assert!(
        domains
            .iter()
            .all(|domain| domain.frontier.scope == expected_frontier.scope)
    );
}

#[test]
fn terminal_task_cancellation_is_an_urgent_monotonic_application_command() {
    let command = ApplicationCommand::Run(RunCommand::CancelTerminalTask {
        identity: ApplicationTerminalTaskIdentity {
            session_scope_id: SafeText::new("session").expect("valid session identity"),
            run_id: SafeText::new("run").expect("valid run identity"),
            task_id: SafeText::new("terminal-task").expect("valid task identity"),
            expected_generation: 4,
        },
    });

    assert_eq!(command.kind(), "run");
    assert_eq!(
        command.policy(),
        CommandPolicy {
            lane: CommandLane::Urgent,
            settlement: EffectSettlementClass::MonotonicControl,
            requires_session: true,
        }
    );
    let encoded = serde_json::to_vec(&command).expect("terminal cancel should serialize");
    let decoded: ApplicationCommand =
        serde_json::from_slice(&encoded).expect("terminal cancel should deserialize");
    assert_eq!(decoded, command);
}
