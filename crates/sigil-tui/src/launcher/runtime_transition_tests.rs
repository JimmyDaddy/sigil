use super::*;

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
