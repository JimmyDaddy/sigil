use std::sync::Arc;

use async_trait::async_trait;
use sigil_kernel::{
    EffectReconciliationProbe, EffectReconciliationProbeReceiptV1,
    EffectReconciliationProbeRequestV1, EffectReconciliationRequiredEntryV1, JsonlSessionStore,
    MutationEventRecorder, ReconciliationProbeKindV1,
};

use super::EffectReconciliationProbeRegistry;

struct WorkspaceProbe;

#[async_trait]
impl EffectReconciliationProbe for WorkspaceProbe {
    fn kind(&self) -> ReconciliationProbeKindV1 {
        ReconciliationProbeKindV1::WorkspaceObservation
    }

    fn validate(&self, request: &EffectReconciliationProbeRequestV1) -> anyhow::Result<()> {
        request.validate()
    }

    async fn observe(
        &self,
        request: EffectReconciliationProbeRequestV1,
    ) -> anyhow::Result<EffectReconciliationProbeReceiptV1> {
        Ok(EffectReconciliationProbeReceiptV1 {
            schema_version: sigil_kernel::EFFECT_RECONCILIATION_SCHEMA_VERSION,
            reconciliation_id: request.reconciliation_id,
            effect_id: request.effect_id,
            effect_digest: request.effect_digest,
            probe_id: request.probe_id,
            outcome: sigil_kernel::EffectReconciliationOutcomeV1::StillUncertain,
            receipt_digest: format!("sha256:{}", "a".repeat(64)),
            observed_at_unix_ms: 1,
        })
    }
}

#[test]
fn registry_rejects_duplicate_probe_owners() {
    let mut registry = EffectReconciliationProbeRegistry::default();
    registry
        .register(Arc::new(WorkspaceProbe))
        .expect("first probe owns the kind");
    assert!(registry.register(Arc::new(WorkspaceProbe)).is_err());
}

#[tokio::test]
async fn registered_probe_claims_and_settles_exact_uncertain_effect() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let recorder =
        MutationEventRecorder::new(JsonlSessionStore::new(temp.path().join("session.jsonl"))?);
    let required = EffectReconciliationRequiredEntryV1 {
        schema_version: sigil_kernel::EFFECT_RECONCILIATION_SCHEMA_VERSION,
        reconciliation_id: "reconciliation-1".to_owned(),
        effect_id: "effect-1".to_owned(),
        effect_digest: format!("sha256:{}", "a".repeat(64)),
        replay_contract_fingerprint: "tool-replay-v1:non_replayable_unknown_effect".to_owned(),
        reason_code: "workspace_effect_evidence_incomplete".to_owned(),
        requested_at_unix_ms: 1,
        logical_run_id: Some("run-1".to_owned()),
        task_id: None,
        step_id: None,
        participant_attempt_id: None,
        base_workspace_observation_id: Some("snapshot-1".to_owned()),
        current_workspace_observation_id: None,
        known_receipt_ids: Vec::new(),
        allowed_probe_kinds: vec![ReconciliationProbeKindV1::WorkspaceObservation],
        probe_budget_ms: 100,
    };
    assert!(recorder.append_effect_reconciliation_required(&required)?);
    let mut registry = EffectReconciliationProbeRegistry::default();
    registry.register(Arc::new(WorkspaceProbe))?;

    let result = super::run_effect_reconciliation_probe(
        &recorder,
        &registry,
        &required,
        ReconciliationProbeKindV1::WorkspaceObservation,
        2,
    )
    .await?;
    assert!(matches!(
        result,
        super::EffectReconciliationProbeRunOutcome::Settled(_)
    ));
    Ok(())
}
