use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use super::*;

fn limits() -> WebTaskTreeBudgetLimits {
    WebTaskTreeBudgetLimits {
        max_fetch_calls: 3,
        max_client_search_calls: 3,
        max_hosted_requests: 2,
        max_network_attempts: 4,
        max_wire_bytes: 10,
        max_decoded_bytes: 20,
        max_model_bytes: 30,
        max_concurrent_requests: 2,
        max_attempts_per_host: 3,
    }
}

fn request(correlation: &str) -> WebBudgetReservationRequest {
    WebBudgetReservationRequest {
        correlation_id: correlation.to_owned(),
        attempt_id: format!("attempt-{correlation}"),
        route_lease_id: format!("lease-{correlation}"),
        route_fingerprint: "route-fingerprint".to_owned(),
        kind: WebBudgetReservationKind::ClientSearchCall,
    }
}

fn fetch_request(correlation: &str) -> WebBudgetReservationRequest {
    WebBudgetReservationRequest {
        kind: WebBudgetReservationKind::FetchCall,
        ..request(correlation)
    }
}

#[test]
fn provisional_reservation_refunds_before_wire_and_committed_counts_never_refund() {
    let budget = WebTaskTreeBudget::new("root-run", limits(), None).expect("budget");
    budget
        .reserve(request("pre-wire"))
        .expect("reservation")
        .refund_pre_wire()
        .expect("refund");
    assert_eq!(
        budget
            .snapshot()
            .expect("snapshot")
            .provisional_reservations,
        0
    );

    let mut committed = budget.reserve(request("committed")).expect("reservation");
    committed.commit_call().expect("commit call");
    committed
        .commit_attempt("attempt-committed", "example.com")
        .expect("commit attempt");
    assert!(committed.refund_pre_wire().is_err());
    let snapshot = budget.snapshot().expect("snapshot");
    assert_eq!(snapshot.logical_calls, 1);
    assert_eq!(snapshot.fetch_calls, 0);
    assert_eq!(snapshot.client_search_calls, 1);
    assert_eq!(snapshot.network_attempts, 1);
}

#[test]
fn redirect_attempts_and_chunk_bytes_charge_exactly_once() {
    let budget = WebTaskTreeBudget::new("root-run", limits(), None).expect("budget");
    let mut reservation = budget.reserve(request("query")).expect("reservation");
    reservation.commit_call().expect("logical call");
    reservation
        .commit_attempt("attempt-query", "example.com")
        .expect("initial attempt");
    reservation
        .commit_attempt("attempt-redirect", "example.com")
        .expect("redirect attempt");
    assert!(
        reservation
            .commit_attempt("attempt-redirect", "example.com")
            .is_err(),
        "the same redirect attempt must not be double charged"
    );
    reservation
        .charge_chunk(WebBudgetByteKind::Wire, 4)
        .expect("wire chunk");
    reservation
        .charge_chunk(WebBudgetByteKind::Wire, 6)
        .expect("wire boundary");
    reservation
        .charge_chunk(WebBudgetByteKind::Decoded, 7)
        .expect("decoded chunk");
    reservation
        .charge_chunk(WebBudgetByteKind::Model, 9)
        .expect("model chunk");
    let snapshot = budget.snapshot().expect("snapshot");
    assert_eq!(snapshot.network_attempts, 2);
    assert_eq!(snapshot.attempts_per_host.get("example.com"), Some(&2));
    assert_eq!(snapshot.wire_bytes, 10);
    assert_eq!(snapshot.decoded_bytes, 7);
    assert_eq!(snapshot.model_bytes, 9);
}

#[test]
fn byte_exhaustion_fires_root_cancellation_hook_once() {
    let cancellations = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&cancellations);
    let budget = WebTaskTreeBudget::new(
        "root-run",
        limits(),
        Some(Arc::new(move || {
            observed.fetch_add(1, Ordering::SeqCst);
        })),
    )
    .expect("budget");
    let mut reservation = budget.reserve(request("query")).expect("reservation");
    reservation
        .charge_chunk(WebBudgetByteKind::Wire, 10)
        .expect("at cap");
    assert!(matches!(
        reservation.charge_chunk(WebBudgetByteKind::Wire, 1),
        Err(WebBudgetError::Exhausted {
            dimension: "wire_bytes"
        })
    ));
    assert!(
        reservation
            .charge_chunk(WebBudgetByteKind::Wire, 1)
            .is_err()
    );
    assert_eq!(cancellations.load(Ordering::SeqCst), 1);
    assert!(budget.snapshot().expect("snapshot").exhausted);
}

#[test]
fn concurrency_is_released_only_after_explicit_quiescence() {
    let cancellations = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&cancellations);
    let budget = WebTaskTreeBudget::new(
        "root-run",
        limits(),
        Some(Arc::new(move || {
            observed.fetch_add(1, Ordering::SeqCst);
        })),
    )
    .expect("budget");
    let permit = budget.acquire_concurrency().expect("permit");
    permit
        .release_after_quiescence()
        .expect("quiescent release");
    assert_eq!(
        budget
            .snapshot()
            .expect("snapshot")
            .active_concurrent_requests,
        0
    );

    let unsafe_drop = budget.acquire_concurrency().expect("permit");
    drop(unsafe_drop);
    let snapshot = budget.snapshot().expect("snapshot");
    assert_eq!(snapshot.active_concurrent_requests, 1);
    assert!(snapshot.cleanup_incomplete);
    assert_eq!(cancellations.load(Ordering::SeqCst), 1);
}

#[test]
fn budget_exhaustion_requests_cooperative_root_cancellation() {
    let owner = crate::RunCancellationOwner::new();
    let cancellation = owner.handle();
    let budget =
        WebTaskTreeBudget::new("root-run", limits(), Some(owner.budget_cancellation_hook()))
            .expect("budget");
    let mut reservation = budget.reserve(request("query")).expect("reservation");
    assert!(
        reservation
            .charge_chunk(WebBudgetByteKind::Wire, 11)
            .is_err()
    );
    assert!(cancellation.is_cancel_requested());
}

#[test]
fn fetch_and_client_search_limits_are_independent() {
    let mut separate_limits = limits();
    separate_limits.max_fetch_calls = 1;
    separate_limits.max_client_search_calls = 1;
    let budget = WebTaskTreeBudget::new("root-run", separate_limits, None).expect("budget");

    let mut fetch = budget
        .reserve(fetch_request("fetch-one"))
        .expect("first fetch reservation");
    fetch.commit_call().expect("first fetch commit");
    assert!(matches!(
        budget.reserve(fetch_request("fetch-two")),
        Err(WebBudgetError::Exhausted {
            dimension: "fetch_calls"
        })
    ));

    let separate_budget =
        WebTaskTreeBudget::new("separate-root", separate_limits, None).expect("budget");
    let mut fetch = separate_budget
        .reserve(fetch_request("fetch"))
        .expect("fetch reservation");
    fetch.commit_call().expect("fetch commit");
    let mut search = separate_budget
        .reserve(request("search"))
        .expect("search remains independently available");
    search.commit_call().expect("search commit");
    let snapshot = separate_budget.snapshot().expect("snapshot");
    assert_eq!(snapshot.logical_calls, 2);
    assert_eq!(snapshot.fetch_calls, 1);
    assert_eq!(snapshot.client_search_calls, 1);

    let search_first_budget =
        WebTaskTreeBudget::new("search-first-root", separate_limits, None).expect("budget");
    let mut search = search_first_budget
        .reserve(request("search-first"))
        .expect("search reservation");
    search.commit_call().expect("search commit");
    let mut fetch = search_first_budget
        .reserve(fetch_request("fetch-after-search"))
        .expect("fetch remains independently available");
    fetch.commit_call().expect("fetch commit");
    assert!(matches!(
        search_first_budget.reserve(request("search-second")),
        Err(WebBudgetError::Exhausted {
            dimension: "client_search_calls"
        })
    ));
}
