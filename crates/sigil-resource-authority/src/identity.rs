//! RFC-0071 section 7.6 / 10.7: canonical local identity and alias containment.
//!
//! Managed generations use the identity captured at creation. Borrowed resources only receive an
//! identity *observation*; the authority never claims ownership, never chmods permanently and
//! never deletes borrowed content. Alias policy: descendant symlink is a leaf entry (no-follow),
//! and a hard link to outside the managed generation is rejected / copy-up contained.

use std::path::Path;

use sigil_kernel::resource::CanonicalHash;

/// Canonical identity digest from no-follow metadata (inode / file identity, size, owner).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CanonicalLocalIdentity {
    pub digest: CanonicalHash,
    pub is_regular_file: bool,
    pub is_directory: bool,
    pub is_symlink: bool,
    pub link_count: u64,
}

/// Closed alias containment classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AliasContainmentClassV1 {
    Contained,
    DescendantSymlinkLeaf,
    ExternalHardLink,
    RejectedExternalAlias,
    OutcomeUncertain,
}

/// Closed identity error classification.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdentityErrorV1 {
    #[error("identity observation failed: {0}")]
    ObservationFailed(String),
    #[error("path is a symlink/reparse point at an authority boundary")]
    SymlinkAtBoundary,
    #[error("hard link escapes the managed generation")]
    HardLinkEscape,
    #[error("cannot observe a borrowed resource identity without admission")]
    BorrowedWithoutAdmission,
}

/// No-follow identity capture for one path.
pub fn canonical_identity(path: &Path) -> Result<CanonicalLocalIdentity, IdentityErrorV1> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| IdentityErrorV1::ObservationFailed(error.to_string()))?;
    #[cfg(unix)]
    let (inode, link_count) = {
        use std::os::unix::fs::MetadataExt;
        (metadata.ino(), metadata.nlink())
    };
    #[cfg(not(unix))]
    let (_inode, link_count) = (0u64, 1u64);
    let mut digest_material = Vec::new();
    digest_material.extend_from_slice(path.to_string_lossy().as_bytes());
    digest_material.extend_from_slice(&inode.to_le_bytes());
    // A directory's link count also changes when a child directory is created. It is a
    // containment boundary, so bind its inode/path identity rather than mutable entry counts.
    let stable_link_count = if metadata.is_dir() { 0 } else { link_count };
    digest_material.extend_from_slice(&stable_link_count.to_le_bytes());
    // Directory byte length is not a stable identity: creating an admitted child changes it on
    // common filesystems while leaving the directory inode and link identity intact. Files still
    // bind their current size so borrowed-file content replacement remains detectable.
    let stable_size = if metadata.is_dir() { 0 } else { metadata.len() };
    digest_material.extend_from_slice(&stable_size.to_le_bytes());
    let digest = identity_digest(&digest_material);
    Ok(CanonicalLocalIdentity {
        digest,
        is_regular_file: metadata.is_file(),
        is_directory: metadata.is_dir(),
        is_symlink: metadata.file_type().is_symlink(),
        link_count: stable_link_count,
    })
}

/// Digest helper for identities.
pub fn identity_digest(payload: &[u8]) -> CanonicalHash {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(payload);
    CanonicalHash::from_bytes(hasher.finalize().into())
}

/// Alias check for one descendant entry: symlinks are leaves, directories are walked no-follow.
pub fn classify_alias(
    ancestor: &Path,
    entry: &Path,
) -> Result<AliasContainmentClassV1, IdentityErrorV1> {
    let identity = canonical_identity(entry)?;
    if identity.is_symlink {
        return Ok(AliasContainmentClassV1::DescendantSymlinkLeaf);
    }
    if identity.link_count > 1 && identity.is_regular_file {
        // Hard link: containment proven only when the linked inode is inside the generation.
        let normalized = entry
            .strip_prefix(ancestor)
            .map_err(|_| IdentityErrorV1::HardLinkEscape)?;
        if normalized
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err(IdentityErrorV1::HardLinkEscape);
        }
        return Ok(AliasContainmentClassV1::ExternalHardLink);
    }
    Ok(AliasContainmentClassV1::Contained)
}

/// Borrowed resource: identity observation only, no ownership claim.
#[derive(Debug, Clone)]
pub struct BorrowedIdentityObservationV1 {
    pub identity: CanonicalLocalIdentity,
    pub observed_generation: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn r71_identity_symlink_is_a_leaf_not_followed() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().expect("tempdir");
        let target = temp.path().join("target");
        std::fs::write(&target, b"data").expect("target");
        let link = temp.path().join("leaf");
        symlink(&target, &link).expect("link");
        let identity = canonical_identity(&link).expect("identity");
        assert!(
            identity.is_symlink,
            "symlink leaf must be identified as link"
        );
        assert_eq!(
            classify_alias(temp.path(), &link).expect("class"),
            AliasContainmentClassV1::DescendantSymlinkLeaf
        );
    }

    #[test]
    fn r71_identity_plain_file_reports_contained() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file = temp.path().join("plain.txt");
        std::fs::write(&file, b"data").expect("file");
        assert_eq!(
            classify_alias(temp.path(), &file).expect("class"),
            AliasContainmentClassV1::Contained
        );
    }

    #[test]
    fn r71_identity_digest_is_stable() {
        assert_eq!(identity_digest(b"x"), identity_digest(b"x"));
    }

    #[test]
    fn r71_directory_identity_survives_admitted_child_creation() {
        let temp = tempfile::tempdir().expect("tempdir");
        let before = canonical_identity(temp.path()).expect("directory identity");
        std::fs::create_dir(temp.path().join("child")).expect("child directory");
        let after = canonical_identity(temp.path()).expect("directory identity after child");
        assert_eq!(before, after, "before={before:?} after={after:?}");
    }
}
