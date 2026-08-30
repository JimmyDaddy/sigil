//! RFC-0071 sections 12.4 / 12.5: typed resource recovery and active-blocker admission gate.
//!
//! An exact resource operation is a typed payload under the existing RecoveryActionV1 . The
//! admission gate key must be stable across new tool calls: requirement id, call id and attempt
//! id never participate in the dedupe key, or a model can bypass the gate merely by changing the
//! tool call id.

use serde::{Deserialize, Serialize};

use crate::resource::{
    AuthorityGeneration, CanonicalHash, OpaqueRequirementId, ResourceRefV1,
    ResourceRequirementKeyV1,
};

/// Closed managed-resource recovery operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ManagedResourceRecoveryOperationV1 {
    AllocateFreshExecutionTemp {
        requirement_id: OpaqueRequirementId,
        requirement_key: ResourceRequirementKeyV1,
        failed_generation: Option<ResourceRefV1>,
    },
    QuarantineGeneration {
        resource: ResourceRefV1,
        expected_binding: CanonicalHash,
    },
    ReconcileOrphan {
        resource: ResourceRefV1,
        expected_binding: CanonicalHash,
    },
    ResetSessionScratch {
        resource: ResourceRefV1,
        expected_binding: CanonicalHash,
        preserve_quarantine: bool,
    },
    PurgeQuarantine {
        resources: Vec<ResourceRefV1>,
        expected_bindings_hash: CanonicalHash,
        retention_eligibility_proof: CanonicalHash,
    },
    ReconcileStorageNamespace {
        storage_key: String,
        expected_frontier: CanonicalHash,
        failed_grant_id: Option<String>,
        failed_resource: Option<ResourceRefV1>,
    },
    ResumeStorageNamespaceGrant {
        storage_key: String,
        expected_frontier: CanonicalHash,
        resource: ResourceRefV1,
        expected_resource_binding: CanonicalHash,
        expected_grant_hash: CanonicalHash,
    },
    RebuildRebuildableStorageGeneration {
        storage_key: String,
        failed_resource: ResourceRefV1,
        expected_grant_hash: CanonicalHash,
        authoritative_source_frontier_hash: CanonicalHash,
        semantic_rebuild_authorization_hash: CanonicalHash,
    },
    ExecuteAuthorizedMaintenance {
        plan_hash: CanonicalHash,
        selected_resource_refs_hash: CanonicalHash,
        expected_authority_generation: AuthorityGeneration,
    },
    RevealPrivateDiagnostic {
        diagnostic_ref: String,
    },
}

/// Closed recovery confirmation class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecoveryConfirmationClassV1 {
    ResetSessionScratch,
    PurgeQuarantine,
    DeleteLegacyStorage,
    RevealPrivateDiagnostic,
    RelaxContainmentOrExternalAccess,
}

/// Closed blocker admission key (dedupe identity; never contains call/attempt ids).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResourceBlockerAdmissionKeyV1 {
    /// Ordinary pre-provision blocker: requirement-scoped stable key.
    Requirement {
        scope: String,
        requirement_key: ResourceRequirementKeyV1,
    },
    /// Bootstrap failure: application/workspace stable key.
    Bootstrap { scope: String, class: String },
    /// Shared quota: arena-scoped key blocks all new reservations of that arena.
    ArenaQuota { arena: String, quota_class: String },
    /// Realized-generation strength: resource_id + generation + expected binding + capability
    /// class only. A broken generation blocks shell/terminal/file/storage equivalently.
    RealizedGeneration {
        resource: ResourceRefV1,
        expected_binding: CanonicalHash,
        capability_class: String,
    },
}

/// Closed gate outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmissionGateOutcomeV1 {
    Allowed,
    BlockedByExistingRecovery { blocker_index: u64 },
}

/// Closed gate error classification.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AdmissionGateErrorV1 {
    #[error("admission gate has no durable projection; failing closed")]
    ProjectionUnavailable,
    #[error("stable key is not canonical (call/attempt id leaked into the key)")]
    NonCanonicalKey,
}

/// Signature of the durable blocker projection.
pub trait ActiveBlockerProjectionV1 {
    fn find(&self, key: &ResourceBlockerAdmissionKeyV1) -> Option<(u64, &str)>;
}

/// Durable admission gate: one lookup per plan/admission, never duplicated with the legacy
/// retryable error field.
pub fn check_admission_gate(
    projection: &dyn ActiveBlockerProjectionV1,
    key: &ResourceBlockerAdmissionKeyV1,
    known_canonical: bool,
) -> Result<AdmissionGateOutcomeV1, AdmissionGateErrorV1> {
    if !known_canonical {
        return Err(AdmissionGateErrorV1::NonCanonicalKey);
    }
    match projection.find(key) {
        Some((index, _)) => Ok(AdmissionGateOutcomeV1::BlockedByExistingRecovery {
            blocker_index: index,
        }),
        None => Ok(AdmissionGateOutcomeV1::Allowed),
    }
}

#[cfg(test)]
#[path = "tests/resource_recovery_tests.rs"]
mod tests;
