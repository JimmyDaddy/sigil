use super::*;
use futures::future::BoxFuture;
use sigil_application::*;
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
    mpsc,
};

struct SlowAdmissionPort {
    snapshot: Arc<Mutex<ProjectionSnapshot>>,
    release: Arc<Mutex<Option<mpsc::Receiver<()>>>>,
    started: mpsc::Sender<()>,
    calls: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<ApplicationCommandRequest>>>,
}

impl ApplicationPort for SlowAdmissionPort {
    fn open_projection(
        &self,
        _: OpenProjectionRequest,
    ) -> BoxFuture<'static, Result<ProjectionSnapshot, ApplicationError>> {
        let mut snapshot = self.snapshot.lock().expect("snapshot lock").clone();
        snapshot.feed = vec![ProjectionFeedItem::CurrentState];
        Box::pin(async move { Ok(snapshot) })
    }
    fn page(
        &self,
        _: ProjectionPageRequest,
    ) -> BoxFuture<'static, Result<ProjectionPage, ApplicationError>> {
        Box::pin(async { Err(ApplicationError::NotFound) })
    }
    fn cancel_page(
        &self,
        _request_id: PageRequestId,
    ) -> BoxFuture<'static, PageCancellationReceipt> {
        Box::pin(async move { PageCancellationReceipt::CancelledBeforeLoad })
    }
    fn acknowledge(
        &self,
        _: ProjectionDeliveryAck,
    ) -> BoxFuture<'static, Result<(), ApplicationError>> {
        Box::pin(async { Ok(()) })
    }
    fn execute(
        &self,
        request: ApplicationCommandRequest,
    ) -> BoxFuture<'static, Result<ApplicationCommandReceipt, ApplicationError>> {
        let calls = Arc::clone(&self.calls);
        let requests = Arc::clone(&self.requests);
        let release = Arc::clone(&self.release);
        let started = self.started.clone();
        Box::pin(async move {
            calls.fetch_add(1, Ordering::SeqCst);
            requests.lock().expect("request log").push(request.clone());
            let _ = started.send(());
            if let Some(release) = release.lock().expect("gate lock").take() {
                release.recv().expect("release slow reservation");
                return Err(ApplicationError::Unavailable);
            }
            Ok(ApplicationCommandReceipt::Uncertain(
                UncertainCommandReceipt {
                    command_id: request.envelope.command_id.clone(),
                    command_kind: request.envelope.command.kind().to_owned(),
                    reservation_fingerprint: command_fingerprint(&request)?,
                    recovery: CommandRecoveryBinding {
                        key: request
                            .admission
                            .reservation_key(&request.envelope.command_id),
                        phase: CommandLifecyclePhase::EffectStarted,
                    },
                    owner_recovery_binding: Some("test-worker".to_owned()),
                },
            ))
        })
    }
}

fn wait_admission(app: &mut AppState, worker: &mut Option<WorkerRuntime>) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        poll_application_admission(app, worker)?;
        if worker
            .as_ref()
            .and_then(|worker| worker.pending_admission.as_ref())
            .is_some_and(|pending| {
                pending.receiver.is_none()
                    && pending
                        .handle
                        .as_ref()
                        .is_none_or(|handle| handle.is_finished())
            })
        {
            return Ok(());
        }
        anyhow::ensure!(Instant::now() < deadline, "admission did not settle");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn revise_admission_is_nonblocking_deduplicated_and_retries_the_original_envelope() -> Result<()> {
    let scope = ApplicationScope {
        application_instance: ApplicationInstanceId::new("slow-ui")?,
        authenticated_subject: AuthenticatedSubject::new("local")?,
        workspace: Some(WorkspaceScopeId::new("workspace")?),
        session: Some(SessionScopeId::new("session")?),
    };
    let snapshot = Arc::new(Mutex::new(crate::application_bridge::tests::snapshot(
        scope.clone(),
    )));
    let (release_tx, release_rx) = mpsc::channel();
    let (started_tx, started_rx) = mpsc::channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let port = Arc::new(SlowAdmissionPort {
        snapshot: Arc::clone(&snapshot),
        release: Arc::new(Mutex::new(Some(release_rx))),
        started: started_tx,
        calls: Arc::clone(&calls),
        requests: Arc::clone(&requests),
    });
    let application = Arc::new(crate::application_bridge::tests::session(port, scope)?);
    futures::executor::block_on(application.refresh())?;
    assert!(application.current_projection()?.is_some());
    let mut app = AppState::from_root_config(
        Path::new("sigil.toml"),
        &crate::app::tests::common::test_config(),
    );
    let (worker_tx, worker_rx) = runner::WorkerCommandSender::test_channel();
    let mut worker = Some(WorkerRuntime {
        worker_tx,
        application: Some(Arc::clone(&application)),
        pending_admission: None,
        pending_interactions: Vec::new(),
        worker_rx: mpsc::channel().1,
        join_handle: None,
        ready: true,
    });
    let revise = AppAction::RevisePlan {
        plan_id: "plan-one".to_owned(),
        expected_plan_hash: "plan-hash".to_owned(),
    };
    let started = Instant::now();
    process_app_action(&mut app, &mut worker, revise.clone())?;
    assert!(started.elapsed() < Duration::from_millis(100));
    started_rx.recv_timeout(Duration::from_secs(2))?;
    process_app_action(&mut app, &mut worker, revise.clone())?;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let original = worker
        .as_ref()
        .expect("worker")
        .pending_admission
        .as_ref()
        .expect("pending")
        .request
        .clone();
    process_app_action(&mut app, &mut worker, AppAction::CancelRun)?;
    assert!(matches!(
        worker_rx.recv_timeout(Duration::from_millis(100))?,
        WorkerCommand::CancelRun
    ));
    release_tx.send(())?;
    wait_admission(&mut app, &mut worker)?;
    {
        let mut changed = snapshot.lock().expect("snapshot");
        changed.envelope.cut.through_sequence += 1;
        changed.envelope.cut.durable_cursor = "later-cut".to_owned();
        changed.envelope.projection.frontier = changed.envelope.cut.clone();
    }
    futures::executor::block_on(application.refresh())?;
    process_app_action(&mut app, &mut worker, revise.clone())?;
    wait_admission(&mut app, &mut worker)?;
    assert_eq!(
        &requests.lock().expect("requests")[..],
        &[original.clone(), original]
    );
    let failure = |hash: &str| WorkerMessage::PlanActionFailed {
        action: sigil_kernel::PublicPlanAction::Revise,
        plan_id: "plan-one".to_owned(),
        expected_plan_hash: hash.to_owned(),
        message: "revision cannot start yet".to_owned(),
        entries: None,
    };
    apply_worker_message_state(
        worker.as_mut().expect("worker"),
        None,
        &failure("a-different-plan-hash"),
    );
    assert!(
        !worker
            .as_ref()
            .expect("worker")
            .pending_admission
            .as_ref()
            .expect("pending")
            .domain_resolved,
        "a failure for a different plan hash cannot settle the pending command"
    );
    process_app_action(&mut app, &mut worker, revise.clone())?;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "Uncertain keeps the original command without redispatch"
    );
    apply_worker_message_state(
        worker.as_mut().expect("worker"),
        None,
        &failure("plan-hash"),
    );
    process_app_action(&mut app, &mut worker, revise)?;
    wait_admission(&mut app, &mut worker)?;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        3,
        "exact domain failure permits a new revision command"
    );
    apply_worker_message_state(
        worker.as_mut().expect("worker"),
        None,
        &failure("plan-hash"),
    );
    let next_revision = AppAction::RevisePlan {
        plan_id: "next-plan".to_owned(),
        expected_plan_hash: "next-hash".to_owned(),
    };
    process_app_action(&mut app, &mut worker, next_revision.clone())?;
    wait_admission(&mut app, &mut worker)?;
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    assert!(same_application_interaction(
        &worker
            .as_ref()
            .expect("worker")
            .pending_admission
            .as_ref()
            .expect("new revision")
            .action,
        &next_revision,
    ));
    app.join_session_auxiliary_until(Instant::now() + Duration::from_secs(1))?;
    shutdown_and_join_worker(&mut worker)?;
    Ok(())
}

#[test]
fn production_shutdown_restores_terminal_before_join_and_reports_a_stuck_worker() -> Result<()> {
    let (worker_tx, commands) = runner::WorkerCommandSender::test_channel();
    let (release_tx, release_rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        release_rx.recv().expect("test release");
    });
    let mut worker = Some(WorkerRuntime {
        worker_tx,
        application: None,
        pending_admission: None,
        pending_interactions: Vec::new(),
        worker_rx: mpsc::channel().1,
        join_handle: Some(handle),
        ready: true,
    });
    let started = Instant::now();
    let (restored, joined) =
        restore_terminal_then_join_worker(&mut worker, started + Duration::from_millis(60), || {
            assert!(
                matches!(commands.try_recv(), Err(mpsc::TryRecvError::Empty)),
                "terminal restoration precedes even the queued cleanup work"
            );
            Ok(())
        });
    restored?;
    assert!(
        joined
            .expect_err("stuck worker cannot exit cleanly")
            .to_string()
            .contains("cleanup_complete=false")
    );
    assert!(started.elapsed() < Duration::from_millis(200));
    assert!(
        worker
            .as_ref()
            .expect("retain worker after timeout")
            .join_handle
            .is_some()
    );
    release_tx.send(())?;
    shutdown_and_join_worker_until(&mut worker, Instant::now() + Duration::from_secs(1))?;
    assert!(worker.is_none());
    Ok(())
}

#[test]
fn queue_and_save_admission_remain_responsive_and_retry_the_frozen_request() -> Result<()> {
    for action in [
        AppAction::SetConversationQueuePaused { paused: true },
        AppAction::SavePlan {
            plan_id: "plan-one".to_owned(),
            expected_plan_hash: "plan-hash".to_owned(),
        },
    ] {
        let scope = ApplicationScope {
            application_instance: ApplicationInstanceId::new("interactive-ui")?,
            authenticated_subject: AuthenticatedSubject::new("local")?,
            workspace: Some(WorkspaceScopeId::new("workspace")?),
            session: Some(SessionScopeId::new("session")?),
        };
        let snapshot = Arc::new(Mutex::new(crate::application_bridge::tests::snapshot(
            scope.clone(),
        )));
        let (release_tx, release_rx) = mpsc::channel();
        let (started_tx, started_rx) = mpsc::channel();
        let calls = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let port = Arc::new(SlowAdmissionPort {
            snapshot: Arc::clone(&snapshot),
            release: Arc::new(Mutex::new(Some(release_rx))),
            started: started_tx,
            calls: Arc::clone(&calls),
            requests: Arc::clone(&requests),
        });
        let application = Arc::new(crate::application_bridge::tests::session(port, scope)?);
        futures::executor::block_on(application.refresh())?;
        let mut app = AppState::from_root_config(
            Path::new("sigil.toml"),
            &crate::app::tests::common::test_config(),
        );
        app.runtime.is_busy = true;
        let (worker_tx, worker_rx) = runner::WorkerCommandSender::test_channel();
        let mut worker = Some(WorkerRuntime {
            worker_tx,
            application: Some(Arc::clone(&application)),
            pending_admission: None,
            pending_interactions: Vec::new(),
            worker_rx: mpsc::channel().1,
            join_handle: None,
            ready: true,
        });
        let generic_input = AppAction::SubmitUserInputDecision {
            command_id: Some("generic-input".to_owned()),
            request_id: "generic-request".to_owned(),
            generation: 1,
            expected_request_hash: "request-hash".to_owned(),
            decision: sigil_kernel::UserInputDecisionV1::RunCancelled,
        };
        assert!(!queue_application_interaction(
            &mut app,
            &mut worker,
            &generic_input
        )?);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let started = Instant::now();
        process_app_action(&mut app, &mut worker, action.clone())?;
        assert!(started.elapsed() < Duration::from_millis(100));
        started_rx.recv_timeout(Duration::from_secs(2))?;
        process_app_action(&mut app, &mut worker, action.clone())?;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let original = worker.as_ref().expect("worker").pending_interactions[0]
            .request
            .clone();
        process_app_action(&mut app, &mut worker, AppAction::CancelRun)?;
        assert!(matches!(
            worker_rx.recv_timeout(Duration::from_millis(100))?,
            WorkerCommand::CancelRun
        ));
        release_tx.send(())?;
        wait_interaction_admission(&mut app, &mut worker)?;
        assert!(app.runtime.is_busy);
        {
            let mut changed = snapshot.lock().expect("snapshot");
            changed.envelope.cut.through_sequence += 1;
            changed.envelope.cut.durable_cursor = "later-cut".to_owned();
            changed.envelope.projection.frontier = changed.envelope.cut.clone();
        }
        futures::executor::block_on(application.refresh())?;
        process_app_action(&mut app, &mut worker, action.clone())?;
        wait_interaction_admission(&mut app, &mut worker)?;
        assert_eq!(
            &requests.lock().expect("requests")[..],
            &[original.clone(), original]
        );
        process_app_action(&mut app, &mut worker, action)?;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "uncertain admission must not redispatch"
        );
        app.join_session_auxiliary_until(Instant::now() + Duration::from_secs(1))?;
        shutdown_and_join_worker(&mut worker)?;
    }
    Ok(())
}

fn wait_interaction_admission(
    app: &mut AppState,
    worker: &mut Option<WorkerRuntime>,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        poll_application_admission(app, worker)?;
        if worker.as_ref().is_some_and(|runtime| {
            runtime.pending_interactions.iter().all(|pending| {
                pending.receiver.is_none()
                    && pending
                        .handle
                        .as_ref()
                        .is_none_or(|handle| handle.is_finished())
            })
        }) {
            return Ok(());
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "interaction admission did not settle"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn worker_join_uses_the_original_deadline_without_starting_a_new_budget() -> Result<()> {
    let (release_tx, release_rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        release_rx.recv().expect("test release");
    });
    let started = Instant::now();
    assert!(wait_for_worker_thread(Some(handle), started - Duration::from_millis(1)).is_err());
    assert!(started.elapsed() < Duration::from_millis(50));
    release_tx.send(())?;
    Ok(())
}

#[test]
fn terminal_restore_precedes_owned_runtime_shutdown_without_waiting_for_blocking_io() -> Result<()>
{
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (finished_tx, finished_rx) = mpsc::channel();
    runtime.spawn_blocking(move || {
        entered_tx.send(()).expect("blocking query entered");
        release_rx.recv().expect("release query");
        finished_tx.send(()).expect("query finished");
    });
    entered_rx.recv_timeout(Duration::from_secs(2))?;
    let mut runtime = Some(runtime);
    let mut restored = false;
    let started = Instant::now();
    restore_terminal_and_shutdown_event_runtime(&mut runtime, || {
        restored = true;
        Ok(())
    })?;
    assert!(restored);
    assert!(runtime.is_none());
    assert!(started.elapsed() < Duration::from_millis(100));
    assert!(matches!(
        finished_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    release_tx.send(())?;
    finished_rx.recv_timeout(Duration::from_secs(2))?;
    Ok(())
}

#[test]
fn dropping_projection_task_aborts_the_observation_on_error_paths() -> Result<()> {
    struct OnDrop(Arc<std::sync::atomic::AtomicBool>);
    impl Drop for OnDrop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;
    for explicit_abort in [false, true] {
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let probe = OnDrop(Arc::clone(&dropped));
        let task: AbortOnDropTask<()> = runtime
            .spawn(async move {
                let _probe = probe;
                std::future::pending::<()>().await;
            })
            .into();
        runtime.block_on(tokio::task::yield_now());
        assert!(!task.is_finished());
        if explicit_abort {
            task.abort();
        }
        drop(task);
        runtime.block_on(tokio::task::yield_now());
        assert!(dropped.load(Ordering::SeqCst));
    }
    Ok(())
}

#[test]
fn production_dispatch_keeps_input_p95_below_100ms_during_live_and_worker_flood() -> Result<()> {
    use sigil_kernel::EventHandler;
    let fixture = tempfile::tempdir()?;
    let store = sigil_kernel::JsonlSessionStore::new(fixture.path().join("session.jsonl"))?;
    let session = sigil_kernel::Session::load_from_store("fixture", "model", store)?;
    let mut recorder =
        sigil_runtime::ApplicationRunEventRecorder::start(&session, "p95-run", "input")?;
    recorder.begin_live_attempt("p95-attempt")?;
    let mut config = crate::app::tests::common::test_config();
    config.workspace.root = fixture.path().display().to_string();
    let mut app = AppState::from_root_config(&fixture.path().join("sigil.toml"), &config);
    app.session_id = session.session_scope_id().to_owned();
    app.session_log_path = session.store_path().expect("durable session").to_owned();
    let (messages_tx, messages_rx) = mpsc::channel();
    messages_tx.send(WorkerMessage::LivePreviewSource {
        source: recorder.live_preview_source(),
    })?;
    messages_tx.send(WorkerMessage::LivePreviewDurableFrontier {
        session_id: app.session_id.clone(),
        run_id: "p95-run".to_owned(),
        sequence: recorder.public_sequence()?,
    })?;
    const WORKER_MESSAGES: usize = 8_192;
    for _ in 0..WORKER_MESSAGES {
        messages_tx.send(WorkerMessage::Notice("worker progress".to_owned()))?;
    }
    let (worker_tx, _commands) = runner::WorkerCommandSender::test_channel();
    let mut worker = Some(WorkerRuntime {
        worker_tx,
        application: None,
        pending_admission: None,
        pending_interactions: Vec::new(),
        worker_rx: messages_rx,
        join_handle: None,
        ready: true,
    });
    const DELTAS: usize = 100_000;
    let producer = std::thread::spawn(move || -> Result<()> {
        for index in 0..DELTAS {
            recorder.handle(sigil_kernel::RunEvent::TextDelta("x".to_owned()))?;
            if index.is_multiple_of(512) {
                std::thread::yield_now();
            }
        }
        Ok(())
    });
    let mut latencies = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    while latencies.len() < 128 || !producer.is_finished() {
        anyhow::ensure!(Instant::now() < deadline, "live flood failed to finish");
        let before = app.composer.input.len();
        let started = Instant::now();
        // These are the same drain, background polling, and keyboard methods run_app dispatches.
        drain_worker_messages_inner(&mut app, &mut worker, None)?;
        app.poll_background_tasks();
        app.handle_key_event(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('z'),
            crossterm::event::KeyModifiers::NONE,
        ))?;
        assert_eq!(app.composer.input.len(), before + 1);
        latencies.push(started.elapsed());
        app.handle_key_event(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Backspace,
            crossterm::event::KeyModifiers::NONE,
        ))?;
        std::thread::sleep(Duration::from_millis(1));
    }
    producer.join().expect("live producer joined")?;
    latencies.sort_unstable();
    let p95 = latencies[(latencies.len() * 95).div_ceil(100) - 1];
    eprintln!(
        "tui production_dispatch delta_count={DELTAS} worker_messages={WORKER_MESSAGES} samples={} input_p95_us={}",
        latencies.len(),
        p95.as_micros()
    );
    assert!(p95 <= Duration::from_millis(100));
    app.join_session_auxiliary_until(Instant::now() + Duration::from_secs(2))?;
    shutdown_and_join_worker(&mut worker)?;
    Ok(())
}

#[tokio::test]
async fn retiring_a_scope_waits_for_its_actual_blocking_ack_after_the_future_is_aborted()
-> Result<()> {
    use sigil_runtime::RuntimeApplicationProjectionSource;
    struct HeldAck {
        entered: tokio::sync::Notify,
        release: Mutex<mpsc::Receiver<()>>,
    }
    impl sigil_kernel::session::ActiveProjectionObserver for HeldAck {
        fn active_projection_changed(&self, _: sigil_kernel::session::ActiveProjectionNotice) {
            self.entered.notify_one();
            self.release
                .lock()
                .expect("release lock")
                .recv_timeout(Duration::from_secs(5))
                .expect("release committed ACK");
        }
    }
    let fixture = tempfile::tempdir()?;
    let mut config = crate::app::tests::common::test_config();
    config.workspace.root = fixture.path().display().to_string();
    let config_path = fixture.path().join("sigil.toml");
    std::fs::write(&config_path, toml::to_string(&config)?)?;
    let (provider, route) =
        sigil_runtime::provider_connections::resolve_default_model_route(&config)?;
    let store = sigil_kernel::JsonlSessionStore::new(fixture.path().join("session.jsonl"))?;
    let mut session =
        sigil_kernel::Session::new_with_route(provider, route).with_store(store.clone());
    session.ensure_identity_entry()?;
    sigil_runtime::bind_session_composition(&mut session, &config)?;
    let _recorder =
        sigil_runtime::ApplicationRunEventRecorder::start(&session, "old-run", "input")?;
    let scope = ApplicationScope {
        application_instance: ApplicationInstanceId::new("scope-drain")?,
        authenticated_subject: AuthenticatedSubject::new("local")?,
        workspace: Some(WorkspaceScopeId::new("workspace")?),
        session: Some(SessionScopeId::new(session.session_scope_id())?),
    };
    let binding = Arc::new(
        sigil_runtime::RuntimeSessionProjectionBinding::new(
            config_path,
            fixture.path().to_owned(),
            store.path().to_owned(),
            session.session_scope_id().to_owned(),
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
    let snapshot = binding
        .open_projection(OpenProjectionRequest {
            scope: scope.clone(),
            observer_generation: 1,
            resume_from: None,
        })
        .await?;
    let outbox = sigil_kernel::PublicEventOutboxProjectionV1::from_records(
        &store.read_handle().read_event_records()?,
    )?;
    let event_ids = outbox
        .pending_for_adapter("tui")
        .into_iter()
        .map(|entry| entry.public_event_id.clone())
        .collect::<Vec<_>>();
    assert!(!event_ids.is_empty());
    let (started, _) = mpsc::channel();
    let port: Arc<dyn ApplicationPort> = Arc::new(SlowAdmissionPort {
        snapshot: Arc::new(Mutex::new(crate::application_bridge::tests::snapshot(
            scope.clone(),
        ))),
        release: Arc::new(Mutex::new(None)),
        started,
        calls: Arc::default(),
        requests: Arc::default(),
    });
    let old = Arc::new(
        crate::application_bridge::tests::session_with_projection_binding(
            Arc::clone(&port),
            scope.clone(),
            binding,
        )?,
    );
    let mut next_scope = scope;
    next_scope.session = Some(SessionScopeId::new("next-session")?);
    let next = Arc::new(crate::application_bridge::tests::session(port, next_scope)?);
    let (release, receiver) = mpsc::channel();
    let observer = Arc::new(HeldAck {
        entered: tokio::sync::Notify::new(),
        release: Mutex::new(receiver),
    });
    let subscription = store.register_active_projection_observer(observer.clone());
    let acknowledgement = Arc::clone(&old);
    let task = tokio::spawn(async move {
        acknowledgement
            .acknowledge_public_events(event_ids, &snapshot.envelope.cut)
            .await
    });
    let mut owners = Vec::new();
    retain_projection_observation(&mut owners, Arc::clone(&old), task.abort_handle());
    tokio::time::timeout(Duration::from_secs(2), observer.entered.notified()).await?;
    task.abort();
    assert!(
        task.await
            .expect_err("outer ACK future is aborted")
            .is_cancelled()
    );
    assert_eq!(old.pending_observations(), 1);
    assert_eq!(next.pending_observations(), 0);
    let (worker_tx, _commands) = runner::WorkerCommandSender::test_channel();
    let mut worker = Some(WorkerRuntime {
        worker_tx,
        application: Some(next),
        pending_admission: None,
        pending_interactions: Vec::new(),
        worker_rx: mpsc::channel().1,
        join_handle: None,
        ready: true,
    });
    shutdown_and_join_worker(&mut worker)?;
    assert!(
        drain_projection_observations_until(
            &mut owners,
            Instant::now() + Duration::from_millis(40)
        )
        .expect_err("retired scope is still writing its admitted ACK")
        .to_string()
        .contains("cleanup_complete=false")
    );
    assert_eq!(old.pending_observations(), 1);
    release.send(())?;
    drain_projection_observations_until(&mut owners, Instant::now() + Duration::from_secs(2))?;
    assert!(owners.is_empty());
    assert_eq!(old.pending_observations(), 0);
    drop(subscription);
    eprintln!(
        "tui retired_scope old_pending_after_abort=1 current_pending=0 cleanup_before_release=false cleanup_after_release=true"
    );
    Ok(())
}

#[test]
fn disconnected_worker_shutdown_accounts_for_blocked_interaction_admission() -> Result<()> {
    let scope = ApplicationScope {
        application_instance: ApplicationInstanceId::new("disconnected-ui")?,
        authenticated_subject: AuthenticatedSubject::new("local")?,
        workspace: Some(WorkspaceScopeId::new("workspace")?),
        session: Some(SessionScopeId::new("session")?),
    };
    let (release_tx, release_rx) = mpsc::channel();
    let (started_tx, started_rx) = mpsc::channel();
    let port = Arc::new(SlowAdmissionPort {
        snapshot: Arc::new(Mutex::new(crate::application_bridge::tests::snapshot(
            scope.clone(),
        ))),
        release: Arc::new(Mutex::new(Some(release_rx))),
        started: started_tx,
        calls: Arc::new(AtomicUsize::new(0)),
        requests: Arc::new(Mutex::new(Vec::new())),
    });
    let application = Arc::new(crate::application_bridge::tests::session(port, scope)?);
    futures::executor::block_on(application.refresh())?;
    let action = AppAction::SavePlan {
        plan_id: "plan-one".to_owned(),
        expected_plan_hash: "plan-hash".to_owned(),
    };
    let request = application
        .prepare_action(&action, None, None)?
        .expect("save plan has an application command");
    let mut pending = PendingApplicationAdmission {
        application: Arc::clone(&application),
        request,
        action,
        receiver: None,
        handle: None,
        retryable: true,
        domain_resolved: false,
    };
    pending.start()?;
    started_rx.recv_timeout(Duration::from_secs(2))?;
    // Observe the fixture's eventual exit while a timed-out shutdown retains its owner.
    let result_rx = pending.receiver.take().expect("admission response");
    let (worker_tx, _commands) = runner::WorkerCommandSender::test_channel();
    let (message_tx, worker_rx) = mpsc::channel();
    let mut worker = Some(WorkerRuntime {
        worker_tx,
        application: Some(application),
        pending_admission: None,
        pending_interactions: vec![pending],
        worker_rx,
        join_handle: None,
        ready: true,
    });
    drop(message_tx);
    assert!(matches!(
        worker.as_ref().expect("worker").worker_rx.try_recv(),
        Err(mpsc::TryRecvError::Disconnected)
    ));
    let started = Instant::now();
    let outcome = shutdown_and_join_worker_until(&mut worker, started + Duration::from_millis(30));
    let elapsed = started.elapsed();
    let admission_still_blocked = matches!(result_rx.try_recv(), Err(mpsc::TryRecvError::Empty));

    release_tx.send(())?;
    let admission_result = result_rx.recv_timeout(Duration::from_secs(2))?;
    assert!(matches!(
        admission_result,
        Err(ApplicationError::Unavailable)
    ));
    assert!(matches!(
        result_rx.recv_timeout(Duration::from_secs(2)),
        Err(mpsc::RecvTimeoutError::Disconnected)
    ));
    assert!(admission_still_blocked);
    assert!(
        worker.is_some(),
        "an unfinished admission retains its owning runtime"
    );
    assert!(
        outcome
            .expect_err("worker disconnect does not finish its admission thread")
            .to_string()
            .contains("cleanup_complete=false")
    );
    assert!(
        elapsed < Duration::from_secs(1),
        "admission cleanup must use the supplied shared deadline"
    );
    shutdown_and_join_worker_until(&mut worker, Instant::now() + Duration::from_secs(1))?;
    assert!(worker.is_none());
    Ok(())
}

#[test]
fn joined_worker_panic_cannot_be_erased_by_retrying_shutdown() -> Result<()> {
    let (worker_tx, _commands) = runner::WorkerCommandSender::test_channel();
    let handle = std::thread::spawn(|| panic!("worker shutdown fixture panic"));
    let mut worker = Some(WorkerRuntime {
        worker_tx,
        application: None,
        pending_admission: None,
        pending_interactions: Vec::new(),
        worker_rx: mpsc::channel().1,
        join_handle: Some(handle),
        ready: true,
    });
    for _ in 0..2 {
        let error =
            shutdown_and_join_worker_until(&mut worker, Instant::now() + Duration::from_secs(1))
                .expect_err("join panic remains cleanup-incomplete");
        assert!(error.to_string().contains("stage=owned-thread-join"));
        assert!(error.to_string().contains("cleanup_complete=false"));
        assert!(
            worker
                .as_ref()
                .expect("retain failed worker state")
                .join_handle
                .is_none(),
            "panic join result was consumed"
        );
    }
    Ok(())
}
