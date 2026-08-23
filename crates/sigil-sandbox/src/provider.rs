//! RFC-0071: sandbox provider contract (R71.1 shape only).
//!
//! R71.3 implements the four-backend family and the one-shot factory that atomically produces
//! binder + physical verifier + same-instance launch supervisor / pending verifier + terminal
//! installer, submitting them through the RA-owned sealer callback.

use sigil_kernel::resource::SandboxBackendClassV1;

/// Stable provider id used in registration evidence.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct SandboxProviderIdV1(String);

impl SandboxProviderIdV1 {
    pub const fn new(value: String) -> Self {
        Self(value)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Closed unavailable classification; provider unavailable must never be treated as a silent
/// Local fallback.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SandboxProviderUnavailableV1 {
    #[error("provider is contract-only until R71.3")]
    ContractOnly,
    #[error("required capability is unsupported on this platform: {0}")]
    CapabilityUnsupported(String),
    #[error("required confinement cannot be proven; refusing implicit Local fallback")]
    ConfinementUnproven,
}

/// One-shot factory: the only entry point a composition moves. Returns the four boxed components
/// through the RA sealer callback; the factory itself is never reconstructed outside sigil-sandbox.
pub struct SandboxProviderFactoryV1 {
    backend_class: SandboxBackendClassV1,
}

impl SandboxProviderFactoryV1 {
    pub fn backend_class(&self) -> SandboxBackendClassV1 {
        self.backend_class
    }
}

/// Placeholder factory backend coverage frozen for R71.1 (probe materializes in R71.3).
pub fn declared_backend_classes() -> Vec<SandboxBackendClassV1> {
    use SandboxBackendClassV1::*;
    vec![
        MacOsSeatbelt,
        LinuxBubblewrap,
        Docker,
        WindowsRestricted,
        LocalUnconfined,
    ]
}
