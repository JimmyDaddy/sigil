use std::{
    path::Path,
    sync::{Arc, mpsc},
    time::Duration,
};

use anyhow::Result;
use futures::future::BoxFuture;
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

fn snapshot(scope: ApplicationScope) -> ProjectionSnapshot {
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

fn session(
    port: Arc<dyn ApplicationPort>,
    scope: ApplicationScope,
) -> Result<TuiApplicationSession> {
    TuiApplicationSession::new(
        port,
        scope,
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
        Arc::new(sigil_runtime::r71_shadow_planner::ShadowPlannerV1::new(
            sigil_runtime::r71_shadow_planner::ShadowPlannerConfigV1::default(),
        )),
        &[Channel::ApplicationControlLog],
    )?;
    let scope = ApplicationScope {
        application_instance: sigil_application::ApplicationInstanceId::new("tui-recovery")?,
        authenticated_subject: AuthenticatedSubject::new("local-user")?,
        workspace: Some(sigil_application::WorkspaceScopeId::new("workspace")?),
        session: Some(sigil_application::SessionScopeId::new("session")?),
    };
    let (worker_tx, worker_rx) = WorkerCommandSender::test_channel();
    let executor = Arc::new(TuiWorkerCommandExecutor {
        worker_tx,
        reasoning_effort: ReasoningEffort::Medium,
        session_id: "session".to_owned(),
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
fn application_factory_rejects_before_boot_cutover_without_dispatching_a_worker_command() {
    let config = crate::app::tests::common::test_config();
    let app = crate::app::AppState::from_root_config(Path::new("sigil.toml"), &config);
    let (worker_tx, worker_rx) = WorkerCommandSender::test_channel();

    let error = build_for_worker(&app, worker_tx.clone(), ReasoningEffort::Medium)
        .expect_err("the application factory must require the published boot cutover");
    assert!(
        error
            .to_string()
            .contains("application port requires the published boot cutover")
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
