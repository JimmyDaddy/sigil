use super::*;

fn real_runtime_transition_fixture() -> Result<(tempfile::TempDir, AppState, Option<WorkerRuntime>)>
{
    let fixture = tempfile::tempdir()?;
    let workspace = fixture.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let config_path = fixture.path().join("sigil.toml");
    let state = fixture.path().join("state");
    let cache = fixture.path().join("cache");
    for path in [&state, &cache] {
        std::fs::create_dir(path)?;
        sigil_kernel::config::secure_private_path_permissions(path)?;
    }
    let mut config = crate::app::tests::common::test_config();
    config.composition = sigil_kernel::RuntimeCompositionConfig::core();
    config.workspace.root = workspace.display().to_string();
    config.storage.state_root = sigil_kernel::StorageRoot::Path(state.display().to_string());
    config.storage.cache_root = sigil_kernel::StorageRoot::Path(cache.display().to_string());
    config.session.log_dir = None;
    config.agent.runtime_provider.clear();
    config.agent.connection = Some(sigil_kernel::ConnectionId::new("local-test")?);
    config.agent.model = "local-model".to_owned();
    config.connections = std::collections::BTreeMap::from([(
        "local-test".to_owned(),
        serde_json::json!({
            "label": "local",
            "provider": "custom",
            "protocol": "chat_completions",
            "base_url": "http://127.0.0.1:1",
            "credential": { "source": "none" }
        }),
    )]);
    config.save(&config_path)?;
    let boot = sigil_runtime::application_host::boot_current_schema(&config_path, fixture.path())?;
    let (config, _, _, cutover, composition, _) = boot.into_published_parts();
    let mut app = AppState::from_root_config(&config_path, &config);
    app.set_authority_composition(Arc::new(composition));
    app.set_boot_cutover(Arc::new(cutover));
    app.enter_workspace_trust_gate()?;
    app.confirm_workspace_trust_gate()?;
    let spawned = runner::spawn_agent_worker_with_route_directive_and_attachment(
        config,
        config_path,
        app.session_log_path.clone(),
        app.workspace_root.clone(),
        sigil_kernel::InteractionMode::Interactive,
        runner::WorkerSessionRouteDirective::default(),
        app.authority_composition().cloned(),
        app.boot_cutover().cloned(),
        app.worker_session_attachment(),
    )?;
    let application = Arc::new(application_bridge::build_for_worker(
        &app,
        spawned.command_tx.clone(),
        app.runtime.reasoning_effort.clone(),
        spawned.projection_owner,
    )?);
    let mut worker = WorkerRuntime {
        worker_tx: spawned.command_tx,
        application: Some(application),
        pending_admission: None,
        pending_interactions: Vec::new(),
        worker_rx: spawned.message_rx,
        join_handle: Some(spawned.join_handle),
        ready: false,
    };
    let deadline = Instant::now() + Duration::from_secs(15);
    while !worker.ready {
        let message = worker
            .worker_rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))?;
        anyhow::ensure!(
            !matches!(
                &message,
                WorkerMessage::RunFailed(_) | WorkerMessage::SessionRouteRecoveryRequired { .. }
            ),
            "real fixture worker failed before Ready: {message:?}"
        );
        apply_worker_message_state(&mut worker, None, &message);
        app.handle_worker_message(message)?;
    }
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(
            worker
                .application
                .as_ref()
                .context("real application")?
                .refresh(),
        )?;
    Ok((fixture, app, Some(worker)))
}

fn wait_for_real_transition(app: &AppState) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(15);
    while app
        .runtime_transition
        .as_ref()
        .context("transition owner must remain installed")?
        .is_running()
    {
        anyhow::ensure!(Instant::now() < deadline, "real transition did not finish");
        std::thread::sleep(Duration::from_millis(2));
    }
    Ok(())
}

#[test]
fn real_runtime_transition_and_replay_refresh_under_owned_tokio_runtime() -> Result<()> {
    assert!(tokio::runtime::Handle::try_current().is_err());
    let (_fixture, mut app, mut worker) = real_runtime_transition_fixture()?;
    let original = std::fs::read(&app.session_log_path)?;
    let route = app
        .current_session_route()
        .context("initial session route")?;
    app.composer.input = "preserve this draft across permission changes".to_owned();
    for (index, mode) in [
        sigil_kernel::PermissionMode::AutoEdit,
        sigil_kernel::PermissionMode::Manual,
    ]
    .into_iter()
    .enumerate()
    {
        let mut config = app
            .persisted_config_snapshot()
            .cloned()
            .context("saved configuration")?;
        config.permission.mode = mode;
        config.save(&app.config_path)?;
        app.apply_persisted_config_snapshot(&config);
        start(&mut app, &mut worker, route.clone())?;
        wait_for_real_transition(&app)?;
        if index == 0 {
            // Exercise the retry entry with the original durable command identity. Replaying a
            // settled transition must refresh under the same owned reactor without a new effect.
            let owner = app
                .runtime_transition
                .as_mut()
                .context("retained transition")?;
            owner
                .handle
                .take()
                .context("completed transition thread")?
                .join()
                .map_err(|_| anyhow::anyhow!("session controller panicked"))?;
            owner.launch(true)?;
            wait_for_real_transition(&app)?;
        }
        assert!(poll(&mut app, &mut worker)?);
        let owner = app.runtime_transition.as_ref().context("completed owner")?;
        assert!(owner.completed);
        assert!(owner.activation_acknowledged);
        assert_eq!(owner.intent_bound, Some(true));
        assert!(
            owner
                .cleanup_failure
                .lock()
                .expect("cleanup ledger")
                .is_none()
        );
        assert!(owner.application.current_projection()?.is_some());
        assert_eq!(owner.application.pending_observations(), 0);
        let mut phases = [0; 3];
        for record in owner.application.read_handle()?.read_event_records()? {
            if let Some(sigil_kernel::SessionLogEntry::Control(
                sigil_kernel::ControlEntry::SessionRuntimeTransitionV1(transition),
            )) = record.session_log_entry()?
            {
                let phase = match transition.transition {
                    sigil_kernel::SessionRuntimeTransitionPhaseV1::Intent { .. } => 0,
                    sigil_kernel::SessionRuntimeTransitionPhaseV1::Configured { .. } => 1,
                    sigil_kernel::SessionRuntimeTransitionPhaseV1::Activated { .. } => 2,
                };
                phases[phase] += 1;
            }
        }
        assert_eq!(
            phases,
            [index + 1; 3],
            "replay must not duplicate durable effects"
        );
        assert!(worker.as_ref().is_some_and(|runtime| runtime.ready));
        assert_eq!(app.runtime.permission_mode, mode.as_str());
        assert_eq!(
            app.composer.input,
            "preserve this draft across permission changes"
        );
        assert!(std::fs::read(&app.session_log_path)?.starts_with(&original));
    }
    shutdown::shutdown_tui_owners(
        &mut app,
        &mut worker,
        &mut TuiShutdownState::default(),
        &mut None,
        false,
        |_| {},
    )?;
    assert!(worker.is_none());
    assert!(app.runtime_transition.is_none());
    Ok(())
}

#[test]
fn exit_joins_real_session_controller_runtime_and_replacement_worker() -> Result<()> {
    let (_fixture, mut app, mut worker) = real_runtime_transition_fixture()?;
    let route = app
        .current_session_route()
        .context("initial session route")?;
    start(&mut app, &mut worker, route)?;
    shutdown::shutdown_tui_owners(
        &mut app,
        &mut worker,
        &mut TuiShutdownState::default(),
        &mut None,
        false,
        |_| {},
    )?;
    assert!(worker.is_none());
    assert!(app.runtime_transition.is_none());
    Ok(())
}

#[test]
fn ordinary_transition_cleanup_failure_survives_consuming_the_worker() -> Result<()> {
    let (worker_tx, _) = runner::WorkerCommandSender::test_channel();
    let mut worker = Some(WorkerRuntime {
        worker_tx,
        application: None,
        pending_admission: None,
        pending_interactions: Vec::new(),
        worker_rx: mpsc::channel().1,
        join_handle: Some(std::thread::spawn(|| panic!("transition cleanup panic"))),
        ready: true,
    });
    let failure = Arc::default();
    let outcome = stop_owned_worker(&mut worker, &failure);
    assert!(outcome.is_err());
    assert!(
        worker.is_none(),
        "the failed worker's real join was consumed"
    );
    assert!(
        failure
            .lock()
            .expect("cleanup failure ledger lock")
            .as_deref()
            .expect("real cleanup panic remains recorded")
            .contains("transition cleanup panic")
    );
    // The same sticky evidence is transferred across replacement owners; an empty retry must
    // not erase an earlier actual cleanup failure before the final launcher drain sees it.
    stop_owned_worker(&mut worker, &failure)?;
    assert!(
        failure
            .lock()
            .expect("cleanup failure ledger lock")
            .is_some()
    );
    Ok(())
}

#[test]
fn maintenance_replacement_preserves_actual_cleanup_failure_until_launcher_exit() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let mut config = crate::app::tests::common::test_config();
    config.workspace.root = fixture.path().display().to_string();
    let mut app = AppState::from_root_config(&fixture.path().join("sigil.toml"), &config);
    let (worker_tx, _) = runner::WorkerCommandSender::test_channel();
    let mut worker = Some(WorkerRuntime {
        worker_tx,
        application: None,
        pending_admission: None,
        pending_interactions: Vec::new(),
        worker_rx: mpsc::channel().1,
        join_handle: Some(std::thread::spawn(|| panic!("maintenance stop panic"))),
        ready: true,
    });
    maintain(&mut app, &mut worker, None)?;
    let until = Instant::now() + Duration::from_secs(2);
    while app
        .runtime_maintenance
        .as_ref()
        .expect("maintenance owner remains installed")
        .is_running()
    {
        anyhow::ensure!(Instant::now() < until, "maintenance must settle");
        std::thread::yield_now();
    }
    poll_maintenance(&mut app, &mut worker)?;
    assert!(worker.is_none());
    // A new maintenance operation replaces the old UI owner after its failed worker was joined.
    maintain(&mut app, &mut worker, None)?;
    let result = shutdown::shutdown_tui_owners(
        &mut app,
        &mut worker,
        &mut TuiShutdownState::default(),
        &mut None,
        false,
        |_| {},
    );
    assert!(
        format!(
            "{:#}",
            result.expect_err("replacement cannot erase real stop failure")
        )
        .contains("maintenance stop panic")
    );
    assert!(app.runtime_maintenance.is_none());
    Ok(())
}

#[test]
fn outer_error_closes_an_idle_worker_before_joining_its_owner() -> Result<()> {
    let (worker_tx, commands) = runner::WorkerCommandSender::test_channel();
    let (stopped, stop_result) = mpsc::channel();
    let worker_handle = std::thread::spawn(move || {
        assert!(matches!(
            commands.recv(),
            Ok(runner::WorkerCommand::Shutdown)
        ));
        stopped.send(()).expect("observe shutdown");
    });
    let (exited, exit_result) = mpsc::channel();
    let exit_owner = std::thread::spawn(move || {
        let fail_after_install = || -> Result<()> {
            let _worker = WorkerRuntime {
                worker_tx,
                application: None,
                pending_admission: None,
                pending_interactions: Vec::new(),
                worker_rx: mpsc::channel().1,
                join_handle: Some(worker_handle),
                ready: true,
            };
            anyhow::bail!("controlled outer error before shutdown");
        };
        assert!(fail_after_install().is_err());
        exited.send(()).expect("observe completed exit");
    });
    stop_result.recv_timeout(Duration::from_secs(1))?;
    exit_result.recv_timeout(Duration::from_secs(1))?;
    exit_owner.join().expect("outer error cleanup");
    Ok(())
}

#[test]
fn activated_route_is_published_before_ready_clears_pending_selection() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let mut config = crate::app::tests::common::test_config();
    config.workspace.root = fixture.path().display().to_string();
    let mut app = AppState::from_root_config(&fixture.path().join("sigil.toml"), &config);
    let original_model = app.runtime.model_name.clone();
    let original_route = app.current_session_route();
    let (provider, route, _) =
        sigil_runtime::provider_connections::ResolvedRouteConfigSnapshot::from_root_config(&config)
            .resolved_route(&sigil_kernel::ModelRef::new(
                config
                    .agent
                    .connection
                    .clone()
                    .expect("configured connection"),
                "deepseek-v4-pro",
            )?)
            .expect("selected route");
    app.mark_pending_session_route_selection(provider.clone(), route.clone());
    assert_eq!(app.runtime.model_name, original_model);
    assert_eq!(app.current_session_route(), original_route);
    let runtime_config = app.runtime_config_for_session_route(config, &route)?;
    app.apply_activated_session_route(runtime_config, provider.clone(), route.clone());
    app.handle_worker_message(WorkerMessage::WorkerReady)?;
    assert_eq!(app.runtime.provider_name, provider);
    assert_eq!(app.runtime.model_name, route.model_ref.model_id);
    assert_eq!(app.current_session_route().as_ref(), Some(&route));
    assert!(app.pending_session_route_selection().is_none());
    assert_eq!(
        app.session_runtime_config_snapshot()
            .expect("active configuration")
            .agent
            .model,
        route.model_ref.model_id,
    );
    Ok(())
}

#[test]
fn route_selection_without_application_owner_preserves_the_live_worker_and_draft() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let mut config = crate::app::tests::common::test_config();
    config.workspace.root = fixture.path().display().to_string();
    let mut app = AppState::from_root_config(&fixture.path().join("sigil.toml"), &config);
    app.composer.input = "retained draft".to_owned();
    let route =
        sigil_runtime::provider_connections::ResolvedRouteConfigSnapshot::from_root_config(&config)
            .resolved_route(&sigil_kernel::ModelRef::new(
                config
                    .agent
                    .connection
                    .clone()
                    .expect("configured connection"),
                config.agent.model.clone(),
            )?)
            .expect("configured route")
            .1;
    let original_route = app.current_session_route();
    let (worker_tx, commands) = runner::WorkerCommandSender::test_channel();
    let mut worker = Some(WorkerRuntime {
        worker_tx,
        application: None,
        pending_admission: None,
        pending_interactions: Vec::new(),
        worker_rx: mpsc::channel().1,
        join_handle: None,
        ready: true,
    });
    assert!(start(&mut app, &mut worker, route).is_err());
    assert!(!poll(&mut app, &mut worker)?);
    assert!(worker.as_ref().is_some_and(|runtime| runtime.ready));
    assert!(commands.try_recv().is_err());
    assert_eq!(app.composer.input, "retained draft");
    assert_eq!(app.current_session_route(), original_route);
    assert!(
        app.runtime_transition
            .as_ref()
            .and_then(RuntimeTransitionOwner::observation_application)
            .is_none()
    );
    Ok(())
}

#[test]
fn replacement_setup_view_retains_background_cleanup_and_rejects_old_result() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let mut config = crate::app::tests::common::test_config();
    config.workspace.root = fixture.path().display().to_string();
    let path = fixture.path().join("sigil.toml");
    let mut previous = AppState::from_root_config(&path, &config);
    let (worker_tx, commands) = runner::WorkerCommandSender::test_channel();
    let (release, blocked) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        blocked.recv().expect("release old worker");
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
    maintain(&mut previous, &mut worker, None)?;
    commands.recv_timeout(Duration::from_secs(1))?;
    let mut replacement = AppState::from_root_config(&path, &config);
    replacement.set_last_notice("replacement setup view");
    let started = Instant::now();
    previous
        .runtime_maintenance
        .as_mut()
        .expect("previous view retains the active maintenance owner")
        .invalidate_view();
    replacement.runtime_maintenance = previous.runtime_maintenance.take();
    drop(previous);
    assert!(started.elapsed() < Duration::from_millis(100));
    assert!(replacement.has_pending_background_tasks());
    assert!(!poll_maintenance(&mut replacement, &mut worker)?);
    assert!(worker.is_none());
    release.send(())?;
    let deadline = Instant::now() + Duration::from_secs(2);
    while replacement
        .runtime_maintenance
        .as_ref()
        .expect("replacement view retains cleanup ownership")
        .is_running()
    {
        anyhow::ensure!(
            Instant::now() < deadline,
            "retired owner cleanup did not finish"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(poll_maintenance(&mut replacement, &mut worker)?);
    assert!(!poll_maintenance(&mut replacement, &mut worker)?);
    assert!(worker.is_none());
    assert_eq!(replacement.last_notice(), Some("replacement setup view"));
    Ok(())
}

#[test]
fn owned_stop_keeps_production_input_dispatch_responsive_and_exit_retains_the_worker() -> Result<()>
{
    let fixture = tempfile::tempdir()?;
    let mut config = crate::app::tests::common::test_config();
    config.workspace.root = fixture.path().display().to_string();
    let mut app = AppState::from_root_config(&fixture.path().join("sigil.toml"), &config);
    let (worker_tx, commands) = runner::WorkerCommandSender::test_channel();
    let (release, blocked) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        blocked.recv().expect("release owned worker");
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
    maintain(&mut app, &mut worker, None)?;
    assert!(started.elapsed() < Duration::from_millis(100));
    assert!(worker.is_none());
    commands.recv_timeout(Duration::from_secs(1))?;
    let mut latencies = Vec::new();
    for _ in 0..128 {
        let before = app.composer.input.len();
        let started = Instant::now();
        // This is the same lifecycle polling, worker draining and key dispatch as run_app.
        poll_maintenance(&mut app, &mut worker)?;
        drain_worker_messages_inner(&mut app, &mut worker, None)?;
        app.handle_key_event(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('x'),
            crossterm::event::KeyModifiers::NONE,
        ))?;
        assert_eq!(app.composer.input.len(), before + 1);
        latencies.push(started.elapsed());
        app.handle_key_event(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Backspace,
            crossterm::event::KeyModifiers::NONE,
        ))?;
    }
    latencies.sort_unstable();
    let p95 = latencies[121];
    assert!(p95 < Duration::from_millis(100));
    let owner = app
        .runtime_maintenance
        .as_ref()
        .expect("host retains transition");
    owner.request_exit();
    assert!(owner.is_running(), "exit request is not worker termination");
    assert!(app.has_pending_background_tasks());
    release.send(())?;
    let deadline = Instant::now() + Duration::from_secs(2);
    while app
        .runtime_maintenance
        .as_ref()
        .is_some_and(|owner| owner.is_running())
    {
        anyhow::ensure!(Instant::now() < deadline, "owned cleanup did not finish");
        std::thread::sleep(Duration::from_millis(2));
    }
    poll_maintenance(&mut app, &mut worker)?;
    assert!(worker.is_none());
    assert!(
        app.runtime_maintenance
            .as_ref()
            .expect("host retains the completed maintenance owner")
            .workers
            .lock()
            .expect("maintenance worker ownership mutex should not be poisoned")
            .is_none()
    );
    eprintln!(
        "tui lifecycle blocked_stop input_samples=128 input_p95_us={} exit_kept_owner=true",
        p95.as_micros()
    );
    Ok(())
}
