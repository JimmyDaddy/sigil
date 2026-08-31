//! RFC-0071 host-process identity observation adapter.
//!
//! `sigil-process` supplies platform birth facts. This crate turns those facts into scoped,
//! one-shot evidence. It never interprets an authority inventory or decides whether a resource or
//! a process tree may settle.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use ring::rand::{SecureRandom, SystemRandom};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    process_observation::{
        HostProcessIdentityRecoveryProbeV1, HostProcessIdentityRegistrationV1,
        HostProcessObservationFactoryV1, HostProcessObservationServiceV1,
        HostProcessObservationVerifierV1, HostProcessRecoveryObservationV1,
        ProcessObservationErrorV1, ProcessObservationScopeV1, ProcessObservationSubjectKindV1,
        ProcessVitalityV1, VerifiedHostProcessIdentityV1, VerifiedHostProcessRecoveryObservationV1,
    },
    resource::CanonicalHash,
};
use sigil_process::{
    ProcessIdentityObservationErrorV1, ProcessIdentityV1, observe_current_process_identity,
    observe_process_identity,
};
use uuid::Uuid;

const OBSERVATION_DOMAIN: &[u8] = b"sigil-process-observer-v2\0";
const REGISTRATION_DOMAIN: &[u8] = b"sigil-process-observer-registration-v2\0";
const RECOVERY_DOMAIN: &[u8] = b"sigil-process-observer-recovery-v2\0";
const DEFAULT_MAX_EVIDENCE_AGE: Duration = Duration::from_secs(60);
const MAX_PENDING_OBSERVATIONS: usize = 1024;
const SERVICE_GENERATION: u64 = 1;

#[derive(Clone)]
struct IssuedRegistrationV1 {
    identity: VerifiedHostProcessIdentityV1,
    birth_identity: ProcessIdentityV1,
    issued_at: Instant,
}

#[derive(Clone)]
struct IssuedRecoveryObservationV1 {
    subject_registration_hash: CanonicalHash,
    vitality: ProcessVitalityV1,
    observed_at_monotonic_ms: u64,
    issued_at: Instant,
}

struct ObserverStateV1 {
    service_instance_hash: CanonicalHash,
    started_at: Instant,
    max_evidence_age: Duration,
    registrations: Mutex<BTreeMap<String, IssuedRegistrationV1>>,
    recovery_observations: Mutex<BTreeMap<String, IssuedRecoveryObservationV1>>,
}

impl ObserverStateV1 {
    fn new(service_instance_hash: CanonicalHash, max_evidence_age: Duration) -> Self {
        Self {
            service_instance_hash,
            started_at: Instant::now(),
            max_evidence_age,
            registrations: Mutex::new(BTreeMap::new()),
            recovery_observations: Mutex::new(BTreeMap::new()),
        }
    }

    fn observed_at_monotonic_ms(&self) -> u64 {
        u64::try_from(self.started_at.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

/// Real host-process observation service. Construction is private to the factory so its three
/// facets share one private, one-shot issuance state.
pub struct ProcessObserverServiceV1 {
    state: Arc<ObserverStateV1>,
}

impl ProcessObserverServiceV1 {
    fn from_state(state: Arc<ObserverStateV1>) -> Self {
        Self { state }
    }

    fn register_process(
        &self,
        subject_kind: ProcessObservationSubjectKindV1,
        scope: ProcessObservationScopeV1,
        process_id: u32,
    ) -> Result<HostProcessIdentityRegistrationV1, ProcessObservationErrorV1> {
        self.register_process_with_id_source(subject_kind, scope, process_id, new_observation_id)
    }

    fn register_process_with_id_source(
        &self,
        subject_kind: ProcessObservationSubjectKindV1,
        scope: ProcessObservationScopeV1,
        process_id: u32,
        mut next_observation_id: impl FnMut() -> Result<String, ProcessObservationErrorV1>,
    ) -> Result<HostProcessIdentityRegistrationV1, ProcessObservationErrorV1> {
        if !scope.is_well_formed() {
            return Err(ProcessObservationErrorV1::MalformedScope);
        }
        let birth_identity =
            observe_process_identity(process_id).map_err(map_registration_error)?;
        let birth_identity_hash =
            CanonicalHash::from_bytes(birth_identity.birth_identity_fingerprint());
        let registration_nonce = next_observation_id()?;
        let registration_hash = registration_hash(
            process_id,
            birth_identity_hash,
            subject_kind,
            &scope,
            &registration_nonce,
            self.state.service_instance_hash,
        );
        let identity = VerifiedHostProcessIdentityV1::from_verified_registration(
            process_id,
            birth_identity_hash,
            subject_kind,
            scope,
            registration_nonce,
            registration_hash,
            self.state.service_instance_hash,
            SERVICE_GENERATION,
        );
        let issuance_id = next_observation_id()?;
        let observed_at_monotonic_ms = self.state.observed_at_monotonic_ms();
        let mut registrations = self
            .state
            .registrations
            .lock()
            .map_err(|_| ProcessObservationErrorV1::NotObservable)?;
        registrations.retain(|_, issued| issued.issued_at.elapsed() <= self.state.max_evidence_age);
        if registrations.len() >= MAX_PENDING_OBSERVATIONS {
            return Err(ProcessObservationErrorV1::NotObservable);
        }
        registrations.insert(
            issuance_id.clone(),
            IssuedRegistrationV1 {
                identity: identity.clone(),
                birth_identity,
                issued_at: Instant::now(),
            },
        );
        Ok(HostProcessIdentityRegistrationV1::new(
            identity,
            issuance_id,
            observed_at_monotonic_ms,
        ))
    }

    fn verify_registration(
        &self,
        registration: HostProcessIdentityRegistrationV1,
        expected_kind: ProcessObservationSubjectKindV1,
        expected_scope: &ProcessObservationScopeV1,
    ) -> Result<VerifiedHostProcessIdentityV1, ProcessObservationErrorV1> {
        if !expected_scope.is_well_formed() {
            return Err(ProcessObservationErrorV1::MalformedScope);
        }
        let issued = self
            .state
            .registrations
            .lock()
            .map_err(|_| ProcessObservationErrorV1::NotObservable)?
            .get(registration.issuance_id())
            .cloned()
            .ok_or(ProcessObservationErrorV1::VerifierInstanceDrift)?;
        if issued.issued_at.elapsed() > self.state.max_evidence_age {
            return Err(ProcessObservationErrorV1::EvidenceExpired);
        }
        if !issued
            .identity
            .has_exact_binding(expected_kind, expected_scope)
        {
            return Err(if issued.identity.subject_kind() != expected_kind {
                ProcessObservationErrorV1::SubjectKindMismatch
            } else {
                ProcessObservationErrorV1::ScopeMismatch
            });
        }
        if registration.identity() != &issued.identity {
            return Err(ProcessObservationErrorV1::VerifierInstanceDrift);
        }
        match observe_process_identity(issued.identity.process_id()) {
            Ok(current_identity) if current_identity == issued.birth_identity => {}
            Ok(_) => return Err(ProcessObservationErrorV1::BirthIdentityMismatch),
            // The concrete child was observed live when the private issuance was made. It can
            // finish while the sandbox crosses into Resource Authority. This does not fabricate
            // a live observation: the returned DTO is that historical birth registration, and
            // the sandbox must still reap/settle its owned child while recovery re-observes it as
            // Quiescent. Retaining this narrow outcome keeps an ordinary short command from
            // failing solely because it exited between the two birth reads.
            Err(ProcessIdentityObservationErrorV1::Absent)
            | Err(ProcessIdentityObservationErrorV1::NotLive(_)) => {}
            Err(error) => return Err(map_registration_error(error)),
        }
        self.state
            .registrations
            .lock()
            .map_err(|_| ProcessObservationErrorV1::NotObservable)?
            .remove(registration.issuance_id())
            .ok_or(ProcessObservationErrorV1::VerifierInstanceDrift)?;
        Ok(issued.identity)
    }

    fn observe_for_recovery(
        &self,
        subject: &VerifiedHostProcessIdentityV1,
        expected_kind: ProcessObservationSubjectKindV1,
        expected_scope: &ProcessObservationScopeV1,
    ) -> Result<HostProcessRecoveryObservationV1, ProcessObservationErrorV1> {
        if !expected_scope.is_well_formed() {
            return Err(ProcessObservationErrorV1::MalformedScope);
        }
        if !subject.has_exact_binding(expected_kind, expected_scope) {
            return Err(if subject.subject_kind() != expected_kind {
                ProcessObservationErrorV1::SubjectKindMismatch
            } else {
                ProcessObservationErrorV1::ScopeMismatch
            });
        }
        let vitality = match observe_process_identity(subject.process_id()) {
            Ok(identity) => {
                if CanonicalHash::from_bytes(identity.birth_identity_fingerprint())
                    != subject.birth_identity_hash()
                {
                    return Err(ProcessObservationErrorV1::BirthIdentityMismatch);
                }
                ProcessVitalityV1::Live
            }
            Err(ProcessIdentityObservationErrorV1::Absent)
            | Err(ProcessIdentityObservationErrorV1::NotLive(_)) => ProcessVitalityV1::Quiescent,
            Err(
                ProcessIdentityObservationErrorV1::InvalidProcessId
                | ProcessIdentityObservationErrorV1::NotObservable(_),
            ) => return Err(ProcessObservationErrorV1::NotObservable),
        };
        let issuance_id = new_observation_id()?;
        let observed_at_monotonic_ms = self.state.observed_at_monotonic_ms();
        let mut observations = self
            .state
            .recovery_observations
            .lock()
            .map_err(|_| ProcessObservationErrorV1::NotObservable)?;
        observations.retain(|_, issued| issued.issued_at.elapsed() <= self.state.max_evidence_age);
        if observations.len() >= MAX_PENDING_OBSERVATIONS {
            return Err(ProcessObservationErrorV1::NotObservable);
        }
        observations.insert(
            issuance_id.clone(),
            IssuedRecoveryObservationV1 {
                subject_registration_hash: subject.registration_hash(),
                vitality,
                observed_at_monotonic_ms,
                issued_at: Instant::now(),
            },
        );
        Ok(HostProcessRecoveryObservationV1::new(
            subject.registration_hash(),
            vitality,
            issuance_id,
            observed_at_monotonic_ms,
        ))
    }

    fn verify_recovery_observation(
        &self,
        subject: &VerifiedHostProcessIdentityV1,
        expected_kind: ProcessObservationSubjectKindV1,
        expected_scope: &ProcessObservationScopeV1,
        observation: HostProcessRecoveryObservationV1,
    ) -> Result<VerifiedHostProcessRecoveryObservationV1, ProcessObservationErrorV1> {
        if !expected_scope.is_well_formed() {
            return Err(ProcessObservationErrorV1::MalformedScope);
        }
        if !subject.has_exact_binding(expected_kind, expected_scope) {
            return Err(if subject.subject_kind() != expected_kind {
                ProcessObservationErrorV1::SubjectKindMismatch
            } else {
                ProcessObservationErrorV1::ScopeMismatch
            });
        }
        let issued = self
            .state
            .recovery_observations
            .lock()
            .map_err(|_| ProcessObservationErrorV1::NotObservable)?
            .get(observation.issuance_id())
            .cloned()
            .ok_or(ProcessObservationErrorV1::VerifierInstanceDrift)?;
        if issued.issued_at.elapsed() > self.state.max_evidence_age {
            return Err(ProcessObservationErrorV1::EvidenceExpired);
        }
        if observation.subject_registration_hash() != subject.registration_hash()
            || observation.subject_registration_hash() != issued.subject_registration_hash
            || observation.vitality() != issued.vitality
            || observation.observed_at_monotonic_ms() != issued.observed_at_monotonic_ms
        {
            return Err(ProcessObservationErrorV1::VerifierInstanceDrift);
        }
        self.state
            .recovery_observations
            .lock()
            .map_err(|_| ProcessObservationErrorV1::NotObservable)?
            .remove(observation.issuance_id())
            .ok_or(ProcessObservationErrorV1::VerifierInstanceDrift)?;
        Ok(VerifiedHostProcessRecoveryObservationV1 {
            vitality: issued.vitality,
            verifier_instance_hash: self.state.service_instance_hash,
            verifier_service_generation: SERVICE_GENERATION,
            verified_observation_hash: recovery_observation_hash(
                subject,
                issued.vitality,
                issued.observed_at_monotonic_ms,
                self.state.service_instance_hash,
            ),
        })
    }
}

impl HostProcessObservationServiceV1 for ProcessObserverServiceV1 {
    fn register_current_authority_owner(
        &self,
        scope: ProcessObservationScopeV1,
    ) -> Result<HostProcessIdentityRegistrationV1, ProcessObservationErrorV1> {
        self.register_process(
            ProcessObservationSubjectKindV1::AuthorityBootstrapOwner,
            scope,
            std::process::id(),
        )
    }

    fn register_spawned_process(
        &self,
        scope: ProcessObservationScopeV1,
        process_id: u32,
    ) -> Result<HostProcessIdentityRegistrationV1, ProcessObservationErrorV1> {
        self.register_process(
            ProcessObservationSubjectKindV1::ManagedExecution,
            scope,
            process_id,
        )
    }
}

impl HostProcessIdentityRecoveryProbeV1 for ProcessObserverServiceV1 {
    fn observe_identity_for_authority_recovery(
        &self,
        subject: &VerifiedHostProcessIdentityV1,
        expected_kind: ProcessObservationSubjectKindV1,
        expected_scope: &ProcessObservationScopeV1,
    ) -> Result<HostProcessRecoveryObservationV1, ProcessObservationErrorV1> {
        self.observe_for_recovery(subject, expected_kind, expected_scope)
    }
}

impl HostProcessObservationVerifierV1 for ProcessObserverServiceV1 {
    fn verifier_instance_hash(&self) -> CanonicalHash {
        self.state.service_instance_hash
    }

    fn verifier_service_generation(&self) -> u64 {
        SERVICE_GENERATION
    }

    fn verify_registration(
        &self,
        registration: HostProcessIdentityRegistrationV1,
        expected_kind: ProcessObservationSubjectKindV1,
        expected_scope: &ProcessObservationScopeV1,
    ) -> Result<VerifiedHostProcessIdentityV1, ProcessObservationErrorV1> {
        self.verify_registration(registration, expected_kind, expected_scope)
    }

    fn verify_recovery_observation(
        &self,
        subject: &VerifiedHostProcessIdentityV1,
        expected_kind: ProcessObservationSubjectKindV1,
        expected_scope: &ProcessObservationScopeV1,
        observation: HostProcessRecoveryObservationV1,
    ) -> Result<VerifiedHostProcessRecoveryObservationV1, ProcessObservationErrorV1> {
        self.verify_recovery_observation(subject, expected_kind, expected_scope, observation)
    }
}

/// Same-instance factory (the only public constructor for all three observation facets).
pub struct ProcessObserverFactoryV1 {
    state: Arc<ObserverStateV1>,
}

impl ProcessObserverFactoryV1 {
    /// Builds a factory bound to the current real host process. The binding value is composition
    /// input, while the service instance is also bound to this process birth and fresh entropy.
    pub fn new(factory_binding_hash: CanonicalHash) -> Result<Self, ProcessObservationErrorV1> {
        Self::with_max_evidence_age(factory_binding_hash, DEFAULT_MAX_EVIDENCE_AGE)
    }

    fn with_max_evidence_age(
        factory_binding_hash: CanonicalHash,
        max_evidence_age: Duration,
    ) -> Result<Self, ProcessObservationErrorV1> {
        let current = observe_current_process_identity().map_err(map_registration_error)?;
        let nonce = new_observation_id()?;
        let mut hasher = Sha256::new();
        hasher.update(OBSERVATION_DOMAIN);
        hasher.update(factory_binding_hash.as_bytes());
        hasher.update(current.birth_identity_fingerprint());
        update_sized_bytes(&mut hasher, nonce.as_bytes());
        let service_instance_hash = CanonicalHash::from_bytes(hasher.finalize().into());
        Ok(Self {
            state: Arc::new(ObserverStateV1::new(
                service_instance_hash,
                max_evidence_age,
            )),
        })
    }

    #[must_use]
    pub fn instantiate(self) -> Arc<dyn HostProcessObservationFactoryV1> {
        Arc::new(self)
    }
}

impl HostProcessObservationFactoryV1 for ProcessObserverFactoryV1 {
    fn observation_service(&self) -> Arc<dyn HostProcessObservationServiceV1> {
        Arc::new(ProcessObserverServiceV1::from_state(Arc::clone(
            &self.state,
        )))
    }

    fn authority_recovery_probe(&self) -> Arc<dyn HostProcessIdentityRecoveryProbeV1> {
        Arc::new(ProcessObserverServiceV1::from_state(Arc::clone(
            &self.state,
        )))
    }

    fn observation_verifier(&self) -> Arc<dyn HostProcessObservationVerifierV1> {
        Arc::new(ProcessObserverServiceV1::from_state(Arc::clone(
            &self.state,
        )))
    }
}

fn map_registration_error(error: ProcessIdentityObservationErrorV1) -> ProcessObservationErrorV1 {
    match error {
        ProcessIdentityObservationErrorV1::InvalidProcessId
        | ProcessIdentityObservationErrorV1::Absent
        | ProcessIdentityObservationErrorV1::NotLive(_)
        | ProcessIdentityObservationErrorV1::NotObservable(_) => {
            ProcessObservationErrorV1::NotObservable
        }
    }
}

fn new_observation_id() -> Result<String, ProcessObservationErrorV1> {
    let mut random_bytes = [0u8; 16];
    SystemRandom::new()
        .fill(&mut random_bytes)
        .map_err(|_| ProcessObservationErrorV1::NotObservable)?;
    random_bytes[6] = (random_bytes[6] & 0x0f) | 0x40;
    random_bytes[8] = (random_bytes[8] & 0x3f) | 0x80;
    Ok(format!(
        "host-observation-v2:{}",
        Uuid::from_bytes(random_bytes)
    ))
}

fn registration_hash(
    process_id: u32,
    birth_identity_hash: CanonicalHash,
    subject_kind: ProcessObservationSubjectKindV1,
    scope: &ProcessObservationScopeV1,
    registration_nonce: &str,
    service_instance_hash: CanonicalHash,
) -> CanonicalHash {
    let mut hasher = Sha256::new();
    hasher.update(REGISTRATION_DOMAIN);
    hasher.update(process_id.to_be_bytes());
    hasher.update(birth_identity_hash.as_bytes());
    hasher.update([subject_kind_discriminant(subject_kind)]);
    hasher.update(scope.authority_epoch.to_be_bytes());
    hasher.update(scope.application_composition_epoch.to_be_bytes());
    hasher.update(scope.execution_scope_hash.as_bytes());
    update_sized_bytes(&mut hasher, registration_nonce.as_bytes());
    hasher.update(service_instance_hash.as_bytes());
    hasher.update(SERVICE_GENERATION.to_be_bytes());
    CanonicalHash::from_bytes(hasher.finalize().into())
}

fn recovery_observation_hash(
    subject: &VerifiedHostProcessIdentityV1,
    vitality: ProcessVitalityV1,
    observed_at_monotonic_ms: u64,
    verifier_instance_hash: CanonicalHash,
) -> CanonicalHash {
    let mut hasher = Sha256::new();
    hasher.update(RECOVERY_DOMAIN);
    hasher.update(subject.registration_hash().as_bytes());
    hasher.update([vitality_discriminant(vitality)]);
    hasher.update(observed_at_monotonic_ms.to_be_bytes());
    hasher.update(verifier_instance_hash.as_bytes());
    hasher.update(SERVICE_GENERATION.to_be_bytes());
    CanonicalHash::from_bytes(hasher.finalize().into())
}

fn subject_kind_discriminant(subject_kind: ProcessObservationSubjectKindV1) -> u8 {
    match subject_kind {
        ProcessObservationSubjectKindV1::AuthorityBootstrapOwner => 1,
        ProcessObservationSubjectKindV1::ManagedExecution => 2,
    }
}

fn vitality_discriminant(vitality: ProcessVitalityV1) -> u8 {
    match vitality {
        ProcessVitalityV1::Live => 1,
        ProcessVitalityV1::Quiescent => 2,
    }
}

fn update_sized_bytes(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

/// Returns a domain-separated canonical digest for composition bindings.
#[must_use]
pub fn canonical_digest(payload: &[u8]) -> CanonicalHash {
    let mut hasher = Sha256::new();
    hasher.update(OBSERVATION_DOMAIN);
    hasher.update(payload);
    CanonicalHash::from_bytes(hasher.finalize().into())
}

#[cfg(test)]
#[path = "tests/lib_tests.rs"]
mod tests;
