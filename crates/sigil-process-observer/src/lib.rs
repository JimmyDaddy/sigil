//! RFC-0071 host-process observation adapter.
//!
//! `sigil-process` supplies platform birth facts. This crate only turns a current-host Live
//! observation into kernel-shaped evidence and verifies its private issuance record. It does not
//! decide resource settlement, interpret authority inventory, or turn a missing PID into a
//! terminal/tree-quiescence proof.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use ring::rand::{SecureRandom, SystemRandom};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    process_observation::{
        HostProcessObservationFactoryV1, HostProcessObservationServiceV1, HostProcessObservationV1,
        HostProcessObservationVerifierV1, ProcessObservationErrorV1, ProcessObservationPurposeV1,
        ProcessVitalityV1, VerifiedProcessObservationV1,
    },
    resource::CanonicalHash,
};
use sigil_process::{
    ProcessIdentityObservationErrorV1, ProcessIdentityV1, observe_current_process_identity,
};
use uuid::Uuid;

const OBSERVATION_DOMAIN: &[u8] = b"sigil-process-observer-live-observation-v1\0";
const VERIFIED_OBSERVATION_DOMAIN: &[u8] = b"sigil-process-observer-verified-observation-v1\0";
const DEFAULT_MAX_EVIDENCE_AGE: Duration = Duration::from_secs(60);
const MAX_PENDING_LIVE_OBSERVATIONS: usize = 1024;

#[derive(Clone)]
struct IssuedLiveObservationV1 {
    purpose: ProcessObservationPurposeV1,
    process_ref: String,
    birth_identity: ProcessIdentityV1,
    birth_identity_hash: CanonicalHash,
    observed_at_ms: u64,
    issued_at: Instant,
}

struct ObserverStateV1 {
    verifier_instance_hash: CanonicalHash,
    started_at: Instant,
    max_evidence_age: Duration,
    issued: Mutex<BTreeMap<String, IssuedLiveObservationV1>>,
}

impl ObserverStateV1 {
    fn new(verifier_instance_hash: CanonicalHash, max_evidence_age: Duration) -> Self {
        Self {
            verifier_instance_hash,
            started_at: Instant::now(),
            max_evidence_age,
            issued: Mutex::new(BTreeMap::new()),
        }
    }

    fn observed_at_ms(&self) -> u64 {
        u64::try_from(self.started_at.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

/// Real current-host observation service.
///
/// Construction is private to [`ProcessObserverFactoryV1`] so every service/verifier pair shares
/// the same private one-shot issuance state.
pub struct ProcessObserverServiceV1 {
    state: Arc<ObserverStateV1>,
}

impl ProcessObserverServiceV1 {
    fn from_state(state: Arc<ObserverStateV1>) -> Self {
        Self { state }
    }

    fn observe_current_host_live(
        &self,
        purpose: ProcessObservationPurposeV1,
        process_ref: &str,
    ) -> Result<HostProcessObservationV1, ProcessObservationErrorV1> {
        self.observe_current_host_live_with_observation_id(
            purpose,
            process_ref,
            new_live_observation_id,
        )
    }

    fn observe_current_host_live_with_observation_id(
        &self,
        purpose: ProcessObservationPurposeV1,
        process_ref: &str,
        new_observation_id: impl FnOnce() -> Result<String, ProcessObservationErrorV1>,
    ) -> Result<HostProcessObservationV1, ProcessObservationErrorV1> {
        let current_process_ref = std::process::id().to_string();
        if process_ref != current_process_ref {
            return Err(ProcessObservationErrorV1::NotObservable);
        }
        let birth_identity = observe_current_process_identity().map_err(map_process_error)?;
        let birth_identity_hash =
            CanonicalHash::from_bytes(birth_identity.birth_identity_fingerprint());
        let observed_at_ms = self.state.observed_at_ms();
        let observation_id = new_observation_id()?;
        let issued = IssuedLiveObservationV1 {
            purpose,
            process_ref: current_process_ref.clone(),
            birth_identity,
            birth_identity_hash,
            observed_at_ms,
            issued_at: Instant::now(),
        };
        let mut issued_observations = self
            .state
            .issued
            .lock()
            .map_err(|_| ProcessObservationErrorV1::NotObservable)?;
        issued_observations
            .retain(|_, issued| issued.issued_at.elapsed() <= self.state.max_evidence_age);
        if issued_observations.len() >= MAX_PENDING_LIVE_OBSERVATIONS {
            return Err(ProcessObservationErrorV1::NotObservable);
        }
        issued_observations.insert(observation_id.clone(), issued);
        Ok(HostProcessObservationV1 {
            process_ref: current_process_ref,
            birth_identity_hash,
            vitality: ProcessVitalityV1::Live,
            // Kernel V1 calls this a process ref. It is deliberately an opaque one-shot issuer
            // reference, not an owner group, PID alias, or platform locator.
            owner_process_ref: observation_id,
            observed_at_ms,
        })
    }

    fn verify_live_observation(
        &self,
        purpose: ProcessObservationPurposeV1,
        observation: &HostProcessObservationV1,
    ) -> Result<VerifiedProcessObservationV1, ProcessObservationErrorV1> {
        let issued = self
            .state
            .issued
            .lock()
            .map_err(|_| ProcessObservationErrorV1::NotObservable)?
            .get(&observation.owner_process_ref)
            .cloned()
            .ok_or(ProcessObservationErrorV1::NotObservable)?;
        if issued.purpose != purpose {
            return Err(ProcessObservationErrorV1::PurposeMismatch);
        }
        if issued.issued_at.elapsed() > self.state.max_evidence_age {
            return Err(ProcessObservationErrorV1::NotObservable);
        }
        if observation.process_ref != issued.process_ref
            || observation.birth_identity_hash != issued.birth_identity_hash
            || observation.vitality != ProcessVitalityV1::Live
            || observation.observed_at_ms != issued.observed_at_ms
        {
            return Err(ProcessObservationErrorV1::NotObservable);
        }

        let current_identity = observe_current_process_identity().map_err(map_process_error)?;
        if current_identity != issued.birth_identity {
            return Err(ProcessObservationErrorV1::BirthIdentityUnresolved);
        }

        // The evidence identifier is one-shot: consuming a verified Live observation prevents a
        // caller from replaying the same DTO after it has crossed the kernel boundary.
        self.state
            .issued
            .lock()
            .map_err(|_| ProcessObservationErrorV1::NotObservable)?
            .remove(&observation.owner_process_ref)
            .ok_or(ProcessObservationErrorV1::NotObservable)?;

        Ok(VerifiedProcessObservationV1 {
            process_ref: issued.process_ref,
            birth_identity_hash: issued.birth_identity_hash,
            vitality: ProcessVitalityV1::Live,
            verifier_instance_hash: self.state.verifier_instance_hash,
            verified_observation_hash: verified_observation_hash(
                &self.state.verifier_instance_hash,
                purpose,
                observation,
            ),
        })
    }
}

impl HostProcessObservationServiceV1 for ProcessObserverServiceV1 {
    fn observe(
        &self,
        purpose: ProcessObservationPurposeV1,
        process_ref: &str,
    ) -> Result<HostProcessObservationV1, ProcessObservationErrorV1> {
        match purpose {
            ProcessObservationPurposeV1::SessionWriterAttachment
            | ProcessObservationPurposeV1::StorageAdmission => {
                self.observe_current_host_live(purpose, process_ref)
            }
            // V1 carries only an untrusted PID-shaped string. Without a durable authenticated
            // expected birth identity it cannot distinguish an exited old process from PID reuse,
            // permission loss, or a different host process. The RA-only recovery facet added by
            // the E02 integration must supply that exact subject before Quiescent can exist.
            ProcessObservationPurposeV1::TerminalProof => {
                Err(ProcessObservationErrorV1::BirthIdentityUnresolved)
            }
        }
    }
}

impl HostProcessObservationVerifierV1 for ProcessObserverServiceV1 {
    fn verifier_instance_hash(&self) -> CanonicalHash {
        self.state.verifier_instance_hash
    }

    fn verify_observation(
        &self,
        purpose: ProcessObservationPurposeV1,
        observation: &HostProcessObservationV1,
    ) -> Result<VerifiedProcessObservationV1, ProcessObservationErrorV1> {
        self.verify_live_observation(purpose, observation)
    }
}

/// Same-instance factory (the only public constructor for service/verifier pairs).
pub struct ProcessObserverFactoryV1 {
    state: Arc<ObserverStateV1>,
}

impl ProcessObserverFactoryV1 {
    #[must_use]
    pub fn new(verifier_instance_hash: CanonicalHash) -> Self {
        Self {
            state: Arc::new(ObserverStateV1::new(
                verifier_instance_hash,
                DEFAULT_MAX_EVIDENCE_AGE,
            )),
        }
    }

    #[cfg(test)]
    fn with_max_evidence_age(
        verifier_instance_hash: CanonicalHash,
        max_evidence_age: Duration,
    ) -> Self {
        Self {
            state: Arc::new(ObserverStateV1::new(
                verifier_instance_hash,
                max_evidence_age,
            )),
        }
    }

    #[must_use]
    pub fn instantiate(self) -> Arc<dyn HostProcessObservationFactoryV1> {
        Arc::new(self)
    }
}

impl HostProcessObservationFactoryV1 for ProcessObserverFactoryV1 {
    fn observation_service(&self) -> Box<dyn HostProcessObservationServiceV1> {
        Box::new(ProcessObserverServiceV1::from_state(Arc::clone(
            &self.state,
        )))
    }

    fn observation_verifier(&self) -> Arc<dyn HostProcessObservationVerifierV1> {
        Arc::new(ProcessObserverServiceV1::from_state(Arc::clone(
            &self.state,
        )))
    }
}

fn map_process_error(error: ProcessIdentityObservationErrorV1) -> ProcessObservationErrorV1 {
    match error {
        ProcessIdentityObservationErrorV1::Absent => {
            ProcessObservationErrorV1::BirthIdentityUnresolved
        }
        ProcessIdentityObservationErrorV1::InvalidProcessId
        | ProcessIdentityObservationErrorV1::NotLive(_)
        | ProcessIdentityObservationErrorV1::NotObservable(_) => {
            ProcessObservationErrorV1::NotObservable
        }
    }
}

fn new_live_observation_id() -> Result<String, ProcessObservationErrorV1> {
    new_live_observation_id_from_random_source(|random_bytes| {
        SystemRandom::new().fill(random_bytes).map_err(|_| ())
    })
}

fn new_live_observation_id_from_random_source(
    fill_random_bytes: impl FnOnce(&mut [u8; 16]) -> Result<(), ()>,
) -> Result<String, ProcessObservationErrorV1> {
    let mut random_bytes = [0u8; 16];
    fill_random_bytes(&mut random_bytes)
        // The UUID convenience API can panic if its internal entropy call fails. Failure to
        // obtain entropy is instead a closed observation failure, before a record reaches
        // issuance.
        .map_err(|()| ProcessObservationErrorV1::NotObservable)?;
    random_bytes[6] = (random_bytes[6] & 0x0f) | 0x40;
    random_bytes[8] = (random_bytes[8] & 0x3f) | 0x80;
    Ok(format!(
        "host-observation-v1:{}",
        Uuid::from_bytes(random_bytes)
    ))
}

fn verified_observation_hash(
    verifier_instance_hash: &CanonicalHash,
    purpose: ProcessObservationPurposeV1,
    observation: &HostProcessObservationV1,
) -> CanonicalHash {
    let mut hasher = Sha256::new();
    hasher.update(VERIFIED_OBSERVATION_DOMAIN);
    hasher.update(verifier_instance_hash.as_bytes());
    hasher.update([purpose_discriminant(purpose)]);
    update_sized_bytes(&mut hasher, observation.process_ref.as_bytes());
    hasher.update(observation.birth_identity_hash.as_bytes());
    hasher.update([vitality_discriminant(observation.vitality)]);
    update_sized_bytes(&mut hasher, observation.owner_process_ref.as_bytes());
    hasher.update(observation.observed_at_ms.to_be_bytes());
    CanonicalHash::from_bytes(hasher.finalize().into())
}

fn purpose_discriminant(purpose: ProcessObservationPurposeV1) -> u8 {
    match purpose {
        ProcessObservationPurposeV1::SessionWriterAttachment => 1,
        ProcessObservationPurposeV1::StorageAdmission => 2,
        ProcessObservationPurposeV1::TerminalProof => 3,
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

/// Returns a domain-separated canonical digest for non-process instance bindings.
///
/// Runtime and Resource Authority currently use this helper only to seed a factory verifier
/// instance. Process birth identity is always calculated by `sigil-process`, never by this
/// generic helper.
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
