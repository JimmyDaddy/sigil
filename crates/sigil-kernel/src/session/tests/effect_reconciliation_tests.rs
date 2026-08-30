use super::*;

fn stored(
    event_type: DurableEventType,
    sequence: u64,
    payload: serde_json::Value,
) -> Result<StoredEvent> {
    let mut event = StoredEvent::new(
        event_type,
        EventClass::Critical,
        format!("effect-event-{sequence}"),
        "session-effect-recovery".to_owned(),
        sequence,
        payload,
    )?;
    event.record_checksum = event.compute_record_checksum()?;
    Ok(event)
}

fn required() -> EffectReconciliationRequiredEntryV1 {
    EffectReconciliationRequiredEntryV1 {
        schema_version: EFFECT_RECONCILIATION_SCHEMA_VERSION,
        reconciliation_id: "effect-recovery-1".to_owned(),
        effect_id: "tool-call-1".to_owned(),
        effect_digest: format!("sha256:{}", "a".repeat(64)),
        replay_contract_fingerprint: "tool-replay-v1:prepared_workspace_mutation_v1".to_owned(),
        reason_code: "tool_outcome_uncertain".to_owned(),
        requested_at_unix_ms: 1,
        logical_run_id: None,
        task_id: None,
        step_id: None,
        participant_attempt_id: None,
        base_workspace_observation_id: None,
        current_workspace_observation_id: None,
        known_receipt_ids: Vec::new(),
        allowed_probe_kinds: Vec::new(),
        probe_budget_ms: 0,
    }
}

#[test]
fn reconciliation_projection_pairs_one_exact_terminal() {
    let mut projection = EffectReconciliationProjectionV1::default();
    projection
        .apply_required(required())
        .expect("required is accepted");
    let terminal = EffectReconciliationTerminalEntryV1 {
        schema_version: EFFECT_RECONCILIATION_SCHEMA_VERSION,
        reconciliation_id: "effect-recovery-1".to_owned(),
        effect_id: "tool-call-1".to_owned(),
        effect_digest: format!("sha256:{}", "a".repeat(64)),
        outcome: EffectReconciliationOutcomeV1::ObservedApplied,
        probe_receipt_digest: Some(format!("sha256:{}", "b".repeat(64))),
        probe_id: None,
    };
    projection
        .apply_terminal(terminal.clone())
        .expect("terminal is accepted");
    assert_eq!(projection.terminal("effect-recovery-1"), Some(&terminal));
}

#[test]
fn reconciliation_projection_rejects_mismatched_or_duplicate_terminal() {
    let mut projection = EffectReconciliationProjectionV1::default();
    projection
        .apply_required(required())
        .expect("required is accepted");
    let mut terminal = EffectReconciliationTerminalEntryV1 {
        schema_version: EFFECT_RECONCILIATION_SCHEMA_VERSION,
        reconciliation_id: "effect-recovery-1".to_owned(),
        effect_id: "other-tool".to_owned(),
        effect_digest: format!("sha256:{}", "a".repeat(64)),
        outcome: EffectReconciliationOutcomeV1::StillUncertain,
        probe_receipt_digest: None,
        probe_id: None,
    };
    assert!(projection.apply_terminal(terminal.clone()).is_err());
    terminal.effect_id = "tool-call-1".to_owned();
    projection
        .apply_terminal(terminal.clone())
        .expect("matching terminal is accepted");
    assert!(projection.apply_terminal(terminal).is_err());
}

#[test]
fn durable_projection_rebuilds_active_fence_and_matching_terminal() -> Result<()> {
    let required = required();
    let terminal = EffectReconciliationTerminalEntryV1 {
        schema_version: EFFECT_RECONCILIATION_SCHEMA_VERSION,
        reconciliation_id: required.reconciliation_id.clone(),
        effect_id: required.effect_id.clone(),
        effect_digest: required.effect_digest.clone(),
        outcome: EffectReconciliationOutcomeV1::ObservedNotApplied,
        probe_receipt_digest: Some(format!("sha256:{}", "c".repeat(64))),
        probe_id: None,
    };
    let active =
        EffectReconciliationProjectionV1::from_records(&[SessionStreamRecord::Stored(stored(
            DurableEventType::EffectReconciliationRequired,
            1,
            serde_json::to_value(&required)?,
        )?)])?;
    assert_eq!(active.active(), vec![&required]);
    let settled = EffectReconciliationProjectionV1::from_records(&[
        SessionStreamRecord::Stored(stored(
            DurableEventType::EffectReconciliationRequired,
            1,
            serde_json::to_value(&required)?,
        )?),
        SessionStreamRecord::Stored(stored(
            DurableEventType::EffectReconciliationTerminal,
            2,
            serde_json::to_value(&terminal)?,
        )?),
    ])?;
    assert!(settled.active().is_empty());
    assert_eq!(
        settled.terminal(&required.reconciliation_id),
        Some(&terminal)
    );
    Ok(())
}

#[test]
fn reconciliation_probe_is_single_claim_and_terminal_binds_its_exact_receipt() -> Result<()> {
    let mut required = required();
    required.allowed_probe_kinds = vec![ReconciliationProbeKindV1::WorkspaceObservation];
    required.probe_budget_ms = 500;
    let started = EffectReconciliationProbeStartedEntryV1 {
        schema_version: EFFECT_RECONCILIATION_SCHEMA_VERSION,
        reconciliation_id: required.reconciliation_id.clone(),
        effect_id: required.effect_id.clone(),
        effect_digest: required.effect_digest.clone(),
        probe_id: "probe-1".to_owned(),
        probe_kind: ReconciliationProbeKindV1::WorkspaceObservation,
        claimed_at_unix_ms: 2,
    };
    let mut projection = EffectReconciliationProjectionV1::default();
    projection.apply_required(required.clone())?;
    projection.apply_probe_started(started.clone())?;
    assert_eq!(
        projection.active_probe(&required.reconciliation_id),
        Some(&started)
    );
    assert!(projection.apply_probe_started(started.clone()).is_err());

    let terminal = EffectReconciliationTerminalEntryV1 {
        schema_version: EFFECT_RECONCILIATION_SCHEMA_VERSION,
        reconciliation_id: required.reconciliation_id.clone(),
        effect_id: required.effect_id.clone(),
        effect_digest: required.effect_digest.clone(),
        outcome: EffectReconciliationOutcomeV1::StillUncertain,
        probe_receipt_digest: Some(format!("sha256:{}", "d".repeat(64))),
        probe_id: Some(started.probe_id.clone()),
    };
    projection.apply_terminal(terminal)?;
    assert!(projection.active().is_empty());
    assert!(
        projection
            .active_probe(&required.reconciliation_id)
            .is_none()
    );
    Ok(())
}
