use super::*;
use sigil_kernel::resource::EnforcementRequirementClassV1;

fn enforcement() -> RequestedEnforcementV1 {
    RequestedEnforcementV1 {
        requirement: EnforcementRequirementClassV1::RequiredExact,
        deny_ambient_system_temp_write: true,
        deny_ambient_home_write: true,
        deny_ungranted_workspace_write: true,
        require_process_tree_ownership: true,
        require_network_policy: true,
        requested_capability_set_hash: canonical_hash(&[b"caps"]),
        profile_hash: canonical_hash(&[b"profile"]),
    }
}

#[test]
fn r71_launch_plan_build_and_validate_are_stable() {
    let plan = SealedSandboxLaunchPlanV1::build(
        "draft-1".to_owned(),
        enforcement(),
        canonical_hash(&[b"manifest"]),
        canonical_hash(&[b"profile"]),
    );
    plan.validate().expect("stable");
    let again = SealedSandboxLaunchPlanV1::build(
        "draft-1".to_owned(),
        enforcement(),
        canonical_hash(&[b"manifest"]),
        canonical_hash(&[b"profile"]),
    );
    assert_eq!(plan.launch_plan_hash, again.launch_plan_hash);
}

#[test]
fn r71_launch_plan_drift_is_rejected() {
    let mut plan = SealedSandboxLaunchPlanV1::build(
        "draft-1".to_owned(),
        enforcement(),
        canonical_hash(&[b"manifest"]),
        canonical_hash(&[b"profile"]),
    );
    plan.launch_plan_hash = canonical_hash(&[b"tampered"]);
    let error = plan.validate().expect_err("must fail");
    assert!(matches!(error, LaunchPlanErrorV1::PlanHashMismatch));
}
