use std::time::{Duration, Instant};

#[test]
fn saturating_elapsed_returns_zero_for_future_instants() {
    let future = Instant::now() + Duration::from_secs(1);

    assert_eq!(super::saturating_elapsed(future), Duration::ZERO);
}

#[test]
fn saturating_elapsed_returns_elapsed_duration_for_past_instants() {
    let past = Instant::now() - Duration::from_millis(1);

    assert!(super::saturating_elapsed(past) >= Duration::from_millis(1));
}
