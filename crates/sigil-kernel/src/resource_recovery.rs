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
mod tests {
    use super::*;
    use crate::resource::{
        EnvironmentProfileClassV1, OpaqueResourceId, OpaqueSessionId, ResourceAccessV1,
        ResourceBlockerScopeV1, ResourceCleanupPolicyV1, ResourceJournalScopeV1, ResourceKindV1,
        ResourceLeaseLifetimeV1, ResourcePurposeV1, ResourceQuotaClassV1, ResourceQuotaProfileV1,
        ResourceRetentionPolicyV1,
    };

    fn hash(seed: u8) -> CanonicalHash {
        let mut b = [0u8; 32];
        b[0] = seed;
        CanonicalHash::from_bytes(b)
    }

    fn requirement_key(scope: &str) -> ResourceRequirementKeyV1 {
        ResourceRequirementKeyV1 {
            blocker_scope: ResourceBlockerScopeV1::Session(OpaqueSessionId::new(scope.to_owned())),
            kind: ResourceKindV1::ExecutionTemp,
            purpose: ResourcePurposeV1::ExecutionPrerequisite,
            access: std::collections::BTreeSet::from([ResourceAccessV1::Read]),
            lease_lifetime: ResourceLeaseLifetimeV1::ToolCall,
            quota_profile: ResourceQuotaProfileV1 {
                class: ResourceQuotaClassV1::AttemptEphemeral,
                max_bytes: 1,
                max_entries: 1,
                max_open_holders: 1,
                max_age_ms: None,
                hard_runtime_enforcement_required: true,
                profile_hash: hash(1),
            },
            retention_policy: ResourceRetentionPolicyV1::ReleaseOnSettlement,
            cleanup_policy: ResourceCleanupPolicyV1::ReleaseExactGenerationOnSettlement,
            environment_class: EnvironmentProfileClassV1::FreshIsolatedHome,
            toolchain_class: None,
            subject_binding_hash: None,
            canonical_hash: hash(2),
        }
    }

    struct TestProjection {
        blocked: ResourceBlockerAdmissionKeyV1,
    }

    impl ActiveBlockerProjectionV1 for TestProjection {
        fn find(&self, key: &ResourceBlockerAdmissionKeyV1) -> Option<(u64, &str)> {
            if key == &self.blocked {
                Some((7, "test-blocker"))
            } else {
                None
            }
        }
    }

    #[test]
    fn r71_requirement_gate_dedupes_across_new_call_ids() {
        let key_a = ResourceBlockerAdmissionKeyV1::Requirement {
            scope: "session-1".to_owned(),
            requirement_key: requirement_key("session-1"),
        };
        let key_b = ResourceBlockerAdmissionKeyV1::Requirement {
            scope: "session-1".to_owned(),
            requirement_key: requirement_key("session-1"),
        };
        assert_eq!(
            key_a, key_b,
            "logically equivalent pre-provision requests must dedupe"
        );
        let projection = TestProjection { blocked: key_a };
        let outcome = check_admission_gate(&projection, &key_b, true).expect("gate");
        assert!(matches!(
            outcome,
            AdmissionGateOutcomeV1::BlockedByExistingRecovery { blocker_index: 7 }
        ));
    }

    #[test]
    fn r71_realized_generation_gate_blocks_across_purpose_swaps() {
        let resource = ResourceRefV1 {
            resource_id: OpaqueResourceId::new("r1".to_owned()),
            kind: ResourceKindV1::ExecutionTemp,
            owner_scope: crate::resource::ResourceOwnerScopeV1::Application,
            journal_scope: ResourceJournalScopeV1::Application,
            generation: 3,
        };
        let key = ResourceBlockerAdmissionKeyV1::RealizedGeneration {
            resource: resource.clone(),
            expected_binding: hash(5),
            capability_class: "filesystem".to_owned(),
        };
        let equivalent = ResourceBlockerAdmissionKeyV1::RealizedGeneration {
            resource,
            expected_binding: hash(5),
            capability_class: "filesystem".to_owned(),
        };
        assert_eq!(key, equivalent);
        let projection = TestProjection { blocked: key };
        assert!(matches!(
            check_admission_gate(&projection, &equivalent, true).expect("gate"),
            AdmissionGateOutcomeV1::BlockedByExistingRecovery { .. }
        ));
    }

    #[test]
    fn r71_scope_change_does_not_hit_same_requirement_blocker() {
        let key_a = ResourceBlockerAdmissionKeyV1::Requirement {
            scope: "session-1".to_owned(),
            requirement_key: requirement_key("session-1"),
        };
        let key_b = ResourceBlockerAdmissionKeyV1::Requirement {
            scope: "session-2".to_owned(),
            requirement_key: requirement_key("session-2"),
        };
        let projection = TestProjection { blocked: key_a };
        let outcome = check_admission_gate(&projection, &key_b, true).expect("gate");
        assert_eq!(outcome, AdmissionGateOutcomeV1::Allowed);
    }

    #[test]
    fn r71_gate_rejects_non_canonical_key_flag() {
        let projection = TestProjection {
            blocked: ResourceBlockerAdmissionKeyV1::Bootstrap {
                scope: "app".to_owned(),
                class: "state".to_owned(),
            },
        };
        let key = ResourceBlockerAdmissionKeyV1::ArenaQuota {
            arena: "state".to_owned(),
            quota_class: "runtime-state".to_owned(),
        };
        let error = check_admission_gate(&projection, &key, false).expect_err("non canonical");
        assert!(matches!(error, AdmissionGateErrorV1::NonCanonicalKey));
    }
}
