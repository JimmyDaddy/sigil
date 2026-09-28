use super::*;

#[test]
fn aborted_blocking_task_retains_ownership_until_actual_completion() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build blocking shutdown test runtime");
    let (entered, entry) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let mut handles = vec![runtime.spawn_blocking(move || {
        entered.send(()).expect("announce blocking task entry");
        released.recv().expect("wait for blocking task release");
    })];
    entry
        .recv_timeout(Duration::from_secs(1))
        .expect("blocking task starts before drain");
    let mut task_panicked = false;
    let started = Instant::now();
    assert!(matches!(
        drain_owned_tasks_until(
            &mut handles,
            &mut task_panicked,
            &runtime,
            started + Duration::from_millis(30)
        ),
        Err(OwnedTaskDrainFailure::DeadlineExceeded { pending_tasks: 1 })
    ));
    assert!(!handles[0].is_finished());
    assert!(started.elapsed() < Duration::from_millis(300));
    release.send(()).expect("release blocking task");
    drain_owned_tasks_until(
        &mut handles,
        &mut task_panicked,
        &runtime,
        Instant::now() + Duration::from_secs(1),
    )
    .expect("drain released blocking task");
    assert!(handles.is_empty());
}
#[test]
fn reaped_task_panic_remains_a_shutdown_failure_without_retaining_the_handle() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build task panic test runtime");
    let handle = runtime.spawn(async { panic!("owned task fixture panic") });
    let deadline = Instant::now() + Duration::from_secs(1);
    while !handle.is_finished() && Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert!(handle.is_finished());
    let id = handle.id();
    let mut handles = vec![handle];
    let mut task_panicked = false;
    let joined = reap_finished_owned_tasks(&mut handles, &mut task_panicked);
    assert_eq!(
        joined,
        vec![(id, false)],
        "panic is not a successful joined owner"
    );
    assert!(handles.is_empty());
    assert!(task_panicked);
    for _ in 0..2 {
        assert!(matches!(
            drain_owned_tasks_until(&mut handles, &mut task_panicked, &runtime, deadline),
            Err(OwnedTaskDrainFailure::TaskPanicked)
        ));
    }
}

#[test]
fn explicit_shutdown_keeps_draining_after_pending_poll_without_latching_failure() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build retained owner runtime");
    let (entered, entry) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let mut handles = vec![runtime.spawn_blocking(move || {
        entered.send(()).expect("announce owned task");
        released.recv().expect("release owned task");
    })];
    entry
        .recv_timeout(Duration::from_secs(1))
        .expect("owner started");
    let (sender, _commands) = crate::runner::WorkerCommandSender::test_channel();
    sender.begin_shutdown();
    let stop_control = sender.stop_control();
    let (messages, _message_rx) = mpsc::channel();
    let (pending, pending_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut task_panicked = false;
        drain_stopping_owner(
            WorkerShutdownStage::SessionMaintenance,
            &stop_control,
            &messages,
            |deadline| {
                let result =
                    drain_owned_tasks_until(&mut handles, &mut task_panicked, &runtime, deadline);
                if matches!(result, Err(OwnedTaskDrainFailure::DeadlineExceeded { .. })) {
                    let _ = pending.send(());
                }
                result
            },
        );
        assert!(handles.is_empty());
        assert!(!task_panicked);
    });
    let observed_pending = pending_rx.recv_timeout(Duration::from_secs(3));
    let still_owned = !worker.is_finished();
    release.send(()).expect("release real blocking work");
    worker.join().expect("join explicit drain");
    observed_pending.expect("the actual bounded manager poll should expire first");
    assert!(still_owned);
    assert!(
        sender.cleanup_complete(),
        "a pending poll must not poison the eventual success: {}",
        sender.shutdown_diagnostic("fixture")
    );
}

#[test]
fn explicit_shutdown_keeps_panic_failure_after_draining_other_owned_work() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let mut handles = vec![
        runtime.spawn(async { panic!("real drain panic") }),
        runtime.spawn(async {}),
    ];
    let mut task_panicked = false;
    let (sender, _commands) = crate::runner::WorkerCommandSender::test_channel();
    sender.begin_shutdown();
    let (messages, _message_rx) = mpsc::channel();
    for _ in 0..2 {
        drain_stopping_owner(
            WorkerShutdownStage::RunQuiescence,
            &sender.stop_control(),
            &messages,
            |_| join_owned_tasks(&mut handles, &mut task_panicked, &runtime),
        );
        assert!(handles.is_empty());
        assert!(task_panicked);
        assert!(
            !sender.cleanup_complete(),
            "a retry must not clear actual panic evidence"
        );
    }
}

#[test]
fn reaper_reports_only_consumed_successful_owner_joins() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let (release, released) = tokio::sync::oneshot::channel();
    let handle = runtime.spawn(async move {
        released.await.expect("release");
    });
    let id = handle.id();
    let mut handles = vec![handle];
    let mut panicked = false;
    assert!(reap_finished_owned_tasks(&mut handles, &mut panicked).is_empty());
    release.send(()).expect("release real owner");
    let deadline = Instant::now() + Duration::from_secs(1);
    while !handles[0].is_finished() && Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert_eq!(
        reap_finished_owned_tasks(&mut handles, &mut panicked),
        vec![(id, true)]
    );
    assert!(handles.is_empty());
    assert!(reap_finished_owned_tasks(&mut handles, &mut panicked).is_empty());
}
