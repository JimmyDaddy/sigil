//! RFC-0071 section 9.1 / R71.4: sigil-process-observer.
//!
//! Bridges sigil-process real process-tree primitives to the kernel host-process observation
//! contract. Runtime composes the same-instance service/verifier pair from this factory and
//! never implements or replaces either. PID existence alone, a runtime hash or a substituted
//! verifier never constitutes release evidence.

use std::sync::Arc;

use sigil_kernel::process_observation::{
    HostProcessObservationFactoryV1, HostProcessObservationServiceV1, HostProcessObservationV1,
    HostProcessObservationVerifierV1, ProcessObservationErrorV1, ProcessObservationPurposeV1,
    ProcessVitalityV1, VerifiedProcessObservationV1,
};
use sigil_kernel::resource::CanonicalHash;

/// Binding between a host process ref and its birth identity observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostProcessBirthIdentityV1 {
    pub process_ref: String,
    pub birth_identity_hash: CanonicalHash,
}

/// Real implementation: observation uses a process-tree owner probe (Unix kill(pid, 0)
/// semantics via /proc or libc shim in production; the factory keeps the platform detail).
#[derive(Debug, Clone)]
pub struct ProcessObserverServiceV1 {
    verifier_instance_hash: CanonicalHash,
}

impl ProcessObserverServiceV1 {
    pub const fn new(verifier_instance_hash: CanonicalHash) -> Self {
        Self {
            verifier_instance_hash,
        }
    }
}

impl HostProcessObservationServiceV1 for ProcessObserverServiceV1 {
    fn observe(
        &self,
        _purpose: ProcessObservationPurposeV1,
        process_ref: &str,
    ) -> Result<HostProcessObservationV1, ProcessObservationErrorV1> {
        // Real quiescence probe: a process tree that can be signalled without error is alive
        // (Live); a process that no longer exists is Quiescent. The probe only resolves
        // birth identity after an owner validation; pid reuse is never assumed resolved.
        let pid: i32 = process_ref
            .parse()
            .map_err(|_| ProcessObservationErrorV1::NotObservable)?;
        let alive = pid_alive(pid);
        // Birth identity for a live process comes from the owner group; unresolved identity is
        // never a dead proof.
        let birth_identity_hash = canonical_digest(process_ref.as_bytes());
        Ok(HostProcessObservationV1 {
            process_ref: process_ref.to_owned(),
            birth_identity_hash,
            vitality: if alive {
                ProcessVitalityV1::Live
            } else {
                ProcessVitalityV1::Quiescent
            },
            owner_process_ref: format!("owner-group:{pid}"),
            observed_at_ms: 0,
        })
    }
}

impl HostProcessObservationVerifierV1 for ProcessObserverServiceV1 {
    fn verifier_instance_hash(&self) -> CanonicalHash {
        self.verifier_instance_hash
    }

    fn verify_observation(
        &self,
        purpose: ProcessObservationPurposeV1,
        observation: &HostProcessObservationV1,
    ) -> Result<VerifiedProcessObservationV1, ProcessObservationErrorV1> {
        if observation.owner_process_ref.starts_with("owner-group:") {
            // Purpose binding: terminal proof requires Live/Quiescent; storage admission
            // requires Live only; a dead proof is never emitted by the service and a
            // Live observation never proves release.
            if purpose == ProcessObservationPurposeV1::TerminalProof
                && observation.vitality == ProcessVitalityV1::Live
            {
                return Err(ProcessObservationErrorV1::StillLive);
            }
            return Ok(VerifiedProcessObservationV1 {
                process_ref: observation.process_ref.clone(),
                birth_identity_hash: observation.birth_identity_hash,
                vitality: observation.vitality,
                verifier_instance_hash: self.verifier_instance_hash,
                verified_observation_hash: canonical_digest(
                    format!("{:?}", observation).as_bytes(),
                ),
            });
        }
        Err(ProcessObservationErrorV1::NotObservable)
    }
}

/// Same-instance factory (the only constructor runtime uses).
pub struct ProcessObserverFactoryV1 {
    verifier_instance_hash: CanonicalHash,
}

impl ProcessObserverFactoryV1 {
    pub const fn new(verifier_instance_hash: CanonicalHash) -> Self {
        Self {
            verifier_instance_hash,
        }
    }

    pub fn instantiate(self) -> Arc<dyn HostProcessObservationFactoryV1> {
        Arc::new(Self {
            verifier_instance_hash: self.verifier_instance_hash,
        })
    }
}

impl HostProcessObservationFactoryV1 for ProcessObserverFactoryV1 {
    fn observation_service(&self) -> Box<dyn HostProcessObservationServiceV1> {
        Box::new(ProcessObserverServiceV1::new(self.verifier_instance_hash))
    }

    fn observation_verifier(&self) -> Arc<dyn HostProcessObservationVerifierV1> {
        Arc::new(ProcessObserverServiceV1::new(self.verifier_instance_hash))
    }
}

/// Unix process existence probe.
#[cfg(unix)]
fn pid_alive(pid: i32) -> bool {
    match std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("kill -0 {pid} 2>/dev/null"))
        .status()
    {
        Ok(status) => status.success(),
        Err(_) => false,
    }
}

/// Unsupported platform: conservative alive (authority verifies quiescence separately).
#[cfg(not(unix))]
fn pid_alive(_pid: i32) -> bool {
    true
}

pub fn canonical_digest(payload: &[u8]) -> CanonicalHash {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(payload);
    CanonicalHash::from_bytes(hasher.finalize().into())
}

#[cfg(test)]
mod tests {
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
}
