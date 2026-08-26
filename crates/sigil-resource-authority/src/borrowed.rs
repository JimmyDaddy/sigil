//! RFC-0071 section 7.6: borrowed resource identity/admission lease contract.
//!
//! Borrowed resources (Workspace / ToolchainStore / UserConfig / ExternalUserPath / SystemTemp)
//! receive canonical identity observation and approved-access handle leases only. The authority
//! never deletes, never permanently chmods, and never claims ownership of borrowed content. A
//! borrowed generation is one identity observation generation, NOT a right to reclaim.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use sigil_kernel::resource::{
    AuthorityGeneration, CanonicalHash, OpaquePermissionSubjectRef, ResourceAccessV1,
    ResourceCleanupStatusV1,
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
    #[error("workspace registration failed: {0}")]
    RegistrationFailed(String),
}

/// Current-schema workspace activation capsule. It contains only opaque identity facts; the
/// physical root is retained by the authority registry and is never exposed to kernel callers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BorrowedWorkspaceRegistrationCapsuleV1 {
    pub application_id: String,
    pub workspace_id: String,
    pub subject_ref: OpaquePermissionSubjectRef,
    pub authority_generation: AuthorityGeneration,
    pub root_identity_hash: CanonicalHash,
    pub observation_version: u64,
}

#[derive(Debug, Clone)]
struct WorkspaceBindingV1 {
    root: PathBuf,
    root_identity: CanonicalLocalIdentity,
    capsule: BorrowedWorkspaceRegistrationCapsuleV1,
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
    workspace_bindings: BTreeMap<String, WorkspaceBindingV1>,
}

impl BorrowedSubjectRegistryV1 {
    pub const fn new() -> Self {
        Self {
            observations: BTreeMap::new(),
            workspace_bindings: BTreeMap::new(),
        }
    }

    /// Activates one exact workspace identity for the application composition. Repeating the
    /// same activation is idempotent; changing the root behind an existing subject is a typed
    /// identity drift and never silently replaces the binding.
    pub fn activate_workspace(
        &mut self,
        application_id: impl Into<String>,
        workspace_id: impl Into<String>,
        workspace_root: &Path,
        authority_generation: AuthorityGeneration,
    ) -> Result<BorrowedWorkspaceRegistrationCapsuleV1, BorrowedErrorV1> {
        let application_id = application_id.into();
        let workspace_id = workspace_id.into();
        let root_metadata = std::fs::symlink_metadata(workspace_root)
            .map_err(|error| BorrowedErrorV1::RegistrationFailed(error.to_string()))?;
        if root_metadata.file_type().is_symlink() {
            return Err(BorrowedErrorV1::SymlinkAtBoundary);
        }
        let root = workspace_root
            .canonicalize()
            .map_err(|error| BorrowedErrorV1::RegistrationFailed(error.to_string()))?;
        let root_identity = crate::identity::canonical_identity(&root)
            .map_err(|error| BorrowedErrorV1::RegistrationFailed(error.to_string()))?;
        if !root_identity.is_directory || root_identity.is_symlink {
            return Err(BorrowedErrorV1::SymlinkAtBoundary);
        }
        let mut key_material = Vec::new();
        key_material.extend_from_slice(application_id.as_bytes());
        key_material.push(0);
        key_material.extend_from_slice(workspace_id.as_bytes());
        key_material.push(0);
        let subject_digest = crate::identity::identity_digest(&key_material);
        let subject_ref = OpaquePermissionSubjectRef::new(format!(
            "borrowed-workspace-{}",
            subject_digest.to_hex()
        ));
        let key = subject_ref.as_str().to_owned();
        if let Some(existing) = self.workspace_bindings.get(&key) {
            if existing.root_identity != root_identity
                || existing.authority_generation() != authority_generation
            {
                return Err(BorrowedErrorV1::IdentityDrift);
            }
            return Ok(existing.capsule.clone());
        }
        let observation_version = self
            .workspace_bindings
            .values()
            .map(|binding| binding.capsule.observation_version)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        let capsule = BorrowedWorkspaceRegistrationCapsuleV1 {
            application_id: application_id.clone(),
            workspace_id: workspace_id.clone(),
            subject_ref: subject_ref.clone(),
            authority_generation,
            root_identity_hash: root_identity.digest,
            observation_version,
        };
        self.observe_with_identity(
            &subject_ref,
            BorrowedSubjectClassV1::Workspace,
            authority_generation.epoch,
            Some(root_identity),
        )?;
        self.workspace_bindings.insert(
            key,
            WorkspaceBindingV1 {
                root,
                root_identity,
                capsule: capsule.clone(),
            },
        );
        Ok(capsule)
    }

    pub fn workspace_root_for(&self, subject_ref: &OpaquePermissionSubjectRef) -> Option<&Path> {
        self.workspace_bindings
            .get(subject_ref.as_str())
            .map(|binding| binding.root.as_path())
    }

    pub fn workspace_capsule_for(
        &self,
        subject_ref: &OpaquePermissionSubjectRef,
    ) -> Option<&BorrowedWorkspaceRegistrationCapsuleV1> {
        self.workspace_bindings
            .get(subject_ref.as_str())
            .map(|binding| &binding.capsule)
    }

    /// Returns the single workspace active in one application composition. Multiple workspace
    /// activations must use a higher-level surface registry and are rejected by this closed V1
    /// planner rather than guessed.
    pub fn sole_workspace_subject(&self) -> Option<OpaquePermissionSubjectRef> {
        (self.workspace_bindings.len() == 1)
            .then(|| self.workspace_bindings.keys().next().cloned())
            .flatten()
            .map(OpaquePermissionSubjectRef::new)
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

impl WorkspaceBindingV1 {
    fn authority_generation(&self) -> AuthorityGeneration {
        self.capsule.authority_generation
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
    fn r71_workspace_activation_is_idempotent_and_detects_root_drift() {
        let first = tempfile::tempdir().expect("first workspace");
        let second = tempfile::tempdir().expect("second workspace");
        let generation = AuthorityGeneration {
            epoch: 3,
            instance_hash: CanonicalHash::from_bytes([7; 32]),
        };
        let mut registry = BorrowedSubjectRegistryV1::new();
        let capsule = registry
            .activate_workspace("app", "workspace", first.path(), generation)
            .expect("activate");
        let repeated = registry
            .activate_workspace("app", "workspace", first.path(), generation)
            .expect("repeat activation");
        assert_eq!(repeated, capsule);
        let error = registry
            .activate_workspace("app", "workspace", second.path(), generation)
            .expect_err("root replacement must drift");
        assert!(matches!(error, BorrowedErrorV1::IdentityDrift));
    }

    #[cfg(unix)]
    #[test]
    fn r71_workspace_activation_rejects_symlink_root() {
        let workspace = tempfile::tempdir().expect("workspace");
        let parent = tempfile::tempdir().expect("parent");
        let link = parent.path().join("workspace-link");
        std::os::unix::fs::symlink(workspace.path(), &link).expect("symlink");
        let generation = AuthorityGeneration {
            epoch: 1,
            instance_hash: CanonicalHash::from_bytes([8; 32]),
        };
        let error = BorrowedSubjectRegistryV1::new()
            .activate_workspace("app", "workspace", &link, generation)
            .expect_err("symlink root must fail closed");
        assert!(matches!(error, BorrowedErrorV1::SymlinkAtBoundary));
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
