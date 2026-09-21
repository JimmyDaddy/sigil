use super::*;
use std::sync::mpsc;

fn fixture_app(path: &std::path::Path) -> AppState {
    let mut config = crate::app::tests::common::test_config();
    config.workspace.root = path.display().to_string();
    AppState::from_root_config(&path.join("sigil.toml"), &config)
}

fn worker_with(handle: std::thread::JoinHandle<()>) -> WorkerRuntime {
    let (worker_tx, _) = runner::WorkerCommandSender::test_channel();
    WorkerRuntime {
        worker_tx,
        application: None,
        pending_admission: None,
        pending_interactions: Vec::new(),
        worker_rx: mpsc::channel().1,
        join_handle: Some(handle),
        ready: true,
    }
}

#[test]
fn exit_before_hint_joins_every_owner_without_a_slow_notice() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let mut app = fixture_app(fixture.path());
    let handle = std::thread::spawn(|| {});
    while !handle.is_finished() {
        std::thread::yield_now();
    }
    let mut worker = Some(worker_with(handle));
    let sender = worker
        .as_ref()
        .expect("fixture worker remains installed")
        .worker_tx
        .clone();
    let mut notices = Vec::new();
    shutdown_tui_owners(
        &mut app,
        &mut worker,
        &mut TuiShutdownState::default(),
        &mut None,
        false,
        |message| notices.push(message.to_owned()),
    )?;
    assert!(
        worker.is_none(),
        "normal exit consumed the actual JoinHandle"
    );
    assert!(notices.is_empty());
    assert!(
        sender
            .shutdown_diagnostic("worker")
            .contains("cleanup_complete=true")
    );
    Ok(())
}

#[test]
fn slow_exit_waits_for_real_worker_and_runtime_join_then_returns_success() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let mut app = fixture_app(fixture.path());
    let (worker_release, release) = mpsc::channel();
    let mut worker = Some(worker_with(std::thread::spawn(move || {
        release.recv().expect("release worker");
    })));
    let event_runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;
    let (runtime_release, release) = mpsc::channel();
    let (entered, ready) = mpsc::channel();
    let (finished, finished_rx) = mpsc::channel();
    event_runtime.spawn_blocking(move || {
        entered
            .send(())
            .expect("announce blocking observation entry");
        release.recv().expect("release blocking observation");
        finished
            .send(())
            .expect("announce blocking observation completion");
    });
    ready.recv_timeout(Duration::from_secs(2))?;
    let mut event_runtime = Some(event_runtime);
    let mut state = TuiShutdownState {
        hint_after: Duration::ZERO,
        ..Default::default()
    };
    let mut notices = Vec::new();
    let mut releases = Some((worker_release, runtime_release));
    shutdown_tui_owners(
        &mut app,
        &mut worker,
        &mut state,
        &mut event_runtime,
        false,
        |message| {
            notices.push(message.to_owned());
            if let Some((worker, runtime)) = releases.take() {
                assert!(message.contains("Still cleaning up"));
                assert!(matches!(
                    finished_rx.try_recv(),
                    Err(mpsc::TryRecvError::Empty)
                ));
                worker.send(()).expect("release the actual worker");
                runtime
                    .send(())
                    .expect("release the owned runtime observation");
            }
        },
    )?;
    finished_rx.recv_timeout(Duration::from_secs(1))?;
    assert!(worker.is_none());
    assert!(event_runtime.is_none());
    assert!(
        notices
            .last()
            .expect("slow exit emits a final cleanup notice")
            .contains("Cleanup completed")
    );
    Ok(())
}

#[test]
fn confirmed_panic_is_preserved_while_other_owners_still_join() -> Result<()> {
    let (release, gate) = mpsc::channel();
    let mut slow = Some(std::thread::spawn(move || {
        gate.recv().expect("wait for explicit slow owner release");
    }));
    let mut failed = Some(std::thread::spawn(|| panic!("controlled cleanup failure")));
    let mut release = Some(release);
    let outcome = drain_shutdown(
        Instant::now(),
        Duration::ZERO,
        || {
            let mut pass = ShutdownPass::default();
            pass.observe("failed-owner", poll_owned_thread(&mut failed));
            pass.observe("slow-owner", poll_owned_thread(&mut slow));
            pass
        },
        |_| {
            if let Some(release) = release.take() {
                release
                    .send(())
                    .expect("release remaining owner after panic");
            }
        },
    );
    assert!(
        format!(
            "{:#}",
            outcome.expect_err("confirmed panic must remain a shutdown failure")
        )
        .contains("controlled cleanup failure")
    );
    assert!(failed.is_none());
    assert!(
        slow.is_none(),
        "confirmed failure does not detach the remaining owner"
    );
    Ok(())
}

#[test]
fn bootstrap_cleanup_error_is_returned_after_its_thread_joins() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let mut app = fixture_app(fixture.path());
    app.session_log_path = fixture.path().join("missing/session-bootstrap.jsonl");
    let error = shutdown_tui_owners(
        &mut app,
        &mut None,
        &mut TuiShutdownState::default(),
        &mut None,
        true,
        |_| {},
    )
    .expect_err("read failure must not be reported as successful cleanup");
    assert!(format!("{error:#}").contains("bootstrap-session-cleanup"));
    Ok(())
}
