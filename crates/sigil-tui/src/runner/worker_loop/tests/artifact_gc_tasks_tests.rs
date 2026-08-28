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
    };

    assert!(!tasks.has_active());

    for handle in &tasks.retired {
        handle.abort();
    }
    tasks.abort_all();
    tasks.cancel_and_join(&runtime);
}
