use std::{sync::Arc, thread, time::Duration};

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
