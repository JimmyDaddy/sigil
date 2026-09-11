use std::future;

use super::*;

#[test]
fn retired_result_handle_does_not_keep_gc_conflict_gate_active() {
    let runtime = Runtime::new().expect("test runtime");
    let handle = runtime.spawn(async { future::pending::<()>().await });
    assert!(!handle.is_finished());
    let mut tasks = ArtifactGcTaskManager {
        active: None,
        retired: vec![handle],
        task_panicked: false,
    };

    assert!(!tasks.has_active());

    for handle in &tasks.retired {
        handle.abort();
    }
    tasks.abort_all();
    tasks.cancel_and_join(&runtime);
}

#[test]
fn finished_artifact_gc_panic_survives_result_acceptance_and_shutdown() {
    let runtime = tokio::runtime::Runtime::new().expect("build artifact GC test runtime");
    let handle = runtime.spawn(async { panic!("artifact GC fixture panic") });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    while !handle.is_finished() && std::time::Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert!(handle.is_finished());
    let mut manager = ArtifactGcTaskManager::new();
    manager.active = Some(super::ActiveArtifactGcTask {
        request_id: 1,
        session_scope_id: "panic-scope".to_owned(),
        cancelled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        handle,
    });
    assert!(manager.accept_result(1, "panic-scope"));
    assert!(manager.retired.is_empty());
    assert!(manager.task_panicked);
    for _ in 0..2 {
        assert!(matches!(
            manager.shutdown_until(&runtime, deadline),
            Err(super::super::shutdown::OwnedTaskDrainFailure::TaskPanicked)
        ));
    }
}
