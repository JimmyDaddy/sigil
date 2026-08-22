//! Runtime executor for the durable, read-only effect reconciliation lifecycle.
//!
//! This module deliberately has no generic command runner.  A caller must register a typed probe
//! for every allowed observation kind, which prevents recovery from replaying an unknown effect.

use std::{collections::BTreeMap, sync::Arc};

use anyhow::{Result, anyhow, bail};
use sigil_kernel::{
    EFFECT_RECONCILIATION_SCHEMA_VERSION, EffectReconciliationProbe,
    EffectReconciliationProbeReceiptV1, EffectReconciliationProbeRequestV1,
    EffectReconciliationProbeStartedEntryV1, EffectReconciliationRequiredEntryV1,
    EffectReconciliationTerminalEntryV1, MutationEventRecorder, ReconciliationProbeKindV1,
};

/// Registered runtime probes indexed by their durable kind.
#[derive(Default)]
pub struct EffectReconciliationProbeRegistry {
    probes: BTreeMap<ReconciliationProbeKindV1, Arc<dyn EffectReconciliationProbe>>,
}

impl EffectReconciliationProbeRegistry {
    /// Registers the unique runtime owner for a probe kind.
    ///
    /// # Errors
    ///
    /// Returns an error if a second implementation attempts to own the same durable kind.
    pub fn register(&mut self, probe: Arc<dyn EffectReconciliationProbe>) -> Result<()> {
        let kind = probe.kind();
        if self.probes.insert(kind, probe).is_some() {
            bail!(
                "effect reconciliation probe kind {} is already registered",
                kind.as_str()
            );
        }
        Ok(())
    }

    /// Looks up the exact registered probe without exposing a fallback command surface.
    #[must_use]
    pub fn get(
        &self,
        kind: ReconciliationProbeKindV1,
    ) -> Option<&Arc<dyn EffectReconciliationProbe>> {
        self.probes.get(&kind)
    }
}

/// Result of one conditional probe attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectReconciliationProbeRunOutcome {
    /// The exact probe receipt was durably bound to the uncertain effect.
    Settled(EffectReconciliationTerminalEntryV1),
    /// The same durable claim already existed.  It is safe for a later recovery owner to resume
    /// the identical read-only probe, but it must not mint a different claim or replay the effect.
    ResumedExistingClaim { probe_id: String },
}

/// Runs a registered read-only probe and conditionally commits its matching terminal settlement.
///
/// `ObservedNotApplied` is only evidence that a future explicit recovery action may consider a
/// replay contract; this function never replays the original effect itself.  Probe failures leave
/// the original reconciliation active, so the Task remains blocked and auditable.
pub async fn run_effect_reconciliation_probe(
    recorder: &MutationEventRecorder,
    registry: &EffectReconciliationProbeRegistry,
    required: &EffectReconciliationRequiredEntryV1,
    probe_kind: ReconciliationProbeKindV1,
    now_unix_ms: u64,
) -> Result<EffectReconciliationProbeRunOutcome> {
    if !required.allowed_probe_kinds.is_empty()
        && !required.allowed_probe_kinds.contains(&probe_kind)
    {
        bail!("reconciliation probe kind is not authorized by the durable requirement");
    }
    let probe = registry.get(probe_kind).ok_or_else(|| {
        anyhow!(
            "no runtime reconciliation probe is registered for {}",
            probe_kind.as_str()
        )
    })?;
    let probe_id = sigil_kernel::stable_event_uuid(
        "sigil-effect-reconciliation-probe-v1",
        &format!(
            "{}\0{}\0{}",
            required.reconciliation_id,
            required.effect_digest,
            probe_kind.as_str()
        ),
    );
    let request = EffectReconciliationProbeRequestV1 {
        schema_version: EFFECT_RECONCILIATION_SCHEMA_VERSION,
        reconciliation_id: required.reconciliation_id.clone(),
        effect_id: required.effect_id.clone(),
        effect_digest: required.effect_digest.clone(),
        replay_contract_fingerprint: required.replay_contract_fingerprint.clone(),
        probe_id: probe_id.clone(),
        probe_kind,
        budget_ms: required.probe_budget_ms.max(1),
    };
    request.validate()?;
    probe.validate(&request)?;
    let started = EffectReconciliationProbeStartedEntryV1 {
        schema_version: EFFECT_RECONCILIATION_SCHEMA_VERSION,
        reconciliation_id: required.reconciliation_id.clone(),
        effect_id: required.effect_id.clone(),
        effect_digest: required.effect_digest.clone(),
        probe_id: probe_id.clone(),
        probe_kind,
        claimed_at_unix_ms: now_unix_ms,
    };
    let claimed_now = recorder.append_effect_reconciliation_probe_started(&started)?;
    let receipt = tokio::time::timeout(
        std::time::Duration::from_millis(request.budget_ms),
        probe.observe(request),
    )
    .await
    .map_err(|_| anyhow!("effect reconciliation probe exceeded its durable budget"))??;
    receipt.validate()?;
    validate_probe_receipt_matches(&receipt, &started)?;
    let terminal = EffectReconciliationTerminalEntryV1 {
        schema_version: EFFECT_RECONCILIATION_SCHEMA_VERSION,
        reconciliation_id: required.reconciliation_id.clone(),
        effect_id: required.effect_id.clone(),
        effect_digest: required.effect_digest.clone(),
        outcome: receipt.outcome,
        probe_receipt_digest: Some(receipt.receipt_digest),
        probe_id: Some(probe_id.clone()),
    };
    let settled_now = recorder.append_effect_reconciliation_terminal(&terminal)?;
    if settled_now {
        Ok(EffectReconciliationProbeRunOutcome::Settled(terminal))
    } else if claimed_now {
        // A second writer won the terminal CAS after this owner finished its safe read.  It is
        // intentionally not an error and never changes the existing settlement.
        Ok(EffectReconciliationProbeRunOutcome::ResumedExistingClaim { probe_id })
    } else {
        Ok(EffectReconciliationProbeRunOutcome::ResumedExistingClaim { probe_id })
    }
}

fn validate_probe_receipt_matches(
    receipt: &EffectReconciliationProbeReceiptV1,
    started: &EffectReconciliationProbeStartedEntryV1,
) -> Result<()> {
    if receipt.reconciliation_id != started.reconciliation_id
        || receipt.effect_id != started.effect_id
        || receipt.effect_digest != started.effect_digest
        || receipt.probe_id != started.probe_id
    {
        bail!("effect reconciliation probe receipt does not match its durable claim");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
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
}
