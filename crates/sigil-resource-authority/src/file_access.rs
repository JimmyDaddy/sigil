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

use std::collections::BTreeSet;
#[cfg(unix)]
use std::ffi::{CStr, CString};
#[cfg(any(unix, windows))]
use std::fs::OpenOptions;
use std::io::Read;

mod cache;
mod streaming;
mod traversal;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use traversal::{collect_plan_entries, collect_plan_grep, open_plan_file};

#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;

use sigil_kernel::managed_execution::BorrowedResourceAccessReceiptV1;
use sigil_kernel::managed_file_access::ManagedFileExecutionInputV1;
use sigil_kernel::managed_file_access::{
    ManagedFileAccessAdmissionTokenV1, ManagedFileAccessErrorV1, ManagedFileAccessPlanRequestV1,
    ManagedFileAccessRequestV1, ManagedFileAccessResultV1, ManagedFileAccessServiceV1,
    ManagedFileAdmissionBindingV1, ManagedFileExecutionContextV1, ManagedFileExecutionOutcomeV1,
    ManagedFileExecutionRequestV1, ManagedFileOperationV1, ManagedFileOutputCaptureV1,
    ManagedFileOutputDetailsV1,
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
    plans: Mutex<cache::PlannedFileCache>,
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
    operation_scope_digest: CanonicalHash,
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
            plans: Mutex::new(cache::PlannedFileCache::default()),
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
        plan: &PlannedFileAccessV1,
        context: Option<&ManagedFileExecutionContextV1>,
    ) -> Result<Option<CanonicalHash>, ManagedFileAccessErrorV1> {
        if plan.expected_physical_identity.is_none()
            || !operation_requires_content_snapshot(plan.operation)
        {
            return Ok(None);
        }
        let (file, _ancestors) = open_plan_file(plan)?;
        if !file.metadata().map_err(relative_io_error)?.is_file() {
            return Ok(None);
        }
        hash_file(file, context).map(Some)
    }

    fn verify_planned_physical_identity(
        plan: &PlannedFileAccessV1,
        context: Option<&ManagedFileExecutionContextV1>,
    ) -> Result<(), ManagedFileAccessErrorV1> {
        if Self::current_physical_identity(&plan.physical_path)? != plan.expected_physical_identity
        {
            return Err(ManagedFileAccessErrorV1::PlanStale);
        }
        if Self::expected_content_digest(plan, context)? != plan.expected_content_digest {
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

fn hash_file(
    mut file: std::fs::File,
    context: Option<&ManagedFileExecutionContextV1>,
) -> Result<CanonicalHash, ManagedFileAccessErrorV1> {
    use sha2::{Digest, Sha256};
    if file.metadata().map_err(relative_io_error)?.len() > MAX_READ_SCAN_BYTES {
        return Err(ManagedFileAccessErrorV1::ResourceLimit(
            "file exceeds the authority snapshot limit".to_owned(),
        ));
    }
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut observed = 0u64;
    loop {
        if let Some(context) = context {
            streaming::check_context(context)?;
        }
        let length = file.read(&mut buffer).map_err(relative_io_error)?;
        if length == 0 {
            break;
        }
        observed = observed.saturating_add(length as u64);
        // Metadata is only a fast refusal. Retain the observed-byte bound because the opened
        // file may grow while planning or snapshot revalidation is reading it.
        if observed > MAX_READ_SCAN_BYTES {
            return Err(ManagedFileAccessErrorV1::ResourceLimit(
                "file exceeds the authority snapshot limit".to_owned(),
            ));
        }
        hasher.update(&buffer[..length]);
    }
    Ok(CanonicalHash::from_bytes(hasher.finalize().into()))
}

fn operation_requires_content_snapshot(operation: ManagedFileOperationV1) -> bool {
    matches!(
        operation,
        ManagedFileOperationV1::Write
            | ManagedFileOperationV1::Edit
            | ManagedFileOperationV1::Delete
            | ManagedFileOperationV1::Rename
    )
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
        // The request can arrive through deserialization as well as the validating constructor.
        sigil_kernel::managed_file_access::ManagedFileLogicalPathV1::new(&logical_path)?;
        let physical_path = logical_path
            .split('/')
            .filter(|component| !component.is_empty() && *component != ".")
            .fold(root.clone(), |path, component| path.join(component));
        #[cfg(any(unix, windows))]
        let root_handle = Self::open_workspace_root(&root)?;
        #[cfg(any(unix, windows))]
        let (expected_physical_identity, expected_content_digest) = traversal::plan_snapshot(
            &root,
            &logical_path,
            &root_handle,
            operation_requires_content_snapshot(request.operation),
        )?;
        #[cfg(not(any(unix, windows)))]
        let (expected_physical_identity, expected_content_digest): (
            Option<CanonicalHash>,
            Option<CanonicalHash>,
        ) = return Err(ManagedFileAccessErrorV1::OperationNotPermitted);
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
            expected_content_digest
                .unwrap_or_else(|| Self::hash_parts(&[b"no-file-content"]))
                .as_bytes(),
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
            target_exists: expected_physical_identity.is_some(),
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
                    operation_scope_digest: Self::hash_parts(&[request.operation_scope.as_bytes()]),
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
            || plan.operation_scope_digest
                != Self::hash_parts(&[request.input.operation_scope().as_bytes()])
        {
            return Err(ManagedFileAccessErrorV1::AdmissionMismatch);
        }
        // Revalidate both the borrowed root and the approved leaf before consuming the one-shot
        // token. Root drift takes precedence because the leaf path is no longer meaningful when
        // its authority boundary has been replaced. A replacement inode, absent-to-present
        // transition, or hard-link alias must not reach an effectful open/truncate/unlink.
        streaming::check_context(&request.context)?;
        let current_root = self.current_root_identity(&plan.subject_ref)?;
        if current_root != plan.root_identity {
            return Err(ManagedFileAccessErrorV1::SubjectIdentityDrift);
        }
        Self::verify_planned_physical_identity(&plan, Some(&request.context))?;
        // Validate the pathless plan before consuming the one-shot admission. A stale or
        // cross-plan request must not burn a valid approval token.
        let result = self.access(request.access.clone(), token)?;
        let mut context = request.context;
        let physical = execute_physical(&plan, request.input, &mut context);
        // A snapshot may serve multiple independently admitted calls. The bounded cache owns
        // its lifetime; only the kernel-issued admission token is consumed by this attempt.
        let PhysicalExecutionOutcomeV1 {
            payload,
            observed_bytes,
            returned_entries,
            total_entries,
            returned_lines,
            total_lines,
            truncated,
            output_capture,
            output_details,
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
            output_capture,
            output_details,
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
        Self::verify_planned_physical_identity(&plan, None)?;
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
    } else if error.kind() == std::io::ErrorKind::NotFound {
        ManagedFileAccessErrorV1::NotFound
    } else if error.kind() == std::io::ErrorKind::PermissionDenied {
        ManagedFileAccessErrorV1::PermissionDenied
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
    } else if error.kind() == std::io::ErrorKind::NotFound {
        ManagedFileAccessErrorV1::NotFound
    } else if error.kind() == std::io::ErrorKind::PermissionDenied {
        ManagedFileAccessErrorV1::PermissionDenied
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
    read_text_with_budget(&mut file, max_bytes)
}

#[cfg(windows)]
fn read_relative_text_with_budget(
    plan: &PlannedFileAccessV1,
    max_bytes: u64,
) -> Result<(String, bool), ManagedFileAccessErrorV1> {
    let handle = windows_open_plan(plan, WindowsOpenKind::Read, false)?;
    let mut file = handle.file;
    read_text_with_budget(&mut file, max_bytes)
}

#[cfg(not(any(unix, windows)))]
fn read_relative_text_with_budget(
    _plan: &PlannedFileAccessV1,
    _max_bytes: u64,
) -> Result<(String, bool), ManagedFileAccessErrorV1> {
    Err(ManagedFileAccessErrorV1::OperationNotPermitted)
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

#[derive(Default)]
struct PhysicalExecutionOutcomeV1 {
    payload: String,
    observed_bytes: u64,
    returned_entries: u64,
    total_entries: u64,
    returned_lines: u64,
    total_lines: u64,
    truncated: bool,
    output_capture: ManagedFileOutputCaptureV1,
    output_details: ManagedFileOutputDetailsV1,
}

fn execute_physical(
    plan: &PlannedFileAccessV1,
    input: sigil_kernel::managed_file_access::ManagedFileExecutionInputV1,
    context: &mut ManagedFileExecutionContextV1,
) -> Result<PhysicalExecutionOutcomeV1, ManagedFileAccessErrorV1> {
    streaming::check_context(context)?;
    let cancellation = context.cancellation.clone();
    let _effect = cancellation
        .as_ref()
        .map(|handle| {
            handle.begin_effect(
                sigil_kernel::RunEffectClass::Forward,
                sigil_kernel::RunEffectKind::Tool,
            )
        })
        .transpose()
        .map_err(|_| ManagedFileAccessErrorV1::Interrupted)?;
    let mutation_recorder = context.mutation_recorder.as_ref();
    match (plan.operation, input) {
        (
            ManagedFileOperationV1::Read,
            ManagedFileExecutionInputV1::Read {
                offset,
                limit,
                max_bytes,
            },
        ) => {
            let (file, _ancestors) = open_plan_file(plan)?;
            streaming::read(file, context, offset, limit, max_bytes)
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
            let mut scan = traversal::WalkBudget::default();
            collect_plan_entries(
                plan,
                recursive,
                max_depth.min(MAX_DIRECTORY_SCAN_DEPTH),
                &mut entries,
                false,
                &mut scan,
                context,
            )?;
            entries.sort();
            let total = entries.len();
            let truncated = total > limit || scan.truncated;
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
                ..Default::default()
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
            let regex = regex::Regex::new(&pattern)
                .map_err(|error| ManagedFileAccessErrorV1::InvalidInput(error.to_string()))?;
            let mut stream = streaming::GrepStream::new(context, limit, max_bytes)?;
            collect_plan_grep(plan, &regex, &mut stream, context)?;
            stream.finish(context)
        }
        (ManagedFileOperationV1::Glob, ManagedFileExecutionInputV1::Glob { pattern, limit }) => {
            let matcher = globset::Glob::new(&pattern)
                .map_err(|error| {
                    ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
                })?
                .compile_matcher();
            let mut entries = Vec::new();
            let mut scan = traversal::WalkBudget::default();
            collect_plan_entries(
                plan,
                true,
                MAX_DIRECTORY_SCAN_DEPTH,
                &mut entries,
                true,
                &mut scan,
                context,
            )?;
            entries.retain(|entry| matcher.is_match(entry));
            entries.sort();
            let total = entries.len();
            let truncated = total > limit || scan.truncated;
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
                ..Default::default()
            })
        }
        (ManagedFileOperationV1::Write, ManagedFileExecutionInputV1::Write { content }) => {
            let recorder = mutation_recorder.ok_or_else(|| {
                ManagedFileAccessErrorV1::PhysicalExecutionFailed(
                    "mutation recorder is required for workspace writes".to_owned(),
                )
            })?;
            streaming::check_context(context)?;
            sigil_kernel::write_file_with_mutation_expected_in_batch(
                Some(recorder),
                &plan.root,
                &context.tool_call_id,
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
                ..Default::default()
            })
        }
        (
            ManagedFileOperationV1::Edit,
            ManagedFileExecutionInputV1::Edit { old_text, new_text },
        ) => {
            let (file, _ancestors) = open_plan_file(plan)?;
            let mut file = file;
            if !file.metadata().map_err(relative_io_error)?.is_file() {
                return Err(ManagedFileAccessErrorV1::NotRegularFile);
            }
            let (current, truncated) = read_text_with_budget(&mut file, MAX_READ_SCAN_BYTES)?;
            // The mutation coordinator revalidates the snapshot before replacing the leaf.
            // Release this read handle so Windows can perform the admitted atomic replacement.
            drop(file);
            if truncated {
                return Err(ManagedFileAccessErrorV1::ResourceLimit(
                    "edit file exceeds the content limit".to_owned(),
                ));
            }
            if old_text.is_empty() {
                return Err(ManagedFileAccessErrorV1::InvalidInput(
                    "old_text must not be empty".to_owned(),
                ));
            }
            let matches = current.match_indices(&old_text).take(2).count();
            if matches != 1 {
                return Err(ManagedFileAccessErrorV1::InvalidInput(
                    if matches == 0 {
                        "old_text not found"
                    } else {
                        "old_text is ambiguous; include more surrounding context"
                    }
                    .to_owned(),
                ));
            }
            streaming::check_context(context)?;
            let updated = current.replacen(&old_text, &new_text, 1);
            let recorder = mutation_recorder.ok_or_else(|| {
                ManagedFileAccessErrorV1::PhysicalExecutionFailed(
                    "mutation recorder is required for workspace edits".to_owned(),
                )
            })?;
            streaming::check_context(context)?;
            sigil_kernel::write_file_with_mutation_expected_in_batch(
                Some(recorder),
                &plan.root,
                &context.tool_call_id,
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
                ..Default::default()
            })
        }
        (ManagedFileOperationV1::Delete, ManagedFileExecutionInputV1::Delete) => {
            let recorder = mutation_recorder.ok_or_else(|| {
                ManagedFileAccessErrorV1::PhysicalExecutionFailed(
                    "mutation recorder is required for workspace deletes".to_owned(),
                )
            })?;
            if plan.expected_physical_identity.is_none() {
                return Err(ManagedFileAccessErrorV1::NotFound);
            }
            let (file, _ancestors) = open_plan_file(plan)?;
            let metadata = file.metadata().map_err(relative_io_error)?;
            if !metadata.is_file() {
                return Err(ManagedFileAccessErrorV1::NotRegularFile);
            }
            drop(file);
            streaming::check_context(context)?;
            sigil_kernel::delete_file_with_mutation_expected_in_batch(
                Some(recorder),
                &plan.root,
                &context.tool_call_id,
                None,
                &plan.logical_path,
                &plan.physical_path,
                plan.expected_content_digest
                    .map(|digest| format!("sha256:{}", digest.to_hex())),
            )
            .map_err(|error| mutation_error(error, plan))?;
            Ok(PhysicalExecutionOutcomeV1 {
                payload: "managed file delete applied".to_owned(),
                observed_bytes: metadata.len(),
                returned_entries: 0,
                total_entries: 0,
                returned_lines: 0,
                total_lines: 0,
                truncated: false,
                ..Default::default()
            })
        }
        _ => Err(ManagedFileAccessErrorV1::AdmissionMismatch),
    }
}

#[cfg(unix)]
fn directory_entry_names(
    directory: &std::fs::File,
    max_entries: usize,
    context: &ManagedFileExecutionContextV1,
) -> Result<Vec<String>, ManagedFileAccessErrorV1> {
    // A fresh open description prevents one traversal from consuming another traversal's
    // directory cursor; dup/try_clone would share that cursor with the authority root.
    use std::os::fd::IntoRawFd;
    let duplicate = open_at(directory, ".", libc::O_RDONLY | libc::O_DIRECTORY, 0)
        .map_err(relative_io_error)?
        .into_raw_fd();
    // SAFETY: `duplicate` is a valid fd and ownership transfers to DIR on success.
    let stream = unsafe { libc::fdopendir(duplicate) };
    if stream.is_null() {
        // SAFETY: fdopendir did not take ownership on failure.
        unsafe { libc::close(duplicate) };
        return Err(relative_io_error(std::io::Error::last_os_error()));
    }
    struct DirectoryStream(*mut libc::DIR);
    impl Drop for DirectoryStream {
        fn drop(&mut self) {
            // SAFETY: this guard exclusively owns the stream returned by fdopendir.
            unsafe { libc::closedir(self.0) };
        }
    }
    let stream = DirectoryStream(stream);
    let mut names = Vec::new();
    loop {
        streaming::check_context(context)?;
        // SAFETY: `stream` remains valid until closed below.
        let entry = unsafe { libc::readdir(stream.0) };
        if entry.is_null() {
            break;
        }
        // SAFETY: d_name is the NUL-terminated name returned by readdir.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }
            .to_string_lossy()
            .into_owned();
        if name != "." && name != ".." {
            names.push(name);
            if names.len() >= max_entries {
                break;
            }
        }
    }
    Ok(names)
}

fn read_text_with_budget(
    file: &mut std::fs::File,
    max_bytes: u64,
) -> Result<(String, bool), ManagedFileAccessErrorV1> {
    let mut bytes = Vec::new();
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(relative_io_error)?;
    let truncated = bytes.len() as u64 > max_bytes;
    if truncated {
        bytes.truncate(max_bytes.try_into().unwrap_or(usize::MAX));
        if let Err(error) = std::str::from_utf8(&bytes) {
            if error.error_len().is_some() {
                return Err(ManagedFileAccessErrorV1::InvalidInput(
                    "file is not UTF-8 text".to_owned(),
                ));
            }
            bytes.truncate(error.valid_up_to());
        }
    }
    let text = String::from_utf8(bytes)
        .map_err(|_| ManagedFileAccessErrorV1::InvalidInput("file is not UTF-8 text".to_owned()))?;
    Ok((text, truncated))
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
    /// Observes type and identity without opening the file's contents for reading.
    Metadata,
    /// Opens either a regular file or a directory without asserting a leaf type.
    Any,
    Read,
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
        WindowsOpenKind::Metadata => {
            options.access_mode(windows_sys::Win32::Storage::FileSystem::FILE_READ_ATTRIBUTES);
        }
        WindowsOpenKind::Any | WindowsOpenKind::Read | WindowsOpenKind::Directory => {
            options.read(true);
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
        GENERIC_READ, RtlNtStatusToDosError, STATUS_SUCCESS, SetLastError, UNICODE_STRING,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_NORMAL, FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, SYNCHRONIZE,
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
        WindowsOpenKind::Metadata => 0,
        WindowsOpenKind::Any | WindowsOpenKind::Read | WindowsOpenKind::Directory => GENERIC_READ,
    } | SYNCHRONIZE
        | FILE_READ_ATTRIBUTES;
    let create_options = FILE_SYNCHRONOUS_IO_NONALERT
        | FILE_OPEN_REPARSE_POINT
        | match kind {
            WindowsOpenKind::Metadata | WindowsOpenKind::Any | WindowsOpenKind::Directory => {
                FILE_OPEN_FOR_BACKUP_INTENT
            }
            WindowsOpenKind::Read => FILE_NON_DIRECTORY_FILE,
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

#[cfg(test)]
#[path = "tests/file_access_tests.rs"]
mod tests;
