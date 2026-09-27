use std::{
    path::Path,
    sync::{Arc, mpsc},
    time::Duration,
};

use anyhow::Result;
use futures::future::BoxFuture;
use ratatui::{Terminal, backend::TestBackend, layout::Rect};
use sigil_application::{
    APPLICATION_CONTRACT_SCHEMA_VERSION, ApplicationCommandReceipt, ApplicationError,
    ApplicationFrontier, ApplicationPort, ApplicationProjection, ApplicationQueueSurfaceProjection,
    ApplicationScope, AuthenticatedSubject, CapabilitySurfaceProjection,
    ConfigurationSurfaceProjection, ConversationSurfaceProjection, HostConnectionInstanceId,
    OpenProjectionRequest, ProjectionDeliveryAck, ProjectionPage, ProjectionPageRequest,
    ProjectionSnapshot, ProjectionSnapshotEnvelope, ResourceRecoverySurfaceContractV1,
    RunSurfaceProjection, SafeText, SessionSurfaceProjection, TerminalSurfaceProjection,
    UserInputSurfaceProjection,
};

use super::*;

/// A transport-only actor acknowledges real dispatch but never invents domain completion.
pub(crate) fn acknowledged_test_channel(
    owner: Option<sigil_kernel::SessionApplicationOperationOwner>,
) -> (WorkerCommandSender, mpsc::Receiver<WorkerCommand>) {
    let (sender, receiver) = WorkerCommandSender::test_channel();
    let (observed, observations) = mpsc::channel();
    std::thread::spawn(move || {
        while let Ok(command) = receiver.recv() {
            match command {
                WorkerCommand::QueryApplicationOperation { binding, reply } => {
                    let result = owner
                        .as_ref()
                        .ok_or_else(|| "fixture has no domain owner".to_owned())
                        .and_then(|owner| {
                            let (binding, reader) = owner
                                .observe_operation(&binding)
                                .map_err(|error| error.to_string())?;
                            let proof = sigil_kernel::session::reconcile_application_operation(
                                &reader, &binding,
                            )
                            .map_err(|error| error.to_string())?;
                            Ok((binding, proof))
                        });
                    let _ = reply.send(result);
                }
                WorkerCommand::FindCommittedApplicationOperation {
                    target,
                    key_digest,
                    reply,
                } => {
                    let result = owner
                        .as_ref()
                        .ok_or_else(|| "fixture has no domain owner".to_owned())
                        .and_then(|owner| {
                            let (scope, reader) = owner
                                .observe_target(&target)
                                .map_err(|error| error.to_string())?;
                            sigil_kernel::session::committed_application_operation(
                                &reader,
                                &scope,
                                &key_digest,
                            )
                            .map_err(|error| error.to_string())
                        });
                    let _ = reply.send(result);
                }
                WorkerCommand::PrepareApplicationOperation { binding, reply } => {
                    let result = owner
                        .as_ref()
                        .ok_or_else(|| "fixture has no domain owner".to_owned())
                        .and_then(|owner| {
                            owner.prepare(&binding).map_err(|error| error.to_string())
                        });
                    let _ = reply.send(result);
                }
                WorkerCommand::ApplicationDispatch { command, reply, .. } => {
                    let _ = observed.send(*command);
                    let _ = reply.send(Ok(WorkerApplicationDispatchOutcome::Dispatched));
                }
                command => {
                    let _ = observed.send(command);
                }
            }
        }
    });
    (sender, observations)
}

/// Connects the shipping application executor and durable projection to a real test worker.
/// This fixture changes only boot assembly; command admission, projection, reservations and
/// observer acknowledgements all use their production implementations.
pub(crate) fn connect_real_worker(
    config_path: &Path,
    workspace: &Path,
    session_path: &Path,
    session_id: &str,
    worker_tx: WorkerCommandSender,
    composition: &sigil_runtime::r71_authority_composition::RuntimeAuthorityCompositionV1,
    projection_owner: sigil_runtime::RuntimeSessionProjectionOwner,
) -> Result<TuiApplicationSession> {
    let scope = ApplicationScope {
        application_instance: sigil_application::ApplicationInstanceId::new(
            "core-worker-application",
        )?,
        authenticated_subject: AuthenticatedSubject::new("local-user")?,
        workspace: Some(sigil_application::WorkspaceScopeId::new(
            sigil_kernel::stable_workspace_id(workspace).map_err(anyhow::Error::msg)?,
        )?),
        session: Some(sigil_application::SessionScopeId::new(session_id)?),
    };
    let projection = Arc::new(
        sigil_runtime::RuntimeSessionProjectionBinding::new(
            config_path.to_owned(),
            workspace.to_owned(),
            session_path.to_owned(),
            session_id.to_owned(),
            scope.application_instance.clone(),
            scope.authenticated_subject.clone(),
            scope.workspace.clone(),
            1,
            1,
            1,
            1,
        )?
        .with_owner(projection_owner),
    );
    let endpoint = TuiWorkerEndpoint::new(worker_tx);
    let executor = Arc::new(TuiWorkerCommandExecutor {
        endpoint: Arc::clone(&endpoint),
        projection_binding: Some(Arc::clone(&projection)),
        reasoning_effort: ReasoningEffort::Max,
        session_id: session_id.to_owned(),
        session_bindings: Arc::new(Mutex::new(BTreeMap::new())),
        session_maintenance_bindings: Arc::new(Mutex::new(BTreeMap::new())),
        provider_route_bindings: Arc::new(Mutex::new(BTreeMap::new())),
        mcp_oauth_bindings: Arc::new(Mutex::new(BTreeMap::new())),
        configuration_bindings: Arc::new(Mutex::new(BTreeMap::new())),
    });
    let service: Arc<dyn ApplicationPort> =
        Arc::new(sigil_runtime::RuntimeApplicationService::new(
            projection.clone(),
            executor,
            Arc::new(sigil_runtime::ManagedApplicationReservationStore::open(
                Arc::clone(&composition.storage_writer),
                "tui-application",
            )?),
            Arc::new(sigil_runtime::RuntimeApplicationDeliveryAckStore::open(
                Arc::clone(&composition.storage_writer),
                "core-worker-projection-delivery",
                scope.clone(),
                1,
            )?),
        ));
    let mut application = session_with_projection_binding(service, scope, projection)?;
    // Resume looks up the original K through the client endpoint before preparing a new
    // command. It must use the same actual worker as the service executor, as production does.
    application.endpoint = endpoint;
    Ok(application)
}

struct StaticProjectionSource {
    snapshot: ProjectionSnapshot,
}

impl sigil_runtime::RuntimeApplicationProjectionSource for StaticProjectionSource {
    fn open_projection(
        &self,
        request: OpenProjectionRequest,
    ) -> BoxFuture<'static, Result<ProjectionSnapshot, ApplicationError>> {
        let snapshot = self.snapshot.clone();
        Box::pin(async move {
            if request.scope != snapshot.envelope.scope {
                return Err(ApplicationError::ScopeMismatch);
            }
            Ok(snapshot)
        })
    }

    fn page(
        &self,
        _request: ProjectionPageRequest,
    ) -> BoxFuture<'static, Result<ProjectionPage, ApplicationError>> {
        Box::pin(async { Err(ApplicationError::NotFound) })
    }
}

struct NoopDeliveryAcker;

impl sigil_runtime::RuntimeApplicationDeliveryAcker for NoopDeliveryAcker {
    fn acknowledge(
        &self,
        _acknowledgement: ProjectionDeliveryAck,
    ) -> BoxFuture<'static, Result<(), ApplicationError>> {
        Box::pin(async { Ok(()) })
    }
}

pub(crate) fn snapshot(scope: ApplicationScope) -> ProjectionSnapshot {
    let frontier = ApplicationFrontier {
        schema_version: APPLICATION_CONTRACT_SCHEMA_VERSION,
        scope: scope.clone(),
        writer_generation: 1,
        stream_generation: 1,
        through_sequence: 0,
        durable_cursor: "tui-recovery-test".to_owned(),
    };
    let projection = ApplicationProjection {
        schema_version: APPLICATION_CONTRACT_SCHEMA_VERSION,
        scope: scope.clone(),
        writer_generation: 1,
        stream_generation: 1,
        observer_generation: 1,
        frontier: frontier.clone(),
        resource_recovery: ResourceRecoverySurfaceContractV1 {
            schema_version: sigil_application::RESOURCE_RECOVERY_SURFACE_SCHEMA_VERSION,
            blocker: None,
            resource_effects: Vec::new(),
            action_envelope: None,
        },
        session: SessionSurfaceProjection {
            session_id: scope.session.clone(),
            title: SafeText::new("test session").expect("static projection text"),
            status: SafeText::new("idle").expect("static projection text"),
        },
        conversation: ConversationSurfaceProjection {
            message_count: 0,
            latest_message: None,
        },
        run: RunSurfaceProjection {
            status: SafeText::new("idle").expect("static projection text"),
            active_binding: None,
        },
        plan_task: sigil_application::PlanTaskSurfaceProjection {
            status: SafeText::new("none").expect("static projection text"),
            action_binding: None,
        },
        agents: sigil_application::AgentSurfaceProjection {
            active_count: 0,
            summary: Vec::new(),
        },
        approval: sigil_application::ApprovalSurfaceProjection {
            pending: false,
            binding: None,
            summary: None,
        },
        user_input: UserInputSurfaceProjection {
            pending: true,
            binding: Some("recovery-request".to_owned()),
            prompt: Some(SafeText::new("answer").expect("static projection text")),
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
        attention: sigil_application::AttentionSurfaceProjection { last_notice: None },
        queue: ApplicationQueueSurfaceProjection {
            generation: sigil_application::queue_generation(
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
    ProjectionSnapshot {
        envelope: ProjectionSnapshotEnvelope {
            schema_version: APPLICATION_CONTRACT_SCHEMA_VERSION,
            scope,
            writer_generation: 1,
            stream_generation: 1,
            observer_generation: 1,
            cut: frontier,
            projection,
        },
        feed: Vec::new(),
    }
}

#[test]
fn stale_projection_does_not_hide_an_optimistic_live_run() -> Result<()> {
    let mut app = crate::app::AppState::from_root_config(
        Path::new("sigil.toml"),
        &crate::app::tests::common::test_config(),
    );
    app.runtime.is_busy = true;
    app.runtime.run_phase = crate::app::RunPhase::Thinking;

    let scope = ApplicationScope {
        application_instance: sigil_application::ApplicationInstanceId::new("tui-live-run")
            .expect("valid application scope"),
        authenticated_subject: AuthenticatedSubject::new("local-user")
            .expect("valid authenticated subject"),
        workspace: Some(
            sigil_application::WorkspaceScopeId::new("workspace").expect("valid workspace scope"),
        ),
        session: Some(
            sigil_application::SessionScopeId::new("session").expect("valid session scope"),
        ),
    };
    let mut projection = snapshot(scope).envelope.projection;
    projection.run.status = SafeText::new("finished").expect("valid run status");

    app.apply_application_projection(&projection);

    assert!(app.runtime.is_busy);
    assert!(app.live_activity_summary().is_some());

    app.set_terminal_size(120, 30);
    let surface = crate::surface_adapter::build_surface_model(
        Rect::new(0, 0, 120, 30),
        &app,
        crate::surface::SurfaceState {
            frame_generation: 1,
            terminal_epoch: 1,
        },
    );
    assert!(surface.live_panel.progress.is_some());

    let mut terminal = Terminal::new(TestBackend::new(120, 30))?;
    terminal.draw(|frame| crate::ui::render_surface(frame, &surface))?;
    let rendered = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("Thinking..."));

    // A refresh started before the worker terminal message may still return the old running
    // projection. Once the local terminal transition wins, that stale projection must not
    // resurrect the loading state.
    app.clear_worker_run_state();
    projection.run.status = SafeText::new("running").expect("valid run status");
    app.apply_application_projection(&projection);
    assert!(!app.runtime.is_busy);
    Ok(())
}

#[test]
fn prepare_action_refreshes_the_initial_projection_before_admission() -> Result<()> {
    let scope = ApplicationScope {
        application_instance: sigil_application::ApplicationInstanceId::new("tui-cold-start")?,
        authenticated_subject: AuthenticatedSubject::new("local-user")?,
        workspace: Some(sigil_application::WorkspaceScopeId::new("workspace")?),
        session: Some(sigil_application::SessionScopeId::new("session")?),
    };
    let port: Arc<dyn ApplicationPort> = Arc::new(sigil_application::FakeApplication::new(
        snapshot(scope.clone()).envelope,
    )?);
    let application = session(port, scope)?;
    assert!(application.current_projection()?.is_none());

    let request = application
        .prepare_action(
            &AppAction::SubmitPrompt("resume immediately".to_owned()),
            None,
            None,
        )?
        .expect("prompt must map through the application contract");
    assert_eq!(request.envelope.expected_frontier.through_sequence, 0);
    assert!(application.current_projection()?.is_some());
    Ok(())
}

pub(crate) fn session(
    port: Arc<dyn ApplicationPort>,
    scope: ApplicationScope,
) -> Result<TuiApplicationSession> {
    let session_scope_id = scope
        .session
        .as_ref()
        .expect("test application scope has a session")
        .as_str()
        .to_owned();
    let projection_binding = Arc::new(sigil_runtime::RuntimeSessionProjectionBinding::new(
        std::path::PathBuf::from("sigil.toml"),
        std::env::current_dir()?,
        std::path::PathBuf::from("tui-application-bridge-test.jsonl"),
        session_scope_id,
        scope.application_instance.clone(),
        scope.authenticated_subject.clone(),
        scope.workspace.clone(),
        1,
        1,
        1,
        1,
    )?);
    session_with_projection_binding(port, scope, projection_binding)
}

pub(crate) fn session_with_projection_binding(
    port: Arc<dyn ApplicationPort>,
    scope: ApplicationScope,
    projection_binding: Arc<sigil_runtime::RuntimeSessionProjectionBinding>,
) -> Result<TuiApplicationSession> {
    TuiApplicationSession::new(
        port,
        scope,
        projection_binding,
        1,
        1,
        HostConnectionInstanceId::new("tui-recovery-test-connection")?,
        ApplicationReasoningEffort::Medium,
        Arc::new(Mutex::new(BTreeMap::new())),
        Arc::new(Mutex::new(BTreeMap::new())),
        Arc::new(Mutex::new(BTreeMap::new())),
        Arc::new(Mutex::new(BTreeMap::new())),
        Arc::new(Mutex::new(BTreeMap::new())),
    )
    .map_err(Into::into)
}

#[tokio::test]
async fn tui_projection_commit_precedes_the_exact_durable_public_outbox_ack() -> Result<()> {
    use sigil_runtime::managed_storage_writer::StorageWriterChannelV1 as Channel;

    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    let config = crate::app::tests::common::test_config();
    std::fs::write(&config_path, toml::to_string(&config)?)?;
    let (provider_name, route) =
        sigil_runtime::provider_connections::resolve_default_model_route(&config)?;
    let session_path = temp.path().join("tui-public-outbox-ack.jsonl");
    let store = sigil_kernel::JsonlSessionStore::new(&session_path)?;
    let mut durable_session =
        sigil_kernel::Session::new_with_route(provider_name, route).with_store(store.clone());
    durable_session.ensure_identity_entry()?;
    sigil_runtime::bind_session_composition(&mut durable_session, &config)?;
    let session_scope_id = durable_session.session_scope_id().to_owned();
    let event = sigil_kernel::PublicRunEvent::new(
        &session_scope_id,
        "tui-public-outbox-run",
        1,
        sigil_kernel::PublicRunEventKind::Notice {
            message: "durable TUI acknowledgement".to_owned(),
        },
    );
    let public_event_id = format!("tui-public:{session_scope_id}:1");
    sigil_kernel::PublicEventOutboxRecorder::new(store.clone()).append_outbox(
        &sigil_kernel::PublicEventOutboxEntryV1 {
            schema_version: sigil_kernel::PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
            public_event_id: public_event_id.clone(),
            domain_event_id: public_event_id,
            run_id: event.run_id.clone(),
            sequence: event.sequence,
            payload_digest: sigil_kernel::stable_event_hash(serde_json::to_vec(&event)?),
            event,
        },
    )?;

    let scope = ApplicationScope {
        application_instance: sigil_application::ApplicationInstanceId::new("tui-public-ack")?,
        authenticated_subject: AuthenticatedSubject::new("local-user")?,
        workspace: Some(sigil_application::WorkspaceScopeId::new("workspace")?),
        session: Some(sigil_application::SessionScopeId::new(&session_scope_id)?),
    };
    let projection_binding = Arc::new(
        sigil_runtime::RuntimeSessionProjectionBinding::new(
            config_path.clone(),
            std::env::current_dir()?,
            session_path.clone(),
            session_scope_id.clone(),
            scope.application_instance.clone(),
            scope.authenticated_subject.clone(),
            scope.workspace.clone(),
            1,
            1,
            1,
            1,
        )?
        .with_owner(sigil_runtime::RuntimeSessionProjectionOwner::from_store(
            &store,
        )),
    );
    let state_root = temp.path().join("state");
    let execution_temp = temp.path().join("execution-temp");
    std::fs::create_dir_all(state_root.join("cache"))?;
    std::fs::create_dir_all(&execution_temp)?;
    let composition = sigil_runtime::r71_authority_composition::compose_runtime_authority(
        &state_root,
        &execution_temp,
        sigil_kernel::resource::CanonicalHash::from_bytes([0x61; 32]),
        sigil_kernel::resource::AuthorityGeneration {
            epoch: 1,
            instance_hash: sigil_kernel::resource::CanonicalHash::from_bytes([0x75; 32]),
        },
        Arc::new(sigil_runtime::r71_shadow_planner::ShadowPlannerV1::new(
            sigil_runtime::r71_shadow_planner::ShadowPlannerConfigV1::default(),
        )),
        &[
            Channel::ApplicationControlLog,
            Channel::ApplicationCommandIndex,
            Channel::ApplicationControlRecovery,
        ],
    )?;
    let (worker_tx, _worker_rx) = acknowledged_test_channel(None);
    let executor = Arc::new(TuiWorkerCommandExecutor {
        endpoint: TuiWorkerEndpoint::new(worker_tx),
        projection_binding: None,
        reasoning_effort: ReasoningEffort::Medium,
        session_id: session_scope_id,
        session_bindings: Arc::new(Mutex::new(BTreeMap::new())),
        session_maintenance_bindings: Arc::new(Mutex::new(BTreeMap::new())),
        provider_route_bindings: Arc::new(Mutex::new(BTreeMap::new())),
        mcp_oauth_bindings: Arc::new(Mutex::new(BTreeMap::new())),
        configuration_bindings: Arc::new(Mutex::new(BTreeMap::new())),
    });
    let projection_source: Arc<dyn sigil_runtime::RuntimeApplicationProjectionSource> =
        projection_binding.clone();
    let service: Arc<dyn ApplicationPort> =
        Arc::new(sigil_runtime::RuntimeApplicationService::new(
            projection_source,
            executor,
            Arc::new(sigil_runtime::ManagedApplicationReservationStore::open(
                Arc::clone(&composition.storage_writer),
                "tui-application",
            )?),
            Arc::new(NoopDeliveryAcker),
        ));
    let application = session_with_projection_binding(service, scope, projection_binding)?;

    let projection = application.refresh().await?;
    let mut app = crate::AppState::from_root_config(&config_path, &config);
    assert!(
        app.apply_application_projection(&projection),
        "the durable application projection must commit into AppState before its ACK"
    );
    let delivery = application.refresh_delivery().await?;
    for notice in delivery.notices {
        app.handle_worker_message(crate::runner::WorkerMessage::Notice(
            notice.as_str().to_owned(),
        ))?;
    }
    let delivered_ids = application.take_applied_delivery_event_ids()?;
    assert_eq!(
        application
            .acknowledge_public_events(delivered_ids, &delivery.frontier)
            .await?,
        1
    );
    let outbox = sigil_kernel::PublicEventOutboxProjectionV1::from_records(
        &sigil_kernel::JsonlSessionStore::read_event_records(&session_path)?,
    )?;
    assert!(
        outbox.pending_for_adapter("tui").is_empty(),
        "the runtime-owned ACK must consume only the projection's committed durable cut"
    );
    Ok(())
}

#[test]
fn tui_application_session_replays_uncertain_input_without_reenqueuing_the_worker() -> Result<()> {
    use sigil_runtime::managed_storage_writer::StorageWriterChannelV1 as Channel;

    let temp = tempfile::tempdir()?;
    let state_root = temp.path().join("state");
    let execution_temp = temp.path().join("execution-temp");
    std::fs::create_dir_all(state_root.join("cache"))?;
    std::fs::create_dir_all(&execution_temp)?;
    let composition = sigil_runtime::r71_authority_composition::compose_runtime_authority(
        &state_root,
        &execution_temp,
        sigil_kernel::resource::CanonicalHash::from_bytes([0x61; 32]),
        sigil_kernel::resource::AuthorityGeneration {
            epoch: 1,
            instance_hash: sigil_kernel::resource::CanonicalHash::from_bytes([0x75; 32]),
        },
        Arc::new(sigil_runtime::r71_shadow_planner::ShadowPlannerV1::new(
            sigil_runtime::r71_shadow_planner::ShadowPlannerConfigV1::default(),
        )),
        &[
            Channel::ApplicationControlLog,
            Channel::ApplicationCommandIndex,
            Channel::ApplicationControlRecovery,
        ],
    )?;
    let mut owner_session = sigil_kernel::Session::new("test", "model").with_store(
        sigil_kernel::JsonlSessionStore::new(temp.path().join("domain.jsonl"))?,
    );
    owner_session.append_user_message(sigil_kernel::ModelMessage::user("domain owner"))?;
    let owner = owner_session.application_operation_owner()?;
    let scope = ApplicationScope {
        application_instance: sigil_application::ApplicationInstanceId::new("tui-recovery")?,
        authenticated_subject: AuthenticatedSubject::new("local-user")?,
        workspace: Some(sigil_application::WorkspaceScopeId::new("workspace")?),
        session: Some(sigil_application::SessionScopeId::new(
            owner_session.session_scope_id(),
        )?),
    };
    let (worker_tx, worker_rx) = acknowledged_test_channel(Some(owner));
    let executor = Arc::new(TuiWorkerCommandExecutor {
        endpoint: TuiWorkerEndpoint::new(worker_tx),
        projection_binding: None,
        reasoning_effort: ReasoningEffort::Medium,
        session_id: owner_session.session_scope_id().to_owned(),
        session_bindings: Arc::new(Mutex::new(BTreeMap::new())),
        session_maintenance_bindings: Arc::new(Mutex::new(BTreeMap::new())),
        provider_route_bindings: Arc::new(Mutex::new(BTreeMap::new())),
        mcp_oauth_bindings: Arc::new(Mutex::new(BTreeMap::new())),
        configuration_bindings: Arc::new(Mutex::new(BTreeMap::new())),
    });
    let service: Arc<dyn ApplicationPort> =
        Arc::new(sigil_runtime::RuntimeApplicationService::new(
            Arc::new(StaticProjectionSource {
                snapshot: snapshot(scope.clone()),
            }),
            executor,
            Arc::new(sigil_runtime::ManagedApplicationReservationStore::open(
                Arc::clone(&composition.storage_writer),
                "tui-application",
            )?),
            Arc::new(NoopDeliveryAcker),
        ));
    let initial = session(Arc::clone(&service), scope.clone())?;
    futures::executor::block_on(initial.refresh())?;
    let action = AppAction::SubmitUserInputDecision {
        command_id: Some("managed-plan-review-recovery-command".to_owned()),
        request_id: "recovery-request".to_owned(),
        generation: 1,
        expected_request_hash: format!("sha256:{}", "a".repeat(64)),
        decision: sigil_kernel::UserInputDecisionV1::RunCancelled,
    };
    let first = initial
        .try_execute_action(&action, None, None)?
        .expect("user input must map through the real application service");
    let ApplicationCommandReceipt::Uncertain(original) = first else {
        panic!("first managed application reservation must be uncertain");
    };
    assert!(
        original
            .owner_recovery_binding
            .as_ref()
            .is_some_and(|binding| !binding.starts_with("tui-worker:")),
        "uncertain receipt must retain the actual prepared operation identity"
    );
    assert!(matches!(
        worker_rx.recv_timeout(Duration::from_secs(1))?,
        WorkerCommand::SubmitUserInputDecision {
            command_id: Some(command_id),
            ..
        } if command_id == "managed-plan-review-recovery-command"
    ));

    let reattached = session(service, scope)?;
    futures::executor::block_on(reattached.refresh())?;
    let replay = reattached
        .try_execute_action(&action, None, None)?
        .expect("the same exact application command must replay its durable receipt");
    let ApplicationCommandReceipt::ReplayedUncertain(replayed) = replay else {
        panic!("uncertain terminal must replay as ReplayedUncertain");
    };
    assert_eq!(
        replayed, original,
        "the replay must retain the exact original uncertain receipt"
    );
    assert!(
        matches!(
            worker_rx.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ),
        "the managed application reservation replay must not enqueue the original command"
    );
    Ok(())
}

#[test]
fn approval_application_bridge_preserves_durable_scope_and_replays_without_reenqueue() -> Result<()>
{
    use sigil_runtime::managed_storage_writer::StorageWriterChannelV1 as Channel;

    let temp = tempfile::tempdir()?;
    let session_path = temp.path().join("approval-session.jsonl");
    let mut durable_session = sigil_kernel::Session::new("test", "test-model")
        .with_store(sigil_kernel::JsonlSessionStore::new(&session_path)?);
    durable_session.ensure_identity_entry()?;
    let session_id = durable_session.session_scope_id().to_owned();
    uuid::Uuid::parse_str(&session_id)?;
    assert_ne!(session_id, session_path.display().to_string());

    let state_root = temp.path().join("state");
    let execution_temp = temp.path().join("execution-temp");
    std::fs::create_dir_all(state_root.join("cache"))?;
    std::fs::create_dir_all(&execution_temp)?;
    let composition = sigil_runtime::r71_authority_composition::compose_runtime_authority(
        &state_root,
        &execution_temp,
        sigil_kernel::resource::CanonicalHash::from_bytes([0x61; 32]),
        sigil_kernel::resource::AuthorityGeneration {
            epoch: 1,
            instance_hash: sigil_kernel::resource::CanonicalHash::from_bytes([0x75; 32]),
        },
        Arc::new(sigil_runtime::r71_shadow_planner::ShadowPlannerV1::new(
            sigil_runtime::r71_shadow_planner::ShadowPlannerConfigV1::default(),
        )),
        &[
            Channel::ApplicationControlLog,
            Channel::ApplicationCommandIndex,
            Channel::ApplicationControlRecovery,
        ],
    )?;
    let scope = ApplicationScope {
        application_instance: sigil_application::ApplicationInstanceId::new("tui-approval")?,
        authenticated_subject: AuthenticatedSubject::new("local-user")?,
        workspace: Some(sigil_application::WorkspaceScopeId::new("workspace")?),
        session: Some(sigil_application::SessionScopeId::new(&session_id)?),
    };
    let (worker_tx, worker_rx) = acknowledged_test_channel(None);
    let executor = Arc::new(TuiWorkerCommandExecutor {
        endpoint: TuiWorkerEndpoint::new(worker_tx),
        projection_binding: None,
        reasoning_effort: ReasoningEffort::Medium,
        session_id: session_id.clone(),
        session_bindings: Arc::new(Mutex::new(BTreeMap::new())),
        session_maintenance_bindings: Arc::new(Mutex::new(BTreeMap::new())),
        provider_route_bindings: Arc::new(Mutex::new(BTreeMap::new())),
        mcp_oauth_bindings: Arc::new(Mutex::new(BTreeMap::new())),
        configuration_bindings: Arc::new(Mutex::new(BTreeMap::new())),
    });
    let reservations = Arc::new(sigil_runtime::ManagedApplicationReservationStore::open(
        Arc::clone(&composition.storage_writer),
        "tui-application",
    )?);
    let service_for = |snapshot: ProjectionSnapshot| -> Arc<dyn ApplicationPort> {
        Arc::new(sigil_runtime::RuntimeApplicationService::new(
            Arc::new(StaticProjectionSource { snapshot }),
            executor.clone(),
            reservations.clone(),
            Arc::new(NoopDeliveryAcker),
        ))
    };
    let action = AppAction::ApprovalDecision {
        call_id: "call-approval".to_owned(),
        approval_request_id: "request-approval".to_owned(),
        approved: true,
    };
    let without_binding = session(service_for(snapshot(scope.clone())), scope.clone())?;
    futures::executor::block_on(without_binding.refresh())?;
    assert!(matches!(
        without_binding.try_execute_action(&action, None, None),
        Err(ApplicationError::InvalidRequest(_))
    ));
    assert!(matches!(
        worker_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));

    let binding = "run-approval:call-approval:request-approval".to_owned();
    let mut pending = snapshot(scope.clone());
    pending.envelope.projection.approval.pending = true;
    pending.envelope.projection.approval.binding = Some(binding.clone());
    let service = service_for(pending);
    let initial = session(Arc::clone(&service), scope.clone())?;
    futures::executor::block_on(initial.refresh())?;
    assert!(matches!(
        initial.try_execute_action(
            &AppAction::ApprovalDecision {
                call_id: "an-older-card".to_owned(),
                approval_request_id: "an-older-request".to_owned(),
                approved: true,
            },
            None,
            None
        ),
        Err(ApplicationError::ScopeMismatch)
    ));
    let receipt = initial
        .try_execute_action(&action, None, None)?
        .expect("approval action must pass through the shipping application bridge");
    let ApplicationCommandReceipt::Uncertain(original) = receipt else {
        panic!("the asynchronous worker enqueue must retain an uncertain application receipt");
    };
    let WorkerCommand::ApprovalCommand(command) = worker_rx.recv_timeout(Duration::from_secs(1))?
    else {
        panic!("approval must translate into the worker approval envelope");
    };
    assert_eq!(command.command_id, original.command_id.as_str());
    assert_eq!(command.session_id, session_id);
    assert!(matches!(
        command.payload,
        WorkerApprovalCommand::Decision {
            call_id,
            approval_request_id,
            approved: true,
        } if call_id == "call-approval" && approval_request_id == "request-approval"
    ));

    let foreign_scope = ApplicationScope {
        session: Some(sigil_application::SessionScopeId::new(
            uuid::Uuid::new_v4().to_string(),
        )?),
        ..scope.clone()
    };
    let foreign = session(Arc::clone(&service), foreign_scope)?;
    assert!(matches!(
        futures::executor::block_on(foreign.refresh()),
        Err(ApplicationError::ScopeMismatch)
    ));

    let reattached = session(service, scope)?;
    futures::executor::block_on(reattached.refresh())?;
    let replay = futures::executor::block_on(reattached.application.execute_with_id(
        original.command_id.clone(),
        ApplicationCommand::Approval(sigil_application::ApprovalCommand::Resolve {
            binding,
            accepted: true,
            resolution: None,
        }),
    ))?;
    assert!(matches!(
        replay,
        ApplicationCommandReceipt::ReplayedUncertain(replayed) if replayed == original
    ));
    assert!(
        matches!(worker_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "a rejected scope and a durable replay must not enqueue another approval"
    );
    Ok(())
}

#[test]
fn application_factory_rejects_before_boot_cutover_without_dispatching_a_worker_command() {
    let fixture = tempfile::tempdir().expect("isolated application fixture");
    let store = sigil_kernel::JsonlSessionStore::new(fixture.path().join("session.jsonl"))
        .expect("fixture owner");
    let config = crate::app::tests::common::test_config();
    let app = crate::app::AppState::from_root_config(Path::new("sigil.toml"), &config);
    let (worker_tx, worker_rx) = acknowledged_test_channel(None);

    let error = build_for_worker(
        &app,
        worker_tx.clone(),
        ReasoningEffort::Medium,
        sigil_runtime::RuntimeSessionProjectionOwner::from_store(&store),
    )
    .expect_err("the application factory must require the published boot cutover");
    assert!(
        error
            .to_string()
            .contains("application requires boot cutover")
    );
    assert!(
        matches!(
            worker_rx.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ),
        "a rejected factory precondition must not send a worker command"
    );
}

#[test]
fn stable_tui_client_epoch_is_stable_per_scope_and_changes_with_scope() -> Result<()> {
    let scope = ApplicationScope {
        application_instance: sigil_application::ApplicationInstanceId::new("tui-application")?,
        authenticated_subject: AuthenticatedSubject::new("local-user")?,
        workspace: Some(sigil_application::WorkspaceScopeId::new("workspace-a")?),
        session: Some(sigil_application::SessionScopeId::new("session-a")?),
    };
    let repeated = stable_tui_client_epoch(&scope);
    assert_eq!(repeated, stable_tui_client_epoch(&scope));
    assert_ne!(repeated, 0);

    let different_scope = ApplicationScope {
        session: Some(sigil_application::SessionScopeId::new("session-b")?),
        ..scope
    };
    assert_ne!(repeated, stable_tui_client_epoch(&different_scope));
    Ok(())
}

#[test]
fn image_only_action_crosses_application_boundary_without_placeholder_text() -> Result<()> {
    let scope = ApplicationScope {
        application_instance: sigil_application::ApplicationInstanceId::new("image-only")?,
        authenticated_subject: AuthenticatedSubject::new("local-user")?,
        workspace: Some(sigil_application::WorkspaceScopeId::new("workspace")?),
        session: Some(sigil_application::SessionScopeId::new("session")?),
    };
    let port: Arc<dyn ApplicationPort> = Arc::new(sigil_application::FakeApplication::new(
        snapshot(scope.clone()).envelope,
    )?);
    let application = session(port, scope)?;
    let image = sigil_kernel::ImageAttachment::from_bytes(
        "image-1",
        sigil_kernel::ImageMimeType::Png,
        1,
        1,
        vec![1],
    )?;
    let request = application
        .prepare_action(
            &AppAction::SubmitPromptWithAttachments {
                prompt: String::new(),
                attachments: vec![image.clone()],
            },
            None,
            None,
        )?
        .expect("image action");
    assert!(matches!(request.envelope.command,
        sigil_application::ApplicationCommand::Conversation(sigil_application::ConversationCommand::SubmitPromptWithAttachments { prompt: None, attachments, .. }) if attachments == vec![image]));
    Ok(())
}

#[test]
fn inline_skill_images_cross_application_boundary_without_placeholder_arguments() -> Result<()> {
    let scope = ApplicationScope {
        application_instance: sigil_application::ApplicationInstanceId::new("skill-image")?,
        authenticated_subject: AuthenticatedSubject::new("local-user")?,
        workspace: Some(sigil_application::WorkspaceScopeId::new("workspace")?),
        session: Some(sigil_application::SessionScopeId::new("session")?),
    };
    let port: Arc<dyn ApplicationPort> = Arc::new(sigil_application::FakeApplication::new(
        snapshot(scope.clone()).envelope,
    )?);
    let application = session(port, scope)?;
    let image = sigil_kernel::ImageAttachment::from_bytes(
        "image-1",
        sigil_kernel::ImageMimeType::Png,
        1,
        1,
        vec![1],
    )?;
    let request = application
        .prepare_action(
            &AppAction::InvokeInlineSkill {
                skill_id: "review".to_owned(),
                arguments: String::new(),
                attachments: vec![image.clone()],
            },
            None,
            None,
        )?
        .expect("inline skill action");
    assert!(matches!(request.envelope.command,
        sigil_application::ApplicationCommand::Agent(sigil_application::AgentCommand::InvokeInlineSkill { skill_id, arguments: None, attachments, .. })
        if skill_id.as_str() == "review" && attachments == vec![image]));
    Ok(())
}
