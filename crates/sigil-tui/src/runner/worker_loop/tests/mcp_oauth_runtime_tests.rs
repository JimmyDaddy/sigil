use super::*;

#[test]
fn shutdown_does_not_block_on_a_full_oauth_control_lane() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build OAuth shutdown test runtime");
    let (control_tx, control_rx) = tokio::sync::mpsc::channel(1);
    control_tx
        .try_send(sigil_runtime::McpOAuthFlowControl::Cancel)
        .expect("fill OAuth control lane");
    let cancelled = Arc::new(AtomicBool::new(false));
    let active = ActiveMcpOAuthFlow {
        control_tx,
        cancelled: Arc::clone(&cancelled),
        handle: runtime.spawn(std::future::pending()),
    };
    let (returned, received) = mpsc::channel();
    let stop_thread = std::thread::spawn(move || {
        returned
            .send(active.request_stop())
            .expect("return owned OAuth task")
    });
    let result = received.recv_timeout(Duration::from_millis(100));
    // Always release the fixture lane before asserting, even if a blocking-send regression
    // prevented shutdown from returning its still-owned task handle.
    drop(control_rx);
    stop_thread.join().expect("join OAuth stop request");
    let mut handles = vec![result.expect("a full OAuth lane must not block stop")];
    assert!(cancelled.load(Ordering::Acquire));
    super::super::shutdown::drain_owned_tasks_until(
        &mut handles,
        &mut false,
        &runtime,
        Instant::now() + Duration::from_secs(1),
    )
    .expect("drain stopped OAuth task");
    assert!(handles.is_empty());
}
