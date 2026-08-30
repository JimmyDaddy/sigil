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
