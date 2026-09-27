use std::{future::poll_fn, task::Poll};

use super::*;

fn active_run(session_id: &str) -> Arc<HttpProductionActiveRun> {
    let (cancel_sender, _receiver) = mpsc::unbounded_channel();
    Arc::new(HttpProductionActiveRun {
        session_id: session_id.to_owned(),
        broker: Arc::new(HttpApprovalBroker::default()),
        cancel_sender,
        projection_owner: Arc::new(Mutex::new(None)),
    })
}

#[tokio::test]
async fn http_active_run_idle_observer_requires_exact_session_owner_release() {
    let runs = Mutex::new(BTreeMap::from([
        ("first-run".to_owned(), active_run("first-session")),
        ("second-run".to_owned(), active_run("second-session")),
    ]));
    let ready = HttpActiveRunsReady::default();
    let wait = ready.wait_for_session_idle(&runs, "first-session");
    tokio::pin!(wait);
    assert!(
        poll_fn(|context| Poll::Ready(wait.as_mut().poll(context)))
            .await
            .is_pending()
    );

    runs.lock()
        .expect("run map should lock")
        .remove("second-run");
    ready.notify_all();
    assert!(
        poll_fn(|context| Poll::Ready(wait.as_mut().poll(context)))
            .await
            .is_pending(),
        "another session's release cannot make the observed session idle"
    );

    runs.lock()
        .expect("run map should lock")
        .remove("first-run");
    ready.notify_all();
    assert!(matches!(
        poll_fn(|context| Poll::Ready(wait.as_mut().poll(context))).await,
        Poll::Ready(Ok(()))
    ));
}

#[test]
fn http_active_run_idle_observer_does_not_block_runtime_shutdown_or_release_the_run() {
    let runs = Arc::new(Mutex::new(BTreeMap::from([(
        "retained-run".to_owned(),
        active_run("retained-session"),
    )])));
    let observed_runs = Arc::clone(&runs);
    let (finished_sender, finished_receiver) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("observer runtime should start");
        runtime.block_on(async move {
            let ready = HttpActiveRunsReady::default();
            let (started_sender, started_receiver) = tokio::sync::oneshot::channel();
            tokio::spawn(async move {
                let wait = ready.wait_for_session_idle(&observed_runs, "retained-session");
                tokio::pin!(wait);
                let mut started_sender = Some(started_sender);
                poll_fn(|context| {
                    let result = wait.as_mut().poll(context);
                    if result.is_pending()
                        && let Some(sender) = started_sender.take()
                    {
                        let _ = sender.send(());
                    }
                    result
                })
                .await
                .expect("observer should either stay pending or observe a real release");
            });
            started_receiver
                .await
                .expect("observer should actually await the retained run");
        });
        drop(runtime);
        finished_sender
            .send(())
            .expect("test should still own the completion receiver");
    });

    finished_receiver
        .recv_timeout(Duration::from_secs(2))
        .expect("runtime shutdown must cancel an idle observer without a blocking worker");
    worker
        .join()
        .expect("observer runtime thread should finish");
    assert!(
        runs.lock()
            .expect("run map should remain accessible")
            .contains_key("retained-run"),
        "cancelling the observer must not forge actual run release"
    );
}
