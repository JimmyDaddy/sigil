use anyhow::Result;

use super::*;
use crate::ProviderWireStateV1;

fn transport_evidence() -> ProviderTurnRecoveryEvidenceV1 {
    ProviderTurnRecoveryEvidenceV1 {
        logical_run_id: "logical-turn-1".to_owned(),
        failed_physical_attempt_id: "attempt-1".to_owned(),
        request_material_fingerprint: "hmac-sha256:material".to_owned(),
        request_envelope_digest: format!("sha256:{}", "a".repeat(64)),
        source_frontier: Some(ProviderRequestSourceFrontierV1 {
            session_id: "session-recovery".to_owned(),
            durable_end_offset: 10,
            stream_sequence: Some(2),
            event_id: Some("event-terminal-1".to_owned()),
            record_checksum: Some(format!("sha256:jcs-v1:{}", "b".repeat(64))),
        }),
        failure: ProviderFailureObservationV1::transport_interrupted(
            ProviderWireStateV1::RequestBytesMayHaveBeenSent,
        ),
        output_state: ProviderOutputStateV1::None,
        local_tool_effect_state: EffectSettlementStateV1::None,
        hosted_effect_state: EffectSettlementStateV1::None,
        request_reconstruction:
            ProviderRequestReconstructionDispositionV1::DurableFrontierAndRuntimeInputs,
        request_material_availability:
            ProviderTurnRequestMaterialAvailabilityV1::DurableFrontierAndRuntimeInputs,
        partial_output_has_tool_calls: false,
    }
}

fn direct_event(
    event_type: DurableEventType,
    event_id: &str,
    sequence: u64,
    payload: serde_json::Value,
) -> StoredEvent {
    let mut event = StoredEvent::new(
        event_type,
        event_type
            .expected_event_class()
            .expect("recovery event has an event class"),
        event_id.to_owned(),
        "session-recovery".to_owned(),
        sequence,
        payload,
    )
    .expect("recovery direct event should build");
    event.record_checksum = event
        .compute_record_checksum()
        .expect("recovery event checksum should compute");
    event
}

fn schedule() -> ProviderTurnRecoveryScheduledEntry {
    ProviderTurnRecoveryScheduledEntry {
        schema_version: PROVIDER_TURN_RECOVERY_SCHEMA_VERSION,
        recovery_id: "recovery-1".to_owned(),
        logical_run_id: "logical-turn-1".to_owned(),
        failed_physical_attempt_id: "attempt-1".to_owned(),
        next_physical_attempt_ordinal: 2,
        request_envelope_digest: format!("sha256:{}", "a".repeat(64)),
        source_frontier: transport_evidence().source_frontier,
        failure_class: ProviderFailureClassV1::TransportInterrupted,
        retry_kind: ProviderTurnRecoveryRetryKindV1::Transport,
        not_before_unix_ms: 100,
        retry_after_ms: 50,
        budget_snapshot: RecoveryBudgetProjectionV1 {
            retry_count: 1,
            max_transport_retries: 2,
            partial_output_retry_count: 0,
            max_partial_output_retries: 1,
            cumulative_delay_ms: 50,
            max_cumulative_delay_ms: 120_000,
        },
        recovery_policy_fingerprint: ProviderTurnRecoveryPolicyV1::default().fingerprint(),
    }
}

#[test]
fn recovery_policy_requires_zero_effect_durable_reconstruction() {
    let policy = ProviderTurnRecoveryPolicyV1 {
        jitter_ratio_millionths: 0,
        ..ProviderTurnRecoveryPolicyV1::default()
    };
    assert!(matches!(
        policy.decide(
            &transport_evidence(),
            RecoveryBudgetProjectionV1::default(),
            false
        ),
        RecoveryDispositionV1::RetryProviderTurn {
            retry_after_ms: 500
        }
    ));

    let mut not_reconstructable = transport_evidence();
    not_reconstructable.request_reconstruction =
        ProviderRequestReconstructionDispositionV1::ProcessLocalOverlayRequired;
    not_reconstructable.request_material_availability =
        ProviderTurnRequestMaterialAvailabilityV1::Unavailable;
    assert_eq!(
        policy.decide(
            &not_reconstructable,
            RecoveryBudgetProjectionV1::default(),
            false,
        ),
        RecoveryDispositionV1::Block {
            reason_code: "recovery_material_unavailable"
        }
    );

    let mut exact_process_local = not_reconstructable;
    exact_process_local.request_material_availability =
        ProviderTurnRequestMaterialAvailabilityV1::ExactFrozenInCurrentProcess;
    assert!(matches!(
        policy.decide(
            &exact_process_local,
            RecoveryBudgetProjectionV1::default(),
            false,
        ),
        RecoveryDispositionV1::RetryProviderTurn { .. }
    ));

    let mut settled_output = transport_evidence();
    settled_output.output_state = ProviderOutputStateV1::DurableSurfaceCommitted;
    assert_eq!(
        policy.decide(
            &settled_output,
            RecoveryBudgetProjectionV1::default(),
            false,
        ),
        RecoveryDispositionV1::Block {
            reason_code: "provider_output_or_effect_committed"
        }
    );
}

#[test]
fn recovery_policy_honors_typed_retry_after_with_hard_caps() {
    let policy = ProviderTurnRecoveryPolicyV1::default();
    let mut evidence = transport_evidence();
    evidence.failure = ProviderFailureObservationV1 {
        class: ProviderFailureClassV1::RateLimited,
        retry_after_ms: Some(60_000),
        wire_state: ProviderWireStateV1::NoBytesSent,
        provider_retry_hint: crate::ProviderRetryHintV1::RetryAfterMs(60_000),
        safe_diagnostic_code: "provider_rate_limited".to_owned(),
    };
    assert_eq!(
        policy.decide(&evidence, RecoveryBudgetProjectionV1::default(), false),
        RecoveryDispositionV1::RetryProviderTurn {
            retry_after_ms: DEFAULT_PROVIDER_TURN_MAX_DELAY_MS
        }
    );
    assert_eq!(
        policy.decide(
            &evidence,
            RecoveryBudgetProjectionV1 {
                retry_count: 2,
                ..RecoveryBudgetProjectionV1::default()
            },
            false,
        ),
        RecoveryDispositionV1::Pause {
            reason_code: "provider_retry_budget_exhausted"
        }
    );
}

#[test]
fn recovery_policy_jitter_is_bounded_deterministic_and_fingerprinted() {
    let policy = ProviderTurnRecoveryPolicyV1::default();
    assert!(policy.fingerprint().contains(":100000:"));

    let first = policy.decide(
        &transport_evidence(),
        RecoveryBudgetProjectionV1::default(),
        false,
    );
    let second = policy.decide(
        &transport_evidence(),
        RecoveryBudgetProjectionV1::default(),
        false,
    );
    assert_eq!(
        first, second,
        "durable recovery must not depend on process entropy"
    );
    assert!(matches!(
        first,
        RecoveryDispositionV1::RetryProviderTurn {
            retry_after_ms: 450..=550
        }
    ));
}

#[test]
fn recovery_policy_allows_one_zero_effect_partial_stream_replacement() {
    let policy = ProviderTurnRecoveryPolicyV1::default();
    let mut evidence = transport_evidence();
    evidence.failure = ProviderFailureObservationV1::classified(
        ProviderFailureClassV1::StreamEndedUnexpectedly,
        ProviderWireStateV1::ResponseStarted,
        "provider_stream_ended_unexpectedly",
    );
    assert!(matches!(
        policy.decide(&evidence, RecoveryBudgetProjectionV1::default(), false),
        RecoveryDispositionV1::RetryProviderTurn { .. }
    ));
    assert_eq!(
        policy.decide(
            &evidence,
            RecoveryBudgetProjectionV1 {
                partial_output_retry_count: 1,
                ..RecoveryBudgetProjectionV1::default()
            },
            false,
        ),
        RecoveryDispositionV1::Pause {
            reason_code: "provider_partial_output_retry_budget_exhausted"
        }
    );
    evidence.partial_output_has_tool_calls = true;
    assert_eq!(
        policy.decide(&evidence, RecoveryBudgetProjectionV1::default(), false),
        RecoveryDispositionV1::Block {
            reason_code: "partial_provider_tool_request_requires_review"
        }
    );
}

#[test]
fn recovery_policy_fault_matrix_is_explicit_for_every_typed_failure_family() {
    let policy = ProviderTurnRecoveryPolicyV1::default();
    for class in [
        ProviderFailureClassV1::RejectedBeforeDispatch,
        ProviderFailureClassV1::RateLimited,
        ProviderFailureClassV1::TransientServer,
        ProviderFailureClassV1::TransportInterrupted,
        ProviderFailureClassV1::StreamEndedUnexpectedly,
    ] {
        let mut evidence = transport_evidence();
        evidence.failure = ProviderFailureObservationV1::classified(
            class,
            ProviderWireStateV1::RequestBytesMayHaveBeenSent,
            "deterministic_retryable_fault",
        );
        assert!(
            matches!(
                policy.decide(&evidence, RecoveryBudgetProjectionV1::default(), false),
                RecoveryDispositionV1::RetryProviderTurn { .. }
            ),
            "{class:?} must take the bounded retry path at a zero-effect frontier"
        );
    }
    for class in [
        ProviderFailureClassV1::Authentication,
        ProviderFailureClassV1::BillingOrQuota,
        ProviderFailureClassV1::RouteUnavailable,
        ProviderFailureClassV1::ContextCapacity,
    ] {
        let mut evidence = transport_evidence();
        evidence.failure = ProviderFailureObservationV1::classified(
            class,
            ProviderWireStateV1::NoBytesSent,
            "deterministic_configuration_fault",
        );
        assert_eq!(
            policy.decide(&evidence, RecoveryBudgetProjectionV1::default(), false),
            RecoveryDispositionV1::Block {
                reason_code: "provider_configuration_or_capacity_required"
            },
            "{class:?} must ask for configuration or capacity repair"
        );
    }
    for class in [
        ProviderFailureClassV1::ProtocolViolation,
        ProviderFailureClassV1::PermanentRequest,
    ] {
        let mut evidence = transport_evidence();
        evidence.failure = ProviderFailureObservationV1::classified(
            class,
            ProviderWireStateV1::NoBytesSent,
            "deterministic_request_fault",
        );
        assert_eq!(
            policy.decide(&evidence, RecoveryBudgetProjectionV1::default(), false),
            RecoveryDispositionV1::Block {
                reason_code: "provider_request_requires_attention"
            },
            "{class:?} must not be retried blindly"
        );
    }
    let mut cancelled = transport_evidence();
    cancelled.failure = ProviderFailureObservationV1::classified(
        ProviderFailureClassV1::Cancelled,
        ProviderWireStateV1::NoBytesSent,
        "deterministic_cancelled",
    );
    assert_eq!(
        policy.decide(&cancelled, RecoveryBudgetProjectionV1::default(), false),
        RecoveryDispositionV1::Cancelled
    );
}

#[test]
fn recovery_projection_claims_only_one_unstarted_due_schedule() -> Result<()> {
    let schedule = schedule();
    let scheduled = direct_event(
        DurableEventType::ProviderTurnRecoveryScheduled,
        "event-scheduled-1",
        1,
        serde_json::to_value(&schedule)?,
    );
    let projection = ProviderTurnRecoveryProjection::from_records(&[SessionStreamRecord::Stored(
        scheduled.clone(),
    )])?;
    assert!(projection.claimable_schedules_at(99).is_empty());
    assert_eq!(projection.claimable_schedules_at(100).len(), 1);

    let started = ProviderTurnRecoveryStartedEntry {
        schema_version: PROVIDER_TURN_RECOVERY_SCHEMA_VERSION,
        recovery_id: schedule.recovery_id.clone(),
        logical_run_id: schedule.logical_run_id.clone(),
        physical_attempt_id: "attempt-2".to_owned(),
        started_at_unix_ms: 101,
    };
    let started = direct_event(
        DurableEventType::ProviderTurnRecoveryStarted,
        "event-started-1",
        2,
        serde_json::to_value(started)?,
    );
    let projection = ProviderTurnRecoveryProjection::from_records(&[
        SessionStreamRecord::Stored(scheduled),
        SessionStreamRecord::Stored(started),
    ])?;
    assert!(projection.claimable_schedules_at(1_000).is_empty());
    Ok(())
}

#[test]
fn recovery_projection_rejects_duplicate_logical_terminal() -> Result<()> {
    let terminal = ProviderTurnRecoveryExhaustedEntry {
        schema_version: PROVIDER_TURN_RECOVERY_SCHEMA_VERSION,
        logical_run_id: "logical-turn-1".to_owned(),
        last_physical_attempt_id: "attempt-1".to_owned(),
        reason_code: "provider_retry_budget_exhausted".to_owned(),
        budget_snapshot: RecoveryBudgetProjectionV1::default(),
        terminal_disposition: ProviderTurnRecoveryTerminalDispositionV1::Paused,
    };
    let error = ProviderTurnRecoveryProjection::from_records(&[
        SessionStreamRecord::Stored(direct_event(
            DurableEventType::ProviderTurnRecoveryExhausted,
            "event-exhausted-1",
            1,
            serde_json::to_value(&terminal).expect("terminal serializes"),
        )),
        SessionStreamRecord::Stored(direct_event(
            DurableEventType::ProviderTurnRecoveryExhausted,
            "event-exhausted-2",
            2,
            serde_json::to_value(terminal).expect("terminal serializes"),
        )),
    ])
    .expect_err("a logical provider turn has one recovery terminal owner");
    assert!(error.to_string().contains("terminal more than once"));
    Ok(())
}
