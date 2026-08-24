//! RFC-0071 R71.7: bounded release-output owner.
//!
//! Release tools write into an owner-selected, nonshipping output root. The owner fixes that
//! root, consumes one registration capsule per attempt, refuses aliases and overwrite, and
//! returns a closed receipt. A tree attempt never scans or adopts an existing root; if it fails
//! after creating entries the error carries only the exact partial frontier.

use std::{collections::BTreeSet, fs, path::PathBuf, sync::Mutex};

use sigil_kernel::resource::{
    CanonicalHash, OpaquePermissionSubjectRef, OpaqueRegistrationCapsuleId,
};
use tempfile::Builder;

pub const BORROWED_RELEASE_OUTPUT_SCHEMA_VERSION: u16 = 1;
pub const MAX_BORROWED_RELEASE_FILE_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_BORROWED_RELEASE_TREE_ENTRIES: usize = 2_048;
pub const MAX_BORROWED_RELEASE_TREE_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BorrowedReleaseOutputOperationV1 {
    File,
    Tree,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BorrowedReleaseOutputEntryV1 {
    pub relative_path: PathBuf,
    pub content: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct BorrowedReleaseOutputRequestV1 {
    pub schema_version: u16,
    pub capsule_id: OpaqueRegistrationCapsuleId,
    pub operation: BorrowedReleaseOutputOperationV1,
    pub destination: PathBuf,
    pub content: Vec<u8>,
    pub entries: Vec<BorrowedReleaseOutputEntryV1>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BorrowedReleaseOutputReceiptV1 {
    pub schema_version: u16,
    pub capsule_id: OpaqueRegistrationCapsuleId,
    pub subject_ref: OpaquePermissionSubjectRef,
    pub observation_generation: u64,
    pub operation: BorrowedReleaseOutputOperationV1,
    pub content_digest: CanonicalHash,
    pub committed_entry_count: u64,
    pub committed_total_bytes: u64,
    pub partial: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BorrowedReleaseOutputErrorV1 {
    #[error("borrowed release output request schema is unsupported")]
    UnsupportedSchema,
    #[error("borrowed release output registration capsule was already consumed")]
    CapsuleReplay,
    #[error("release output operation and payload do not match")]
    OperationMismatch,
    #[error("release output destination must be below the fixed owner root")]
    DestinationOutsideRoot,
    #[error("release output path contains a symlink/reparse point or non-directory component")]
    SymlinkAtBoundary,
    #[error("release output destination is already occupied")]
    DestinationOccupied,
    #[error("release output payload is empty or exceeds its bounded limit")]
    PayloadOutOfBounds,
    #[error("release output tree entry is invalid or duplicated")]
    EntryInvalid,
    #[error("release output tree root was partially committed: {reason}")]
    Partial {
        receipt: Box<BorrowedReleaseOutputReceiptV1>,
        reason: String,
    },
    #[error("release output filesystem operation failed: {0}")]
    Filesystem(String),
}

pub trait BorrowedReleaseOutputServiceV1: Send + Sync {
    fn publish(
        &self,
        request: BorrowedReleaseOutputRequestV1,
    ) -> Result<BorrowedReleaseOutputReceiptV1, BorrowedReleaseOutputErrorV1>;
}

/// Real owner for one release-tool invocation family. `output_root` is fixed at construction and
/// is the only parent under which this service will create output.
pub struct AuthorityBorrowedReleaseOutputServiceV1 {
    output_root: PathBuf,
    consumed_capsules: Mutex<(BTreeSet<String>, u64)>,
}

impl std::fmt::Debug for AuthorityBorrowedReleaseOutputServiceV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthorityBorrowedReleaseOutputServiceV1")
            .field("output_root", &"[private]")
            .finish_non_exhaustive()
    }
}

impl AuthorityBorrowedReleaseOutputServiceV1 {
    pub fn new(output_root: impl Into<PathBuf>) -> Self {
        Self {
            output_root: output_root.into(),
            consumed_capsules: Mutex::new((BTreeSet::new(), 0)),
        }
    }

    /// Reserves one absent campaign root before the release owner starts its internal run
    /// preparation. The final report files still require one-shot publish capsules.
    pub fn prepare_tree_root(
        &self,
        root: &std::path::Path,
    ) -> Result<(), BorrowedReleaseOutputErrorV1> {
        self.validate_root()?;
        self.validate_below_root(root)?;
        let parent = root
            .parent()
            .ok_or(BorrowedReleaseOutputErrorV1::DestinationOutsideRoot)?;
        self.validate_existing_path(parent)?;
        let parent_identity = crate::identity::canonical_identity(parent).map_err(map_identity)?;
        if let Ok(metadata) = fs::symlink_metadata(root) {
            if metadata.file_type().is_symlink() {
                return Err(BorrowedReleaseOutputErrorV1::SymlinkAtBoundary);
            }
            return Err(BorrowedReleaseOutputErrorV1::DestinationOccupied);
        }
        fs::create_dir(root).map_err(map_filesystem)?;
        let current_parent = crate::identity::canonical_identity(parent).map_err(map_identity)?;
        if current_parent != parent_identity {
            return Err(BorrowedReleaseOutputErrorV1::Filesystem(
                "release output parent identity changed while reserving root".to_owned(),
            ));
        }
        sync_directory(parent).map_err(map_filesystem)
    }

    fn next_observation(
        &self,
        capsule_id: &OpaqueRegistrationCapsuleId,
    ) -> Result<u64, BorrowedReleaseOutputErrorV1> {
        let mut consumed = self.consumed_capsules.lock().map_err(|_| {
            BorrowedReleaseOutputErrorV1::Filesystem("capsule table poisoned".to_owned())
        })?;
        if !consumed.0.insert(capsule_id.as_str().to_owned()) {
            return Err(BorrowedReleaseOutputErrorV1::CapsuleReplay);
        }
        consumed.1 = consumed.1.saturating_add(1);
        Ok(consumed.1)
    }

    fn validate_root(&self) -> Result<(), BorrowedReleaseOutputErrorV1> {
        let metadata = fs::symlink_metadata(&self.output_root).map_err(map_filesystem)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(BorrowedReleaseOutputErrorV1::SymlinkAtBoundary);
        }
        Ok(())
    }

    fn validate_request(
        &self,
        request: &BorrowedReleaseOutputRequestV1,
    ) -> Result<(), BorrowedReleaseOutputErrorV1> {
        if request.schema_version != BORROWED_RELEASE_OUTPUT_SCHEMA_VERSION {
            return Err(BorrowedReleaseOutputErrorV1::UnsupportedSchema);
        }
        self.validate_root()?;
        match request.operation {
            BorrowedReleaseOutputOperationV1::File => {
                if request.content.is_empty()
                    || request.content.len() > MAX_BORROWED_RELEASE_FILE_BYTES
                    || !request.entries.is_empty()
                {
                    return Err(BorrowedReleaseOutputErrorV1::PayloadOutOfBounds);
                }
                let parent = request
                    .destination
                    .parent()
                    .ok_or(BorrowedReleaseOutputErrorV1::DestinationOutsideRoot)?;
                self.validate_existing_path(parent)?;
                self.validate_below_root(&request.destination)?;
                let leaf = request
                    .destination
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or(BorrowedReleaseOutputErrorV1::EntryInvalid)?;
                if leaf.is_empty() || leaf.len() > 240 {
                    return Err(BorrowedReleaseOutputErrorV1::EntryInvalid);
                }
            }
            BorrowedReleaseOutputOperationV1::Tree => {
                if !request.content.is_empty()
                    || request.entries.is_empty()
                    || request.entries.len() > MAX_BORROWED_RELEASE_TREE_ENTRIES
                {
                    return Err(BorrowedReleaseOutputErrorV1::PayloadOutOfBounds);
                }
                self.validate_below_root(&request.destination)?;
                let parent = request
                    .destination
                    .parent()
                    .ok_or(BorrowedReleaseOutputErrorV1::DestinationOutsideRoot)?;
                self.validate_existing_path(parent)?;
                if let Ok(metadata) = fs::symlink_metadata(&request.destination) {
                    if metadata.file_type().is_symlink() {
                        return Err(BorrowedReleaseOutputErrorV1::SymlinkAtBoundary);
                    }
                    return Err(BorrowedReleaseOutputErrorV1::DestinationOccupied);
                }
                let mut seen = BTreeSet::new();
                let mut total = 0usize;
                for entry in &request.entries {
                    let relative = validate_relative_entry(&entry.relative_path)?;
                    if !seen.insert(relative.to_owned()) || entry.content.is_empty() {
                        return Err(BorrowedReleaseOutputErrorV1::EntryInvalid);
                    }
                    total = total
                        .checked_add(entry.content.len())
                        .ok_or(BorrowedReleaseOutputErrorV1::PayloadOutOfBounds)?;
                    if total > MAX_BORROWED_RELEASE_TREE_BYTES {
                        return Err(BorrowedReleaseOutputErrorV1::PayloadOutOfBounds);
                    }
                }
            }
        }
        Ok(())
    }

    fn validate_below_root(
        &self,
        path: &std::path::Path,
    ) -> Result<(), BorrowedReleaseOutputErrorV1> {
        if path.is_relative() {
            return Err(BorrowedReleaseOutputErrorV1::DestinationOutsideRoot);
        }
        let relative = path
            .strip_prefix(&self.output_root)
            .map_err(|_| BorrowedReleaseOutputErrorV1::DestinationOutsideRoot)?;
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(BorrowedReleaseOutputErrorV1::DestinationOutsideRoot);
        }
        Ok(())
    }

    fn validate_existing_path(
        &self,
        path: &std::path::Path,
    ) -> Result<(), BorrowedReleaseOutputErrorV1> {
        if path == self.output_root {
            return self.validate_root();
        }
        self.validate_below_root(path)?;
        let relative = path
            .strip_prefix(&self.output_root)
            .map_err(|_| BorrowedReleaseOutputErrorV1::DestinationOutsideRoot)?;
        if relative.as_os_str().is_empty() {
            return Ok(());
        }
        let mut current = self.output_root.clone();
        for component in relative.components() {
            let std::path::Component::Normal(component) = component else {
                return Err(BorrowedReleaseOutputErrorV1::DestinationOutsideRoot);
            };
            current.push(component);
            let metadata = fs::symlink_metadata(&current).map_err(map_filesystem)?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(BorrowedReleaseOutputErrorV1::SymlinkAtBoundary);
            }
        }
        Ok(())
    }

    fn subject_for(
        &self,
        parent: &std::path::Path,
    ) -> Result<OpaquePermissionSubjectRef, BorrowedReleaseOutputErrorV1> {
        let identity = crate::identity::canonical_identity(parent).map_err(map_identity)?;
        if identity.is_symlink || !identity.is_directory {
            return Err(BorrowedReleaseOutputErrorV1::SymlinkAtBoundary);
        }
        Ok(OpaquePermissionSubjectRef::new(format!(
            "borrowed-release-output-parent:{}",
            identity.digest.to_hex()
        )))
    }
}

impl BorrowedReleaseOutputServiceV1 for AuthorityBorrowedReleaseOutputServiceV1 {
    fn publish(
        &self,
        request: BorrowedReleaseOutputRequestV1,
    ) -> Result<BorrowedReleaseOutputReceiptV1, BorrowedReleaseOutputErrorV1> {
        self.validate_request(&request)?;
        let observation_generation = self.next_observation(&request.capsule_id)?;
        match request.operation {
            BorrowedReleaseOutputOperationV1::File => {
                self.publish_file(request, observation_generation)
            }
            BorrowedReleaseOutputOperationV1::Tree => {
                self.publish_tree(request, observation_generation)
            }
        }
    }
}

impl AuthorityBorrowedReleaseOutputServiceV1 {
    fn publish_file(
        &self,
        request: BorrowedReleaseOutputRequestV1,
        observation_generation: u64,
    ) -> Result<BorrowedReleaseOutputReceiptV1, BorrowedReleaseOutputErrorV1> {
        let parent = request
            .destination
            .parent()
            .ok_or(BorrowedReleaseOutputErrorV1::DestinationOutsideRoot)?;
        let subject_ref = self.subject_for(parent)?;
        let parent_identity = crate::identity::canonical_identity(parent).map_err(map_identity)?;
        let current_parent = crate::identity::canonical_identity(parent).map_err(map_identity)?;
        if current_parent != parent_identity {
            return Err(BorrowedReleaseOutputErrorV1::Filesystem(
                "release output parent identity changed before publish".to_owned(),
            ));
        }
        if let Ok(metadata) = fs::symlink_metadata(&request.destination) {
            if metadata.file_type().is_symlink() {
                return Err(BorrowedReleaseOutputErrorV1::SymlinkAtBoundary);
            }
            return Err(BorrowedReleaseOutputErrorV1::DestinationOccupied);
        }
        let content_digest = digest_bytes(&request.content);
        let mut staged = Builder::new()
            .prefix(".sigil-release-")
            .tempfile_in(parent)
            .map_err(map_filesystem)?;
        std::io::Write::write_all(&mut staged, &request.content).map_err(map_filesystem)?;
        staged.as_file().sync_all().map_err(map_filesystem)?;
        staged
            .persist_noclobber(&request.destination)
            .map_err(|error| {
                if error.error.kind() == std::io::ErrorKind::AlreadyExists {
                    BorrowedReleaseOutputErrorV1::DestinationOccupied
                } else {
                    map_filesystem(error.error)
                }
            })?;
        sync_directory(parent).map_err(map_filesystem)?;
        Ok(BorrowedReleaseOutputReceiptV1 {
            schema_version: BORROWED_RELEASE_OUTPUT_SCHEMA_VERSION,
            capsule_id: request.capsule_id,
            subject_ref,
            observation_generation,
            operation: BorrowedReleaseOutputOperationV1::File,
            content_digest,
            committed_entry_count: 1,
            committed_total_bytes: request.content.len() as u64,
            partial: false,
        })
    }

    fn publish_tree(
        &self,
        request: BorrowedReleaseOutputRequestV1,
        observation_generation: u64,
    ) -> Result<BorrowedReleaseOutputReceiptV1, BorrowedReleaseOutputErrorV1> {
        let parent = request
            .destination
            .parent()
            .ok_or(BorrowedReleaseOutputErrorV1::DestinationOutsideRoot)?;
        let subject_ref = self.subject_for(parent)?;
        let parent_identity = crate::identity::canonical_identity(parent).map_err(map_identity)?;
        let current_parent = crate::identity::canonical_identity(parent).map_err(map_identity)?;
        if current_parent != parent_identity {
            return Err(BorrowedReleaseOutputErrorV1::Filesystem(
                "release output parent identity changed before tree publish".to_owned(),
            ));
        }
        if let Ok(metadata) = fs::symlink_metadata(&request.destination) {
            if metadata.file_type().is_symlink() {
                return Err(BorrowedReleaseOutputErrorV1::SymlinkAtBoundary);
            }
            return Err(BorrowedReleaseOutputErrorV1::DestinationOccupied);
        }
        fs::create_dir(&request.destination).map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                BorrowedReleaseOutputErrorV1::DestinationOccupied
            } else {
                map_filesystem(error)
            }
        })?;
        let mut committed_entry_count = 0_u64;
        let mut committed_total_bytes = 0_u64;
        let mut committed = Vec::new();
        for entry in request.entries {
            let relative = validate_relative_entry(&entry.relative_path)?;
            let destination = request.destination.join(relative);
            let entry_result =
                self.publish_tree_entry(&request.destination, &destination, &entry.content);
            if let Err(error) = entry_result {
                let receipt = BorrowedReleaseOutputReceiptV1 {
                    schema_version: BORROWED_RELEASE_OUTPUT_SCHEMA_VERSION,
                    capsule_id: request.capsule_id,
                    subject_ref,
                    observation_generation,
                    operation: BorrowedReleaseOutputOperationV1::Tree,
                    content_digest: digest_entries(&committed),
                    committed_entry_count,
                    committed_total_bytes,
                    partial: true,
                };
                return Err(BorrowedReleaseOutputErrorV1::Partial {
                    receipt: Box::new(receipt),
                    reason: error.to_string(),
                });
            }
            committed.push((relative.to_owned(), entry.content.clone()));
            committed_entry_count = committed_entry_count.saturating_add(1);
            committed_total_bytes =
                committed_total_bytes.saturating_add(entry.content.len() as u64);
        }
        sync_directory(&request.destination).map_err(map_filesystem)?;
        sync_directory(parent).map_err(map_filesystem)?;
        Ok(BorrowedReleaseOutputReceiptV1 {
            schema_version: BORROWED_RELEASE_OUTPUT_SCHEMA_VERSION,
            capsule_id: request.capsule_id,
            subject_ref,
            observation_generation,
            operation: BorrowedReleaseOutputOperationV1::Tree,
            content_digest: digest_entries(&committed),
            committed_entry_count,
            committed_total_bytes,
            partial: false,
        })
    }

    fn publish_tree_entry(
        &self,
        tree_root: &std::path::Path,
        destination: &std::path::Path,
        content: &[u8],
    ) -> Result<(), BorrowedReleaseOutputErrorV1> {
        let parent = destination
            .parent()
            .ok_or(BorrowedReleaseOutputErrorV1::EntryInvalid)?;
        ensure_tree_parent(tree_root, parent)?;
        if let Ok(metadata) = fs::symlink_metadata(destination) {
            if metadata.file_type().is_symlink() {
                return Err(BorrowedReleaseOutputErrorV1::SymlinkAtBoundary);
            }
            return Err(BorrowedReleaseOutputErrorV1::DestinationOccupied);
        }
        let mut staged = Builder::new()
            .prefix(".sigil-release-")
            .tempfile_in(parent)
            .map_err(map_filesystem)?;
        std::io::Write::write_all(&mut staged, content).map_err(map_filesystem)?;
        staged.as_file().sync_all().map_err(map_filesystem)?;
        staged.persist_noclobber(destination).map_err(|error| {
            if error.error.kind() == std::io::ErrorKind::AlreadyExists {
                BorrowedReleaseOutputErrorV1::DestinationOccupied
            } else {
                map_filesystem(error.error)
            }
        })?;
        sync_directory(parent).map_err(map_filesystem)
    }
}

fn validate_relative_entry(
    path: &std::path::Path,
) -> Result<&std::path::Path, BorrowedReleaseOutputErrorV1> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(BorrowedReleaseOutputErrorV1::EntryInvalid);
    }
    Ok(path)
}

fn ensure_tree_parent(
    tree_root: &std::path::Path,
    path: &std::path::Path,
) -> Result<(), BorrowedReleaseOutputErrorV1> {
    let relative = path
        .strip_prefix(tree_root)
        .map_err(|_| BorrowedReleaseOutputErrorV1::EntryInvalid)?;
    let mut current = tree_root.to_path_buf();
    for component in relative.components() {
        let std::path::Component::Normal(component) = component else {
            return Err(BorrowedReleaseOutputErrorV1::EntryInvalid);
        };
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(BorrowedReleaseOutputErrorV1::SymlinkAtBoundary);
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&current).map_err(map_filesystem)?;
            }
            Err(error) => return Err(map_filesystem(error)),
        }
    }
    Ok(())
}

fn digest_bytes(bytes: &[u8]) -> CanonicalHash {
    use sha2::{Digest, Sha256};
    CanonicalHash::from_bytes(Sha256::digest(bytes).into())
}

fn digest_entries(entries: &[(PathBuf, Vec<u8>)]) -> CanonicalHash {
    let mut bytes = b"sigil-release-tree-v1\0".to_vec();
    for (path, content) in entries {
        bytes.extend_from_slice(path.to_string_lossy().as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(digest_bytes(content).as_bytes());
        bytes.extend_from_slice(&(content.len() as u64).to_be_bytes());
    }
    digest_bytes(&bytes)
}

fn map_filesystem(error: impl std::fmt::Display) -> BorrowedReleaseOutputErrorV1 {
    BorrowedReleaseOutputErrorV1::Filesystem(error.to_string())
}

fn map_identity(error: crate::identity::IdentityErrorV1) -> BorrowedReleaseOutputErrorV1 {
    match error {
        crate::identity::IdentityErrorV1::SymlinkAtBoundary => {
            BorrowedReleaseOutputErrorV1::SymlinkAtBoundary
        }
        other => BorrowedReleaseOutputErrorV1::Filesystem(other.to_string()),
    }
}

fn sync_directory(path: &std::path::Path) -> Result<(), std::io::Error> {
    #[cfg(unix)]
    {
        fs::File::open(path)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capsule(value: &str) -> OpaqueRegistrationCapsuleId {
        OpaqueRegistrationCapsuleId::new(value.to_owned())
    }

    #[test]
    fn r71_release_file_is_create_new_and_returns_closed_receipt() {
        let temp = tempfile::tempdir().expect("tempdir");
        let service = AuthorityBorrowedReleaseOutputServiceV1::new(temp.path());
        let destination = temp.path().join("route.toml");
        let receipt = service
            .publish(BorrowedReleaseOutputRequestV1 {
                schema_version: BORROWED_RELEASE_OUTPUT_SCHEMA_VERSION,
                capsule_id: capsule("release-file"),
                operation: BorrowedReleaseOutputOperationV1::File,
                destination: destination.clone(),
                content: b"route = \"v1\"\n".to_vec(),
                entries: Vec::new(),
            })
            .expect("file publish");
        assert!(!receipt.partial);
        assert_eq!(receipt.committed_entry_count, 1);
        assert_eq!(fs::read(&destination).expect("output"), b"route = \"v1\"\n");
        let replay = service
            .publish(BorrowedReleaseOutputRequestV1 {
                schema_version: BORROWED_RELEASE_OUTPUT_SCHEMA_VERSION,
                capsule_id: capsule("release-file"),
                operation: BorrowedReleaseOutputOperationV1::File,
                destination: temp.path().join("other.toml"),
                content: b"other".to_vec(),
                entries: Vec::new(),
            })
            .expect_err("replay");
        assert_eq!(replay, BorrowedReleaseOutputErrorV1::CapsuleReplay);
    }

    #[test]
    fn r71_release_tree_commits_bounded_entries_without_adopting_root() {
        let temp = tempfile::tempdir().expect("tempdir");
        let service = AuthorityBorrowedReleaseOutputServiceV1::new(temp.path());
        let root = temp.path().join("campaign");
        let receipt = service
            .publish(BorrowedReleaseOutputRequestV1 {
                schema_version: BORROWED_RELEASE_OUTPUT_SCHEMA_VERSION,
                capsule_id: capsule("release-tree"),
                operation: BorrowedReleaseOutputOperationV1::Tree,
                destination: root.clone(),
                content: Vec::new(),
                entries: vec![
                    BorrowedReleaseOutputEntryV1 {
                        relative_path: PathBuf::from("nested/results.jsonl"),
                        content: b"{}\n".to_vec(),
                    },
                    BorrowedReleaseOutputEntryV1 {
                        relative_path: PathBuf::from("summary.md"),
                        content: b"# report\n".to_vec(),
                    },
                ],
            })
            .expect("tree publish");
        assert!(!receipt.partial);
        assert_eq!(receipt.committed_entry_count, 2);
        assert_eq!(
            fs::read(root.join("nested/results.jsonl")).expect("nested output"),
            b"{}\n"
        );
        assert_eq!(
            fs::read(root.join("summary.md")).expect("summary"),
            b"# report\n"
        );
        let occupied = service
            .publish(BorrowedReleaseOutputRequestV1 {
                schema_version: BORROWED_RELEASE_OUTPUT_SCHEMA_VERSION,
                capsule_id: capsule("release-tree-occupied"),
                operation: BorrowedReleaseOutputOperationV1::Tree,
                destination: root,
                content: Vec::new(),
                entries: vec![BorrowedReleaseOutputEntryV1 {
                    relative_path: PathBuf::from("new.txt"),
                    content: b"new".to_vec(),
                }],
            })
            .expect_err("occupied root");
        assert_eq!(occupied, BorrowedReleaseOutputErrorV1::DestinationOccupied);
    }

    #[test]
    fn r71_release_output_rejects_aliases_and_invalid_tree_entries_before_effect() {
        let temp = tempfile::tempdir().expect("tempdir");
        let service = AuthorityBorrowedReleaseOutputServiceV1::new(temp.path());
        let error = service
            .publish(BorrowedReleaseOutputRequestV1 {
                schema_version: BORROWED_RELEASE_OUTPUT_SCHEMA_VERSION,
                capsule_id: capsule("release-invalid"),
                operation: BorrowedReleaseOutputOperationV1::Tree,
                destination: temp.path().join("invalid"),
                content: Vec::new(),
                entries: vec![BorrowedReleaseOutputEntryV1 {
                    relative_path: PathBuf::from("../escape.txt"),
                    content: b"escape".to_vec(),
                }],
            })
            .expect_err("traversal");
        assert_eq!(error, BorrowedReleaseOutputErrorV1::EntryInvalid);
        assert!(!temp.path().join("invalid").exists());
    }

    #[test]
    fn r71_release_tree_returns_closed_partial_frontier_after_late_conflict() {
        let temp = tempfile::tempdir().expect("tempdir");
        let service = AuthorityBorrowedReleaseOutputServiceV1::new(temp.path());
        let root = temp.path().join("partial");
        let error = service
            .publish(BorrowedReleaseOutputRequestV1 {
                schema_version: BORROWED_RELEASE_OUTPUT_SCHEMA_VERSION,
                capsule_id: capsule("release-partial"),
                operation: BorrowedReleaseOutputOperationV1::Tree,
                destination: root.clone(),
                content: Vec::new(),
                entries: vec![
                    BorrowedReleaseOutputEntryV1 {
                        relative_path: PathBuf::from("a.txt"),
                        content: b"first".to_vec(),
                    },
                    BorrowedReleaseOutputEntryV1 {
                        relative_path: PathBuf::from("a.txt/nested.txt"),
                        content: b"conflict".to_vec(),
                    },
                ],
            })
            .expect_err("late tree conflict");
        match error {
            BorrowedReleaseOutputErrorV1::Partial { receipt, .. } => {
                assert!(receipt.partial);
                assert_eq!(receipt.committed_entry_count, 1);
                assert_eq!(receipt.committed_total_bytes, 5);
            }
            other => panic!("expected partial receipt, got {other:?}"),
        }
        assert_eq!(
            fs::read(root.join("a.txt")).expect("partial file"),
            b"first"
        );
        assert!(!root.join("a.txt/nested.txt").exists());
    }
}
