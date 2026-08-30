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

/// Exact evidence accepted for ArtifactStaging/ArtifactStore semantic retirement.
///
/// The evidence is intentionally pathless. The runtime may calculate the candidate set, but it
/// cannot turn that set into a physical delete authorization without the authority-owned paired
/// grant bindings and current generation below.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactRetireEligibilityEvidenceV1 {
    pub authority_generation: AuthorityGeneration,
    pub artifact_staging_grant_hash: CanonicalHash,
    pub artifact_store_grant_hash: CanonicalHash,
    pub selected_refs_hash: CanonicalHash,
    pub selected_count: u64,
    pub selected_bytes: u64,
    pub eligibility_frontier: u64,
    pub policy_hash: CanonicalHash,
}

/// One-shot authority capability for one exact artifact retirement selection.
#[derive(Debug)]
pub struct ArtifactRetireTokenV1 {
    evidence: ArtifactRetireEligibilityEvidenceV1,
    token_hash: CanonicalHash,
    consumed: bool,
}

impl ArtifactRetireTokenV1 {
    /// Consumes the physical-retirement claim exactly once.
    pub fn consume_claim(&mut self) -> Result<(), MaintenanceErrorV1> {
        if self.consumed {
            return Err(MaintenanceErrorV1::DuplicateClaim);
        }
        self.consumed = true;
        Ok(())
    }

    #[must_use]
    pub fn evidence(&self) -> &ArtifactRetireEligibilityEvidenceV1 {
        &self.evidence
    }

    #[must_use]
    pub fn token_hash(&self) -> CanonicalHash {
        self.token_hash
    }
}

/// Authority-owned paired-grant retire frontier for the managed artifact physical adapter.
#[derive(Debug)]
pub struct ArtifactRetireAuthorityV1 {
    authority_generation: AuthorityGeneration,
    artifact_staging_grant_hash: CanonicalHash,
    artifact_store_grant_hash: CanonicalHash,
    next_token_sequence: std::sync::atomic::AtomicU64,
}

impl ArtifactRetireAuthorityV1 {
    #[must_use]
    pub fn new(
        authority_generation: AuthorityGeneration,
        artifact_staging_grant_hash: CanonicalHash,
        artifact_store_grant_hash: CanonicalHash,
    ) -> Self {
        Self {
            authority_generation,
            artifact_staging_grant_hash,
            artifact_store_grant_hash,
            next_token_sequence: std::sync::atomic::AtomicU64::new(1),
        }
    }

    #[must_use]
    pub fn authority_generation(&self) -> AuthorityGeneration {
        self.authority_generation
    }

    #[must_use]
    pub fn artifact_staging_grant_hash(&self) -> CanonicalHash {
        self.artifact_staging_grant_hash
    }

    #[must_use]
    pub fn artifact_store_grant_hash(&self) -> CanonicalHash {
        self.artifact_store_grant_hash
    }

    /// Issues a one-shot token only for the exact current generation and paired artifact grants.
    pub fn authorize(
        &self,
        evidence: ArtifactRetireEligibilityEvidenceV1,
    ) -> Result<ArtifactRetireTokenV1, MaintenanceErrorV1> {
        if evidence.selected_count == 0 {
            return Err(MaintenanceErrorV1::EmptySelection);
        }
        if evidence.authority_generation != self.authority_generation {
            return Err(MaintenanceErrorV1::GenerationDrift);
        }
        if evidence.artifact_staging_grant_hash != self.artifact_staging_grant_hash
            || evidence.artifact_store_grant_hash != self.artifact_store_grant_hash
        {
            return Err(MaintenanceErrorV1::GrantMismatch);
        }
        if evidence.eligibility_frontier == 0 {
            return Err(MaintenanceErrorV1::EligibilityFrontierMissing);
        }
        let sequence = self
            .next_token_sequence
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let token_hash = hash_artifact_retire_token(&evidence, sequence);
        Ok(ArtifactRetireTokenV1 {
            evidence,
            token_hash,
            consumed: false,
        })
    }
}

fn hash_artifact_retire_token(
    evidence: &ArtifactRetireEligibilityEvidenceV1,
    sequence: u64,
) -> CanonicalHash {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(evidence.authority_generation.epoch.to_be_bytes());
    hasher.update(evidence.authority_generation.instance_hash.as_bytes());
    hasher.update(evidence.artifact_staging_grant_hash.as_bytes());
    hasher.update(evidence.artifact_store_grant_hash.as_bytes());
    hasher.update(evidence.selected_refs_hash.as_bytes());
    hasher.update(evidence.selected_count.to_be_bytes());
    hasher.update(evidence.selected_bytes.to_be_bytes());
    hasher.update(evidence.eligibility_frontier.to_be_bytes());
    hasher.update(evidence.policy_hash.as_bytes());
    hasher.update(sequence.to_be_bytes());
    CanonicalHash::from_bytes(hasher.finalize().into())
}

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
    #[error("artifact retire grant binding does not match the authority composition")]
    GrantMismatch,
    #[error("artifact retire eligibility frontier is missing")]
    EligibilityFrontierMissing,
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
#[path = "tests/maintenance_tests.rs"]
mod tests;
