//! RFC-0071 section 7.6: borrowed resource identity/admission lease contract.
//!
//! Borrowed resources (Workspace / ToolchainStore / UserConfig / ExternalUserPath / SystemTemp)
//! receive canonical identity observation and approved-access handle leases only. The authority
//! never deletes, never permanently chmods, and never claims ownership of borrowed content. A
//! borrowed generation is one identity observation generation, NOT a right to reclaim.

use std::collections::BTreeMap;

use sigil_kernel::resource::{
    OpaquePermissionSubjectRef, ResourceAccessV1, ResourceCleanupStatusV1,
};

use crate::identity::CanonicalLocalIdentity;

/// Closed borrowed subject class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BorrowedSubjectClassV1 {
    Workspace,
    ToolchainStore,
    UserConfig,
    ExternalUserPath,
    SystemTemp,
}

impl BorrowedSubjectClassV1 {
    /// Closed mapping: subject identity observation only; never a reclaim right.
    pub const fn permits_ownership_claim(self) -> bool {
        false
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::ToolchainStore => "toolchain-store",
            Self::UserConfig => "user-config",
            Self::ExternalUserPath => "external-user-path",
            Self::SystemTemp => "system-temp",
        }
    }
}

/// Closed borrowed error classification.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BorrowedErrorV1 {
    #[error("borrowed subject has no admission for this access")]
    NoAdmission,
    #[error("borrowed subject identity drifted between observation and access")]
    IdentityDrift,
    #[error("borrowed subject is a symlink/reparse point; authority refuses to mutate")]
    SymlinkAtBoundary,
    #[error("SystemTemp is a deny/read-boundary fact; it is never a writable grant")]
    SystemTempWriteDenied,
    #[error("an authority must never claim ownership of a borrowed resource")]
    OwnershipClaimRejected,
    #[error("approval is required for external user paths; subject is unregistered")]
    ExternalSubjectUnregistered,
}

/// One borrowed identity observation generation.
#[derive(Debug, Clone)]
pub struct BorrowedIdentityGenerationV1 {
    pub subject_class: BorrowedSubjectClassV1,
    pub subject_ref: OpaquePermissionSubjectRef,
    pub observation_generation: u64,
    pub identity: CanonicalLocalIdentity,
    pub admitted_access: std::collections::BTreeSet<ResourceAccessV1>,
}

/// Borrowed access handle lease: a borrowing contract, never ownership.
#[derive(Debug)]
pub struct BorrowedAccessLeaseV1 {
    pub subject_ref: OpaquePermissionSubjectRef,
    pub observation_generation: u64,
    pub granted_access: std::collections::BTreeSet<ResourceAccessV1>,
    pub cleanup_status: ResourceCleanupStatusV1,
}

/// Authority-private borrowed subject registry (generation + observed identity per subject ref).
#[derive(Debug, Default)]
pub struct BorrowedSubjectRegistryV1 {
    observations: BTreeMap<String, (BorrowedSubjectClassV1, u64, Option<CanonicalLocalIdentity>)>,
}

impl BorrowedSubjectRegistryV1 {
    pub const fn new() -> Self {
        Self {
            observations: BTreeMap::new(),
        }
    }

    /// Registers an identity observation for a borrowed subject.
    pub fn observe(
        &mut self,
        subject_ref: &OpaquePermissionSubjectRef,
        class: BorrowedSubjectClassV1,
        generation: u64,
    ) -> Result<(), BorrowedErrorV1> {
        self.observe_with_identity(subject_ref, class, generation, None)
    }

    /// Registers an identity observation with the observed canonical identity (R71.6
    /// adjudication evidence: identity_before in the access receipt).
    pub fn observe_with_identity(
        &mut self,
        subject_ref: &OpaquePermissionSubjectRef,
        class: BorrowedSubjectClassV1,
        generation: u64,
        identity: Option<CanonicalLocalIdentity>,
    ) -> Result<(), BorrowedErrorV1> {
        let key = subject_ref.as_str().to_owned();
        if self
            .observations
            .get(&key)
            .is_some_and(|(existing, _, _)| *existing != class)
        {
            return Err(BorrowedErrorV1::NoAdmission);
        }
        self.observations.insert(key, (class, generation, identity));
        Ok(())
    }

    pub fn generation_for(&self, subject_ref: &OpaquePermissionSubjectRef) -> Option<u64> {
        self.observations
            .get(subject_ref.as_str())
            .map(|(_, g, _)| *g)
    }

    pub fn class_for(
        &self,
        subject_ref: &OpaquePermissionSubjectRef,
    ) -> Option<BorrowedSubjectClassV1> {
        self.observations
            .get(subject_ref.as_str())
            .map(|(c, _, _)| *c)
    }

    pub fn identity_for(
        &self,
        subject_ref: &OpaquePermissionSubjectRef,
    ) -> Option<&CanonicalLocalIdentity> {
        self.observations
            .get(subject_ref.as_str())
            .and_then(|(_, _, identity)| identity.as_ref())
    }
}

/// SystemTemp deny/read-boundary fact (never a writable grant).
pub fn system_temp_read_boundary_fact() -> String {
    "system-temp: read-boundary only; no write/create/delete grant in V1".to_owned()
}

/// Validates a borrowed generation: identity must match observation and access must be admitted.
pub fn validate_borrowed_access(
    generation: &BorrowedIdentityGenerationV1,
    requested: ResourceAccessV1,
    observed_identity: &CanonicalLocalIdentity,
    allow_system_temp_write: bool,
) -> Result<BorrowedAccessLeaseV1, BorrowedErrorV1> {
    if generation.identity != *observed_identity {
        return Err(BorrowedErrorV1::IdentityDrift);
    }
    if !generation.admitted_access.contains(&requested) {
        return Err(BorrowedErrorV1::NoAdmission);
    }
    if generation.subject_class == BorrowedSubjectClassV1::SystemTemp
        && matches!(
            requested,
            ResourceAccessV1::Write
                | ResourceAccessV1::Create
                | ResourceAccessV1::DeleteManaged
                | ResourceAccessV1::DeleteExactSubject
                | ResourceAccessV1::DeleteSubjectSubtree
        )
        && !allow_system_temp_write
    {
        return Err(BorrowedErrorV1::SystemTempWriteDenied);
    }
    Ok(BorrowedAccessLeaseV1 {
        subject_ref: generation.subject_ref.clone(),
        observation_generation: generation.observation_generation,
        granted_access: generation.admitted_access.clone(),
        cleanup_status: ResourceCleanupStatusV1::NotApplicableBorrowed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::canonical_identity;
    use std::collections::BTreeSet;

    #[test]
    fn r71_borrowed_observation_generation_is_per_subject() {
        let mut registry = BorrowedSubjectRegistryV1::new();
        let subject = OpaquePermissionSubjectRef::new("workspace:root".to_owned());
        registry
            .observe(&subject, BorrowedSubjectClassV1::Workspace, 1)
            .expect("observe");
        registry
            .observe(&subject, BorrowedSubjectClassV1::Workspace, 2)
            .expect("re-observe");
        assert_eq!(registry.generation_for(&subject), Some(2));
        // Cross-class re-observation fails closed.
        let error = registry
            .observe(&subject, BorrowedSubjectClassV1::ExternalUserPath, 3)
            .expect_err("cross-class");
        assert!(matches!(error, BorrowedErrorV1::NoAdmission));
    }

    #[test]
    fn r71_borrowed_identity_drift_is_rejected() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file = temp.path().join("existing.txt");
        std::fs::write(&file, b"one").expect("write");
        let identity = canonical_identity(&file).expect("identity");
        let generation = BorrowedIdentityGenerationV1 {
            subject_class: BorrowedSubjectClassV1::Workspace,
            subject_ref: OpaquePermissionSubjectRef::new("subject-1".to_owned()),
            observation_generation: 1,
            identity,
            admitted_access: BTreeSet::from([ResourceAccessV1::Read]),
        };
        // Mutate the file; the observed identity no longer matches.
        std::fs::write(&file, b"two-two-totally-different").expect("rewrite");
        let mutated = canonical_identity(&file).expect("identity 2");
        let error = validate_borrowed_access(&generation, ResourceAccessV1::Read, &mutated, false)
            .expect_err("drift must fail");
        assert!(matches!(error, BorrowedErrorV1::IdentityDrift));
    }

    #[test]
    fn r71_system_temp_write_is_denied_unless_approved() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path();
        let identity = canonical_identity(dir).expect("identity");
        let generation = BorrowedIdentityGenerationV1 {
            subject_class: BorrowedSubjectClassV1::SystemTemp,
            subject_ref: OpaquePermissionSubjectRef::new("system-temp".to_owned()),
            observation_generation: 1,
            identity,
            admitted_access: BTreeSet::from([ResourceAccessV1::Write]),
        };
        let identity = canonical_identity(dir).expect("identity 2");
        let error =
            validate_borrowed_access(&generation, ResourceAccessV1::Write, &identity, false)
                .expect_err("default deny write");
        assert!(matches!(error, BorrowedErrorV1::SystemTempWriteDenied));
        // Explicit unconfined approval permits the write grant.
        let lease = validate_borrowed_access(&generation, ResourceAccessV1::Write, &identity, true)
            .expect("explicit allow");
        assert_eq!(
            lease.cleanup_status,
            ResourceCleanupStatusV1::NotApplicableBorrowed
        );
    }
}
