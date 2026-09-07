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
use std::io::Read;
#[cfg(windows)]
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

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

use crate::borrowed::{BorrowedSubjectClassV1, BorrowedSubjectRegistryV1};

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
}

/// Hard ceiling for directory entries inspected by one list/glob/grep operation. The public
/// result marks a scan truncated once this work budget is reached; `limit` only bounds the
/// returned projection and must not be mistaken for a work budget.
const MAX_DIRECTORY_SCAN_ENTRIES: usize = 100_000;

/// Hard ceiling for recursive directory depth. Callers may request a smaller depth, but never
/// enlarge the authority's work budget by passing `usize::MAX`.
const MAX_DIRECTORY_SCAN_DEPTH: usize = 64;

/// Hard ceiling for bytes read by one direct file read. This keeps offset paging and preview
/// operations bounded even when the requested page is beyond a very large file.
const MAX_READ_SCAN_BYTES: u64 = 64 * 1024 * 1024;

/// Hard ceiling for bytes read by one recursive grep scan. A bounded scan may return a partial
/// match set with `truncated=true`, but it never reads an unbounded file tree into memory.
const MAX_GREP_SCAN_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone)]
struct PlannedFileAccessV1 {
    subject_ref: OpaquePermissionSubjectRef,
    root: PathBuf,
    logical_path: String,
    operation_scope: String,
    physical_path: PathBuf,
    #[cfg(any(unix, windows))]
    root_handle: Arc<std::fs::File>,
    expected_physical_identity: Option<CanonicalHash>,
    expected_content_digest: Option<CanonicalHash>,
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
        }
    }

    fn claim_key(token: &ManagedFileAccessAdmissionTokenV1) -> String {
        match token {
            ManagedFileAccessAdmissionTokenV1::Tool(tool) => tool.claim_id().to_owned(),
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

    fn expected_content_digest(
        path: &Path,
    ) -> Result<Option<CanonicalHash>, ManagedFileAccessErrorV1> {
        let metadata = match std::fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(ManagedFileAccessErrorV1::PhysicalExecutionFailed(
                    error.to_string(),
                ));
            }
        };
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Ok(None);
        }
        let content = std::fs::read(path).map_err(|error| {
            ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
        })?;
        Ok(Some(content_digest(&content)))
    }

    fn verify_planned_physical_identity(
        plan: &PlannedFileAccessV1,
    ) -> Result<(), ManagedFileAccessErrorV1> {
        if Self::current_physical_identity(&plan.physical_path)? != plan.expected_physical_identity
        {
            return Err(ManagedFileAccessErrorV1::PlanStale);
        }
        if Self::expected_content_digest(&plan.physical_path)? != plan.expected_content_digest {
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

fn content_digest(content: &[u8]) -> CanonicalHash {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(content);
    CanonicalHash::from_bytes(hasher.finalize().into())
}

fn mutation_error(
    error: impl std::fmt::Display,
    plan: &PlannedFileAccessV1,
) -> ManagedFileAccessErrorV1 {
    let message = error.to_string();
    if message.contains("changed before") || message.contains("does not match") {
        ManagedFileAccessErrorV1::PlanStale
    } else {
        ManagedFileAccessErrorV1::ReconciliationRequired {
            operation_id: plan.plan_hash.to_hex(),
            binding_hash: plan.operation_digest,
        }
    }
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
        let expected_content_digest = Self::expected_content_digest(&physical_path)?;
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
                    root,
                    logical_path,
                    operation_scope: request.operation_scope,
                    physical_path,
                    #[cfg(any(unix, windows))]
                    root_handle,
                    expected_physical_identity,
                    expected_content_digest,
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
            // Older qualification fixtures use descriptive scopes without the canonical NUL
            // framing. Shipping planners always use `operation_scope()`; retain fixture
            // compatibility while enforcing the production binding whenever canonical data is
            // present.
            || (plan.operation_scope.contains('\0')
                && plan.operation_scope != request.input.operation_scope())
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
        let mutation_recorder = request.mutation_recorder;
        let physical = execute_physical(&plan, request.input, mutation_recorder.as_ref());
        // An admitted call owns its plan for exactly one physical attempt. Remove it before
        // propagating the physical result so errors cannot pin authority handles indefinitely.
        self.plans
            .lock()
            .map_err(|_| ManagedFileAccessErrorV1::PlanStale)?
            .remove(&file_access_plan_hash.to_hex());
        let PhysicalExecutionOutcomeV1 {
            payload,
            observed_bytes,
            returned_entries,
            total_entries,
            returned_lines,
            total_lines,
            truncated,
        } = physical?;
        let result_digest =
            Self::hash_parts(&[payload.as_bytes(), result.result_digest.as_bytes()]);
        let changed_files = match request.access.operation {
            ManagedFileOperationV1::Write
            | ManagedFileOperationV1::Edit
            | ManagedFileOperationV1::Delete
            | ManagedFileOperationV1::Rename => vec![plan.logical_path.clone()],
            ManagedFileOperationV1::Read
            | ManagedFileOperationV1::List
            | ManagedFileOperationV1::Glob
            | ManagedFileOperationV1::Grep => Vec::new(),
        };
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
            changed_files,
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
        // A write plan may intentionally target an absent leaf.  Previewing that plan is still
        // required before the user can approve it, so an absent target is the empty current
        // document rather than a physical failure.  The effectful path remains create-new and
        // identity-bound in the descriptor-relative atomic replacement path.
        let (raw, source_truncated) = if request.operation == ManagedFileOperationV1::Write
            && plan.expected_physical_identity.is_none()
        {
            (String::new(), false)
        } else {
            read_relative_text_with_budget(&plan, MAX_READ_SCAN_BYTES)?
        };
        let safe = sigil_kernel::safe_persistence_text(&raw);
        let truncated = source_truncated || safe.len() > request.max_bytes;
        let payload = if truncated {
            truncate_utf8(&safe, request.max_bytes)
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
    // Every descriptor-relative component is opened without following aliases. Callers that
    // need a directory or regular file still validate the resulting handle/type separately.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            component.as_ptr(),
            // O_NONBLOCK prevents a FIFO or other special node from stalling the authority
            // before its type can be rejected below. It is ignored for regular files/directories.
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
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

fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut end = max_bytes.min(value.len());
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
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
    if identity.is_symlink
        || (!identity.is_regular_file && !identity.is_directory)
        || (identity.is_regular_file && identity.link_count > 1)
    {
        return Err(ManagedFileAccessErrorV1::AliasCollision);
    }
    match plan.expected_physical_identity {
        Some(expected) if identity.digest != expected => Err(ManagedFileAccessErrorV1::PlanStale),
        None if !allow_create_for_absent_plan => Err(ManagedFileAccessErrorV1::PlanStale),
        _ => Ok(file),
    }
}

#[cfg(unix)]
fn read_relative_text_with_budget(
    plan: &PlannedFileAccessV1,
    max_bytes: u64,
) -> Result<(String, bool), ManagedFileAccessErrorV1> {
    let mut file = open_relative_path(plan, libc::O_RDONLY)?;
    read_text_with_budget(&mut file, max_bytes).map_err(|error| error.source)
}

#[cfg(windows)]
fn read_relative_text_with_budget(
    plan: &PlannedFileAccessV1,
    max_bytes: u64,
) -> Result<(String, bool), ManagedFileAccessErrorV1> {
    let handle = windows_open_plan(plan, WindowsOpenKind::Read, false)?;
    let mut file = handle.file;
    read_text_with_budget(&mut file, max_bytes).map_err(|error| error.source)
}

#[cfg(not(any(unix, windows)))]
fn read_relative_text_with_budget(
    plan: &PlannedFileAccessV1,
    max_bytes: u64,
) -> Result<(String, bool), ManagedFileAccessErrorV1> {
    let mut file = std::fs::File::open(&plan.physical_path)
        .map_err(|error| ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string()))?;
    read_text_with_budget(&mut file, max_bytes).map_err(|error| error.source)
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
    mutation_recorder: Option<&sigil_kernel::MutationEventRecorder>,
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
            let (raw, source_truncated) =
                read_relative_text_with_budget(plan, MAX_READ_SCAN_BYTES)?;
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
            let truncated = source_truncated
                || offset.saturating_add(selected.len()) < lines.len()
                || payload.len() > max_bytes;
            if payload.len() > max_bytes {
                payload = truncate_utf8(&payload, max_bytes);
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
            let mut scanned_entries = 0usize;
            let mut scan_truncated = false;
            collect_entries_relative(
                &directory,
                "",
                recursive,
                max_depth.min(MAX_DIRECTORY_SCAN_DEPTH),
                0,
                &mut entries,
                None,
                &mut scanned_entries,
                &mut scan_truncated,
            )?;
            entries.sort();
            let total = entries.len();
            let truncated = total > limit || scan_truncated;
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
            let mut scan_truncated = false;
            let mut scanned_entries = 0usize;
            let display_path = if plan.logical_path == "." {
                String::new()
            } else {
                plan.logical_path.clone()
            };
            if is_directory(&target).map_err(|error| {
                ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
            })? {
                let ignore = build_gitignore(plan)?;
                collect_grep_relative(
                    &target,
                    &display_path,
                    &regex,
                    &mut matches,
                    &mut observed_bytes,
                    &ignore,
                    &mut scanned_entries,
                    &mut scan_truncated,
                )?;
            } else {
                let mut file = target;
                let (raw, was_truncated) = read_text_with_budget(&mut file, MAX_GREP_SCAN_BYTES)
                    .map_err(|error| error.source)?;
                scan_truncated |= was_truncated;
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
            let truncated = total > limit || scan_truncated;
            matches.truncate(limit);
            let mut payload = matches.join("\n");
            let byte_truncated = payload.len() > max_bytes;
            if payload.len() > max_bytes {
                payload = truncate_utf8(&payload, max_bytes);
            }
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
            let matcher = globset::Glob::new(&pattern)
                .map_err(|error| {
                    ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
                })?
                .compile_matcher();
            let directory = open_relative_path(plan, libc::O_RDONLY | libc::O_DIRECTORY)?;
            let ignore = build_gitignore(plan)?;
            let mut entries = Vec::new();
            let mut scanned_entries = 0usize;
            let mut scan_truncated = false;
            collect_entries_relative(
                &directory,
                "",
                true,
                MAX_DIRECTORY_SCAN_DEPTH,
                0,
                &mut entries,
                Some(&ignore),
                &mut scanned_entries,
                &mut scan_truncated,
            )?;
            entries.retain(|entry| matcher.is_match(entry));
            entries.sort();
            let total = entries.len();
            let truncated = total > limit || scan_truncated;
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
            let recorder = mutation_recorder.ok_or_else(|| {
                ManagedFileAccessErrorV1::PhysicalExecutionFailed(
                    "mutation recorder is required for workspace writes".to_owned(),
                )
            })?;
            sigil_kernel::write_file_with_mutation_expected_in_batch(
                Some(recorder),
                &plan.root,
                &plan.plan_hash.to_hex(),
                None,
                &plan.logical_path,
                &plan.physical_path,
                plan.expected_content_digest
                    .map(|digest| format!("sha256:{}", digest.to_hex())),
                content.as_bytes(),
            )
            .map_err(|error| mutation_error(error, plan))?;
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
            let mut file = open_relative_path(plan, libc::O_RDONLY)?;
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
            let recorder = mutation_recorder.ok_or_else(|| {
                ManagedFileAccessErrorV1::PhysicalExecutionFailed(
                    "mutation recorder is required for workspace edits".to_owned(),
                )
            })?;
            sigil_kernel::write_file_with_mutation_expected_in_batch(
                Some(recorder),
                &plan.root,
                &plan.plan_hash.to_hex(),
                None,
                &plan.logical_path,
                &plan.physical_path,
                plan.expected_content_digest
                    .map(|digest| format!("sha256:{}", digest.to_hex())),
                updated.as_bytes(),
            )
            .map_err(|error| mutation_error(error, plan))?;
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
            let recorder = mutation_recorder.ok_or_else(|| {
                ManagedFileAccessErrorV1::PhysicalExecutionFailed(
                    "mutation recorder is required for workspace deletes".to_owned(),
                )
            })?;
            if plan.expected_physical_identity.is_none() {
                return Err(ManagedFileAccessErrorV1::PlanStale);
            }
            sigil_kernel::delete_file_with_mutation_expected_in_batch(
                Some(recorder),
                &plan.root,
                &plan.plan_hash.to_hex(),
                None,
                &plan.logical_path,
                &plan.physical_path,
                plan.expected_content_digest
                    .map(|digest| format!("sha256:{}", digest.to_hex())),
            )
            .map_err(|error| mutation_error(error, plan))?;
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
    Ok(matches!(
        entry_kind_at(directory, name)?,
        DirectoryEntryKind::Directory
    ))
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DirectoryEntryKind {
    Directory,
    Regular,
    Symlink,
    Other,
}

#[cfg(unix)]
fn entry_kind_at(directory: &std::fs::File, name: &str) -> std::io::Result<DirectoryEntryKind> {
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
    Ok(match stat.st_mode & libc::S_IFMT {
        libc::S_IFDIR => DirectoryEntryKind::Directory,
        libc::S_IFREG => DirectoryEntryKind::Regular,
        libc::S_IFLNK => DirectoryEntryKind::Symlink,
        _ => DirectoryEntryKind::Other,
    })
}

#[derive(Debug)]
struct BoundedTextReadError {
    observed_bytes: u64,
    source: ManagedFileAccessErrorV1,
}

fn read_text_with_budget(
    file: &mut std::fs::File,
    max_bytes: u64,
) -> Result<(String, bool), BoundedTextReadError> {
    let read_limit = max_bytes.saturating_add(1).try_into().unwrap_or(usize::MAX);
    let mut bytes = Vec::new();
    if file
        .take(read_limit as u64)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return Err(BoundedTextReadError {
            observed_bytes: bytes.len() as u64,
            source: ManagedFileAccessErrorV1::PhysicalExecutionFailed(
                "non-UTF-8 or unreadable file".to_owned(),
            ),
        });
    }
    let observed_bytes = bytes.len() as u64;
    let truncated = observed_bytes > max_bytes;
    if truncated {
        bytes.truncate(max_bytes.try_into().unwrap_or(usize::MAX));
        if let Err(error) = std::str::from_utf8(&bytes) {
            if error.error_len().is_some() {
                return Err(BoundedTextReadError {
                    observed_bytes,
                    source: ManagedFileAccessErrorV1::PhysicalExecutionFailed(
                        "non-UTF-8 or unreadable file".to_owned(),
                    ),
                });
            }
            bytes.truncate(error.valid_up_to());
        }
    }
    let text = String::from_utf8(bytes).map_err(|_| BoundedTextReadError {
        observed_bytes,
        source: ManagedFileAccessErrorV1::PhysicalExecutionFailed(
            "non-UTF-8 or unreadable file".to_owned(),
        ),
    })?;
    Ok((text, truncated))
}

#[cfg(unix)]
fn build_gitignore(
    plan: &PlannedFileAccessV1,
) -> Result<ignore::gitignore::Gitignore, ManagedFileAccessErrorV1> {
    let mut builder = ignore::gitignore::GitignoreBuilder::new(&plan.root);
    let mut directories = vec![plan.root.clone()];
    let mut visited = 0usize;
    while let Some(directory) = directories.pop() {
        let entries = std::fs::read_dir(&directory).map_err(|error| {
            ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
        })?;
        for entry in entries {
            let entry = entry.map_err(|error| {
                ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
            })?;
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
                ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
            })?;
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                directories.push(path);
                continue;
            }
            if metadata.is_file()
                && entry.file_name() == ".gitignore"
                && let Some(error) = builder.add(&path)
            {
                return Err(ManagedFileAccessErrorV1::PhysicalExecutionFailed(
                    error.to_string(),
                ));
            }
            visited = visited.saturating_add(1);
            if visited > 100_000 {
                return Err(ManagedFileAccessErrorV1::PhysicalExecutionFailed(
                    "gitignore discovery exceeded the directory entry budget".to_owned(),
                ));
            }
        }
    }
    builder
        .build()
        .map_err(|error| ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string()))
}

#[cfg(unix)]
fn collect_entries_relative(
    directory: &std::fs::File,
    prefix: &str,
    recursive: bool,
    max_depth: usize,
    depth: usize,
    entries: &mut Vec<String>,
    ignore: Option<&ignore::gitignore::Gitignore>,
    scanned_entries: &mut usize,
    scan_truncated: &mut bool,
) -> Result<(), ManagedFileAccessErrorV1> {
    for name in directory_entry_names(directory)
        .map_err(|error| ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string()))?
    {
        if *scanned_entries >= MAX_DIRECTORY_SCAN_ENTRIES {
            *scan_truncated = true;
            break;
        }
        *scanned_entries = (*scanned_entries).saturating_add(1);
        let kind = entry_kind_at(directory, &name).map_err(|error| {
            ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
        })?;
        if matches!(
            kind,
            DirectoryEntryKind::Symlink | DirectoryEntryKind::Other
        ) {
            continue;
        }
        let relative = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        if ignore.is_some_and(|ignore| {
            ignore
                .matched_path_or_any_parents(
                    Path::new(&relative),
                    kind == DirectoryEntryKind::Directory,
                )
                .is_ignore()
        }) {
            continue;
        }
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
                ignore,
                scanned_entries,
                scan_truncated,
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
    ignore: &ignore::gitignore::Gitignore,
    scanned_entries: &mut usize,
    scan_truncated: &mut bool,
) -> Result<(), ManagedFileAccessErrorV1> {
    for name in directory_entry_names(directory)
        .map_err(|error| ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string()))?
    {
        if *scanned_entries >= MAX_DIRECTORY_SCAN_ENTRIES {
            *scan_truncated = true;
            break;
        }
        *scanned_entries = (*scanned_entries).saturating_add(1);
        let relative = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let kind = entry_kind_at(directory, &name).map_err(|error| {
            ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
        })?;
        if matches!(
            kind,
            DirectoryEntryKind::Symlink | DirectoryEntryKind::Other
        ) {
            continue;
        }
        if ignore
            .matched_path_or_any_parents(
                Path::new(&relative),
                kind == DirectoryEntryKind::Directory,
            )
            .is_ignore()
        {
            continue;
        }
        if kind == DirectoryEntryKind::Directory {
            let child = open_at(directory, &name, libc::O_RDONLY | libc::O_DIRECTORY, 0)
                .map_err(relative_io_error)?;
            collect_grep_relative(
                &child,
                &relative,
                regex,
                matches,
                observed_bytes,
                ignore,
                scanned_entries,
                scan_truncated,
            )?;
            continue;
        }
        let mut file = match open_at(directory, &name, libc::O_RDONLY, 0) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => continue,
            Err(error) => return Err(relative_io_error(error)),
        };
        let remaining = MAX_GREP_SCAN_BYTES.saturating_sub(*observed_bytes);
        if remaining == 0 {
            *scan_truncated = true;
            break;
        }
        let (raw, was_truncated) = match read_text_with_budget(&mut file, remaining) {
            Ok(result) => result,
            Err(error) => {
                *observed_bytes = observed_bytes.saturating_add(error.observed_bytes);
                if *observed_bytes >= MAX_GREP_SCAN_BYTES {
                    *scan_truncated = true;
                    break;
                }
                continue;
            }
        };
        if was_truncated {
            *scan_truncated = true;
        }
        if raw.is_empty() && was_truncated {
            break;
        }
        if raw.is_empty() {
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
    scanned_entries: &mut usize,
    scan_truncated: &mut bool,
) -> Result<(), ManagedFileAccessErrorV1> {
    let directory =
        windows_open_relative_root(root, base, WindowsOpenKind::Directory, false, root_guard)?;
    let directory_path = if base == "." || base.is_empty() {
        root.to_path_buf()
    } else {
        root.join(base.replace('/', &std::path::MAIN_SEPARATOR.to_string()))
    };
    for entry in std::fs::read_dir(&directory_path).map_err(relative_io_error)? {
        if *scanned_entries >= MAX_DIRECTORY_SCAN_ENTRIES {
            *scan_truncated = true;
            break;
        }
        let entry = entry.map_err(relative_io_error)?;
        *scanned_entries = (*scanned_entries).saturating_add(1);
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
                scanned_entries,
                scan_truncated,
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
    scanned_entries: &mut usize,
    scan_truncated: &mut bool,
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
            if *scanned_entries >= MAX_DIRECTORY_SCAN_ENTRIES {
                *scan_truncated = true;
                break;
            }
            let name = entry
                .map_err(relative_io_error)?
                .file_name()
                .to_string_lossy()
                .into_owned();
            *scanned_entries = (*scanned_entries).saturating_add(1);
            windows_collect_grep(
                root,
                &windows_child_path(base, &name),
                regex,
                matches,
                observed_bytes,
                root_guard,
                scanned_entries,
                scan_truncated,
            )?;
        }
        return Ok(());
    }
    let mut file = handle.file;
    let (raw, was_truncated) =
        read_text_with_budget(&mut file, MAX_GREP_SCAN_BYTES).map_err(|error| error.source)?;
    *scan_truncated |= was_truncated;
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
            let (raw, source_truncated) = read_text_with_budget(&mut file, MAX_READ_SCAN_BYTES)
                .map_err(|error| error.source)?;
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
            let truncated = source_truncated
                || offset.saturating_add(selected.len()) < lines.len()
                || payload.len() > max_bytes;
            payload = truncate_utf8(&payload, max_bytes);
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
            let mut scanned_entries = 0usize;
            let mut scan_truncated = false;
            windows_collect_entries(
                &plan.root,
                &plan.logical_path,
                recursive,
                max_depth.min(MAX_DIRECTORY_SCAN_DEPTH),
                0,
                &mut entries,
                Some(plan.root_handle.as_ref()),
                &mut scanned_entries,
                &mut scan_truncated,
            )?;
            entries.sort();
            let total = entries.len();
            let truncated = total > limit || scan_truncated;
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
            let mut scanned_entries = 0usize;
            let mut scan_truncated = false;
            windows_collect_grep(
                &plan.root,
                &plan.logical_path,
                &regex,
                &mut matches,
                &mut observed_bytes,
                Some(plan.root_handle.as_ref()),
                &mut scanned_entries,
                &mut scan_truncated,
            )?;
            let total = matches.len();
            let truncated = total > limit || scan_truncated;
            matches.truncate(limit);
            let mut payload = matches.join("\n");
            let byte_truncated = payload.len() > max_bytes;
            payload = truncate_utf8(&payload, max_bytes);
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
            let mut scanned_entries = 0usize;
            let mut scan_truncated = false;
            windows_collect_entries(
                &plan.root,
                &plan.logical_path,
                true,
                MAX_DIRECTORY_SCAN_DEPTH,
                0,
                &mut entries,
                Some(plan.root_handle.as_ref()),
                &mut scanned_entries,
                &mut scan_truncated,
            )?;
            entries.retain(|entry| matcher.is_match(entry));
            entries.sort();
            let total = entries.len();
            let truncated = total > limit || scan_truncated;
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
#[path = "tests/file_access_tests.rs"]
mod tests;
