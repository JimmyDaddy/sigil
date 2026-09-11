use super::*;
use std::{collections::VecDeque, sync::Arc, sync::Mutex};

use futures::future::BoxFuture;
use sigil_application::*;

fn unavailable() -> ProjectionFailure {
    ProjectionFailure::Application(ApplicationError::Unavailable)
}

#[test]
fn projection_retry_deadline_keeps_ready_ui_waking_without_early_attempts() {
    let now = Instant::now();
    let mut retry = ProjectionRetry::new(7);
    assert!(retry.can_start(7, now));
    let action = retry.failed(7, now, &unavailable()).expect("current epoch");
    assert_eq!(
        action,
        FailureAction {
            retry: true,
            notify: false
        }
    );
    assert_eq!(
        projection_wake_deadline(None, retry.remaining(now), None),
        Some(Duration::from_millis(250))
    );
    // Repeated worker wakeups do not reset or bypass the already-scheduled retry.
    for elapsed in [0, 1, 120, 249] {
        assert!(!retry.can_start(7, now + Duration::from_millis(elapsed)));
    }
    assert!(retry.can_start(7, now + Duration::from_millis(250)));
    assert_eq!(
        projection_wake_deadline(
            Some(Duration::from_millis(120)),
            retry.remaining(now),
            Some(Duration::from_millis(500)),
        ),
        Some(Duration::from_millis(120))
    );
}

#[test]
fn projection_retry_backoff_is_capped_and_reports_prolonged_failure_and_recovery_once() {
    let mut now = Instant::now();
    let mut retry = ProjectionRetry::new(1);
    let mut notices = 0;
    for delay_ms in [250, 500, 1_000, 2_000, 2_000, 2_000] {
        let action = retry.failed(1, now, &unavailable()).expect("same epoch");
        assert!(action.retry);
        notices += usize::from(action.notify);
        assert_eq!(retry.remaining(now), Some(Duration::from_millis(delay_ms)));
        assert!(!retry.can_start(1, now));
        now += Duration::from_millis(delay_ms);
    }
    assert_eq!(notices, 1);
    assert!(retry.succeeded(1));
    assert!(!retry.succeeded(1));
    assert_eq!(retry.remaining(now), None);
    assert_eq!(
        projection_wake_deadline(None, retry.remaining(now), None),
        None
    );
    assert_eq!(
        retry.failed(1, now, &unavailable()).expect("new failure"),
        FailureAction {
            retry: true,
            notify: false
        }
    );
    assert_eq!(retry.remaining(now), Some(Duration::from_millis(250)));
}

#[test]
fn projection_retry_does_not_retry_permanent_or_task_failures_and_reports_them_once() {
    for error in [
        ApplicationError::UnknownSchema(99),
        ApplicationError::ScopeMismatch,
        ApplicationError::ScopeRequired,
        ApplicationError::ResetRequired,
        ApplicationError::CorruptProjection("bad digest".to_owned()),
        ApplicationError::InvalidRequest("invalid cut".to_owned()),
        ApplicationError::NotFound,
    ] {
        let now = Instant::now();
        let mut retry = ProjectionRetry::new(1);
        retry.failed(1, now, &unavailable());
        let error = ProjectionFailure::Application(error);
        let action = retry.failed(1, now, &error).expect("current epoch");
        assert_eq!(
            action,
            FailureAction {
                retry: false,
                notify: true
            }
        );
        assert_eq!(retry.remaining(now), None);
        assert_eq!(
            retry
                .failed(1, now, &error)
                .expect("repeated permanent error"),
            FailureAction {
                retry: false,
                notify: false
            }
        );
        assert!(!error.to_string().is_empty());
    }
    let mut retry = ProjectionRetry::new(1);
    let error = ProjectionFailure::Task("projection task panicked".to_owned());
    assert_eq!(
        retry.failed(1, Instant::now(), &error),
        Some(FailureAction {
            retry: false,
            notify: true
        })
    );
    assert_eq!(error.to_string(), "projection task panicked");
}

#[test]
fn projection_retry_epoch_change_discards_old_refresh_and_ack_scheduling() {
    let now = Instant::now();
    let mut refresh = ProjectionRetry::new(1);
    let mut ack = ProjectionRetry::new(1);
    refresh.failed(1, now, &unavailable());
    ack.failed(1, now, &unavailable());
    refresh.reset(2);
    ack.reset(2);
    for retry in [&mut refresh, &mut ack] {
        assert_eq!(retry.failed(1, now, &unavailable()), None);
        assert!(!retry.succeeded(1));
        assert!(!retry.can_start(1, now));
        assert!(retry.can_start(2, now));
        assert_eq!(retry.remaining(now), None);
    }
    assert!(!should_replace_pending_ack(2, None, 1, &frontier(10)));
    assert_eq!(
        projection_wake_deadline(None, refresh.remaining(now), ack.remaining(now)),
        None
    );
}

#[test]
fn projection_owner_reboot_resets_retries_and_rejects_old_in_flight_results() {
    let now = Instant::now();
    let old_owner = Arc::new("same-session");
    let new_owner = Arc::new("same-session");
    assert_eq!(*old_owner, *new_owner);
    assert!(!Arc::ptr_eq(&old_owner, &new_owner));
    let old_epoch = 7;
    let mut epoch = old_epoch;
    let mut owner = Some(old_owner);
    let mut refresh = ProjectionRetry::new(epoch);
    let mut acknowledgement = ProjectionRetry::new(epoch);
    for retry in [&mut refresh, &mut acknowledgement] {
        retry.failed(epoch, now, &unavailable());
        retry.failed(epoch, now + Duration::from_millis(250), &unavailable());
        assert_eq!(
            retry.remaining(now + Duration::from_millis(250)),
            Some(Duration::from_millis(500))
        );
    }

    assert!(reconcile_projection_owner(
        &mut owner,
        Some(Arc::clone(&new_owner)),
        &mut epoch,
        &mut refresh,
        &mut acknowledgement,
    ));
    assert_eq!(epoch, 8);
    assert!(Arc::ptr_eq(owner.as_ref().expect("new owner"), &new_owner));
    for retry in [&refresh, &acknowledgement] {
        assert_eq!(retry.remaining(now), None);
        assert!(retry.can_start(epoch, now));
        assert!(!retry.can_start(old_epoch, now));
    }

    let old_frontier = frontier(20);
    let mut new_frontier = frontier(1);
    new_frontier.writer_generation += 1;
    let mut pending_ack = Some((old_epoch, old_frontier.clone()));
    assert!(should_replace_pending_ack(
        epoch,
        pending_ack.as_ref().map(|(epoch, cut)| (*epoch, cut)),
        epoch,
        &new_frontier,
    ));
    pending_ack = Some((epoch, new_frontier.clone()));

    let retry_now = now + Duration::from_secs(1);
    for retry in [&mut refresh, &mut acknowledgement] {
        assert!(
            retry
                .failed(epoch, retry_now, &unavailable())
                .expect("new epoch")
                .retry
        );
        assert_eq!(retry.remaining(retry_now), Some(Duration::from_millis(250)));
        assert_eq!(retry.failed(old_epoch, retry_now, &unavailable()), None);
        assert!(!retry.succeeded(old_epoch));
        assert_eq!(retry.remaining(retry_now), Some(Duration::from_millis(250)));
        assert!(!retry.can_start(epoch, retry_now + Duration::from_millis(249)));
        assert!(retry.can_start(epoch, retry_now + Duration::from_millis(250)));
    }
    if should_replace_pending_ack(
        epoch,
        pending_ack.as_ref().map(|(epoch, cut)| (*epoch, cut)),
        old_epoch,
        &old_frontier,
    ) {
        pending_ack = Some((old_epoch, old_frontier));
    }
    assert_eq!(pending_ack, Some((epoch, new_frontier)));
    assert!(!should_replace_pending_ack(
        epoch,
        None,
        old_epoch,
        &frontier(30)
    ));
}

#[test]
fn projection_owner_clone_preserves_deadlines_and_presence_changes_reset_once() {
    let now = Instant::now();
    let original_owner = Arc::new("same-session");
    let mut owner = Some(Arc::clone(&original_owner));
    let mut epoch = u64::MAX;
    let mut refresh = ProjectionRetry::new(epoch);
    let mut acknowledgement = ProjectionRetry::new(epoch);
    refresh.failed(epoch, now, &unavailable());
    acknowledgement.failed(epoch, now, &unavailable());

    assert!(!reconcile_projection_owner(
        &mut owner,
        Some(Arc::clone(&original_owner)),
        &mut epoch,
        &mut refresh,
        &mut acknowledgement,
    ));
    assert_eq!(epoch, u64::MAX);
    assert!(Arc::ptr_eq(
        owner.as_ref().expect("same owner"),
        &original_owner
    ));
    for retry in [&refresh, &acknowledgement] {
        assert_eq!(
            retry.remaining(now + Duration::from_millis(50)),
            Some(Duration::from_millis(200))
        );
        assert!(!retry.can_start(epoch, now + Duration::from_millis(249)));
    }

    assert!(reconcile_projection_owner(
        &mut owner,
        None,
        &mut epoch,
        &mut refresh,
        &mut acknowledgement,
    ));
    assert!(owner.is_none());
    assert_eq!(epoch, 1);
    for retry in [&refresh, &acknowledgement] {
        assert_eq!(retry.remaining(now), None);
        assert!(retry.can_start(epoch, now));
        assert!(!retry.can_start(u64::MAX, now));
    }
    assert!(!reconcile_projection_owner(
        &mut owner,
        None,
        &mut epoch,
        &mut refresh,
        &mut acknowledgement,
    ));
    assert_eq!(epoch, 1);
    assert!(owner.is_none());

    assert!(reconcile_projection_owner(
        &mut owner,
        Some(Arc::clone(&original_owner)),
        &mut epoch,
        &mut refresh,
        &mut acknowledgement,
    ));
    assert_eq!(epoch, 2);
    assert!(Arc::ptr_eq(
        owner.as_ref().expect("reconnected owner"),
        &original_owner
    ));
    for retry in [&refresh, &acknowledgement] {
        assert_eq!(retry.remaining(now), None);
        assert!(retry.can_start(epoch, now));
        assert!(!retry.can_start(1, now));
    }
}

#[test]
fn projection_retry_ack_preserves_the_newer_applied_cut_and_rejects_foreign_owners() {
    let old = frontier(10);
    let newer = frontier(20);
    assert!(!should_replace_pending_ack(1, Some((1, &newer)), 1, &old));
    assert!(should_replace_pending_ack(1, Some((1, &old)), 1, &newer));
    assert!(should_replace_pending_ack(1, None, 1, &old));
    assert!(should_replace_pending_ack(2, Some((1, &old)), 2, &newer));
    let mut foreign = newer.clone();
    foreign.scope.session = Some(SessionScopeId::new("other-session").expect("session"));
    assert!(!should_replace_pending_ack(1, Some((1, &old)), 1, &foreign));
    foreign = newer.clone();
    foreign.writer_generation += 1;
    assert!(!should_replace_pending_ack(1, Some((1, &old)), 1, &foreign));
    foreign = newer;
    foreign.stream_generation += 1;
    assert!(!should_replace_pending_ack(1, Some((1, &old)), 1, &foreign));
    let now = Instant::now();
    let mut retry = ProjectionRetry::new(1);
    retry.failed(1, now, &unavailable());
    assert_eq!(
        projection_wake_deadline(None, None, retry.remaining(now)),
        Some(Duration::from_millis(250))
    );
    assert!(!retry.can_start(1, now));
}

struct TerminalProjectionPort {
    responses: Mutex<VecDeque<Result<ProjectionSnapshot, ApplicationError>>>,
    opens: Mutex<Vec<OpenProjectionRequest>>,
    acknowledgements: Mutex<Vec<ProjectionDeliveryAck>>,
}

impl ApplicationPort for TerminalProjectionPort {
    fn open_projection(
        &self,
        request: OpenProjectionRequest,
    ) -> BoxFuture<'static, Result<ProjectionSnapshot, ApplicationError>> {
        self.opens.lock().expect("open log").push(request);
        let response = self
            .responses
            .lock()
            .expect("response queue")
            .pop_front()
            .expect("unexpected projection refresh");
        Box::pin(async move { response })
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
        acknowledgement: ProjectionDeliveryAck,
    ) -> BoxFuture<'static, Result<(), ApplicationError>> {
        self.acknowledgements
            .lock()
            .expect("ACK log")
            .push(acknowledgement);
        Box::pin(async { Ok(()) })
    }

    fn execute(
        &self,
        _request: ApplicationCommandRequest,
    ) -> BoxFuture<'static, Result<ApplicationCommandReceipt, ApplicationError>> {
        panic!("projection recovery must not dispatch commands")
    }
}

#[test]
fn projection_retry_terminal_refresh_recovers_without_another_worker_message_or_fake_ack() {
    let initial = snapshot(10, "running");
    let terminal = snapshot(11, "completed");
    let payload = ApplicationEvent::ProjectionReplaced(Box::new(terminal.projection.clone()));
    let event = ApplicationEventEnvelope {
        schema_version: APPLICATION_CONTRACT_SCHEMA_VERSION,
        scope: initial.scope.clone(),
        writer_generation: 1,
        stream_generation: 1,
        observer_generation: 1,
        event_id: "actual-public-terminal".to_owned(),
        base_frontier: initial.cut.clone(),
        next_frontier: terminal.cut.clone(),
        payload_digest: event_payload_digest(&payload).expect("payload digest"),
        payload,
        delivery_event_ids: Vec::new(),
    };
    let port = Arc::new(TerminalProjectionPort {
        responses: Mutex::new(VecDeque::from([
            Ok(ProjectionSnapshot {
                envelope: initial.clone(),
                feed: Vec::new(),
            }),
            Err(ApplicationError::Unavailable),
            Ok(ProjectionSnapshot {
                envelope: initial.clone(),
                feed: vec![ProjectionFeedItem::Event(Box::new(event))],
            }),
        ])),
        opens: Mutex::new(Vec::new()),
        acknowledgements: Mutex::new(Vec::new()),
    });
    let application_port: Arc<dyn ApplicationPort> = port.clone();
    let client = ApplicationClient::new(
        application_port,
        initial.scope.clone(),
        1,
        9,
        HostConnectionInstanceId::new("original-worker").expect("connection"),
    )
    .expect("client");
    futures::executor::block_on(client.refresh()).expect("initial projection");
    let now = Instant::now();
    let mut retry = ProjectionRetry::new(1);
    let error = match futures::executor::block_on(client.refresh()) {
        Ok(_) => panic!("first terminal refresh must fail"),
        Err(error) => ProjectionFailure::Application(error),
    };
    let pending = retry.failed(1, now, &error).expect("current epoch").retry;
    assert!(pending);
    assert_eq!(
        client.current_frontier().expect("frontier"),
        Some(initial.cut.clone())
    );
    assert!(port.acknowledgements.lock().expect("ACK log").is_empty());
    assert!(!retry.can_start(1, now + Duration::from_millis(249)));
    assert_eq!(port.opens.lock().expect("open log").len(), 2);

    // Only the retry deadline advances. No worker event or user command is delivered.
    let wake = projection_wake_deadline(None, retry.remaining(now), None).expect("idle retry wake");
    assert!(pending && retry.can_start(1, now + wake));
    let completed = futures::executor::block_on(client.refresh()).expect("terminal retry");
    retry.succeeded(1);
    assert_eq!(completed, terminal.projection);
    assert_eq!(
        client.current_frontier().expect("frontier"),
        Some(terminal.cut.clone())
    );
    assert_eq!(
        *port.acknowledgements.lock().expect("ACK log"),
        vec![ProjectionDeliveryAck {
            scope: initial.scope,
            observer_generation: 1,
            event_id: "actual-public-terminal".to_owned(),
            frontier: terminal.cut,
        }]
    );
    let opens = port.opens.lock().expect("open log");
    assert_eq!(opens[1].resume_from, Some(initial.cut.clone()));
    assert_eq!(opens[2].resume_from, Some(initial.cut));
    assert_eq!(retry.remaining(now + wake), None);
}

fn frontier(sequence: u64) -> ApplicationFrontier {
    ApplicationFrontier {
        schema_version: APPLICATION_CONTRACT_SCHEMA_VERSION,
        scope: ApplicationScope {
            application_instance: ApplicationInstanceId::new("test-app").expect("app"),
            authenticated_subject: AuthenticatedSubject::new("test-user").expect("subject"),
            workspace: Some(WorkspaceScopeId::new("test-workspace").expect("workspace")),
            session: Some(SessionScopeId::new("test-session").expect("session")),
        },
        writer_generation: 1,
        stream_generation: 1,
        through_sequence: sequence,
        durable_cursor: format!("session-stream:{sequence}"),
    }
}

fn snapshot(sequence: u64, status: &str) -> ProjectionSnapshotEnvelope {
    let cut = frontier(sequence);
    let scope = cut.scope.clone();
    let projection = ApplicationProjection {
        schema_version: APPLICATION_CONTRACT_SCHEMA_VERSION,
        scope: scope.clone(),
        writer_generation: 1,
        stream_generation: 1,
        observer_generation: 1,
        frontier: cut.clone(),
        resource_recovery: ResourceRecoverySurfaceContractV1 {
            schema_version: RESOURCE_RECOVERY_SURFACE_SCHEMA_VERSION,
            blocker: None,
            resource_effects: Vec::new(),
            action_envelope: None,
        },
        session: SessionSurfaceProjection {
            session_id: scope.session.clone(),
            title: SafeText::new("fixture").expect("title"),
            status: SafeText::new(status).expect("status"),
        },
        conversation: ConversationSurfaceProjection {
            message_count: 1,
            latest_message: None,
        },
        run: RunSurfaceProjection {
            status: SafeText::new(status).expect("run status"),
            active_binding: None,
        },
        plan_task: PlanTaskSurfaceProjection {
            status: SafeText::new("none").expect("plan status"),
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
            generation: SafeText::new("fixture-queue").expect("queue generation"),
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
        observer_generation: 1,
        cut,
        projection,
    }
}
