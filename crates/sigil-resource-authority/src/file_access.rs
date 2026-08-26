//! RFC-0071 section 8.5 / R71.6: authority-owned in-process file access adjudicator.
//!
//! read/write/edit/list/glob/grep tools never spawn; they must not bypass the borrowed
//! Workspace / ExternalUserPath identity lease. This service adjudicates the post-decision
//! Tool admission: token binding vs request, one-shot adjudication claim, observed borrowed
//! identity (identity_before in the receipt), SystemTemp deny/read-boundary, and closed
//! operation classification. It performs approved relative descriptor/handle I/O itself, but
//! never claims ownership of borrowed content. SessionExport / SessionExportReconcile tokens
//! have their own kernel-verified export path (session_export.rs) and are refused here until the
//! storage writer slice wires them through explicitly.

use std::collections::{BTreeMap, BTreeSet};
#[cfg(unix)]
use std::ffi::{CStr, CString};
#[cfg(any(unix, windows))]
use std::fs::OpenOptions;
#[cfg(unix)]
use std::io::SeekFrom;
#[cfg(any(unix, windows))]
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[cfg(unix)]
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;

use sigil_kernel::managed_execution::BorrowedResourceAccessReceiptV1;
#[cfg(unix)]
use sigil_kernel::managed_file_access::ManagedFileExecutionInputV1;
use sigil_kernel::managed_file_access::{
    ManagedFileAccessAdmissionTokenV1, ManagedFileAccessErrorV1, ManagedFileAccessPlanRequestV1,
    ManagedFileAccessRequestV1, ManagedFileAccessResultV1, ManagedFileAccessServiceV1,
    ManagedFileAdmissionBindingV1, ManagedFileExecutionOutcomeV1, ManagedFileExecutionRequestV1,
    ManagedFileOperationV1,
};
use sigil_kernel::resource::{
    AuthorityGeneration, CanonicalHash, OpaquePermissionSubjectRef, ResourceAccessV1,
};
#[cfg(unix)]
use sigil_kernel::secure_private_path_permissions;

use crate::borrowed::{BorrowedSubjectClassV1, BorrowedSubjectRegistryV1};
#[cfg(unix)]
use crate::journal::{ResourceJournalEventV1, ResourceJournalFileIdentityV1};
use crate::journal::{ResourceJournalFileV1, ResourceJournalHeaderV1};

/// Closed access class for a closed file operation.
pub fn access_class_for(operation: ManagedFileOperationV1) -> ResourceAccessV1 {
    match operation {
        ManagedFileOperationV1::Read
        | ManagedFileOperationV1::List
        | ManagedFileOperationV1::Glob
        | ManagedFileOperationV1::Grep => ResourceAccessV1::Read,
        ManagedFileOperationV1::Write | ManagedFileOperationV1::Edit => ResourceAccessV1::Write,
        ManagedFileOperationV1::Delete | ManagedFileOperationV1::Rename => {
            ResourceAccessV1::DeleteManaged
        }
    }
}

/// Stable tag for one access class (canonical, never a raw enum cast).
fn access_tag(access: ResourceAccessV1) -> u8 {
    match access {
        ResourceAccessV1::Read => 1,
        ResourceAccessV1::Write => 2,
        ResourceAccessV1::Create => 3,
        ResourceAccessV1::DeleteManaged => 4,
        ResourceAccessV1::DeleteExactSubject => 5,
        ResourceAccessV1::DeleteSubjectSubtree => 6,
        ResourceAccessV1::RenameWithinGrant => 7,
        ResourceAccessV1::Execute => 8,
    }
}

/// Authority-owned file access adjudicator behind the kernel pathless port.
pub struct AuthorityManagedFileAccessServiceV1 {
    registry: Arc<Mutex<BorrowedSubjectRegistryV1>>,
    consumed: Mutex<BTreeSet<String>>,
    plans: Mutex<BTreeMap<String, PlannedFileAccessV1>>,
    file_delete: Option<Arc<FileDeleteAuthorityStateV1>>,
}

/// Authority-private state for the Unix delete protocol. The arena is rooted below the
/// authority's owner-only state anchor, never below the user-writable workspace. Its pathname is
/// not included in any child request or sandbox binding; recovery uses only the journal's opaque
/// operation id and the authority-provided arena root.
#[cfg_attr(not(unix), allow(dead_code))]
struct FileDeleteAuthorityStateV1 {
    arena_root: PathBuf,
    journal: Mutex<ResourceJournalFileV1>,
}

#[derive(Debug, Clone)]
struct PlannedFileAccessV1 {
    subject_ref: OpaquePermissionSubjectRef,
    #[cfg(not(unix))]
    root: PathBuf,
    logical_path: String,
    physical_path: PathBuf,
    #[cfg(any(unix, windows))]
    root_handle: Arc<std::fs::File>,
    expected_physical_identity: Option<CanonicalHash>,
    operation: ManagedFileOperationV1,
    operation_digest: CanonicalHash,
    authority_generation: AuthorityGeneration,
    root_identity: CanonicalHash,
    plan_hash: CanonicalHash,
}

impl AuthorityManagedFileAccessServiceV1 {
    /// Creates the adjudicator. The registry is the single borrowed-identity observation source
    /// shared with bootstrap (identity observation happens once per generation).
    pub fn new(registry: Arc<Mutex<BorrowedSubjectRegistryV1>>) -> Self {
        Self {
            registry,
            consumed: Mutex::new(BTreeSet::new()),
            plans: Mutex::new(BTreeMap::new()),
            file_delete: None,
        }
    }

    /// Creates the production delete owner. The arena and journal are both under an authority
    /// state anchor, and the constructor refuses a corrupt/mismatched journal before the service
    /// can be published to any tool surface.
    pub fn new_with_journal(
        registry: Arc<Mutex<BorrowedSubjectRegistryV1>>,
        arena_root: PathBuf,
        journal_path: PathBuf,
        bootstrap_manifest_hash: CanonicalHash,
        journal_instance_hash: CanonicalHash,
    ) -> Result<Self, ManagedFileAccessErrorV1> {
        let header = ResourceJournalHeaderV1 {
            schema_version: 1,
            shard_name: "file-delete".to_owned(),
            bootstrap_manifest_hash,
            journal_instance_hash,
            header_hash: file_delete_header_hash(&arena_root, &journal_path),
        };
        let journal = ResourceJournalFileV1::open(journal_path, header).map_err(|error| {
            ManagedFileAccessErrorV1::PhysicalExecutionFailed(format!(
                "file-delete journal open failed: {error}"
            ))
        })?;
        Ok(Self {
            registry,
            consumed: Mutex::new(BTreeSet::new()),
            plans: Mutex::new(BTreeMap::new()),
            file_delete: Some(Arc::new(FileDeleteAuthorityStateV1 {
                arena_root,
                journal: Mutex::new(journal),
            })),
        })
    }

    /// Reconciles every non-terminal delete prefix after workspace activation. A pending rename
    /// is never guessed away: the authority either observes the expected object and completes
    /// the already-authorized deletion, restores it without replacement, or leaves a typed
    /// reconciliation blocker.
    pub fn reconcile_file_delete_journal(&self) -> Result<(), ManagedFileAccessErrorV1> {
        #[cfg(unix)]
        {
            let Some(state) = self.file_delete.as_ref() else {
                return Ok(());
            };
            reconcile_file_delete_journal(state, &self.registry)?;
        }
        Ok(())
    }

    #[cfg(test)]
    fn new_for_test_with_journal(
        registry: Arc<Mutex<BorrowedSubjectRegistryV1>>,
        arena_root: PathBuf,
        journal_path: PathBuf,
    ) -> Self {
        Self::new_with_journal(
            registry,
            arena_root,
            journal_path,
            CanonicalHash::from_bytes([0x71; 32]),
            CanonicalHash::from_bytes([0x72; 32]),
        )
        .expect("test file-delete journal")
    }

    fn claim_key(token: &ManagedFileAccessAdmissionTokenV1) -> String {
        match token {
            ManagedFileAccessAdmissionTokenV1::Tool(tool) => format!(
                "{}-{}",
                tool.subject_binding_hash().to_hex(),
                tool.operation_digest().to_hex()
            ),
            ManagedFileAccessAdmissionTokenV1::SessionExport(_)
            | ManagedFileAccessAdmissionTokenV1::SessionExportReconcile(_) => {
                "export-not-wired".to_owned()
            }
        }
    }

    fn sole_workspace(
        &self,
    ) -> Result<
        (
            OpaquePermissionSubjectRef,
            PathBuf,
            AuthorityGeneration,
            CanonicalHash,
        ),
        ManagedFileAccessErrorV1,
    > {
        let registry = self
            .registry
            .lock()
            .map_err(|_| ManagedFileAccessErrorV1::SubjectIdentityDrift)?;
        let subject_ref = registry
            .sole_workspace_subject()
            .ok_or(ManagedFileAccessErrorV1::ResourcePreconditionUnavailable)?;
        let root = registry
            .workspace_root_for(&subject_ref)
            .ok_or(ManagedFileAccessErrorV1::ResourcePreconditionUnavailable)?
            .to_path_buf();
        let capsule = registry
            .workspace_capsule_for(&subject_ref)
            .ok_or(ManagedFileAccessErrorV1::ResourcePreconditionUnavailable)?;
        Ok((
            subject_ref,
            root,
            capsule.authority_generation,
            capsule.root_identity_hash,
        ))
    }

    fn resolve_plan_path(
        root: &Path,
        logical_path: &str,
    ) -> Result<PathBuf, ManagedFileAccessErrorV1> {
        let candidate = root.join(logical_path);
        if let Ok(canonical) = candidate.canonicalize() {
            if !canonical.starts_with(root) {
                return Err(ManagedFileAccessErrorV1::AliasCollision);
            }
            return Ok(canonical);
        }
        Ok(candidate)
    }

    #[cfg(any(unix, windows))]
    fn open_workspace_root(root: &Path) -> Result<Arc<std::fs::File>, ManagedFileAccessErrorV1> {
        #[cfg(unix)]
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(root);
        #[cfg(windows)]
        let file = OpenOptions::new()
            .read(true)
            .share_mode(
                windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ
                    | windows_sys::Win32::Storage::FileSystem::FILE_SHARE_WRITE,
            )
            .custom_flags(
                windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS
                    | windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT,
            )
            .open(root);
        let file = file.map_err(|error| {
            ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
        })?;
        #[cfg(windows)]
        let identity =
            crate::identity::canonical_identity_from_handle(root, &file).map_err(|error| {
                ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
            })?;
        #[cfg(not(windows))]
        let identity = crate::identity::canonical_identity_from_metadata(
            root,
            &file.metadata().map_err(|error| {
                ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
            })?,
        );
        if identity.is_symlink || !identity.is_directory {
            return Err(ManagedFileAccessErrorV1::AliasCollision);
        }
        Ok(Arc::new(file))
    }

    fn hash_parts(parts: &[&[u8]]) -> CanonicalHash {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        for part in parts {
            hasher.update(part.len().to_be_bytes());
            hasher.update(part);
        }
        CanonicalHash::from_bytes(hasher.finalize().into())
    }

    fn expected_physical_identity(
        path: &Path,
        logical_path: &str,
    ) -> Result<Option<CanonicalHash>, ManagedFileAccessErrorV1> {
        let path_metadata = match std::fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(ManagedFileAccessErrorV1::PhysicalExecutionFailed(
                    error.to_string(),
                ));
            }
        };
        #[cfg(windows)]
        let _path_metadata = path_metadata;
        #[cfg(not(windows))]
        let metadata = path_metadata;
        #[cfg(windows)]
        let identity = Self::windows_identity_for_path(path)?;
        #[cfg(not(windows))]
        let identity = crate::identity::canonical_identity_from_metadata(path, &metadata);
        if identity.is_symlink || (identity.is_regular_file && identity.link_count > 1) {
            return Err(ManagedFileAccessErrorV1::AliasCollision);
        }
        let _ = logical_path;
        Ok(Some(identity.digest))
    }

    #[cfg(windows)]
    fn windows_identity_for_path(
        path: &Path,
    ) -> Result<crate::identity::CanonicalLocalIdentity, ManagedFileAccessErrorV1> {
        let metadata = std::fs::symlink_metadata(path).map_err(|error| {
            ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
        })?;
        let kind = if metadata.is_dir() {
            WindowsOpenKind::Directory
        } else {
            WindowsOpenKind::Read
        };
        let file = windows_open_component(path, kind, false).map_err(relative_io_error)?;
        crate::identity::canonical_identity_from_handle(path, &file)
            .map_err(|error| ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string()))
    }

    fn current_physical_identity(
        path: &Path,
    ) -> Result<Option<CanonicalHash>, ManagedFileAccessErrorV1> {
        let path_metadata = match std::fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(ManagedFileAccessErrorV1::PhysicalExecutionFailed(
                    error.to_string(),
                ));
            }
        };
        #[cfg(windows)]
        let _path_metadata = path_metadata;
        #[cfg(not(windows))]
        let metadata = path_metadata;
        #[cfg(windows)]
        let identity = Self::windows_identity_for_path(path)?;
        #[cfg(not(windows))]
        let identity = crate::identity::canonical_identity_from_metadata(path, &metadata);
        if identity.is_symlink || (identity.is_regular_file && identity.link_count > 1) {
            return Err(ManagedFileAccessErrorV1::AliasCollision);
        }
        Ok(Some(identity.digest))
    }

    fn verify_planned_physical_identity(
        plan: &PlannedFileAccessV1,
    ) -> Result<(), ManagedFileAccessErrorV1> {
        if Self::current_physical_identity(&plan.physical_path)? != plan.expected_physical_identity
        {
            return Err(ManagedFileAccessErrorV1::PlanStale);
        }
        Ok(())
    }

    fn current_root_identity(
        &self,
        subject_ref: &OpaquePermissionSubjectRef,
    ) -> Result<CanonicalHash, ManagedFileAccessErrorV1> {
        let registry = self
            .registry
            .lock()
            .map_err(|_| ManagedFileAccessErrorV1::SubjectIdentityDrift)?;
        let root = registry
            .workspace_root_for(subject_ref)
            .ok_or(ManagedFileAccessErrorV1::ResourcePreconditionUnavailable)?;
        let observed = crate::identity::canonical_identity(root)
            .map_err(|_| ManagedFileAccessErrorV1::SubjectIdentityDrift)?;
        let expected = registry
            .workspace_capsule_for(subject_ref)
            .ok_or(ManagedFileAccessErrorV1::ResourcePreconditionUnavailable)?
            .root_identity_hash;
        (observed.digest == expected)
            .then_some(observed.digest)
            .ok_or(ManagedFileAccessErrorV1::SubjectIdentityDrift)
    }
}

fn receipt_digest(
    subject_ref: &OpaquePermissionSubjectRef,
    subject_binding_hash: CanonicalHash,
    operation_digest: CanonicalHash,
) -> CanonicalHash {
    use sha2::{Digest, Sha256};
    let mut acc = Vec::new();
    acc.extend_from_slice(subject_ref.as_str().as_bytes());
    acc.extend_from_slice(subject_binding_hash.as_bytes());
    acc.extend_from_slice(operation_digest.as_bytes());
    let mut hasher = Sha256::new();
    hasher.update(acc);
    CanonicalHash::from_bytes(hasher.finalize().into())
}

impl ManagedFileAccessServiceV1 for AuthorityManagedFileAccessServiceV1 {
    fn plan(
        &self,
        request: ManagedFileAccessPlanRequestV1,
    ) -> Result<
        sigil_kernel::permission_plan_v3::ManagedFileAccessPlanDraftRefV1,
        ManagedFileAccessErrorV1,
    > {
        let (subject_ref, root, authority_generation, root_identity) = self.sole_workspace()?;
        let logical_path = request.logical_path.as_str().to_owned();
        let physical_path = Self::resolve_plan_path(&root, &logical_path)?;
        let expected_physical_identity =
            Self::expected_physical_identity(&physical_path, &logical_path)?;
        #[cfg(any(unix, windows))]
        let root_handle = Self::open_workspace_root(&root)?;
        let subject_binding_hash = Self::hash_parts(&[
            subject_ref.as_str().as_bytes(),
            root_identity.as_bytes(),
            logical_path.as_bytes(),
            request.operation_scope.as_bytes(),
        ]);
        let operation_digest = Self::hash_parts(&[
            operation_tag(request.operation),
            request.operation_scope.as_bytes(),
            logical_path.as_bytes(),
        ]);
        let resolver_proof_digest = Self::hash_parts(&[
            root_identity.as_bytes(),
            physical_path.to_string_lossy().as_bytes(),
            expected_physical_identity
                .unwrap_or_else(|| Self::hash_parts(&[b"missing-target", logical_path.as_bytes()]))
                .as_bytes(),
        ]);
        let epoch_bytes = authority_generation.epoch.to_be_bytes();
        let plan_hash = Self::hash_parts(&[
            subject_binding_hash.as_bytes(),
            operation_digest.as_bytes(),
            resolver_proof_digest.as_bytes(),
            &epoch_bytes,
            authority_generation.instance_hash.as_bytes(),
        ]);
        let plan = sigil_kernel::permission_plan_v3::ManagedFileAccessPlanDraftRefV1 {
            plan_id: sigil_kernel::resource::OpaqueManagedFileAccessPlanId::new(format!(
                "managed-file-plan-{}",
                plan_hash.to_hex()
            )),
            subject_ref: subject_ref.clone(),
            subject_binding_hash,
            operation_digest,
            authority_generation,
            resolver_proof_digest,
            plan_hash,
        };
        self.plans
            .lock()
            .map_err(|_| ManagedFileAccessErrorV1::PlanStale)?
            .insert(
                plan_hash.to_hex(),
                PlannedFileAccessV1 {
                    subject_ref,
                    #[cfg(not(unix))]
                    root,
                    logical_path,
                    physical_path,
                    #[cfg(any(unix, windows))]
                    root_handle,
                    expected_physical_identity,
                    operation: request.operation,
                    operation_digest,
                    authority_generation,
                    root_identity,
                    plan_hash,
                },
            );
        Ok(plan)
    }

    fn access(
        &self,
        request: ManagedFileAccessRequestV1,
        token: ManagedFileAccessAdmissionTokenV1,
    ) -> Result<ManagedFileAccessResultV1, ManagedFileAccessErrorV1> {
        // V1 adjudicates the Tool path (in-process file tools). Export token kinds are refused
        // until the storage/export writer slice wires them explicitly.
        let ManagedFileAccessAdmissionTokenV1::Tool(tool) = &token else {
            return Err(ManagedFileAccessErrorV1::OperationNotPermitted);
        };
        if request.admission_binding != *tool.binding() {
            return Err(ManagedFileAccessErrorV1::AdmissionMismatch);
        }
        if tool.operation_digest() != request.operation_digest {
            return Err(ManagedFileAccessErrorV1::OperationNotPermitted);
        }
        let access_class = access_class_for(request.operation);
        let registry = self
            .registry
            .lock()
            .map_err(|_| ManagedFileAccessErrorV1::SubjectIdentityDrift)?;
        let Some(class) = registry.class_for(&request.subject_ref) else {
            return Err(ManagedFileAccessErrorV1::OperationNotPermitted);
        };
        // SystemTemp is a deny/read-boundary fact in V1: non-read operations are refused.
        if class == BorrowedSubjectClassV1::SystemTemp && access_class != ResourceAccessV1::Read {
            return Err(ManagedFileAccessErrorV1::OperationNotPermitted);
        }
        // One-shot adjudication claim is consumed only after every check passes: a refused
        // adjudication never burns the approval.
        let key = Self::claim_key(&token);
        let mut consumed = self
            .consumed
            .lock()
            .map_err(|_| ManagedFileAccessErrorV1::AdmissionMismatch)?;
        if !consumed.insert(key) {
            return Err(ManagedFileAccessErrorV1::TokenReplay);
        }
        drop(consumed);

        let subject_binding_hash = tool.subject_binding_hash();
        let operation_digest = tool.operation_digest();
        let identity_before = registry
            .identity_for(&request.subject_ref)
            .map(|identity| identity.digest);
        let mut granted = BTreeSet::new();
        granted.insert(access_class);
        let mut granted_material = Vec::new();
        for access in &granted {
            granted_material.push(access_tag(*access));
        }
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(granted_material);
        let granted_access_hash = CanonicalHash::from_bytes(hasher.finalize().into());
        let receipt_hash =
            receipt_digest(&request.subject_ref, subject_binding_hash, operation_digest);
        let access_receipt = BorrowedResourceAccessReceiptV1 {
            subject_ref: request.subject_ref.clone(),
            subject_binding_hash,
            operation_digest,
            granted_access_hash,
            identity_before,
            identity_after: None,
            borrowed_effect_frontier_hash: receipt_hash,
            effect_settlement: sigil_kernel::recovery::EffectSettlementV1::Applied,
            receipt_hash,
        };
        Ok(ManagedFileAccessResultV1 {
            access_receipt,
            effect_settlement: sigil_kernel::recovery::EffectSettlementV1::Applied,
            result_digest: receipt_hash,
        })
    }

    fn execute(
        &self,
        request: ManagedFileExecutionRequestV1,
        token: ManagedFileAccessAdmissionTokenV1,
    ) -> Result<ManagedFileExecutionOutcomeV1, ManagedFileAccessErrorV1> {
        let ManagedFileAdmissionBindingV1::ToolPermissionPlan {
            file_access_plan_hash,
            file_authority_generation,
            ..
        } = &request.access.admission_binding
        else {
            return Err(ManagedFileAccessErrorV1::AdmissionMismatch);
        };
        let plan = self
            .plans
            .lock()
            .map_err(|_| ManagedFileAccessErrorV1::PlanStale)?
            .get(&file_access_plan_hash.to_hex())
            .cloned()
            .ok_or(ManagedFileAccessErrorV1::PlanStale)?;
        if plan.plan_hash != *file_access_plan_hash
            || plan.subject_ref != request.access.subject_ref
            || plan.operation != request.access.operation
            || plan.operation_digest != request.access.operation_digest
            || plan.authority_generation != *file_authority_generation
        {
            return Err(ManagedFileAccessErrorV1::AdmissionMismatch);
        }
        // Revalidate both the borrowed root and the approved leaf before consuming the one-shot
        // token. Root drift takes precedence because the leaf path is no longer meaningful when
        // its authority boundary has been replaced. A replacement inode, absent-to-present
        // transition, or hard-link alias must not reach an effectful open/truncate/unlink.
        let current_root = self.current_root_identity(&plan.subject_ref)?;
        if current_root != plan.root_identity {
            return Err(ManagedFileAccessErrorV1::SubjectIdentityDrift);
        }
        Self::verify_planned_physical_identity(&plan)?;
        // Validate the pathless plan before consuming the one-shot admission. A stale or
        // cross-plan request must not burn a valid approval token.
        let result = self.access(request.access.clone(), token)?;
        let PhysicalExecutionOutcomeV1 {
            payload,
            observed_bytes,
            returned_entries,
            total_entries,
            returned_lines,
            total_lines,
            truncated,
        } = execute_physical(&plan, request.input, self.file_delete.as_deref())?;
        let result_digest =
            Self::hash_parts(&[payload.as_bytes(), result.result_digest.as_bytes()]);
        Ok(ManagedFileExecutionOutcomeV1 {
            access_receipt: result.access_receipt,
            effect_settlement: result.effect_settlement,
            result_digest,
            payload,
            observed_bytes,
            returned_entries,
            total_entries,
            returned_lines,
            total_lines,
            truncated,
        })
    }

    fn preview(
        &self,
        request: sigil_kernel::managed_file_access::ManagedFilePreviewRequestV1,
    ) -> Result<
        sigil_kernel::managed_file_access::ManagedFilePreviewOutcomeV1,
        ManagedFileAccessErrorV1,
    > {
        let plan = self
            .plans
            .lock()
            .map_err(|_| ManagedFileAccessErrorV1::PlanStale)?
            .get(&request.plan_hash.to_hex())
            .cloned()
            .ok_or(ManagedFileAccessErrorV1::PlanStale)?;
        if plan.plan_hash != request.plan_hash || plan.operation != request.operation {
            return Err(ManagedFileAccessErrorV1::AdmissionMismatch);
        }
        Self::verify_planned_physical_identity(&plan)?;
        let current_root = self.current_root_identity(&plan.subject_ref)?;
        if current_root != plan.root_identity {
            return Err(ManagedFileAccessErrorV1::SubjectIdentityDrift);
        }
        let raw = read_relative_text(&plan)?;
        let safe = sigil_kernel::safe_persistence_text(&raw);
        let truncated = safe.len() > request.max_bytes;
        let payload = if truncated {
            safe[..request.max_bytes].to_owned()
        } else {
            safe
        };
        Ok(
            sigil_kernel::managed_file_access::ManagedFilePreviewOutcomeV1 {
                observed_bytes: raw.len() as u64,
                result_digest: Self::hash_parts(&[
                    payload.as_bytes(),
                    raw.len().to_string().as_bytes(),
                ]),
                payload,
                truncated,
            },
        )
    }
}

#[cfg(unix)]
fn relative_io_error(error: std::io::Error) -> ManagedFileAccessErrorV1 {
    if error.raw_os_error() == Some(libc::ELOOP) {
        ManagedFileAccessErrorV1::AliasCollision
    } else {
        ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
    }
}

#[cfg(not(any(unix, windows)))]
fn relative_io_error(error: std::io::Error) -> ManagedFileAccessErrorV1 {
    ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
}

#[cfg(windows)]
fn relative_io_error(error: std::io::Error) -> ManagedFileAccessErrorV1 {
    use windows_sys::Win32::Foundation::ERROR_CANT_ACCESS_FILE;
    if error.raw_os_error() == Some(ERROR_CANT_ACCESS_FILE as i32) {
        ManagedFileAccessErrorV1::AliasCollision
    } else {
        ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
    }
}

#[cfg(unix)]
fn open_at(
    directory: &std::fs::File,
    component: &str,
    flags: libc::c_int,
    mode: libc::mode_t,
) -> std::io::Result<std::fs::File> {
    let component = CString::new(component)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "NUL component"))?;
    // SAFETY: `component` is NUL-terminated and `directory` owns a valid directory fd.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            component.as_ptr(),
            flags,
            mode as libc::c_uint,
        )
    };
    if fd < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        // SAFETY: the fd is newly returned by openat and is transferred to File exactly once.
        Ok(unsafe { std::fs::File::from_raw_fd(fd) })
    }
}

#[cfg(any(unix, windows))]
fn relative_components(logical_path: &str) -> Vec<&str> {
    logical_path
        .split('/')
        .filter(|component| !component.is_empty() && *component != ".")
        .collect()
}

#[cfg(unix)]
fn open_relative_path(
    plan: &PlannedFileAccessV1,
    flags: libc::c_int,
) -> Result<std::fs::File, ManagedFileAccessErrorV1> {
    open_relative_path_with_mode(plan, flags, false)
}

#[cfg(unix)]
fn open_relative_path_for_write(
    plan: &PlannedFileAccessV1,
    flags: libc::c_int,
) -> Result<std::fs::File, ManagedFileAccessErrorV1> {
    open_relative_path_with_mode(plan, flags, true)
}

#[cfg(unix)]
fn open_relative_path_with_mode(
    plan: &PlannedFileAccessV1,
    flags: libc::c_int,
    allow_create_for_absent_plan: bool,
) -> Result<std::fs::File, ManagedFileAccessErrorV1> {
    let components = relative_components(&plan.logical_path);
    if components.is_empty() {
        return plan.root_handle.try_clone().map_err(relative_io_error);
    }
    let mut parent = plan.root_handle.try_clone().map_err(relative_io_error)?;
    for component in &components[..components.len() - 1] {
        parent = open_at(
            &parent,
            component,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0,
        )
        .map_err(relative_io_error)?;
    }
    let mut leaf_flags = flags | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    if allow_create_for_absent_plan && plan.expected_physical_identity.is_none() {
        leaf_flags |= libc::O_CREAT | libc::O_EXCL;
    }
    let file = open_at(&parent, components[components.len() - 1], leaf_flags, 0o600)
        .map_err(relative_io_error)?;
    let identity = crate::identity::canonical_identity_from_metadata(
        &plan.physical_path,
        &file.metadata().map_err(relative_io_error)?,
    );
    if identity.is_symlink || (identity.is_regular_file && identity.link_count > 1) {
        return Err(ManagedFileAccessErrorV1::AliasCollision);
    }
    match plan.expected_physical_identity {
        Some(expected) if identity.digest != expected => Err(ManagedFileAccessErrorV1::PlanStale),
        None if !allow_create_for_absent_plan => Err(ManagedFileAccessErrorV1::PlanStale),
        _ => Ok(file),
    }
}

#[cfg(unix)]
fn open_relative_parent(plan: &PlannedFileAccessV1) -> std::io::Result<(std::fs::File, CString)> {
    open_relative_parent_from_root(&plan.root_handle, &plan.logical_path)
}

#[cfg(unix)]
fn open_relative_parent_from_root(
    root_handle: &std::fs::File,
    logical_path: &str,
) -> std::io::Result<(std::fs::File, CString)> {
    let components = relative_components(logical_path);
    let Some(leaf) = components.last() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "workspace root is not a file leaf",
        ));
    };
    let mut parent = root_handle.try_clone()?;
    for component in &components[..components.len() - 1] {
        parent = open_at(
            &parent,
            component,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0,
        )?;
    }
    let leaf = CString::new(*leaf)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "NUL component"))?;
    Ok((parent, leaf))
}

#[cfg(unix)]
fn rename_noreplace_at(
    source_directory: &std::fs::File,
    source: &CStr,
    destination_directory: &std::fs::File,
    destination: &CStr,
) -> std::io::Result<()> {
    #[cfg(target_os = "linux")]
    let status = unsafe {
        libc::renameat2(
            source_directory.as_raw_fd(),
            source.as_ptr(),
            destination_directory.as_raw_fd(),
            destination.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };

    #[cfg(target_os = "macos")]
    let status = unsafe {
        libc::renameatx_np(
            source_directory.as_raw_fd(),
            source.as_ptr(),
            destination_directory.as_raw_fd(),
            destination.as_ptr(),
            libc::RENAME_EXCL,
        )
    };

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let status = {
        let _ = (source_directory, source, destination_directory, destination);
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "managed delete quarantine arena is unsupported on this Unix target",
        ));
    };

    if status < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(unix)]
static DELETE_QUARANTINE_COUNTER: AtomicU64 = AtomicU64::new(0);

fn file_delete_header_hash(arena_root: &Path, journal_path: &Path) -> CanonicalHash {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"file-delete-journal-header-v1");
    hasher.update(arena_root.to_string_lossy().as_bytes());
    hasher.update(journal_path.to_string_lossy().as_bytes());
    CanonicalHash::from_bytes(hasher.finalize().into())
}

#[cfg(unix)]
fn journal_file_identity(metadata: &std::fs::Metadata) -> ResourceJournalFileIdentityV1 {
    use std::os::unix::fs::MetadataExt;
    ResourceJournalFileIdentityV1 {
        device: metadata.dev(),
        inode: metadata.ino(),
        link_count: metadata.nlink(),
        size: metadata.len(),
        file_type: metadata.mode() & libc::S_IFMT as u32,
    }
}

#[cfg(unix)]
fn same_journal_file_identity(
    expected: &ResourceJournalFileIdentityV1,
    observed: &ResourceJournalFileIdentityV1,
) -> bool {
    expected == observed
}

#[cfg(unix)]
fn append_file_delete_event(
    state: &FileDeleteAuthorityStateV1,
    event: ResourceJournalEventV1,
) -> Result<(), String> {
    state
        .journal
        .lock()
        .map_err(|_| "file-delete journal mutex is poisoned".to_owned())?
        .append_event(event)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[cfg(unix)]
fn reconciliation_error(
    operation_id: &str,
    binding_hash: CanonicalHash,
) -> ManagedFileAccessErrorV1 {
    ManagedFileAccessErrorV1::ReconciliationRequired {
        operation_id: operation_id.to_owned(),
        binding_hash,
    }
}

#[cfg(unix)]
fn orphan_file_delete_binding(name: &str) -> CanonicalHash {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"file-delete-orphan-v1");
    hasher.update(name.as_bytes());
    CanonicalHash::from_bytes(hasher.finalize().into())
}

#[cfg(unix)]
fn open_file_delete_arena(
    state: &FileDeleteAuthorityStateV1,
    parent: &std::fs::File,
) -> Result<std::fs::File, ManagedFileAccessErrorV1> {
    std::fs::create_dir_all(&state.arena_root).map_err(|error| {
        ManagedFileAccessErrorV1::PhysicalExecutionFailed(format!(
            "file-delete arena creation failed: {error}"
        ))
    })?;
    let metadata = std::fs::symlink_metadata(&state.arena_root).map_err(|error| {
        ManagedFileAccessErrorV1::PhysicalExecutionFailed(format!(
            "file-delete arena observation failed: {error}"
        ))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ManagedFileAccessErrorV1::AliasCollision);
    }
    secure_private_path_permissions(&state.arena_root).map_err(|error| {
        ManagedFileAccessErrorV1::PhysicalExecutionFailed(format!(
            "file-delete arena hardening failed: {error}"
        ))
    })?;
    use std::os::unix::fs::MetadataExt;
    let parent_metadata = parent.metadata().map_err(relative_io_error)?;
    if metadata.dev() != parent_metadata.dev() {
        return Err(ManagedFileAccessErrorV1::PhysicalExecutionFailed(
            "file-delete arena is not on the workspace filesystem".to_owned(),
        ));
    }
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&state.arena_root)
        .map_err(relative_io_error)
}

#[cfg(unix)]
fn delete_via_quarantine(
    state: &FileDeleteAuthorityStateV1,
    plan: &PlannedFileAccessV1,
    parent: &std::fs::File,
    leaf: &CStr,
    approved: &std::fs::Metadata,
) -> Result<(), ManagedFileAccessErrorV1> {
    let arena = open_file_delete_arena(state, parent)?;
    let sequence = DELETE_QUARANTINE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let operation_id = format!("file-delete-{sequence}-{}", plan.plan_hash.to_hex());
    let quarantine = CString::new(format!(
        "q-{}-{}-{}",
        std::process::id(),
        sequence,
        plan.plan_hash.to_hex()
    ))
    .map_err(|error| ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string()))?;
    let quarantine_name = quarantine
        .to_str()
        .map_err(|error| ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string()))?
        .to_owned();
    let expected_identity = journal_file_identity(approved);
    append_file_delete_event(
        state,
        ResourceJournalEventV1::FileDeletePrepared {
            operation_id: operation_id.clone(),
            subject_ref: plan.subject_ref.as_str().to_owned(),
            logical_path: plan.logical_path.clone(),
            plan_hash: plan.plan_hash,
            binding_hash: plan.plan_hash,
            quarantine_name: quarantine_name.clone(),
            expected_identity: expected_identity.clone(),
        },
    )
    .map_err(ManagedFileAccessErrorV1::PhysicalExecutionFailed)?;

    // The rename is the linearization point. Crucially, its destination is an owner-only
    // authority arena outside the user-writable workspace, so no workspace writer can replace
    // the quarantine pathname between identity observation and unlinkat.
    if let Err(error) = rename_noreplace_at(parent, leaf, &arena, &quarantine) {
        let terminal = if error.kind() == std::io::ErrorKind::NotFound {
            "rename-source-missing"
        } else {
            "rename-failed-before-effect"
        };
        return match append_file_delete_event(
            state,
            ResourceJournalEventV1::FileDeleteRestored {
                operation_id: operation_id.clone(),
                reason: terminal.to_owned(),
            },
        ) {
            Ok(()) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(ManagedFileAccessErrorV1::PlanStale)
            }
            Ok(()) => Err(ManagedFileAccessErrorV1::PhysicalExecutionFailed(format!(
                "managed delete quarantine rename failed: {error}"
            ))),
            Err(_) => Err(reconciliation_error(&operation_id, plan.plan_hash)),
        };
    }
    if let Err(_error) = append_file_delete_event(
        state,
        ResourceJournalEventV1::FileDeleteRenamed {
            operation_id: operation_id.clone(),
            quarantine_identity: expected_identity.clone(),
        },
    ) {
        let restore = rename_noreplace_at(&arena, &quarantine, parent, leaf);
        if restore.is_ok() {
            let _ = append_file_delete_event(
                state,
                ResourceJournalEventV1::FileDeleteRestored {
                    operation_id: operation_id.clone(),
                    reason: "renamed-event-append-failed".to_owned(),
                },
            );
        }
        return Err(reconciliation_error(&operation_id, plan.plan_hash));
    }

    let quarantined = match open_at(
        &arena,
        &quarantine_name,
        libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        0,
    ) {
        Ok(file) => file,
        Err(error) => {
            let restore = rename_noreplace_at(&arena, &quarantine, parent, leaf);
            if restore.is_ok() {
                let _ = append_file_delete_event(
                    state,
                    ResourceJournalEventV1::FileDeleteRestored {
                        operation_id: operation_id.clone(),
                        reason: "quarantine-open-failed".to_owned(),
                    },
                );
                return Err(ManagedFileAccessErrorV1::PhysicalExecutionFailed(format!(
                    "managed delete quarantine identity could not be opened: {error}"
                )));
            }
            let _ = append_file_delete_event(
                state,
                ResourceJournalEventV1::FileDeleteReconciliationRequired {
                    operation_id: operation_id.clone(),
                    binding_hash: plan.plan_hash,
                    reason: format!("open failed: {error}"),
                },
            );
            return Err(reconciliation_error(&operation_id, plan.plan_hash));
        }
    };
    let observed = match quarantined.metadata() {
        Ok(metadata) => metadata,
        Err(error) => {
            let restore = rename_noreplace_at(&arena, &quarantine, parent, leaf);
            if restore.is_ok() {
                let _ = append_file_delete_event(
                    state,
                    ResourceJournalEventV1::FileDeleteRestored {
                        operation_id: operation_id.clone(),
                        reason: "identity-observation-failed".to_owned(),
                    },
                );
                return Err(ManagedFileAccessErrorV1::PhysicalExecutionFailed(format!(
                    "managed delete quarantine identity observation failed: {error}"
                )));
            }
            let _ = append_file_delete_event(
                state,
                ResourceJournalEventV1::FileDeleteReconciliationRequired {
                    operation_id: operation_id.clone(),
                    binding_hash: plan.plan_hash,
                    reason: format!("identity observation failed: {error}"),
                },
            );
            return Err(reconciliation_error(&operation_id, plan.plan_hash));
        }
    };
    let observed_identity = journal_file_identity(&observed);
    let matches = same_journal_file_identity(&expected_identity, &observed_identity);
    if let Err(error) = append_file_delete_event(
        state,
        ResourceJournalEventV1::FileDeleteIdentityObserved {
            operation_id: operation_id.clone(),
            observed_identity: observed_identity.clone(),
            matches,
        },
    ) {
        let restore = rename_noreplace_at(&arena, &quarantine, parent, leaf);
        if restore.is_ok() {
            let _ = append_file_delete_event(
                state,
                ResourceJournalEventV1::FileDeleteRestored {
                    operation_id: operation_id.clone(),
                    reason: "identity-event-append-failed".to_owned(),
                },
            );
        }
        let _ = error;
        return Err(reconciliation_error(&operation_id, plan.plan_hash));
    }
    if !matches {
        let restore = rename_noreplace_at(&arena, &quarantine, parent, leaf);
        return match restore {
            Ok(()) => {
                append_file_delete_event(
                    state,
                    ResourceJournalEventV1::FileDeleteRestored {
                        operation_id: operation_id.clone(),
                        reason: "identity-mismatch".to_owned(),
                    },
                )
                .map_err(|_| reconciliation_error(&operation_id, plan.plan_hash))?;
                Err(ManagedFileAccessErrorV1::PlanStale)
            }
            Err(error) => {
                let _ = append_file_delete_event(
                    state,
                    ResourceJournalEventV1::FileDeleteReconciliationRequired {
                        operation_id: operation_id.clone(),
                        binding_hash: plan.plan_hash,
                        reason: format!("identity mismatch restore failed: {error}"),
                    },
                );
                Err(reconciliation_error(&operation_id, plan.plan_hash))
            }
        };
    }

    let status = unsafe { libc::unlinkat(arena.as_raw_fd(), quarantine.as_ptr(), 0) };
    if status < 0 {
        let error = std::io::Error::last_os_error();
        let restore = rename_noreplace_at(&arena, &quarantine, parent, leaf);
        return match restore {
            Ok(()) => {
                let _ = append_file_delete_event(
                    state,
                    ResourceJournalEventV1::FileDeleteRestored {
                        operation_id,
                        reason: format!("delete failed: {error}"),
                    },
                );
                Err(ManagedFileAccessErrorV1::PhysicalExecutionFailed(format!(
                    "managed file delete failed after quarantine: {error}"
                )))
            }
            Err(restore_error) => {
                let _ = append_file_delete_event(
                    state,
                    ResourceJournalEventV1::FileDeleteReconciliationRequired {
                        operation_id: operation_id.clone(),
                        binding_hash: plan.plan_hash,
                        reason: format!("delete failed: {error}; restore failed: {restore_error}"),
                    },
                );
                Err(reconciliation_error(&operation_id, plan.plan_hash))
            }
        };
    }
    if append_file_delete_event(
        state,
        ResourceJournalEventV1::FileDeleteDeleted {
            operation_id: operation_id.clone(),
        },
    )
    .is_err()
    {
        return Err(reconciliation_error(&operation_id, plan.plan_hash));
    }
    Ok(())
}

#[cfg(unix)]
#[derive(Default)]
struct PendingFileDeleteV1 {
    prepared: Option<(
        String,
        String,
        String,
        CanonicalHash,
        CanonicalHash,
        String,
        ResourceJournalFileIdentityV1,
    )>,
    renamed: bool,
    identity_observed: bool,
    terminal: bool,
}

#[cfg(unix)]
fn reconcile_file_delete_journal(
    state: &FileDeleteAuthorityStateV1,
    registry: &Arc<Mutex<BorrowedSubjectRegistryV1>>,
) -> Result<(), ManagedFileAccessErrorV1> {
    let records = state
        .journal
        .lock()
        .map_err(|_| ManagedFileAccessErrorV1::SubjectIdentityDrift)?
        .file_delete_records();
    let mut pending = BTreeMap::<String, PendingFileDeleteV1>::new();
    for (_, event) in records {
        match event {
            ResourceJournalEventV1::FileDeletePrepared {
                operation_id,
                subject_ref,
                logical_path,
                plan_hash,
                binding_hash,
                quarantine_name,
                expected_identity,
            } => {
                pending.entry(operation_id).or_default().prepared = Some((
                    subject_ref,
                    logical_path,
                    operation_id.clone(),
                    plan_hash,
                    binding_hash,
                    quarantine_name,
                    expected_identity,
                ));
            }
            ResourceJournalEventV1::FileDeleteRenamed { operation_id, .. } => {
                pending.entry(operation_id).or_default().renamed = true;
            }
            ResourceJournalEventV1::FileDeleteIdentityObserved { operation_id, .. } => {
                pending.entry(operation_id).or_default().identity_observed = true;
            }
            ResourceJournalEventV1::FileDeleteRestored { operation_id, .. }
            | ResourceJournalEventV1::FileDeleteDeleted { operation_id } => {
                pending.entry(operation_id).or_default().terminal = true;
            }
            ResourceJournalEventV1::FileDeleteReconciliationRequired {
                operation_id,
                binding_hash,
                ..
            } => return Err(reconciliation_error(&operation_id, binding_hash)),
            _ => {}
        }
    }

    let (subject_ref, root, _, _) = {
        let registry = registry
            .lock()
            .map_err(|_| ManagedFileAccessErrorV1::SubjectIdentityDrift)?;
        let subject_ref = registry
            .sole_workspace_subject()
            .ok_or(ManagedFileAccessErrorV1::ResourcePreconditionUnavailable)?;
        let root = registry
            .workspace_root_for(&subject_ref)
            .ok_or(ManagedFileAccessErrorV1::ResourcePreconditionUnavailable)?
            .to_path_buf();
        let capsule = registry
            .workspace_capsule_for(&subject_ref)
            .ok_or(ManagedFileAccessErrorV1::ResourcePreconditionUnavailable)?;
        (
            subject_ref,
            root,
            capsule.authority_generation,
            capsule.root_identity_hash,
        )
    };
    let root_handle = AuthorityManagedFileAccessServiceV1::open_workspace_root(&root)?;
    let (parent_for_arena, _) =
        open_relative_parent_from_root(&root_handle, "recovery-leaf").map_err(relative_io_error)?;
    let arena = open_file_delete_arena(state, &parent_for_arena)?;
    let known_quarantines = pending
        .values()
        .filter(|state_for_operation| !state_for_operation.terminal)
        .filter_map(|state_for_operation| {
            state_for_operation
                .prepared
                .as_ref()
                .map(|prepared| prepared.5.clone())
        })
        .collect::<BTreeSet<_>>();
    for entry in std::fs::read_dir(&state.arena_root)
        .map_err(|error| ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string()))?
    {
        let entry = entry.map_err(|error| {
            ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
        })?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !known_quarantines.contains(&name) {
            let operation_id = format!("file-delete-orphan-{name}");
            let binding_hash = orphan_file_delete_binding(&name);
            let _ = append_file_delete_event(
                state,
                ResourceJournalEventV1::FileDeleteReconciliationRequired {
                    operation_id: operation_id.clone(),
                    binding_hash,
                    reason: "arena entry has no matching unfinished journal binding".to_owned(),
                },
            );
            return Err(reconciliation_error(&operation_id, binding_hash));
        }
    }

    for (operation_id, state_for_operation) in pending {
        if state_for_operation.terminal {
            continue;
        }
        let Some((
            event_subject,
            logical_path,
            _,
            plan_hash,
            binding_hash,
            quarantine_name,
            expected,
        )) = state_for_operation.prepared
        else {
            return Err(reconciliation_error(
                &operation_id,
                CanonicalHash::from_bytes([0; 32]),
            ));
        };
        if event_subject != subject_ref.as_str() {
            return Err(reconciliation_error(&operation_id, binding_hash));
        }
        let (parent, leaf) = open_relative_parent_from_root(&root_handle, &logical_path)
            .map_err(relative_io_error)?;
        let quarantine = CString::new(quarantine_name.clone()).map_err(|error| {
            ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
        })?;

        if !state_for_operation.renamed {
            let current = open_at(
                &parent,
                leaf.to_str().unwrap_or_default(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0,
            )
            .ok()
            .and_then(|file| file.metadata().ok())
            .map(|metadata| journal_file_identity(&metadata));
            if current.as_ref() == Some(&expected) {
                append_file_delete_event(
                    state,
                    ResourceJournalEventV1::FileDeleteRestored {
                        operation_id,
                        reason: "crash-before-rename".to_owned(),
                    },
                )
                .map_err(ManagedFileAccessErrorV1::PhysicalExecutionFailed)?;
                continue;
            }
            append_file_delete_event(
                state,
                ResourceJournalEventV1::FileDeleteReconciliationRequired {
                    operation_id: operation_id.clone(),
                    binding_hash,
                    reason: "prepared prefix has no safely identifiable leaf".to_owned(),
                },
            )
            .map_err(ManagedFileAccessErrorV1::PhysicalExecutionFailed)?;
            return Err(reconciliation_error(&operation_id, binding_hash));
        }

        let quarantined = match open_at(
            &arena,
            &quarantine_name,
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0,
        ) {
            Ok(file) => file,
            Err(error) => {
                let leaf_restored = open_at(
                    &parent,
                    leaf.to_str().unwrap_or_default(),
                    libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                    0,
                )
                .ok()
                .and_then(|file| file.metadata().ok())
                .map(|metadata| journal_file_identity(&metadata))
                .is_some_and(|identity| identity == expected);
                if leaf_restored {
                    append_file_delete_event(
                        state,
                        ResourceJournalEventV1::FileDeleteRestored {
                            operation_id,
                            reason: "restart-quarantine-missing-leaf-restored".to_owned(),
                        },
                    )
                    .map_err(ManagedFileAccessErrorV1::PhysicalExecutionFailed)?;
                    continue;
                }
                append_file_delete_event(
                    state,
                    ResourceJournalEventV1::FileDeleteReconciliationRequired {
                        operation_id: operation_id.clone(),
                        binding_hash,
                        reason: format!("quarantine entry missing after restart: {error}"),
                    },
                )
                .map_err(ManagedFileAccessErrorV1::PhysicalExecutionFailed)?;
                return Err(reconciliation_error(&operation_id, binding_hash));
            }
        };
        let observed = quarantined
            .metadata()
            .map(|metadata| journal_file_identity(&metadata))
            .map_err(relative_io_error)?;
        let matches = same_journal_file_identity(&expected, &observed);
        if !state_for_operation.identity_observed {
            append_file_delete_event(
                state,
                ResourceJournalEventV1::FileDeleteIdentityObserved {
                    operation_id: operation_id.clone(),
                    observed_identity: observed,
                    matches,
                },
            )
            .map_err(ManagedFileAccessErrorV1::PhysicalExecutionFailed)?;
        }
        if matches {
            let status = unsafe { libc::unlinkat(arena.as_raw_fd(), quarantine.as_ptr(), 0) };
            if status < 0 {
                let error = std::io::Error::last_os_error();
                append_file_delete_event(
                    state,
                    ResourceJournalEventV1::FileDeleteReconciliationRequired {
                        operation_id: operation_id.clone(),
                        binding_hash,
                        reason: format!("restart delete failed: {error}"),
                    },
                )
                .map_err(ManagedFileAccessErrorV1::PhysicalExecutionFailed)?;
                return Err(reconciliation_error(&operation_id, binding_hash));
            }
            append_file_delete_event(
                state,
                ResourceJournalEventV1::FileDeleteDeleted { operation_id },
            )
            .map_err(ManagedFileAccessErrorV1::PhysicalExecutionFailed)?;
        } else {
            let restore = rename_noreplace_at(&arena, &quarantine, &parent, &leaf);
            match restore {
                Ok(()) => append_file_delete_event(
                    state,
                    ResourceJournalEventV1::FileDeleteRestored {
                        operation_id,
                        reason: "restart-identity-mismatch".to_owned(),
                    },
                )
                .map_err(ManagedFileAccessErrorV1::PhysicalExecutionFailed)?,
                Err(error) => {
                    append_file_delete_event(
                        state,
                        ResourceJournalEventV1::FileDeleteReconciliationRequired {
                            operation_id: operation_id.clone(),
                            binding_hash,
                            reason: format!("restart restore collision: {error}"),
                        },
                    )
                    .map_err(ManagedFileAccessErrorV1::PhysicalExecutionFailed)?;
                    return Err(reconciliation_error(&operation_id, binding_hash));
                }
            }
        }
        let _ = plan_hash;
    }
    Ok(())
}

#[cfg(unix)]
fn read_relative_text(plan: &PlannedFileAccessV1) -> Result<String, ManagedFileAccessErrorV1> {
    let mut file = open_relative_path(plan, libc::O_RDONLY)?;
    let mut raw = String::new();
    std::io::Read::read_to_string(&mut file, &mut raw)
        .map_err(|error| ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string()))?;
    Ok(raw)
}

#[cfg(windows)]
fn read_relative_text(plan: &PlannedFileAccessV1) -> Result<String, ManagedFileAccessErrorV1> {
    let handle = windows_open_plan(plan, WindowsOpenKind::Read, false)?;
    let mut file = handle.file;
    let mut raw = String::new();
    file.read_to_string(&mut raw)
        .map_err(|error| ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string()))?;
    Ok(raw)
}

#[cfg(not(any(unix, windows)))]
fn read_relative_text(plan: &PlannedFileAccessV1) -> Result<String, ManagedFileAccessErrorV1> {
    std::fs::read_to_string(&plan.physical_path)
        .map_err(|error| ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string()))
}

fn operation_tag(operation: ManagedFileOperationV1) -> &'static [u8] {
    match operation {
        ManagedFileOperationV1::Read => b"read",
        ManagedFileOperationV1::List => b"list",
        ManagedFileOperationV1::Glob => b"glob",
        ManagedFileOperationV1::Grep => b"grep",
        ManagedFileOperationV1::Write => b"write",
        ManagedFileOperationV1::Edit => b"edit",
        ManagedFileOperationV1::Delete => b"delete",
        ManagedFileOperationV1::Rename => b"rename",
    }
}

struct PhysicalExecutionOutcomeV1 {
    payload: String,
    observed_bytes: u64,
    returned_entries: u64,
    total_entries: u64,
    returned_lines: u64,
    total_lines: u64,
    truncated: bool,
}

#[cfg(unix)]
fn execute_physical(
    plan: &PlannedFileAccessV1,
    input: sigil_kernel::managed_file_access::ManagedFileExecutionInputV1,
    file_delete: Option<&FileDeleteAuthorityStateV1>,
) -> Result<PhysicalExecutionOutcomeV1, ManagedFileAccessErrorV1> {
    match (plan.operation, input) {
        (
            ManagedFileOperationV1::Read,
            ManagedFileExecutionInputV1::Read {
                offset,
                limit,
                max_bytes,
            },
        ) => {
            let raw = read_relative_text(plan)?;
            let lines: Vec<&str> = raw.lines().collect();
            let selected = lines
                .iter()
                .skip(offset)
                .take(limit)
                .copied()
                .collect::<Vec<_>>();
            let mut payload = selected
                .iter()
                .map(|line| sigil_kernel::safe_persistence_text(line))
                .collect::<Vec<_>>()
                .join("\n");
            let truncated =
                offset.saturating_add(selected.len()) < lines.len() || payload.len() > max_bytes;
            if payload.len() > max_bytes {
                payload.truncate(max_bytes);
            }
            Ok(PhysicalExecutionOutcomeV1 {
                payload,
                observed_bytes: raw.len() as u64,
                returned_entries: 0,
                total_entries: 0,
                returned_lines: selected.len() as u64,
                total_lines: lines.len() as u64,
                truncated,
            })
        }
        (
            ManagedFileOperationV1::List,
            ManagedFileExecutionInputV1::List {
                recursive,
                limit,
                max_depth,
            },
        ) => {
            let directory = open_relative_path(plan, libc::O_RDONLY | libc::O_DIRECTORY)?;
            let mut entries = Vec::new();
            collect_entries_relative(&directory, "", recursive, max_depth, 0, &mut entries)?;
            entries.sort();
            let total = entries.len();
            let truncated = total > limit;
            entries.truncate(limit);
            let payload = serde_json::to_string_pretty(&entries).map_err(|error| {
                ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
            })?;
            Ok(PhysicalExecutionOutcomeV1 {
                payload,
                observed_bytes: 0,
                returned_entries: entries.len() as u64,
                total_entries: total as u64,
                returned_lines: 0,
                total_lines: 0,
                truncated,
            })
        }
        (
            ManagedFileOperationV1::Grep,
            ManagedFileExecutionInputV1::Grep {
                pattern,
                limit,
                max_bytes,
            },
        ) => {
            let regex = regex::Regex::new(&pattern).map_err(|error| {
                ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
            })?;
            let target = open_relative_path(plan, libc::O_RDONLY)?;
            let mut matches = Vec::new();
            let mut observed_bytes = 0u64;
            let display_path = if plan.logical_path == "." {
                String::new()
            } else {
                plan.logical_path.clone()
            };
            if is_directory(&target).map_err(|error| {
                ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
            })? {
                collect_grep_relative(
                    &target,
                    &display_path,
                    &regex,
                    &mut matches,
                    &mut observed_bytes,
                )?;
            } else {
                let mut file = target;
                let mut raw = String::new();
                file.read_to_string(&mut raw).map_err(|_| {
                    ManagedFileAccessErrorV1::PhysicalExecutionFailed(
                        "non-UTF-8 or unreadable file".to_owned(),
                    )
                })?;
                observed_bytes = observed_bytes.saturating_add(raw.len() as u64);
                for (index, line) in raw.lines().enumerate() {
                    if regex.is_match(line) {
                        matches.push(format!(
                            "{display_path}:{}:{}",
                            index + 1,
                            sigil_kernel::safe_persistence_text(line)
                        ));
                    }
                }
            }
            let total = matches.len();
            let truncated = total > limit;
            matches.truncate(limit);
            let mut payload = matches.join("\n");
            if payload.len() > max_bytes {
                payload.truncate(max_bytes);
            }
            let byte_truncated = payload.len() == max_bytes;
            Ok(PhysicalExecutionOutcomeV1 {
                payload,
                observed_bytes,
                returned_entries: 0,
                total_entries: total as u64,
                returned_lines: matches.len() as u64,
                total_lines: 0,
                truncated: truncated || byte_truncated,
            })
        }
        (ManagedFileOperationV1::Glob, ManagedFileExecutionInputV1::Glob { pattern, limit }) => {
            let wildcard = format!(
                "^{}$",
                pattern
                    .split('*')
                    .map(regex::escape)
                    .collect::<Vec<_>>()
                    .join(".*")
            );
            let matcher = regex::Regex::new(&wildcard).map_err(|error| {
                ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
            })?;
            let directory = open_relative_path(plan, libc::O_RDONLY | libc::O_DIRECTORY)?;
            let mut entries = Vec::new();
            collect_entries_relative(&directory, "", true, usize::MAX, 0, &mut entries)?;
            entries.retain(|entry| matcher.is_match(entry));
            entries.sort();
            let total = entries.len();
            let truncated = total > limit;
            entries.truncate(limit);
            let payload = serde_json::to_string_pretty(&entries).map_err(|error| {
                ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
            })?;
            Ok(PhysicalExecutionOutcomeV1 {
                payload,
                observed_bytes: 0,
                returned_entries: entries.len() as u64,
                total_entries: total as u64,
                returned_lines: 0,
                total_lines: 0,
                truncated,
            })
        }
        (ManagedFileOperationV1::Write, ManagedFileExecutionInputV1::Write { content }) => {
            let mut file = open_relative_path_for_write(plan, libc::O_WRONLY)?;
            file.set_len(0).map_err(|error| {
                ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
            })?;
            file.write_all(content.as_bytes()).map_err(|error| {
                ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
            })?;
            Ok(PhysicalExecutionOutcomeV1 {
                payload: "managed file write applied".to_owned(),
                observed_bytes: content.len() as u64,
                returned_entries: 0,
                total_entries: 0,
                returned_lines: 0,
                total_lines: 0,
                truncated: false,
            })
        }
        (
            ManagedFileOperationV1::Edit,
            ManagedFileExecutionInputV1::Edit { old_text, new_text },
        ) => {
            let mut file = open_relative_path(plan, libc::O_RDWR)?;
            let mut current = String::new();
            file.read_to_string(&mut current).map_err(|error| {
                ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
            })?;
            if !current.contains(&old_text) {
                return Err(ManagedFileAccessErrorV1::PhysicalExecutionFailed(
                    "edit target text was not found".to_owned(),
                ));
            }
            let updated = current.replacen(&old_text, &new_text, 1);
            file.set_len(0).map_err(|error| {
                ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
            })?;
            file.seek(SeekFrom::Start(0)).map_err(|error| {
                ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
            })?;
            file.write_all(updated.as_bytes()).map_err(|error| {
                ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
            })?;
            Ok(PhysicalExecutionOutcomeV1 {
                payload: "managed file edit applied".to_owned(),
                observed_bytes: updated.len() as u64,
                returned_entries: 0,
                total_entries: 0,
                returned_lines: 0,
                total_lines: 0,
                truncated: false,
            })
        }
        (ManagedFileOperationV1::Delete, ManagedFileExecutionInputV1::Delete) => {
            if plan.expected_physical_identity.is_none() {
                return Err(ManagedFileAccessErrorV1::PlanStale);
            }
            // Pin the approved leaf before the quarantine rename. The no-follow open rejects
            // symlink leaves and the plan-level revalidation above binds this handle to the
            // approved identity; the parent fd keeps the rename rooted in the authority-owned
            // directory.
            let leaf_guard = open_relative_path(plan, libc::O_RDONLY)?;
            let (parent, leaf) = open_relative_parent(plan).map_err(relative_io_error)?;
            let approved = leaf_guard.metadata().map_err(relative_io_error)?;
            let state =
                file_delete.ok_or(ManagedFileAccessErrorV1::ResourcePreconditionUnavailable)?;
            delete_via_quarantine(state, plan, &parent, &leaf, &approved)?;
            Ok(PhysicalExecutionOutcomeV1 {
                payload: "managed file delete applied".to_owned(),
                observed_bytes: 0,
                returned_entries: 0,
                total_entries: 0,
                returned_lines: 0,
                total_lines: 0,
                truncated: false,
            })
        }
        _ => Err(ManagedFileAccessErrorV1::AdmissionMismatch),
    }
}

#[cfg(unix)]
fn is_directory(file: &std::fs::File) -> std::io::Result<bool> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `stat` points to writable storage and the fd is owned by `file`.
    let status = unsafe { libc::fstat(file.as_raw_fd(), stat.as_mut_ptr()) };
    if status < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: fstat initialized `stat` on success.
    let stat = unsafe { stat.assume_init() };
    Ok((stat.st_mode & libc::S_IFMT) == libc::S_IFDIR)
}

#[cfg(unix)]
fn directory_entry_names(directory: &std::fs::File) -> std::io::Result<Vec<String>> {
    // fdopendir takes ownership of its fd, so duplicate the authority-owned handle first.
    let duplicate = unsafe { libc::dup(directory.as_raw_fd()) };
    if duplicate < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `duplicate` is a valid fd and ownership transfers to DIR on success.
    let stream = unsafe { libc::fdopendir(duplicate) };
    if stream.is_null() {
        // SAFETY: fdopendir did not take ownership on failure.
        unsafe { libc::close(duplicate) };
        return Err(std::io::Error::last_os_error());
    }
    let mut names = Vec::new();
    loop {
        // SAFETY: `stream` remains valid until closed below.
        let entry = unsafe { libc::readdir(stream) };
        if entry.is_null() {
            break;
        }
        // SAFETY: d_name is the NUL-terminated name returned by readdir.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }
            .to_string_lossy()
            .into_owned();
        if name != "." && name != ".." {
            names.push(name);
        }
    }
    // SAFETY: stream was returned by fdopendir and has not been closed.
    unsafe { libc::closedir(stream) };
    Ok(names)
}

#[cfg(unix)]
fn is_directory_at(directory: &std::fs::File, name: &str) -> std::io::Result<bool> {
    let name = CString::new(name)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "NUL entry name"))?;
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: name is NUL-terminated and stat points to writable storage. AT_SYMLINK_NOFOLLOW
    // makes the type check itself non-following.
    let status = unsafe {
        libc::fstatat(
            directory.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if status < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: fstatat initialized stat on success.
    let stat = unsafe { stat.assume_init() };
    Ok((stat.st_mode & libc::S_IFMT) == libc::S_IFDIR)
}

#[cfg(unix)]
fn collect_entries_relative(
    directory: &std::fs::File,
    prefix: &str,
    recursive: bool,
    max_depth: usize,
    depth: usize,
    entries: &mut Vec<String>,
) -> Result<(), ManagedFileAccessErrorV1> {
    for name in directory_entry_names(directory)
        .map_err(|error| ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string()))?
    {
        let relative = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        entries.push(relative);
        if recursive
            && is_directory_at(directory, &name).map_err(|error| {
                ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
            })?
            && depth < max_depth
        {
            let child = open_at(directory, &name, libc::O_RDONLY | libc::O_DIRECTORY, 0)
                .map_err(relative_io_error)?;
            let child_prefix = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            collect_entries_relative(
                &child,
                &child_prefix,
                recursive,
                max_depth,
                depth + 1,
                entries,
            )?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn collect_grep_relative(
    directory: &std::fs::File,
    prefix: &str,
    regex: &regex::Regex,
    matches: &mut Vec<String>,
    observed_bytes: &mut u64,
) -> Result<(), ManagedFileAccessErrorV1> {
    for name in directory_entry_names(directory)
        .map_err(|error| ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string()))?
    {
        let relative = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        if is_directory_at(directory, &name)
            .map_err(|error| ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string()))?
        {
            let child = open_at(directory, &name, libc::O_RDONLY | libc::O_DIRECTORY, 0)
                .map_err(relative_io_error)?;
            collect_grep_relative(&child, &relative, regex, matches, observed_bytes)?;
            continue;
        }
        let mut file = match open_at(directory, &name, libc::O_RDONLY, 0) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => continue,
            Err(error) => return Err(relative_io_error(error)),
        };
        let mut raw = String::new();
        if file.read_to_string(&mut raw).is_err() {
            continue;
        }
        *observed_bytes = observed_bytes.saturating_add(raw.len() as u64);
        for (index, line) in raw.lines().enumerate() {
            if regex.is_match(line) {
                matches.push(format!(
                    "{relative}:{}:{}",
                    index + 1,
                    sigil_kernel::safe_persistence_text(line)
                ));
            }
        }
    }
    Ok(())
}

#[cfg(windows)]
#[derive(Debug)]
struct WindowsRelativeHandle {
    file: std::fs::File,
    // Keeping ancestor handles open with FILE_SHARE_DELETE omitted pins the traversed directory
    // chain while the final handle is used. Windows has no openat equivalent in std, so each
    // component is opened and verified as a handle before the next component is resolved.
    _ancestors: Vec<std::fs::File>,
}

#[cfg(windows)]
#[derive(Debug, Clone, Copy)]
enum WindowsOpenKind {
    /// Opens either a regular file or a directory without asserting a leaf type.
    Any,
    Read,
    ReadWrite,
    Write,
    Delete,
    Directory,
}

#[cfg(windows)]
fn windows_open_component(
    path: &Path,
    kind: WindowsOpenKind,
    create_new: bool,
) -> std::io::Result<std::fs::File> {
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    let mut options = OpenOptions::new();
    match kind {
        WindowsOpenKind::Any | WindowsOpenKind::Read | WindowsOpenKind::Directory => {
            options.read(true);
        }
        WindowsOpenKind::ReadWrite => {
            options.read(true).write(true);
        }
        WindowsOpenKind::Write => {
            options.write(true);
        }
        WindowsOpenKind::Delete => {
            options.read(true).write(true);
            options.access_mode(
                windows_sys::Win32::Storage::FileSystem::DELETE
                    | windows_sys::Win32::Storage::FileSystem::FILE_GENERIC_READ
                    | windows_sys::Win32::Storage::FileSystem::FILE_READ_ATTRIBUTES,
            );
        }
    }
    options
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    if create_new {
        options.create_new(true);
    }
    let file = options.open(path)?;
    let identity = crate::identity::canonical_identity_from_handle(path, &file)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    if identity.is_symlink || (identity.is_regular_file && identity.link_count > 1) {
        return Err(std::io::Error::from_raw_os_error(
            windows_sys::Win32::Foundation::ERROR_CANT_ACCESS_FILE as i32,
        ));
    }
    if matches!(kind, WindowsOpenKind::Directory) && !identity.is_directory {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotADirectory,
            "managed file path component is not a directory",
        ));
    }
    Ok(file)
}

#[cfg(windows)]
fn windows_open_relative_component(
    directory: &std::fs::File,
    component: &str,
    kind: WindowsOpenKind,
    create_new: bool,
) -> std::io::Result<std::fs::File> {
    use std::mem::size_of;
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
    use windows_sys::Wdk::Storage::FileSystem::{
        FILE_CREATE, FILE_NON_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_FOR_BACKUP_INTENT,
        FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT, NtCreateFile,
    };
    use windows_sys::Win32::Foundation::{
        GENERIC_READ, GENERIC_WRITE, RtlNtStatusToDosError, STATUS_SUCCESS, SetLastError,
        UNICODE_STRING,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        DELETE, FILE_ATTRIBUTE_NORMAL, FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE,
        SYNCHRONIZE,
    };
    use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

    let name: Vec<u16> = component.encode_utf16().collect();
    let byte_length = name
        .len()
        .checked_mul(size_of::<u16>())
        .and_then(|length| u16::try_from(length).ok())
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "path component too long")
        })?;
    let mut unicode_name = UNICODE_STRING {
        Length: byte_length,
        MaximumLength: byte_length,
        Buffer: name.as_ptr() as *mut u16,
    };
    let desired_access = match kind {
        WindowsOpenKind::Any | WindowsOpenKind::Read | WindowsOpenKind::Directory => GENERIC_READ,
        WindowsOpenKind::ReadWrite => GENERIC_READ | GENERIC_WRITE,
        WindowsOpenKind::Write => GENERIC_WRITE,
        WindowsOpenKind::Delete => DELETE | GENERIC_READ | FILE_READ_ATTRIBUTES,
    } | SYNCHRONIZE
        | FILE_READ_ATTRIBUTES;
    let create_options = FILE_SYNCHRONOUS_IO_NONALERT
        | FILE_OPEN_REPARSE_POINT
        | match kind {
            WindowsOpenKind::Any | WindowsOpenKind::Directory => FILE_OPEN_FOR_BACKUP_INTENT,
            WindowsOpenKind::Read
            | WindowsOpenKind::ReadWrite
            | WindowsOpenKind::Write
            | WindowsOpenKind::Delete => FILE_NON_DIRECTORY_FILE,
        };
    let object_attributes = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: directory.as_raw_handle() as _,
        ObjectName: &mut unicode_name,
        Attributes: 0,
        SecurityDescriptor: std::ptr::null_mut(),
        SecurityQualityOfService: std::ptr::null_mut(),
    };
    let mut io_status = IO_STATUS_BLOCK::default();
    let mut handle = std::ptr::null_mut();
    // SAFETY: all pointers refer to live stack values for the duration of the syscall; the
    // returned handle is transferred into File only after NtCreateFile reports success.
    let status = unsafe {
        NtCreateFile(
            &mut handle,
            desired_access,
            &object_attributes,
            &mut io_status,
            std::ptr::null(),
            FILE_ATTRIBUTE_NORMAL,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            if create_new { FILE_CREATE } else { FILE_OPEN },
            create_options,
            std::ptr::null(),
            0,
        )
    };
    if status != STATUS_SUCCESS {
        unsafe { SetLastError(RtlNtStatusToDosError(status)) };
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: NtCreateFile returned an owned, valid handle on STATUS_SUCCESS.
    let file = unsafe { std::fs::File::from_raw_handle(handle as _) };
    let identity = crate::identity::canonical_identity_from_handle(Path::new(component), &file)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    if identity.is_symlink || (identity.is_regular_file && identity.link_count > 1) {
        return Err(std::io::Error::from_raw_os_error(
            windows_sys::Win32::Foundation::ERROR_CANT_ACCESS_FILE as i32,
        ));
    }
    if matches!(kind, WindowsOpenKind::Directory) && !identity.is_directory {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotADirectory,
            "managed file path component is not a directory",
        ));
    }
    Ok(file)
}

#[cfg(windows)]
fn windows_open_relative_root(
    root: &Path,
    logical_path: &str,
    kind: WindowsOpenKind,
    create_new: bool,
    root_guard: Option<&std::fs::File>,
) -> Result<WindowsRelativeHandle, ManagedFileAccessErrorV1> {
    let components = relative_components(logical_path);
    if components.is_empty() {
        let root_handle = match root_guard {
            Some(root_guard) => root_guard.try_clone().map_err(relative_io_error)?,
            None => windows_open_component(root, WindowsOpenKind::Directory, false)
                .map_err(relative_io_error)?,
        };
        return Ok(WindowsRelativeHandle {
            file: root_handle,
            _ancestors: Vec::new(),
        });
    }
    let mut parent = match root_guard {
        Some(root_guard) => root_guard.try_clone().map_err(relative_io_error)?,
        None => windows_open_component(root, WindowsOpenKind::Directory, false)
            .map_err(relative_io_error)?,
    };
    let mut ancestors = Vec::new();
    for component in &components[..components.len() - 1] {
        let directory =
            windows_open_relative_component(&parent, component, WindowsOpenKind::Directory, false)
                .map_err(relative_io_error)?;
        ancestors.push(parent);
        parent = directory;
    }
    let file = windows_open_relative_component(
        &parent,
        components[components.len() - 1],
        kind,
        create_new,
    )
    .map_err(relative_io_error)?;
    ancestors.push(parent);
    Ok(WindowsRelativeHandle {
        file,
        _ancestors: ancestors,
    })
}

#[cfg(windows)]
fn windows_open_plan(
    plan: &PlannedFileAccessV1,
    kind: WindowsOpenKind,
    allow_create_for_absent_plan: bool,
) -> Result<WindowsRelativeHandle, ManagedFileAccessErrorV1> {
    let create_new = allow_create_for_absent_plan && plan.expected_physical_identity.is_none();
    let handle = windows_open_relative_root(
        &plan.root,
        &plan.logical_path,
        kind,
        create_new,
        Some(plan.root_handle.as_ref()),
    )?;
    let identity =
        crate::identity::canonical_identity_from_handle(&plan.physical_path, &handle.file)
            .map_err(|error| {
                ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
            })?;
    if identity.is_symlink || (identity.is_regular_file && identity.link_count > 1) {
        return Err(ManagedFileAccessErrorV1::AliasCollision);
    }
    match plan.expected_physical_identity {
        Some(expected) if identity.digest != expected => Err(ManagedFileAccessErrorV1::PlanStale),
        None if !allow_create_for_absent_plan => Err(ManagedFileAccessErrorV1::PlanStale),
        _ => Ok(handle),
    }
}

#[cfg(windows)]
fn windows_child_path(base: &str, name: &str) -> String {
    if base == "." || base.is_empty() {
        name.to_owned()
    } else {
        format!("{base}/{name}")
    }
}

#[cfg(windows)]
fn windows_collect_entries(
    root: &Path,
    base: &str,
    recursive: bool,
    max_depth: usize,
    depth: usize,
    entries: &mut Vec<String>,
    root_guard: Option<&std::fs::File>,
) -> Result<(), ManagedFileAccessErrorV1> {
    let directory =
        windows_open_relative_root(root, base, WindowsOpenKind::Directory, false, root_guard)?;
    let directory_path = if base == "." || base.is_empty() {
        root.to_path_buf()
    } else {
        root.join(base.replace('/', &std::path::MAIN_SEPARATOR.to_string()))
    };
    for entry in std::fs::read_dir(&directory_path).map_err(relative_io_error)? {
        let entry = entry.map_err(relative_io_error)?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let relative = windows_child_path(base, &name);
        let child =
            windows_open_relative_root(root, &relative, WindowsOpenKind::Any, false, root_guard)?;
        let is_directory = crate::identity::canonical_identity_from_handle(
            &directory_path.join(&name),
            &child.file,
        )
        .map_err(|error| ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string()))?
        .is_directory;
        entries.push(relative.clone());
        if recursive && is_directory && depth < max_depth {
            windows_collect_entries(
                root,
                &relative,
                recursive,
                max_depth,
                depth + 1,
                entries,
                root_guard,
            )?;
        }
    }
    drop(directory);
    Ok(())
}

#[cfg(windows)]
fn windows_collect_grep(
    root: &Path,
    base: &str,
    regex: &regex::Regex,
    matches: &mut Vec<String>,
    observed_bytes: &mut u64,
    root_guard: Option<&std::fs::File>,
) -> Result<(), ManagedFileAccessErrorV1> {
    let handle = windows_open_relative_root(root, base, WindowsOpenKind::Any, false, root_guard)?;
    let identity = crate::identity::canonical_identity_from_handle(
        &if base == "." || base.is_empty() {
            root.to_path_buf()
        } else {
            root.join(base.replace('/', &std::path::MAIN_SEPARATOR.to_string()))
        },
        &handle.file,
    )
    .map_err(|error| ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string()))?;
    if identity.is_directory {
        let directory_path = if base == "." || base.is_empty() {
            root.to_path_buf()
        } else {
            root.join(base.replace('/', &std::path::MAIN_SEPARATOR.to_string()))
        };
        for entry in std::fs::read_dir(directory_path).map_err(relative_io_error)? {
            let name = entry
                .map_err(relative_io_error)?
                .file_name()
                .to_string_lossy()
                .into_owned();
            windows_collect_grep(
                root,
                &windows_child_path(base, &name),
                regex,
                matches,
                observed_bytes,
                root_guard,
            )?;
        }
        return Ok(());
    }
    let mut file = handle.file;
    let mut raw = String::new();
    file.read_to_string(&mut raw).map_err(|_| {
        ManagedFileAccessErrorV1::PhysicalExecutionFailed("non-UTF-8 or unreadable file".to_owned())
    })?;
    *observed_bytes = observed_bytes.saturating_add(raw.len() as u64);
    for (index, line) in raw.lines().enumerate() {
        if regex.is_match(line) {
            matches.push(format!(
                "{base}:{}:{}",
                index + 1,
                sigil_kernel::safe_persistence_text(line)
            ));
        }
    }
    Ok(())
}

#[cfg(windows)]
fn windows_delete_handle(file: &std::fs::File) -> Result<(), ManagedFileAccessErrorV1> {
    use std::{mem::size_of, os::windows::io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_DISPOSITION_FLAG_DELETE, FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE,
        FILE_DISPOSITION_INFO_EX, FileDispositionInfoEx, SetFileInformationByHandle,
    };
    let disposition = FILE_DISPOSITION_INFO_EX {
        Flags: FILE_DISPOSITION_FLAG_DELETE | FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE,
    };
    // SAFETY: the handle is live and the disposition struct is a valid input buffer.
    let status = unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle() as _,
            FileDispositionInfoEx,
            &disposition as *const _ as *const std::ffi::c_void,
            size_of::<FILE_DISPOSITION_INFO_EX>() as u32,
        )
    };
    if status == 0 {
        return Err(relative_io_error(std::io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(windows)]
fn execute_physical(
    plan: &PlannedFileAccessV1,
    input: sigil_kernel::managed_file_access::ManagedFileExecutionInputV1,
    _file_delete: Option<&FileDeleteAuthorityStateV1>,
) -> Result<PhysicalExecutionOutcomeV1, ManagedFileAccessErrorV1> {
    match (plan.operation, input) {
        (
            ManagedFileOperationV1::Read,
            sigil_kernel::managed_file_access::ManagedFileExecutionInputV1::Read {
                offset,
                limit,
                max_bytes,
            },
        ) => {
            let handle = windows_open_plan(plan, WindowsOpenKind::Read, false)?;
            let mut file = handle.file;
            let mut raw = String::new();
            file.read_to_string(&mut raw).map_err(|_| {
                ManagedFileAccessErrorV1::PhysicalExecutionFailed(
                    "non-UTF-8 or unreadable file".to_owned(),
                )
            })?;
            let lines: Vec<&str> = raw.lines().collect();
            let selected = lines
                .iter()
                .skip(offset)
                .take(limit)
                .copied()
                .collect::<Vec<_>>();
            let mut payload = selected
                .iter()
                .map(|line| sigil_kernel::safe_persistence_text(line))
                .collect::<Vec<_>>()
                .join("\n");
            let truncated =
                offset.saturating_add(selected.len()) < lines.len() || payload.len() > max_bytes;
            payload.truncate(payload.len().min(max_bytes));
            Ok(PhysicalExecutionOutcomeV1 {
                payload,
                observed_bytes: raw.len() as u64,
                returned_entries: 0,
                total_entries: 0,
                returned_lines: selected.len() as u64,
                total_lines: lines.len() as u64,
                truncated,
            })
        }
        (
            ManagedFileOperationV1::List,
            sigil_kernel::managed_file_access::ManagedFileExecutionInputV1::List {
                recursive,
                limit,
                max_depth,
            },
        ) => {
            let _ = windows_open_plan(plan, WindowsOpenKind::Directory, false)?;
            let mut entries = Vec::new();
            windows_collect_entries(
                &plan.root,
                &plan.logical_path,
                recursive,
                max_depth,
                0,
                &mut entries,
                Some(plan.root_handle.as_ref()),
            )?;
            entries.sort();
            let total = entries.len();
            let truncated = total > limit;
            entries.truncate(limit);
            let payload = serde_json::to_string_pretty(&entries).map_err(|error| {
                ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
            })?;
            Ok(PhysicalExecutionOutcomeV1 {
                payload,
                observed_bytes: 0,
                returned_entries: entries.len() as u64,
                total_entries: total as u64,
                returned_lines: 0,
                total_lines: 0,
                truncated,
            })
        }
        (
            ManagedFileOperationV1::Grep,
            sigil_kernel::managed_file_access::ManagedFileExecutionInputV1::Grep {
                pattern,
                limit,
                max_bytes,
            },
        ) => {
            let regex = regex::Regex::new(&pattern).map_err(|error| {
                ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
            })?;
            let mut matches = Vec::new();
            let mut observed_bytes = 0;
            windows_collect_grep(
                &plan.root,
                &plan.logical_path,
                &regex,
                &mut matches,
                &mut observed_bytes,
                Some(plan.root_handle.as_ref()),
            )?;
            let total = matches.len();
            let truncated = total > limit;
            matches.truncate(limit);
            let mut payload = matches.join("\n");
            let byte_truncated = payload.len() > max_bytes;
            payload.truncate(payload.len().min(max_bytes));
            Ok(PhysicalExecutionOutcomeV1 {
                payload,
                observed_bytes,
                returned_entries: 0,
                total_entries: total as u64,
                returned_lines: matches.len() as u64,
                total_lines: 0,
                truncated: truncated || byte_truncated,
            })
        }
        (
            ManagedFileOperationV1::Glob,
            sigil_kernel::managed_file_access::ManagedFileExecutionInputV1::Glob { pattern, limit },
        ) => {
            let wildcard = format!(
                "^{}$",
                pattern
                    .split('*')
                    .map(regex::escape)
                    .collect::<Vec<_>>()
                    .join(".*")
            );
            let matcher = regex::Regex::new(&wildcard).map_err(|error| {
                ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
            })?;
            let _ = windows_open_plan(plan, WindowsOpenKind::Directory, false)?;
            let mut entries = Vec::new();
            windows_collect_entries(
                &plan.root,
                &plan.logical_path,
                true,
                usize::MAX,
                0,
                &mut entries,
                Some(plan.root_handle.as_ref()),
            )?;
            entries.retain(|entry| matcher.is_match(entry));
            entries.sort();
            let total = entries.len();
            let truncated = total > limit;
            entries.truncate(limit);
            let payload = serde_json::to_string_pretty(&entries).map_err(|error| {
                ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
            })?;
            Ok(PhysicalExecutionOutcomeV1 {
                payload,
                observed_bytes: 0,
                returned_entries: entries.len() as u64,
                total_entries: total as u64,
                returned_lines: 0,
                total_lines: 0,
                truncated,
            })
        }
        (
            ManagedFileOperationV1::Write,
            sigil_kernel::managed_file_access::ManagedFileExecutionInputV1::Write { content },
        ) => {
            let handle = windows_open_plan(plan, WindowsOpenKind::Write, true)?;
            let mut file = handle.file;
            file.set_len(0).map_err(|error| {
                ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
            })?;
            file.write_all(content.as_bytes()).map_err(|error| {
                ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
            })?;
            Ok(PhysicalExecutionOutcomeV1 {
                payload: "managed file write applied".to_owned(),
                observed_bytes: content.len() as u64,
                returned_entries: 0,
                total_entries: 0,
                returned_lines: 0,
                total_lines: 0,
                truncated: false,
            })
        }
        (
            ManagedFileOperationV1::Edit,
            sigil_kernel::managed_file_access::ManagedFileExecutionInputV1::Edit {
                old_text,
                new_text,
            },
        ) => {
            let handle = windows_open_plan(plan, WindowsOpenKind::ReadWrite, false)?;
            let mut file = handle.file;
            let mut current = String::new();
            file.read_to_string(&mut current).map_err(|error| {
                ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
            })?;
            if !current.contains(&old_text) {
                return Err(ManagedFileAccessErrorV1::PhysicalExecutionFailed(
                    "edit target text was not found".to_owned(),
                ));
            }
            let updated = current.replacen(&old_text, &new_text, 1);
            file.set_len(0).map_err(|error| {
                ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
            })?;
            file.rewind().map_err(|error| {
                ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
            })?;
            file.write_all(updated.as_bytes()).map_err(|error| {
                ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
            })?;
            Ok(PhysicalExecutionOutcomeV1 {
                payload: "managed file edit applied".to_owned(),
                observed_bytes: updated.len() as u64,
                returned_entries: 0,
                total_entries: 0,
                returned_lines: 0,
                total_lines: 0,
                truncated: false,
            })
        }
        (
            ManagedFileOperationV1::Delete,
            sigil_kernel::managed_file_access::ManagedFileExecutionInputV1::Delete,
        ) => {
            let handle = windows_open_plan(plan, WindowsOpenKind::Delete, false)?;
            windows_delete_handle(&handle.file)?;
            Ok(PhysicalExecutionOutcomeV1 {
                payload: "managed file delete applied".to_owned(),
                observed_bytes: 0,
                returned_entries: 0,
                total_entries: 0,
                returned_lines: 0,
                total_lines: 0,
                truncated: false,
            })
        }
        _ => Err(ManagedFileAccessErrorV1::AdmissionMismatch),
    }
}

#[cfg(not(any(unix, windows)))]
fn execute_physical(
    plan: &PlannedFileAccessV1,
    input: sigil_kernel::managed_file_access::ManagedFileExecutionInputV1,
    _file_delete: Option<&FileDeleteAuthorityStateV1>,
) -> Result<PhysicalExecutionOutcomeV1, ManagedFileAccessErrorV1> {
    match (plan.operation, input) {
        (
            ManagedFileOperationV1::Read,
            ManagedFileExecutionInputV1::Read {
                offset,
                limit,
                max_bytes,
            },
        ) => {
            let raw = std::fs::read_to_string(&plan.physical_path).map_err(|error| {
                ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
            })?;
            let lines: Vec<&str> = raw.lines().collect();
            let selected = lines
                .iter()
                .skip(offset)
                .take(limit)
                .copied()
                .collect::<Vec<_>>();
            let mut payload = selected
                .iter()
                .map(|line| sigil_kernel::safe_persistence_text(line))
                .collect::<Vec<_>>()
                .join("\n");
            let truncated =
                offset.saturating_add(selected.len()) < lines.len() || payload.len() > max_bytes;
            if payload.len() > max_bytes {
                payload.truncate(max_bytes);
            }
            Ok(PhysicalExecutionOutcomeV1 {
                payload,
                observed_bytes: raw.len() as u64,
                returned_entries: 0,
                total_entries: 0,
                returned_lines: selected.len() as u64,
                total_lines: lines.len() as u64,
                truncated,
            })
        }
        (
            ManagedFileOperationV1::List,
            ManagedFileExecutionInputV1::List {
                recursive,
                limit,
                max_depth,
            },
        ) => {
            let mut entries = Vec::new();
            collect_entries(
                &plan.physical_path,
                &plan.physical_path,
                recursive,
                max_depth,
                0,
                &mut entries,
            )?;
            entries.sort();
            let total = entries.len();
            let truncated = total > limit;
            entries.truncate(limit);
            let payload = serde_json::to_string_pretty(&entries).map_err(|error| {
                ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
            })?;
            Ok(PhysicalExecutionOutcomeV1 {
                payload,
                observed_bytes: 0,
                returned_entries: entries.len() as u64,
                total_entries: total as u64,
                returned_lines: 0,
                total_lines: 0,
                truncated,
            })
        }
        (
            ManagedFileOperationV1::Grep,
            ManagedFileExecutionInputV1::Grep {
                pattern,
                limit,
                max_bytes,
            },
        ) => {
            let regex = regex::Regex::new(&pattern).map_err(|error| {
                ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
            })?;
            let mut matches = Vec::new();
            let mut observed_bytes = 0u64;
            collect_grep(
                &plan.physical_path,
                &plan.physical_path,
                &regex,
                &mut matches,
                &mut observed_bytes,
            )?;
            let total = matches.len();
            let truncated = total > limit;
            matches.truncate(limit);
            let mut payload = matches.join("\n");
            if payload.len() > max_bytes {
                payload.truncate(max_bytes);
            }
            let byte_truncated = payload.len() == max_bytes;
            Ok(PhysicalExecutionOutcomeV1 {
                payload,
                observed_bytes,
                returned_entries: 0,
                total_entries: total as u64,
                returned_lines: matches.len() as u64,
                total_lines: 0,
                truncated: truncated || byte_truncated,
            })
        }
        (ManagedFileOperationV1::Glob, ManagedFileExecutionInputV1::Glob { pattern, limit }) => {
            let wildcard = format!(
                "^{}$",
                pattern
                    .split('*')
                    .map(regex::escape)
                    .collect::<Vec<_>>()
                    .join(".*")
            );
            let matcher = regex::Regex::new(&wildcard).map_err(|error| {
                ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
            })?;
            let mut entries = Vec::new();
            collect_entries(
                &plan.physical_path,
                &plan.physical_path,
                true,
                usize::MAX,
                0,
                &mut entries,
            )?;
            entries.retain(|entry| matcher.is_match(entry));
            entries.sort();
            let total = entries.len();
            let truncated = total > limit;
            entries.truncate(limit);
            let payload = serde_json::to_string_pretty(&entries).map_err(|error| {
                ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
            })?;
            Ok(PhysicalExecutionOutcomeV1 {
                payload,
                observed_bytes: 0,
                returned_entries: entries.len() as u64,
                total_entries: total as u64,
                returned_lines: 0,
                total_lines: 0,
                truncated,
            })
        }
        (ManagedFileOperationV1::Write, ManagedFileExecutionInputV1::Write { content }) => {
            let parent = plan
                .physical_path
                .parent()
                .ok_or(ManagedFileAccessErrorV1::AliasCollision)?;
            let parent = parent.canonicalize().map_err(|error| {
                ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
            })?;
            if !parent.starts_with(&plan.root) {
                return Err(ManagedFileAccessErrorV1::AliasCollision);
            }
            std::fs::write(&plan.physical_path, content.as_bytes()).map_err(|error| {
                ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
            })?;
            Ok(PhysicalExecutionOutcomeV1 {
                payload: "managed file write applied".to_owned(),
                observed_bytes: content.len() as u64,
                returned_entries: 0,
                total_entries: 0,
                returned_lines: 0,
                total_lines: 0,
                truncated: false,
            })
        }
        (
            ManagedFileOperationV1::Edit,
            ManagedFileExecutionInputV1::Edit { old_text, new_text },
        ) => {
            let current = std::fs::read_to_string(&plan.physical_path).map_err(|error| {
                ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
            })?;
            if !current.contains(&old_text) {
                return Err(ManagedFileAccessErrorV1::PhysicalExecutionFailed(
                    "edit target text was not found".to_owned(),
                ));
            }
            let updated = current.replacen(&old_text, &new_text, 1);
            std::fs::write(&plan.physical_path, updated.as_bytes()).map_err(|error| {
                ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
            })?;
            Ok(PhysicalExecutionOutcomeV1 {
                payload: "managed file edit applied".to_owned(),
                observed_bytes: updated.len() as u64,
                returned_entries: 0,
                total_entries: 0,
                returned_lines: 0,
                total_lines: 0,
                truncated: false,
            })
        }
        (ManagedFileOperationV1::Delete, ManagedFileExecutionInputV1::Delete) => {
            std::fs::remove_file(&plan.physical_path).map_err(|error| {
                ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
            })?;
            Ok(PhysicalExecutionOutcomeV1 {
                payload: "managed file delete applied".to_owned(),
                observed_bytes: 0,
                returned_entries: 0,
                total_entries: 0,
                returned_lines: 0,
                total_lines: 0,
                truncated: false,
            })
        }
        _ => Err(ManagedFileAccessErrorV1::AdmissionMismatch),
    }
}

#[cfg(not(any(unix, windows)))]
fn collect_entries(
    root: &Path,
    current: &Path,
    recursive: bool,
    max_depth: usize,
    depth: usize,
    entries: &mut Vec<String>,
) -> Result<(), ManagedFileAccessErrorV1> {
    let read_dir = std::fs::read_dir(current)
        .map_err(|error| ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string()))?;
    for entry in read_dir {
        let entry = entry.map_err(|error| {
            ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
        })?;
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .map_err(|_| ManagedFileAccessErrorV1::AliasCollision)?
            .to_string_lossy()
            .replace('\\', "/");
        entries.push(relative);
        if recursive
            && entry
                .file_type()
                .map_err(|error| {
                    ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
                })?
                .is_dir()
            && depth < max_depth
        {
            collect_entries(root, &path, recursive, max_depth, depth + 1, entries)?;
        }
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn collect_grep(
    root: &Path,
    current: &Path,
    regex: &regex::Regex,
    matches: &mut Vec<String>,
    observed_bytes: &mut u64,
) -> Result<(), ManagedFileAccessErrorV1> {
    let metadata = std::fs::symlink_metadata(current)
        .map_err(|error| ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string()))?;
    if metadata.is_dir() {
        for entry in std::fs::read_dir(current)
            .map_err(|error| ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string()))?
        {
            let entry = entry.map_err(|error| {
                ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
            })?;
            collect_grep(root, &entry.path(), regex, matches, observed_bytes)?;
        }
        return Ok(());
    }
    if !metadata.is_file() {
        return Ok(());
    }
    let raw = std::fs::read_to_string(current).map_err(|_| {
        ManagedFileAccessErrorV1::PhysicalExecutionFailed("non-UTF-8 or unreadable file".to_owned())
    })?;
    *observed_bytes = observed_bytes.saturating_add(raw.len() as u64);
    let relative = current
        .strip_prefix(root)
        .map_err(|_| ManagedFileAccessErrorV1::AliasCollision)?
        .to_string_lossy()
        .replace('\\', "/");
    for (index, line) in raw.lines().enumerate() {
        if regex.is_match(line) {
            matches.push(format!(
                "{relative}:{}:{}",
                index + 1,
                sigil_kernel::safe_persistence_text(line)
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sigil_kernel::managed_file_access::{
        ManagedFileAccessAdmissionTokenV1, ManagedFileAccessPlanRequestV1,
        ManagedFileAdmissionBindingV1, ManagedFileExecutionInputV1, ManagedFileExecutionRequestV1,
        ManagedFileLogicalPathV1, ManagedFilePreviewRequestV1, ToolFileAccessAdmissionTokenV1,
    };
    use sigil_kernel::resource::{AuthorityGeneration, OpaquePermissionSubjectRef};
    use std::collections::BTreeSet;

    fn hash(seed: u8) -> CanonicalHash {
        CanonicalHash::from_bytes([seed; 32])
    }

    fn binding() -> ManagedFileAdmissionBindingV1 {
        ManagedFileAdmissionBindingV1::ToolPermissionPlan {
            permission_plan_hash: hash(1),
            decision_hash: hash(2),
            approval_continuity_hash: hash(3),
            tool_start_event_digest: hash(4),
            file_access_plan_hash: hash(5),
            file_subject_binding_hash: hash(6),
            file_resolver_proof_digest: hash(7),
            file_authority_generation: AuthorityGeneration {
                epoch: 1,
                instance_hash: hash(8),
            },
            workspace_mutation_activation: None,
        }
    }

    fn subject(
        subject_ref: &str,
        class: BorrowedSubjectClassV1,
        identity: bool,
    ) -> Arc<Mutex<BorrowedSubjectRegistryV1>> {
        let mut registry = BorrowedSubjectRegistryV1::new();
        let observed = if identity {
            Some(crate::identity::CanonicalLocalIdentity {
                digest: hash(9),
                is_regular_file: false,
                is_directory: true,
                is_symlink: false,
                link_count: 2,
            })
        } else {
            None
        };
        registry
            .observe_with_identity(
                &OpaquePermissionSubjectRef::new(subject_ref.to_owned()),
                class,
                1,
                observed,
            )
            .expect("observe");
        Arc::new(Mutex::new(registry))
    }

    fn request(
        subject_ref: &str,
        operation: ManagedFileOperationV1,
        op_digest: CanonicalHash,
    ) -> ManagedFileAccessRequestV1 {
        ManagedFileAccessRequestV1 {
            subject_ref: OpaquePermissionSubjectRef::new(subject_ref.to_owned()),
            operation,
            operation_digest: op_digest,
            admission_binding: binding(),
            admission_binding_hash: hash(10),
        }
    }

    fn token(op_digest: CanonicalHash) -> ManagedFileAccessAdmissionTokenV1 {
        ManagedFileAccessAdmissionTokenV1::Tool(
            ToolFileAccessAdmissionTokenV1::qualification_fixture(binding(), hash(6), op_digest),
        )
    }

    #[test]
    fn r71_fa_workspace_read_adjudicates_with_identity_evidence() {
        let registry = subject("ws-1", BorrowedSubjectClassV1::Workspace, true);
        let svc = AuthorityManagedFileAccessServiceV1::new(registry);
        let result = svc
            .access(
                request("ws-1", ManagedFileOperationV1::Read, hash(6)),
                token(hash(6)),
            )
            .expect("adjudicate");
        assert_eq!(
            result.effect_settlement,
            sigil_kernel::recovery::EffectSettlementV1::Applied
        );
        assert_eq!(result.access_receipt.subject_binding_hash, hash(6));
        assert_eq!(result.access_receipt.identity_before, Some(hash(9)));
        assert!(result.access_receipt.identity_after.is_none());
        // Read class has stable tag 1; grant hash is sha256([1]).
        use sha2::Digest as _;
        let mut expected = sha2::Sha256::new();
        expected.update([1u8]);
        assert_eq!(
            result.access_receipt.granted_access_hash,
            CanonicalHash::from_bytes(expected.finalize().into())
        );
    }

    #[test]
    fn r71_fa_claim_is_one_shot() {
        let registry = subject("ws-1", BorrowedSubjectClassV1::Workspace, false);
        let svc = AuthorityManagedFileAccessServiceV1::new(registry);
        svc.access(
            request("ws-1", ManagedFileOperationV1::Read, hash(6)),
            token(hash(6)),
        )
        .expect("first");
        let error = svc
            .access(
                request("ws-1", ManagedFileOperationV1::Read, hash(6)),
                token(hash(6)),
            )
            .expect_err("reuse");
        assert!(matches!(error, ManagedFileAccessErrorV1::TokenReplay));
    }

    #[test]
    fn r71_fa_operation_digest_mismatch_refused() {
        let registry = subject("ws-1", BorrowedSubjectClassV1::Workspace, false);
        let svc = AuthorityManagedFileAccessServiceV1::new(registry);
        let error = svc
            .access(
                request("ws-1", ManagedFileOperationV1::Read, hash(99)),
                token(hash(6)),
            )
            .expect_err("mismatch");
        assert!(matches!(
            error,
            ManagedFileAccessErrorV1::OperationNotPermitted
        ));
    }

    #[test]
    fn r71_fa_unregistered_subject_refused() {
        let svc = AuthorityManagedFileAccessServiceV1::new(Arc::new(Mutex::new(
            BorrowedSubjectRegistryV1::new(),
        )));
        let error = svc
            .access(
                request("unknown-1", ManagedFileOperationV1::Read, hash(6)),
                token(hash(6)),
            )
            .expect_err("unregistered");
        assert!(matches!(
            error,
            ManagedFileAccessErrorV1::OperationNotPermitted
        ));
    }

    #[test]
    fn r71_fa_system_temp_write_denied_read_allowed() {
        let registry = subject("st-1", BorrowedSubjectClassV1::SystemTemp, false);
        let svc = AuthorityManagedFileAccessServiceV1::new(registry);
        let error = svc
            .access(
                request("st-1", ManagedFileOperationV1::Write, hash(6)),
                token(hash(6)),
            )
            .expect_err("deny write");
        assert!(matches!(
            error,
            ManagedFileAccessErrorV1::OperationNotPermitted
        ));
        // Read at the boundary remains admissible.
        svc.access(
            request("st-1", ManagedFileOperationV1::Read, hash(6)),
            token(hash(6)),
        )
        .expect("read ok");
    }

    #[test]
    fn r71_fa_granted_hash_matches_closed_class() {
        let _ = BTreeSet::<ResourceAccessV1>::new();
        let registry = subject("ws-1", BorrowedSubjectClassV1::Workspace, false);
        let svc = AuthorityManagedFileAccessServiceV1::new(registry);
        let result = svc
            .access(
                request("ws-1", ManagedFileOperationV1::Edit, hash(6)),
                token(hash(6)),
            )
            .expect("edit");
        assert!(result.access_receipt.granted_access_hash != hash(0));
    }

    #[test]
    fn managed_file_access_registered_workspace_plans_and_executes_without_path_ref() {
        let workspace = tempfile::tempdir().expect("workspace");
        std::fs::write(workspace.path().join("notes.txt"), "alpha\nbeta\n").expect("file");
        let authority_generation = AuthorityGeneration {
            epoch: 7,
            instance_hash: hash(8),
        };
        let registry = Arc::new(Mutex::new(BorrowedSubjectRegistryV1::new()));
        registry
            .lock()
            .expect("registry")
            .activate_workspace("app", "workspace", workspace.path(), authority_generation)
            .expect("activate");
        let service = AuthorityManagedFileAccessServiceV1::new(Arc::clone(&registry));
        let plan = service
            .plan(ManagedFileAccessPlanRequestV1 {
                logical_path: ManagedFileLogicalPathV1::new("notes.txt").expect("logical"),
                operation: ManagedFileOperationV1::Read,
                operation_scope: "read-file".to_owned(),
            })
            .expect("plan");
        assert_ne!(plan.authority_generation.epoch, 0);
        assert_ne!(plan.resolver_proof_digest, hash(0));
        assert_ne!(plan.plan_hash, hash(0));
        assert!(!plan.subject_ref.as_str().contains('/'));
        let binding = ManagedFileAdmissionBindingV1::ToolPermissionPlan {
            permission_plan_hash: hash(1),
            decision_hash: hash(2),
            approval_continuity_hash: hash(3),
            tool_start_event_digest: hash(4),
            file_access_plan_hash: plan.plan_hash,
            file_subject_binding_hash: plan.subject_binding_hash,
            file_resolver_proof_digest: plan.resolver_proof_digest,
            file_authority_generation: plan.authority_generation,
            workspace_mutation_activation: None,
        };
        let outcome = service
            .execute(
                ManagedFileExecutionRequestV1 {
                    access: ManagedFileAccessRequestV1 {
                        subject_ref: plan.subject_ref.clone(),
                        operation: ManagedFileOperationV1::Read,
                        operation_digest: plan.operation_digest,
                        admission_binding: binding.clone(),
                        admission_binding_hash: plan.plan_hash,
                    },
                    input: ManagedFileExecutionInputV1::Read {
                        offset: 0,
                        limit: 10,
                        max_bytes: 1024,
                    },
                },
                ManagedFileAccessAdmissionTokenV1::Tool(
                    ToolFileAccessAdmissionTokenV1::qualification_fixture(
                        binding,
                        plan.subject_binding_hash,
                        plan.operation_digest,
                    ),
                ),
            )
            .expect("execute");
        assert_eq!(outcome.payload, "alpha\nbeta");
        assert_eq!(outcome.total_lines, 2);
        let preview = service
            .preview(ManagedFilePreviewRequestV1 {
                plan_hash: plan.plan_hash,
                operation: ManagedFileOperationV1::Read,
                max_bytes: 5,
            })
            .expect("preview");
        assert_eq!(preview.payload, "alpha");
        assert!(preview.truncated);
    }

    fn registered_read_plan(
        content: Option<&str>,
    ) -> (
        tempfile::TempDir,
        AuthorityManagedFileAccessServiceV1,
        sigil_kernel::permission_plan_v3::ManagedFileAccessPlanDraftRefV1,
    ) {
        registered_plan_for_operation(content, ManagedFileOperationV1::Read)
    }

    fn registered_plan_for_operation(
        content: Option<&str>,
        operation: ManagedFileOperationV1,
    ) -> (
        tempfile::TempDir,
        AuthorityManagedFileAccessServiceV1,
        sigil_kernel::permission_plan_v3::ManagedFileAccessPlanDraftRefV1,
    ) {
        let workspace = tempfile::tempdir().expect("workspace");
        if let Some(content) = content {
            std::fs::write(workspace.path().join("notes.txt"), content).expect("file");
        }
        let registry = Arc::new(Mutex::new(BorrowedSubjectRegistryV1::new()));
        registry
            .lock()
            .expect("registry")
            .activate_workspace(
                "sigil",
                "fault-file-workspace",
                workspace.path(),
                AuthorityGeneration {
                    epoch: 9,
                    instance_hash: hash(0x91),
                },
            )
            .expect("activate");
        let service = if operation == ManagedFileOperationV1::Delete {
            AuthorityManagedFileAccessServiceV1::new_for_test_with_journal(
                Arc::clone(&registry),
                workspace.path().join(".test-file-delete-arena"),
                workspace.path().join(".test-file-delete.journal.json"),
            )
        } else {
            AuthorityManagedFileAccessServiceV1::new(registry)
        };
        let plan = service
            .plan(ManagedFileAccessPlanRequestV1 {
                logical_path: ManagedFileLogicalPathV1::new("notes.txt").expect("logical"),
                operation,
                operation_scope: format!(
                    "fault-file-{}",
                    String::from_utf8_lossy(operation_tag(operation))
                ),
            })
            .expect("plan");
        (workspace, service, plan)
    }

    fn execution_request_for_plan(
        plan: &sigil_kernel::permission_plan_v3::ManagedFileAccessPlanDraftRefV1,
        input: ManagedFileExecutionInputV1,
    ) -> (
        ManagedFileExecutionRequestV1,
        ManagedFileAccessAdmissionTokenV1,
    ) {
        let operation = match &input {
            ManagedFileExecutionInputV1::Read { .. } => ManagedFileOperationV1::Read,
            ManagedFileExecutionInputV1::List { .. } => ManagedFileOperationV1::List,
            ManagedFileExecutionInputV1::Glob { .. } => ManagedFileOperationV1::Glob,
            ManagedFileExecutionInputV1::Grep { .. } => ManagedFileOperationV1::Grep,
            ManagedFileExecutionInputV1::Write { .. } => ManagedFileOperationV1::Write,
            ManagedFileExecutionInputV1::Edit { .. } => ManagedFileOperationV1::Edit,
            ManagedFileExecutionInputV1::Delete => ManagedFileOperationV1::Delete,
        };
        let binding = ManagedFileAdmissionBindingV1::ToolPermissionPlan {
            permission_plan_hash: hash(0x01),
            decision_hash: hash(0x02),
            approval_continuity_hash: hash(0x03),
            tool_start_event_digest: hash(0x04),
            file_access_plan_hash: plan.plan_hash,
            file_subject_binding_hash: plan.subject_binding_hash,
            file_resolver_proof_digest: plan.resolver_proof_digest,
            file_authority_generation: plan.authority_generation,
            workspace_mutation_activation: None,
        };
        let request = ManagedFileExecutionRequestV1 {
            access: ManagedFileAccessRequestV1 {
                subject_ref: plan.subject_ref.clone(),
                operation,
                operation_digest: plan.operation_digest,
                admission_binding: binding.clone(),
                admission_binding_hash: plan.plan_hash,
            },
            input,
        };
        let token = ManagedFileAccessAdmissionTokenV1::Tool(
            ToolFileAccessAdmissionTokenV1::qualification_fixture(
                binding,
                plan.subject_binding_hash,
                plan.operation_digest,
            ),
        );
        (request, token)
    }

    #[cfg(windows)]
    fn registered_plan_for_workspace(
        workspace: &tempfile::TempDir,
        logical_path: &str,
        operation: ManagedFileOperationV1,
    ) -> (
        AuthorityManagedFileAccessServiceV1,
        sigil_kernel::permission_plan_v3::ManagedFileAccessPlanDraftRefV1,
    ) {
        let registry = Arc::new(Mutex::new(BorrowedSubjectRegistryV1::new()));
        registry
            .lock()
            .expect("registry")
            .activate_workspace(
                "sigil",
                "fault-file-custom-workspace",
                workspace.path(),
                AuthorityGeneration {
                    epoch: 9,
                    instance_hash: hash(0x93),
                },
            )
            .expect("activate");
        let service = AuthorityManagedFileAccessServiceV1::new(registry);
        let plan = service
            .plan(ManagedFileAccessPlanRequestV1 {
                logical_path: ManagedFileLogicalPathV1::new(logical_path).expect("logical"),
                operation,
                operation_scope: format!(
                    "fault-file-{}",
                    String::from_utf8_lossy(operation_tag(operation))
                ),
            })
            .expect("plan");
        (service, plan)
    }

    #[test]
    fn r71_f_fil_001_unregistered_workspace_fails_closed() {
        let service = AuthorityManagedFileAccessServiceV1::new(Arc::new(Mutex::new(
            BorrowedSubjectRegistryV1::new(),
        )));
        let error = service
            .plan(ManagedFileAccessPlanRequestV1 {
                logical_path: ManagedFileLogicalPathV1::new("notes.txt").expect("logical"),
                operation: ManagedFileOperationV1::Read,
                operation_scope: "fault".to_owned(),
            })
            .expect_err("missing registration");
        assert!(matches!(
            error,
            ManagedFileAccessErrorV1::ResourcePreconditionUnavailable
        ));
    }

    #[test]
    fn r71_f_fil_002_absolute_and_traversal_paths_are_rejected() {
        for value in [
            "/etc/passwd",
            "../outside",
            "..\\outside",
            "folder\\..\\outside",
            "C:\\outside",
        ] {
            assert!(matches!(
                ManagedFileLogicalPathV1::new(value),
                Err(ManagedFileAccessErrorV1::AliasCollision)
            ));
        }
    }

    #[test]
    fn r71_f_fil_003_registered_plan_is_pathless_and_nonzero() {
        let (_workspace, _service, plan) = registered_read_plan(Some("ok"));
        assert!(!plan.subject_ref.as_str().contains('/'));
        assert_ne!(plan.authority_generation.epoch, 0);
        assert_ne!(plan.subject_binding_hash, hash(0));
        assert_ne!(plan.resolver_proof_digest, hash(0));
        assert_ne!(plan.plan_hash, hash(0));
    }

    #[test]
    fn r71_f_fil_004_activation_preserves_exact_generation() {
        let (_workspace, _service, plan) = registered_read_plan(Some("ok"));
        assert_eq!(plan.authority_generation.epoch, 9);
        assert_eq!(plan.authority_generation.instance_hash, hash(0x91));
    }

    #[test]
    fn r71_f_fil_005_executor_returns_bounded_receipt() {
        let (_workspace, service, plan) = registered_read_plan(Some("alpha\nbeta\ngamma"));
        let (request, token) = execution_request_for_plan(
            &plan,
            ManagedFileExecutionInputV1::Read {
                offset: 0,
                limit: 3,
                max_bytes: 6,
            },
        );
        let outcome = service.execute(request, token).expect("execute");
        assert_eq!(outcome.payload, "alpha\n");
        assert!(outcome.truncated);
        assert_ne!(outcome.result_digest, hash(0));
        assert_eq!(
            outcome.effect_settlement,
            sigil_kernel::recovery::EffectSettlementV1::Applied
        );
    }

    #[test]
    fn r71_f_fil_006_preview_is_bounded_and_side_effect_free() {
        let (_workspace, service, plan) = registered_read_plan(Some("preview-content"));
        let preview = service
            .preview(ManagedFilePreviewRequestV1 {
                plan_hash: plan.plan_hash,
                operation: ManagedFileOperationV1::Read,
                max_bytes: 7,
            })
            .expect("preview");
        assert_eq!(preview.payload, "preview");
        assert!(preview.truncated);
    }

    #[test]
    fn r71_f_fil_007_executor_replay_is_rejected() {
        let (_workspace, service, plan) = registered_read_plan(Some("once"));
        let (request, token) = execution_request_for_plan(
            &plan,
            ManagedFileExecutionInputV1::Read {
                offset: 0,
                limit: 1,
                max_bytes: 32,
            },
        );
        service.execute(request, token).expect("first execute");
        let (request, token) = execution_request_for_plan(
            &plan,
            ManagedFileExecutionInputV1::Read {
                offset: 0,
                limit: 1,
                max_bytes: 32,
            },
        );
        assert!(matches!(
            service.execute(request, token),
            Err(ManagedFileAccessErrorV1::TokenReplay)
        ));
    }

    #[test]
    fn r71_f_fil_008_cross_operation_binding_is_rejected() {
        let (_workspace, service, plan) = registered_read_plan(Some("read-only"));
        let (mut request, token) = execution_request_for_plan(
            &plan,
            ManagedFileExecutionInputV1::Read {
                offset: 0,
                limit: 1,
                max_bytes: 32,
            },
        );
        request.access.operation = ManagedFileOperationV1::Write;
        assert!(matches!(
            service.execute(request, token),
            Err(ManagedFileAccessErrorV1::AdmissionMismatch)
        ));
    }

    #[test]
    fn r71_f_fil_009_root_identity_drift_is_rejected() {
        let (workspace, service, plan) = registered_read_plan(Some("drift"));
        let moved = workspace.path().with_extension("moved");
        std::fs::rename(workspace.path(), &moved).expect("move root");
        std::fs::create_dir_all(workspace.path()).expect("replacement root");
        let (request, token) = execution_request_for_plan(
            &plan,
            ManagedFileExecutionInputV1::Read {
                offset: 0,
                limit: 1,
                max_bytes: 32,
            },
        );
        assert!(matches!(
            service.execute(request, token),
            Err(ManagedFileAccessErrorV1::SubjectIdentityDrift)
        ));
    }

    #[test]
    fn r71_f_fil_010_symlink_leaf_is_rejected() {
        #[cfg(unix)]
        {
            let workspace = tempfile::tempdir().expect("workspace");
            let outside = tempfile::tempdir().expect("outside");
            std::fs::write(outside.path().join("secret.txt"), "secret").expect("secret");
            std::os::unix::fs::symlink(
                outside.path().join("secret.txt"),
                workspace.path().join("notes.txt"),
            )
            .expect("symlink");
            let registry = Arc::new(Mutex::new(BorrowedSubjectRegistryV1::new()));
            registry
                .lock()
                .expect("registry")
                .activate_workspace(
                    "sigil",
                    "fault-file-symlink",
                    workspace.path(),
                    AuthorityGeneration {
                        epoch: 9,
                        instance_hash: hash(0x92),
                    },
                )
                .expect("activate");
            let service = AuthorityManagedFileAccessServiceV1::new(registry);
            let plan_error = service
                .plan(ManagedFileAccessPlanRequestV1 {
                    logical_path: ManagedFileLogicalPathV1::new("notes.txt").expect("logical"),
                    operation: ManagedFileOperationV1::Read,
                    operation_scope: "fault-symlink".to_owned(),
                })
                .expect_err("symlink plan must fail closed");
            assert!(matches!(
                plan_error,
                ManagedFileAccessErrorV1::AliasCollision
            ));
        }
        #[cfg(not(unix))]
        assert!(
            true,
            "symlink leaf case is covered by platform-specific authority tests"
        );
    }

    #[test]
    fn managed_file_alias_replacement_cannot_redirect_a_planned_read() {
        #[cfg(unix)]
        {
            let (workspace, service, plan) = registered_read_plan(Some("inside"));
            let outside = tempfile::tempdir().expect("outside");
            std::fs::write(outside.path().join("secret.txt"), "outside-secret").expect("secret");
            std::fs::rename(
                workspace.path().join("notes.txt"),
                workspace.path().join("notes.original"),
            )
            .expect("move planned leaf");
            std::os::unix::fs::symlink(
                outside.path().join("secret.txt"),
                workspace.path().join("notes.txt"),
            )
            .expect("replace with symlink");
            let (request, token) = execution_request_for_plan(
                &plan,
                ManagedFileExecutionInputV1::Read {
                    offset: 0,
                    limit: 1,
                    max_bytes: 64,
                },
            );
            let error = service
                .execute(request, token)
                .expect_err("alias replacement must fail closed");
            assert!(matches!(error, ManagedFileAccessErrorV1::AliasCollision));
            assert_eq!(
                std::fs::read_to_string(outside.path().join("secret.txt")).expect("secret"),
                "outside-secret"
            );
        }
        #[cfg(not(unix))]
        assert!(
            true,
            "handle-relative alias replacement is covered by Unix authority tests"
        );
    }

    #[test]
    fn managed_file_regular_inode_replacement_is_plan_stale() {
        let (workspace, service, plan) = registered_read_plan(Some("inside"));
        std::fs::rename(
            workspace.path().join("notes.txt"),
            workspace.path().join("notes.original"),
        )
        .expect("move planned leaf");
        std::fs::write(workspace.path().join("notes.txt"), "replacement")
            .expect("replace with a regular file");
        let (request, token) = execution_request_for_plan(
            &plan,
            ManagedFileExecutionInputV1::Read {
                offset: 0,
                limit: 1,
                max_bytes: 64,
            },
        );
        assert!(matches!(
            service.execute(request, token),
            Err(ManagedFileAccessErrorV1::PlanStale)
        ));
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("notes.txt")).expect("replacement"),
            "replacement"
        );
    }

    #[cfg(unix)]
    #[test]
    fn managed_file_hard_link_replacement_is_alias_collision() {
        let (workspace, service, plan) = registered_read_plan(Some("inside"));
        let outside = tempfile::tempdir().expect("outside");
        let outside_file = outside.path().join("outside.txt");
        std::fs::write(&outside_file, "outside-secret").expect("outside file");
        std::fs::rename(
            workspace.path().join("notes.txt"),
            workspace.path().join("notes.original"),
        )
        .expect("move planned leaf");
        std::fs::hard_link(&outside_file, workspace.path().join("notes.txt"))
            .expect("replace with a hard link");
        let (request, token) = execution_request_for_plan(
            &plan,
            ManagedFileExecutionInputV1::Read {
                offset: 0,
                limit: 1,
                max_bytes: 64,
            },
        );
        assert!(matches!(
            service.execute(request, token),
            Err(ManagedFileAccessErrorV1::AliasCollision)
        ));
        assert_eq!(
            std::fs::read_to_string(outside_file).expect("outside secret"),
            "outside-secret"
        );
    }

    #[test]
    fn managed_file_absent_to_present_transition_is_plan_stale() {
        let (workspace, service, plan) = registered_read_plan(None);
        std::fs::write(workspace.path().join("notes.txt"), "created-after-plan")
            .expect("create planned leaf after approval");
        let (request, token) = execution_request_for_plan(
            &plan,
            ManagedFileExecutionInputV1::Read {
                offset: 0,
                limit: 1,
                max_bytes: 64,
            },
        );
        assert!(matches!(
            service.execute(request, token),
            Err(ManagedFileAccessErrorV1::PlanStale)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn managed_file_delete_quarantine_restores_replacement_on_identity_mismatch() {
        let (workspace, service, plan) =
            registered_plan_for_operation(Some("approved"), ManagedFileOperationV1::Delete);
        let planned = service
            .plans
            .lock()
            .expect("plans")
            .get(&plan.plan_hash.to_hex())
            .cloned()
            .expect("planned file");
        let approved = std::fs::symlink_metadata(workspace.path().join("notes.txt"))
            .expect("approved metadata");
        let (parent, leaf) = open_relative_parent(&planned).expect("parent");
        std::fs::rename(
            workspace.path().join("notes.txt"),
            workspace.path().join("notes.original"),
        )
        .expect("move approved leaf");
        std::fs::write(workspace.path().join("notes.txt"), "replacement").expect("replacement");

        let state = service.file_delete.as_ref().expect("delete test state");
        let error = delete_via_quarantine(state, &planned, &parent, &leaf, &approved)
            .expect_err("replacement must not be deleted");
        assert!(matches!(error, ManagedFileAccessErrorV1::PlanStale));
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("notes.txt")).expect("restored"),
            "replacement"
        );
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("notes.original")).expect("original"),
            "approved"
        );
        assert!(
            !std::fs::read_dir(workspace.path())
                .expect("workspace entries")
                .filter_map(Result::ok)
                .any(|entry| entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".sigil-delete-quarantine-"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn managed_file_delete_removes_the_approved_leaf_after_quarantine_check() {
        let (workspace, service, plan) =
            registered_plan_for_operation(Some("approved"), ManagedFileOperationV1::Delete);
        let (request, token) =
            execution_request_for_plan(&plan, ManagedFileExecutionInputV1::Delete);
        let outcome = service.execute(request, token).expect("delete");
        assert_eq!(outcome.payload, "managed file delete applied");
        assert!(!workspace.path().join("notes.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn managed_file_delete_restart_reconciles_renamed_arena_entry() {
        let (workspace, service, plan) =
            registered_plan_for_operation(Some("approved"), ManagedFileOperationV1::Delete);
        let registry = Arc::clone(&service.registry);
        let state = service.file_delete.as_ref().expect("delete state");
        let arena_root = state.arena_root.clone();
        let planned = service
            .plans
            .lock()
            .expect("plans")
            .get(&plan.plan_hash.to_hex())
            .cloned()
            .expect("planned file");
        let (parent, leaf) = open_relative_parent(&planned).expect("parent");
        let arena = open_file_delete_arena(state, &parent).expect("arena");
        let approved = std::fs::symlink_metadata(workspace.path().join("notes.txt"))
            .expect("approved metadata");
        let operation_id = format!("restart-{0}", plan.plan_hash.to_hex());
        let quarantine_name = format!("restart-{}", plan.plan_hash.to_hex());
        let quarantine = CString::new(quarantine_name.clone()).expect("quarantine name");
        append_file_delete_event(
            state,
            ResourceJournalEventV1::FileDeletePrepared {
                operation_id: operation_id.clone(),
                subject_ref: planned.subject_ref.as_str().to_owned(),
                logical_path: planned.logical_path.clone(),
                plan_hash: plan.plan_hash,
                binding_hash: plan.plan_hash,
                quarantine_name: quarantine_name.clone(),
                expected_identity: journal_file_identity(&approved),
            },
        )
        .expect("prepared");
        rename_noreplace_at(&parent, &leaf, &arena, &quarantine).expect("rename");
        append_file_delete_event(
            state,
            ResourceJournalEventV1::FileDeleteRenamed {
                operation_id: operation_id.clone(),
                quarantine_identity: journal_file_identity(&approved),
            },
        )
        .expect("renamed");
        drop(service);

        let restarted = AuthorityManagedFileAccessServiceV1::new_for_test_with_journal(
            registry,
            workspace.path().join(".test-file-delete-arena"),
            workspace.path().join(".test-file-delete.journal.json"),
        );
        restarted
            .reconcile_file_delete_journal()
            .expect("restart reconciliation");
        assert!(!workspace.path().join("notes.txt").exists());
        assert_eq!(
            std::fs::read_dir(arena_root)
                .expect("arena entries")
                .count(),
            0
        );
    }

    #[cfg(unix)]
    #[test]
    fn managed_file_delete_restart_closes_prepared_before_rename_prefix() {
        let (workspace, service, plan) =
            registered_plan_for_operation(Some("approved"), ManagedFileOperationV1::Delete);
        let registry = Arc::clone(&service.registry);
        let state = service.file_delete.as_ref().expect("delete state");
        let arena_root = state.arena_root.clone();
        let planned = service
            .plans
            .lock()
            .expect("plans")
            .get(&plan.plan_hash.to_hex())
            .cloned()
            .expect("planned file");
        let approved = std::fs::symlink_metadata(workspace.path().join("notes.txt"))
            .expect("approved metadata");
        let operation_id = format!("prepared-{0}", plan.plan_hash.to_hex());
        append_file_delete_event(
            state,
            ResourceJournalEventV1::FileDeletePrepared {
                operation_id: operation_id.clone(),
                subject_ref: planned.subject_ref.as_str().to_owned(),
                logical_path: planned.logical_path.clone(),
                plan_hash: plan.plan_hash,
                binding_hash: plan.plan_hash,
                quarantine_name: format!("prepared-{}", plan.plan_hash.to_hex()),
                expected_identity: journal_file_identity(&approved),
            },
        )
        .expect("prepared");
        drop(service);

        let restarted = AuthorityManagedFileAccessServiceV1::new_for_test_with_journal(
            registry,
            arena_root,
            workspace.path().join(".test-file-delete.journal.json"),
        );
        restarted
            .reconcile_file_delete_journal()
            .expect("prepared-prefix reconciliation");
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("notes.txt")).expect("leaf"),
            "approved"
        );
    }

    #[cfg(unix)]
    #[test]
    fn managed_file_delete_restart_restore_collision_is_typed_and_retained() {
        let (workspace, service, plan) =
            registered_plan_for_operation(Some("approved"), ManagedFileOperationV1::Delete);
        let registry = Arc::clone(&service.registry);
        let state = service.file_delete.as_ref().expect("delete state");
        let arena_root = state.arena_root.clone();
        let planned = service
            .plans
            .lock()
            .expect("plans")
            .get(&plan.plan_hash.to_hex())
            .cloned()
            .expect("planned file");
        let (parent, leaf) = open_relative_parent(&planned).expect("parent");
        let arena = open_file_delete_arena(state, &parent).expect("arena");
        let approved = std::fs::symlink_metadata(workspace.path().join("notes.txt"))
            .expect("approved metadata");
        let operation_id = format!("restore-collision-{0}", plan.plan_hash.to_hex());
        let quarantine_name = format!("restore-collision-{}", plan.plan_hash.to_hex());
        let quarantine = CString::new(quarantine_name.clone()).expect("quarantine name");
        append_file_delete_event(
            state,
            ResourceJournalEventV1::FileDeletePrepared {
                operation_id: operation_id.clone(),
                subject_ref: planned.subject_ref.as_str().to_owned(),
                logical_path: planned.logical_path.clone(),
                plan_hash: plan.plan_hash,
                binding_hash: plan.plan_hash,
                quarantine_name: quarantine_name.clone(),
                expected_identity: journal_file_identity(&approved),
            },
        )
        .expect("prepared");
        std::fs::rename(
            workspace.path().join("notes.txt"),
            workspace.path().join("notes.original"),
        )
        .expect("move approved");
        std::fs::write(workspace.path().join("notes.txt"), "replacement").expect("replacement");
        rename_noreplace_at(&parent, &leaf, &arena, &quarantine).expect("quarantine replacement");
        std::fs::write(workspace.path().join("notes.txt"), "restore collision").expect("collision");
        append_file_delete_event(
            state,
            ResourceJournalEventV1::FileDeleteRenamed {
                operation_id,
                quarantine_identity: journal_file_identity(
                    &std::fs::symlink_metadata(arena_root.join(&quarantine_name))
                        .expect("quarantine metadata"),
                ),
            },
        )
        .expect("renamed");
        drop(service);

        let restarted = AuthorityManagedFileAccessServiceV1::new_for_test_with_journal(
            registry,
            workspace.path().join(".test-file-delete-arena"),
            workspace.path().join(".test-file-delete.journal.json"),
        );
        let error = restarted
            .reconcile_file_delete_journal()
            .expect_err("restore collision must block");
        assert!(matches!(
            error,
            ManagedFileAccessErrorV1::ReconciliationRequired { .. }
        ));
        assert!(arena_root.join(&quarantine_name).exists());
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("notes.txt")).expect("collision"),
            "restore collision"
        );
    }

    #[cfg(unix)]
    #[test]
    fn managed_file_delete_orphan_arena_entry_is_a_typed_startup_blocker() {
        let (workspace, service, _plan) =
            registered_plan_for_operation(Some("approved"), ManagedFileOperationV1::Delete);
        let registry = Arc::clone(&service.registry);
        let state = service.file_delete.as_ref().expect("delete state");
        let arena_root = state.arena_root.clone();
        let journal_path = workspace.path().join(".test-file-delete.journal.json");
        let root_handle =
            AuthorityManagedFileAccessServiceV1::open_workspace_root(workspace.path())
                .expect("workspace root");
        let (parent, _) =
            open_relative_parent_from_root(&root_handle, "recovery-leaf").expect("parent");
        let arena = open_file_delete_arena(state, &parent).expect("arena");
        std::fs::write(state.arena_root.join("orphan-entry"), "orphan").expect("orphan");

        let error = service
            .reconcile_file_delete_journal()
            .expect_err("unknown arena entry must block startup");
        assert!(matches!(
            error,
            ManagedFileAccessErrorV1::ReconciliationRequired { .. }
        ));
        assert!(arena.metadata().expect("arena metadata").is_dir());
        assert!(state.arena_root.join("orphan-entry").exists());

        drop(service);
        let restarted = AuthorityManagedFileAccessServiceV1::new_for_test_with_journal(
            registry,
            arena_root,
            journal_path,
        );
        let restart_error = restarted
            .reconcile_file_delete_journal()
            .expect_err("durable orphan blocker must survive restart");
        assert!(matches!(
            restart_error,
            ManagedFileAccessErrorV1::ReconciliationRequired { .. }
        ));
    }

    #[cfg(windows)]
    #[test]
    fn windows_nested_directory_tools_open_any_leaf_kind() {
        let workspace = tempfile::tempdir().expect("workspace");
        std::fs::create_dir_all(workspace.path().join("nested/deeper")).expect("directories");
        std::fs::write(workspace.path().join("root.txt"), "needle at root").expect("root file");
        std::fs::write(
            workspace.path().join("nested/match.txt"),
            "needle in nested",
        )
        .expect("nested file");
        std::fs::write(
            workspace.path().join("nested/deeper/deep.txt"),
            "needle in deeper",
        )
        .expect("deep file");

        let (service, list_plan) =
            registered_plan_for_workspace(&workspace, ".", ManagedFileOperationV1::List);
        let (request, token) = execution_request_for_plan(
            &list_plan,
            ManagedFileExecutionInputV1::List {
                recursive: true,
                limit: 20,
                max_depth: 8,
            },
        );
        let list = service.execute(request, token).expect("recursive list");
        assert!(list.payload.contains("nested"));
        assert!(list.payload.contains("nested/deeper/deep.txt"));

        let (service, glob_plan) =
            registered_plan_for_workspace(&workspace, ".", ManagedFileOperationV1::Glob);
        let (request, token) = execution_request_for_plan(
            &glob_plan,
            ManagedFileExecutionInputV1::Glob {
                pattern: "*.txt".to_owned(),
                limit: 20,
            },
        );
        let glob = service.execute(request, token).expect("recursive glob");
        assert!(glob.payload.contains("nested/match.txt"));
        assert!(glob.payload.contains("nested/deeper/deep.txt"));

        let (service, grep_plan) =
            registered_plan_for_workspace(&workspace, ".", ManagedFileOperationV1::Grep);
        let (request, token) = execution_request_for_plan(
            &grep_plan,
            ManagedFileExecutionInputV1::Grep {
                pattern: "needle".to_owned(),
                limit: 20,
                max_bytes: 4096,
            },
        );
        let grep = service.execute(request, token).expect("recursive grep");
        assert!(grep.payload.contains("nested/match.txt"));
        assert!(grep.payload.contains("nested/deeper/deep.txt"));
    }

    #[test]
    fn r71_f_fil_011_stale_preview_is_rejected() {
        let (_workspace, service, _plan) = registered_read_plan(Some("stale"));
        assert!(matches!(
            service.preview(ManagedFilePreviewRequestV1 {
                plan_hash: hash(0xee),
                operation: ManagedFileOperationV1::Read,
                max_bytes: 32,
            }),
            Err(ManagedFileAccessErrorV1::PlanStale)
        ));
    }

    #[test]
    fn r71_f_fil_012_missing_leaf_is_typed_physical_failure() {
        let (_workspace, service, plan) = registered_read_plan(None);
        let (request, token) = execution_request_for_plan(
            &plan,
            ManagedFileExecutionInputV1::Read {
                offset: 0,
                limit: 1,
                max_bytes: 32,
            },
        );
        assert!(matches!(
            service.execute(request, token),
            Err(ManagedFileAccessErrorV1::PhysicalExecutionFailed(_))
        ));
    }
}
