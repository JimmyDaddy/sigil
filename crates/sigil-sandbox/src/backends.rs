//! RFC-0071 section 11.3: closed backend capability declarations.
//!
//! Backend mapping is a closed matrix: exact resource mapping, what must be proven, and truthful
//! platform support state (`supported` / `unsupported` / `diagnostic-only`). A compiler-pass or
//! an ignored test is never support evidence.

use sigil_kernel::resource::{CanonicalHash, EnforcementCompletenessV1, SandboxBackendClassV1};

use crate::receipt::{ReadIsolationCompletenessV1, SandboxPlatformSupportV1};

/// Closed backend capability flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackendCapabilityFlagsV1 {
    pub filesystem_isolation: bool,
    pub network_deny: bool,
    pub process_tree_ownership: bool,
    pub denied_ambient_read: bool,
    pub declares_required_capabilities: bool,
}

/// Closed backend declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxBackendDeclarationV1 {
    pub backend_class: SandboxBackendClassV1,
    pub platform_support: SandboxPlatformSupportV1,
    pub capabilities: BackendCapabilityFlagsV1,
    pub profile_hash: CanonicalHash,
    pub manifest_hash: CanonicalHash,
    pub default_read_isolation: ReadIsolationCompletenessV1,
    pub enforcement: EnforcementCompletenessV1,
}

impl SandboxBackendDeclarationV1 {
    /// Closed platform-default matrix (per current platform family).
    pub fn for_backend(backend: SandboxBackendClassV1, platform_is_primary: bool) -> Self {
        use SandboxBackendClassV1::*;
        let (support, fs, net, tree, read, enforcement) = match backend {
            MacOsSeatbelt => {
                if platform_is_primary {
                    (
                        SandboxPlatformSupportV1::Supported,
                        true,
                        true,
                        true,
                        true,
                        EnforcementCompletenessV1::Exact,
                    )
                } else {
                    (
                        SandboxPlatformSupportV1::Unsupported,
                        false,
                        false,
                        false,
                        false,
                        EnforcementCompletenessV1::None,
                    )
                }
            }
            LinuxBubblewrap => {
                if platform_is_primary {
                    (
                        SandboxPlatformSupportV1::Supported,
                        true,
                        true,
                        true,
                        true,
                        EnforcementCompletenessV1::Exact,
                    )
                } else {
                    (
                        SandboxPlatformSupportV1::Unsupported,
                        false,
                        false,
                        false,
                        false,
                        EnforcementCompletenessV1::None,
                    )
                }
            }
            Docker => (
                SandboxPlatformSupportV1::Supported,
                true,
                true,
                true,
                true,
                EnforcementCompletenessV1::Exact,
            ),
            WindowsRestricted => {
                if platform_is_primary {
                    (
                        SandboxPlatformSupportV1::Supported,
                        true,
                        true,
                        true,
                        true,
                        EnforcementCompletenessV1::Exact,
                    )
                } else {
                    (
                        SandboxPlatformSupportV1::Unsupported,
                        false,
                        false,
                        false,
                        false,
                        EnforcementCompletenessV1::None,
                    )
                }
            }
            LocalUnconfined => (
                SandboxPlatformSupportV1::Supported,
                false,
                false,
                false,
                false,
                EnforcementCompletenessV1::None,
            ),
        };
        Self {
            backend_class: backend,
            platform_support: support,
            capabilities: BackendCapabilityFlagsV1 {
                filesystem_isolation: fs,
                network_deny: net,
                process_tree_ownership: tree,
                denied_ambient_read: read,
                declares_required_capabilities: true,
            },
            profile_hash: CanonicalHash::from_bytes([
                backend_discriminant(backend),
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
            ]),
            manifest_hash: CanonicalHash::from_bytes([
                backend_discriminant(backend),
                1u8,
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
                0u8,
            ]),
            default_read_isolation: if read {
                ReadIsolationCompletenessV1::Full
            } else {
                ReadIsolationCompletenessV1::None
            },
            enforcement,
        }
    }

    /// True when the backend can serve a required-exact filesystem request.
    pub fn can_enforce_required_exact(&self) -> bool {
        self.platform_support == SandboxPlatformSupportV1::Supported
            && self.enforcement == EnforcementCompletenessV1::Exact
            && self.capabilities.filesystem_isolation
    }
}

const fn backend_discriminant(backend: SandboxBackendClassV1) -> u8 {
    match backend {
        SandboxBackendClassV1::MacOsSeatbelt => 1,
        SandboxBackendClassV1::LinuxBubblewrap => 2,
        SandboxBackendClassV1::Docker => 3,
        SandboxBackendClassV1::WindowsRestricted => 4,
        SandboxBackendClassV1::LocalUnconfined => 5,
    }
}

#[cfg(test)]
#[path = "tests/backends_tests.rs"]
mod tests;
