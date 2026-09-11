use super::*;
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

#[test]
fn cancellation_before_claim_cannot_recreate_a_ticket_or_cancel_a_later_query() {
    let owner = DesktopHistoryQueryOwner::default();
    let old = owner.prepare("workspace", "session").expect("ticket");
    owner.cancel("workspace", "session", &old).expect("cancel");
    assert!(matches!(
        owner.claim("workspace", "session", &old),
        Err(HistoryQueryError::Cancelled)
    ));
    let next = owner.prepare("workspace", "session").expect("next ticket");
    assert_ne!(old, next);
    owner
        .cancel("workspace", "session", &old)
        .expect("idempotent old cancel");
    drop(
        owner
            .claim("workspace", "session", &next)
            .expect("new query remains valid"),
    );
    assert!(owner.0.lock().expect("owner").pending.is_empty());
}

#[test]
fn binding_mismatch_and_duplicate_claim_do_not_revoke_another_read() {
    let owner = DesktopHistoryQueryOwner::default();
    let id = owner.prepare("workspace", "session").expect("ticket");
    assert!(matches!(
        owner.claim("other", "session", &id),
        Err(HistoryQueryError::BindingMismatch)
    ));
    assert_eq!(
        owner.cancel("workspace", "other", &id),
        Err(HistoryQueryError::BindingMismatch)
    );
    let lease = owner
        .claim("workspace", "session", &id)
        .expect("matching claim");
    assert!(matches!(
        owner.claim("workspace", "session", &id),
        Err(HistoryQueryError::AlreadyClaimed)
    ));
    drop(lease);
    assert!(owner.0.lock().expect("owner").pending.is_empty());
}

#[test]
fn lost_prepare_replies_expire_lazily_without_expiring_claimed_cold_reads() {
    let owner = DesktopHistoryQueryOwner::default();
    let ids = (0..MAX_HISTORY_QUERIES)
        .map(|_| owner.prepare("workspace", "session").expect("ticket"))
        .collect::<Vec<_>>();
    let active = owner
        .claim("workspace", "session", &ids[0])
        .expect("active cold read");
    {
        let mut state = owner.0.lock().expect("owner");
        for ticket in state.pending.values_mut() {
            ticket.prepared_at = Instant::now() - UNUSED_HISTORY_QUERY_LIFETIME;
        }
    }
    let fresh = owner
        .prepare("workspace", "session")
        .expect("expired unused capacity is reclaimed");
    assert_eq!(owner.0.lock().expect("owner").pending.len(), 2);
    assert!(!*active.cancellation.borrow());
    assert!(matches!(
        owner.claim("workspace", "session", &ids[1]),
        Err(HistoryQueryError::Cancelled)
    ));
    owner
        .0
        .lock()
        .expect("owner")
        .pending
        .get_mut(&fresh)
        .expect("fresh ticket")
        .prepared_at = Instant::now() - UNUSED_HISTORY_QUERY_LIFETIME;
    assert!(matches!(
        owner.claim("workspace", "session", &fresh),
        Err(HistoryQueryError::Cancelled)
    ));
    drop(active);
    assert!(owner.0.lock().expect("owner").pending.is_empty());
}

#[tokio::test]
async fn cancellation_after_claim_before_poll_does_not_dispatch_http_work() {
    let owner = DesktopHistoryQueryOwner::default();
    let id = owner.prepare("workspace", "session").expect("ticket");
    let lease = owner.claim("workspace", "session", &id).expect("claim");
    owner.cancel("workspace", "session", &id).expect("cancel");
    let polled = AtomicBool::new(false);
    let result = lease
        .run(async {
            polled.store(true, Ordering::Release);
        })
        .await;
    assert_eq!(result, Err(HistoryQueryError::Cancelled));
    assert!(!polled.load(Ordering::Acquire));
    assert!(owner.0.lock().expect("owner").pending.is_empty());
}

#[tokio::test]
async fn in_flight_cancellation_drops_the_actual_observation_before_releasing_capacity() {
    struct Dropped(Arc<AtomicBool>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }
    let owner = DesktopHistoryQueryOwner::default();
    let id = owner.prepare("workspace", "session").expect("ticket");
    let lease = owner.claim("workspace", "session", &id).expect("claim");
    let dropped = Arc::new(AtomicBool::new(false));
    let witness = Dropped(Arc::clone(&dropped));
    let (started, observed) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(lease.run(async move {
        let _witness = witness;
        let _ = started.send(());
        std::future::pending::<()>().await;
    }));
    tokio::time::timeout(Duration::from_secs(3), observed)
        .await
        .expect("dispatch deadline")
        .expect("real future entered");
    owner.cancel("workspace", "session", &id).expect("cancel");
    assert_eq!(owner.0.lock().expect("owner").pending.len(), 1);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .expect("cancellation deadline")
            .expect("joined"),
        Err(HistoryQueryError::Cancelled)
    );
    assert!(dropped.load(Ordering::Acquire));
    assert!(owner.0.lock().expect("owner").pending.is_empty());
}

#[tokio::test]
async fn capacity_validation_failure_workspace_close_and_exit_reclaim_tickets() {
    let owner = DesktopHistoryQueryOwner::default();
    let ids = (0..MAX_HISTORY_QUERIES)
        .map(|_| owner.prepare("workspace", "session").expect("ticket"))
        .collect::<Vec<_>>();
    assert_eq!(
        owner.prepare("workspace", "session"),
        Err(HistoryQueryError::Capacity)
    );
    owner
        .cancel("workspace", "session", &ids[0])
        .expect("unstarted cancellation");
    let other = owner
        .prepare("other", "session")
        .expect("reclaimed capacity");
    let lease = owner.claim("workspace", "session", &ids[1]).expect("claim");
    assert_eq!(
        lease.run(async { Err::<(), _>("invalid page") }).await,
        Ok(Err("invalid page"))
    );
    owner
        .cancel_workspace("workspace")
        .expect("workspace close");
    assert_eq!(owner.0.lock().expect("owner").pending.len(), 1);
    let active = owner
        .claim("other", "session", &other)
        .expect("other workspace remains");
    owner.stop();
    assert_eq!(
        owner.prepare("other", "session"),
        Err(HistoryQueryError::Cancelled)
    );
    assert_eq!(
        active.run(std::future::pending::<()>()).await,
        Err(HistoryQueryError::Cancelled)
    );
    assert!(owner.0.lock().expect("owner").pending.is_empty());
}
