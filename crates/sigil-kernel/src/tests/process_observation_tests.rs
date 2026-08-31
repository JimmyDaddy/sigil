use super::*;
use crate::resource::CanonicalHash;

fn scope(
    authority_epoch: u64,
    application_composition_epoch: u64,
    seed: u8,
) -> ProcessObservationScopeV1 {
    ProcessObservationScopeV1 {
        authority_epoch,
        application_composition_epoch,
        execution_scope_hash: CanonicalHash::from_bytes([seed; 32]),
    }
}

fn identity(
    subject_kind: ProcessObservationSubjectKindV1,
    scope: ProcessObservationScopeV1,
) -> VerifiedHostProcessIdentityV1 {
    VerifiedHostProcessIdentityV1::from_verified_registration(
        741,
        CanonicalHash::from_bytes([0x11; 32]),
        subject_kind,
        scope,
        "durable-registration-nonce".to_owned(),
        CanonicalHash::from_bytes([0x22; 32]),
        CanonicalHash::from_bytes([0x33; 32]),
        7,
    )
}

#[test]
fn process_observation_scope_requires_nonzero_authority_and_composition_epochs() {
    assert!(!scope(0, 1, 0x41).is_well_formed());
    assert!(!scope(1, 0, 0x42).is_well_formed());
    assert!(scope(1, 1, 0x43).is_well_formed());
}

#[test]
fn verified_identity_serde_round_trip_is_durable_dto_not_verifier_proof() {
    let identity = identity(
        ProcessObservationSubjectKindV1::ManagedExecution,
        scope(4, 9, 0x51),
    );
    let json = serde_json::to_value(&identity).expect("serialize durable process DTO");
    let decoded: VerifiedHostProcessIdentityV1 =
        serde_json::from_value(json.clone()).expect("deserialize durable process DTO");

    // This checks only durable inventory shape. It deliberately does not invoke an observer
    // verifier: a public DTO constructor or serde round trip is never authentication evidence.
    assert_eq!(decoded, identity);
    assert_eq!(decoded.process_id(), 741);
    assert_eq!(
        decoded.birth_identity_hash(),
        CanonicalHash::from_bytes([0x11; 32])
    );
    assert_eq!(
        decoded.registration_hash(),
        CanonicalHash::from_bytes([0x22; 32])
    );
    assert_eq!(
        decoded.registration_service_instance_hash(),
        CanonicalHash::from_bytes([0x33; 32])
    );
    assert_eq!(decoded.registration_service_generation(), 7);

    let mut missing_issuer_generation = json;
    missing_issuer_generation
        .as_object_mut()
        .expect("identity is a JSON object")
        .remove("registration_service_generation");
    assert!(
        serde_json::from_value::<VerifiedHostProcessIdentityV1>(missing_issuer_generation).is_err()
    );
}

#[test]
fn verified_identity_exact_binding_rejects_every_subject_or_scope_drift() {
    let expected_scope = scope(3, 8, 0x61);
    let identity = identity(
        ProcessObservationSubjectKindV1::ManagedExecution,
        expected_scope.clone(),
    );

    assert!(identity.has_exact_binding(
        ProcessObservationSubjectKindV1::ManagedExecution,
        &expected_scope,
    ));
    assert!(!identity.has_exact_binding(
        ProcessObservationSubjectKindV1::AuthorityBootstrapOwner,
        &expected_scope,
    ));
    assert!(!identity.has_exact_binding(
        ProcessObservationSubjectKindV1::ManagedExecution,
        &scope(4, 8, 0x61),
    ));
    assert!(!identity.has_exact_binding(
        ProcessObservationSubjectKindV1::ManagedExecution,
        &scope(3, 9, 0x61),
    ));
    assert!(!identity.has_exact_binding(
        ProcessObservationSubjectKindV1::ManagedExecution,
        &scope(3, 8, 0x62),
    ));
}
