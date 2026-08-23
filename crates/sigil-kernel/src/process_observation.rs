//! RFC-0071 section 9.1 / 8.2: host-process observation contract.
//!
//! A runtime never implements or replaces the observation verifier. The same-instance factory
//! returns the service and verifier pair; runtime composes the exact verifier into the
//! SessionLog attachment validator and the storage admission Live/Quiescent checks. PID
//! existence, a runtime hash or a substituted verifier never constitutes release evidence.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::resource::CanonicalHash;

/// Closed process vitality classification (Live/Quiescent only; no dead proof).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProcessVitalityV1 {
    Live,
    Quiescent,
}

/// Purpose-bound observation: exactly one purpose per observation request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProcessObservationPurposeV1 {
    SessionWriterAttachment,
    StorageAdmission,
    TerminalProof,
}

/// Host-owner identity observation (birth identity + vital state).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostProcessObservationV1 {
    pub process_ref: String,
    pub birth_identity_hash: CanonicalHash,
    pub vitality: ProcessVitalityV1,
    pub owner_process_ref: String,
    pub observed_at_ms: u64,
}

/// Verified observation: the verifier proves a real birth identity / quiescence probe outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedProcessObservationV1 {
    pub process_ref: String,
    pub birth_identity_hash: CanonicalHash,
    pub vitality: ProcessVitalityV1,
    pub verifier_instance_hash: CanonicalHash,
    pub verified_observation_hash: CanonicalHash,
}

/// Same-instance service/verifier factory contract.
pub trait HostProcessObservationFactoryV1: Send + Sync {
    fn observation_service(&self) -> Box<dyn HostProcessObservationServiceV1>;
    fn observation_verifier(&self) -> Arc<dyn HostProcessObservationVerifierV1>;
}

/// Service: performs the underlying sigil-process birth-identity/quiescence probe.
pub trait HostProcessObservationServiceV1: Send {
    fn observe(
        &self,
        purpose: ProcessObservationPurposeV1,
        process_ref: &str,
    ) -> Result<HostProcessObservationV1, ProcessObservationErrorV1>;
}

/// Verifier: checks evidence provenance, purpose binding and instance identity.
pub trait HostProcessObservationVerifierV1: Send + Sync {
    fn verifier_instance_hash(&self) -> CanonicalHash;

    fn verify_observation(
        &self,
        purpose: ProcessObservationPurposeV1,
        observation: &HostProcessObservationV1,
    ) -> Result<VerifiedProcessObservationV1, ProcessObservationErrorV1>;
}

/// Closed observation error taxonomy.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProcessObservationErrorV1 {
    #[error("process is not an observable host-owned process")]
    NotObservable,
    #[error("birth identity could not be resolved; refusing a dead proof")]
    BirthIdentityUnresolved,
    #[error("observation purpose does not match the binding")]
    PurposeMismatch,
    #[error("verifier instance hash drifted; evidence rejected")]
    VerifierInstanceDrift,
    #[error("process still live; release evidence rejected")]
    StillLive,
}

/// Closed verifier error for capability checking.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CapabilityVerifyErrorV1 {
    #[error("capability verify failed: {0}")]
    VerifyFailed(String),
    #[error("capability verify error carries no recoverable state")]
    NotRecoverable,
}
