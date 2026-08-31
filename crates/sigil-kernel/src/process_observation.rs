//! RFC-0071 section 1.1: host-process identity observation contract.
//!
//! `sigil-process` owns raw platform birth facts. The observer adapter may register a current
//! authority owner or a sandbox-produced child, but it never decides resource settlement. The
//! Resource Authority is the only durable consumer of a verified identity and the only consumer
//! of the recovery probe. A PID-shaped string is deliberately not an observation input.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::resource::CanonicalHash;

/// A process fact that has been checked against an authenticated durable subject.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProcessVitalityV1 {
    Live,
    Quiescent,
}

/// The only two host-process subjects admitted by the initial E02 production slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProcessObservationSubjectKindV1 {
    AuthorityBootstrapOwner,
    ManagedExecution,
}

/// Exact authority and execution context bound to one host-process registration.
///
/// `execution_scope_hash` is opaque to the observer. The Resource Authority and sandbox bind it
/// to the prepared physical attempt before a process can be spawned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessObservationScopeV1 {
    pub authority_epoch: u64,
    pub application_composition_epoch: u64,
    pub execution_scope_hash: CanonicalHash,
}

impl ProcessObservationScopeV1 {
    /// Returns whether the context can describe a live authority epoch.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        self.authority_epoch != 0 && self.application_composition_epoch != 0
    }
}

/// Durable host-process birth fact emitted after a same-factory registration is verified.
///
/// This is serializable inventory data, not a Rust-level capability: deserialization or the
/// public platform-adapter constructor can manufacture a value. Resource Authority therefore
/// never treats the DTO itself as authorization. Effect entrypoints accept only a current
/// verifier's one-shot issuance, and recovery re-observes this subject against OS birth facts.
/// The fact proves the process birth was observed live at registration time; it is not a claim
/// that the process remains live when the DTO is later consumed.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifiedHostProcessIdentityV1 {
    process_id: u32,
    birth_identity_hash: CanonicalHash,
    subject_kind: ProcessObservationSubjectKindV1,
    scope: ProcessObservationScopeV1,
    registration_nonce: String,
    registration_hash: CanonicalHash,
    registration_service_instance_hash: CanonicalHash,
    registration_service_generation: u64,
}

impl std::fmt::Debug for VerifiedHostProcessIdentityV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VerifiedHostProcessIdentityV1")
            .field("process_id", &self.process_id)
            .field("birth_identity_hash", &self.birth_identity_hash)
            .field("subject_kind", &self.subject_kind)
            .field("scope", &self.scope)
            .field("registration_hash", &self.registration_hash)
            .field(
                "registration_service_instance_hash",
                &self.registration_service_instance_hash,
            )
            .field(
                "registration_service_generation",
                &self.registration_service_generation,
            )
            .finish_non_exhaustive()
    }
}

impl VerifiedHostProcessIdentityV1 {
    /// Constructs durable process-fact data for a platform adapter.
    ///
    /// This constructor is not an authority issuance path. Resource Authority receives a
    /// subject only as the result of
    /// [`HostProcessObservationVerifierV1::verify_registration`], whose private issuance record
    /// it verifies before persisting the DTO.
    #[must_use]
    pub fn from_verified_registration(
        process_id: u32,
        birth_identity_hash: CanonicalHash,
        subject_kind: ProcessObservationSubjectKindV1,
        scope: ProcessObservationScopeV1,
        registration_nonce: String,
        registration_hash: CanonicalHash,
        registration_service_instance_hash: CanonicalHash,
        registration_service_generation: u64,
    ) -> Self {
        Self {
            process_id,
            birth_identity_hash,
            subject_kind,
            scope,
            registration_nonce,
            registration_hash,
            registration_service_instance_hash,
            registration_service_generation,
        }
    }

    #[must_use]
    pub const fn process_id(&self) -> u32 {
        self.process_id
    }

    #[must_use]
    pub const fn birth_identity_hash(&self) -> CanonicalHash {
        self.birth_identity_hash
    }

    #[must_use]
    pub const fn subject_kind(&self) -> ProcessObservationSubjectKindV1 {
        self.subject_kind
    }

    #[must_use]
    pub const fn scope(&self) -> &ProcessObservationScopeV1 {
        &self.scope
    }

    #[must_use]
    pub const fn registration_hash(&self) -> CanonicalHash {
        self.registration_hash
    }

    #[must_use]
    pub fn registration_nonce(&self) -> &str {
        &self.registration_nonce
    }

    #[must_use]
    pub const fn registration_service_instance_hash(&self) -> CanonicalHash {
        self.registration_service_instance_hash
    }

    #[must_use]
    pub const fn registration_service_generation(&self) -> u64 {
        self.registration_service_generation
    }

    #[must_use]
    pub fn has_exact_binding(
        &self,
        expected_kind: ProcessObservationSubjectKindV1,
        expected_scope: &ProcessObservationScopeV1,
    ) -> bool {
        self.subject_kind == expected_kind && self.scope == *expected_scope
    }
}

/// One-shot observer output. Its public representation is not trusted by Resource Authority;
/// only the matching verifier's private issuance record makes it acceptable for persistence.
#[derive(Debug)]
pub struct HostProcessIdentityRegistrationV1 {
    identity: VerifiedHostProcessIdentityV1,
    issuance_id: String,
    observed_at_monotonic_ms: u64,
}

impl HostProcessIdentityRegistrationV1 {
    /// Creates observer output that still requires same-factory verification.
    #[must_use]
    pub fn new(
        identity: VerifiedHostProcessIdentityV1,
        issuance_id: String,
        observed_at_monotonic_ms: u64,
    ) -> Self {
        Self {
            identity,
            issuance_id,
            observed_at_monotonic_ms,
        }
    }

    #[must_use]
    pub const fn identity(&self) -> &VerifiedHostProcessIdentityV1 {
        &self.identity
    }

    #[must_use]
    pub fn issuance_id(&self) -> &str {
        &self.issuance_id
    }

    #[must_use]
    pub const fn observed_at_monotonic_ms(&self) -> u64 {
        self.observed_at_monotonic_ms
    }
}

/// One-shot fresh recovery observation. The durable subject is supplied separately by Resource
/// Authority, so a missing PID can never become a proof without that subject and matching
/// verifier issuance.
#[derive(Debug)]
pub struct HostProcessRecoveryObservationV1 {
    subject_registration_hash: CanonicalHash,
    vitality: ProcessVitalityV1,
    issuance_id: String,
    observed_at_monotonic_ms: u64,
}

impl HostProcessRecoveryObservationV1 {
    /// Creates one observer-issued recovery observation pending same-factory verification.
    #[must_use]
    pub fn new(
        subject_registration_hash: CanonicalHash,
        vitality: ProcessVitalityV1,
        issuance_id: String,
        observed_at_monotonic_ms: u64,
    ) -> Self {
        Self {
            subject_registration_hash,
            vitality,
            issuance_id,
            observed_at_monotonic_ms,
        }
    }

    #[must_use]
    pub const fn subject_registration_hash(&self) -> CanonicalHash {
        self.subject_registration_hash
    }

    #[must_use]
    pub const fn vitality(&self) -> ProcessVitalityV1 {
        self.vitality
    }

    #[must_use]
    pub fn issuance_id(&self) -> &str {
        &self.issuance_id
    }

    #[must_use]
    pub const fn observed_at_monotonic_ms(&self) -> u64 {
        self.observed_at_monotonic_ms
    }
}

/// Verified fresh recovery result, issued only by the same-factory verifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedHostProcessRecoveryObservationV1 {
    pub vitality: ProcessVitalityV1,
    pub verifier_instance_hash: CanonicalHash,
    pub verifier_service_generation: u64,
    pub verified_observation_hash: CanonicalHash,
}

/// Self-registration and sandbox child-registration facet.
///
/// `register_current_authority_owner` never accepts a PID. `register_spawned_process` is called
/// only by the physical sandbox spawn path after it owns a concrete child; its result is still
/// useless until the Resource Authority verifies it against the prepared claim's exact scope.
pub trait HostProcessObservationServiceV1: Send + Sync {
    fn register_current_authority_owner(
        &self,
        scope: ProcessObservationScopeV1,
    ) -> Result<HostProcessIdentityRegistrationV1, ProcessObservationErrorV1>;

    fn register_spawned_process(
        &self,
        scope: ProcessObservationScopeV1,
        process_id: u32,
    ) -> Result<HostProcessIdentityRegistrationV1, ProcessObservationErrorV1>;
}

/// Resource-Authority-only low-level re-observation facet.
pub trait HostProcessIdentityRecoveryProbeV1: Send + Sync {
    fn observe_identity_for_authority_recovery(
        &self,
        subject: &VerifiedHostProcessIdentityV1,
        expected_kind: ProcessObservationSubjectKindV1,
        expected_scope: &ProcessObservationScopeV1,
    ) -> Result<HostProcessRecoveryObservationV1, ProcessObservationErrorV1>;
}

/// Verifies same-factory registration and recovery evidence.
pub trait HostProcessObservationVerifierV1: Send + Sync {
    fn verifier_instance_hash(&self) -> CanonicalHash;

    fn verifier_service_generation(&self) -> u64;

    fn verify_registration(
        &self,
        registration: HostProcessIdentityRegistrationV1,
        expected_kind: ProcessObservationSubjectKindV1,
        expected_scope: &ProcessObservationScopeV1,
    ) -> Result<VerifiedHostProcessIdentityV1, ProcessObservationErrorV1>;

    fn verify_recovery_observation(
        &self,
        subject: &VerifiedHostProcessIdentityV1,
        expected_kind: ProcessObservationSubjectKindV1,
        expected_scope: &ProcessObservationScopeV1,
        observation: HostProcessRecoveryObservationV1,
    ) -> Result<VerifiedHostProcessRecoveryObservationV1, ProcessObservationErrorV1>;
}

/// Same-instance three-facet factory. Composition injects the recovery probe into Resource
/// Authority only; runtime and UI surfaces receive neither a probe nor a verifier.
pub trait HostProcessObservationFactoryV1: Send + Sync {
    fn observation_service(&self) -> Arc<dyn HostProcessObservationServiceV1>;
    fn authority_recovery_probe(&self) -> Arc<dyn HostProcessIdentityRecoveryProbeV1>;
    fn observation_verifier(&self) -> Arc<dyn HostProcessObservationVerifierV1>;
}

/// Closed observation error taxonomy. None of these errors may be mapped to Quiescent.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProcessObservationErrorV1 {
    #[error("process is not observable")]
    NotObservable,
    #[error("process birth identity does not match the durable subject")]
    BirthIdentityMismatch,
    #[error("process observation subject kind does not match")]
    SubjectKindMismatch,
    #[error("process observation scope does not match")]
    ScopeMismatch,
    #[error("process observation scope is malformed")]
    MalformedScope,
    #[error("process observation evidence expired")]
    EvidenceExpired,
    #[error("verifier instance hash drifted; evidence rejected")]
    VerifierInstanceDrift,
    #[error("process remains live")]
    StillLive,
}

/// Closed verifier error shared with the kernel capability issuer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CapabilityVerifyErrorV1 {
    #[error("capability verify failed: {0}")]
    VerifyFailed(String),
    #[error("capability verify error carries no recoverable state")]
    NotRecoverable,
}

#[cfg(test)]
#[path = "tests/process_observation_tests.rs"]
mod tests;
