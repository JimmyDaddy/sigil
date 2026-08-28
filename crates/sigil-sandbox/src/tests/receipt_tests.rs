use super::*;
use sigil_kernel::resource::{
    OpaqueResourceId, ResourceJournalScopeV1, ResourceKindV1, ResourceOwnerScopeV1,
};

fn resource() -> ResourceRefV1 {
    ResourceRefV1 {
        resource_id: OpaqueResourceId::new("r1".to_owned()),
        kind: ResourceKindV1::ExecutionTemp,
        owner_scope: ResourceOwnerScopeV1::Application,
        journal_scope: ResourceJournalScopeV1::Application,
        generation: 1,
    }
}

#[test]
fn r71_exact_policy_rejects_overgrant() {
    let requested = BTreeSet::from([ResourceAccessV1::Read]);
    let observed = BTreeSet::from([ResourceAccessV1::Read, ResourceAccessV1::Write]);
    let error = verify_enforcement(
        &resource(),
        &requested,
        &AccessWideningPolicyV1::Exact,
        &observed,
        SandboxBackendClassV1::LinuxBubblewrap,
        EnforcementCompletenessV1::Exact,
    )
    .expect_err("overgrant must fail");
    assert!(matches!(
        error,
        EnforcementVerificationErrorV1::ExceededByOvergrant
    ));
}

#[test]
fn r71_declared_superset_is_recorded_not_fabricated() {
    let requested = BTreeSet::from([ResourceAccessV1::Read]);
    let observed = BTreeSet::from([ResourceAccessV1::Read, ResourceAccessV1::Execute]);
    let declaration = CanonicalHash::from_bytes([7u8; 32]);
    let receipt = verify_enforcement(
        &resource(),
        &requested,
        &AccessWideningPolicyV1::AllowDeclaredSuperset {
            declaration_hash: declaration,
        },
        &observed,
        SandboxBackendClassV1::MacOsSeatbelt,
        EnforcementCompletenessV1::Exact,
    )
    .expect("declared superset ok");
    assert_eq!(
        receipt.policy_satisfaction,
        AccessPolicySatisfactionV1::DeclaredSuperset {
            declaration_hash: declaration
        }
    );
    assert_eq!(receipt.access.unavoidable_widening.len(), 1);
}

#[test]
fn r71_local_reports_none_never_a_subset() {
    let requested = BTreeSet::from([ResourceAccessV1::Read]);
    let receipt = verify_enforcement(
        &resource(),
        &requested,
        &AccessWideningPolicyV1::ExplicitUnconfined,
        &requested,
        SandboxBackendClassV1::LocalUnconfined,
        EnforcementCompletenessV1::None,
    )
    .expect("local explicit unconfined ok");
    assert_eq!(receipt.enforcement, EnforcementCompletenessV1::None);
    assert!(
        receipt.access.effective.is_empty(),
        "effective must be empty (none)"
    );
}

#[test]
fn r71_local_without_unconfined_policy_fails_closed() {
    let requested = BTreeSet::from([ResourceAccessV1::Read]);
    let error = verify_enforcement(
        &resource(),
        &requested,
        &AccessWideningPolicyV1::Exact,
        &requested,
        SandboxBackendClassV1::LocalUnconfined,
        EnforcementCompletenessV1::None,
    )
    .expect_err("must fail");
    assert!(matches!(
        error,
        EnforcementVerificationErrorV1::LocalRequiresUnconfined
    ));
}
