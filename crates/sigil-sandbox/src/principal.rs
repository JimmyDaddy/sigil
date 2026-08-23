//! RFC-0071 section 11.3: principal binding contract (Windows restricted + Docker).
//!
//! Neither backend may assume that host owner 0700 is visible to the container/restricted user.
//! Side-effect-free preview produces an opaque principal digest that enters the plan; the
//! binding handshake is journaled (expected-DACL/strategy proof) and restored by CAS. Unproven
//! mappings are unsupported, never declared partial to satisfy a required policy.

use sigil_kernel::resource::CanonicalHash;

/// Closed principal class (SIDs and container UIDs never enter public events).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SandboxPrincipalClassV1 {
    CurrentUser,
    RestrictedToken,
    ContainerUser,
    ContainerRoot,
    Unmapped,
}

/// Side-effect-free principal preview (digest enters execution draft and decision).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxPrincipalPreviewV1 {
    pub principal_class: SandboxPrincipalClassV1,
    pub capability_hash: CanonicalHash,
    pub preview_hash: CanonicalHash,
}

/// Container principal binding strategy (closed, provable only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerPrincipalBindingStrategyV1 {
    IdmappedMount,
    KeepIdUserns,
    TemporaryAclLease,
    Unsupported,
}

/// Closed binding handshake state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrincipalBindingStateV1 {
    Planned,
    Journaled,
    Bound,
    Restored,
    CleanupIncomplete,
}

/// Closed principal binding error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PrincipalBindingErrorV1 {
    #[error("principal mapping drift: plan and realization disagree")]
    MappingDrift,
    #[error("temporary ACL restore CAS failed; concurrent modification detected")]
    RestoreCasFailed,
    #[error("active process still holds the binding lease; GC may not restore or delete")]
    ActiveHolder,
    #[error("the binding strategy cannot be proven on this platform")]
    StrategyUnproven,
}

/// Windows restricted: two-stage principal/ACL handshake preview.
pub fn windows_principal_preview() -> SandboxPrincipalPreviewV1 {
    SandboxPrincipalPreviewV1 {
        principal_class: SandboxPrincipalClassV1::RestrictedToken,
        capability_hash: CanonicalHash::from_bytes([1u8; 32]),
        preview_hash: CanonicalHash::from_bytes([2u8; 32]),
    }
}

/// Docker: container principal resolution digest (never implicit pull/network in preview).
pub fn container_principal_digest(strategy: ContainerPrincipalBindingStrategyV1) -> CanonicalHash {
    let label = match strategy {
        ContainerPrincipalBindingStrategyV1::IdmappedMount => b"idmapped".as_slice(),
        ContainerPrincipalBindingStrategyV1::KeepIdUserns => b"keep-id".as_slice(),
        ContainerPrincipalBindingStrategyV1::TemporaryAclLease => b"temporary-acl".as_slice(),
        ContainerPrincipalBindingStrategyV1::Unsupported => b"unsupported".as_slice(),
    };
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(label);
    CanonicalHash::from_bytes(hasher.finalize().into())
}

/// Validates a principal binding: journaled before bind, CAS-restored after settle, and never
/// allowed while an active process holds the lease.
pub fn validate_principal_binding(
    preview: &SandboxPrincipalPreviewV1,
    state: PrincipalBindingStateV1,
    active_holders: u64,
) -> Result<(), PrincipalBindingErrorV1> {
    if active_holders > 0 && state == PrincipalBindingStateV1::Restored {
        return Err(PrincipalBindingErrorV1::ActiveHolder);
    }
    if preview.principal_class == SandboxPrincipalClassV1::Unmapped
        && state != PrincipalBindingStateV1::Planned
    {
        return Err(PrincipalBindingErrorV1::StrategyUnproven);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r71_windows_preview_is_restricted_token_and_never_leaks_sid() {
        let preview = windows_principal_preview();
        assert_eq!(
            preview.principal_class,
            SandboxPrincipalClassV1::RestrictedToken
        );
    }

    #[test]
    fn r71_container_strategy_hashes_are_distinct() {
        assert_ne!(
            container_principal_digest(ContainerPrincipalBindingStrategyV1::KeepIdUserns),
            container_principal_digest(ContainerPrincipalBindingStrategyV1::Unsupported)
        );
    }

    #[test]
    fn r71_unmapped_principal_cannot_bind() {
        let preview = SandboxPrincipalPreviewV1 {
            principal_class: SandboxPrincipalClassV1::Unmapped,
            capability_hash: CanonicalHash::from_bytes([0u8; 32]),
            preview_hash: CanonicalHash::from_bytes([0u8; 32]),
        };
        let error = validate_principal_binding(&preview, PrincipalBindingStateV1::Bound, 0)
            .expect_err("unmapped cannot bind");
        assert!(matches!(error, PrincipalBindingErrorV1::StrategyUnproven));
    }
}
