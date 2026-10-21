use super::*;
use std::io::{Read, Write};

struct ProviderFixture {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<Result<()>>>,
}

impl ProviderFixture {
    fn finish(mut self) -> Result<()> {
        self.handle
            .take()
            .context("provider fixture owner")?
            .join()
            .map_err(|_| anyhow::anyhow!("provider fixture panicked"))?
    }
}

impl Drop for ProviderFixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn pump_until(
    app: &mut AppState,
    worker: &mut Option<WorkerRuntime>,
    mut done: impl FnMut(&AppState, &Option<WorkerRuntime>) -> bool,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !done(app, worker) {
        drain_worker_messages_inner(app, worker, None)?;
        poll_application_admission(app, worker)?;
        anyhow::ensure!(
            Instant::now() < deadline,
            "configured lifecycle did not settle: {:?}",
            app.last_notice()
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    Ok(())
}

fn complete_maintenance(app: &mut AppState, worker: &mut Option<WorkerRuntime>) -> Result<()> {
    let config = app
        .session_runtime_config_snapshot()
        .cloned()
        .context("runtime config")?;
    maintain(app, worker, Some(config))?;
    let deadline = Instant::now() + Duration::from_secs(15);
    while app
        .runtime_maintenance
        .as_ref()
        .is_some_and(|owner| owner.is_running())
    {
        anyhow::ensure!(Instant::now() < deadline, "session rebind did not finish");
        std::thread::sleep(Duration::from_millis(2));
    }
    poll_maintenance(app, worker)?;
    anyhow::ensure!(worker.is_some(), "rebind failed: {:?}", app.last_notice());
    pump_until(app, worker, |_, worker| {
        worker.as_ref().is_some_and(|worker| worker.ready)
    })
}

fn apply_real_config_save(app: &mut AppState, worker: &mut Option<WorkerRuntime>) -> Result<()> {
    let expected = app
        .persisted_config_snapshot()
        .cloned()
        .context("saved config")?;
    let mut next = expected.clone();
    next.appearance.info_rail = !next.appearance.info_rail;
    next.permission.mode = sigil_kernel::PermissionMode::Manual;
    // Unit application ports deliberately suppress filesystem publication. Use the real CAS
    // publisher here, then the same typed publication and transition consumed by ConfigSaved.
    next.save_if_unchanged(&app.config_path, &expected)?;
    assert_eq!(
        sigil_kernel::RootConfig::load(&app.config_path)?
            .appearance
            .info_rail,
        next.appearance.info_rail
    );
    let publication = Arc::new(crate::app::ConfigurationSaveRequest {
        expected,
        next_base: next.clone(),
        config_path: app.config_path.clone(),
        follow_up: crate::app::ConfigurationSaveFollowUp::RebootRuntime,
        root_only: true,
        draft_binding: None,
        draft: Mutex::new(None),
        published_root_config: Mutex::new(Some(next)),
        close_after_save: false,
    });
    assert!(matches!(
        apply_configuration_publication(app, &publication)?,
        Some(AppAction::ConfigSaved { .. })
    ));
    start_after_config_save(app, worker)?;
    wait_for_real_transition(app)?;
    poll(app, worker)?;
    assert!(
        app.runtime_transition
            .as_ref()
            .is_some_and(|owner| owner.activation_acknowledged)
    );
    assert_eq!(app.runtime.permission_mode, "manual");
    Ok(())
}

fn execute_session_action(
    app: &mut AppState,
    worker: &mut Option<WorkerRuntime>,
    action: AppAction,
) -> Result<()> {
    let application = worker
        .as_ref()
        .and_then(|worker| worker.application.clone())
        .context("live application")?;
    let reactor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    reactor.block_on(application.refresh())?;
    let request = application
        .prepare_action(&action, None, None)?
        .context("session command")?;
    let command_id = request.envelope.command_id.clone();
    let command_kind = request.envelope.command.kind().to_owned();
    let fingerprint = sigil_application::command_fingerprint(&request)?;
    let key = request.admission.reservation_key(&command_id);
    let receipt = reactor.block_on(application.execute_prepared(request))?;
    let sigil_application::ApplicationCommandReceipt::Uncertain(receipt) = receipt else {
        anyhow::bail!("session transition has no settlement contract: {receipt:?}");
    };
    receipt.validate()?;
    assert_eq!(receipt.command_id, command_id);
    assert_eq!(receipt.command_kind, command_kind);
    assert_eq!(receipt.reservation_fingerprint, fingerprint);
    assert_eq!(receipt.recovery.key, key);
    assert_eq!(
        receipt.recovery.phase,
        sigil_application::CommandLifecyclePhase::EffectStarted
    );
    assert!(receipt.owner_recovery_binding.is_some());
    let (expected_path, expect_new) = match action {
        AppAction::StartNewSession { session_log_path } => (session_log_path, true),
        AppAction::SwitchSession { session_log_path } => (session_log_path, false),
        _ => anyhow::bail!("fixture requires a typed session transition"),
    };
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut messages_seen = 0;
    eprintln!(
        "session transition begin new={expect_new} source={} requested={}",
        app.session_id,
        expected_path.display()
    );
    let (transitioned_id, transitioned_path) = loop {
        let runtime = worker.as_mut().context("transition worker")?;
        if let Some(message) = try_recv_worker_message(runtime) {
            messages_seen += 1;
            eprintln!(
                "session transition new={expect_new} message={:?}",
                std::mem::discriminant(&message)
            );
            let failure = match &message {
                WorkerMessage::RunFailed(error) => Some(format!("worker failure: {error}")),
                WorkerMessage::SessionRouteRecoveryRequired {
                    code,
                    target_session,
                    ..
                } => Some(format!(
                    "typed recovery {code:?}, target={:?}",
                    target_session.as_ref().map(|target| &target.session_id)
                )),
                _ => None,
            };
            let transitioned_id = match &message {
                WorkerMessage::NewSessionStarted {
                    session_id,
                    session_log_path,
                    ..
                } if expect_new => Some((session_id.clone(), session_log_path.clone())),
                WorkerMessage::SessionSwitched {
                    session_id,
                    session_log_path,
                    ..
                } if !expect_new => Some((session_id.clone(), session_log_path.clone())),
                _ => None,
            };
            apply_worker_message_state(runtime, None, &message);
            app.handle_worker_message(message)?;
            if let Some(failure) = failure {
                anyhow::bail!(
                    "session transition new={expect_new} requested={}: {failure}; notice={:?}",
                    expected_path.display(),
                    app.last_notice()
                );
            }
            if let Some((id, path)) = transitioned_id {
                assert_eq!(
                    std::fs::canonicalize(&path)?,
                    std::fs::canonicalize(&expected_path)?,
                    "typed transition must identify the exact requested durable file"
                );
                break (id, path);
            }
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "matching typed session transition missing: new={expect_new}, requested={}, messages={messages_seen}, notice={:?}, worker_finished={}",
            expected_path.display(),
            app.last_notice(),
            runtime
                .join_handle
                .as_ref()
                .is_none_or(std::thread::JoinHandle::is_finished)
        );
        std::thread::sleep(Duration::from_millis(2));
    };
    assert_eq!(app.session_id, transitioned_id);
    assert_eq!(app.session_log_path, transitioned_path);
    let store = sigil_kernel::JsonlSessionStore::new(&expected_path)?;
    assert_eq!(
        store.read_handle().read_event_records()?[0]
            .stored_event()
            .session_id,
        transitioned_id
    );
    assert_eq!(
        app.worker_session_attachment()
            .context("transferred attachment")?
            .session_path(),
        store.path()
    );
    assert!(app.take_worker_rebind_required());
    complete_maintenance(app, worker)
}

#[test]
fn config_saved_new_session_reuses_journals_and_can_return_to_retained_scope() -> Result<()> {
    let reactor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let _entered = reactor.enter();
    let (fixture, mut app, mut worker) = real_runtime_transition_fixture()?;
    let first_path = app.session_log_path.clone();
    let first_id = app.session_id.clone();
    let original = worker
        .as_ref()
        .and_then(|worker| worker.application.clone())
        .context("original application")?;
    app.composer.input = "保留三行草稿\nsecond line\nthird line".to_owned();
    apply_real_config_save(&mut app, &mut worker)?;
    apply_real_config_save(&mut app, &mut worker)?;
    assert!(matches!(sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease::acquire(&first_path),
        Err(sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentError::Busy { .. })),
        "the configured active session keeps its exclusive attachment");
    let new_path = app.session_log_dir.join("session-after-config.jsonl");
    execute_session_action(
        &mut app,
        &mut worker,
        AppAction::StartNewSession {
            session_log_path: new_path,
        },
    )?;
    assert_ne!(app.session_id, first_id);
    assert!(
        app.runtime_transition.is_none(),
        "completed old-scope route owner must retire after its worker joins"
    );
    drop(
        sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease::acquire(
            &first_path,
        )?,
    );
    assert!(app.worker_ready());
    assert_eq!(app.runtime.permission_mode, "manual");
    assert_eq!(app.composer.input, "保留三行草稿\nsecond line\nthird line");
    let current = worker
        .as_ref()
        .and_then(|worker| worker.application.as_ref())
        .context("new application")?;
    assert!(Arc::ptr_eq(
        &original.journals().context("original journals")?,
        &current.journals().context("new journals")?
    ));
    // Keep the old service alive deliberately: its recovery bindings and ACK lease must not
    // block an explicit return, and its durable frontier must still be the original scope.
    execute_session_action(
        &mut app,
        &mut worker,
        AppAction::SwitchSession {
            session_log_path: first_path,
        },
    )?;
    assert_eq!(app.session_id, first_id);
    let reactor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let old = reactor.block_on(original.refresh())?;
    let current = reactor.block_on(
        worker
            .as_ref()
            .and_then(|worker| worker.application.as_ref())
            .context("returned application")?
            .refresh(),
    )?;
    assert_eq!(old.scope, current.scope);
    let mut foreign = TuiApplicationBindingInputs::from_app(&app)?;
    foreign.workspace_root = fixture.path().join("other-workspace");
    std::fs::create_dir(&foreign.workspace_root)?;
    let runtime = worker.as_ref().context("returned worker")?;
    let store = sigil_kernel::JsonlSessionStore::new(&app.session_log_path)?;
    assert!(
        application_bridge::build_from_inputs(
            foreign,
            runtime.worker_tx.clone(),
            app.runtime.reasoning_effort.clone(),
            sigil_runtime::RuntimeSessionProjectionOwner::from_store(&store),
            original.journals(),
        )
        .is_err(),
        "shared journals must not cross workspace authority"
    );
    shutdown::shutdown_tui_owners(
        &mut app,
        &mut worker,
        &mut TuiShutdownState::default(),
        &mut None,
        false,
        |_| {},
    )?;
    Ok(())
}

fn read_request(socket: &mut std::net::TcpStream) -> Result<serde_json::Value> {
    // macOS accept inherits O_NONBLOCK from the listener. Request/body reads below use the
    // socket timeout, while only the listener's accept loop uses nonblocking polling.
    socket
        .set_nonblocking(false)
        .context("configure blocking fixture request stream")?;
    socket.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut bytes = Vec::new();
    let (header_end, length) = loop {
        let mut chunk = [0; 4096];
        let count = socket
            .read(&mut chunk)
            .context("read scripted provider request headers")?;
        anyhow::ensure!(count != 0, "fixture request ended before headers");
        bytes.extend_from_slice(&chunk[..count]);
        anyhow::ensure!(bytes.len() < 2 * 1024 * 1024, "bounded fixture request");
        if let Some(index) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            let headers = std::str::from_utf8(&bytes[..index])?;
            let length = headers
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                .context("request content length")?
                .1
                .trim()
                .parse::<usize>()?;
            anyhow::ensure!(length < 2 * 1024 * 1024, "bounded fixture body");
            break (index + 4, length);
        }
    };
    while bytes.len() < header_end + length {
        let mut chunk = [0; 4096];
        let count = socket
            .read(&mut chunk)
            .context("read scripted provider request body")?;
        anyhow::ensure!(count != 0, "fixture request ended before body");
        bytes.extend_from_slice(&chunk[..count]);
    }
    Ok(serde_json::from_slice(
        &bytes[header_end..header_end + length],
    )?)
}

#[test]
fn config_saved_new_session_executes_builtin_read_and_cancels_a_real_next_run() -> Result<()> {
    let reactor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let _entered = reactor.enter();
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let address = listener.local_addr()?;
    let (held, held_rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let stopping = Arc::clone(&stop);
    let handle = std::thread::spawn(move || -> Result<()> {
        for index in 0..3 {
            let deadline = Instant::now() + Duration::from_secs(15);
            let mut socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if stopping.load(Ordering::Acquire) {
                            return Ok(());
                        }
                        anyhow::ensure!(
                            Instant::now() < deadline,
                            "provider request {index} missing"
                        );
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => {
                        return Err(error).context("accept scripted provider connection");
                    }
                }
            };
            let request = read_request(&mut socket)?;
            let messages = request["messages"].as_array().context("messages")?;
            if index == 1 {
                assert!(
                    messages.iter().any(|message| message["role"] == "tool"
                        && message["content"].as_str().is_some_and(
                            |content| content.contains("configured-session-read-canary")
                        )),
                    "actual builtin output must reach the provider"
                );
            }
            if index == 2 {
                socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {\"choices\":[{\"delta\":{\"content\":\"pending\"},\"finish_reason\":null}]}\n\n")?;
                held.send(())?;
                let mut byte = [0];
                match socket.read(&mut byte) {
                    Ok(0) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {}
                    Err(error) => {
                        return Err(error)
                            .context("observe cancellation closing held provider connection");
                    }
                    Ok(_) => anyhow::bail!("cancelled provider connection remained open"),
                }
                break;
            }
            let delta = if index == 0 {
                serde_json::json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"read-after-config","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"canary.txt\"}"}}]},"finish_reason":"tool_calls"}]})
            } else {
                serde_json::json!({"choices":[{"delta":{"content":"read complete"},"finish_reason":"stop"}]})
            };
            let body = format!("data: {delta}\n\ndata: [DONE]\n\n");
            write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )?;
        }
        Ok(())
    });
    let server = ProviderFixture {
        stop,
        handle: Some(handle),
    };
    let (_fixture, mut app, mut worker) =
        real_runtime_transition_fixture_at(&format!("http://{address}"))?;
    std::fs::write(
        app.workspace_root.join("canary.txt"),
        "configured-session-read-canary\n",
    )?;
    apply_real_config_save(&mut app, &mut worker)?;
    let new_path = app.session_log_dir.join("session-read-after-config.jsonl");
    execute_session_action(
        &mut app,
        &mut worker,
        AppAction::StartNewSession {
            session_log_path: new_path,
        },
    )?;
    app.composer.input = "read the fixture canary".to_owned();
    let action = app.submit_input()?.context("read submission")?;
    assert!(queue_run_admission(&mut app, &mut worker, &action)?);
    pump_until(&mut app, &mut worker, |app, worker| {
        !app.runtime.is_busy
            && worker
                .as_ref()
                .is_some_and(|worker| worker.pending_interactions.is_empty())
    })?;
    let reader = worker
        .as_ref()
        .and_then(|worker| worker.application.as_ref())
        .context("current session application")?
        .read_handle()?;
    let records = reader
        .read_entries()
        .context("read builtin result through the existing session owner")?;
    assert!(records.iter().any(|entry| matches!(entry, sigil_kernel::SessionLogEntry::ToolResultV3(result) if result.call_id == "read-after-config" && result.facts.status == "ok" && result.facts.error.is_none())), "new session must contain the successful builtin result");
    eprintln!(
        "configured session ready/read proved; preparing real cancellation in {}",
        app.session_id
    );
    app.composer.input = "wait for cancellation".to_owned();
    let action = app.submit_input()?.context("cancellation submission")?;
    assert!(queue_run_admission(&mut app, &mut worker, &action)?);
    held_rx.recv_timeout(Duration::from_secs(15))?;
    drain_worker_messages_inner(&mut app, &mut worker, None)?;
    poll_application_admission(&mut app, &mut worker)?;
    // Urgent UI cancellation uses the current worker's root owner directly, including before
    // an application projection can publish a run binding. Exercise that shipping path.
    let action = app
        .handle_key_event(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('c'),
            crossterm::event::KeyModifiers::CONTROL,
        ))?
        .context("real Ctrl-C cancellation")?;
    assert!(matches!(action, AppAction::CancelRun));
    process_app_action(&mut app, &mut worker, action)?;
    eprintln!("real Ctrl-C dispatched to current worker; waiting for cancellation settlement");
    pump_until(&mut app, &mut worker, |app, worker| {
        !app.runtime.is_busy
            && worker
                .as_ref()
                .is_some_and(|worker| worker.pending_interactions.is_empty())
    })?;
    assert!(app.worker_ready());
    eprintln!("real cancellation settled; reading the owned durable source");
    let cancellation_records = reader
        .read_event_records()
        .context("read cancellation records through the existing session owner")?;
    let requested = cancellation_records
        .iter()
        .filter(|record| {
            record
                .stored_event()
                .payload
                .get("record")
                .and_then(serde_json::Value::as_str)
                == Some("requested")
        })
        .map(|record| {
            serde_json::from_value::<sigil_kernel::RunCancellationRequestedEntry>(
                record.stored_event().payload.clone(),
            )
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let finalized = cancellation_records
        .iter()
        .filter(|record| {
            record
                .stored_event()
                .payload
                .get("record")
                .and_then(serde_json::Value::as_str)
                == Some("finalized")
        })
        .map(|record| {
            serde_json::from_value::<sigil_kernel::RunCancellationFinalizedEntry>(
                record.stored_event().payload.clone(),
            )
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(finalized.len(), 1);
    assert_eq!(requested.len(), 1);
    assert_eq!(finalized[0].request_id, requested[0].request_id);
    assert_eq!(finalized[0].run_scope_id, requested[0].run_scope_id);
    assert_eq!(
        requested[0].target,
        sigil_kernel::RunCancellationTarget::Run
    );
    assert!(finalized[0].cleanup_complete);
    assert_eq!(
        (finalized[0].active_effects, finalized[0].active_tasks),
        (0, 0)
    );
    assert_eq!(
        finalized[0].outcome,
        sigil_kernel::RunCancellationTerminalOutcome::Cancelled
    );
    shutdown::shutdown_tui_owners(
        &mut app,
        &mut worker,
        &mut TuiShutdownState::default(),
        &mut None,
        false,
        |_| {},
    )?;
    server.finish().context("join scripted provider fixture")?;
    Ok(())
}

#[test]
fn failed_session_attachment_rejects_later_input_without_a_phantom_preparing_run() -> Result<()> {
    let (fixture, mut app, mut worker) = real_runtime_transition_fixture()?;
    let foreign = Arc::new(
        sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease::acquire(
            fixture.path().join("foreign.jsonl"),
        )?,
    );
    app.retain_worker_session_attachment(app.session_log_path.clone(), foreign);
    let config = app
        .session_runtime_config_snapshot()
        .cloned()
        .context("runtime config")?;
    maintain(&mut app, &mut worker, Some(config))?;
    let deadline = Instant::now() + Duration::from_secs(15);
    while app
        .runtime_maintenance
        .as_ref()
        .is_some_and(|owner| owner.is_running())
    {
        anyhow::ensure!(
            Instant::now() < deadline,
            "failed attachment cleanup did not join"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    poll_maintenance(&mut app, &mut worker)?;
    assert!(worker.is_none());
    assert!(
        app.runtime_maintenance
            .as_ref()
            .is_some_and(|owner| owner.has_failed())
    );
    assert!(app.last_notice().is_some_and(|notice| {
        notice.contains("transferred worker attachment belongs to another durable session")
    }));
    let unchanged = std::fs::read(&app.session_log_path)?;
    let draft = "keep this input\nsecond line";
    app.composer.input = draft.to_owned();
    let action = app
        .submit_input()?
        .context("Enter creates a local submission intent")?;
    assert!(
        app.runtime.is_busy,
        "exercise the actual optimistic submission"
    );
    process_app_action(&mut app, &mut worker, action)?;
    assert_eq!(app.composer.input, draft);
    assert!(!app.runtime.is_busy);
    assert!(!app.worker_ready());
    assert!(app.runtime.pending_worker_commands.is_empty());
    assert!(app.deferred_application_actions.is_empty());
    assert!(app.retained_application_admissions.is_empty());
    assert!(app.runtime.pending_session_route_recovery_code.is_none());
    assert_eq!(
        std::fs::read(&app.session_log_path)?,
        unchanged,
        "rejected input must not admit a durable run"
    );
    assert!(
        app.last_notice()
            .is_some_and(|notice| notice.contains("session runtime is unavailable; input kept"))
    );
    shutdown::shutdown_tui_owners(
        &mut app,
        &mut worker,
        &mut TuiShutdownState::default(),
        &mut None,
        false,
        |_| {},
    )?;
    Ok(())
}
