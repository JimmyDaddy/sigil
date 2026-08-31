use std::{thread, time::Duration};

use super::*;
use sigil_kernel::process_observation::CapabilityVerifyErrorV1;
use uuid::Uuid;

fn factory() -> Arc<dyn HostProcessObservationFactoryV1> {
    ProcessObserverFactoryV1::new(CanonicalHash::from_bytes([1u8; 32])).instantiate()
}

#[test]
fn r71_process_observer_issues_and_consumes_current_host_live_evidence() {
    let factory = factory();
    let service = factory.observation_service();
    let verifier = factory.observation_verifier();
    let observation = service
        .observe(
            ProcessObservationPurposeV1::StorageAdmission,
            &std::process::id().to_string(),
        )
        .expect("current host must be observable");

    assert!(
        observation
            .owner_process_ref
            .starts_with("host-observation-v1:")
    );
    let issuer_reference = observation
        .owner_process_ref
        .strip_prefix("host-observation-v1:")
        .expect("issuer prefix");
    let issuer_reference = Uuid::parse_str(issuer_reference).expect("v4 issuer UUID");
    assert_eq!(issuer_reference.get_version_num(), 4);
    let verified = verifier
        .verify_observation(ProcessObservationPurposeV1::StorageAdmission, &observation)
        .expect("factory-issued current-host Live evidence must verify");
    assert_eq!(verified.vitality, ProcessVitalityV1::Live);
    assert_eq!(verified.process_ref, std::process::id().to_string());
    assert!(matches!(
        verifier.verify_observation(ProcessObservationPurposeV1::StorageAdmission, &observation),
        Err(ProcessObservationErrorV1::NotObservable)
    ));
}

#[test]
fn r71_process_observer_rejects_forged_live_dto_and_cross_purpose_use() {
    let factory = factory();
    let service = factory.observation_service();
    let verifier = factory.observation_verifier();
    let observation = service
        .observe(
            ProcessObservationPurposeV1::SessionWriterAttachment,
            &std::process::id().to_string(),
        )
        .expect("current host must be observable");
    let mut forged = observation.clone();
    forged.birth_identity_hash = CanonicalHash::from_bytes([9u8; 32]);
    let mut forged_process_ref = observation.clone();
    forged_process_ref.process_ref = "not-the-current-host".to_owned();
    let mut forged_vitality = observation.clone();
    forged_vitality.vitality = ProcessVitalityV1::Quiescent;
    let mut forged_observed_at = observation.clone();
    forged_observed_at.observed_at_ms = forged_observed_at.observed_at_ms.wrapping_add(1);
    let mut forged_issuer_ref = observation.clone();
    forged_issuer_ref.owner_process_ref.push_str("-forged");

    assert!(matches!(
        verifier.verify_observation(
            ProcessObservationPurposeV1::SessionWriterAttachment,
            &forged
        ),
        Err(ProcessObservationErrorV1::NotObservable)
    ));
    for forged_content in [
        forged_process_ref,
        forged_vitality,
        forged_observed_at,
        forged_issuer_ref,
    ] {
        assert!(matches!(
            verifier.verify_observation(
                ProcessObservationPurposeV1::SessionWriterAttachment,
                &forged_content
            ),
            Err(ProcessObservationErrorV1::NotObservable)
        ));
    }
    assert!(matches!(
        verifier.verify_observation(ProcessObservationPurposeV1::StorageAdmission, &observation),
        Err(ProcessObservationErrorV1::PurposeMismatch)
    ));
    verifier
        .verify_observation(
            ProcessObservationPurposeV1::SessionWriterAttachment,
            &observation,
        )
        .expect("an earlier failed verification must not consume the issuer record");
}

#[test]
fn r71_process_observer_rejects_stale_live_evidence() {
    let factory = ProcessObserverFactoryV1::with_max_evidence_age(
        CanonicalHash::from_bytes([2u8; 32]),
        Duration::ZERO,
    )
    .instantiate();
    let service = factory.observation_service();
    let verifier = factory.observation_verifier();
    let observation = service
        .observe(
            ProcessObservationPurposeV1::StorageAdmission,
            &std::process::id().to_string(),
        )
        .expect("current host must be observable");
    thread::sleep(Duration::from_millis(1));

    assert!(matches!(
        verifier.verify_observation(ProcessObservationPurposeV1::StorageAdmission, &observation),
        Err(ProcessObservationErrorV1::NotObservable)
    ));
}

#[test]
fn r71_process_observer_rejects_entropy_failure_without_issuing_evidence() {
    assert!(matches!(
        new_live_observation_id_from_random_source(|_| Err(())),
        Err(ProcessObservationErrorV1::NotObservable)
    ));

    let state = Arc::new(ObserverStateV1::new(
        CanonicalHash::from_bytes([3u8; 32]),
        Duration::from_secs(60),
    ));
    let service = ProcessObserverServiceV1::from_state(Arc::clone(&state));

    assert!(matches!(
        service.observe_current_host_live_with_observation_id(
            ProcessObservationPurposeV1::StorageAdmission,
            &std::process::id().to_string(),
            || new_live_observation_id_from_random_source(|_| Err(())),
        ),
        Err(ProcessObservationErrorV1::NotObservable)
    ));
    assert!(
        state
            .issued
            .lock()
            .expect("test-only observer issuance lock")
            .is_empty()
    );
}

#[test]
fn r71_process_observer_rejects_non_current_live_claim_and_terminal_pid_probe() {
    let factory = factory();
    let service = factory.observation_service();
    let non_current = std::process::id().saturating_add(1).to_string();

    assert!(matches!(
        service.observe(ProcessObservationPurposeV1::StorageAdmission, &non_current),
        Err(ProcessObservationErrorV1::NotObservable)
    ));
    assert!(matches!(
        service.observe(
            ProcessObservationPurposeV1::TerminalProof,
            &std::process::id().to_string(),
        ),
        Err(ProcessObservationErrorV1::BirthIdentityUnresolved)
    ));
}

#[test]
fn r71_process_observer_factory_returns_same_instance_pair() {
    let factory = factory();
    let verifier_a = factory.observation_verifier();
    let verifier_b = factory.observation_verifier();
    assert_eq!(
        verifier_a.verifier_instance_hash(),
        verifier_b.verifier_instance_hash()
    );
}

#[test]
fn r71_process_observer_rejects_evidence_from_another_factory_instance() {
    let factory_a = factory();
    let observation = factory_a
        .observation_service()
        .observe(
            ProcessObservationPurposeV1::StorageAdmission,
            &std::process::id().to_string(),
        )
        .expect("current host must be observable");
    let factory_b = factory();

    assert!(matches!(
        factory_b
            .observation_verifier()
            .verify_observation(ProcessObservationPurposeV1::StorageAdmission, &observation),
        Err(ProcessObservationErrorV1::NotObservable)
    ));

    factory_a
        .observation_verifier()
        .verify_observation(ProcessObservationPurposeV1::StorageAdmission, &observation)
        .expect("a different factory must not consume the original issuer record");
}

#[test]
fn r71_process_observer_capability_error_is_closed() {
    let error = CapabilityVerifyErrorV1::VerifyFailed("x".to_owned());
    assert!(format!("{error}").contains("verify failed"));
}
