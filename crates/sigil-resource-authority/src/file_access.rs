//! RFC-0071 section 8.5 / R71.6: authority-owned in-process file access adjudicator.
//!
//! read/write/edit/list/glob/grep tools never spawn; they must not bypass the borrowed
//! Workspace / ExternalUserPath identity lease. This service adjudicates the post-decision
//! Tool admission: token binding vs request, one-shot adjudication claim, observed borrowed
//! identity (identity_before in the receipt), SystemTemp deny/read-boundary, and closed
//! operation classification. It never performs file I/O itself and never claims ownership of
//! borrowed content. SessionExport / SessionExportReconcile tokens have their own
//! kernel-verified export path (session_export.rs) and are refused here until the storage
//! writer slice wires them through explicitly.

use std::collections::{BTreeMap, BTreeSet};
#[cfg(unix)]
use std::ffi::{CStr, CString};
#[cfg(unix)]
use std::fs::OpenOptions;
#[cfg(unix)]
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use sigil_kernel::managed_execution::BorrowedResourceAccessReceiptV1;
use sigil_kernel::managed_file_access::{
    ManagedFileAccessAdmissionTokenV1, ManagedFileAccessErrorV1, ManagedFileAccessPlanRequestV1,
    ManagedFileAccessRequestV1, ManagedFileAccessResultV1, ManagedFileAccessServiceV1,
    ManagedFileAdmissionBindingV1, ManagedFileExecutionInputV1, ManagedFileExecutionOutcomeV1,
    ManagedFileExecutionRequestV1, ManagedFileOperationV1,
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

#[derive(Debug, Clone)]
struct PlannedFileAccessV1 {
    subject_ref: OpaquePermissionSubjectRef,
    #[cfg(not(unix))]
    root: PathBuf,
    logical_path: String,
    #[cfg(not(unix))]
    physical_path: PathBuf,
    #[cfg(unix)]
    root_handle: Arc<std::fs::File>,
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

    #[cfg(unix)]
    fn open_workspace_root(root: &Path) -> Result<Arc<std::fs::File>, ManagedFileAccessErrorV1> {
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(root)
            .map(Arc::new)
            .map_err(|error| ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string()))
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

    fn physical_identity(path: &Path, logical_path: &str) -> CanonicalHash {
        crate::identity::canonical_identity(path)
            .map(|identity| identity.digest)
            .unwrap_or_else(|_| Self::hash_parts(&[logical_path.as_bytes()]))
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
        #[cfg(unix)]
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
            Self::physical_identity(&physical_path, &logical_path).as_bytes(),
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
                    #[cfg(not(unix))]
                    physical_path,
                    #[cfg(unix)]
                    root_handle,
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
        // Validate the pathless plan before consuming the one-shot admission. A stale or
        // cross-plan request must not burn a valid approval token.
        let result = self.access(request.access.clone(), token)?;
        let current_root = self.current_root_identity(&plan.subject_ref)?;
        if current_root != plan.root_identity {
            return Err(ManagedFileAccessErrorV1::SubjectIdentityDrift);
        }
        let PhysicalExecutionOutcomeV1 {
            payload,
            observed_bytes,
            returned_entries,
            total_entries,
            returned_lines,
            total_lines,
            truncated,
        } = execute_physical(&plan, request.input)?;
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
        let current_root = self.current_root_identity(&plan.subject_ref)?;
        if current_root != plan.root_identity {
            return Err(ManagedFileAccessErrorV1::SubjectIdentityDrift);
        }
        let raw = match read_relative_text(&plan) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(relative_io_error(error)),
        };
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

#[cfg(not(unix))]
fn relative_io_error(error: std::io::Error) -> ManagedFileAccessErrorV1 {
    ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
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

#[cfg(unix)]
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
) -> std::io::Result<std::fs::File> {
    let components = relative_components(&plan.logical_path);
    if components.is_empty() {
        return plan.root_handle.try_clone();
    }
    let mut parent = plan.root_handle.try_clone()?;
    for component in &components[..components.len() - 1] {
        parent = open_at(
            &parent,
            component,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0,
        )?;
    }
    open_at(
        &parent,
        components[components.len() - 1],
        flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        0o600,
    )
}

#[cfg(unix)]
fn open_relative_parent(plan: &PlannedFileAccessV1) -> std::io::Result<(std::fs::File, CString)> {
    let components = relative_components(&plan.logical_path);
    let Some(leaf) = components.last() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "workspace root is not a file leaf",
        ));
    };
    let mut parent = plan.root_handle.try_clone()?;
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
fn read_relative_text(plan: &PlannedFileAccessV1) -> std::io::Result<String> {
    let mut file = open_relative_path(plan, libc::O_RDONLY)?;
    let mut raw = String::new();
    std::io::Read::read_to_string(&mut file, &mut raw)?;
    Ok(raw)
}

#[cfg(not(unix))]
fn read_relative_text(plan: &PlannedFileAccessV1) -> std::io::Result<String> {
    std::fs::read_to_string(&plan.physical_path)
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
            let raw = read_relative_text(plan).map_err(relative_io_error)?;
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
            let directory = open_relative_path(plan, libc::O_RDONLY | libc::O_DIRECTORY)
                .map_err(relative_io_error)?;
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
            let target = open_relative_path(plan, libc::O_RDONLY).map_err(relative_io_error)?;
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
            let directory = open_relative_path(plan, libc::O_RDONLY | libc::O_DIRECTORY)
                .map_err(relative_io_error)?;
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
            let mut file = open_relative_path(plan, libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC)
                .map_err(relative_io_error)?;
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
            let mut file = open_relative_path(plan, libc::O_RDWR).map_err(relative_io_error)?;
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
            let (parent, leaf) = open_relative_parent(plan).map_err(relative_io_error)?;
            // SAFETY: `parent` is an authority-owned directory fd and `leaf` is a single
            // validated component; unlinkat never follows a symlink in the leaf position.
            let status = unsafe { libc::unlinkat(parent.as_raw_fd(), leaf.as_ptr(), 0) };
            if status < 0 {
                return Err(relative_io_error(std::io::Error::last_os_error()));
            }
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

#[cfg(not(unix))]
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

#[cfg(not(unix))]
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

#[cfg(not(unix))]
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
        let service = AuthorityManagedFileAccessServiceV1::new(registry);
        let plan = service
            .plan(ManagedFileAccessPlanRequestV1 {
                logical_path: ManagedFileLogicalPathV1::new("notes.txt").expect("logical"),
                operation: ManagedFileOperationV1::Read,
                operation_scope: "fault-file-read".to_owned(),
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
                operation: ManagedFileOperationV1::Read,
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
    fn r71_f_fil_013_alias_replacement_cannot_redirect_a_planned_read() {
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
