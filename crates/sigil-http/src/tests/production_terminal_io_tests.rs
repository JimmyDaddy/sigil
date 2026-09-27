use super::*;

#[tokio::test]
async fn production_terminal_io_yields_the_executor_and_joins_before_returning() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let driver = production_queue_driver(&temp, "terminal-io");
    let registry = driver.build_registry(Arc::new(HttpDurableCommandStore::open(
        temp.path().join("commands.json"),
        16,
    )?))?;
    let session = registry.create_session(HttpSessionCreateRequest::default())?;
    let terminal_io = HttpRunTerminalIo::new(&registry, &driver.event_bus, &session, "terminal-io");
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let receipt = temp.path().join("joined-receipt");
    let worker_receipt = receipt.clone();
    let worker = tokio::spawn(async move {
        terminal_io
            .run(move |_| {
                let _ = entered_tx.send(());
                // A finite controlled blocking boundary lets this test fail cleanly if I/O is ever
                // moved back onto the single-thread executor, without stranding the test runtime.
                release_rx
                    .recv_timeout(Duration::from_secs(2))
                    .map_err(|error| HttpRunDriverError::new(error.to_string()))?;
                std::fs::write(worker_receipt, b"durable completion")
                    .map_err(|error| HttpRunDriverError::new(error.to_string()))
            })
            .await
    });
    entered_rx.await?;
    let still_pending = !worker.is_finished();
    let receipt_before_release = receipt.exists();
    let _ = release_tx.send(());
    worker.await??;
    assert!(
        still_pending,
        "blocking work must leave the async executor responsive"
    );
    assert!(
        !receipt_before_release,
        "completion must wait for the owned worker"
    );
    assert_eq!(std::fs::read(receipt)?, b"durable completion");
    Ok(())
}
