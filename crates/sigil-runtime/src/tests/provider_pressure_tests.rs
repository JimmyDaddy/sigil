use super::*;

struct ManualProviderPressureClock {
    now: Mutex<Instant>,
}

impl ManualProviderPressureClock {
    fn new(now: Instant) -> Self {
        Self {
            now: Mutex::new(now),
        }
    }

    fn advance(&self, duration: Duration) {
        let mut now = self.now.lock().expect("manual clock lock");
        *now = now.checked_add(duration).expect("manual clock advance");
    }
}

impl ProviderPressureClock for ManualProviderPressureClock {
    fn now(&self) -> Instant {
        *self.now.lock().expect("manual clock lock")
    }
}

fn pressure_with_clock(clock: Arc<ManualProviderPressureClock>) -> TaskProviderPressure {
    TaskProviderPressure {
        state: Arc::new(Mutex::new(ProviderPressureState::default())),
        clock,
    }
}

#[test]
fn route_fingerprint_is_stable_and_scoped_by_provider_and_model() {
    let first = provider_route_fingerprint("deepseek", "deepseek-v4-flash");

    assert_eq!(
        first,
        provider_route_fingerprint(" deepseek ", " deepseek-v4-flash ")
    );
    assert_ne!(
        first,
        provider_route_fingerprint("anthropic", "deepseek-v4-flash")
    );
    assert_ne!(
        first,
        provider_route_fingerprint("deepseek", "deepseek-v4-pro")
    );
    assert!(first.starts_with("sha256:"));
}

#[test]
fn retry_after_cooldown_blocks_then_releases_the_same_route() -> Result<()> {
    let clock = Arc::new(ManualProviderPressureClock::new(Instant::now()));
    let pressure = pressure_with_clock(Arc::clone(&clock));
    let admission = pressure.admit("deepseek", "deepseek-v4-flash")?;

    pressure.record_rate_limit(&admission, Some(2_000));

    let error = pressure
        .check("deepseek", "deepseek-v4-flash")
        .expect_err("route should cool down");
    let cooldown = error
        .downcast_ref::<ProviderRouteCooldownError>()
        .expect("typed cooldown");
    assert_eq!(cooldown.retry_after_ms(), 2_000);
    assert_eq!(cooldown.route_fingerprint(), admission.fingerprint);
    pressure.check("deepseek", "deepseek-v4-pro")?;

    clock.advance(Duration::from_millis(2_000));
    pressure.check("deepseek", "deepseek-v4-flash")?;
    Ok(())
}

#[test]
fn stale_in_flight_success_does_not_clear_a_newer_rate_limit() -> Result<()> {
    let clock = Arc::new(ManualProviderPressureClock::new(Instant::now()));
    let pressure = pressure_with_clock(clock);
    let first = pressure.admit("deepseek", "deepseek-v4-flash")?;
    let sibling = pressure.admit("deepseek", "deepseek-v4-flash")?;

    pressure.record_rate_limit(&first, Some(1_000));
    pressure.record_success(&sibling);

    assert!(pressure.check("deepseek", "deepseek-v4-flash").is_err());
    Ok(())
}

#[test]
fn fallback_cooldown_is_deterministic_bounded_and_increases() {
    let route = provider_route_fingerprint("deepseek", "deepseek-v4-flash");
    let first = fallback_cooldown(&route, 1);
    let second = fallback_cooldown(&route, 2);

    assert_eq!(first, fallback_cooldown(&route, 1));
    assert!(first >= DEFAULT_RATE_LIMIT_COOLDOWN);
    assert!(second > first);
    assert!(
        fallback_cooldown(&route, u32::MAX) <= MAX_FALLBACK_COOLDOWN + Duration::from_millis(250)
    );
    assert_eq!(
        bounded_cooldown(Some(u64::MAX), &route, 1),
        MAX_RATE_LIMIT_COOLDOWN
    );
}

#[test]
fn retry_schedule_delay_uses_attempt_derived_bounded_jitter() -> Result<()> {
    let clock = Arc::new(ManualProviderPressureClock::new(Instant::now()));
    let pressure = pressure_with_clock(clock);
    let admission = pressure.admit("deepseek", "deepseek-v4-flash")?;
    pressure.record_rate_limit(&admission, Some(1_000));
    let first_attempt = TaskParticipantAttemptId::new("attempt-first")?;
    let second_attempt = TaskParticipantAttemptId::new("attempt-second")?;

    let first = pressure
        .retry_schedule_delay("deepseek", "deepseek-v4-flash", &first_attempt)
        .expect("cooling route has retry delay");
    let repeated = pressure
        .retry_schedule_delay("deepseek", "deepseek-v4-flash", &first_attempt)
        .expect("same attempt has retry delay");
    let second = pressure
        .retry_schedule_delay("deepseek", "deepseek-v4-flash", &second_attempt)
        .expect("second attempt has retry delay");

    assert_eq!(first, repeated);
    assert_eq!(first.1, admission.fingerprint);
    assert_ne!(
        retry_attempt_jitter_ms(&first.1, first_attempt.as_str()),
        retry_attempt_jitter_ms(&first.1, second_attempt.as_str())
    );
    assert!((1_000..=1_250).contains(&first.0));
    assert!((1_000..=1_250).contains(&second.0));
    Ok(())
}
