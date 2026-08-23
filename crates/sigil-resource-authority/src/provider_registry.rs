//! RFC-0071: provider registry contract-only scaffold (R71.1).
//!
//! R71.1 freezes the one-shot factory registration boundary: the registry API consumes exactly
//! one factory, constructs/consumes a current-call-bound sealed submission internally and only
//! returns the narrow activated wrapper. sandbox never receives a raw component or terminal
//! facet. Concrete Dormant/Activated lifecycle arrives in R71.3.

use sigil_kernel::resource::CanonicalHash;

/// Narrow runtime-facing wrapper returned by the factory registration path.
#[derive(Debug)]
pub struct ActivatedSandboxRuntimeProviderV1 {
    pub provider_instance_hash: CanonicalHash,
    pub provider_generation: u64,
}

/// Credential for the factory; the authority never re-exports it.
#[derive(Debug)]
pub struct SandboxOneShotFactoryRegistrationV1 {
    pub factory_hash: CanonicalHash,
}

/// Registration outcome carrying only the narrow wrapper.
#[derive(Debug)]
pub struct SandboxRegistrationReceiptV1 {
    pub registration_hash: CanonicalHash,
    pub provider: ActivatedSandboxRuntimeProviderV1,
}

/// Closed registration error classification.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProviderRegistrationErrorV1 {
    #[error("sandbox provider factory registration is contract-only until R71.3")]
    ContractOnly,
    #[error("duplicate provider registration attempt")]
    DuplicateRegistration,
    #[error("factory attestation mismatch")]
    AttestationMismatch,
}
