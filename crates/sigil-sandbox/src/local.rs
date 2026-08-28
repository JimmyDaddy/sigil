//! RFC-0071 section 11.4: Local provider.
//!
//! Local performs no filesystem confinement and may only run when the policy is explicitly
//! unconfined (danger-full-access). It never fabricates effective enforcement and never accepts
//! a required-exact request.

use sigil_kernel::resource::{CanonicalHash, EnforcementCompletenessV1, SandboxBackendClassV1};

use crate::receipt::{EnforcementVerificationErrorV1, SandboxPlatformSupportV1};

/// Closed local spawn plan classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalRunPolicyV1 {
    ExplicitUnconfined,
    RequiredConfinement,
}

/// Local provider descriptor: always truthful none, only under ExplicitUnconfined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalProviderDescriptorV1 {
    pub backend_class: SandboxBackendClassV1,
    pub platform_support: SandboxPlatformSupportV1,
    pub enforcement: EnforcementCompletenessV1,
}

impl Default for LocalProviderDescriptorV1 {
    fn default() -> Self {
        Self {
            backend_class: SandboxBackendClassV1::LocalUnconfined,
            platform_support: SandboxPlatformSupportV1::Supported,
            enforcement: EnforcementCompletenessV1::None,
        }
    }
}

/// Guards a Local execution request: required confinement is rejected before spawn.
pub fn local_confinement_guard(
    request_class: LocalRunPolicyV1,
) -> Result<LocalProviderDescriptorV1, EnforcementVerificationErrorV1> {
    match request_class {
        LocalRunPolicyV1::ExplicitUnconfined => Ok(LocalProviderDescriptorV1::default()),
        LocalRunPolicyV1::RequiredConfinement => {
            Err(EnforcementVerificationErrorV1::LocalRequiresUnconfined)
        }
    }
}

/// Local effective enforcement is always none (never a requested-set clone).
pub fn local_effective_enforcement(
    descriptor: &LocalProviderDescriptorV1,
) -> EnforcementCompletenessV1 {
    descriptor.enforcement
}

/// Local bind evidence: no root bindings exist, so the set is empty but must be present in the
/// per-resource receipt as an explicit observation.
pub fn local_bind_evidence() -> Vec<CanonicalHash> {
    Vec::new()
}

#[cfg(test)]
#[path = "tests/local_tests.rs"]
mod tests;
