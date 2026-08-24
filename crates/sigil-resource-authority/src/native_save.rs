//! RFC-0071 R71.7: host-private borrowed native-save authority.
//!
//! The native desktop owns the user-selected destination, but the workspace server is the only
//! writer. The request is a host-private registration capsule: the authority observes the
//! destination parent without following aliases, verifies the content digest, performs a
//! create-new atomic publish, and returns only a closed borrowed receipt.

use std::{collections::BTreeSet, path::PathBuf, sync::Mutex};

use serde::{Deserialize, Serialize};
use sigil_kernel::resource::{
    CanonicalHash, OpaquePermissionSubjectRef, OpaqueRegistrationCapsuleId,
};
use tempfile::Builder;

use crate::{
    borrowed::{BorrowedSubjectClassV1, BorrowedSubjectRegistryV1},
    identity::canonical_identity,
};

pub const BORROWED_NATIVE_SAVE_SCHEMA_VERSION: u16 = 1;
pub const MAX_BORROWED_NATIVE_SAVE_BYTES: usize = 256 * 1024;

/// The closed set of host-private native-save purposes in R71.7.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BorrowedNativeSavePurposeV1 {
    SupportBundle,
}

/// Host-private registration capsule. `destination` is deliberately confined to the native
/// client/server stack; it must never be projected to a renderer, public DTO, or closed receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BorrowedNativeSaveRequestV1 {
    pub schema_version: u16,
    pub purpose: BorrowedNativeSavePurposeV1,
    pub capsule_id: OpaqueRegistrationCapsuleId,
    #[serde(rename = "destination")]
    pub raw_destination: PathBuf,
    pub content: String,
    pub content_hash: CanonicalHash,
}

/// Closed native-save receipt: no path, no authority token, and no mutable writer handle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BorrowedNativeSaveReceiptV1 {
    pub schema_version: u16,
    pub capsule_id: OpaqueRegistrationCapsuleId,
    pub subject_ref: OpaquePermissionSubjectRef,
    pub observation_generation: u64,
    pub content_hash: CanonicalHash,
    pub byte_length: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BorrowedNativeSaveErrorV1 {
    #[error("native save request schema is unsupported")]
    UnsupportedSchema,
    #[error("native save purpose is not admitted")]
    PurposeNotAdmitted,
    #[error("native save content is empty or exceeds the bounded limit")]
    ContentOutOfBounds,
    #[error("native save content digest does not match the registration capsule")]
    ContentDigestMismatch,
    #[error("native save destination must be an absolute path")]
    DestinationNotAbsolute,
    #[error("native save destination must be a regular file name")]
    DestinationInvalid,
    #[error("native save destination or one of its parents is a symlink/reparse point")]
    SymlinkAtBoundary,
    #[error("native save destination already exists; overwrite is not admitted")]
    DestinationOccupied,
    #[error("native save destination parent identity changed during registration")]
    IdentityDrift,
    #[error("native save registration capsule was already consumed")]
    CapsuleReplay,
    #[error("native save filesystem operation failed: {0}")]
    Filesystem(String),
}

/// Transport-neutral authority port used by the runtime and host-private HTTP adapter.
pub trait BorrowedNativeSaveServiceV1: Send + Sync {
    fn save(
        &self,
        request: BorrowedNativeSaveRequestV1,
    ) -> Result<BorrowedNativeSaveReceiptV1, BorrowedNativeSaveErrorV1>;
}

/// Real authority implementation. It shares the borrowed registry with file access so the
/// registration capsule and subsequent host-private write use one observation authority.
pub struct AuthorityBorrowedNativeSaveServiceV1 {
    registry: std::sync::Arc<Mutex<BorrowedSubjectRegistryV1>>,
    consumed_capsules: Mutex<BTreeSet<String>>,
}

impl std::fmt::Debug for AuthorityBorrowedNativeSaveServiceV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthorityBorrowedNativeSaveServiceV1")
            .field("registry", &"shared")
            .finish_non_exhaustive()
    }
}

impl AuthorityBorrowedNativeSaveServiceV1 {
    pub fn new(registry: std::sync::Arc<Mutex<BorrowedSubjectRegistryV1>>) -> Self {
        Self {
            registry,
            consumed_capsules: Mutex::new(BTreeSet::new()),
        }
    }

    fn validate_request(
        request: &BorrowedNativeSaveRequestV1,
    ) -> Result<(), BorrowedNativeSaveErrorV1> {
        if request.schema_version != BORROWED_NATIVE_SAVE_SCHEMA_VERSION {
            return Err(BorrowedNativeSaveErrorV1::UnsupportedSchema);
        }
        if request.purpose != BorrowedNativeSavePurposeV1::SupportBundle {
            return Err(BorrowedNativeSaveErrorV1::PurposeNotAdmitted);
        }
        if request.content.is_empty() || request.content.len() > MAX_BORROWED_NATIVE_SAVE_BYTES {
            return Err(BorrowedNativeSaveErrorV1::ContentOutOfBounds);
        }
        let digest = digest_bytes(request.content.as_bytes());
        if digest != request.content_hash {
            return Err(BorrowedNativeSaveErrorV1::ContentDigestMismatch);
        }
        if !request.raw_destination.is_absolute() {
            return Err(BorrowedNativeSaveErrorV1::DestinationNotAbsolute);
        }
        let Some(file_name) = request
            .raw_destination
            .file_name()
            .and_then(|name| name.to_str())
        else {
            return Err(BorrowedNativeSaveErrorV1::DestinationInvalid);
        };
        if file_name.is_empty()
            || file_name.len() > 160
            || !file_name.starts_with("sigil-support-")
            || !file_name.ends_with(".json")
        {
            return Err(BorrowedNativeSaveErrorV1::DestinationInvalid);
        }
        Ok(())
    }
}

impl BorrowedNativeSaveServiceV1 for AuthorityBorrowedNativeSaveServiceV1 {
    fn save(
        &self,
        request: BorrowedNativeSaveRequestV1,
    ) -> Result<BorrowedNativeSaveReceiptV1, BorrowedNativeSaveErrorV1> {
        Self::validate_request(&request)?;
        {
            let mut consumed = self.consumed_capsules.lock().map_err(|_| {
                BorrowedNativeSaveErrorV1::Filesystem("capsule table poisoned".into())
            })?;
            if !consumed.insert(request.capsule_id.as_str().to_owned()) {
                return Err(BorrowedNativeSaveErrorV1::CapsuleReplay);
            }
        }
        let destination = &request.raw_destination;
        let parent = destination
            .parent()
            .ok_or(BorrowedNativeSaveErrorV1::DestinationInvalid)?;
        reject_symlink_boundaries(parent)?;
        let parent_identity = canonical_identity(parent).map_err(map_identity_error)?;
        if !parent_identity.is_directory || parent_identity.is_symlink {
            return Err(BorrowedNativeSaveErrorV1::SymlinkAtBoundary);
        }

        let subject_ref = OpaquePermissionSubjectRef::new(format!(
            "borrowed-native-save-parent:{}",
            parent_identity.digest.to_hex()
        ));
        let observation_generation = 1;
        {
            let mut registry = self
                .registry
                .lock()
                .map_err(|_| BorrowedNativeSaveErrorV1::Filesystem("registry poisoned".into()))?;
            registry
                .observe_with_identity(
                    &subject_ref,
                    BorrowedSubjectClassV1::ExternalUserPath,
                    observation_generation,
                    Some(parent_identity),
                )
                .map_err(|error| BorrowedNativeSaveErrorV1::Filesystem(error.to_string()))?;
        }

        let current_parent_identity = canonical_identity(parent).map_err(map_identity_error)?;
        if current_parent_identity != parent_identity {
            return Err(BorrowedNativeSaveErrorV1::IdentityDrift);
        }
        if let Ok(metadata) = destination.symlink_metadata() {
            if metadata.file_type().is_symlink() {
                return Err(BorrowedNativeSaveErrorV1::SymlinkAtBoundary);
            }
            return Err(BorrowedNativeSaveErrorV1::DestinationOccupied);
        }

        let mut staged = Builder::new()
            .prefix(".sigil-support-")
            .tempfile_in(parent)
            .map_err(|error| BorrowedNativeSaveErrorV1::Filesystem(error.to_string()))?;
        set_private_file_mode(&staged)?;
        std::io::Write::write_all(&mut staged, request.content.as_bytes())
            .and_then(|()| staged.as_file().sync_all())
            .map_err(|error| BorrowedNativeSaveErrorV1::Filesystem(error.to_string()))?;
        match staged.persist_noclobber(destination) {
            Ok(_) => {}
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(BorrowedNativeSaveErrorV1::DestinationOccupied);
            }
            Err(error) => {
                return Err(BorrowedNativeSaveErrorV1::Filesystem(
                    error.error.to_string(),
                ));
            }
        }
        sync_parent(parent)?;
        Ok(BorrowedNativeSaveReceiptV1 {
            schema_version: BORROWED_NATIVE_SAVE_SCHEMA_VERSION,
            capsule_id: request.capsule_id,
            subject_ref,
            observation_generation,
            content_hash: request.content_hash,
            byte_length: request.content.len() as u64,
        })
    }
}

fn digest_bytes(bytes: &[u8]) -> CanonicalHash {
    use sha2::{Digest, Sha256};
    CanonicalHash::from_bytes(Sha256::digest(bytes).into())
}

fn map_identity_error(error: crate::identity::IdentityErrorV1) -> BorrowedNativeSaveErrorV1 {
    match error {
        crate::identity::IdentityErrorV1::SymlinkAtBoundary => {
            BorrowedNativeSaveErrorV1::SymlinkAtBoundary
        }
        other => BorrowedNativeSaveErrorV1::Filesystem(other.to_string()),
    }
}

fn reject_symlink_boundaries(path: &std::path::Path) -> Result<(), BorrowedNativeSaveErrorV1> {
    let metadata = path
        .symlink_metadata()
        .map_err(|error| BorrowedNativeSaveErrorV1::Filesystem(error.to_string()))?;
    if metadata.file_type().is_symlink() {
        return Err(BorrowedNativeSaveErrorV1::SymlinkAtBoundary);
    }
    Ok(())
}

fn set_private_file_mode(file: &tempfile::NamedTempFile) -> Result<(), BorrowedNativeSaveErrorV1> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|error| BorrowedNativeSaveErrorV1::Filesystem(error.to_string()))?;
    }
    Ok(())
}

fn sync_parent(parent: &std::path::Path) -> Result<(), BorrowedNativeSaveErrorV1> {
    #[cfg(unix)]
    {
        std::fs::File::open(parent)
            .and_then(|file| file.sync_all())
            .map_err(|error| BorrowedNativeSaveErrorV1::Filesystem(error.to_string()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(destination: PathBuf, content: &str) -> BorrowedNativeSaveRequestV1 {
        BorrowedNativeSaveRequestV1 {
            schema_version: BORROWED_NATIVE_SAVE_SCHEMA_VERSION,
            purpose: BorrowedNativeSavePurposeV1::SupportBundle,
            capsule_id: OpaqueRegistrationCapsuleId::new(format!(
                "capsule-{}",
                digest_bytes(content.as_bytes()).to_hex()
            )),
            raw_destination: destination,
            content: content.to_owned(),
            content_hash: digest_bytes(content.as_bytes()),
        }
    }

    #[test]
    fn r71_native_save_writes_real_closed_receipt_and_rejects_overwrite() {
        let temp = tempfile::tempdir().expect("tempdir");
        let registry = std::sync::Arc::new(Mutex::new(BorrowedSubjectRegistryV1::new()));
        let service = AuthorityBorrowedNativeSaveServiceV1::new(registry);
        let destination = temp.path().join("sigil-support-123.json");
        let receipt = service
            .save(request(destination.clone(), "{\"schema_version\":1}"))
            .expect("native save");
        assert_eq!(receipt.byte_length, 20);
        assert_eq!(
            std::fs::read_to_string(&destination).expect("saved"),
            "{\"schema_version\":1}"
        );
        let error = service
            .save(request(destination, "{\"schema_version\":2}"))
            .expect_err("overwrite must be rejected");
        assert_eq!(error, BorrowedNativeSaveErrorV1::DestinationOccupied);
    }

    #[cfg(unix)]
    #[test]
    fn r71_native_save_rejects_symlink_destination() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().expect("tempdir");
        let target = temp.path().join("target.json");
        std::fs::write(&target, "private").expect("target");
        let destination = temp.path().join("sigil-support-123.json");
        symlink(&target, &destination).expect("link");
        let service = AuthorityBorrowedNativeSaveServiceV1::new(std::sync::Arc::new(Mutex::new(
            BorrowedSubjectRegistryV1::new(),
        )));
        let error = service
            .save(request(destination, "{}"))
            .expect_err("symlink must be rejected");
        assert_eq!(error, BorrowedNativeSaveErrorV1::SymlinkAtBoundary);
        assert_eq!(std::fs::read_to_string(target).expect("target"), "private");
    }
}
