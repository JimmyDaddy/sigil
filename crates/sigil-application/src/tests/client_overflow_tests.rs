use super::*;
use std::collections::VecDeque;

struct SequencedProjectionPort {
    snapshots: Mutex<VecDeque<Result<ProjectionSnapshot, ApplicationError>>>,
    opens: Mutex<Vec<OpenProjectionRequest>>,
    acknowledgements: Mutex<Vec<ProjectionDeliveryAck>>,
    commands: Mutex<Vec<ApplicationCommandRequest>>,
}

impl ApplicationPort for SequencedProjectionPort {
    fn open_projection(
        &self,
        request: OpenProjectionRequest,
    ) -> BoxFuture<'static, Result<ProjectionSnapshot, ApplicationError>> {
        let result = (|| {
            self.opens
                .lock()
                .map_err(|_| ApplicationError::Unavailable)?
                .push(request);
            self.snapshots
                .lock()
                .map_err(|_| ApplicationError::Unavailable)?
                .pop_front()
                .unwrap_or(Err(ApplicationError::Unavailable))
        })();
        Box::pin(async move { result })
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
        let result = (|| {
            acknowledgement.validate()?;
            self.acknowledgements
                .lock()
                .map_err(|_| ApplicationError::Unavailable)?
                .push(acknowledgement);
            Ok(())
        })();
        Box::pin(async move { result })
    }

    fn execute(
        &self,
        request: ApplicationCommandRequest,
    ) -> BoxFuture<'static, Result<ApplicationCommandReceipt, ApplicationError>> {
        let result = self
            .commands
            .lock()
            .map_err(|_| ApplicationError::Unavailable)
            .map(|mut commands| commands.push(request));
        Box::pin(async move {
            result?;
            Err(ApplicationError::Unavailable)
        })
    }
}

fn snapshot_at(sequence: u64) -> ProjectionSnapshot {
    let mut envelope = snapshot();
    envelope.cut.through_sequence = sequence;
    envelope.cut.durable_cursor = format!("cursor-{sequence}");
    envelope.projection.frontier = envelope.cut.clone();
    envelope.projection.conversation.message_count = sequence;
    ProjectionSnapshot {
        envelope,
        feed: Vec::new(),
    }
}

fn overflow_at(sequence: u64) -> ProjectionSnapshot {
    let mut snapshot = snapshot_at(sequence);
    snapshot.feed.push(ProjectionFeedItem::ResetRequired {
        reason: "projection-feed-overflow",
    });
    snapshot
}

fn event_after(sequence: u64) -> ProjectionFeedItem {
    let base = snapshot_at(sequence).envelope;
    let next = snapshot_at(sequence + 1).envelope;
    let payload = ApplicationEvent::ProjectionReplaced(Box::new(next.projection));
    ProjectionFeedItem::Event(Box::new(ApplicationEventEnvelope {
        schema_version: APPLICATION_CONTRACT_SCHEMA_VERSION,
        scope: base.scope,
        writer_generation: base.writer_generation,
        stream_generation: base.stream_generation,
        observer_generation: base.observer_generation,
        event_id: format!("event-{}", sequence + 1),
        base_frontier: base.cut,
        next_frontier: next.cut,
        payload_digest: event_payload_digest(&payload).expect("event digest"),
        payload,
        delivery_event_ids: Vec::new(),
    }))
}

fn sequenced_client(
    snapshots: Vec<Result<ProjectionSnapshot, ApplicationError>>,
) -> (ApplicationClient, Arc<SequencedProjectionPort>) {
    let port = Arc::new(SequencedProjectionPort {
        snapshots: Mutex::new(snapshots.into()),
        opens: Mutex::new(Vec::new()),
        acknowledgements: Mutex::new(Vec::new()),
        commands: Mutex::new(Vec::new()),
    });
    let application_port: Arc<dyn ApplicationPort> = port.clone();
    let client = ApplicationClient::new(
        application_port,
        scope(),
        9,
        42,
        HostConnectionInstanceId::new("original-connection").expect("valid connection"),
    )
    .expect("client");
    (client, port)
}

fn assert_previous_cut_untouched(
    client: &ApplicationClient,
    port: &SequencedProjectionPort,
    expected_opens: usize,
) {
    assert_eq!(
        client.current_projection().expect("projection state"),
        Some(snapshot_at(10).envelope.projection)
    );
    assert_eq!(
        client.current_frontier().expect("frontier state"),
        Some(snapshot_at(10).envelope.cut)
    );
    assert!(port.acknowledgements.lock().expect("ack log").is_empty());
    assert_eq!(port.opens.lock().expect("open log").len(), expected_opens);
}

#[test]
fn application_client_overflow_resnapshot_preserves_identity_and_resumes_event_acknowledgements() {
    let mut delta = snapshot_at(300);
    delta.feed.push(event_after(300));
    let (client, port) = sequenced_client(vec![
        Ok(snapshot_at(10)),
        Ok(overflow_at(10)),
        Ok(snapshot_at(300)),
        Ok(delta),
    ]);
    futures::executor::block_on(client.refresh()).expect("initial refresh");
    let command_id = ApplicationCommandId::new("retained-command").expect("command id");
    let command = ApplicationCommand::Run(RunCommand::Cancel {
        binding: "run-1".to_owned(),
        reason: None,
    });
    assert_eq!(
        futures::executor::block_on(client.execute_with_id(command_id.clone(), command.clone())),
        Err(ApplicationError::Unavailable)
    );

    assert_eq!(
        futures::executor::block_on(client.refresh()).expect("overflow recovery"),
        snapshot_at(300).envelope.projection
    );
    assert!(port.acknowledgements.lock().expect("ack log").is_empty());
    assert_eq!(client.client_epoch(), 42);
    assert_eq!(client.observer_generation(), 9);
    assert_eq!(client.scope(), &scope());
    assert_eq!(
        futures::executor::block_on(client.execute_with_id(command_id.clone(), command)),
        Err(ApplicationError::Unavailable)
    );
    let commands = port.commands.lock().expect("command log");
    assert_eq!(commands.len(), 2);
    assert_eq!(commands[0].admission, commands[1].admission);
    assert_eq!(
        commands[0].admission.reservation_key(&command_id),
        commands[1].admission.reservation_key(&command_id)
    );
    assert_eq!(commands[0].envelope.expected_frontier.through_sequence, 10);
    assert_eq!(commands[1].envelope.expected_frontier.through_sequence, 300);
    drop(commands);

    assert_eq!(
        futures::executor::block_on(client.refresh()).expect("next incremental refresh"),
        snapshot_at(301).envelope.projection
    );
    assert_eq!(
        *port.acknowledgements.lock().expect("ack log"),
        vec![ProjectionDeliveryAck {
            scope: scope(),
            observer_generation: 9,
            event_id: "event-301".to_owned(),
            frontier: snapshot_at(301).envelope.cut,
        }]
    );
    let opens = port.opens.lock().expect("open log");
    assert_eq!(
        opens
            .iter()
            .map(|open| open.resume_from.clone())
            .collect::<Vec<_>>(),
        vec![
            None,
            Some(snapshot_at(10).envelope.cut),
            None,
            Some(snapshot_at(300).envelope.cut)
        ]
    );
    assert!(
        opens
            .iter()
            .all(|open| open.scope == scope() && open.observer_generation == 9)
    );
}

#[test]
fn application_client_overflow_rejects_invalid_fresh_identity_or_cut_without_commit_or_ack() {
    let mut variants = Vec::new();
    let mut wrong_scope = snapshot_at(300);
    let mut other_scope = scope();
    other_scope.session = Some(SessionScopeId::new("other-session").expect("session id"));
    wrong_scope.envelope.scope = other_scope.clone();
    wrong_scope.envelope.cut.scope = other_scope.clone();
    wrong_scope.envelope.projection.scope = other_scope.clone();
    wrong_scope.envelope.projection.frontier.scope = other_scope.clone();
    wrong_scope.envelope.projection.session.session_id = other_scope.session;
    variants.push(("scope", wrong_scope, ApplicationError::ScopeMismatch));
    let mut wrong_observer = snapshot_at(300);
    wrong_observer.envelope.observer_generation = 10;
    wrong_observer.envelope.projection.observer_generation = 10;
    variants.push(("observer", wrong_observer, ApplicationError::ScopeMismatch));
    let mut wrong_writer = snapshot_at(300);
    wrong_writer.envelope.writer_generation = 2;
    wrong_writer.envelope.cut.writer_generation = 2;
    wrong_writer.envelope.projection.writer_generation = 2;
    wrong_writer.envelope.projection.frontier.writer_generation = 2;
    variants.push(("writer", wrong_writer, ApplicationError::ResetRequired));
    let mut wrong_stream = snapshot_at(300);
    wrong_stream.envelope.stream_generation = 2;
    wrong_stream.envelope.cut.stream_generation = 2;
    wrong_stream.envelope.projection.stream_generation = 2;
    wrong_stream.envelope.projection.frontier.stream_generation = 2;
    variants.push(("stream", wrong_stream, ApplicationError::ResetRequired));
    variants.push(("equal", snapshot_at(10), ApplicationError::ResetRequired));
    variants.push(("backward", snapshot_at(9), ApplicationError::ResetRequired));
    let mut wrong_schema = snapshot_at(300);
    wrong_schema.envelope.projection.schema_version += 1;
    variants.push((
        "schema",
        wrong_schema,
        ApplicationError::UnknownSchema(APPLICATION_CONTRACT_SCHEMA_VERSION + 1),
    ));
    let mut wrong_cut_schema = snapshot_at(300);
    wrong_cut_schema.envelope.cut.schema_version += 1;
    wrong_cut_schema.envelope.projection.frontier.schema_version += 1;
    variants.push((
        "cut schema",
        wrong_cut_schema,
        ApplicationError::ResetRequired,
    ));

    for (name, fresh, expected) in variants {
        let (client, port) =
            sequenced_client(vec![Ok(snapshot_at(10)), Ok(overflow_at(10)), Ok(fresh)]);
        futures::executor::block_on(client.refresh()).expect("initial refresh");
        assert_eq!(
            futures::executor::block_on(client.refresh()),
            Err(expected),
            "{name}"
        );
        assert_previous_cut_untouched(&client, &port, 3);
    }
}

#[test]
fn application_client_overflow_rejects_corrupt_fresh_envelope_without_commit_or_ack() {
    let mut fresh = snapshot_at(300);
    fresh.envelope.projection.frontier.durable_cursor = "inconsistent-cursor".to_owned();
    let (client, port) =
        sequenced_client(vec![Ok(snapshot_at(10)), Ok(overflow_at(10)), Ok(fresh)]);
    futures::executor::block_on(client.refresh()).expect("initial refresh");
    assert!(matches!(
        futures::executor::block_on(client.refresh()),
        Err(ApplicationError::CorruptProjection(_))
    ));
    assert_previous_cut_untouched(&client, &port, 3);
}

#[test]
fn application_client_overflow_rejects_fresh_feed_and_does_not_repeat_the_resnapshot() {
    for item in [
        event_after(300),
        ProjectionFeedItem::ResetRequired {
            reason: "projection-feed-overflow",
        },
    ] {
        let mut fresh = snapshot_at(300);
        fresh.feed.push(item);
        let (client, port) = sequenced_client(vec![
            Ok(snapshot_at(10)),
            Ok(overflow_at(10)),
            Ok(fresh),
            Ok(snapshot_at(400)),
        ]);
        futures::executor::block_on(client.refresh()).expect("initial refresh");
        assert!(matches!(
            futures::executor::block_on(client.refresh()),
            Err(ApplicationError::CorruptProjection(_))
        ));
        assert_previous_cut_untouched(&client, &port, 3);
        assert_eq!(port.snapshots.lock().expect("response queue").len(), 1);
    }
}

#[test]
fn application_client_overflow_propagates_fresh_source_errors_without_commit_or_retry() {
    for error in [
        ApplicationError::Unavailable,
        ApplicationError::ResetRequired,
    ] {
        let (client, port) = sequenced_client(vec![
            Ok(snapshot_at(10)),
            Ok(overflow_at(10)),
            Err(error.clone()),
        ]);
        futures::executor::block_on(client.refresh()).expect("initial refresh");
        assert_eq!(futures::executor::block_on(client.refresh()), Err(error));
        assert_previous_cut_untouched(&client, &port, 3);
    }
}

#[test]
fn application_client_overflow_validates_the_original_resume_envelope_before_recovery() {
    let mut corrupt = overflow_at(10);
    corrupt.envelope.projection.frontier.writer_generation = 2;
    for resumed in [overflow_at(11), corrupt] {
        let (client, port) =
            sequenced_client(vec![Ok(snapshot_at(10)), Ok(resumed), Ok(snapshot_at(300))]);
        futures::executor::block_on(client.refresh()).expect("initial refresh");
        assert!(matches!(
            futures::executor::block_on(client.refresh()),
            Err(ApplicationError::ResetRequired | ApplicationError::CorruptProjection(_))
        ));
        assert_previous_cut_untouched(&client, &port, 2);
        assert_eq!(port.snapshots.lock().expect("response queue").len(), 1);
    }
}

#[test]
fn application_client_does_not_resnapshot_for_gaps_other_resets_or_mixed_overflow_feed() {
    for feed in [
        vec![ProjectionFeedItem::Gap {
            expected: 11,
            observed: 12,
        }],
        vec![ProjectionFeedItem::ResetRequired {
            reason: "different-reset",
        }],
        vec![
            ProjectionFeedItem::ResetRequired {
                reason: "projection-feed-overflow",
            },
            event_after(10),
        ],
    ] {
        let mut resumed = snapshot_at(10);
        resumed.feed = feed;
        let (client, port) = sequenced_client(vec![Ok(snapshot_at(10)), Ok(resumed)]);
        futures::executor::block_on(client.refresh()).expect("initial refresh");
        assert_eq!(
            futures::executor::block_on(client.refresh()),
            Err(ApplicationError::ResetRequired)
        );
        assert_previous_cut_untouched(&client, &port, 2);
    }
}

#[test]
fn application_client_does_not_resnapshot_for_resumed_source_errors_or_corrupt_events() {
    let mut corrupt_feed = snapshot_at(10);
    let mut corrupt_event = event_after(10);
    let ProjectionFeedItem::Event(event) = &mut corrupt_event else {
        panic!("fixture must be an event");
    };
    event.payload_digest = "invalid-digest".to_owned();
    corrupt_feed.feed.push(corrupt_event);
    for resumed in [
        Err(ApplicationError::ResetRequired),
        Err(ApplicationError::Unavailable),
        Ok(corrupt_feed),
    ] {
        let (client, port) = sequenced_client(vec![Ok(snapshot_at(10)), resumed]);
        futures::executor::block_on(client.refresh()).expect("initial refresh");
        assert!(futures::executor::block_on(client.refresh()).is_err());
        assert_previous_cut_untouched(&client, &port, 2);
    }
}

#[test]
fn application_client_does_not_resnapshot_an_overflow_marker_without_a_prior_cut() {
    let (client, port) = sequenced_client(vec![Ok(overflow_at(10)), Ok(snapshot_at(300))]);
    assert_eq!(
        futures::executor::block_on(client.refresh()),
        Err(ApplicationError::ResetRequired)
    );
    assert_client_state_empty(&client, &port);
}

fn assert_client_state_empty(client: &ApplicationClient, port: &SequencedProjectionPort) {
    assert_eq!(client.current_frontier().expect("frontier state"), None);
    assert_eq!(client.current_projection().expect("projection state"), None);
    assert!(port.acknowledgements.lock().expect("ack log").is_empty());
    assert_eq!(port.opens.lock().expect("open log").len(), 1);
}
