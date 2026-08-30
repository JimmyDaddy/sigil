//! RFC-0071 R71.7: authority-owned borrowed configuration mutation.
//!
//! The runtime configuration schema remains a kernel/runtime concern, but the physical root
//! configuration replacement is owned by this authority port. Callers provide a typed snapshot
//! and a one-shot registration capsule; the service observes the borrowed parent, verifies the
//! expected bytes under the update lock, performs the versioned atomic replacement, and returns a
//! closed receipt without a path or reusable writer capability.

use std::{collections::BTreeSet, fs, path::PathBuf, sync::Mutex};

use crate::identity::canonical_identity;
use serde::{Deserialize, Serialize};
use sigil_kernel::resource::{
    CanonicalHash, OpaquePermissionSubjectRef, OpaqueRegistrationCapsuleId,
};
use sigil_kernel::{ConfigUpdateLockGuard, RootConfig};

pub const BORROWED_CONFIGURATION_SCHEMA_VERSION: u16 = 1;
const MAX_BORROWED_CONFIGURATION_BYTES: usize = 512 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BorrowedConfigurationOperationV1 {
    Bootstrap,
    VersionedReplace,
}

/// Host-private configuration registration capsule. The destination is fixed by the service
/// owner and is intentionally absent from this request.
#[derive(Debug, Clone)]
pub struct BorrowedConfigurationRequestV1 {
    pub schema_version: u16,
    pub capsule_id: OpaqueRegistrationCapsuleId,
    pub operation: BorrowedConfigurationOperationV1,
    pub expected_current_hash: Option<CanonicalHash>,
    pub config: RootConfig,
}

/// Closed receipt for one configuration root observation and replacement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BorrowedConfigurationReceiptV1 {
    pub schema_version: u16,
    pub capsule_id: OpaqueRegistrationCapsuleId,
    pub subject_ref: OpaquePermissionSubjectRef,
    pub observation_generation: u64,
    pub operation: BorrowedConfigurationOperationV1,
    pub previous_identity: Option<CanonicalHash>,
    pub committed_identity: CanonicalHash,
    pub previous_version: Option<u64>,
    pub committed_version: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BorrowedConfigurationErrorV1 {
    #[error("borrowed configuration request schema is unsupported")]
    UnsupportedSchema,
    #[error("borrowed configuration capsule was already consumed")]
    CapsuleReplay,
    #[error("borrowed configuration operation does not match the live root")]
    OperationMismatch,
    #[error("borrowed configuration expected-current hash is required")]
    ExpectedCurrentHashMissing,
    #[error("borrowed configuration changed since the registration capsule was issued")]
    IdentityDrift,
    #[error("borrowed configuration root or parent is a symlink/reparse point")]
    SymlinkAtBoundary,
    #[error("borrowed configuration root must be a regular file")]
    RootNotRegular,
    #[error("borrowed configuration payload is empty or exceeds the bounded limit")]
    PayloadOutOfBounds,
    #[error("borrowed configuration filesystem operation failed: {0}")]
    Filesystem(String),
}

/// Transport-neutral owner port. The optional update lock is supplied by the provider-connection
/// transaction so credential and configuration publication retain one cross-process lock.
pub trait BorrowedConfigurationServiceV1: Send + Sync {
    fn publish(
        &self,
        request: BorrowedConfigurationRequestV1,
    ) -> Result<BorrowedConfigurationReceiptV1, BorrowedConfigurationErrorV1>;

    fn publish_with_lock(
        &self,
        request: BorrowedConfigurationRequestV1,
        lock: &ConfigUpdateLockGuard,
    ) -> Result<BorrowedConfigurationReceiptV1, BorrowedConfigurationErrorV1>;
}

/// Real configuration owner for one server boot. It never accepts a caller-selected path.
pub struct AuthorityBorrowedConfigurationServiceV1 {
    config_path: PathBuf,
    consumed_capsules: Mutex<(BTreeSet<String>, u64)>,
}

impl std::fmt::Debug for AuthorityBorrowedConfigurationServiceV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthorityBorrowedConfigurationServiceV1")
            .field("config_path", &"[private]")
            .finish_non_exhaustive()
    }
}

impl AuthorityBorrowedConfigurationServiceV1 {
    pub fn new(config_path: impl Into<PathBuf>) -> Self {
        Self {
            config_path: config_path.into(),
            consumed_capsules: Mutex::new((BTreeSet::new(), 0)),
        }
    }

    fn validate_request(
        request: &BorrowedConfigurationRequestV1,
    ) -> Result<usize, BorrowedConfigurationErrorV1> {
        if request.schema_version != BORROWED_CONFIGURATION_SCHEMA_VERSION {
            return Err(BorrowedConfigurationErrorV1::UnsupportedSchema);
        }
        let rendered = toml::to_string(&request.config)
            .map_err(|error| BorrowedConfigurationErrorV1::Filesystem(error.to_string()))?;
        if rendered.is_empty() || rendered.len() > MAX_BORROWED_CONFIGURATION_BYTES {
            return Err(BorrowedConfigurationErrorV1::PayloadOutOfBounds);
        }
        Ok(rendered.len())
    }

    fn publish_locked(
        &self,
        request: BorrowedConfigurationRequestV1,
        _lock: &ConfigUpdateLockGuard,
    ) -> Result<BorrowedConfigurationReceiptV1, BorrowedConfigurationErrorV1> {
        let _payload_len = Self::validate_request(&request)?;
        let mut consumed = self.consumed_capsules.lock().map_err(|_| {
            BorrowedConfigurationErrorV1::Filesystem("capsule table poisoned".to_owned())
        })?;
        if !consumed.0.insert(request.capsule_id.as_str().to_owned()) {
            return Err(BorrowedConfigurationErrorV1::CapsuleReplay);
        }
        consumed.1 = consumed.1.saturating_add(1);
        let observation_generation = consumed.1;
        drop(consumed);

        let parent = self
            .config_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| std::path::Path::new("."));
        let parent_metadata = fs::symlink_metadata(parent).map_err(map_filesystem)?;
        if parent_metadata.file_type().is_symlink() || !parent_metadata.is_dir() {
            return Err(BorrowedConfigurationErrorV1::SymlinkAtBoundary);
        }
        let parent_identity = canonical_identity(parent).map_err(map_identity)?;
        if parent_identity.is_symlink || !parent_identity.is_directory {
            return Err(BorrowedConfigurationErrorV1::SymlinkAtBoundary);
        }

        let previous_bytes = match fs::symlink_metadata(&self.config_path) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    return Err(BorrowedConfigurationErrorV1::SymlinkAtBoundary);
                }
                if !metadata.is_file() {
                    return Err(BorrowedConfigurationErrorV1::RootNotRegular);
                }
                Some(fs::read(&self.config_path).map_err(map_filesystem)?)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(map_filesystem(error)),
        };
        match (request.operation, previous_bytes.as_ref()) {
            (BorrowedConfigurationOperationV1::Bootstrap, None)
            | (BorrowedConfigurationOperationV1::VersionedReplace, Some(_)) => {}
            _ => return Err(BorrowedConfigurationErrorV1::OperationMismatch),
        }
        let previous_hash = previous_bytes.as_deref().map(digest_bytes);
        match request.operation {
            BorrowedConfigurationOperationV1::Bootstrap => {
                if request.expected_current_hash.is_some() {
                    return Err(BorrowedConfigurationErrorV1::OperationMismatch);
                }
            }
            BorrowedConfigurationOperationV1::VersionedReplace => {
                if request.expected_current_hash != previous_hash {
                    return Err(if request.expected_current_hash.is_none() {
                        BorrowedConfigurationErrorV1::ExpectedCurrentHashMissing
                    } else {
                        BorrowedConfigurationErrorV1::IdentityDrift
                    });
                }
            }
        }
        let previous_identity = previous_bytes
            .as_ref()
            .map(|_| canonical_identity(&self.config_path).map(|identity| identity.digest))
            .transpose()
            .map_err(map_identity)?;

        let current_parent_identity = canonical_identity(parent).map_err(map_identity)?;
        if current_parent_identity != parent_identity {
            return Err(BorrowedConfigurationErrorV1::IdentityDrift);
        }
        request
            .config
            .save_with_update_lock(&self.config_path, _lock)
            .map_err(map_filesystem)?;
        let committed_identity = canonical_identity(&self.config_path).map_err(map_identity)?;
        if committed_identity.is_symlink || !committed_identity.is_regular_file {
            return Err(BorrowedConfigurationErrorV1::SymlinkAtBoundary);
        }
        let subject_ref = OpaquePermissionSubjectRef::new(format!(
            "borrowed-configuration-parent:{}",
            parent_identity.digest.to_hex()
        ));
        Ok(BorrowedConfigurationReceiptV1 {
            schema_version: BORROWED_CONFIGURATION_SCHEMA_VERSION,
            capsule_id: request.capsule_id,
            subject_ref,
            observation_generation,
            operation: request.operation,
            previous_identity,
            committed_identity: committed_identity.digest,
            previous_version: previous_identity.map(|_| observation_generation.saturating_sub(1)),
            committed_version: observation_generation,
        })
    }
}

impl BorrowedConfigurationServiceV1 for AuthorityBorrowedConfigurationServiceV1 {
    fn publish(
        &self,
        request: BorrowedConfigurationRequestV1,
    ) -> Result<BorrowedConfigurationReceiptV1, BorrowedConfigurationErrorV1> {
        let lock = ConfigUpdateLockGuard::acquire(&self.config_path).map_err(map_filesystem)?;
        self.publish_locked(request, &lock)
    }

    fn publish_with_lock(
        &self,
        request: BorrowedConfigurationRequestV1,
        lock: &ConfigUpdateLockGuard,
    ) -> Result<BorrowedConfigurationReceiptV1, BorrowedConfigurationErrorV1> {
        self.publish_locked(request, lock)
    }
}

fn digest_bytes(bytes: &[u8]) -> CanonicalHash {
    use sha2::{Digest, Sha256};
    CanonicalHash::from_bytes(Sha256::digest(bytes).into())
}

fn map_filesystem(error: impl std::fmt::Display) -> BorrowedConfigurationErrorV1 {
    BorrowedConfigurationErrorV1::Filesystem(error.to_string())
}

fn map_identity(error: crate::identity::IdentityErrorV1) -> BorrowedConfigurationErrorV1 {
    match error {
        crate::identity::IdentityErrorV1::SymlinkAtBoundary => {
            BorrowedConfigurationErrorV1::SymlinkAtBoundary
        }
        other => BorrowedConfigurationErrorV1::Filesystem(other.to_string()),
    }
}

#[cfg(test)]
#[path = "tests/configuration_tests.rs"]
mod tests;
