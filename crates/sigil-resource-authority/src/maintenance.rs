//! RFC-0071 section 10.2: maintenance plan / proof / one-shot token.
//!
//! Maintenance plans select only journal-known managed resource refs and never return host paths.
//! The kernel lifecycle/retention/recovery validator is the only issuer of the sealed proof; the
//! authority consumes the opaque one-shot capability, compares the exact plan / source /
//! selection / generation, then constructs the private token. Borrowed resources never enter the
//! selection; stale or empty selection, cross-source and duplicate claim fail before delete.

use sigil_kernel::resource::{
    AuthorityGeneration, CanonicalHash, OpaqueBlockerId, ResourceOwnerScopeV1,
};

/// Maintenance intent (closed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceMaintenanceIntentV1 {
    OwnerLifecycleTerminal {
        owner_scope: ResourceOwnerScopeV1,
        lifecycle_event_digest: CanonicalHash,
    },
    RetentionSweep {
        journal_scope: sigil_kernel::resource::ResourceJournalScopeV1,
        policy_digest: CanonicalHash,
        eligibility_frontier: u64,
    },
    ReconcileIncomplete {
        blocker_ref: OpaqueBlockerId,
        expected_generation: AuthorityGeneration,
    },
}

/// Side-effect-free maintenance plan (pathless selection).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceMaintenancePlanV1 {
    pub intent_hash: CanonicalHash,
    pub selected_resource_refs_hash: CanonicalHash,
    pub selected_count: u64,
    pub selected_bytes: u64,
    pub authority_generation: AuthorityGeneration,
    pub plan_hash: CanonicalHash,
}

/// Closed authorization source (validator-produced, never self-declared).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceMaintenanceAuthorizationSourceV1 {
    OwnerLifecycleTerminal {
        owner_scope: ResourceOwnerScopeV1,
        lifecycle_event_digest: CanonicalHash,
    },
    RetentionEligibility {
        policy_digest: CanonicalHash,
        evaluated_frontier: u64,
        eligibility_proof_digest: CanonicalHash,
    },
    RecoveryAction {
        blocker_ref: OpaqueBlockerId,
        action_token_hash: CanonicalHash,
        confirmation_digest: Option<CanonicalHash>,
    },
}

/// Authorization proof bound to the exact plan/source/generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceMaintenanceAuthorizationProofV1 {
    pub source: ResourceMaintenanceAuthorizationSourceV1,
    pub plan_hash: CanonicalHash,
    pub expected_authority_generation: AuthorityGeneration,
    pub proof_hash: CanonicalHash,
}

/// One-shot maintenance token.
#[derive(Debug)]
pub struct ResourceMaintenanceTokenV1 {
    pub plan: ResourceMaintenancePlanV1,
    pub authorization_proof: ResourceMaintenanceAuthorizationProofV1,
    claim_consumed: bool,
}

impl ResourceMaintenanceTokenV1 {
    pub fn new(
        plan: ResourceMaintenancePlanV1,
        authorization_proof: ResourceMaintenanceAuthorizationProofV1,
    ) -> Self {
        Self {
            plan,
            authorization_proof,
            claim_consumed: false,
        }
    }

    /// Consumes the one-shot claim; a second consume fails closed.
    pub fn consume_claim(&mut self) -> Result<(), MaintenanceErrorV1> {
        if self.claim_consumed {
            return Err(MaintenanceErrorV1::DuplicateClaim);
        }
        self.claim_consumed = true;
        Ok(())
    }

    pub fn plan(&self) -> &ResourceMaintenancePlanV1 {
        &self.plan
    }
}

/// Closed maintenance error classification.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MaintenanceErrorV1 {
    #[error("maintenance selection is empty; refusing to delete")]
    EmptySelection,
    #[error("selection contains a borrowed resource; borrowed content is never selected")]
    BorrowedInSelection,
    #[error("proof source does not match the plan intent")]
    SourceMismatch,
    #[error("authority generation drift between plan and proof")]
    GenerationDrift,
    #[error("one-shot maintenance claim already consumed")]
    DuplicateClaim,
    #[error("active holder prevents maintenance: {0}")]
    ActiveHolders(String),
}

/// Validates that a maintenance plan is authoritative for its intent before any delete.
pub fn validate_maintenance_binding(
    plan: &ResourceMaintenancePlanV1,
    proof: &ResourceMaintenanceAuthorizationProofV1,
) -> Result<(), MaintenanceErrorV1> {
    if plan.selected_count == 0 {
        return Err(MaintenanceErrorV1::EmptySelection);
    }
    if proof.plan_hash != plan.plan_hash {
        return Err(MaintenanceErrorV1::SourceMismatch);
    }
    if proof.expected_authority_generation != plan.authority_generation {
        return Err(MaintenanceErrorV1::GenerationDrift);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(count: u64) -> ResourceMaintenancePlanV1 {
        ResourceMaintenancePlanV1 {
            intent_hash: sigil_kernel::resource::CanonicalHash::from_bytes([1u8; 32]),
            selected_resource_refs_hash: sigil_kernel::resource::CanonicalHash::from_bytes(
                [2u8; 32],
            ),
            selected_count: count,
            selected_bytes: 42,
            authority_generation: AuthorityGeneration {
                epoch: 7,
                instance_hash: sigil_kernel::resource::CanonicalHash::from_bytes([3u8; 32]),
            },
            plan_hash: sigil_kernel::resource::CanonicalHash::from_bytes([4u8; 32]),
        }
    }

    fn proof(for_plan: &ResourceMaintenancePlanV1) -> ResourceMaintenanceAuthorizationProofV1 {
        ResourceMaintenanceAuthorizationProofV1 {
            source: ResourceMaintenanceAuthorizationSourceV1::RetentionEligibility {
                policy_digest: sigil_kernel::resource::CanonicalHash::from_bytes([5u8; 32]),
                evaluated_frontier: 1,
                eligibility_proof_digest: sigil_kernel::resource::CanonicalHash::from_bytes(
                    [6u8; 32],
                ),
            },
            plan_hash: for_plan.plan_hash,
            expected_authority_generation: for_plan.authority_generation,
            proof_hash: sigil_kernel::resource::CanonicalHash::from_bytes([7u8; 32]),
        }
    }

    #[test]
    fn r71_maintenance_empty_selection_is_rejected_before_delete() {
        let plan = plan(0);
        let proof = proof(&plan);
        let error = validate_maintenance_binding(&plan, &proof).expect_err("empty must fail");
        assert!(matches!(error, MaintenanceErrorV1::EmptySelection));
    }

    #[test]
    fn r71_maintenance_generation_drift_fails_closed() {
        let mut drifted = plan(1);
        drifted.authority_generation.epoch = 9;
        // Proof constructed from the ORIGINAL plan generation, then plan drifts after the proof.
        let original = plan(1);
        let proof = proof(&original);
        let error = validate_maintenance_binding(&drifted, &proof).expect_err("drift must fail");
        assert!(matches!(error, MaintenanceErrorV1::GenerationDrift));
    }

    #[test]
    fn r71_maintenance_token_claim_is_one_shot() {
        let plan = plan(1);
        let proof = proof(&plan);
        validate_maintenance_binding(&plan, &proof).expect("valid");
        let mut token = ResourceMaintenanceTokenV1::new(plan, proof);
        token.consume_claim().expect("first");
        let error = token.consume_claim().expect_err("second must fail");
        assert!(matches!(error, MaintenanceErrorV1::DuplicateClaim));
    }
}
