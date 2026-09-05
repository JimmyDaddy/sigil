use std::{sync::Arc, thread, time::Duration};

use sigil_kernel::process_observation::{
    HostProcessRecoveryFacetV1, ProcessCoverageEffectiveV1, ProcessCoverageRequirementV1,
    ProcessRecoveryCoverageEvidenceV1, ProcessRecoveryFacetRequestV1,
    ProcessRecoveryPhysicalObjectV1, ProcessRecoveryPurposeV1,
};

use super::*;

fn scope(seed: u8) -> ProcessObservationScopeV1 {
    ProcessObservationScopeV1 {
        authority_epoch: 7,
        application_composition_epoch: 11,
        execution_scope_hash: CanonicalHash::from_bytes([seed; 32]),
    }
}

fn factory() -> Arc<dyn HostProcessObservationFactoryV1> {
    ProcessObserverFactoryV1::new(CanonicalHash::from_bytes([1_u8; 32]))
        .expect("current test process must be observable")
        .instantiate()
}

#[test]
fn r71_process_observer_registers_and_consumes_current_owner_identity() {
    let factory = factory();
    let service = factory.observation_service();
    let verifier = factory.observation_verifier();
    let registration = service
        .register_current_authority_owner(scope(1))
        .expect("current owner registration");
    assert_eq!(registration.identity().process_id(), std::process::id());

    let subject = verifier
        .verify_registration(
            registration,
            ProcessObservationSubjectKindV1::AuthorityBootstrapOwner,
            &scope(1),
        )
        .expect("same-factory registration verifies");
    assert!(subject.has_exact_binding(
        ProcessObservationSubjectKindV1::AuthorityBootstrapOwner,
        &scope(1),
    ));
}

#[test]
fn r71_process_observer_registration_rejects_cross_factory_kind_and_replay() {
    let issuing = factory();
    let foreign = factory();
    let registration = issuing
        .observation_service()
        .register_current_authority_owner(scope(10))
        .expect("real owner registration");
    let copy = || {
        HostProcessIdentityRegistrationV1::new(
            registration.identity().clone(),
            registration.issuance_id().to_owned(),
            registration.observed_at_monotonic_ms(),
        )
    };
    assert!(matches!(
        foreign.observation_verifier().verify_registration(
            copy(),
            ProcessObservationSubjectKindV1::AuthorityBootstrapOwner,
            &scope(10)
        ),
        Err(ProcessObservationErrorV1::VerifierInstanceDrift)
    ));
    let verifier = issuing.observation_verifier();
    assert!(matches!(
        verifier.verify_registration(
            copy(),
            ProcessObservationSubjectKindV1::ManagedExecution,
            &scope(10)
        ),
        Err(ProcessObservationErrorV1::SubjectKindMismatch)
    ));
    verifier
        .verify_registration(
            copy(),
            ProcessObservationSubjectKindV1::AuthorityBootstrapOwner,
            &scope(10),
        )
        .expect("rejected probes do not consume valid issuance");
    assert!(matches!(
        verifier.verify_registration(
            copy(),
            ProcessObservationSubjectKindV1::AuthorityBootstrapOwner,
            &scope(10)
        ),
        Err(ProcessObservationErrorV1::VerifierInstanceDrift)
    ));
}

#[test]
fn r71_process_observer_recovery_rejects_cross_factory_scope_and_replay() {
    let issuing = factory();
    let verifier = issuing.observation_verifier();
    let subject = verifier
        .verify_registration(
            issuing
                .observation_service()
                .register_current_authority_owner(scope(11))
                .expect("real owner registration"),
            ProcessObservationSubjectKindV1::AuthorityBootstrapOwner,
            &scope(11),
        )
        .expect("verified owner");
    let observation = issuing
        .authority_recovery_probe()
        .observe_identity_for_authority_recovery(
            &subject,
            ProcessObservationSubjectKindV1::AuthorityBootstrapOwner,
            &scope(11),
        )
        .expect("fresh real owner observation");
    let copy = || {
        HostProcessRecoveryObservationV1::new(
            observation.subject_registration_hash(),
            observation.vitality(),
            observation.issuance_id().to_owned(),
            observation.observed_at_monotonic_ms(),
        )
    };
    assert!(matches!(
        factory()
            .observation_verifier()
            .verify_recovery_observation(
                &subject,
                ProcessObservationSubjectKindV1::AuthorityBootstrapOwner,
                &scope(11),
                copy()
            ),
        Err(ProcessObservationErrorV1::VerifierInstanceDrift)
    ));
    assert!(matches!(
        verifier.verify_recovery_observation(
            &subject,
            ProcessObservationSubjectKindV1::AuthorityBootstrapOwner,
            &scope(12),
            copy()
        ),
        Err(ProcessObservationErrorV1::ScopeMismatch)
    ));
    assert_eq!(
        verifier
            .verify_recovery_observation(
                &subject,
                ProcessObservationSubjectKindV1::AuthorityBootstrapOwner,
                &scope(11),
                copy()
            )
            .expect("rejected probes do not consume valid observation")
            .vitality,
        ProcessVitalityV1::Live
    );
    assert!(matches!(
        verifier.verify_recovery_observation(
            &subject,
            ProcessObservationSubjectKindV1::AuthorityBootstrapOwner,
            &scope(11),
            copy()
        ),
        Err(ProcessObservationErrorV1::VerifierInstanceDrift)
    ));
}

#[test]
fn r71_process_observer_rejects_forged_or_wrong_scope_registration_without_consuming_it() {
    let factory = factory();
    let service = factory.observation_service();
    let verifier = factory.observation_verifier();
    let registration = service
        .register_current_authority_owner(scope(2))
        .expect("registration");
    let forged = HostProcessIdentityRegistrationV1::new(
        registration.identity().clone(),
        "forged-issuance".to_owned(),
        registration.observed_at_monotonic_ms(),
    );
    assert!(matches!(
        verifier.verify_registration(
            forged,
            ProcessObservationSubjectKindV1::AuthorityBootstrapOwner,
            &scope(2),
        ),
        Err(ProcessObservationErrorV1::VerifierInstanceDrift)
    ));
    assert!(matches!(
        verifier.verify_registration(
            registration,
            ProcessObservationSubjectKindV1::AuthorityBootstrapOwner,
            &scope(3),
        ),
        Err(ProcessObservationErrorV1::ScopeMismatch)
    ));
}

#[test]
fn r71_process_observer_recovery_evidence_is_one_shot_and_live_for_the_current_owner() {
    let factory = factory();
    let service = factory.observation_service();
    let verifier = factory.observation_verifier();
    let probe = factory.authority_recovery_probe();
    let registration = service
        .register_current_authority_owner(scope(4))
        .expect("registration");
    let subject = verifier
        .verify_registration(
            registration,
            ProcessObservationSubjectKindV1::AuthorityBootstrapOwner,
            &scope(4),
        )
        .expect("subject");
    let observation = probe
        .observe_identity_for_authority_recovery(
            &subject,
            ProcessObservationSubjectKindV1::AuthorityBootstrapOwner,
            &scope(4),
        )
        .expect("re-observe current owner");
    let verified = verifier
        .verify_recovery_observation(
            &subject,
            ProcessObservationSubjectKindV1::AuthorityBootstrapOwner,
            &scope(4),
            observation,
        )
        .expect("recovery evidence");
    assert_eq!(verified.vitality, ProcessVitalityV1::Live);
}

#[test]
fn r71_process_observer_recovery_facet_binds_exact_frontier_and_coverage() {
    let factory = factory();
    let service = factory.observation_service();
    let verifier = factory.observation_verifier();
    let probe = factory.authority_recovery_probe();
    let owner_scope = scope(13);
    let subject = verifier
        .verify_registration(
            service
                .register_current_authority_owner(owner_scope.clone())
                .expect("real owner registration"),
            ProcessObservationSubjectKindV1::AuthorityBootstrapOwner,
            &owner_scope,
        )
        .expect("verified real owner");
    let request = ProcessRecoveryFacetRequestV1 {
        purpose: ProcessRecoveryPurposeV1::OwnerClaimRecovery,
        physical_object: ProcessRecoveryPhysicalObjectV1::Owner,
        required_coverage: ProcessCoverageRequirementV1::BoundedNativeAllowed,
        effective_coverage: ProcessCoverageEffectiveV1::BoundedNative,
        authority_frontier: CanonicalHash::from_bytes([0x13; 32]),
    };
    let facet = probe
        .observe_recovery_facet_for_authority(
            &subject,
            ProcessObservationSubjectKindV1::AuthorityBootstrapOwner,
            &owner_scope,
            request.clone(),
        )
        .expect("real owner facet");
    let copy = || {
        HostProcessRecoveryFacetV1::new(
            facet.subject_registration_hash(),
            facet.request().clone(),
            facet.vitality(),
            facet.issuance_id().to_owned(),
            facet.observed_at_monotonic_ms(),
            facet.expires_at_monotonic_ms(),
        )
    };
    let wrong_frontier = ProcessRecoveryFacetRequestV1 {
        authority_frontier: CanonicalHash::from_bytes([0x14; 32]),
        ..request.clone()
    };
    assert!(matches!(
        verifier.verify_recovery_facet(
            &subject,
            ProcessObservationSubjectKindV1::AuthorityBootstrapOwner,
            &owner_scope,
            &wrong_frontier,
            copy(),
        ),
        Err(ProcessObservationErrorV1::VerifierInstanceDrift)
    ));
    let verified = verifier
        .verify_recovery_facet(
            &subject,
            ProcessObservationSubjectKindV1::AuthorityBootstrapOwner,
            &owner_scope,
            &request,
            copy(),
        )
        .expect("wrong-frontier rejection must not consume the exact facet");
    assert_eq!(verified.request(), &request);
    assert_eq!(verified.vitality(), ProcessVitalityV1::Live);
    assert_eq!(
        verified.effective_coverage(),
        ProcessCoverageEffectiveV1::BoundedNative
    );
    assert!(matches!(
        verifier.verify_recovery_facet(
            &subject,
            ProcessObservationSubjectKindV1::AuthorityBootstrapOwner,
            &owner_scope,
            &request,
            copy(),
        ),
        Err(ProcessObservationErrorV1::VerifierInstanceDrift)
    ));

    let malformed_strong_request = ProcessRecoveryFacetRequestV1 {
        purpose: ProcessRecoveryPurposeV1::ContainedTreeQuiescence,
        physical_object: ProcessRecoveryPhysicalObjectV1::ContainedTree {
            containment_binding: CanonicalHash::from_bytes([0x15; 32]),
        },
        required_coverage: ProcessCoverageRequirementV1::ContainedTreeRequired,
        effective_coverage: ProcessCoverageEffectiveV1::BoundedNative,
        authority_frontier: CanonicalHash::from_bytes([0x16; 32]),
    };
    assert!(matches!(
        probe.observe_recovery_facet_for_authority(
            &subject,
            ProcessObservationSubjectKindV1::AuthorityBootstrapOwner,
            &owner_scope,
            malformed_strong_request,
        ),
        Err(ProcessObservationErrorV1::MalformedRecoveryFacet)
    ));
}

#[test]
fn r71_process_observer_rejects_caller_claimed_strong_coverage_without_observed_proof() {
    let factory = factory();
    let service = factory.observation_service();
    let verifier = factory.observation_verifier();
    let probe = factory.authority_recovery_probe();
    let owner_scope = scope(14);
    let subject = verifier
        .verify_registration(
            service
                .register_current_authority_owner(owner_scope.clone())
                .expect("real owner registration"),
            ProcessObservationSubjectKindV1::AuthorityBootstrapOwner,
            &owner_scope,
        )
        .expect("verified real owner");

    let contained_tree_claim = ProcessRecoveryFacetRequestV1 {
        purpose: ProcessRecoveryPurposeV1::ContainedTreeQuiescence,
        physical_object: ProcessRecoveryPhysicalObjectV1::ContainedTree {
            containment_binding: CanonicalHash::from_bytes([0x31; 32]),
        },
        required_coverage: ProcessCoverageRequirementV1::ContainedTreeRequired,
        effective_coverage: ProcessCoverageEffectiveV1::ContainedTree,
        authority_frontier: CanonicalHash::from_bytes([0x32; 32]),
    };
    let old_epoch_claim = ProcessRecoveryFacetRequestV1 {
        purpose: ProcessRecoveryPurposeV1::OldEpochQuiescence,
        physical_object: ProcessRecoveryPhysicalObjectV1::OldEpoch {
            inventory_snapshot_hash: CanonicalHash::from_bytes([0x33; 32]),
        },
        required_coverage: ProcessCoverageRequirementV1::ContainedTreeRequired,
        effective_coverage: ProcessCoverageEffectiveV1::ContainedTree,
        authority_frontier: CanonicalHash::from_bytes([0x34; 32]),
    };

    // Both requests have a valid caller shape. They are rejected because this observer has only
    // re-observed the leader; it has not observed a closed member set or complete old epoch.
    assert!(contained_tree_claim.is_well_formed());
    assert!(old_epoch_claim.is_well_formed());
    let leader_evidence = ProcessRecoveryCoverageEvidenceV1::leader(subject.registration_hash());
    assert!(!leader_evidence.proves(&contained_tree_claim));
    assert!(!leader_evidence.proves(&old_epoch_claim));

    for forged_claim in [contained_tree_claim, old_epoch_claim] {
        assert!(matches!(
            probe.observe_recovery_facet_for_authority(
                &subject,
                ProcessObservationSubjectKindV1::AuthorityBootstrapOwner,
                &owner_scope,
                forged_claim,
            ),
            Err(ProcessObservationErrorV1::StrongCoverageEvidenceUnavailable)
        ));
    }
}

#[test]
fn r71_process_observer_requires_exact_registered_member_binding_and_nonzero_physical_bindings() {
    let factory = factory();
    let service = factory.observation_service();
    let verifier = factory.observation_verifier();
    let probe = factory.authority_recovery_probe();
    let member_scope = scope(15);
    let subject = verifier
        .verify_registration(
            service
                .register_current_authority_owner(member_scope.clone())
                .expect("real owner registration"),
            ProcessObservationSubjectKindV1::AuthorityBootstrapOwner,
            &member_scope,
        )
        .expect("verified real owner");
    let matching_member_request = ProcessRecoveryFacetRequestV1 {
        purpose: ProcessRecoveryPurposeV1::RegisteredMemberSettlement,
        physical_object: ProcessRecoveryPhysicalObjectV1::RegisteredMember {
            member_binding: subject.registration_hash(),
        },
        required_coverage: ProcessCoverageRequirementV1::BoundedNativeAllowed,
        effective_coverage: ProcessCoverageEffectiveV1::BoundedNative,
        authority_frontier: CanonicalHash::from_bytes([0x41; 32]),
    };
    let leader_evidence = ProcessRecoveryCoverageEvidenceV1::leader(subject.registration_hash());
    assert!(matching_member_request.is_well_formed());
    assert!(leader_evidence.proves(&matching_member_request));

    let mismatched_member_request = ProcessRecoveryFacetRequestV1 {
        physical_object: ProcessRecoveryPhysicalObjectV1::RegisteredMember {
            member_binding: CanonicalHash::from_bytes([0x42; 32]),
        },
        ..matching_member_request.clone()
    };
    assert!(mismatched_member_request.is_well_formed());
    assert!(!leader_evidence.proves(&mismatched_member_request));
    assert!(matches!(
        probe.observe_recovery_facet_for_authority(
            &subject,
            ProcessObservationSubjectKindV1::AuthorityBootstrapOwner,
            &member_scope,
            mismatched_member_request,
        ),
        Err(ProcessObservationErrorV1::StrongCoverageEvidenceUnavailable)
    ));

    let zero_member_binding = ProcessRecoveryFacetRequestV1 {
        physical_object: ProcessRecoveryPhysicalObjectV1::RegisteredMember {
            member_binding: CanonicalHash::from_bytes([0; 32]),
        },
        ..matching_member_request
    };
    let zero_containment_binding = ProcessRecoveryFacetRequestV1 {
        purpose: ProcessRecoveryPurposeV1::ContainedTreeQuiescence,
        physical_object: ProcessRecoveryPhysicalObjectV1::ContainedTree {
            containment_binding: CanonicalHash::from_bytes([0; 32]),
        },
        required_coverage: ProcessCoverageRequirementV1::ContainedTreeRequired,
        effective_coverage: ProcessCoverageEffectiveV1::ContainedTree,
        authority_frontier: CanonicalHash::from_bytes([0x43; 32]),
    };
    let zero_epoch_snapshot = ProcessRecoveryFacetRequestV1 {
        purpose: ProcessRecoveryPurposeV1::OldEpochQuiescence,
        physical_object: ProcessRecoveryPhysicalObjectV1::OldEpoch {
            inventory_snapshot_hash: CanonicalHash::from_bytes([0; 32]),
        },
        required_coverage: ProcessCoverageRequirementV1::ContainedTreeRequired,
        effective_coverage: ProcessCoverageEffectiveV1::ContainedTree,
        authority_frontier: CanonicalHash::from_bytes([0x44; 32]),
    };
    assert!(!zero_member_binding.is_well_formed());
    assert!(!zero_containment_binding.is_well_formed());
    assert!(!zero_epoch_snapshot.is_well_formed());
}

#[cfg(unix)]
fn spawn_short_lived_child() -> std::io::Result<std::process::Child> {
    std::process::Command::new("sleep").arg("1").spawn()
}

#[cfg(windows)]
fn spawn_short_lived_child() -> std::io::Result<std::process::Child> {
    std::process::Command::new("ping")
        .args(["127.0.0.1", "-n", "2"])
        .spawn()
}

#[test]
fn r71_process_observer_reports_an_owned_reaped_child_as_quiescent_not_live() {
    let mut child = spawn_short_lived_child().expect("test child");
    let factory = factory();
    let service = factory.observation_service();
    let verifier = factory.observation_verifier();
    let probe = factory.authority_recovery_probe();
    let child_scope = scope(5);
    let registration = service
        .register_spawned_process(child_scope.clone(), child.id())
        .expect("live owned child registration");
    let subject = verifier
        .verify_registration(
            registration,
            ProcessObservationSubjectKindV1::ManagedExecution,
            &child_scope,
        )
        .expect("owned child subject");
    child.wait().expect("reap child");
    let observation = probe
        .observe_identity_for_authority_recovery(
            &subject,
            ProcessObservationSubjectKindV1::ManagedExecution,
            &child_scope,
        )
        .expect("terminated child is a subject-bound observation");
    let verified = verifier
        .verify_recovery_observation(
            &subject,
            ProcessObservationSubjectKindV1::ManagedExecution,
            &child_scope,
            observation,
        )
        .expect("subject-bound terminal evidence");
    assert_eq!(verified.vitality, ProcessVitalityV1::Quiescent);
}

#[test]
fn r71_process_observer_keeps_a_real_registration_when_its_owned_child_exits_before_verify() {
    let mut child = spawn_short_lived_child().expect("test child");
    let factory = factory();
    let service = factory.observation_service();
    let verifier = factory.observation_verifier();
    let probe = factory.authority_recovery_probe();
    let child_scope = scope(9);
    let registration = service
        .register_spawned_process(child_scope.clone(), child.id())
        .expect("live owned child registration");

    // This models the narrow post-spawn race: issuance observed a real live birth, but the
    // owned child completed before the authority verifies and persists that registration.
    child.wait().expect("reap owned child before verify");
    let subject = verifier
        .verify_registration(
            registration,
            ProcessObservationSubjectKindV1::ManagedExecution,
            &child_scope,
        )
        .expect("historical birth registration remains valid");
    let observation = probe
        .observe_identity_for_authority_recovery(
            &subject,
            ProcessObservationSubjectKindV1::ManagedExecution,
            &child_scope,
        )
        .expect("exited subject receives fresh recovery observation");
    let verified = verifier
        .verify_recovery_observation(
            &subject,
            ProcessObservationSubjectKindV1::ManagedExecution,
            &child_scope,
            observation,
        )
        .expect("recovery evidence");
    assert_eq!(verified.vitality, ProcessVitalityV1::Quiescent);
}

#[test]
fn r71_process_observer_rejects_expired_registration() {
    let factory = ProcessObserverFactoryV1::with_max_evidence_age(
        CanonicalHash::from_bytes([2_u8; 32]),
        Duration::ZERO,
    )
    .expect("factory")
    .instantiate();
    let registration = factory
        .observation_service()
        .register_current_authority_owner(scope(6))
        .expect("registration");
    thread::sleep(Duration::from_millis(1));
    assert!(matches!(
        factory.observation_verifier().verify_registration(
            registration,
            ProcessObservationSubjectKindV1::AuthorityBootstrapOwner,
            &scope(6),
        ),
        Err(ProcessObservationErrorV1::EvidenceExpired)
    ));
}

#[test]
fn r71_process_observer_entropy_failure_returns_typed_error_before_issuance_insert() {
    let state = Arc::new(ObserverStateV1::new(
        CanonicalHash::from_bytes([3_u8; 32]),
        Duration::from_secs(60),
    ));
    let service = ProcessObserverServiceV1::from_state(Arc::clone(&state));
    assert!(matches!(
        service.register_process_with_id_source(
            ProcessObservationSubjectKindV1::AuthorityBootstrapOwner,
            scope(7),
            std::process::id(),
            || Err(ProcessObservationErrorV1::NotObservable),
        ),
        Err(ProcessObservationErrorV1::NotObservable)
    ));
    assert!(
        state
            .registrations
            .lock()
            .expect("issuance lock")
            .is_empty()
    );
}

#[test]
fn r71_process_observer_fails_closed_when_a_subject_birth_is_replaced() {
    let factory = factory();
    let service = factory.observation_service();
    let verifier = factory.observation_verifier();
    let registration = service
        .register_current_authority_owner(scope(8))
        .expect("registration");
    let subject = verifier
        .verify_registration(
            registration,
            ProcessObservationSubjectKindV1::AuthorityBootstrapOwner,
            &scope(8),
        )
        .expect("subject");
    let replaced = VerifiedHostProcessIdentityV1::from_verified_registration(
        subject.process_id(),
        CanonicalHash::from_bytes([0x44; 32]),
        subject.subject_kind(),
        subject.scope().clone(),
        subject.registration_nonce().to_owned(),
        subject.registration_hash(),
        subject.registration_service_instance_hash(),
        subject.registration_service_generation(),
    );
    assert!(matches!(
        factory
            .authority_recovery_probe()
            .observe_identity_for_authority_recovery(
                &replaced,
                ProcessObservationSubjectKindV1::AuthorityBootstrapOwner,
                &scope(8),
            ),
        Err(ProcessObservationErrorV1::BirthIdentityMismatch)
    ));
}
