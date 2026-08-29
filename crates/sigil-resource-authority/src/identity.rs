//! RFC-0071 section 7.6 / 10.7: canonical local identity and alias containment.
//!
//! Managed generations use the identity captured at creation. Borrowed resources only receive an
//! identity *observation*; the authority never claims ownership, never chmods permanently and
//! never deletes borrowed content. Alias policy: descendant symlink is a leaf entry (no-follow),
//! and a hard link to outside the managed generation is rejected / copy-up contained.

use std::{fs::Metadata, path::Path};

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
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(
                windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ
                    | windows_sys::Win32::Storage::FileSystem::FILE_SHARE_WRITE
                    | windows_sys::Win32::Storage::FileSystem::FILE_SHARE_DELETE,
            )
            .custom_flags(
                windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS
                    | windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT,
            )
            .open(path)
            .map_err(|error| IdentityErrorV1::ObservationFailed(error.to_string()))?;
        return canonical_identity_from_handle(path, &file);
    }

    #[cfg(not(windows))]
    {
        let metadata = std::fs::symlink_metadata(path)
            .map_err(|error| IdentityErrorV1::ObservationFailed(error.to_string()))?;
        Ok(canonical_identity_from_metadata(path, &metadata))
    }
}

/// Computes the same identity digest from metadata obtained through an already opened handle.
///
/// The path is only a stable logical label in this function. The object identity comes from the
/// no-follow metadata/file-id fields, so execution can compare a planned path observation with
/// the object actually opened for effect.
pub fn canonical_identity_from_metadata(
    path: &Path,
    metadata: &Metadata,
) -> CanonicalLocalIdentity {
    #[cfg(unix)]
    let (inode, link_count) = {
        use std::os::unix::fs::MetadataExt;
        (metadata.ino(), metadata.nlink())
    };
    // Windows file IDs are not exposed by stable `std::fs::Metadata`; callers that need an
    // authoritative Windows identity must use `canonical_identity_from_handle` below. Keep this
    // metadata-only fallback for generic callers, but deliberately do not present it as a file
    // ID binding.
    #[cfg(windows)]
    let (volume_serial, inode, link_count) = (0u32, 0u64, 1u64);
    #[cfg(not(any(unix, windows)))]
    let (inode, link_count) = (0u64, 1u64);
    let mut digest_material = Vec::new();
    digest_material.extend_from_slice(path.to_string_lossy().as_bytes());
    digest_material.extend_from_slice(&inode.to_le_bytes());
    #[cfg(windows)]
    digest_material.extend_from_slice(&volume_serial.to_le_bytes());
    // A directory's link count also changes when a child directory is created. It is a
    // containment boundary, so bind its inode/path identity rather than mutable entry counts.
    let stable_link_count = if metadata.is_dir() { 0 } else { link_count };
    digest_material.extend_from_slice(&stable_link_count.to_le_bytes());
    // Directory byte length is not a stable identity: creating an admitted child changes it on
    // common filesystems while leaving the directory inode and link identity intact. Files still
    // bind their current size so borrowed-file content replacement remains detectable.
    let stable_size = if metadata.is_dir() { 0 } else { metadata.len() };
    digest_material.extend_from_slice(&stable_size.to_le_bytes());
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        digest_material.extend_from_slice(&metadata.file_attributes().to_le_bytes());
    }
    let digest = identity_digest(&digest_material);
    #[cfg(windows)]
    let is_reparse_point = {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    };
    #[cfg(not(windows))]
    let is_reparse_point = false;
    CanonicalLocalIdentity {
        digest,
        is_regular_file: metadata.is_file(),
        is_directory: metadata.is_dir(),
        is_symlink: metadata.file_type().is_symlink() || is_reparse_point,
        link_count: stable_link_count,
    }
}

/// Captures the authoritative Windows object identity from an already opened handle.
///
/// `std::fs::Metadata` does not expose the Windows volume serial and file index on stable Rust.
/// All Windows plan/execute paths therefore use this helper so a path replacement or hard link
/// cannot masquerade as the approved object.
#[cfg(windows)]
pub fn canonical_identity_from_handle(
    path: &Path,
    file: &std::fs::File,
) -> Result<CanonicalLocalIdentity, IdentityErrorV1> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
        GetFileInformationByHandle,
    };

    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: `file` owns a live handle and `info` is valid writable storage.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle() as _, &raw mut info) } == 0 {
        return Err(IdentityErrorV1::ObservationFailed(
            std::io::Error::last_os_error().to_string(),
        ));
    }
    let is_directory = info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0;
    let is_symlink = info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0;
    let is_regular_file = !is_directory && !is_symlink;
    let link_count = if is_directory {
        0
    } else {
        u64::from(info.nNumberOfLinks)
    };
    // Directory file size is mutable implementation detail on Windows: creating an entry
    // beneath the directory can change it without changing the directory identity. A snapshot
    // binding must therefore use the stable directory sentinel, just like the metadata path.
    let size = if is_directory {
        0
    } else {
        (u64::from(info.nFileSizeHigh) << 32) | u64::from(info.nFileSizeLow)
    };
    let mut digest_material = Vec::new();
    digest_material.extend_from_slice(path.to_string_lossy().as_bytes());
    digest_material.extend_from_slice(&info.dwVolumeSerialNumber.to_le_bytes());
    digest_material.extend_from_slice(&info.nFileIndexHigh.to_le_bytes());
    digest_material.extend_from_slice(&info.nFileIndexLow.to_le_bytes());
    digest_material.extend_from_slice(&link_count.to_le_bytes());
    digest_material.extend_from_slice(&size.to_le_bytes());
    digest_material.extend_from_slice(&info.dwFileAttributes.to_le_bytes());
    Ok(CanonicalLocalIdentity {
        digest: identity_digest(&digest_material),
        is_regular_file,
        is_directory,
        is_symlink,
        link_count,
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
#[path = "tests/identity_tests.rs"]
mod tests;
