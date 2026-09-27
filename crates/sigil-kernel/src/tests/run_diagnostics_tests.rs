use super::*;

#[test]
fn timings_are_bounded_and_do_not_export_raw_identity() -> anyhow::Result<()> {
    let buffer = RunTimings::new();
    for index in 0..MAX_RUN_TIMINGS + 7 {
        buffer.record(
            "internal-identity",
            RunTimingPhase::Preparation,
            Duration::from_micros(index as u64),
        );
    }
    let snapshot = buffer.snapshot();
    assert_eq!(snapshot.observations.len(), MAX_RUN_TIMINGS);
    assert_eq!(snapshot.dropped, 7);
    assert_eq!(snapshot.observations[0].elapsed_us, 7);
    assert_eq!(snapshot.observations[0].sequence, 8);
    assert!(snapshot.available);
    assert!(!serde_json::to_string(&snapshot)?.contains("internal-identity"));
    assert_eq!(
        snapshot.observations[0].run_key,
        run_timing_key("internal-identity")
    );
    Ok(())
}

#[test]
fn contention_and_invalid_identity_drop_diagnostics_without_blocking() -> anyhow::Result<()> {
    let buffer = RunTimings::new();
    let guard = buffer
        .observations
        .lock()
        .map_err(|_| anyhow::anyhow!("test lock poisoned"))?;
    buffer.record("run", RunTimingPhase::ProviderDispatch, Duration::ZERO);
    assert!(!buffer.snapshot().available);
    drop(guard);
    buffer.record(
        &"x".repeat(257),
        RunTimingPhase::Preparation,
        Duration::ZERO,
    );
    assert_eq!(buffer.snapshot().dropped, 2);
    assert!(buffer.snapshot().observations.is_empty());
    Ok(())
}

#[test]
fn collection_does_not_require_a_tracing_subscriber() {
    let buffer = RunTimings::new();
    tracing::subscriber::with_default(tracing::subscriber::NoSubscriber::default(), || {
        buffer.record(
            "run",
            RunTimingPhase::ProviderFirstContent,
            Duration::from_millis(5),
        );
    });
    let snapshot = buffer.snapshot();
    assert_eq!(snapshot.observations.len(), 1);
    assert_eq!(snapshot.observations[0].elapsed_us, 5_000);
}
