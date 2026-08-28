use super::*;
use sigil_kernel::process_observation::CapabilityVerifyErrorV1;

fn factory() -> Arc<dyn HostProcessObservationFactoryV1> {
    ProcessObserverFactoryV1::new(CanonicalHash::from_bytes([1u8; 32])).instantiate()
}

#[test]
fn r71_process_observer_live_process_cannot_prove_release() {
    let factory = factory();
    let service = factory.observation_service();
    let verifier = factory.observation_verifier();
    let observation = HostProcessObservationV1 {
        process_ref: std::process::id().to_string(),
        birth_identity_hash: canonical_digest(b"live"),
        vitality: ProcessVitalityV1::Live,
        owner_process_ref: format!("owner-group:{}", std::process::id()),
        observed_at_ms: 1,
    };
    let error = verifier
        .verify_observation(ProcessObservationPurposeV1::TerminalProof, &observation)
        .expect_err("live process must fail terminal proof");
    assert!(matches!(error, ProcessObservationErrorV1::StillLive));
    let _ = service;
}

#[test]
fn r71_process_observer_unknown_ref_is_not_observable() {
    let factory = factory();
    let service = factory.observation_service();
    let error = service
        .observe(ProcessObservationPurposeV1::StorageAdmission, "not-a-pid")
        .expect_err("must fail");
    assert!(matches!(error, ProcessObservationErrorV1::NotObservable));
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
fn r71_process_observer_capability_error_is_closed() {
    let error = CapabilityVerifyErrorV1::VerifyFailed("x".to_owned());
    assert!(format!("{error}").contains("verify failed"));
}
