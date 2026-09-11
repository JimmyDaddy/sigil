//! Adapter boundary regressions: attachment ownership and renderer-consumed delivery.
use super::*;
use futures::{executor::block_on, future::BoxFuture};
use sigil_application::*;

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
            generation: SafeText::new("queue-generation").expect("queue generation"),
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

struct Port {
    application: FakeApplication,
    snapshot: Mutex<ProjectionSnapshot>,
    commands: Mutex<Vec<ApplicationCommandRequest>>,
    deliveries: Mutex<Vec<DurableDeliveryRequest>>,
}
impl Port {
    fn new() -> Self {
        Self {
            application: FakeApplication::new(snapshot()).expect("application"),
            snapshot: Mutex::new(ProjectionSnapshot {
                envelope: snapshot(),
                feed: Vec::new(),
            }),
            commands: Mutex::new(Vec::new()),
            deliveries: Mutex::new(Vec::new()),
        }
    }
    fn advance(&self, public_id: &str) {
        let mut current = self.snapshot.lock().expect("snapshot");
        let base = current.envelope.clone();
        let mut next = base.projection.clone();
        next.frontier.through_sequence += 1;
        next.frontier.durable_cursor = format!("cursor-{}", next.frontier.through_sequence);
        let payload = ApplicationEvent::ProjectionReplaced(Box::new(next.clone()));
        current.feed = vec![ProjectionFeedItem::Event(Box::new(
            ApplicationEventEnvelope {
                schema_version: APPLICATION_CONTRACT_SCHEMA_VERSION,
                scope: base.scope,
                writer_generation: base.writer_generation,
                stream_generation: base.stream_generation,
                observer_generation: base.observer_generation,
                event_id: format!("event-{}", next.frontier.through_sequence),
                base_frontier: base.cut,
                next_frontier: next.frontier,
                payload_digest: event_payload_digest(&payload).expect("digest"),
                payload,
                delivery_event_ids: vec![public_id.to_owned()],
            },
        ))];
    }
}
impl ApplicationPort for Port {
    fn open_projection(
        &self,
        _request: OpenProjectionRequest,
    ) -> BoxFuture<'static, Result<ProjectionSnapshot, ApplicationError>> {
        let result = self
            .snapshot
            .lock()
            .map(|value| value.clone())
            .map_err(|_| ApplicationError::Unavailable);
        Box::pin(async move { result })
    }
    fn delivery_batch(
        &self,
        request: DurableDeliveryRequest,
    ) -> BoxFuture<'static, Result<DurableDeliveryBatch, ApplicationError>> {
        self.deliveries
            .lock()
            .expect("deliveries")
            .push(request.clone());
        Box::pin(async move {
            Ok(DurableDeliveryBatch {
                through_sequence: request.frontier.through_sequence,
                request,
                events: Vec::new(),
                has_more: false,
            })
        })
    }
    fn page(
        &self,
        request: ProjectionPageRequest,
    ) -> BoxFuture<'static, Result<ProjectionPage, ApplicationError>> {
        self.application.page(request)
    }
    fn cancel_page(&self, request: PageRequestId) -> BoxFuture<'static, PageCancellationReceipt> {
        self.application.cancel_page(request)
    }
    fn acknowledge(
        &self,
        acknowledgement: ProjectionDeliveryAck,
    ) -> BoxFuture<'static, Result<(), ApplicationError>> {
        self.application.acknowledge(acknowledgement)
    }
    fn execute(
        &self,
        request: ApplicationCommandRequest,
    ) -> BoxFuture<'static, Result<ApplicationCommandReceipt, ApplicationError>> {
        self.commands
            .lock()
            .expect("commands")
            .push(request.clone());
        self.application.execute(request)
    }
}
fn adapter(port: Arc<Port>, connection: &str) -> TuiApplicationAdapter {
    TuiApplicationAdapter::from_port(
        port,
        scope(),
        9,
        1,
        HostConnectionInstanceId::new(connection).expect("connection"),
    )
    .expect("adapter")
}

#[test]
fn prepared_retry_preserves_original_envelope_after_projection_advance() {
    let port = Arc::new(Port::new());
    let adapter = adapter(Arc::clone(&port), "attachment");
    block_on(adapter.refresh()).expect("initial view");
    let request = adapter
        .prepare_command(
            ApplicationCommandId::new("same-interaction").expect("id"),
            ApplicationCommand::Conversation(ConversationCommand::SubmitPrompt {
                prompt: Some(SafeText::new("continue the task").expect("prompt")),
                options: None,
            }),
        )
        .expect("prepare");
    assert!(matches!(
        block_on(adapter.execute_prepared(request.clone())).expect("first"),
        ApplicationCommandReceipt::Settled(_)
    ));
    port.advance("delivery-1");
    block_on(adapter.refresh()).expect("new projection");
    assert_eq!(
        adapter
            .current_projection()
            .expect("projection")
            .expect("loaded")
            .frontier
            .through_sequence,
        1
    );
    assert!(matches!(
        block_on(adapter.execute_prepared(request.clone())).expect("retry"),
        ApplicationCommandReceipt::Replayed(_)
    ));
    let commands = port.commands.lock().expect("commands");
    assert_eq!(commands.as_slice(), &[request.clone(), request]);
}

#[test]
fn foreign_attachment_cannot_dispatch_a_retained_command() {
    let port = Arc::new(Port::new());
    let first = adapter(Arc::clone(&port), "old-attachment");
    let replacement = adapter(Arc::clone(&port), "new-attachment");
    block_on(first.refresh()).expect("initial view");
    let request = first
        .prepare_command(
            ApplicationCommandId::new("old-interaction").expect("id"),
            ApplicationCommand::Conversation(ConversationCommand::SubmitPrompt {
                prompt: Some(SafeText::new("continue").expect("prompt")),
                options: None,
            }),
        )
        .expect("prepare");
    assert_eq!(
        block_on(replacement.execute_prepared(request)),
        Err(ApplicationError::ScopeMismatch)
    );
    assert!(
        port.commands.lock().expect("commands").is_empty(),
        "foreign envelope never reaches host dispatch"
    );
}

#[test]
fn delivery_waits_for_explicit_renderer_consumption_and_drains_once() {
    let port = Arc::new(Port::new());
    let adapter = adapter(Arc::clone(&port), "attachment");
    assert!(
        adapter
            .take_applied_delivery_event_ids()
            .expect("no delivery")
            .is_empty()
    );
    port.advance("visible-terminal-event");
    block_on(adapter.refresh()).expect("terminal projection applied");
    assert!(
        matches!(
            block_on(adapter.refresh_delivery()),
            Err(ApplicationError::Unavailable)
        ),
        "another delivery batch cannot overwrite an unconsumed renderer batch"
    );
    assert!(port.deliveries.lock().expect("deliveries").is_empty());
    assert_eq!(
        adapter
            .take_applied_delivery_event_ids()
            .expect("renderer accepted projection"),
        vec!["visible-terminal-event"]
    );
    assert!(
        adapter
            .take_applied_delivery_event_ids()
            .expect("drained once")
            .is_empty()
    );
    let batch = block_on(adapter.refresh_delivery()).expect("next independent delivery fetch");
    assert!(batch.notices.is_empty());
    assert_eq!(port.deliveries.lock().expect("deliveries").len(), 1);
    assert!(
        adapter
            .take_applied_delivery_event_ids()
            .expect("empty next delivery")
            .is_empty()
    );
}
