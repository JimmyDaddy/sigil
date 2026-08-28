use super::*;

#[test]
fn r71_local_requires_explicit_unconfined() {
    let error = local_confinement_guard(LocalRunPolicyV1::RequiredConfinement)
        .expect_err("required must fail");
    assert!(matches!(
        error,
        EnforcementVerificationErrorV1::LocalRequiresUnconfined
    ));
}

#[test]
fn r71_local_reports_truthful_none() {
    let descriptor =
        local_confinement_guard(LocalRunPolicyV1::ExplicitUnconfined).expect("explicit unconfined");
    assert_eq!(descriptor.enforcement, EnforcementCompletenessV1::None);
    assert!(local_bind_evidence().is_empty());
}
