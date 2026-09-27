//! Authority-owned managed storage admission and quota service.
//!
//! Resource namespace admission is intentionally current-state only. The authority keeps the
//! current grant table, live leases and an independent quota snapshot; it does not replay or
//! migrate a historical resource ledger. A physical writer may publish a bounded marker and
//! `records.jsonl`, but those are checked as current namespace facts, never as an authority log.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use fs2::FileExt;

use sigil_kernel::managed_storage::{
    ManagedStorageAdmissionRequestV1, ManagedStorageErrorV1,
    ManagedStorageExistingNamespaceBindingV1, ManagedStorageNamespaceHandleV1,
    ManagedStorageServiceV1, ManagedStorageStorageReceiptV1, StorageAdmissionGrantV1,
    ValidatedStorageAdmissionCapabilityV1,
};
use sigil_kernel::resource::{
    AuthorityGeneration, CanonicalHash, ManagedStorageSemanticOwnerV1,
    OpaqueKernelCapabilityAuthenticatorV1, OpaqueKernelCapabilityHandleId, OpaqueStorageGrantId,
    OpaqueStorageKeyIdV1,
};

use crate::quota::{QuotaBookV1, QuotaErrorV1};

#[path = "control_log_recovery.rs"]
mod control_log_recovery;

/// Authority-private grant table. A grant is registered for the current authority generation;
/// a lease is live only while it is present in `admitted_namespaces`.
#[derive(Debug, Default)]
pub struct AuthorityStorageGrantTableV1 {
    grants: BTreeMap<String, StorageAdmissionGrantV1>,
    admitted_namespaces: Mutex<BTreeMap<String, StorageAdmissionRecordV1>>,
    probe_sequence: std::sync::atomic::AtomicU64,
}

impl AuthorityStorageGrantTableV1 {
    pub const fn new() -> Self {
        Self {
            grants: BTreeMap::new(),
            admitted_namespaces: Mutex::new(BTreeMap::new()),
            probe_sequence: std::sync::atomic::AtomicU64::new(1),
        }
    }

    fn next_probe_sequence(&self) -> u64 {
        self.probe_sequence
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    }

    /// Registers a current closed grant. Duplicate grant ids are rejected.
    pub fn register(
        &mut self,
        grant: StorageAdmissionGrantV1,
    ) -> Result<(), ManagedStorageErrorV1> {
        let key = grant.grant_id.as_str().to_owned();
        if self.grants.contains_key(&key) {
            return Err(ManagedStorageErrorV1::CapabilityMismatch);
        }
        self.grants.insert(key, grant);
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct StorageAdmissionRecordV1 {
    handle_id: String,
    grant: StorageAdmissionGrantV1,
    request: ManagedStorageAdmissionRequestV1,
    namespace_hash: CanonicalHash,
    // Cloned records retain the active claim while an authority operation still uses them.
    _owner_claim: Arc<StorageOwnerClaimV1>,
}

#[derive(Debug)]
struct StorageOwnerClaimV1 {
    owner_key: String,
    active_owners: Arc<Mutex<BTreeSet<String>>>,
}

impl Drop for StorageOwnerClaimV1 {
    fn drop(&mut self) {
        if let Ok(mut owners) = self.active_owners.lock() {
            owners.remove(&self.owner_key);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
struct PhysicalStorageFrontierV1 {
    byte_length: u64,
    record_count: u64,
    content_hash: CanonicalHash,
}

/// Authority-owned managed storage service behind the kernel port.
pub struct AuthorityManagedStorageServiceV1 {
    table: AuthorityStorageGrantTableV1,
    authority_generation: AuthorityGeneration,
    quota: Arc<Mutex<QuotaBookV1>>,
    active_owners: Arc<Mutex<BTreeSet<String>>>,
    state_root: Option<PathBuf>,
    _process_state: Option<Arc<StorageProcessStateV1>>,
}

struct StorageProcessStateV1 {
    _process_lock: File,
    quota: Arc<Mutex<QuotaBookV1>>,
    active_owners: Arc<Mutex<BTreeSet<String>>>,
}

static STORAGE_PROCESS_STATES: OnceLock<Mutex<BTreeMap<PathBuf, Weak<StorageProcessStateV1>>>> =
    OnceLock::new();

impl AuthorityManagedStorageServiceV1 {
    /// In-memory service used by isolated unit tests and small adapters.
    pub fn new(
        table: AuthorityStorageGrantTableV1,
        authority_generation: AuthorityGeneration,
    ) -> Self {
        let quota_cap = quota_workspace_cap(&table);
        Self {
            table,
            authority_generation,
            quota: Arc::new(Mutex::new(QuotaBookV1::new(quota_cap))),
            active_owners: Arc::new(Mutex::new(BTreeSet::new())),
            state_root: None,
            _process_state: None,
        }
    }

    /// Production service. The only durable storage retained here is the independent quota
    /// snapshot; resource admission and settlement are rebuilt from the current grant table.
    pub fn new_with_state_root(
        table: AuthorityStorageGrantTableV1,
        authority_generation: AuthorityGeneration,
        state_root: impl AsRef<Path>,
    ) -> Result<Self, QuotaErrorV1> {
        let quota_cap = quota_workspace_cap(&table);
        Self::new_with_state_root_and_workspace_cap(
            table,
            authority_generation,
            state_root,
            quota_cap,
            None,
        )
    }

    /// Opens current grants under an explicit, stable workspace policy. Grant profiles still
    /// constrain every reservation independently of this shared workspace ceiling. A cold
    /// reopen may authorize one exact upward cap migration; an active in-process book must
    /// already have the requested cap, so existing leases are never reset or replaced.
    pub fn new_with_state_root_and_workspace_cap(
        table: AuthorityStorageGrantTableV1,
        authority_generation: AuthorityGeneration,
        state_root: impl AsRef<Path>,
        workspace_cap: u64,
        authorized_previous_cap: Option<u64>,
    ) -> Result<Self, QuotaErrorV1> {
        let root = state_root.as_ref().to_path_buf();
        let quota_path = root.join(".authority-quota").join("managed-storage.json");
        let canonical_root = root.canonicalize().map_err(storage_quota_io_error)?;
        let states = STORAGE_PROCESS_STATES.get_or_init(|| Mutex::new(BTreeMap::new()));
        let mut states = states.lock().map_err(|_| {
            QuotaErrorV1::Journal("storage process registry is poisoned".to_owned())
        })?;
        let process_state = if let Some(process_state) =
            states.get(&canonical_root).and_then(Weak::upgrade)
        {
            let current_cap = process_state
                .quota
                .lock()
                .map_err(|_| QuotaErrorV1::Journal("storage quota book is poisoned".to_owned()))?
                .workspace_cap();
            if current_cap != workspace_cap {
                return Err(QuotaErrorV1::Journal(
                    "active managed storage workspace cap mismatch".to_owned(),
                ));
            }
            process_state
        } else {
            let process_lock = acquire_storage_process_lock(&root)?;
            let mut quota = match authorized_previous_cap {
                Some(previous_cap) => {
                    QuotaBookV1::open_with_previous_cap(quota_path, workspace_cap, previous_cap)?
                }
                None => QuotaBookV1::open(quota_path, workspace_cap)?,
            };
            quota.release_all_active()?;
            let process_state = Arc::new(StorageProcessStateV1 {
                _process_lock: process_lock,
                quota: Arc::new(Mutex::new(quota)),
                active_owners: Arc::new(Mutex::new(BTreeSet::new())),
            });
            states.insert(canonical_root, Arc::downgrade(&process_state));
            process_state
        };
        drop(states);
        Ok(Self {
            table,
            authority_generation,
            quota: Arc::clone(&process_state.quota),
            active_owners: Arc::clone(&process_state.active_owners),
            state_root: Some(root),
            _process_state: Some(process_state),
        })
    }

    pub fn grant_table(&self) -> &AuthorityStorageGrantTableV1 {
        &self.table
    }

    fn current_grant_for_request(
        &self,
        request: &ManagedStorageAdmissionRequestV1,
        probe: bool,
    ) -> Result<StorageAdmissionGrantV1, ManagedStorageErrorV1> {
        let mut matches = self.table.grants.values().filter(|grant| {
            grant.capability_family == request.capability_family
                && grant.semantic_owner == request.semantic_owner
                && grant.purpose == request.purpose
                && grant.authority_scope == request.authority_scope
                && (probe || grant.owner_scope == request.owner_scope)
                && grant.source_class == request.source.source_class()
        });
        let Some(grant) = matches.next().cloned() else {
            return Err(ManagedStorageErrorV1::FamilyMismatch);
        };
        if matches.next().is_some() {
            return Err(ManagedStorageErrorV1::FamilyMismatch);
        }
        validate_closed_admission_grant(&grant, request, self.authority_generation, probe)?;
        Ok(grant)
    }

    fn validate_current_record(
        &self,
        record: &StorageAdmissionRecordV1,
    ) -> Result<(), ManagedStorageErrorV1> {
        validate_closed_admission_grant(
            &record.grant,
            &record.request,
            self.authority_generation,
            record.handle_id.starts_with("handle-probe-storage-"),
        )?;
        if self.table.grants.get(record.grant.grant_id.as_str()) != Some(&record.grant) {
            return Err(ManagedStorageErrorV1::CapabilityMismatch);
        }
        Ok(())
    }

    fn reserve_quota(
        &self,
        grant: &StorageAdmissionGrantV1,
        namespace_hash: CanonicalHash,
    ) -> Result<Arc<StorageOwnerClaimV1>, ManagedStorageErrorV1> {
        let owner_key = storage_quota_owner_key(grant.grant_hash, namespace_hash);
        let mut active_owners = self
            .active_owners
            .lock()
            .map_err(|_| ManagedStorageErrorV1::AuthorityUnavailable)?;
        if active_owners.contains(&owner_key) {
            return Err(ManagedStorageErrorV1::DuplicateClaim);
        }
        let mut quota = self
            .quota
            .lock()
            .map_err(|_| ManagedStorageErrorV1::AuthorityUnavailable)?;
        quota.ensure_healthy().map_err(storage_quota_error)?;
        if quota.reservation_for_owner(&owner_key).is_none() {
            quota
                .reserve_owned(owner_key.clone(), &grant.quota_profile, 0, 1)
                .map_err(storage_quota_error)?;
        }
        // A detached holder may reuse its exact charge, but a live holder in another service
        // must never acquire the same namespace. The claim is process-wide like the quota book.
        active_owners.insert(owner_key.clone());
        Ok(Arc::new(StorageOwnerClaimV1 {
            owner_key,
            active_owners: Arc::clone(&self.active_owners),
        }))
    }

    fn reconcile_quota(
        &self,
        record: &StorageAdmissionRecordV1,
        bytes: u64,
        entries: u64,
    ) -> Result<(), ManagedStorageErrorV1> {
        self.quota
            .lock()
            .map_err(|_| ManagedStorageErrorV1::AuthorityUnavailable)?
            .reconcile_owned(
                storage_quota_owner_key(record.grant.grant_hash, record.namespace_hash),
                &record.grant.quota_profile,
                bytes,
                entries,
            )
            .map(|_| ())
            .map_err(storage_quota_error)
    }

    fn release_quota(
        &self,
        record: &StorageAdmissionRecordV1,
    ) -> Result<(), ManagedStorageErrorV1> {
        self.quota
            .lock()
            .map_err(|_| ManagedStorageErrorV1::AuthorityUnavailable)?
            .release_owner(&storage_quota_owner_key(
                record.grant.grant_hash,
                record.namespace_hash,
            ))
            .map_err(storage_quota_error)
    }

    fn admit(
        &self,
        request: ManagedStorageAdmissionRequestV1,
        capability: ValidatedStorageAdmissionCapabilityV1,
        existing: Option<ManagedStorageExistingNamespaceBindingV1>,
    ) -> Result<ManagedStorageNamespaceHandleV1, ManagedStorageErrorV1> {
        let probe = capability.binding().is_none();
        let grant = self.current_grant_for_request(&request, probe)?;
        let namespace_hash = if let Some(binding) = &existing {
            if binding.original_namespace_hash != request.namespace_key_hash {
                return Err(ManagedStorageErrorV1::CapabilityMismatch);
            }
            binding.original_namespace_hash
        } else if probe && !hash_is_nonzero(request.namespace_key_hash) {
            hash_canonical(&("startup-probe", self.table.next_probe_sequence(), &request))
        } else {
            request.namespace_key_hash
        };
        if !hash_is_nonzero(namespace_hash) {
            return Err(ManagedStorageErrorV1::CapabilityMismatch);
        }
        if let Some(binding) = &existing {
            self.validate_existing_namespace_marker(grant.semantic_owner, namespace_hash, binding)?;
        }
        if let Some(binding) = capability.binding()
            && (binding.family() != request.capability_family
                || binding.namespace_hash() != namespace_hash)
        {
            return Err(ManagedStorageErrorV1::CapabilityMismatch);
        }

        let handle_id = if probe {
            format!("handle-probe-storage-{}", self.table.next_probe_sequence())
        } else {
            capability.handle_id.as_str().to_owned()
        };
        let mut admitted = self
            .table
            .admitted_namespaces
            .lock()
            .map_err(|_| ManagedStorageErrorV1::AuthorityUnavailable)?;
        if admitted.contains_key(&handle_id)
            || admitted.values().any(|current| {
                current.grant.grant_hash == grant.grant_hash
                    && current.namespace_hash == namespace_hash
            })
        {
            return Err(ManagedStorageErrorV1::DuplicateClaim);
        }
        let record = StorageAdmissionRecordV1 {
            handle_id: handle_id.clone(),
            grant: grant.clone(),
            request,
            namespace_hash,
            _owner_claim: self.reserve_quota(&grant, namespace_hash)?,
        };
        admitted.insert(handle_id.clone(), record);
        Ok(ManagedStorageNamespaceHandleV1::new(
            OpaqueKernelCapabilityHandleId::new(handle_id),
            namespace_hash,
            grant.capability_family,
            OpaqueKernelCapabilityAuthenticatorV1::new(format!(
                "auth-storage-{}",
                namespace_hash.to_hex()
            )),
        ))
    }

    fn record_for_handle(
        &self,
        handle: &ManagedStorageNamespaceHandleV1,
    ) -> Result<StorageAdmissionRecordV1, ManagedStorageErrorV1> {
        let admitted = self
            .table
            .admitted_namespaces
            .lock()
            .map_err(|_| ManagedStorageErrorV1::AuthorityUnavailable)?;
        let record = admitted
            .get(handle.handle_id.as_str())
            .cloned()
            .ok_or(ManagedStorageErrorV1::HandleFinalized)?;
        if record.namespace_hash != handle.namespace_hash
            || record.grant.capability_family != handle.capability_family
        {
            return Err(ManagedStorageErrorV1::CapabilityMismatch);
        }
        drop(admitted);
        self.validate_current_record(&record)?;
        Ok(record)
    }

    fn physical_namespace_directory(
        &self,
        record: &StorageAdmissionRecordV1,
    ) -> Result<PathBuf, ManagedStorageErrorV1> {
        self.physical_namespace_directory_for(record.grant.semantic_owner, record.namespace_hash)
    }

    fn physical_namespace_directory_for(
        &self,
        owner: ManagedStorageSemanticOwnerV1,
        namespace_hash: CanonicalHash,
    ) -> Result<PathBuf, ManagedStorageErrorV1> {
        let root = self
            .state_root
            .as_ref()
            .ok_or(ManagedStorageErrorV1::AuthorityUnavailable)?
            .canonicalize()
            .map_err(|_| ManagedStorageErrorV1::AuthorityUnavailable)?;
        let leaf = storage_owner_leaf(owner).ok_or(ManagedStorageErrorV1::AuthorityUnavailable)?;
        // The closed channel's ordinary namespace owns the channel leaf directly. Named
        // namespaces (session/object keys) use the stable hash sub-leaf selected by the
        // runtime adapter. Resolve both forms from the current grant table; no historical
        // resource journal is consulted.
        let is_default_namespace =
            self.table.grants.values().any(|grant| {
                grant.semantic_owner == owner && grant.namespace_hash == namespace_hash
            });
        let mut directory = root.join("managed").join(leaf);
        if !is_default_namespace {
            directory = directory.join(namespace_hash.to_hex());
        }
        reject_reparse_components(&directory, true)
            .map_err(|_| ManagedStorageErrorV1::AuthorityUnavailable)?;
        Ok(directory)
    }

    fn validate_existing_namespace_marker(
        &self,
        owner: ManagedStorageSemanticOwnerV1,
        namespace_hash: CanonicalHash,
        binding: &ManagedStorageExistingNamespaceBindingV1,
    ) -> Result<(), ManagedStorageErrorV1> {
        if self.state_root.is_none() {
            return Ok(());
        }
        let directory = self.physical_namespace_directory_for(owner, namespace_hash)?;
        let marker_path = directory.join("authority-admission.json");
        let metadata = fs::symlink_metadata(&marker_path)
            .map_err(|_| ManagedStorageErrorV1::AuthorityUnavailable)?;
        if !is_safe_physical_metadata(&metadata) || !metadata.is_file() {
            return Err(ManagedStorageErrorV1::AuthorityUnavailable);
        }
        #[derive(serde::Deserialize)]
        struct CurrentAdmissionMarkerV1 {
            schema_version: u32,
            handle_id: String,
            namespace_hash: CanonicalHash,
        }
        let marker: CurrentAdmissionMarkerV1 = serde_json::from_slice(
            &read_no_follow_file(&marker_path)
                .map_err(|_| ManagedStorageErrorV1::AuthorityUnavailable)?,
        )
        .map_err(|_| ManagedStorageErrorV1::AuthorityUnavailable)?;
        if marker.schema_version != 3
            || marker.handle_id.is_empty()
            || marker.handle_id != binding.original_handle_id.as_str()
            || marker.namespace_hash != namespace_hash
        {
            return Err(ManagedStorageErrorV1::CapabilityMismatch);
        }
        Ok(())
    }

    fn read_physical_frontier(
        &self,
        record: &StorageAdmissionRecordV1,
        directory: &Path,
    ) -> Result<PhysicalStorageFrontierV1, ManagedStorageErrorV1> {
        if record.grant.semantic_owner == ManagedStorageSemanticOwnerV1::ApplicationControlRecovery
        {
            return self.control_recovery_frontier(record, directory);
        }
        let path = directory.join(
            if record.grant.semantic_owner == ManagedStorageSemanticOwnerV1::ApplicationCommandIndex
            {
                "records.sqlite3"
            } else {
                "records.jsonl"
            },
        );
        let bytes = match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if !is_safe_physical_metadata(&metadata)
                    || !metadata.is_file()
                    || metadata.len() > record.grant.quota_profile.max_bytes
                {
                    return Err(ManagedStorageErrorV1::AuthorityUnavailable);
                }
                if record.grant.semantic_owner
                    == ManagedStorageSemanticOwnerV1::ApplicationControlLog
                {
                    let file = open_no_follow_file(&path)
                        .map_err(|_| ManagedStorageErrorV1::AuthorityUnavailable)?;
                    let (byte_length, record_count, content_hash) =
                        sigil_kernel::managed_storage::read_jsonl_physical_frontier(file)
                            .map_err(|_| ManagedStorageErrorV1::AuthorityUnavailable)?;
                    return Ok(PhysicalStorageFrontierV1 {
                        byte_length,
                        record_count,
                        content_hash,
                    });
                }
                if record.grant.semantic_owner
                    == ManagedStorageSemanticOwnerV1::ApplicationCommandIndex
                {
                    let file = open_no_follow_file(&path)
                        .map_err(|_| ManagedStorageErrorV1::AuthorityUnavailable)?;
                    let (byte_length, content_hash) =
                        sigil_kernel::managed_storage::read_physical_digest(file)
                            .map_err(|_| ManagedStorageErrorV1::AuthorityUnavailable)?;
                    return Ok(PhysicalStorageFrontierV1 {
                        byte_length,
                        record_count: u64::from(byte_length != 0),
                        content_hash,
                    });
                }
                read_no_follow_file(&path)
                    .map_err(|_| ManagedStorageErrorV1::AuthorityUnavailable)?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(_) => return Err(ManagedStorageErrorV1::AuthorityUnavailable),
        };
        let record_count = physical_record_count(record.grant.semantic_owner, &bytes)?;
        Ok(PhysicalStorageFrontierV1 {
            byte_length: bytes.len() as u64,
            record_count,
            content_hash: hash_bytes(&bytes),
        })
    }

    fn finish_session_settlement_attempt(
        &self,
        record: &StorageAdmissionRecordV1,
        result: Result<ManagedStorageStorageReceiptV1, ManagedStorageErrorV1>,
    ) -> Result<ManagedStorageStorageReceiptV1, ManagedStorageErrorV1> {
        if result.is_err()
            && record.grant.semantic_owner == ManagedStorageSemanticOwnerV1::SessionLog
        {
            // Finalization consumes the non-clone handle even when physical observation or
            // quota settlement fails. Release only that authenticated live holder so the
            // existing namespace can be recovered; keep its charge/poison and original error.
            self.table
                .admitted_namespaces
                .lock()
                .map_err(|_| ManagedStorageErrorV1::AuthorityUnavailable)?
                .remove(&record.handle_id);
        }
        result
    }

    fn finalize_record(
        &self,
        handle: ManagedStorageNamespaceHandleV1,
        record: StorageAdmissionRecordV1,
        reason: String,
        frontier: Option<PhysicalStorageFrontierV1>,
    ) -> Result<ManagedStorageStorageReceiptV1, ManagedStorageErrorV1> {
        let (bytes, entries, physical_hash) = frontier
            .map(|frontier| {
                (
                    frontier.byte_length,
                    frontier.record_count,
                    Some(hash_canonical(&(
                        "managed-storage-physical-observation-v1",
                        record.namespace_hash,
                        frontier,
                    ))),
                )
            })
            .unwrap_or((0, 0, None));
        if frontier.is_some() {
            self.reconcile_quota(&record, bytes, entries)?;
        }
        let operation_digest = hash_canonical(&(
            "managed-storage-settlement-v1",
            &record.request,
            &reason,
            bytes,
            entries,
            physical_hash,
        ));
        self.release_quota(&record)?;
        self.table
            .admitted_namespaces
            .lock()
            .map_err(|_| ManagedStorageErrorV1::AuthorityUnavailable)?
            .remove(handle.handle_id.as_str());
        let committed_frontier_hash = hash_canonical(&(
            "managed-storage-frontier-v1",
            record.namespace_hash,
            operation_digest,
        ));
        Ok(ManagedStorageStorageReceiptV1 {
            grant_id: record.grant.grant_id,
            grant_hash: record.grant.grant_hash,
            semantic_owner: record.grant.semantic_owner,
            capability_family: record.grant.capability_family,
            resource_id: record.grant.resource_ref.resource_id,
            operation_digest,
            committed_entry_count: Some(entries),
            committed_frontier_hash,
            receipt_hash: hash_canonical(&(
                "managed-storage-receipt-v1",
                record.namespace_hash,
                operation_digest,
                committed_frontier_hash,
            )),
            physical_frontier_hash: physical_hash,
        })
    }
}

impl ManagedStorageServiceV1 for AuthorityManagedStorageServiceV1 {
    fn admit_namespace(
        &self,
        request: ManagedStorageAdmissionRequestV1,
        capability: ValidatedStorageAdmissionCapabilityV1,
    ) -> Result<ManagedStorageNamespaceHandleV1, ManagedStorageErrorV1> {
        self.admit(request, capability, None)
    }

    fn admit_existing_namespace(
        &self,
        request: ManagedStorageAdmissionRequestV1,
        capability: ValidatedStorageAdmissionCapabilityV1,
        original: ManagedStorageExistingNamespaceBindingV1,
    ) -> Result<ManagedStorageNamespaceHandleV1, ManagedStorageErrorV1> {
        self.admit(request, capability, Some(original))
    }

    fn validate_namespace_write(
        &self,
        handle: &ManagedStorageNamespaceHandleV1,
    ) -> Result<(), ManagedStorageErrorV1> {
        let record = self.record_for_handle(handle)?;
        self.validate_control_log_fence(&record)?;
        // Even an existing capacity grant stops authorizing writes when another namespace's
        // uncertain persistence poisons the shared quota book. record_for_handle releases the
        // admission lock before taking this quota lock; the check performs no filesystem I/O.
        self.quota
            .lock()
            .map_err(|_| ManagedStorageErrorV1::AuthorityUnavailable)?
            .ensure_healthy()
            .map_err(storage_quota_error)
    }

    fn acquire_forward_guard(
        &self,
        handle: &ManagedStorageNamespaceHandleV1,
    ) -> Result<
        Box<dyn sigil_kernel::managed_storage::ManagedStorageForwardGuardV1>,
        ManagedStorageErrorV1,
    > {
        self.control_forward_guard(handle)
    }

    fn preview_control_log_recovery(
        &self,
        recovery: &ManagedStorageNamespaceHandleV1,
        old: &ManagedStorageNamespaceHandleV1,
        successor: &ManagedStorageNamespaceHandleV1,
        request: sigil_kernel::managed_storage::ControlLogRecoveryRequestV1,
    ) -> Result<sigil_kernel::managed_storage::ControlLogRecoveryPreviewV1, ManagedStorageErrorV1>
    {
        self.control_recovery_preview(recovery, old, successor, request)
    }

    fn advance_control_log_recovery(
        &self,
        recovery: &ManagedStorageNamespaceHandleV1,
        old: &ManagedStorageNamespaceHandleV1,
        successor: &ManagedStorageNamespaceHandleV1,
        preview: &sigil_kernel::managed_storage::ControlLogRecoveryPreviewV1,
        header: &[u8],
    ) -> Result<sigil_kernel::managed_storage::ControlLogRecoveryStateV1, ManagedStorageErrorV1>
    {
        self.control_recovery_advance(recovery, old, successor, preview, header)
    }

    fn query_control_log_recovery(
        &self,
        recovery: &ManagedStorageNamespaceHandleV1,
    ) -> Result<
        Option<sigil_kernel::managed_storage::ControlLogRecoveryStateV1>,
        ManagedStorageErrorV1,
    > {
        self.control_recovery_query(recovery)
    }

    fn reconcile_namespace_quota(
        &self,
        handle: &ManagedStorageNamespaceHandleV1,
        bytes: u64,
        entries: u64,
    ) -> Result<(), ManagedStorageErrorV1> {
        let record = self.record_for_handle(handle)?;
        self.reconcile_quota(&record, bytes, entries)
    }

    fn reserve_namespace_quota_capacity(
        &self,
        handle: &ManagedStorageNamespaceHandleV1,
        minimum_bytes: u64,
        preferred_bytes: u64,
        entries: u64,
    ) -> Result<u64, ManagedStorageErrorV1> {
        let record = self.record_for_handle(handle)?;
        self.quota
            .lock()
            .map_err(|_| ManagedStorageErrorV1::AuthorityUnavailable)?
            .reserve_owned_capacity(
                storage_quota_owner_key(record.grant.grant_hash, record.namespace_hash),
                &record.grant.quota_profile,
                minimum_bytes,
                preferred_bytes,
                entries,
            )
            .map(|reservation| reservation.reserved_bytes)
            .map_err(storage_quota_error)
    }

    fn detach_namespace(
        &self,
        handle: ManagedStorageNamespaceHandleV1,
    ) -> Result<(), ManagedStorageErrorV1> {
        self.record_for_handle(&handle)?;
        self.table
            .admitted_namespaces
            .lock()
            .map_err(|_| ManagedStorageErrorV1::AuthorityUnavailable)?
            .remove(handle.handle_id.as_str());
        Ok(())
    }

    fn finalize_namespace(
        &self,
        handle: ManagedStorageNamespaceHandleV1,
        reason: String,
    ) -> Result<ManagedStorageStorageReceiptV1, ManagedStorageErrorV1> {
        let record = self.record_for_handle(&handle)?;
        let result = (|| {
            if record.grant.semantic_owner
                == ManagedStorageSemanticOwnerV1::ApplicationControlRecovery
                && self.state_root.is_some()
            {
                let directory = self.physical_namespace_directory(&record)?;
                let _lock = open_physical_namespace_lock(&directory)?;
                let frontier = self.read_physical_frontier(&record, &directory)?;
                return self.finalize_record(handle, record.clone(), reason, Some(frontier));
            }
            self.finalize_record(handle, record.clone(), reason, None)
        })();
        self.finish_session_settlement_attempt(&record, result)
    }

    fn finalize_namespace_with_physical_frontier(
        &self,
        handle: ManagedStorageNamespaceHandleV1,
        byte_length: u64,
        record_count: u64,
        content_hash: CanonicalHash,
        reason: String,
    ) -> Result<ManagedStorageStorageReceiptV1, ManagedStorageErrorV1> {
        let record = self.record_for_handle(&handle)?;
        let result = (|| {
            if self.state_root.is_some() {
                let directory = self.physical_namespace_directory(&record)?;
                let _lock = open_physical_namespace_lock(&directory)?;
                let observed = self.read_physical_frontier(&record, &directory)?;
                if observed.byte_length != byte_length
                    || observed.record_count != record_count
                    || observed.content_hash != content_hash
                {
                    return Err(ManagedStorageErrorV1::AuthorityUnavailable);
                }
                return self.finalize_record(handle, record.clone(), reason, Some(observed));
            }
            let frontier = PhysicalStorageFrontierV1 {
                byte_length,
                record_count,
                content_hash,
            };
            self.finalize_record(handle, record.clone(), reason, Some(frontier))
        })();
        self.finish_session_settlement_attempt(&record, result)
    }
}

fn request_matches_grant(
    request: &ManagedStorageAdmissionRequestV1,
    grant: &StorageAdmissionGrantV1,
    probe: bool,
) -> bool {
    request.semantic_owner == grant.semantic_owner
        && request.capability_family == grant.capability_family
        && request.purpose == grant.purpose
        && request.authority_scope == grant.authority_scope
        && (probe || request.owner_scope == grant.owner_scope)
        && request.source.source_class() == grant.source_class
        && admission_source_binding_hash(&request.source) == grant.source_binding_hash
}

fn validate_closed_admission_grant(
    grant: &StorageAdmissionGrantV1,
    request: &ManagedStorageAdmissionRequestV1,
    authority_generation: AuthorityGeneration,
    probe: bool,
) -> Result<(), ManagedStorageErrorV1> {
    let ids_present = !grant.grant_id.as_str().is_empty()
        && !grant.resource_ref.resource_id.as_str().is_empty()
        && !grant.semantic_schema.as_str().is_empty();
    let hashes_present = [
        grant.admission_hash,
        grant.purpose_hash,
        grant.source_binding_hash,
        grant.namespace_hash,
        grant.authority_scope_hash,
        grant.resource_binding_digest,
        grant.physical_binding_hash,
        grant.quota_profile.profile_hash,
        grant.authority_generation.instance_hash,
        grant.grant_hash,
    ]
    .into_iter()
    .all(hash_is_nonzero);
    let request_namespace_hash_present = probe || hash_is_nonzero(request.namespace_key_hash);
    let shape_matches = grant.resource_ref.kind == grant.resource_kind
        && grant.resource_ref.owner_scope == grant.owner_scope
        && grant.resource_ref.authority_scope == grant.authority_scope
        && grant.resource_ref.generation != 0
        && grant.authority_generation.epoch != 0
        && grant.quota_profile.max_open_holders != 0
        && request_matches_grant(request, grant, probe);
    if !ids_present
        || !hashes_present
        || !request_namespace_hash_present
        || !shape_matches
        || grant.authority_generation != authority_generation
    {
        return Err(ManagedStorageErrorV1::CapabilityMismatch);
    }
    Ok(())
}

pub fn admission_source_binding_hash(
    source: &sigil_kernel::managed_storage::StorageAdmissionSourceV1,
) -> CanonicalHash {
    hash_canonical(source)
}

fn quota_workspace_cap(table: &AuthorityStorageGrantTableV1) -> u64 {
    table
        .grants
        .values()
        .map(|grant| grant.quota_profile.max_bytes)
        .fold(0u64, u64::saturating_add)
        .max(1)
}

fn storage_quota_owner_key(grant_hash: CanonicalHash, namespace_hash: CanonicalHash) -> String {
    format!(
        "storage:{}:{}",
        grant_hash.to_hex(),
        namespace_hash.to_hex()
    )
}

fn storage_quota_error(error: QuotaErrorV1) -> ManagedStorageErrorV1 {
    use sigil_kernel::managed_storage::ManagedStorageQuotaDimensionV1;
    let (dimension, requested, limit) = match error {
        QuotaErrorV1::ReservationExceeded { reserved, max, .. } => {
            (ManagedStorageQuotaDimensionV1::Bytes, reserved, max)
        }
        QuotaErrorV1::EntryExceeded { reserved, max, .. } => {
            (ManagedStorageQuotaDimensionV1::Entries, reserved, max)
        }
        QuotaErrorV1::WorkspaceOvercommit {
            used,
            incoming,
            cap,
        } => (
            ManagedStorageQuotaDimensionV1::WorkspaceBytes,
            used.saturating_add(incoming),
            cap,
        ),
        _ => return ManagedStorageErrorV1::AuthorityUnavailable,
    };
    ManagedStorageErrorV1::QuotaExceeded {
        dimension,
        requested,
        limit,
    }
}

fn acquire_storage_process_lock(root: &Path) -> Result<File, QuotaErrorV1> {
    let directory = root.join(".authority-quota");
    fs::create_dir_all(&directory).map_err(storage_quota_io_error)?;
    sigil_kernel::secure_private_path_permissions(&directory)
        .map_err(|error| QuotaErrorV1::Journal(error.to_string()))?;
    let path = directory.join("managed-storage.authority.lock");
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(&path).map_err(storage_quota_io_error)?;
    let metadata = fs::symlink_metadata(&path).map_err(storage_quota_io_error)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(QuotaErrorV1::Journal(
            "managed storage authority lock is not a regular file".to_owned(),
        ));
    }
    sigil_kernel::secure_private_path_permissions(&path)
        .map_err(|error| QuotaErrorV1::Journal(error.to_string()))?;
    file.try_lock_exclusive().map_err(|error| {
        QuotaErrorV1::Journal(format!(
            "managed storage authority is already active: {error}"
        ))
    })?;
    Ok(file)
}

fn storage_quota_io_error(error: std::io::Error) -> QuotaErrorV1 {
    QuotaErrorV1::Journal(error.to_string())
}

fn hash_canonical<T: serde::Serialize>(value: &T) -> CanonicalHash {
    let encoded = serde_json::to_vec(value).expect("current authority value is serializable");
    hash_bytes(&encoded)
}

fn hash_bytes(value: &[u8]) -> CanonicalHash {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(value);
    CanonicalHash::from_bytes(hasher.finalize().into())
}

fn storage_owner_leaf(owner: ManagedStorageSemanticOwnerV1) -> Option<&'static str> {
    use sigil_kernel::resource::{AdapterDurableStateClassV1, ManagedStorageSemanticOwnerV1};
    match owner {
        ManagedStorageSemanticOwnerV1::SessionLog => Some("session-log"),
        ManagedStorageSemanticOwnerV1::SessionLifecycleLog => Some("session-lifecycle-log"),
        ManagedStorageSemanticOwnerV1::InteractiveInputHistory => Some("input-history"),
        ManagedStorageSemanticOwnerV1::DurableMemory(_) => Some("durable-memory"),
        ManagedStorageSemanticOwnerV1::SessionCatalog => Some("session-catalog"),
        ManagedStorageSemanticOwnerV1::ArtifactStaging => Some("artifact-staging"),
        ManagedStorageSemanticOwnerV1::ArtifactStore => Some("artifact-store"),
        ManagedStorageSemanticOwnerV1::AdapterDurableState(class) => match class {
            AdapterDurableStateClassV1::ProtocolReplay => Some("adapter-protocol-replay"),
            AdapterDurableStateClassV1::EgressDisclosure => Some("adapter-egress-disclosure"),
            AdapterDurableStateClassV1::IdempotencyLedger => Some("adapter-idempotency-ledger"),
        },
        ManagedStorageSemanticOwnerV1::ApplicationControlLog => Some("application-control-log"),
        ManagedStorageSemanticOwnerV1::ApplicationCommandIndex => Some("application-command-index"),
        ManagedStorageSemanticOwnerV1::ApplicationControlRecovery => {
            Some("application-control-recovery")
        }
        ManagedStorageSemanticOwnerV1::WorkspaceMutationState
        | ManagedStorageSemanticOwnerV1::PlanStore
        | ManagedStorageSemanticOwnerV1::ProviderConnectionState
        | ManagedStorageSemanticOwnerV1::RuntimeCache(_) => None,
    }
}

fn physical_record_count(
    owner: ManagedStorageSemanticOwnerV1,
    bytes: &[u8],
) -> Result<u64, ManagedStorageErrorV1> {
    if matches!(owner, ManagedStorageSemanticOwnerV1::AdapterDurableState(_)) {
        if bytes.is_empty() {
            return Ok(0);
        }
        let value: serde_json::Value = serde_json::from_slice(bytes)
            .map_err(|_| ManagedStorageErrorV1::AuthorityUnavailable)?;
        return value
            .is_object()
            .then_some(1)
            .ok_or(ManagedStorageErrorV1::AuthorityUnavailable);
    }
    if !bytes.is_empty() && !bytes.ends_with(b"\n") {
        return Err(ManagedStorageErrorV1::AuthorityUnavailable);
    }
    Ok(bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .count() as u64)
}

fn open_physical_namespace_lock(directory: &Path) -> Result<File, ManagedStorageErrorV1> {
    let path = directory.join(".authority-storage.lock");
    reject_reparse_components(&path, false)
        .map_err(|_| ManagedStorageErrorV1::AuthorityUnavailable)?;
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options
        .open(&path)
        .map_err(|_| ManagedStorageErrorV1::AuthorityUnavailable)?;
    file.lock_exclusive()
        .map_err(|_| ManagedStorageErrorV1::AuthorityUnavailable)?;
    Ok(file)
}

fn is_safe_physical_metadata(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return false;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return false;
        }
    }
    true
}

fn reject_reparse_components(path: &Path, allow_missing_leaf: bool) -> std::io::Result<()> {
    let components = path.components().collect::<Vec<_>>();
    let last = components.len().saturating_sub(1);
    let mut current = PathBuf::new();
    for (index, component) in components.into_iter().enumerate() {
        current.push(component.as_os_str());
        let metadata = match fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    && allow_missing_leaf
                    && index == last =>
            {
                continue;
            }
            Err(error) => return Err(error),
        };
        if !is_safe_physical_metadata(&metadata) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "managed path contains a symlink or reparse point",
            ));
        }
    }
    Ok(())
}

fn read_no_follow_file(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut file = open_no_follow_file(path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn open_no_follow_file(path: &Path) -> std::io::Result<File> {
    reject_reparse_components(path, false)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if !is_safe_physical_metadata(&metadata) || !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "managed object is not a regular file",
        ));
    }
    Ok(file)
}

fn hash_is_nonzero(value: CanonicalHash) -> bool {
    value.as_bytes().iter().any(|byte| *byte != 0)
}

/// Authority-private storage capability verifier facet.
pub struct AuthorityStorageCapabilityActivationEvidenceVerifierV1;

impl Default for AuthorityStorageCapabilityActivationEvidenceVerifierV1 {
    fn default() -> Self {
        Self
    }
}

/// Authority-private logical key registry.
#[derive(Debug, Default)]
pub struct AuthorityLogicalKeyRegistryV1 {
    keys: BTreeMap<String, (OpaqueStorageKeyIdV1, String)>,
}

impl AuthorityLogicalKeyRegistryV1 {
    pub fn reserve(
        &mut self,
        key_id: OpaqueStorageKeyIdV1,
        kind: sigil_kernel::resource::StorageLogicalKeyKindV1,
    ) -> Result<(), ManagedStorageErrorV1> {
        let key = key_id.as_str().to_owned();
        let kind_label = match kind {
            sigil_kernel::resource::StorageLogicalKeyKindV1::Object => "object",
            sigil_kernel::resource::StorageLogicalKeyKindV1::Stream => "stream",
        };
        if self
            .keys
            .insert(key, (key_id, kind_label.to_owned()))
            .is_some()
        {
            return Err(ManagedStorageErrorV1::DuplicateClaim);
        }
        Ok(())
    }
}

pub fn sample_storage_receipt() -> ManagedStorageStorageReceiptV1 {
    ManagedStorageStorageReceiptV1 {
        grant_id: OpaqueStorageGrantId::new("grant-sample".to_owned()),
        grant_hash: CanonicalHash::from_bytes([9u8; 32]),
        semantic_owner: ManagedStorageSemanticOwnerV1::SessionLifecycleLog,
        capability_family: sigil_kernel::resource::ManagedStorageCapabilityFamilyV1::AppendLog,
        resource_id: sigil_kernel::resource::OpaqueResourceId::new("resource-sample".to_owned()),
        operation_digest: CanonicalHash::from_bytes([8u8; 32]),
        committed_entry_count: Some(7),
        committed_frontier_hash: CanonicalHash::from_bytes([7u8; 32]),
        receipt_hash: CanonicalHash::from_bytes([6u8; 32]),
        physical_frontier_hash: None,
    }
}

#[cfg(test)]
#[path = "tests/storage_tests.rs"]
mod tests;
