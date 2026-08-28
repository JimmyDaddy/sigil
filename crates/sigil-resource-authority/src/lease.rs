//! RFC-0071 section 10.3 / 10.5: managed generation lifecycle state machine and leases.
//!
//! Illegal transitions are rejected with a closed reason code. In particular: Planned never goes
//! directly Active; Ready may not spawn without a bound permission/lease hash; after Active any
//! error must settle first; Quarantined generations are never reactivated; CleanupIncomplete can
//! only forward-reconcile.

use sigil_kernel::resource::{
    AuthorityGeneration, CanonicalHash, PhysicalAttemptId, ResourceCleanupStatusV1,
    ResourceJournalScopeV1,
};

/// Closed lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ResourceGenerationStateV1 {
    Planned,
    Provisioning,
    Ready,
    Bound,
    Active,
    Finalizing,
    Released,
    Quarantined,
    CleanupIncomplete,
}

impl ResourceGenerationStateV1 {
    /// Closed transition matrix. Only edges listed here are legal.
    pub const fn can_transition(self, next: Self) -> bool {
        use ResourceGenerationStateV1::*;
        matches!(
            (self, next),
            (Planned, Provisioning)
                | (Provisioning, Ready)
                | (Ready, Bound)
                | (Bound, Active)
                | (Active, Finalizing)
                | (Finalizing, Released)
                | (Finalizing, Quarantined)
                | (Finalizing, CleanupIncomplete)
                | (Planned, Released)
                | (CleanupIncomplete, CleanupIncomplete)
        )
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::Provisioning => "provisioning",
            Self::Ready => "ready",
            Self::Bound => "bound",
            Self::Active => "active",
            Self::Finalizing => "finalizing",
            Self::Released => "released",
            Self::Quarantined => "quarantined",
            Self::CleanupIncomplete => "cleanup-incomplete",
        }
    }
}

/// Closed illegal-transition reason.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LeaseTransitionErrorV1 {
    #[error("illegal state transition: {from} -> {to}")]
    IllegalTransition {
        from: &'static str,
        to: &'static str,
    },
    #[error("Planned generation may not jump directly to Active")]
    PlannedToActive,
    #[error("Ready generation may not spawn without permission/lease hash binding")]
    UnboundReadySpawn,
    #[error("Quarantined generation can never be reactivated")]
    QuarantinedReactivation,
    #[error("CleanupIncomplete can only forward-reconcile")]
    CleanupIncompleteNotForward,
    #[error("active holders remain: {0}")]
    ActiveHolders(u64),
}

/// One managed generation record (journal-backed identity, not a live handle).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedGenerationRecordV1 {
    pub resource_id: String,
    pub generation: u64,
    pub state: ResourceGenerationStateV1,
    pub authority_generation: AuthorityGeneration,
    pub journal_scope: ResourceJournalScopeV1,
    pub physical_attempt_id: Option<PhysicalAttemptId>,
    pub bound_manifest_hash: Option<CanonicalHash>,
    pub holder_count: u64,
    pub cleanup_status: ResourceCleanupStatusV1,
    pub journal_frontier_hash: CanonicalHash,
}

impl ManagedGenerationRecordV1 {
    /// Applies a closed transition; rejects every illegal edge before mutation.
    pub fn transition(
        &mut self,
        next: ResourceGenerationStateV1,
    ) -> Result<(), LeaseTransitionErrorV1> {
        if matches!(
            (self.state, next),
            (
                ResourceGenerationStateV1::Planned,
                ResourceGenerationStateV1::Active
            )
        ) {
            return Err(LeaseTransitionErrorV1::PlannedToActive);
        }
        if self.state == ResourceGenerationStateV1::Quarantined {
            return Err(LeaseTransitionErrorV1::QuarantinedReactivation);
        }
        if self.state == ResourceGenerationStateV1::CleanupIncomplete
            && next != ResourceGenerationStateV1::CleanupIncomplete
        {
            return Err(LeaseTransitionErrorV1::CleanupIncompleteNotForward);
        }
        if self.state == ResourceGenerationStateV1::Ready
            && next == ResourceGenerationStateV1::Active
            && self.bound_manifest_hash.is_none()
        {
            return Err(LeaseTransitionErrorV1::UnboundReadySpawn);
        }
        if next == ResourceGenerationStateV1::Finalizing && self.holder_count > 0 {
            return Err(LeaseTransitionErrorV1::ActiveHolders(self.holder_count));
        }
        if !ResourceGenerationStateV1::can_transition(self.state, next) {
            return Err(LeaseTransitionErrorV1::IllegalTransition {
                from: self.state.as_str(),
                to: next.as_str(),
            });
        }
        self.state = next;
        Ok(())
    }
}

/// Non-clone lifetime-scoped lease handle.
#[derive(Debug)]
pub struct ManagedLeaseHandleV1 {
    pub resource_id: String,
    pub generation: u64,
    pub bound_manifest_hash: CanonicalHash,
    pub journal_scope: ResourceJournalScopeV1,
}

#[cfg(test)]
#[path = "tests/lease_tests.rs"]
mod tests;
