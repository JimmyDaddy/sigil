use super::*;

#[path = "session_runtime_resume_tests.rs"]
mod resume_tests;
use crate::provider_connections::{
    ProviderFamily, ProviderProtocol, materialize_root_config, provider_connection_template,
};
use sigil_application::{
    ApplicationCommand, ApplicationCommandEnvelope, ApplicationCommandId, ApplicationInstanceId,
    ApplicationScope, AuthenticatedSubject, CommandAdmissionContext, ExpectedFrontier,
    HostConnectionInstanceId, ProviderCommand, SessionScopeId,
};
use std::{
    collections::BTreeMap,
    sync::{Barrier, atomic::AtomicUsize},
};

type ReadyEffect = Box<dyn FnOnce() -> Result<()> + Send>;

#[derive(Default)]
struct Host {
    trace: Mutex<Vec<&'static str>>,
    fail_start: AtomicBool,
    fail_open: AtomicBool,
    wrong_ready: AtomicBool,
    stop_barrier: Mutex<Option<Arc<Barrier>>>,
    starts: AtomicUsize,
    live: AtomicBool,
    on_ready: Mutex<Option<ReadyEffect>>,
}
impl RuntimeSessionWorkerHost for Host {
    fn close_gate(&self) -> Result<()> {
        self.trace
            .lock()
            .expect("controller trace mutex should not be poisoned")
            .push("closed");
        Ok(())
    }
    fn stop(&self) -> Result<()> {
        self.trace
            .lock()
            .expect("controller trace mutex should not be poisoned")
            .push("stop");
        if let Some(barrier) = self
            .stop_barrier
            .lock()
            .expect("controller stop barrier mutex should not be poisoned")
            .take()
        {
            barrier.wait();
            barrier.wait();
        }
        self.trace
            .lock()
            .expect("controller trace mutex should not be poisoned")
            .push("stopped");
        Ok(())
    }
    fn ready(
        &self,
        _: RootConfig,
        expected: &SessionRuntimeReadyV1,
    ) -> Result<SessionRuntimeReadyV1> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        self.trace
            .lock()
            .expect("controller trace mutex should not be poisoned")
            .push("spawn");
        anyhow::ensure!(
            !self.fail_start.swap(false, Ordering::SeqCst),
            "controlled startup failure"
        );
        let mut ready = expected.clone();
        if self.wrong_ready.load(Ordering::SeqCst) {
            ready.worker_generation += 1;
        }
        if let Some(effect) = self
            .on_ready
            .lock()
            .expect("controller readiness callback mutex should not be poisoned")
            .take()
        {
            effect()?;
        }
        self.trace
            .lock()
            .expect("controller trace mutex should not be poisoned")
            .push("ready");
        Ok(ready)
    }
    fn open_gate(&self, _: &SessionRuntimeReadyV1) -> Result<()> {
        anyhow::ensure!(
            !self.fail_open.swap(false, Ordering::SeqCst),
            "controlled gate publication failure"
        );
        self.live.store(true, Ordering::Release);
        self.trace
            .lock()
            .expect("controller trace mutex should not be poisoned")
            .push("open");
        Ok(())
    }
    fn is_live(&self, _: &SessionRuntimeReadyV1) -> Result<bool> {
        Ok(self.live.load(Ordering::Acquire))
    }
}
struct Fixture {
    _temp: tempfile::TempDir,
    store: JsonlSessionStore,
    attachment: Arc<InteractiveSessionAttachmentLease>,
    config: RootConfig,
    route: ResolvedModelRoute,
    request: ApplicationCommandRequest,
    key: CommandReservationKey,
    host: Arc<Host>,
    exit: Arc<AtomicBool>,
}
impl Fixture {
    fn new(same_route: bool) -> Result<Self> {
        let temp = tempfile::tempdir()?;
        let mut connection = provider_connection_template(
            ProviderFamily::Custom,
            ProviderProtocol::OpenAiChatCompletions,
            sigil_kernel::ConnectionId::new("local")?,
            "Local",
        )?
        .0;
        connection.base_url = "https://models.example.test/v1".to_owned();
        let source_ref = sigil_kernel::ModelRef::new(connection.id.clone(), "source")?;
        let config = materialize_root_config(
            &crate::provider_connections::default_setup_root_config(),
            &BTreeMap::from([(connection.id.clone(), connection)]),
            &source_ref,
        )?;
        let snapshot = ResolvedRouteConfigSnapshot::from_root_config(&config);
        let (provider, source, trust) = snapshot
            .resolved_route(&source_ref)
            .expect("configured source route should resolve");
        let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
        let session = Session::load_from_store_with_route_and_trust(
            provider,
            "source",
            Some(source),
            Some(trust),
            store.clone(),
        )?;
        let route = snapshot
            .resolved_route(&sigil_kernel::ModelRef::new(
                source_ref.connection_id,
                if same_route { "source" } else { "target" },
            )?)
            .expect("selected target route should resolve")
            .1;
        let principal = AuthenticatedSubject::new("local-user")?;
        let scope = ApplicationScope {
            application_instance: ApplicationInstanceId::new("app")?,
            authenticated_subject: principal.clone(),
            workspace: None,
            session: Some(SessionScopeId::new(session.session_scope_id())?),
        };
        let command_id = ApplicationCommandId::new("route-command")?;
        let key = CommandReservationKey {
            application_instance: scope.application_instance.clone(),
            authority_scope: scope.clone(),
            principal: principal.clone(),
            client_epoch: 1,
            command_id: command_id.clone(),
        };
        let request = ApplicationCommandRequest {
            admission: CommandAdmissionContext::host_bound(
                principal,
                1,
                HostConnectionInstanceId::new("transport")?,
                scope.clone(),
            )?,
            envelope: ApplicationCommandEnvelope {
                schema_version: sigil_application::APPLICATION_CONTRACT_SCHEMA_VERSION,
                command_id,
                correlation_id: None,
                expected_frontier: ExpectedFrontier {
                    scope,
                    writer_generation: 1,
                    through_sequence: store
                        .read_event_records_writer()?
                        .last()
                        .expect("initialized session should have a durable frontier")
                        .stream_sequence(),
                },
                command: ApplicationCommand::Provider(ProviderCommand::SelectRoute {
                    binding: "target".to_owned(),
                }),
            },
        };
        let attachment = Arc::new(InteractiveSessionAttachmentLease::acquire(store.path())?);
        Ok(Self {
            _temp: temp,
            store,
            attachment,
            config,
            route,
            request,
            key,
            host: Arc::new(Host::default()),
            exit: Arc::new(AtomicBool::new(false)),
        })
    }
    fn controller(&self) -> Result<RuntimeSessionController> {
        RuntimeSessionController::new(
            self.store.clone(),
            Arc::clone(&self.attachment),
            self.config.clone(),
            self.host.clone(),
            2,
            Arc::clone(&self.exit),
        )
    }
    fn bind(&self, controller: &RuntimeSessionController) -> Result<CommandEffectBinding> {
        controller.bind_effect(
            &self.request,
            &self.route,
            self.key.clone(),
            command_fingerprint(&self.request)?,
        )
    }
    fn replay(&self) -> Result<SessionRuntimeTransitionReplayV1> {
        SessionRuntimeTransitionReplayV1::from_records(
            &self.store.read_event_records_writer()?,
            &RuntimeSessionController::operation_id(&self.request)?,
        )
    }
}

#[test]
fn configured_failure_reopens_and_resumes_same_intent_then_reconciles_exact_activation()
-> Result<()> {
    let fixture = Fixture::new(false)?;
    let controller = fixture.controller()?;
    let claim = fixture.bind(&controller)?;
    fixture.host.fail_start.store(true, Ordering::SeqCst);
    assert!(controller.dispatch(&fixture.request).is_err());
    let before = fixture.replay()?;
    assert!(before.configured.is_some());
    assert!(before.activated.is_none());
    assert_eq!(
        claim.owner_effect_id,
        before
            .intent
            .as_ref()
            .expect("failed startup retains its original intent")
            .1
            .event_id
    );
    assert_eq!(
        Session::load_from_store_for_control(fixture.store.clone())?.resolved_model_route(),
        Some(&fixture.route)
    );
    drop(controller);
    let reopened = fixture.controller()?;
    let settled = reopened.dispatch(&fixture.request)?;
    let RuntimeApplicationDispatch::Settled(receipt) = &settled else {
        panic!("expected activation");
    };
    let after = fixture.replay()?;
    assert_eq!(before.intent, after.intent);
    assert_eq!(before.configured, after.configured);
    assert_eq!(
        receipt.domain_commit.source_event_id,
        after
            .activated
            .as_ref()
            .expect("resumed controller commits activation")
            .1
            .event_id
    );
    assert_eq!(
        receipt.domain_commit.source_digest,
        sigil_kernel::sha256_hex(
            after
                .activated
                .expect("activation checksum should remain durable")
                .1
                .record_checksum
                .as_bytes()
        )
    );
    assert_eq!(reopened.reconcile(&fixture.request)?, Some(settled));
    assert_eq!(fixture.host.starts.load(Ordering::SeqCst), 2);
    Ok(())
}

#[test]
fn same_route_requires_real_ready_and_replay_does_not_spawn_again() -> Result<()> {
    let fixture = Fixture::new(true)?;
    let controller = fixture.controller()?;
    fixture.bind(&controller)?;
    let first = controller.dispatch(&fixture.request)?;
    assert_eq!(controller.dispatch(&fixture.request)?, first);
    assert_eq!(fixture.host.starts.load(Ordering::SeqCst), 1);
    assert_eq!(
        *fixture
            .host
            .trace
            .lock()
            .expect("controller trace mutex should not be poisoned"),
        ["closed", "stop", "stopped", "spawn", "ready", "open"]
    );
    assert!(fixture.replay()?.activated.is_some());
    Ok(())
}

#[test]
fn mismatched_ready_is_cleaned_up_and_never_activates() -> Result<()> {
    let fixture = Fixture::new(false)?;
    let controller = fixture.controller()?;
    fixture.bind(&controller)?;
    fixture.host.wrong_ready.store(true, Ordering::SeqCst);
    assert!(controller.dispatch(&fixture.request).is_err());
    assert!(fixture.replay()?.activated.is_none());
    assert!(
        !fixture
            .host
            .trace
            .lock()
            .expect("controller trace mutex should not be poisoned")
            .contains(&"open")
    );
    Ok(())
}

#[test]
fn exit_during_blocked_stop_keeps_owner_and_prevents_late_spawn() -> Result<()> {
    let fixture = Fixture::new(false)?;
    let controller = fixture.controller()?;
    fixture.bind(&controller)?;
    let barrier = Arc::new(Barrier::new(2));
    *fixture
        .host
        .stop_barrier
        .lock()
        .expect("controller stop barrier mutex should not be poisoned") =
        Some(Arc::clone(&barrier));
    let request = fixture.request.clone();
    let task = std::thread::spawn(move || controller.dispatch(&request));
    barrier.wait();
    assert_eq!(fixture.host.starts.load(Ordering::SeqCst), 0);
    fixture.exit.store(true, Ordering::Release);
    barrier.wait();
    assert!(
        task.join()
            .expect("controller dispatch thread should not panic")
            .expect_err("exit must stop the transition")
            .is::<RuntimeSessionControllerExiting>()
    );
    assert!(fixture.replay()?.configured.is_none());
    assert!(fixture.replay()?.activated.is_none());
    Ok(())
}

#[test]
fn real_execution_owner_rejects_before_intent_or_shutdown() -> Result<()> {
    let fixture = Fixture::new(false)?;
    let authority = fixture.attachment.route_mutation_authority(
        fixture
            .request
            .admission
            .scope
            .session
            .as_ref()
            .expect("controller request is bound to a session scope")
            .as_str(),
    )?;
    let _execution = authority.acquire_execution_owner()?;
    assert!(fixture.bind(&fixture.controller()?).is_err());
    assert!(fixture.replay()?.intent.is_none());
    assert!(
        fixture
            .host
            .trace
            .lock()
            .expect("controller trace mutex should not be poisoned")
            .is_empty()
    );
    Ok(())
}

#[test]
fn original_command_identity_cannot_be_rebound_to_another_fingerprint() -> Result<()> {
    let fixture = Fixture::new(false)?;
    let controller = fixture.controller()?;
    fixture.bind(&controller)?;
    let mut changed = fixture.request.clone();
    changed.envelope.command = ApplicationCommand::Provider(ProviderCommand::SelectRoute {
        binding: "other".to_owned(),
    });
    assert_eq!(
        RuntimeSessionController::operation_id(&changed)?,
        RuntimeSessionController::operation_id(&fixture.request)?
    );
    assert!(controller.reconcile(&changed).is_err());
    Ok(())
}

#[test]
fn ready_cannot_activate_after_another_route_revision_wins() -> Result<()> {
    let fixture = Fixture::new(false)?;
    let controller = fixture.controller()?;
    fixture.bind(&controller)?;
    let store = fixture.store.clone();
    let (_, route, trust) = ResolvedRouteConfigSnapshot::from_root_config(&fixture.config)
        .resolved_route(&fixture.route.model_ref)
        .expect("configured route should resolve for the competing revision");
    *fixture
        .host
        .on_ready
        .lock()
        .expect("controller readiness callback mutex should not be poisoned") =
        Some(Box::new(move || {
            store.append_session_entry_event(&SessionLogEntry::Control(
                ControlEntry::SessionRouteTrustBound {
                    route_semantic_fingerprint: route.semantic_fingerprint,
                    egress_trust_binding: trust,
                },
            ))?;
            Ok(())
        }));
    assert!(controller.dispatch(&fixture.request).is_err());
    assert!(fixture.replay()?.configured.is_some());
    assert!(fixture.replay()?.activated.is_none());
    assert!(
        !fixture
            .host
            .trace
            .lock()
            .expect("controller trace mutex should not be poisoned")
            .contains(&"open")
    );
    Ok(())
}

#[test]
fn reopened_transition_cannot_start_a_route_from_changed_configuration() -> Result<()> {
    let mut fixture = Fixture::new(false)?;
    let controller = fixture.controller()?;
    fixture.bind(&controller)?;
    fixture.host.fail_start.store(true, Ordering::SeqCst);
    assert!(controller.dispatch(&fixture.request).is_err());
    let before = fixture.replay()?;
    assert!(before.configured.is_some());
    drop(controller);
    fixture
        .config
        .connections
        .get_mut("local")
        .expect("connection")["base_url"] = serde_json::json!("https://changed.example.test/v1");
    assert!(fixture.controller()?.dispatch(&fixture.request).is_err());
    let after = fixture.replay()?;
    assert_eq!(after.intent, before.intent);
    assert_eq!(after.configured, before.configured);
    assert!(after.activated.is_none());
    assert_eq!(fixture.host.starts.load(Ordering::SeqCst), 1);
    Ok(())
}
