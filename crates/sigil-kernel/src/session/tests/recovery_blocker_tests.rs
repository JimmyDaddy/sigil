use anyhow::Result;

use super::*;
use crate::{
    DurableEventType, EffectSettlementV1, EventClass, FailureScopeV1, RecoverabilityV1,
    RecoveryActionV1, RecoveryBlockerRaisedV1, RecoveryBlockerResolutionStartedV1,
    RecoveryBlockerResolvedV1, RecoveryBlockerV1, RecoveryDomainV1,
};

fn event(
    event_type: DurableEventType,
    sequence: u64,
    payload: serde_json::Value,
) -> Result<StoredEvent> {
    let mut event = StoredEvent::new(
        event_type,
        EventClass::Critical,
        format!("event-{sequence}"),
        "session-recovery-blocker".to_owned(),
        sequence,
        payload,
    )?;
    event.record_checksum = event.compute_record_checksum()?;
    Ok(event)
}

fn blocker() -> RecoveryBlockerV1 {
    RecoveryBlockerV1 {
        schema_version: crate::RECOVERY_BLOCKER_SCHEMA_VERSION,
        blocker_id: "blocker-1".to_owned(),
        domain: RecoveryDomainV1::EffectReconciliation,
        scope: FailureScopeV1::ToolEffect {
            task_id: None,
            step_id: None,
            effect_id: "effect-1".to_owned(),
        },
        recoverability: RecoverabilityV1::ReconcileEffect,
        settlement: EffectSettlementV1::OutcomeUncertain,
        reason_code: "effect_outcome_uncertain".to_owned(),
        safe_summary: "The effect needs a read-only reconciliation probe.".to_owned(),
        evidence_digest: format!("sha256:{}", "a".repeat(64)),
        effect_id: Some("effect-1".to_owned()),
        available_actions: vec![RecoveryActionV1::ReconcileEffect, RecoveryActionV1::Cancel],
        created_at_ms: 10,
    }
}

#[test]
fn durable_recovery_projection_replays_raise_start_and_resolution() -> Result<()> {
    let raised = RecoveryBlockerRaisedV1 { blocker: blocker() };
    let start = RecoveryBlockerResolutionStartedV1 {
        blocker_id: "blocker-1".to_owned(),
        action: RecoveryActionV1::ReconcileEffect,
        attempt_id: "probe-1".to_owned(),
        started_at_ms: 11,
    };
    let resolved = RecoveryBlockerResolvedV1 {
        blocker_id: "blocker-1".to_owned(),
        resolution_receipt_digest: format!("sha256:{}", "b".repeat(64)),
        resolved_at_ms: 12,
    };
    let records = vec![
        SessionStreamRecord::Stored(event(
            DurableEventType::RecoveryBlockerRaised,
            1,
            serde_json::to_value(raised)?,
        )?),
        SessionStreamRecord::Stored(event(
            DurableEventType::RecoveryBlockerResolutionStarted,
            2,
            serde_json::to_value(start)?,
        )?),
        SessionStreamRecord::Stored(event(
            DurableEventType::RecoveryBlockerResolved,
            3,
            serde_json::to_value(resolved)?,
        )?),
    ];

    let projection = RecoveryBlockerProjectionV1::from_records(&records)?;
    assert!(projection.active().is_empty());
    assert!(projection.resolved("blocker-1").is_some());
    Ok(())
}

#[test]
fn durable_recovery_projection_rejects_a_terminal_without_resolution_claim() -> Result<()> {
    let records = vec![
        SessionStreamRecord::Stored(event(
            DurableEventType::RecoveryBlockerRaised,
            1,
            serde_json::to_value(RecoveryBlockerRaisedV1 { blocker: blocker() })?,
        )?),
        SessionStreamRecord::Stored(event(
            DurableEventType::RecoveryBlockerResolved,
            2,
            serde_json::to_value(RecoveryBlockerResolvedV1 {
                blocker_id: "blocker-1".to_owned(),
                resolution_receipt_digest: format!("sha256:{}", "b".repeat(64)),
                resolved_at_ms: 12,
            })?,
        )?),
    ];

    assert!(RecoveryBlockerProjectionV1::from_records(&records).is_err());
    Ok(())
}
