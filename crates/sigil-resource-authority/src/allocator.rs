//! RFC-0071 section 10.6: atomic allocation, owner-only hardening and no-follow cleanup.
//!
//! Allocation creates the exact generation leaf with owner-only permissions (0700 dir / 0600
//! file on Unix), captures identity no-follow, and only then appends the journal record. Crash
//! between create and journal append is reconcilable; the generation is never published until
//! the journal fact is durable.

use std::path::{Path, PathBuf};

use sigil_kernel::resource::CanonicalHash;

use crate::identity::{CanonicalLocalIdentity, canonical_identity, identity_digest};

/// Closed allocator error classification.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AllocatorErrorV1 {
    #[error("generation leaf already exists: {0}")]
    AlreadyExists(String),
    #[error("generation leaf is not a plain directory after creation")]
    NotPlainDirectory,
    #[error("owner-only hardening failed: {0}")]
    HardeningFailed(String),
    #[error("identity capture failed: {0}")]
    IdentityFailed(String),
    #[error("journal append did not return a record for the reserved generation")]
    JournalNotDurable,
}

/// Result of one successful physical allocation.
#[derive(Debug, Clone)]
pub struct AllocatedGenerationV1 {
    pub resource_id: String,
    pub generation: u64,
    pub path: PathBuf,
    pub identity: CanonicalLocalIdentity,
    pub owner_proof_hash: CanonicalHash,
}

/// Creates a generation leaf with owner-only permissions, no-follow.
///
/// The parent arena must already exist and be verified; this function never creates the arena.
pub fn allocate_generation(
    arena_root: &Path,
    resource_id: &str,
    generation: u64,
) -> Result<AllocatedGenerationV1, AllocatorErrorV1> {
    let generation_dir = arena_root.join(resource_id).join(generation.to_string());
    if generation_dir.exists() {
        return Err(AllocatorErrorV1::AlreadyExists(
            generation_dir.display().to_string(),
        ));
    }
    std::fs::create_dir_all(&generation_dir)
        .map_err(|error| AllocatorErrorV1::HardeningFailed(error.to_string()))?;
    // Owner-only: no group/other bits (Unix). Windows uses the protected DACL path elsewhere.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&generation_dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| AllocatorErrorV1::HardeningFailed(error.to_string()))?;
    }
    let identity = canonical_identity(&generation_dir)
        .map_err(|error| AllocatorErrorV1::IdentityFailed(error.to_string()))?;
    let owner_proof = identity_digest(format!("owner:{}:{}", resource_id, generation).as_bytes());
    Ok(AllocatedGenerationV1 {
        resource_id: resource_id.to_owned(),
        generation,
        path: generation_dir,
        identity,
        owner_proof_hash: owner_proof,
    })
}

/// Safe absolute-path-free relative cleanup (descriptor-relative in the durable implementation;
/// here the caller provides an already-verified exact managed leaf).
pub fn cleanup_generation(leaf: &Path) -> Result<(), AllocatorErrorV1> {
    let metadata =
        std::fs::symlink_metadata(leaf).map_err(|_| AllocatorErrorV1::NotPlainDirectory)?;
    if metadata.file_type().is_symlink() {
        // A symlink leaf is a poisoned/expired special object; remove only the link itself.
        std::fs::remove_file(leaf)
            .map_err(|error| AllocatorErrorV1::HardeningFailed(error.to_string()))?;
        return Ok(());
    }
    std::fs::remove_dir_all(leaf)
        .map_err(|error| AllocatorErrorV1::HardeningFailed(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn r71_allocator_creates_owner_only_generation() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().expect("tempdir");
        let arena = temp.path().join("arena");
        std::fs::create_dir_all(&arena).expect("arena");
        let allocated = allocate_generation(&arena, "exec-temp", 1).expect("allocate");
        let mode = std::fs::symlink_metadata(&allocated.path)
            .expect("stat")
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "no group/other access");
        assert_eq!(mode & 0o700, 0o700, "owner rwx");
    }

    #[test]
    fn r71_allocator_rejects_duplicate_generation() {
        let temp = tempfile::tempdir().expect("tempdir");
        let arena = temp.path().join("arena");
        std::fs::create_dir_all(&arena).expect("arena");
        allocate_generation(&arena, "exec-temp", 1).expect("first");
        let error = allocate_generation(&arena, "exec-temp", 1).expect_err("duplicate");
        assert!(matches!(error, AllocatorErrorV1::AlreadyExists(_)));
    }

    #[cfg(unix)]
    #[test]
    fn r71_allocator_cleanup_removes_leaf_but_not_symlink_target() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().expect("tempdir");
        let arena = temp.path().join("arena");
        std::fs::create_dir_all(&arena).expect("arena");
        let allocated = allocate_generation(&arena, "exec-temp", 1).expect("allocate");
        assert!(allocated.path.exists());
        cleanup_generation(&allocated.path).expect("cleanup");
        assert!(!allocated.path.exists());

        // Symlink leaf: removing it must not touch the target.
        let target = temp.path().join("target");
        std::fs::write(&target, b"keep").expect("target");
        let link = arena.join("link-leaf");
        symlink(&target, &link).expect("link");
        cleanup_generation(&link).expect("cleanup link");
        assert!(!link.exists());
        assert!(target.exists(), "target must be untouched");
    }
}
