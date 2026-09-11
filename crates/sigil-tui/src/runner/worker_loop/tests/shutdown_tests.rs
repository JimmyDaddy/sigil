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
    let mut handles = vec![handle];
    let mut task_panicked = false;
    reap_finished_owned_tasks(&mut handles, &mut task_panicked);
    assert!(handles.is_empty());
    assert!(task_panicked);
    for _ in 0..2 {
        assert!(matches!(
            drain_owned_tasks_until(&mut handles, &mut task_panicked, &runtime, deadline),
            Err(OwnedTaskDrainFailure::TaskPanicked)
        ));
    }
}
