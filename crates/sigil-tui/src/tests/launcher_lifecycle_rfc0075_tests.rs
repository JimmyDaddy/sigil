use super::*;
use futures::future::BoxFuture;
use sigil_application::*;
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
    mpsc,
};

struct SlowAdmissionPort {
    settle: bool,
    snapshot: Arc<Mutex<ProjectionSnapshot>>,
    release: Arc<Mutex<Option<mpsc::Receiver<()>>>>,
    started: mpsc::Sender<()>,
    calls: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<ApplicationCommandRequest>>>,
}

// Declare after the worker owner: unwinding must release this one fixture gate before joining
// the admission thread. Taking the sender on explicit release prevents a second signal on Drop.
struct AdmissionGateRelease(Option<mpsc::Sender<()>>);

impl AdmissionGateRelease {
    fn release(&mut self) -> Result<()> {
        if let Some(sender) = self.0.take() {
            sender.send(())?;
        }
        Ok(())
    }
}

impl Drop for AdmissionGateRelease {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}

impl ApplicationPort for SlowAdmissionPort {
    fn recover_control_log(
        &self,
        _: ApplicationScope,
        _: ControlLogRecoveryAction,
    ) -> BoxFuture<'static, Result<ControlLogRecoveryOutcome, ApplicationError>> {
        let started = self.started.clone();
        let release = Arc::clone(&self.release);
        Box::pin(async move {
            let _ = started.send(());
            if let Some(release) = release.lock().expect("recovery gate").take() {
                release
                    .recv_timeout(Duration::from_secs(5))
                    .expect("release recovery");
            }
            Err(ApplicationError::Unavailable)
        })
    }
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
        let settle = self.settle;
        let snapshot = Arc::clone(&self.snapshot);
        Box::pin(async move {
            calls.fetch_add(1, Ordering::SeqCst);
            requests.lock().expect("request log").push(request.clone());
            let _ = started.send(());
            let release = release.lock().expect("gate lock").take();
            if let Some(release) = release {
                tokio::task::spawn_blocking(move || {
                    release.recv().expect("release slow reservation");
                })
                .await
                .expect("join slow reservation");
                if !settle {
                    return Err(ApplicationError::Unavailable);
                }
            }
            if settle {
                let mut frontier = snapshot.lock().expect("snapshot").envelope.cut.clone();
                frontier.through_sequence = frontier.through_sequence.max(1);
                return Ok(ApplicationCommandReceipt::Settled(
                    ApplicationDomainReceipt {
                        command_id: request.envelope.command_id.clone(),
                        command_kind: request.envelope.command.kind().to_owned(),
                        frontier,
                        settlement: request.envelope.command.policy().settlement,
                        summary: "fixture publication committed".to_owned(),
                        domain_commit: ApplicationDomainCommitRef {
                            source_session_scope_id: None,
                            source_event_id: "fixture-config-commit".to_owned(),
                            source_sequence: 1,
                            source_digest: "a".repeat(64),
                        },
                        outcome: None,
                    },
                ));
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

#[test]
fn control_log_recovery_keeps_input_responsive_without_projection_and_joins_on_exit() -> Result<()>
{
    let scope = ApplicationScope {
        application_instance: ApplicationInstanceId::new("recovery-ui")?,
        authenticated_subject: AuthenticatedSubject::new("local")?,
        workspace: Some(WorkspaceScopeId::new("workspace")?),
        session: Some(SessionScopeId::new("session")?),
    };
    let (release_tx, release_rx) = mpsc::channel();
    let (started_tx, started_rx) = mpsc::channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let port = Arc::new(SlowAdmissionPort {
        settle: false,
        snapshot: Arc::new(Mutex::new(crate::application_bridge::tests::snapshot(
            scope.clone(),
        ))),
        release: Arc::new(Mutex::new(Some(release_rx))),
        started: started_tx,
        calls: Arc::clone(&calls),
        requests: Arc::new(Mutex::new(Vec::new())),
    });
    let application = Arc::new(crate::application_bridge::tests::session(port, scope)?);
    assert!(application.current_projection()?.is_none());
    let mut app = AppState::from_root_config(
        Path::new("sigil.toml"),
        &crate::app::tests::common::test_config(),
    );
    let (worker_tx, _commands) = runner::WorkerCommandSender::test_channel();
    let mut worker = Some(WorkerRuntime {
        worker_tx,
        application: Some(application),
        pending_admission: None,
        pending_interactions: Vec::new(),
        worker_rx: mpsc::channel().1,
        join_handle: None,
        ready: true,
    });
    let started = Instant::now();
    process_app_action(
        &mut app,
        &mut worker,
        AppAction::RecoverControlLog(ControlLogRecoveryAction::Preview),
    )?;
    assert!(started.elapsed() < Duration::from_millis(100));
    started_rx.recv_timeout(Duration::from_secs(2))?;
    for _ in 0..128 {
        let started = Instant::now();
        assert!(!control_log_recovery::poll(&mut app)?);
        app.handle_key_event(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('x'),
            crossterm::event::KeyModifiers::NONE,
        ))?;
        assert!(started.elapsed() < Duration::from_millis(100));
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "recovery must not enter ordinary command admission"
    );
    let replacement = AppState::from_root_config(
        Path::new("sigil.toml"),
        &crate::app::tests::common::test_config(),
    );
    let replacement_started = Instant::now();
    control_log_recovery::replace_app_state(&mut app, replacement);
    assert!(replacement_started.elapsed() < Duration::from_millis(100));
    assert!(!control_log_recovery::poll(&mut app)?);
    let mut recovery = std::mem::take(&mut app.control_log_recovery);
    let (joining_tx, joining_rx) = mpsc::channel();
    let cleanup = std::thread::spawn(move || {
        joining_tx.send(()).expect("joining");
        recovery.finish_shutdown()
    });
    joining_rx.recv_timeout(Duration::from_secs(2))?;
    assert!(
        !cleanup.is_finished(),
        "shutdown must retain its in-flight recovery owner"
    );
    release_tx.send(())?;
    cleanup.join().expect("recovery cleanup joined")?;
    shutdown_and_join_worker(&mut worker)?;
    Ok(())
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
        settle: false,
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
        .lock()
        .expect("frozen request")
        .clone()
        .expect("prepared request");
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
            .receipt_resolved,
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
        "a matching publication requests reconciliation of the original command"
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
    let retained = worker
        .as_ref()
        .expect("worker")
        .pending_admission
        .as_ref()
        .expect("unresolved original revision");
    assert!(!same_application_interaction(
        &retained.action,
        &next_revision
    ));
    assert!(
        !retained.receipt_resolved,
        "matching UI failures cannot replace a durable receipt"
    );
    let recorded = requests.lock().expect("recorded requests");
    assert!(
        recorded.iter().all(|request| request == &recorded[0]),
        "every reconciliation preserves the original K/F"
    );
    drop(recorded);
    app.join_session_auxiliary_until(Instant::now() + Duration::from_secs(1))?;
    shutdown_and_join_worker(&mut worker)?;
    Ok(())
}

#[test]
fn bounded_non_exit_shutdown_restores_terminal_and_retains_a_pending_worker() -> Result<()> {
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
    let (exited, exit_result) = mpsc::channel();
    let exit_owner = std::thread::spawn(move || {
        // The outer launcher propagates its deadline error with this owner still in scope.
        drop(worker);
        let _ = exited.send(());
    });
    assert!(matches!(
        exit_result.recv_timeout(Duration::from_millis(40)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    release_tx.send(())?;
    exit_result.recv_timeout(Duration::from_secs(1))?;
    exit_owner.join().expect("outer exit owner");
    Ok(())
}

#[test]
fn queue_enqueue_and_save_admission_remain_responsive_and_retry_the_frozen_request() -> Result<()> {
    for action in [
        AppAction::SubmitPrompt("hello".to_owned()),
        AppAction::SubmitPlanPrompt("review the code".to_owned()),
        AppAction::SubmitTask("implement the change".to_owned()),
        AppAction::ContinueTask {
            task_id: None,
            guidance: Some("continue carefully".to_owned()),
        },
        AppAction::QueueConversationInput {
            prompt: "continue after this run".to_owned(),
            kind: sigil_kernel::ConversationInputKind::Chat,
            target: sigil_kernel::ConversationInputTarget::MainThread,
        },
        AppAction::SetConversationQueuePaused { paused: true },
        AppAction::SavePlan {
            plan_id: "plan-one".to_owned(),
            expected_plan_hash: "plan-hash".to_owned(),
        },
        AppAction::SubmitUserInputDecision {
            command_id: Some("generic-input".to_owned()),
            request_id: "generic-request".to_owned(),
            generation: 1,
            expected_request_hash: "request-hash".to_owned(),
            decision: sigil_kernel::UserInputDecisionV1::RunCancelled,
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
            settle: false,
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
        let started = Instant::now();
        process_app_action(&mut app, &mut worker, action.clone())?;
        assert!(started.elapsed() < Duration::from_millis(100));
        started_rx.recv_timeout(Duration::from_secs(2))?;
        process_app_action(&mut app, &mut worker, action.clone())?;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let original = worker.as_ref().expect("worker").pending_interactions[0]
            .request
            .lock()
            .expect("frozen request")
            .clone()
            .expect("prepared request");
        process_app_action(&mut app, &mut worker, AppAction::CancelRun)?;
        assert!(matches!(
            worker_rx.recv_timeout(Duration::from_millis(100))?,
            WorkerCommand::CancelRun
        ));
        release_tx.send(())?;
        wait_interaction_admission(&mut app, &mut worker)?;
        assert_eq!(app.runtime.is_busy, !is_run_admission_action(&action));
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

#[test]
fn prompt_admission_renders_and_accepts_input_and_stop_before_slow_receipt() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let scope = ApplicationScope {
        application_instance: ApplicationInstanceId::new("prompt-first-frame")?,
        authenticated_subject: AuthenticatedSubject::new("local")?,
        workspace: Some(WorkspaceScopeId::new("workspace")?),
        session: Some(SessionScopeId::new("session")?),
    };
    let (release_tx, release_rx) = mpsc::channel();
    let (started_tx, started_rx) = mpsc::channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let port = Arc::new(SlowAdmissionPort {
        settle: false,
        snapshot: Arc::new(Mutex::new(crate::application_bridge::tests::snapshot(
            scope.clone(),
        ))),
        release: Arc::new(Mutex::new(Some(release_rx))),
        started: started_tx,
        calls: Arc::clone(&calls),
        requests: Arc::new(Mutex::new(Vec::new())),
    });
    let application = Arc::new(crate::application_bridge::tests::session(port, scope)?);
    assert!(
        application.current_projection()?.is_none(),
        "exercise initial resume frontier refresh too"
    );
    let mut config = crate::app::tests::common::test_config();
    config.workspace.root = fixture.path().display().to_string();
    let mut app = AppState::from_root_config(&fixture.path().join("sigil.toml"), &config);
    let (worker_tx, commands) = runner::WorkerCommandSender::test_channel();
    let mut worker = Some(WorkerRuntime {
        worker_tx,
        application: Some(application),
        pending_admission: None,
        pending_interactions: Vec::new(),
        worker_rx: mpsc::channel().1,
        join_handle: None,
        ready: true,
    });
    let mut release_admission = AdmissionGateRelease(Some(release_tx));
    app.composer.input = "visible before admission".to_owned();
    let action = app.submit_input()?.expect("prompt action");
    let start = Instant::now();
    process_app_action(&mut app, &mut worker, action.clone())?;
    assert!(start.elapsed() < Duration::from_millis(100));
    started_rx.recv_timeout(Duration::from_secs(2))?;
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30))?;
    terminal.draw(|frame| crate::ui::render(frame, &app))?;
    let rendered: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(rendered.contains("visible before admission"));
    assert!(rendered.contains("Preparing"));
    assert!(!rendered.contains("reasoning with"));
    process_app_action(&mut app, &mut worker, action)?;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "duplicate UI dispatch retains the same owned admission"
    );
    process_app_action(
        &mut app,
        &mut worker,
        AppAction::QueueConversationInput {
            prompt: "follow up after the original prompt".to_owned(),
            kind: sigil_kernel::ConversationInputKind::Chat,
            target: sigil_kernel::ConversationInputTarget::MainThread,
        },
    )?;
    poll_application_admission(&mut app, &mut worker)?;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "follow-up cannot overtake original prompt admission"
    );
    app.handle_key_event(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('x'),
        crossterm::event::KeyModifiers::NONE,
    ))?;
    process_app_action(&mut app, &mut worker, AppAction::CancelRun)?;
    assert!(matches!(
        commands.recv_timeout(Duration::from_millis(100))?,
        WorkerCommand::CancelRun
    ));
    release_admission.release()?;
    wait_interaction_admission(&mut app, &mut worker)?;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "follow-up is released once the original admission has an outcome"
    );
    assert_eq!(
        app.composer.input, "x",
        "late admission failure must preserve newer input"
    );
    assert!(!app.runtime.is_busy);
    app.join_session_auxiliary_until(Instant::now() + Duration::from_secs(1))?;
    shutdown_and_join_worker(&mut worker)?;
    Ok(())
}

#[test]
fn identical_prompt_after_run_finish_gets_a_new_admission_despite_a_late_old_receipt() -> Result<()>
{
    let fixture = tempfile::tempdir()?;
    let scope = ApplicationScope {
        application_instance: ApplicationInstanceId::new("repeated-prompt")?,
        authenticated_subject: AuthenticatedSubject::new("local")?,
        workspace: Some(WorkspaceScopeId::new("workspace")?),
        session: Some(SessionScopeId::new("session")?),
    };
    let (release_tx, release_rx) = mpsc::channel();
    let (started_tx, started_rx) = mpsc::channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let application = Arc::new(crate::application_bridge::tests::session(
        Arc::new(SlowAdmissionPort {
            settle: false,
            snapshot: Arc::new(Mutex::new(crate::application_bridge::tests::snapshot(
                scope.clone(),
            ))),
            release: Arc::new(Mutex::new(Some(release_rx))),
            started: started_tx,
            calls: Arc::clone(&calls),
            requests: Arc::clone(&requests),
        }),
        scope,
    )?);
    let mut config = crate::app::tests::common::test_config();
    config.workspace.root = fixture.path().display().to_string();
    let mut app = AppState::from_root_config(&fixture.path().join("sigil.toml"), &config);
    let (worker_tx, _commands) = runner::WorkerCommandSender::test_channel();
    let mut worker = Some(WorkerRuntime {
        worker_tx,
        application: Some(application),
        pending_admission: None,
        pending_interactions: Vec::new(),
        worker_rx: mpsc::channel().1,
        join_handle: None,
        ready: true,
    });
    app.composer.input = "same prompt".to_owned();
    let action = app.submit_input()?.expect("first prompt");
    process_app_action(&mut app, &mut worker, action)?;
    started_rx.recv_timeout(Duration::from_secs(2))?;
    let started = WorkerMessage::RunStarted {
        prompt: "same prompt".to_owned(),
    };
    apply_worker_message_state(worker.as_mut().expect("worker"), None, &started);
    let run_owner = sigil_kernel::RunCancellationOwner::new();
    worker.as_ref().expect("worker").pending_interactions[0]
        .run_admission
        .as_ref()
        .expect("exact admission")
        .bind_test_owner(&run_owner);
    run_owner.request_cancel();
    app.handle_worker_message(started)?;
    app.handle_worker_message(WorkerMessage::RunFinished {
        result: sigil_kernel::AgentRunResult {
            final_text: "first response".to_owned(),
            tool_calls: 0,
            final_message_id: None,
        },
        entries: Vec::new(),
    })?;
    app.composer.input = "same prompt".to_owned();
    let action = app.submit_input()?.expect("second intent");
    process_app_action(&mut app, &mut worker, action)?;
    started_rx.recv_timeout(Duration::from_secs(2))?;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let logged = requests.lock().expect("requests");
    assert_ne!(logged[0].envelope.command_id, logged[1].envelope.command_id);
    drop(logged);
    release_tx.send(())?;
    let old = &mut worker.as_mut().expect("worker").pending_interactions[0];
    wait_for_owned_thread(&mut old.handle, Instant::now() + Duration::from_secs(2))?;
    let _old_result = old
        .receiver
        .take()
        .expect("old receipt")
        .recv_timeout(Duration::from_secs(1))?;
    // A late terminal receipt belongs to the old request even when the new prompt text matches.
    let (sender, receiver) = mpsc::channel();
    sender.send(Ok(ApplicationCommandReceipt::Rejected(CommandRejection {
        kind: "late-old-rejection".to_owned(),
        reason: "late old receipt".to_owned(),
    })))?;
    old.receiver = Some(receiver);
    poll_application_admission(&mut app, &mut worker)?;
    assert!(app.runtime.is_busy);
    assert_eq!(app.run_phase(), crate::timeline::RunPhase::Preparing);
    assert!(app.composer.input.is_empty());
    app.join_session_auxiliary_until(Instant::now() + Duration::from_secs(1))?;
    shutdown_and_join_worker(&mut worker)?;
    Ok(())
}

#[test]
fn session_dispatch_and_configuration_admission_do_not_block_the_ui_or_apply_early() -> Result<()> {
    for operation in [
        "session-create",
        "session-switch",
        "approval",
        "mcp",
        "diagnostics",
        "config-failure",
        "config-success",
        "config-edited",
        "config-reopened",
    ] {
        let fixture = tempfile::tempdir()?;
        let scope = ApplicationScope {
            application_instance: ApplicationInstanceId::new(format!("slow-{operation}"))?,
            authenticated_subject: AuthenticatedSubject::new("local")?,
            workspace: Some(WorkspaceScopeId::new("workspace")?),
            session: Some(SessionScopeId::new("session")?),
        };
        let mut snapshot = crate::application_bridge::tests::snapshot(scope.clone());
        snapshot.envelope.projection.approval.binding = Some("run:call:approval".to_owned());
        snapshot.envelope.projection.approval.pending = true;
        let (release_tx, release_rx) = mpsc::channel();
        let (started_tx, started_rx) = mpsc::channel();
        let calls = Arc::new(AtomicUsize::new(0));
        let application = Arc::new(crate::application_bridge::tests::session(
            Arc::new(SlowAdmissionPort {
                settle: matches!(
                    operation,
                    "config-success" | "config-edited" | "config-reopened"
                ),
                snapshot: Arc::new(Mutex::new(snapshot)),
                release: Arc::new(Mutex::new(Some(release_rx))),
                started: started_tx,
                calls: Arc::clone(&calls),
                requests: Arc::new(Mutex::new(Vec::new())),
            }),
            scope,
        )?);
        let mut config = crate::app::tests::common::test_config();
        config.workspace.root = fixture.path().display().to_string();
        let config_path = fixture.path().join("sigil.toml");
        let mut app = AppState::from_root_config(&config_path, &config);
        let (worker_tx, commands) = runner::WorkerCommandSender::test_channel();
        let mut worker = Some(WorkerRuntime {
            worker_tx,
            application: Some(application),
            pending_admission: None,
            pending_interactions: Vec::new(),
            worker_rx: mpsc::channel().1,
            join_handle: None,
            ready: true,
        });
        let mut next = config.clone();
        next.agent.model = "unpublished-model".to_owned();
        if matches!(operation, "config-edited" | "config-reopened") {
            app.composer.input = "/config".to_owned();
            app.submit_input()?;
            assert!(app.is_config_mode());
        }
        let save = Arc::new(crate::app::ConfigurationSaveRequest {
            expected: config.clone(),
            next_base: next,
            config_path,
            follow_up: crate::app::ConfigurationSaveFollowUp::ApplyPersistedDefaultModel,
            root_only: true,
            draft_binding: app.config_draft_binding(),
            draft: Mutex::new(None),
            published_root_config: Mutex::new(None),
            close_after_save: true,
        });
        let target = fixture.path().join("other-session.jsonl");
        let action = match operation {
            "session-create" => AppAction::StartNewSession {
                session_log_path: target,
            },
            "session-switch" => AppAction::SwitchSession {
                session_log_path: target,
            },
            "approval" => AppAction::ApprovalDecision {
                call_id: "call".to_owned(),
                approval_request_id: "approval".to_owned(),
                approved: true,
            },
            "mcp" => AppAction::RefreshMcpServer {
                server_name: "fixture-server".to_owned(),
            },
            "diagnostics" => AppAction::CheckChangedFilesDiagnostics,
            _ => AppAction::PersistConfiguration {
                request: Arc::clone(&save),
            },
        };
        let start = Instant::now();
        process_app_action(&mut app, &mut worker, action)?;
        assert!(start.elapsed() < Duration::from_millis(100), "{operation}");
        started_rx.recv_timeout(Duration::from_secs(2))?;
        assert_eq!(
            app.persisted_config_snapshot()
                .expect("configuration")
                .agent
                .model,
            config.agent.model,
            "configuration cannot apply before publication receipt"
        );
        if operation == "config-edited" {
            app.select_config_section_for_test(crate::config_panel::ConfigSection::Appearance);
            app.handle_key_event(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Enter,
                crossterm::event::KeyModifiers::NONE,
            ))?;
            assert!(app.config_is_dirty());
        } else if operation == "config-reopened" {
            app.handle_key_event(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Esc,
                crossterm::event::KeyModifiers::NONE,
            ))?;
            assert!(!app.is_config_mode());
            app.composer.input = "/config".to_owned();
            app.submit_input()?;
            assert!(app.is_config_mode());
        } else {
            app.handle_key_event(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('x'),
                crossterm::event::KeyModifiers::NONE,
            ))?;
        }
        let current_draft = app.config_draft_binding();
        let current_appearance = app.config_preview_appearance();
        process_app_action(&mut app, &mut worker, AppAction::CancelRun)?;
        assert!(matches!(
            commands.recv_timeout(Duration::from_millis(100))?,
            WorkerCommand::CancelRun
        ));
        let mut published = config.clone();
        published.agent.model = "published-model".to_owned();
        *save
            .published_root_config
            .lock()
            .expect("publication result") = Some(published);
        release_tx.send(())?;
        wait_interaction_admission(&mut app, &mut worker)?;
        assert_eq!(
            app.persisted_config_snapshot()
                .expect("configuration")
                .agent
                .model,
            if matches!(
                operation,
                "config-success" | "config-edited" | "config-reopened"
            ) {
                "published-model"
            } else {
                &config.agent.model
            }
        );
        if matches!(operation, "config-edited" | "config-reopened") {
            assert!(
                app.is_config_mode(),
                "old save must not close a changed/reopened panel"
            );
            assert_eq!(app.config_draft_binding(), current_draft);
            assert_eq!(app.config_preview_appearance(), current_appearance);
            assert_eq!(app.config_is_dirty(), operation == "config-edited");
        }
        assert!(
            worker
                .as_ref()
                .expect("worker")
                .pending_interactions
                .is_empty(),
            "one-shot dispatch owner is joined after its receipt"
        );
        app.join_session_auxiliary_until(Instant::now() + Duration::from_secs(1))?;
        shutdown_and_join_worker(&mut worker)?;
    }
    Ok(())
}

#[test]
fn admission_receipt_keeps_idle_wakes_until_the_owner_thread_is_joined() -> Result<()> {
    let scope = ApplicationScope {
        application_instance: ApplicationInstanceId::new("receipt-before-thread-exit")?,
        authenticated_subject: AuthenticatedSubject::new("local")?,
        workspace: Some(WorkspaceScopeId::new("workspace")?),
        session: Some(SessionScopeId::new("session")?),
    };
    let application = Arc::new(crate::application_bridge::tests::session(
        Arc::new(SlowAdmissionPort {
            settle: false,
            snapshot: Arc::new(Mutex::new(crate::application_bridge::tests::snapshot(
                scope.clone(),
            ))),
            release: Arc::new(Mutex::new(None)),
            started: mpsc::channel().0,
            calls: Arc::new(AtomicUsize::new(0)),
            requests: Arc::new(Mutex::new(Vec::new())),
        }),
        scope,
    )?);
    let (release, wait) = mpsc::channel();
    let (published, received) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        published.send(()).expect("receipt published");
        wait.recv().expect("finish owner cleanup");
    });
    received.recv_timeout(Duration::from_secs(1))?;
    let mut pending = PendingApplicationAdmission {
        application,
        request: Arc::new(Mutex::new(None)),
        action: AppAction::CheckChangedFilesDiagnostics,
        run_admission: None,
        run_submission_intent: None,
        queue_target: None,
        attachment_recovery_binding: None,
        retain_for_recovery: false,
        settled: true,
        refresh_before_prepare: false,
        receiver: None,
        handle: Some(handle),
        retryable: false,
        receipt_resolved: true,
        reconcile_requested: false,
        run_owner_returned: false,
    };
    assert!(
        pending.needs_polling(),
        "consumed receipt must not disable the cleanup wake"
    );
    assert!(!pending.receipt_resolved_and_finished());
    release.send(())?;
    let deadline = Instant::now() + Duration::from_secs(2);
    while !pending.receipt_resolved_and_finished() {
        anyhow::ensure!(Instant::now() < deadline, "owner did not exit");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        pending.needs_polling(),
        "a finished owner still needs one poll to apply completion and join"
    );
    wait_for_owned_thread(&mut pending.handle, deadline)?;
    assert!(!pending.needs_polling());
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
            runtime
                .pending_interactions
                .iter()
                .all(|pending| pending.receiver.is_none() && pending.handle.is_none())
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
    let mut owned = Some(handle);
    assert!(wait_for_owned_thread(&mut owned, started - Duration::from_millis(1)).is_err());
    assert!(started.elapsed() < Duration::from_millis(50));
    assert!(
        owned.is_some(),
        "deadline does not transfer or drop ownership"
    );
    release_tx.send(())?;
    wait_for_worker_thread(owned, Instant::now() + Duration::from_secs(1))?;
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
    let mut restore = || -> std::io::Result<()> {
        restored = true;
        Ok(())
    };
    restore()?;
    let mut shutdown_handle = shutdown::start_event_runtime_shutdown(&mut runtime)?;
    assert!(restored);
    assert!(runtime.is_none());
    assert!(started.elapsed() < Duration::from_millis(100));
    assert!(matches!(
        finished_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    release_tx.send(())?;
    finished_rx.recv_timeout(Duration::from_secs(2))?;
    wait_for_worker_thread(
        shutdown_handle.take(),
        Instant::now() + Duration::from_secs(1),
    )?;
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
                std::thread::sleep(Duration::from_millis(1));
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
        settle: false,
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
        settle: false,
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
        run_admission: None,
        run_submission_intent: None,
        queue_target: None,
        attachment_recovery_binding: None,
        retain_for_recovery: true,
        settled: false,
        refresh_before_prepare: false,
        application: Arc::clone(&application),
        request: Arc::new(std::sync::Mutex::new(Some(request))),
        action,
        receiver: None,
        handle: None,
        retryable: true,
        receipt_resolved: false,
        reconcile_requested: false,
        run_owner_returned: false,
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
    for attempt in 0..2 {
        let error =
            shutdown_and_join_worker_until(&mut worker, Instant::now() + Duration::from_secs(1))
                .expect_err("join panic remains cleanup-incomplete");
        assert!(error.to_string().contains("stage=owned-thread-join"));
        assert!(error.to_string().contains("cleanup_complete=false"));
        if attempt == 0 {
            assert!(format!("{error:#}").contains("worker shutdown fixture panic"));
        }
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

#[test]
fn shutdown_preserves_the_primary_failure_and_all_cleanup_failures() -> Result<()> {
    let primary =
        "background thread `sigil-tui-admission` panicked at projection.rs:700:18: no reactor";
    let error = finish_tui_shutdown(
        Err(anyhow::anyhow!(primary)),
        [
            Ok(()),
            Err(anyhow::anyhow!(
                "owned_thread=admission; cleanup_complete=false"
            )),
            Err(anyhow::anyhow!(
                "projection drain failed; cleanup_complete=false"
            )),
        ],
    )
    .expect_err("shutdown must not hide the original panic");
    let diagnostic = format!("{error:#}");
    assert!(diagnostic.starts_with(primary));
    assert!(diagnostic.contains("owned_thread=admission; cleanup_complete=false"));
    assert!(diagnostic.contains("projection drain failed; cleanup_complete=false"));

    let error = finish_tui_shutdown(Ok(()), [Err(anyhow::anyhow!("worker cleanup failed"))])
        .expect_err("clean loop does not erase a failed cleanup");
    assert_eq!(error.to_string(), "worker cleanup failed");
    finish_tui_shutdown(Ok(()), [Ok(()), Ok(())])?;
    Ok(())
}

fn assert_new_submission_survives_an_unobserved_old_admission(
    stop_old_submission: bool,
    next_prompt: &str,
    rejected_receipt: bool,
) -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let scope = ApplicationScope {
        application_instance: ApplicationInstanceId::new("unobserved-old-admission")?,
        authenticated_subject: AuthenticatedSubject::new("local")?,
        workspace: Some(WorkspaceScopeId::new("workspace")?),
        session: Some(SessionScopeId::new("session")?),
    };
    let (release_tx, release_rx) = mpsc::channel();
    let (started_tx, started_rx) = mpsc::channel();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let application = Arc::new(crate::application_bridge::tests::session(
        Arc::new(SlowAdmissionPort {
            settle: false,
            snapshot: Arc::new(Mutex::new(crate::application_bridge::tests::snapshot(
                scope.clone(),
            ))),
            release: Arc::new(Mutex::new(Some(release_rx))),
            started: started_tx,
            calls: Arc::new(AtomicUsize::new(0)),
            requests: Arc::clone(&requests),
        }),
        scope,
    )?);
    let mut config = crate::app::tests::common::test_config();
    config.workspace.root = fixture.path().display().to_string();
    let mut app = AppState::from_root_config(&fixture.path().join("sigil.toml"), &config);
    let (worker_tx, commands) = runner::WorkerCommandSender::test_channel();
    let mut worker = Some(WorkerRuntime {
        worker_tx,
        application: Some(application),
        pending_admission: None,
        pending_interactions: Vec::new(),
        worker_rx: mpsc::channel().1,
        join_handle: None,
        ready: true,
    });
    app.composer.input = "same prompt".to_owned();
    let action = app.submit_input()?.expect("first submission");
    process_app_action(&mut app, &mut worker, action)?;
    started_rx.recv_timeout(Duration::from_secs(2))?;
    if stop_old_submission {
        process_app_action(&mut app, &mut worker, AppAction::CancelRun)?;
        assert!(matches!(
            commands.recv_timeout(Duration::from_secs(1))?,
            WorkerCommand::CancelRun
        ));
    }
    // The actual worker can publish failure before its application receipt returns, either
    // because Stop found no bound owner yet or because preparation failed before owner binding.
    let failure = WorkerMessage::RunFailed(if stop_old_submission {
        "no active run to cancel".to_owned()
    } else {
        "preparation failed before run owner binding".to_owned()
    });
    apply_worker_message_state(worker.as_mut().expect("worker"), None, &failure);
    app.handle_worker_message(failure)?;
    assert!(!app.runtime.is_busy);
    assert!(!worker.as_ref().expect("worker").pending_interactions[0].run_observed());

    app.composer.input = next_prompt.to_owned();
    let action = app.submit_input()?.expect("new user submission");
    process_app_action(&mut app, &mut worker, action)?;
    started_rx.recv_timeout(Duration::from_secs(2))?;
    let recorded = requests.lock().expect("request log");
    assert_eq!(recorded.len(), 2, "same text still represents a new intent");
    assert_ne!(
        recorded[0].envelope.command_id,
        recorded[1].envelope.command_id
    );
    drop(recorded);

    release_tx.send(())?;
    let old = &mut worker.as_mut().expect("worker").pending_interactions[0];
    wait_for_owned_thread(&mut old.handle, Instant::now() + Duration::from_secs(2))?;
    if rejected_receipt {
        let _actual_error = old
            .receiver
            .take()
            .expect("old receipt")
            .recv_timeout(Duration::from_secs(1))?;
        let (sender, receiver) = mpsc::channel();
        sender.send(Ok(ApplicationCommandReceipt::Rejected(CommandRejection {
            kind: "old-preparation-rejected".to_owned(),
            reason: "old preparation did not start a run".to_owned(),
        })))?;
        old.receiver = Some(receiver);
    }
    poll_pending_application_admission(&mut app, old)?;
    assert!(
        app.runtime.is_busy,
        "old admission cannot clear the new run"
    );
    assert_eq!(app.run_phase(), crate::timeline::RunPhase::Preparing);
    assert!(
        app.composer.input.is_empty(),
        "old admission cannot restore its prompt over a newer submission"
    );
    app.join_session_auxiliary_until(Instant::now() + Duration::from_secs(1))?;
    shutdown_and_join_worker(&mut worker)?;
    Ok(())
}

#[test]
fn stop_before_owner_binding_preserves_new_submission_identity_and_state() -> Result<()> {
    for next_prompt in ["same prompt", "different prompt"] {
        for rejected_receipt in [false, true] {
            assert_new_submission_survives_an_unobserved_old_admission(
                true,
                next_prompt,
                rejected_receipt,
            )?;
        }
    }
    Ok(())
}

#[test]
fn prebind_failure_without_stop_preserves_new_submission_identity_and_state() -> Result<()> {
    for next_prompt in ["same prompt", "different prompt"] {
        for rejected_receipt in [false, true] {
            assert_new_submission_survives_an_unobserved_old_admission(
                false,
                next_prompt,
                rejected_receipt,
            )?;
        }
    }
    Ok(())
}

#[test]
fn configuration_completion_precedes_session_switches_and_preserves_the_next_target() -> Result<()>
{
    for uncertain in [false, true] {
        configuration_completion_and_session_switch_case(uncertain)?;
    }
    Ok(())
}

fn configuration_completion_and_session_switch_case(uncertain: bool) -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let make_scope = |session: &str| -> Result<ApplicationScope> {
        Ok(ApplicationScope {
            application_instance: ApplicationInstanceId::new(format!("lifecycle-{session}"))?,
            authenticated_subject: AuthenticatedSubject::new("local")?,
            workspace: Some(WorkspaceScopeId::new("workspace")?),
            session: Some(SessionScopeId::new(session)?),
        })
    };
    let (release_tx, release_rx) = mpsc::channel();
    let release = Arc::new(Mutex::new(Some(release_rx)));
    let (started_tx, started_rx) = mpsc::channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let scope = make_scope("original")?;
    let application = Arc::new(crate::application_bridge::tests::session(
        Arc::new(SlowAdmissionPort {
            settle: true,
            snapshot: Arc::new(Mutex::new(crate::application_bridge::tests::snapshot(
                scope.clone(),
            ))),
            release: Arc::clone(&release),
            started: started_tx,
            calls: Arc::clone(&calls),
            requests: Arc::new(Mutex::new(Vec::new())),
        }),
        scope,
    )?);
    let make_worker = |application| {
        let (worker_tx, commands) = runner::WorkerCommandSender::test_channel();
        (
            WorkerRuntime {
                worker_tx,
                application: Some(application),
                pending_admission: None,
                pending_interactions: Vec::new(),
                worker_rx: mpsc::channel().1,
                join_handle: None,
                ready: true,
            },
            commands,
        )
    };
    let mut config = crate::app::tests::common::test_config();
    config.workspace.root = fixture.path().display().to_string();
    let config_path = fixture.path().join("sigil.toml");
    let mut app = AppState::from_root_config(&config_path, &config);
    let (runtime, _original_commands) = make_worker(application);
    let mut worker = Some(runtime);
    // An old command's completed admission thread must not become a permanent lifecycle gate.
    app.composer.input = "old admission".to_owned();
    let old_action = app.submit_input()?.expect("old prompt");
    process_app_action(&mut app, &mut worker, old_action)?;
    started_rx.recv_timeout(Duration::from_secs(2))?;
    release_tx.send(())?;
    let old = &mut worker.as_mut().expect("worker").pending_interactions[0];
    wait_for_owned_thread(&mut old.handle, Instant::now() + Duration::from_secs(2))?;
    old.receiver
        .take()
        .expect("old result")
        .recv_timeout(Duration::from_secs(1))??;
    let old_request = old
        .request
        .lock()
        .expect("old request")
        .clone()
        .expect("frozen request");
    let (old_result, old_receiver) = mpsc::channel();
    let delayed = if uncertain {
        Ok(ApplicationCommandReceipt::Uncertain(
            UncertainCommandReceipt {
                command_id: old_request.envelope.command_id.clone(),
                command_kind: old_request.envelope.command.kind().to_owned(),
                reservation_fingerprint: command_fingerprint(&old_request)?,
                recovery: CommandRecoveryBinding {
                    key: old_request
                        .admission
                        .reservation_key(&old_request.envelope.command_id),
                    phase: CommandLifecyclePhase::EffectStarted,
                },
                owner_recovery_binding: Some("fixture-owner".to_owned()),
            },
        ))
    } else {
        Err(ApplicationError::Unavailable)
    };
    old_result.send(delayed)?;
    old.receiver = Some(old_receiver);
    poll_application_admission(&mut app, &mut worker)?;
    assert!(
        worker.as_ref().expect("worker").pending_interactions[0]
            .handle
            .is_none()
    );
    app.handle_worker_message(WorkerMessage::RunFailed(
        "old admission remains recoverable".to_owned(),
    ))?;
    let calls_before_config = calls.load(Ordering::SeqCst);
    let (release_tx, release_rx) = mpsc::channel();
    *release.lock().expect("config gate") = Some(release_rx);
    let mut published = config.clone();
    published.agent.model = "published-before-switch".to_owned();
    let save = Arc::new(crate::app::ConfigurationSaveRequest {
        expected: config,
        next_base: published.clone(),
        config_path,
        follow_up: crate::app::ConfigurationSaveFollowUp::ApplyPersistedDefaultModel,
        root_only: true,
        draft_binding: None,
        draft: Mutex::new(None),
        published_root_config: Mutex::new(Some(published)),
        close_after_save: false,
    });
    process_app_action(
        &mut app,
        &mut worker,
        AppAction::PersistConfiguration { request: save },
    )?;
    started_rx.recv_timeout(Duration::from_secs(2))?;
    let path_a = fixture.path().join("session-a.jsonl");
    let path_b = fixture.path().join("session-b.jsonl");
    for path in [&path_a, &path_b] {
        process_app_action(
            &mut app,
            &mut worker,
            AppAction::SwitchSession {
                session_log_path: path.clone(),
            },
        )?;
    }
    assert_eq!(calls.load(Ordering::SeqCst), calls_before_config + 1);
    assert_eq!(app.deferred_application_actions.len(), 2);
    assert!(!flush_deferred_application_action(&mut app, &mut worker)?);
    release_tx.send(())?;
    wait_interaction_admission(&mut app, &mut worker)?;
    assert_eq!(
        app.persisted_config_snapshot()
            .expect("published configuration")
            .agent
            .model,
        "published-before-switch"
    );

    let (release_switch, wait_switch) = mpsc::channel();
    *release.lock().expect("switch gate") = Some(wait_switch);
    assert!(flush_deferred_application_action(&mut app, &mut worker)?);
    started_rx.recv_timeout(Duration::from_secs(2))?;
    app.handle_worker_message(WorkerMessage::SessionSwitched {
        session_id: "session-a".to_owned(),
        session_log_path: path_a.clone(),
        provider_name: "deepseek".to_owned(),
        model_name: "published-before-switch".to_owned(),
        entries: Vec::new(),
    })?;
    let (event_sender, mut event_receiver) =
        tokio::sync::mpsc::unbounded_channel::<WorkerMessage>();
    drop(event_sender);
    let mut closed_worker_event = Box::pin(wait_for_worker_event(
        app.runtime.worker_rebind_required,
        event_receiver.recv(),
    ));
    let mut context = std::task::Context::from_waker(futures::task::noop_waker_ref());
    assert!(
        std::future::Future::poll(closed_worker_event.as_mut(), &mut context).is_pending(),
        "normal session worker closure must not discard the pending completion or spin the event loop"
    );
    drop(closed_worker_event);
    assert!(!restart_worker_after_session_transition(
        &mut app,
        &mut worker,
        |_, _| { anyhow::bail!("must consume the old session receipt before replacing its owner") }
    )?);
    assert_eq!(app.deferred_application_actions.len(), 1);
    release_switch.send(())?;
    wait_interaction_admission(&mut app, &mut worker)?;

    let new_scope = make_scope("session-a")?;
    let new_requests = Arc::new(Mutex::new(Vec::new()));
    let (new_started, new_started_rx) = mpsc::channel();
    let replacement = Arc::new(crate::application_bridge::tests::session(
        Arc::new(SlowAdmissionPort {
            settle: true,
            snapshot: Arc::new(Mutex::new(crate::application_bridge::tests::snapshot(
                new_scope.clone(),
            ))),
            release: Arc::new(Mutex::new(None)),
            started: new_started,
            calls: Arc::new(AtomicUsize::new(0)),
            requests: Arc::clone(&new_requests),
        }),
        new_scope.clone(),
    )?);
    let mut replacement_commands = None;
    assert!(restart_worker_after_session_transition(
        &mut app,
        &mut worker,
        |_, _| {
            let (runtime, commands) = make_worker(Arc::clone(&replacement));
            replacement_commands = Some(commands);
            Ok(runtime)
        }
    )?);
    assert_eq!(app.retained_application_admissions.len(), 1);
    assert_eq!(
        app.retained_application_admissions[0]
            .request
            .lock()
            .expect("retained request")
            .as_ref()
            .expect("frozen original"),
        &old_request
    );
    assert!(!app.retained_application_admissions[0].reconcile_requested);
    assert!(app.retained_application_admissions[0].handle.is_none());
    let old_dispatch_count = calls.load(Ordering::SeqCst);
    assert!(flush_deferred_application_action(&mut app, &mut worker)?);
    new_started_rx.recv_timeout(Duration::from_secs(2))?;
    wait_interaction_admission(&mut app, &mut worker)?;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        old_dispatch_count,
        "retired request must not automatically execute against a replacement scope"
    );
    let requests = new_requests.lock().expect("new scope request log");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].admission.scope, new_scope);
    assert!(matches!(
        requests[0].envelope.command,
        ApplicationCommand::Session(SessionCommand::Switch { .. })
    ));
    drop(requests);
    assert!(app.deferred_application_actions.is_empty());
    app.handle_worker_message(WorkerMessage::SessionSwitched {
        session_id: "session-b".to_owned(),
        session_log_path: path_b.clone(),
        provider_name: "deepseek".to_owned(),
        model_name: "published-before-switch".to_owned(),
        entries: Vec::new(),
    })?;
    assert_eq!(app.session_log_path, path_b);
    app.join_session_auxiliary_until(Instant::now() + Duration::from_secs(1))?;
    shutdown_and_join_worker(&mut worker)?;
    drop(replacement_commands);
    Ok(())
}

#[test]
fn returned_run_owner_requires_exact_binding_and_joined_admission_before_retaining() -> Result<()> {
    let scope = ApplicationScope {
        application_instance: ApplicationInstanceId::new("returned-owner-ui")?,
        authenticated_subject: AuthenticatedSubject::new("local")?,
        workspace: Some(WorkspaceScopeId::new("workspace")?),
        session: Some(SessionScopeId::new("session")?),
    };
    let (release, released) = mpsc::channel();
    let mut release_guard = AdmissionGateRelease(Some(release));
    let (started, started_rx) = mpsc::channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let application = Arc::new(crate::application_bridge::tests::session(
        Arc::new(SlowAdmissionPort {
            settle: false,
            snapshot: Arc::new(Mutex::new(crate::application_bridge::tests::snapshot(
                scope.clone(),
            ))),
            release: Arc::new(Mutex::new(Some(released))),
            started,
            calls: Arc::clone(&calls),
            requests: Arc::new(Mutex::new(Vec::new())),
        }),
        scope,
    )?);
    futures::executor::block_on(application.refresh())?;
    let action = AppAction::InvokeAgentProfile {
        profile_id: "plan".to_owned(),
        prompt: "objective".to_owned(),
        parent_prompt: "objective".to_owned(),
    };
    let request = application
        .prepare_action(&action, None, None)?
        .context("profile request")?;
    let binding =
        sigil_runtime::application_operation_owner::application_operation_binding(&request)?
            .context("profile binding")?;
    let mut pending = PendingApplicationAdmission {
        application: Arc::clone(&application),
        request: Arc::new(Mutex::new(Some(request.clone()))),
        action,
        run_admission: None,
        run_submission_intent: None,
        queue_target: None,
        attachment_recovery_binding: None,
        retain_for_recovery: true,
        settled: false,
        refresh_before_prepare: false,
        receiver: None,
        handle: None,
        retryable: true,
        receipt_resolved: false,
        reconcile_requested: false,
        run_owner_returned: false,
    };
    pending.start()?;
    started_rx.recv_timeout(Duration::from_secs(2))?;
    let (sender, _commands) = runner::WorkerCommandSender::test_channel();
    let mut worker = Some(WorkerRuntime {
        worker_tx: sender,
        application: Some(application),
        pending_admission: None,
        pending_interactions: vec![pending],
        worker_rx: mpsc::channel().1,
        join_handle: None,
        ready: true,
    });
    let mut app = AppState::from_root_config(
        Path::new("sigil.toml"),
        &crate::app::tests::common::test_config(),
    );
    for field in 0..3 {
        let mut foreign = binding.clone();
        match field {
            0 => foreign.session_scope_id = "other-session".to_owned(),
            1 => foreign.reservation_key_digest = "a".repeat(64),
            _ => foreign.fingerprint = "b".repeat(64),
        }
        apply_worker_message_state(
            worker.as_mut().context("worker")?,
            None,
            &WorkerMessage::ApplicationRunOwnerReturned {
                binding: Box::new(foreign),
            },
        );
        assert!(!worker.as_ref().context("worker")?.pending_interactions[0].run_owner_returned);
    }
    apply_worker_message_state(
        worker.as_mut().context("worker")?,
        None,
        &WorkerMessage::RunFailed("UI failure is not owner proof".to_owned()),
    );
    assert!(!worker.as_ref().context("worker")?.pending_interactions[0].run_owner_returned);
    apply_worker_message_state(
        worker.as_mut().context("worker")?,
        None,
        &WorkerMessage::ApplicationRunOwnerReturned {
            binding: Box::new(binding),
        },
    );
    poll_application_admission(&mut app, &mut worker)?;
    assert!(
        app.retained_application_admissions.is_empty(),
        "admission handle still owns its result"
    );
    assert_eq!(
        worker
            .as_ref()
            .context("worker")?
            .pending_interactions
            .len(),
        1
    );
    release_guard.release()?;
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.retained_application_admissions.is_empty() && Instant::now() < deadline {
        poll_application_admission(&mut app, &mut worker)?;
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(app.retained_application_admissions.len(), 1);
    let retained = &app.retained_application_admissions[0];
    assert_eq!(
        retained.request.lock().expect("request").as_ref(),
        Some(&request)
    );
    assert!(!retained.receipt_resolved && !retained.reconcile_requested && !retained.retryable);
    assert!(retained.handle.is_none() && retained.receiver.is_none());
    assert!(
        worker
            .as_ref()
            .context("worker")?
            .pending_interactions
            .is_empty()
    );
    for _ in 0..4 {
        poll_application_admission(&mut app, &mut worker)?;
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "retained uncertainty is not redispatched"
    );
    shutdown_and_join_worker(&mut worker)?;
    Ok(())
}
