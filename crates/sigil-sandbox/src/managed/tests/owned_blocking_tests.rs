use super::*;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc,
};

#[tokio::test(flavor = "current_thread")]
async fn owned_blocking_work_keeps_executor_responsive_and_joins_actual_release() {
    let (started, start) = tokio::sync::oneshot::channel();
    let (release, blocked) = mpsc::channel();
    let released = Arc::new(AtomicBool::new(false));
    let completed = Arc::clone(&released);
    let mut work = OwnedBlockingWork::spawn("sigil-test-owned-release", move || {
        let _ = started.send(());
        blocked.recv().expect("explicit release");
        completed.store(true, Ordering::SeqCst);
        17
    })
    .expect("owned work");
    start.await.expect("work started");
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(10), &mut work)
            .await
            .is_err()
    );
    assert!(!released.load(Ordering::SeqCst));
    release.send(()).expect("release actual work");
    assert_eq!(work.await.expect("joined work"), 17);
    assert!(released.load(Ordering::SeqCst));
}

#[test]
fn owned_blocking_work_drop_keeps_native_join_until_real_completion() {
    let (started, start) = mpsc::channel();
    let (release, blocked) = mpsc::channel();
    let (dropped, drop_complete) = mpsc::channel();
    let work = OwnedBlockingWork::spawn("sigil-test-native-release", move || {
        started.send(()).expect("started");
        blocked.recv().expect("explicit release");
    })
    .expect("owned native work");
    start.recv().expect("real work started");
    let drop_owner = std::thread::spawn(move || {
        drop(work);
        dropped.send(()).expect("joined");
    });
    assert!(
        drop_complete
            .recv_timeout(std::time::Duration::from_millis(10))
            .is_err()
    );
    release.send(()).expect("release actual work");
    drop_complete
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("drop retained join");
    drop_owner.join().expect("test owner joined");
}

#[tokio::test(flavor = "current_thread")]
async fn owned_blocking_work_preserves_panicked_join_failure() {
    let work =
        OwnedBlockingWork::spawn("sigil-test-panic", || panic!("injected owned work failure"))
            .expect("owned work");
    assert!(matches!(work.await, Err(OwnedBlockingWorkError::Join)));
}
