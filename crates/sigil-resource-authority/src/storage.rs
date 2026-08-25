//! RFC-0071 section 8.6: authority-owned managed storage implementation.
//!
//! This is RA-internal: it holds the private grant table, logical-key registry and one-shot
//! claims. The factory returns only kernel pathsless trait objects; semantic writers never
//! import authority concrete types nor receive an authority token.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use fs2::FileExt;
use serde::Deserialize;

use sigil_kernel::managed_storage::{
    ManagedStorageAdmissionRequestV1, ManagedStorageErrorV1, ManagedStorageNamespaceHandleV1,
    ManagedStorageServiceV1, ManagedStorageStorageReceiptV1, StorageAdmissionGrantV1,
    ValidatedStorageAdmissionCapabilityV1,
};
use sigil_kernel::resource::{
    AuthorityGeneration, CanonicalHash, OpaqueKernelCapabilityAuthenticatorV1,
    OpaqueKernelCapabilityHandleId, OpaqueStorageGrantId, OpaqueStorageKeyIdV1,
};

use crate::journal::{
    JournalErrorV1, ResourceJournalEventV1, ResourceJournalFileV1, ResourceJournalRecordV1,
    ResourceJournalStorageAdmissionV1,
};
use crate::quota::{QuotaBookV1, QuotaErrorV1};

/// Authority-private grant table: grant id -> durable admission grant + claim state.
#[derive(Debug, Default)]
pub struct AuthorityStorageGrantTableV1 {
    grants: BTreeMap<String, StorageAdmissionGrantV1>,
    #[allow(dead_code)]
    consumed_capabilities: BTreeMap<String, ()>,
    /// Finalized namespace registry: one-shot per namespace (interior mutability because
    /// finalize is &self on the kernel port).
    finalized_namespaces: std::sync::Mutex<BTreeMap<String, ()>>,
    /// Exact admitted request and grant, retained until the one-shot finalize CAS.
    admitted_namespaces: std::sync::Mutex<BTreeMap<String, StorageAdmissionRecordV1>>,
    /// Probe-claim sequence: every kernel-owned startup-probe claim gets a distinct probe
    /// namespace so probes and shadow writer claims never share a finalized namespace.
    probe_sequence: std::sync::atomic::AtomicU64,
}

impl AuthorityStorageGrantTableV1 {
    pub const fn new() -> Self {
        Self {
            grants: BTreeMap::new(),
            consumed_capabilities: BTreeMap::new(),
            finalized_namespaces: std::sync::Mutex::new(BTreeMap::new()),
            admitted_namespaces: std::sync::Mutex::new(BTreeMap::new()),
            probe_sequence: std::sync::atomic::AtomicU64::new(1),
        }
    }

    /// Next distinct probe namespace sequence (descendant proof: probes never share ns).
    fn next_probe_sequence(&self) -> u64 {
        self.probe_sequence
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    }

    fn advance_probe_sequence(&self, next: u64) {
        let mut current = self
            .probe_sequence
            .load(std::sync::atomic::Ordering::SeqCst);
        while current < next {
            match self.probe_sequence.compare_exchange(
                current,
                next,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            ) {
                Ok(_) => break,
                Err(observed) => current = observed,
            }
        }
    }

    /// Mark the namespace bound to `handle` finalized exactly once.
    fn record_finalized(
        &self,
        namespace_hash: &CanonicalHash,
    ) -> Result<(), ManagedStorageErrorV1> {
        let key = namespace_hash.to_hex();
        let mut finalized = self
            .finalized_namespaces
            .lock()
            .map_err(|_| ManagedStorageErrorV1::HandleFinalized)?;
        if finalized.contains_key(&key) {
            return Err(ManagedStorageErrorV1::HandleFinalized);
        }
        finalized.insert(key, ());
        Ok(())
    }

    /// Registers a durable grant; duplicate grant ids are rejected.
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
    admission_sequence: u64,
}

#[derive(Debug, Deserialize)]
struct PhysicalStorageAdmissionMarkerV1 {
    schema_version: u32,
    handle_id: String,
    namespace_hash: CanonicalHash,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PhysicalStorageFrontierV1 {
    byte_length: u64,
    record_count: u64,
    content_hash: CanonicalHash,
    frontier_hash: CanonicalHash,
}

/// Authority-owned managed storage service behind the kernel trait object.
pub struct AuthorityManagedStorageServiceV1 {
    table: AuthorityStorageGrantTableV1,
    authority_generation: AuthorityGeneration,
    journal: Option<Mutex<ResourceJournalFileV1>>,
    quota: Mutex<QuotaBookV1>,
    /// The journal parent is the authority-owned state anchor. It is used only by the
    /// authority's physical recovery verifier; no path crosses the kernel storage port.
    journal_root: Option<PathBuf>,
    blocked_grants_after_restart: Mutex<BTreeMap<String, ()>>,
}

impl AuthorityManagedStorageServiceV1 {
    pub fn new(
        table: AuthorityStorageGrantTableV1,
        authority_generation: AuthorityGeneration,
    ) -> Self {
        let quota_cap = quota_workspace_cap(&table);
        Self {
            table,
            authority_generation,
            journal: None,
            quota: Mutex::new(QuotaBookV1::new(quota_cap)),
            journal_root: None,
            blocked_grants_after_restart: Mutex::new(BTreeMap::new()),
        }
    }

    /// Creates the production service with an owner-only durable authority journal.
    pub fn new_with_journal(
        table: AuthorityStorageGrantTableV1,
        authority_generation: AuthorityGeneration,
        journal_path: impl AsRef<Path>,
        bootstrap_manifest_hash: CanonicalHash,
        journal_instance_hash: CanonicalHash,
    ) -> Result<Self, JournalErrorV1> {
        let header = crate::journal::ResourceJournalHeaderV1 {
            schema_version: 1,
            shard_name: "application-resources".to_owned(),
            bootstrap_manifest_hash,
            journal_instance_hash,
            header_hash: hash_debug(&(
                "application-resources",
                bootstrap_manifest_hash,
                journal_instance_hash,
            )),
        };
        let journal = ResourceJournalFileV1::open(journal_path.as_ref().to_path_buf(), header)?;
        let blocked_grants_after_restart = journal
            .unsettled_storage_grants()
            .into_iter()
            .map(|key| (key, ()))
            .collect();
        let (admissions, settled_grants) = journal.storage_admission_state();
        let all_admissions = admissions.clone();
        rehydrate_storage_state(&table, authority_generation, admissions, &settled_grants)?;
        let journal_root = journal_path.as_ref().parent().map(Path::to_path_buf);
        let quota_path = journal_root
            .as_ref()
            .map(|root| root.join(".authority-quota").join("managed-storage.json"))
            .ok_or_else(|| JournalErrorV1::Corrupt("resource journal has no parent".to_owned()))?;
        let mut quota = QuotaBookV1::open(&quota_path, quota_workspace_cap(&table))
            .map_err(|error| JournalErrorV1::Corrupt(error.to_string()))?;
        let admitted = table
            .admitted_namespaces
            .lock()
            .map_err(|_| JournalErrorV1::Corrupt("admission registry poisoned".to_owned()))?
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for record in admitted {
            let owner_key = storage_quota_owner_key(record.grant.grant_hash, record.namespace_hash);
            if settled_grants.contains(&record.grant.grant_hash.to_hex()) {
                quota
                    .release_owner(&owner_key)
                    .map_err(|error| JournalErrorV1::Corrupt(error.to_string()))?;
            } else if quota.reservation_for_owner(&owner_key).is_none() {
                quota
                    .reserve_owned(&owner_key, &record.grant.quota_profile, 0, 1)
                    .map_err(|error| JournalErrorV1::Corrupt(error.to_string()))?;
            }
        }
        for admission in all_admissions {
            if settled_grants.contains(&admission.grant_hash.to_hex()) {
                let owner_key =
                    storage_quota_owner_key(admission.grant_hash, admission.namespace_hash);
                quota
                    .release_owner(&owner_key)
                    .map_err(|error| JournalErrorV1::Corrupt(error.to_string()))?;
            }
        }
        Ok(Self {
            table,
            authority_generation,
            journal: Some(Mutex::new(journal)),
            quota: Mutex::new(quota),
            journal_root,
            blocked_grants_after_restart: Mutex::new(blocked_grants_after_restart),
        })
    }

    pub fn grant_table(&self) -> &AuthorityStorageGrantTableV1 {
        &self.table
    }

    /// Production boot may not continue while an admitted namespace from a previous process
    /// lacks a durable settlement. Reconciliation requires a domain/physical evidence bridge;
    /// silently settling here would erase an effect frontier that this authority cannot prove.
    pub fn require_startup_reconciliation(&self) -> Result<(), ManagedStorageErrorV1> {
        let blocked = self
            .blocked_grants_after_restart
            .lock()
            .map_err(|_| ManagedStorageErrorV1::JournalUnavailable)?;
        if blocked.keys().next().is_some() {
            return Err(ManagedStorageErrorV1::JournalUnavailable);
        }
        Ok(())
    }

    /// Re-reads and settles a journal-backed physical writer frontier while holding the same
    /// namespace lock used by the writer. The submitted facts are only a bounded hint: the
    /// authority revalidates the bytes and keeps the lock through observation and settlement.
    pub fn finalize_namespace_with_physical_frontier(
        &self,
        handle: ManagedStorageNamespaceHandleV1,
        byte_length: u64,
        record_count: u64,
        content_hash: CanonicalHash,
        reason: String,
    ) -> Result<ManagedStorageStorageReceiptV1, ManagedStorageErrorV1> {
        let admitted = self
            .table
            .admitted_namespaces
            .lock()
            .map_err(|_| ManagedStorageErrorV1::JournalUnavailable)?;
        let record = admitted
            .get(handle.handle_id.as_str())
            .cloned()
            .ok_or(ManagedStorageErrorV1::CapabilityMismatch)?;
        if record.namespace_hash != handle.namespace_hash
            || record.grant.capability_family != handle.capability_family
        {
            return Err(ManagedStorageErrorV1::CapabilityMismatch);
        }
        drop(admitted);

        // Startup probes and isolated in-memory authority tests have no durable physical
        // bridge. Their existing logical settlement semantics remain unchanged.
        if self.journal.is_none()
            || handle
                .handle_id
                .as_str()
                .starts_with("handle-probe-storage-")
        {
            return self.finalize_namespace(handle, reason);
        }

        let directory = self.physical_namespace_directory(&record)?;
        let _namespace_lock = open_physical_namespace_lock(&directory)?;
        let observed = self.read_physical_frontier(&record, &directory)?;
        if observed.byte_length != byte_length
            || observed.record_count != record_count
            || observed.content_hash != content_hash
        {
            return Err(ManagedStorageErrorV1::JournalUnavailable);
        }
        self.ensure_physical_frontier(&record, observed)?;
        // The physical lock remains held while finalize appends GenerationSettled and removes
        // the admitted handle. A concurrent writer therefore cannot create an unbound frontier
        // between the proof and the settlement.
        self.finalize_namespace(handle, reason)
    }

    /// Reconciles pending writer admissions through the production physical frontier bridge.
    ///
    /// The bridge is intentionally authority-owned: it resolves the journal parent, locates
    /// the writer's owner-only admission marker, verifies the real `records.jsonl` object and
    /// appends the complete RFC-0071 section 22.4 seven-record chain. Missing markers,
    /// ambiguous physical identities, partial lines and oversized content remain fail closed.
    pub fn reconcile_unsettled_storage_grants_with_physical_bridge(
        &self,
    ) -> Result<Vec<ManagedStorageStorageReceiptV1>, ManagedStorageErrorV1> {
        let records = self
            .table
            .admitted_namespaces
            .lock()
            .map_err(|_| ManagedStorageErrorV1::JournalUnavailable)?
            .iter()
            .filter(|record| {
                self.blocked_grants_after_restart
                    .lock()
                    .map(|blocked| blocked.contains_key(&record.1.grant.grant_hash.to_hex()))
                    .unwrap_or(true)
            })
            .map(|(handle_id, record)| (handle_id.clone(), record.clone()))
            .collect::<Vec<_>>();

        let mut receipts = Vec::with_capacity(records.len());
        for (handle_id, record) in records {
            let directory = self.physical_namespace_directory(&record)?;
            let _namespace_lock = open_physical_namespace_lock(&directory)?;
            let frontier = self.read_physical_frontier(&record, &directory)?;
            self.ensure_physical_frontier(&record, frontier)?;
            self.append_storage_recovery_chain(&record, frontier)?;
            let handle = ManagedStorageNamespaceHandleV1::new(
                OpaqueKernelCapabilityHandleId::new(handle_id),
                record.namespace_hash,
                record.grant.capability_family,
                OpaqueKernelCapabilityAuthenticatorV1::new(format!(
                    "recovery-{}",
                    record.grant.grant_id.as_str()
                )),
            );
            receipts.push(self.finalize_namespace(
                handle,
                "domain-storage-recovery-retained-frontier".to_owned(),
            )?);
        }
        Ok(receipts)
    }

    fn physical_namespace_directory(
        &self,
        record: &StorageAdmissionRecordV1,
    ) -> Result<PathBuf, ManagedStorageErrorV1> {
        let root = self
            .journal_root
            .as_ref()
            .ok_or(ManagedStorageErrorV1::JournalUnavailable)?;
        #[cfg(windows)]
        reject_reparse_components(root, false)
            .map_err(|_| ManagedStorageErrorV1::JournalUnavailable)?;
        let root = root
            .canonicalize()
            .map_err(|_| ManagedStorageErrorV1::JournalUnavailable)?;
        let Some(leaf) = storage_owner_leaf(record.grant.semantic_owner) else {
            return Err(ManagedStorageErrorV1::JournalUnavailable);
        };
        let leaf_root = root.join("managed").join(leaf);
        reject_reparse_components(&leaf_root, false)
            .map_err(|_| ManagedStorageErrorV1::JournalUnavailable)?;
        let leaf_metadata = fs::symlink_metadata(&leaf_root)
            .map_err(|_| ManagedStorageErrorV1::JournalUnavailable)?;
        if !is_safe_physical_metadata(&leaf_metadata) || !leaf_metadata.is_dir() {
            return Err(ManagedStorageErrorV1::JournalUnavailable);
        }

        let mut candidates = vec![leaf_root.clone()];
        for entry in
            fs::read_dir(&leaf_root).map_err(|_| ManagedStorageErrorV1::JournalUnavailable)?
        {
            let entry = entry.map_err(|_| ManagedStorageErrorV1::JournalUnavailable)?;
            let metadata = fs::symlink_metadata(entry.path())
                .map_err(|_| ManagedStorageErrorV1::JournalUnavailable)?;
            if !is_safe_physical_metadata(&metadata) {
                return Err(ManagedStorageErrorV1::JournalUnavailable);
            }
            if metadata.is_dir() {
                candidates.push(entry.path());
            }
        }

        let mut matched = None;
        for candidate in candidates {
            let marker_path = candidate.join("authority-admission.json");
            let marker_metadata = match fs::symlink_metadata(&marker_path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return Err(ManagedStorageErrorV1::JournalUnavailable),
            };
            if !is_safe_physical_metadata(&marker_metadata) || !marker_metadata.is_file() {
                return Err(ManagedStorageErrorV1::JournalUnavailable);
            }
            let marker: PhysicalStorageAdmissionMarkerV1 = serde_json::from_slice(
                &read_no_follow_file(&marker_path)
                    .map_err(|_| ManagedStorageErrorV1::JournalUnavailable)?,
            )
            .map_err(|_| ManagedStorageErrorV1::JournalUnavailable)?;
            if marker.schema_version != 1 {
                return Err(ManagedStorageErrorV1::JournalUnavailable);
            }
            if marker.namespace_hash == record.namespace_hash
                && marker.handle_id == record.handle_id
                && matched.replace(candidate).is_some()
            {
                return Err(ManagedStorageErrorV1::JournalUnavailable);
            }
        }
        matched.ok_or(ManagedStorageErrorV1::JournalUnavailable)
    }

    fn read_physical_frontier(
        &self,
        record: &StorageAdmissionRecordV1,
        directory: &Path,
    ) -> Result<PhysicalStorageFrontierV1, ManagedStorageErrorV1> {
        let record_path = directory.join("records.jsonl");
        let bytes = match fs::symlink_metadata(&record_path) {
            Ok(metadata) => {
                if !is_safe_physical_metadata(&metadata) || !metadata.is_file() {
                    return Err(ManagedStorageErrorV1::JournalUnavailable);
                }
                if metadata.len() > record.grant.quota_profile.max_bytes {
                    return Err(ManagedStorageErrorV1::JournalUnavailable);
                }
                read_no_follow_file(&record_path)
                    .map_err(|_| ManagedStorageErrorV1::JournalUnavailable)?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(_) => return Err(ManagedStorageErrorV1::JournalUnavailable),
        };
        if bytes.len() as u64 > record.grant.quota_profile.max_bytes
            || (!bytes.is_empty() && !bytes.ends_with(b"\n"))
        {
            return Err(ManagedStorageErrorV1::JournalUnavailable);
        }
        let record_count = bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .count() as u64;
        let content_hash = hash_bytes(&bytes);
        Ok(PhysicalStorageFrontierV1 {
            byte_length: bytes.len() as u64,
            record_count,
            content_hash,
            frontier_hash: self.physical_frontier_hash(
                record,
                bytes.len() as u64,
                record_count,
                content_hash,
            )?,
        })
    }

    fn physical_frontier_hash(
        &self,
        record: &StorageAdmissionRecordV1,
        byte_length: u64,
        record_count: u64,
        content_hash: CanonicalHash,
    ) -> Result<CanonicalHash, ManagedStorageErrorV1> {
        let journal_instance_hash = self
            .journal
            .as_ref()
            .map(|journal| {
                journal
                    .lock()
                    .map_err(|_| ManagedStorageErrorV1::JournalUnavailable)
                    .and_then(|journal| {
                        journal
                            .header()
                            .map(|header| header.journal_instance_hash)
                            .ok_or(ManagedStorageErrorV1::JournalUnavailable)
                    })
            })
            .transpose()?
            .unwrap_or(CanonicalHash::from_bytes([0u8; 32]));
        Ok(hash_debug(&(
            record.grant.grant_hash,
            record.handle_id.as_str(),
            record.namespace_hash,
            record.grant.resource_ref.generation,
            self.authority_generation,
            journal_instance_hash,
            byte_length,
            record_count,
            content_hash,
        )))
    }

    fn ensure_physical_frontier(
        &self,
        record: &StorageAdmissionRecordV1,
        frontier: PhysicalStorageFrontierV1,
    ) -> Result<(), ManagedStorageErrorV1> {
        let Some(journal) = &self.journal else {
            return Ok(());
        };
        let mut journal = journal
            .lock()
            .map_err(|_| ManagedStorageErrorV1::JournalUnavailable)?;
        let existing = journal
            .storage_physical_frontier_records(record.grant.grant_hash, record.namespace_hash);
        if existing.iter().any(
            |(_, byte_length, record_count, content_hash, frontier_hash)| {
                *byte_length == frontier.byte_length
                    && *record_count == frontier.record_count
                    && *content_hash == frontier.content_hash
                    && *frontier_hash == frontier.frontier_hash
            },
        ) {
            if existing.iter().any(
                |(_, byte_length, record_count, content_hash, frontier_hash)| {
                    *byte_length != frontier.byte_length
                        || *record_count != frontier.record_count
                        || *content_hash != frontier.content_hash
                        || *frontier_hash != frontier.frontier_hash
                },
            ) {
                return Err(ManagedStorageErrorV1::JournalUnavailable);
            }
            return Ok(());
        }
        if !existing.is_empty() {
            return Err(ManagedStorageErrorV1::JournalUnavailable);
        }
        journal
            .append_event(
                ResourceJournalEventV1::DomainStoragePhysicalFrontierObserved {
                    grant_hash: record.grant.grant_hash,
                    namespace_hash: record.namespace_hash,
                    byte_length: frontier.byte_length,
                    record_count: frontier.record_count,
                    content_hash: frontier.content_hash,
                    frontier_hash: frontier.frontier_hash,
                },
            )
            .map_err(|_| ManagedStorageErrorV1::JournalUnavailable)?;
        Ok(())
    }

    fn append_storage_recovery_chain(
        &self,
        record: &StorageAdmissionRecordV1,
        frontier: PhysicalStorageFrontierV1,
    ) -> Result<(), ManagedStorageErrorV1> {
        use crate::journal::MAX_DOMAIN_STORAGE_RECOVERY_ENVELOPE_BYTES as MAX;

        let grant_hash = record.grant.grant_hash;
        let raised_envelope = serde_json::to_vec(&serde_json::json!({
            "schema": "managed-storage-raised-v2",
            "grant_hash": grant_hash,
            "namespace_hash": record.namespace_hash,
            "request": &record.request,
            "physical_frontier_hash": frontier.frontier_hash,
        }))
        .map_err(|_| ManagedStorageErrorV1::JournalUnavailable)?;
        let action_token_hash = hash_debug(&("retain-verified-physical-frontier-v1", grant_hash));
        let started_envelope = serde_json::to_vec(&serde_json::json!({
            "schema": "managed-storage-resolution-started-v2",
            "grant_hash": grant_hash,
            "action_token_hash": action_token_hash,
        }))
        .map_err(|_| ManagedStorageErrorV1::JournalUnavailable)?;
        let operation_id = format!("storage-recovery-{}", grant_hash.to_hex());
        let authorized_operation = serde_json::to_vec(&serde_json::json!({
            "schema": "managed-storage-recovery-operation-prepared-v1",
            "operation_id": operation_id,
            "operation": "retain-verified-physical-frontier",
            "frontier_hash": frontier.frontier_hash,
        }))
        .map_err(|_| ManagedStorageErrorV1::JournalUnavailable)?;
        let repair_receipt = serde_json::to_vec(&serde_json::json!({
            "schema": "managed-storage-repair-receipt-v1",
            "operation_id": operation_id,
            "action": "retained",
            "byte_length": frontier.byte_length,
            "record_count": frontier.record_count,
            "content_hash": frontier.content_hash,
        }))
        .map_err(|_| ManagedStorageErrorV1::JournalUnavailable)?;
        let terminal_or_successor_event = serde_json::to_vec(&serde_json::json!({
            "schema": "managed-storage-resolved-v2",
            "grant_hash": grant_hash,
            "frontier_hash": frontier.frontier_hash,
        }))
        .map_err(|_| ManagedStorageErrorV1::JournalUnavailable)?;
        let projected_event = serde_json::to_vec(&serde_json::json!({
            "schema": "managed-storage-blocker-projected-v1",
            "grant_hash": grant_hash,
            "projection": "resolved",
            "frontier_hash": frontier.frontier_hash,
        }))
        .map_err(|_| ManagedStorageErrorV1::JournalUnavailable)?;
        if [
            &raised_envelope,
            &started_envelope,
            &authorized_operation,
            &repair_receipt,
            &terminal_or_successor_event,
            &projected_event,
        ]
        .into_iter()
        .any(|envelope| envelope.len() > MAX)
        {
            return Err(ManagedStorageErrorV1::JournalUnavailable);
        }

        let prefix = self
            .journal
            .as_ref()
            .ok_or(ManagedStorageErrorV1::JournalUnavailable)?
            .lock()
            .map_err(|_| ManagedStorageErrorV1::JournalUnavailable)?
            .storage_recovery_records(grant_hash);
        if prefix.len() > 7 {
            return Err(ManagedStorageErrorV1::JournalUnavailable);
        }
        let mut hashes = [None; 7];
        for (index, (journal_record, event)) in prefix.iter().enumerate() {
            let valid = match (index, event) {
                (
                    0,
                    ResourceJournalEventV1::DomainStorageFailureObserved {
                        grant_hash: current,
                        namespace_hash,
                        raised_envelope: envelope,
                        physical_frontier_hash,
                        request_hash,
                    },
                ) => {
                    *current == grant_hash
                        && *namespace_hash == record.namespace_hash
                        && envelope == &raised_envelope
                        && *physical_frontier_hash == frontier.frontier_hash
                        && *request_hash == hash_debug(&record.request)
                }
                (
                    1,
                    ResourceJournalEventV1::DomainStorageResolutionStartedShadow {
                        grant_hash: current,
                        observed_record_hash,
                        action_token_hash: current_action,
                        started_envelope: envelope,
                        request_hash,
                    },
                ) => {
                    *current == grant_hash
                        && hashes[0] == Some(*observed_record_hash)
                        && *current_action == action_token_hash
                        && envelope == &started_envelope
                        && *request_hash == hash_debug(&record.request)
                }
                (
                    2,
                    ResourceJournalEventV1::RecoveryOperationPrepared {
                        grant_hash: current,
                        recovery_operation_id: current_operation,
                        authorized_operation: operation,
                        target_frontier_hash,
                    },
                ) => {
                    *current == grant_hash
                        && current_operation == &operation_id
                        && operation == &authorized_operation
                        && *target_frontier_hash == frontier.frontier_hash
                }
                (
                    3,
                    ResourceJournalEventV1::DomainStorageResolutionPrepared {
                        grant_hash: current,
                        started_shadow_record_hash,
                        recovery_prepared_record_hash,
                        bridge_frontier_hash,
                    },
                ) => {
                    *current == grant_hash
                        && hashes[1] == Some(*started_shadow_record_hash)
                        && hashes[2] == Some(*recovery_prepared_record_hash)
                        && *bridge_frontier_hash == frontier.frontier_hash
                }
                (
                    4,
                    ResourceJournalEventV1::RecoveryOperationSettled {
                        grant_hash: current,
                        recovery_operation_id: current_operation,
                        repair_receipt: receipt,
                        settled_frontier_hash,
                    },
                ) => {
                    *current == grant_hash
                        && current_operation == &operation_id
                        && receipt == &repair_receipt
                        && *settled_frontier_hash == frontier.frontier_hash
                }
                (
                    5,
                    ResourceJournalEventV1::DomainStorageResolutionSettled {
                        grant_hash: current,
                        resolution_prepared_record_hash,
                        recovery_settled_record_hash,
                        receipt_event,
                        terminal_or_successor_event: terminal,
                    },
                ) => {
                    *current == grant_hash
                        && hashes[3] == Some(*resolution_prepared_record_hash)
                        && hashes[4] == Some(*recovery_settled_record_hash)
                        && receipt_event == &repair_receipt
                        && terminal == &terminal_or_successor_event
                }
                (
                    6,
                    ResourceJournalEventV1::DomainBlockerProjected {
                        grant_hash: current,
                        resolution_settled_record_hash,
                        projected_event_ids_hash,
                        final_frontier_hash,
                        projected_event: projection,
                    },
                ) => {
                    *current == grant_hash
                        && hashes[5] == Some(*resolution_settled_record_hash)
                        && *projected_event_ids_hash
                            == hash_debug(&(
                                grant_hash,
                                hashes[5],
                                terminal_or_successor_event.clone(),
                            ))
                        && *final_frontier_hash == frontier.frontier_hash
                        && projection == &projected_event
                }
                _ => false,
            };
            if !valid {
                return Err(ManagedStorageErrorV1::JournalUnavailable);
            }
            hashes[index] = Some(journal_record.record_hash);
        }

        for index in prefix.len()..7 {
            let event = match index {
                0 => ResourceJournalEventV1::DomainStorageFailureObserved {
                    grant_hash,
                    namespace_hash: record.namespace_hash,
                    raised_envelope: raised_envelope.clone(),
                    physical_frontier_hash: frontier.frontier_hash,
                    request_hash: hash_debug(&record.request),
                },
                1 => ResourceJournalEventV1::DomainStorageResolutionStartedShadow {
                    grant_hash,
                    observed_record_hash: hashes[0]
                        .ok_or(ManagedStorageErrorV1::JournalUnavailable)?,
                    action_token_hash,
                    started_envelope: started_envelope.clone(),
                    request_hash: hash_debug(&record.request),
                },
                2 => ResourceJournalEventV1::RecoveryOperationPrepared {
                    grant_hash,
                    recovery_operation_id: operation_id.clone(),
                    authorized_operation: authorized_operation.clone(),
                    target_frontier_hash: frontier.frontier_hash,
                },
                3 => ResourceJournalEventV1::DomainStorageResolutionPrepared {
                    grant_hash,
                    started_shadow_record_hash: hashes[1]
                        .ok_or(ManagedStorageErrorV1::JournalUnavailable)?,
                    recovery_prepared_record_hash: hashes[2]
                        .ok_or(ManagedStorageErrorV1::JournalUnavailable)?,
                    bridge_frontier_hash: frontier.frontier_hash,
                },
                4 => ResourceJournalEventV1::RecoveryOperationSettled {
                    grant_hash,
                    recovery_operation_id: operation_id.clone(),
                    repair_receipt: repair_receipt.clone(),
                    settled_frontier_hash: frontier.frontier_hash,
                },
                5 => ResourceJournalEventV1::DomainStorageResolutionSettled {
                    grant_hash,
                    resolution_prepared_record_hash: hashes[3]
                        .ok_or(ManagedStorageErrorV1::JournalUnavailable)?,
                    recovery_settled_record_hash: hashes[4]
                        .ok_or(ManagedStorageErrorV1::JournalUnavailable)?,
                    receipt_event: repair_receipt.clone(),
                    terminal_or_successor_event: terminal_or_successor_event.clone(),
                },
                6 => ResourceJournalEventV1::DomainBlockerProjected {
                    grant_hash,
                    resolution_settled_record_hash: hashes[5]
                        .ok_or(ManagedStorageErrorV1::JournalUnavailable)?,
                    projected_event_ids_hash: hash_debug(&(
                        grant_hash,
                        hashes[5],
                        terminal_or_successor_event.clone(),
                    )),
                    final_frontier_hash: frontier.frontier_hash,
                    projected_event: projected_event.clone(),
                },
                _ => unreachable!(),
            };
            let appended = self
                .append_journal_event(event)?
                .ok_or(ManagedStorageErrorV1::JournalUnavailable)?;
            hashes[index] = Some(appended.record_hash);
        }
        Ok(())
    }

    fn append_journal_event(
        &self,
        event: ResourceJournalEventV1,
    ) -> Result<Option<ResourceJournalRecordV1>, ManagedStorageErrorV1> {
        let Some(journal) = &self.journal else {
            return Ok(None);
        };
        journal
            .lock()
            .map_err(|_| ManagedStorageErrorV1::JournalUnavailable)?
            .append_event(event)
            .map(Some)
            .map_err(|_| ManagedStorageErrorV1::JournalUnavailable)
    }

    fn existing_settlement(
        &self,
        grant_hash: CanonicalHash,
    ) -> Result<Option<ResourceJournalRecordV1>, ManagedStorageErrorV1> {
        let Some(journal) = &self.journal else {
            return Ok(None);
        };
        journal
            .lock()
            .map_err(|_| ManagedStorageErrorV1::JournalUnavailable)
            .map(|journal| journal.settled_storage_record(grant_hash))
    }

    fn clear_restart_blocker(&self, grant_hash: CanonicalHash) {
        if let Ok(mut blocked) = self.blocked_grants_after_restart.lock() {
            blocked.remove(&grant_hash.to_hex());
        }
    }

    fn reserve_quota(
        &self,
        owner_key: &str,
        profile: &sigil_kernel::resource::ResourceQuotaProfileV1,
    ) -> Result<(), ManagedStorageErrorV1> {
        self.quota
            .lock()
            .map_err(|_| ManagedStorageErrorV1::JournalUnavailable)?
            .reserve_owned(owner_key, profile, 0, 1)
            .map(|_| ())
            .map_err(storage_quota_error)
    }

    fn reconcile_quota(
        &self,
        owner_key: &str,
        profile: &sigil_kernel::resource::ResourceQuotaProfileV1,
        bytes: u64,
        entries: u64,
    ) -> Result<(), ManagedStorageErrorV1> {
        self.quota
            .lock()
            .map_err(|_| ManagedStorageErrorV1::JournalUnavailable)?
            .reconcile_owned(owner_key, profile, bytes, entries)
            .map(|_| ())
            .map_err(storage_quota_error)
    }

    fn release_quota(&self, owner_key: &str) -> Result<(), ManagedStorageErrorV1> {
        self.quota
            .lock()
            .map_err(|_| ManagedStorageErrorV1::JournalUnavailable)?
            .release_owner(owner_key)
            .map_err(storage_quota_error)
    }

    /// Reconciles storage admissions recovered from a previous authority instance. The caller
    /// chooses the typed cleanup status; the journal remains the source of truth and the action
    /// is idempotent when a prior settlement record already exists.
    pub fn reconcile_unsettled_storage_grants(
        &self,
        cleanup_status: &str,
    ) -> Result<Vec<ManagedStorageStorageReceiptV1>, ManagedStorageErrorV1> {
        // A journal-backed pending admission must go through the physical verifier. The old
        // in-memory helper remains available for isolated authority tests only; allowing it to
        // settle a durable pending grant would bypass the frontier proof required by P1-7.
        if self.journal.is_some() {
            return Err(ManagedStorageErrorV1::JournalUnavailable);
        }
        let handles = self
            .table
            .admitted_namespaces
            .lock()
            .map_err(|_| ManagedStorageErrorV1::JournalUnavailable)?
            .iter()
            .filter(|record| {
                self.blocked_grants_after_restart
                    .lock()
                    .map(|blocked| blocked.contains_key(&record.1.grant.grant_hash.to_hex()))
                    .unwrap_or(true)
            })
            .map(|(handle_id, record)| {
                ManagedStorageNamespaceHandleV1::new(
                    OpaqueKernelCapabilityHandleId::new(handle_id.clone()),
                    record.namespace_hash,
                    record.grant.capability_family,
                    OpaqueKernelCapabilityAuthenticatorV1::new(format!(
                        "rehydrated-{}",
                        record.grant.grant_id.as_str()
                    )),
                )
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| self.finalize_namespace(handle, cleanup_status.to_owned()))
            .collect()
    }
}

impl ManagedStorageServiceV1 for AuthorityManagedStorageServiceV1 {
    fn validate_namespace_write(
        &self,
        handle: &ManagedStorageNamespaceHandleV1,
    ) -> Result<(), ManagedStorageErrorV1> {
        let admitted = self
            .table
            .admitted_namespaces
            .lock()
            .map_err(|_| ManagedStorageErrorV1::JournalUnavailable)?;
        let record = admitted
            .get(handle.handle_id.as_str())
            .ok_or(ManagedStorageErrorV1::HandleFinalized)?;
        if record.namespace_hash != handle.namespace_hash
            || record.grant.capability_family != handle.capability_family
        {
            return Err(ManagedStorageErrorV1::CapabilityMismatch);
        }
        Ok(())
    }

    fn finalize_namespace_with_physical_frontier(
        &self,
        handle: ManagedStorageNamespaceHandleV1,
        byte_length: u64,
        record_count: u64,
        content_hash: CanonicalHash,
        reason: String,
    ) -> Result<ManagedStorageStorageReceiptV1, ManagedStorageErrorV1> {
        AuthorityManagedStorageServiceV1::finalize_namespace_with_physical_frontier(
            self,
            handle,
            byte_length,
            record_count,
            content_hash,
            reason,
        )
    }

    fn reconcile_namespace_quota(
        &self,
        handle: &ManagedStorageNamespaceHandleV1,
        bytes: u64,
        entries: u64,
    ) -> Result<(), ManagedStorageErrorV1> {
        let record = self
            .table
            .admitted_namespaces
            .lock()
            .map_err(|_| ManagedStorageErrorV1::JournalUnavailable)?
            .get(handle.handle_id.as_str())
            .cloned()
            .ok_or(ManagedStorageErrorV1::HandleFinalized)?;
        if record.namespace_hash != handle.namespace_hash
            || record.grant.capability_family != handle.capability_family
        {
            return Err(ManagedStorageErrorV1::CapabilityMismatch);
        }
        let owner_key = storage_quota_owner_key(record.grant.grant_hash, record.namespace_hash);
        self.reconcile_quota(&owner_key, &record.grant.quota_profile, bytes, entries)
    }

    fn admit_namespace(
        &self,
        request: ManagedStorageAdmissionRequestV1,
        capability: ValidatedStorageAdmissionCapabilityV1,
    ) -> Result<ManagedStorageNamespaceHandleV1, ManagedStorageErrorV1> {
        // Kernel-owned startup readiness probes use a dedicated probe namespace: the probe
        // must never finalize (consume) the production grant namespace, or the next writer
        // batch for that channel would be refused as already finalized.
        let probe = capability.handle_id.as_str() == "startup-probe";
        // Family-exact closure: any unrelated grant must not masquerade as readiness for a
        // different capability family (R71.6 cutover probe depends on this exactness).
        let Some(grant) = self.table.grants.values().find(|grant| {
            grant.capability_family == request.capability_family
                && grant.semantic_owner == request.semantic_owner
                && grant.purpose == request.purpose
                && grant.journal_scope == request.journal_scope
                && (probe || grant.owner_scope == request.owner_scope)
                && (probe || grant.source_class == request.source.source_class())
        }) else {
            return Err(ManagedStorageErrorV1::FamilyMismatch);
        };
        let binding = capability.binding();
        if !probe {
            if self
                .blocked_grants_after_restart
                .lock()
                .map_err(|_| ManagedStorageErrorV1::JournalUnavailable)?
                .contains_key(&grant.grant_hash.to_hex())
            {
                return Err(ManagedStorageErrorV1::JournalUnavailable);
            }
            let Some(binding) = binding else {
                return Err(ManagedStorageErrorV1::CapabilityMismatch);
            };
            if binding.family() != request.capability_family
                || binding.namespace_hash() != grant.namespace_hash
            {
                return Err(ManagedStorageErrorV1::CapabilityMismatch);
            }
            if grant.source_binding_hash != source_binding_hash(&request.source)
                || grant.authority_generation != self.authority_generation
            {
                return Err(ManagedStorageErrorV1::CapabilityMismatch);
            }
        }
        let (handle_id, namespace_hash, authenticator) = if probe {
            let mut probe_ns = [0x9fu8; 32];
            let seq = self.table.next_probe_sequence();
            probe_ns[24..].copy_from_slice(&seq.to_be_bytes());
            probe_ns[0] = match grant.capability_family {
                sigil_kernel::resource::ManagedStorageCapabilityFamilyV1::AppendLog => 1,
                sigil_kernel::resource::ManagedStorageCapabilityFamilyV1::AtomicObject => 2,
                sigil_kernel::resource::ManagedStorageCapabilityFamilyV1::JournaledAtomicProjection => 3,
                sigil_kernel::resource::ManagedStorageCapabilityFamilyV1::StreamingArtifact => 4,
                sigil_kernel::resource::ManagedStorageCapabilityFamilyV1::ArtifactStore => 5,
                sigil_kernel::resource::ManagedStorageCapabilityFamilyV1::RebuildableDatabaseProjection => 6,
                sigil_kernel::resource::ManagedStorageCapabilityFamilyV1::SemanticLeaseLedger => 7,
            };
            (
                sigil_kernel::resource::OpaqueKernelCapabilityHandleId::new(format!(
                    "handle-probe-storage-{seq}"
                )),
                CanonicalHash::from_bytes(probe_ns),
                OpaqueKernelCapabilityAuthenticatorV1::new(format!("auth-probe-storage-{seq}")),
            )
        } else {
            // Broker-issued claims: the namespace identity is the claim binding itself (the
            // broker seals the true family/namespace kernel-side). Each claim is a distinct
            // one-shot namespace, so named writer batches never share a finalize scope.
            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            hasher.update(capability.handle_id.as_str().as_bytes());
            let claim_ns = CanonicalHash::from_bytes(hasher.finalize().into());
            (
                capability.handle_id.clone(),
                claim_ns,
                OpaqueKernelCapabilityAuthenticatorV1::new(format!(
                    "auth-{}",
                    capability.handle_id.as_str()
                )),
            )
        };
        let handle = ManagedStorageNamespaceHandleV1::new(
            handle_id,
            namespace_hash,
            grant.capability_family,
            authenticator,
        );
        let mut admitted = self
            .table
            .admitted_namespaces
            .lock()
            .map_err(|_| ManagedStorageErrorV1::JournalUnavailable)?;
        if admitted.contains_key(handle.handle_id.as_str()) {
            return Err(ManagedStorageErrorV1::DuplicateClaim);
        }
        let quota_owner_key = storage_quota_owner_key(grant.grant_hash, namespace_hash);
        if !probe {
            self.reserve_quota(&quota_owner_key, &grant.quota_profile)?;
        }
        let admission_sequence =
            match self.append_journal_event(ResourceJournalEventV1::StorageNamespaceAdmitted {
                grant_hash: grant.grant_hash,
                handle_id: handle.handle_id.as_str().to_owned(),
                namespace_hash,
                grant: Box::new(grant.clone()),
                request: Box::new(request.clone()),
            }) {
                Ok(record) => record
                    .map(|record| record.sequence)
                    .unwrap_or(grant.journal_admission_sequence),
                Err(error) => {
                    if !probe {
                        let _ = self.release_quota(&quota_owner_key);
                    }
                    return Err(error);
                }
            };
        admitted.insert(
            handle.handle_id.as_str().to_owned(),
            StorageAdmissionRecordV1 {
                handle_id: handle.handle_id.as_str().to_owned(),
                grant: grant.clone(),
                request,
                namespace_hash,
                admission_sequence,
            },
        );
        Ok(handle)
    }

    fn finalize_namespace(
        &self,
        handle: ManagedStorageNamespaceHandleV1,
        reason: String,
    ) -> Result<ManagedStorageStorageReceiptV1, ManagedStorageErrorV1> {
        let mut admitted = self
            .table
            .admitted_namespaces
            .lock()
            .map_err(|_| ManagedStorageErrorV1::HandleFinalized)?;
        let record = admitted
            .get(handle.handle_id.as_str())
            .cloned()
            .ok_or(ManagedStorageErrorV1::CapabilityMismatch)?;
        if record.namespace_hash != handle.namespace_hash
            || record.grant.capability_family != handle.capability_family
        {
            return Err(ManagedStorageErrorV1::CapabilityMismatch);
        }
        let physical_binding = if handle
            .handle_id
            .as_str()
            .starts_with("handle-probe-storage-")
        {
            None
        } else if let Some(journal) = &self.journal {
            let journal = journal
                .lock()
                .map_err(|_| ManagedStorageErrorV1::JournalUnavailable)?;
            let observations = journal
                .storage_physical_frontier_records(record.grant.grant_hash, record.namespace_hash);
            if observations.len() != 1 {
                return Err(ManagedStorageErrorV1::JournalUnavailable);
            }
            let (observation_record, byte_length, record_count, _, frontier_hash) =
                observations[0].clone();
            Some((
                frontier_hash,
                observation_record.record_hash,
                byte_length,
                record_count,
            ))
        } else {
            None
        };
        let quota_owner_key =
            storage_quota_owner_key(record.grant.grant_hash, record.namespace_hash);
        if let Some((_, _, byte_length, record_count)) = physical_binding {
            self.reconcile_quota(
                &quota_owner_key,
                &record.grant.quota_profile,
                byte_length,
                record_count,
            )?;
        }
        let operation_digest = hash_debug(&(&record.request, &reason, physical_binding));
        let settlement = match self.existing_settlement(record.grant.grant_hash)? {
            Some(existing) => Some(existing),
            None => self.append_journal_event(ResourceJournalEventV1::GenerationSettled {
                grant_hash: record.grant.grant_hash,
                resource_id: record.grant.resource_ref.resource_id.as_str().to_owned(),
                generation: record.grant.resource_ref.generation,
                cleanup_status: reason,
                physical_frontier_hash: physical_binding
                    .map(|(frontier_hash, _, _, _)| frontier_hash),
                physical_observation_record_hash: physical_binding
                    .map(|(_, observation_record_hash, _, _)| observation_record_hash),
            })?,
        };
        // Settlement is appended before quota release. If the release journal write fails, the
        // admitted record remains retryable and the next authority restart can reconcile the
        // settled grant against the still-active quota owner.
        self.release_quota(&quota_owner_key)?;
        self.table.record_finalized(&handle.namespace_hash)?;
        admitted.remove(handle.handle_id.as_str());
        self.clear_restart_blocker(record.grant.grant_hash);
        let committed_frontier_hash = if let Some(settlement) = &settlement {
            settlement.committed_frontier_hash
        } else {
            hash_debug(&(
                record.grant.grant_hash,
                record.namespace_hash,
                operation_digest,
            ))
        };
        let receipt_hash = hash_debug(&(
            record.grant.grant_id.as_str(),
            record.grant.grant_hash,
            record.grant.resource_ref.resource_id.as_str(),
            operation_digest,
            committed_frontier_hash,
            record.admission_sequence,
            settlement.as_ref().map(|record| record.sequence),
        ));
        Ok(ManagedStorageStorageReceiptV1 {
            grant_id: record.grant.grant_id,
            grant_hash: record.grant.grant_hash,
            semantic_owner: record.grant.semantic_owner,
            capability_family: record.grant.capability_family,
            resource_id: record.grant.resource_ref.resource_id,
            operation_digest,
            committed_sequence_or_version: Some(
                settlement
                    .as_ref()
                    .map(|record| record.sequence)
                    .unwrap_or(record.grant.journal_admission_sequence),
            ),
            committed_frontier_hash,
            receipt_hash,
            physical_frontier_hash: physical_binding.map(|(frontier_hash, _, _, _)| frontier_hash),
            physical_observation_record_hash: physical_binding
                .map(|(_, observation_record_hash, _, _)| observation_record_hash),
        })
    }
}

fn rehydrate_storage_state(
    table: &AuthorityStorageGrantTableV1,
    authority_generation: AuthorityGeneration,
    admissions: Vec<ResourceJournalStorageAdmissionV1>,
    settled_grants: &std::collections::BTreeSet<String>,
) -> Result<(), JournalErrorV1> {
    for admission in admissions {
        if let Some(sequence) = admission
            .handle_id
            .strip_prefix("handle-probe-storage-")
            .and_then(|value| value.parse::<u64>().ok())
        {
            table.advance_probe_sequence(sequence.saturating_add(1));
        }
        let grant_id = admission.grant.grant_id.as_str().to_owned();
        // A settled admission is historical evidence only. It no longer authorizes a new
        // namespace, so a later authority composition may legitimately have a different
        // source-bound grant (for example after a cutover manifest changes). Requiring the
        // current registration to byte-match an already terminal historical grant would turn
        // a normal authority upgrade into false journal corruption. The embedded grant hash is
        // still checked below so a tampered event payload remains corrupt.
        if admission.grant_hash != admission.grant.grant_hash {
            return Err(JournalErrorV1::Corrupt(format!(
                "journal admission grant payload hash mismatch for {grant_id}"
            )));
        }
        if settled_grants.contains(&admission.grant_hash.to_hex()) {
            table
                .finalized_namespaces
                .lock()
                .map_err(|_| JournalErrorV1::Corrupt("finalized registry poisoned".to_owned()))?
                .insert(admission.namespace_hash.to_hex(), ());
            continue;
        }
        let Some(registered) = table.grants.get(&grant_id) else {
            return Err(JournalErrorV1::Corrupt(format!(
                "journal admission references unregistered grant {grant_id}"
            )));
        };
        if registered != &admission.grant
            || admission.grant_hash != admission.grant.grant_hash
            || admission.grant.authority_generation != authority_generation
        {
            return Err(JournalErrorV1::Corrupt(format!(
                "journal admission grant binding mismatch for {grant_id}"
            )));
        }
        let previous = table
            .admitted_namespaces
            .lock()
            .map_err(|_| JournalErrorV1::Corrupt("admission registry poisoned".to_owned()))?
            .insert(
                admission.handle_id.clone(),
                StorageAdmissionRecordV1 {
                    handle_id: admission.handle_id.clone(),
                    grant: admission.grant,
                    request: admission.request,
                    namespace_hash: admission.namespace_hash,
                    admission_sequence: admission.admission_sequence,
                },
            );
        if previous.is_some() {
            return Err(JournalErrorV1::Corrupt(format!(
                "duplicate journal admission handle {}",
                admission.handle_id
            )));
        }
    }
    Ok(())
}

fn source_binding_hash(
    source: &sigil_kernel::managed_storage::StorageAdmissionSourceV1,
) -> CanonicalHash {
    match source {
        sigil_kernel::managed_storage::StorageAdmissionSourceV1::ApplicationCutoverRoot {
            cutover_manifest_hash,
            ..
        } => *cutover_manifest_hash,
        _ => hash_debug(source),
    }
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

fn storage_quota_error(_error: QuotaErrorV1) -> ManagedStorageErrorV1 {
    // Keep quota journal failures inside the authority's existing fail-closed storage error
    // boundary; recovery can then classify the durable blocker without exposing filesystem
    // details through the kernel storage port.
    ManagedStorageErrorV1::JournalUnavailable
}

fn hash_debug(value: &impl std::fmt::Debug) -> CanonicalHash {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(format!("{value:?}").as_bytes());
    CanonicalHash::from_bytes(hasher.finalize().into())
}

fn hash_bytes(value: &[u8]) -> CanonicalHash {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(value);
    CanonicalHash::from_bytes(hasher.finalize().into())
}

fn storage_owner_leaf(
    owner: sigil_kernel::resource::ManagedStorageSemanticOwnerV1,
) -> Option<&'static str> {
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
        ManagedStorageSemanticOwnerV1::WorkspaceMutationState
        | ManagedStorageSemanticOwnerV1::ApplicationControlLog
        | ManagedStorageSemanticOwnerV1::PlanStore
        | ManagedStorageSemanticOwnerV1::ProviderConnectionState
        | ManagedStorageSemanticOwnerV1::RuntimeCache(_) => None,
    }
}

/// Shared authority/writer lock for one physical managed namespace. The writer holds this lock
/// while appending and syncing `records.jsonl`; the authority holds it while re-reading the
/// exact bytes used for the durable frontier proof.
fn open_physical_namespace_lock(directory: &Path) -> Result<File, ManagedStorageErrorV1> {
    let path = directory.join(".authority-storage.lock");
    reject_reparse_components(&path, true)
        .map_err(|_| ManagedStorageErrorV1::JournalUnavailable)?;
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
    let file = options
        .open(&path)
        .map_err(|_| ManagedStorageErrorV1::JournalUnavailable)?;
    let metadata =
        fs::symlink_metadata(&path).map_err(|_| ManagedStorageErrorV1::JournalUnavailable)?;
    if !is_safe_physical_metadata(&metadata) || !metadata.is_file() {
        return Err(ManagedStorageErrorV1::JournalUnavailable);
    }
    file.lock_exclusive()
        .map_err(|_| ManagedStorageErrorV1::JournalUnavailable)?;
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

/// Rejects symlink/reparse ancestors before a physical managed object is opened. The final
/// component may be absent when the caller is about to create it; an absent ancestor is always
/// an error so `create(true)` cannot traverse an unverified parent.
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
                format!(
                    "physical managed path contains a symlink or reparse point: {}",
                    current.display()
                ),
            ));
        }
    }
    Ok(())
}

fn read_no_follow_file(path: &Path) -> std::io::Result<Vec<u8>> {
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
    let mut file = options.open(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if !is_safe_physical_metadata(&metadata) || !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "physical managed object is not a regular non-reparse file",
        ));
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Authority-private storage capability verifier facet (RA-owned; factory returns only
/// the kernel verifier trait object).
pub struct AuthorityStorageCapabilityActivationEvidenceVerifierV1;

impl Default for AuthorityStorageCapabilityActivationEvidenceVerifierV1 {
    fn default() -> Self {
        Self
    }
}

/// Authority-private logical key registry (R71.5 materializes journal-backed rehydration;
/// R71.2 freezes the closed key kinds and the descriptor validation fence).
#[derive(Debug, Default)]
pub struct AuthorityLogicalKeyRegistryV1 {
    keys: BTreeMap<String, (OpaqueStorageKeyIdV1, String)>,
}

impl AuthorityLogicalKeyRegistryV1 {
    /// Reserved key registration; duplicate key ids fail closed.
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
            .insert(key.clone(), (key_id.clone(), kind_label.to_owned()))
            .is_some()
        {
            return Err(ManagedStorageErrorV1::DuplicateClaim);
        }
        Ok(())
    }
}

/// Test helper: sample receipt to lock the closed schema shape.
pub fn sample_storage_receipt() -> ManagedStorageStorageReceiptV1 {
    ManagedStorageStorageReceiptV1 {
        grant_id: OpaqueStorageGrantId::new("grant-sample".to_owned()),
        grant_hash: CanonicalHash::from_bytes([9u8; 32]),
        semantic_owner: sigil_kernel::resource::ManagedStorageSemanticOwnerV1::SessionLifecycleLog,
        capability_family: sigil_kernel::resource::ManagedStorageCapabilityFamilyV1::AppendLog,
        resource_id: sigil_kernel::resource::OpaqueResourceId::new("resource-sample".to_owned()),
        operation_digest: CanonicalHash::from_bytes([8u8; 32]),
        committed_sequence_or_version: Some(7),
        committed_frontier_hash: CanonicalHash::from_bytes([7u8; 32]),
        receipt_hash: CanonicalHash::from_bytes([6u8; 32]),
        physical_frontier_hash: None,
        physical_observation_record_hash: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant() -> StorageAdmissionGrantV1 {
        StorageAdmissionGrantV1 {
            grant_id: OpaqueStorageGrantId::new("grant-1".to_owned()),
            admission_hash: CanonicalHash::from_bytes([1u8; 32]),
            semantic_owner: sigil_kernel::resource::ManagedStorageSemanticOwnerV1::SessionLog,
            purpose: sigil_kernel::resource::ManagedStorageAdmissionPurposeV1::DurablePayload,
            purpose_hash: CanonicalHash::from_bytes([2u8; 32]),
            source_class:
                sigil_kernel::resource::StorageAdmissionSourceClassV1::ApplicationCutoverRoot,
            source_binding_hash: CanonicalHash::from_bytes([9u8; 32]),
            namespace_hash: CanonicalHash::from_bytes([3u8; 32]),
            journal_scope: sigil_kernel::resource::ResourceJournalScopeV1::Application,
            journal_scope_hash: CanonicalHash::from_bytes([4u8; 32]),
            resource_ref: sigil_kernel::resource::ResourceRefV1 {
                resource_id: sigil_kernel::resource::OpaqueResourceId::new("resource-1".to_owned()),
                kind: sigil_kernel::resource::ResourceKindV1::RuntimeState,
                owner_scope: sigil_kernel::resource::ResourceOwnerScopeV1::Application,
                journal_scope: sigil_kernel::resource::ResourceJournalScopeV1::Application,
                generation: 1,
            },
            resource_binding_digest: CanonicalHash::from_bytes([5u8; 32]),
            physical_binding_hash: CanonicalHash::from_bytes([6u8; 32]),
            resource_kind: sigil_kernel::resource::ResourceKindV1::RuntimeState,
            owner_scope: sigil_kernel::resource::ResourceOwnerScopeV1::Application,
            capability_family: sigil_kernel::resource::ManagedStorageCapabilityFamilyV1::AppendLog,
            retention_policy: sigil_kernel::resource::ResourceRetentionPolicyV1::SessionPolicy,
            quota_profile: sigil_kernel::resource::ResourceQuotaProfileV1 {
                class: sigil_kernel::resource::ResourceQuotaClassV1::RuntimeState,
                max_bytes: 1024,
                max_entries: 100,
                max_open_holders: 1,
                max_age_ms: None,
                hard_runtime_enforcement_required: true,
                profile_hash: CanonicalHash::from_bytes([7u8; 32]),
            },
            semantic_schema: sigil_kernel::resource::OpaqueSemanticSchemaId::new(
                "schema-1".to_owned(),
            ),
            authority_generation: AuthorityGeneration {
                epoch: 1,
                instance_hash: CanonicalHash::from_bytes([8u8; 32]),
            },
            journal_admission_sequence: 1,
            grant_hash: CanonicalHash::from_bytes([9u8; 32]),
        }
    }

    #[test]
    fn r71_storage_grant_table_rejects_duplicate_grant() {
        let mut table = AuthorityStorageGrantTableV1::new();
        table.register(grant()).expect("first");
        let error = table.register(grant()).expect_err("duplicate must fail");
        assert!(matches!(error, ManagedStorageErrorV1::CapabilityMismatch));
    }

    #[test]
    fn r71_storage_key_registry_rejects_duplicate_key_id() {
        let mut registry = AuthorityLogicalKeyRegistryV1::default();
        let key = OpaqueStorageKeyIdV1::new("key-1".to_owned());
        registry
            .reserve(
                key.clone(),
                sigil_kernel::resource::StorageLogicalKeyKindV1::Object,
            )
            .expect("first");
        let error = registry
            .reserve(key, sigil_kernel::resource::StorageLogicalKeyKindV1::Stream)
            .expect_err("duplicate");
        assert!(matches!(error, ManagedStorageErrorV1::DuplicateClaim));
    }

    #[test]
    fn r71_storage_receipt_shape_is_closed() {
        let receipt = sample_storage_receipt();
        assert_eq!(
            receipt.semantic_owner,
            sigil_kernel::resource::ManagedStorageSemanticOwnerV1::SessionLifecycleLog
        );
        assert_eq!(receipt.committed_sequence_or_version, Some(7));
    }

    fn storage_test_request() -> ManagedStorageAdmissionRequestV1 {
        ManagedStorageAdmissionRequestV1 {
            semantic_owner: sigil_kernel::resource::ManagedStorageSemanticOwnerV1::SessionLog,
            capability_family: sigil_kernel::resource::ManagedStorageCapabilityFamilyV1::AppendLog,
            purpose: sigil_kernel::resource::ManagedStorageAdmissionPurposeV1::DurablePayload,
            source:
                sigil_kernel::managed_storage::StorageAdmissionSourceV1::ApplicationCutoverRoot {
                    cutover_manifest_hash: CanonicalHash::from_bytes([9u8; 32]),
                    application_generation: 1,
                },
            owner_scope: sigil_kernel::resource::ResourceOwnerScopeV1::Application,
            journal_scope: sigil_kernel::resource::ResourceJournalScopeV1::Application,
        }
    }

    fn storage_test_header() -> crate::journal::ResourceJournalHeaderV1 {
        crate::journal::ResourceJournalHeaderV1 {
            schema_version: 1,
            shard_name: "application-resources".to_owned(),
            bootstrap_manifest_hash: CanonicalHash::from_bytes([1u8; 32]),
            journal_instance_hash: CanonicalHash::from_bytes([2u8; 32]),
            header_hash: super::hash_debug(&(
                "application-resources",
                CanonicalHash::from_bytes([1u8; 32]),
                CanonicalHash::from_bytes([2u8; 32]),
            )),
        }
    }

    #[test]
    fn r71_storage_rehydrates_pending_admission_requires_physical_bridge() {
        let directory = tempfile::tempdir().expect("journal directory");
        let path = directory.path().join("authority-resources.journal.json");
        let authority_generation = grant().authority_generation;
        let journal_header = storage_test_header();
        {
            let mut journal =
                crate::journal::ResourceJournalFileV1::open(&path, journal_header.clone())
                    .expect("journal");
            journal
                .append_event(
                    crate::journal::ResourceJournalEventV1::StorageNamespaceAdmitted {
                        grant_hash: grant().grant_hash,
                        handle_id: "rehydrated-handle".to_owned(),
                        namespace_hash: CanonicalHash::from_bytes([3u8; 32]),
                        grant: Box::new(grant()),
                        request: Box::new(storage_test_request()),
                    },
                )
                .expect("admission");
        }
        let mut table = AuthorityStorageGrantTableV1::new();
        table.register(grant()).expect("grant");
        let service = AuthorityManagedStorageServiceV1::new_with_journal(
            table,
            authority_generation,
            &path,
            journal_header.bootstrap_manifest_hash,
            journal_header.journal_instance_hash,
        )
        .expect("rehydrate");
        assert!(matches!(
            service.require_startup_reconciliation(),
            Err(ManagedStorageErrorV1::JournalUnavailable)
        ));
        assert!(matches!(
            service.reconcile_unsettled_storage_grants("recovered-cleanup"),
            Err(ManagedStorageErrorV1::JournalUnavailable)
        ));

        let mut reopened_table = AuthorityStorageGrantTableV1::new();
        reopened_table.register(grant()).expect("grant");
        let reopened = AuthorityManagedStorageServiceV1::new_with_journal(
            reopened_table,
            authority_generation,
            &path,
            journal_header.bootstrap_manifest_hash,
            journal_header.journal_instance_hash,
        )
        .expect("reopen");
        assert!(matches!(
            reopened.reconcile_unsettled_storage_grants("already-settled"),
            Err(ManagedStorageErrorV1::JournalUnavailable)
        ));
    }

    #[test]
    fn r71_storage_rehydrates_pending_grant_binding_mismatch_fail_closed() {
        let directory = tempfile::tempdir().expect("journal directory");
        let path = directory.path().join("authority-resources.journal.json");
        let header = storage_test_header();
        let historical = grant();
        {
            let mut journal = crate::journal::ResourceJournalFileV1::open(&path, header.clone())
                .expect("journal");
            journal
                .append_event(
                    crate::journal::ResourceJournalEventV1::StorageNamespaceAdmitted {
                        grant_hash: historical.grant_hash,
                        handle_id: "pending-binding-mismatch".to_owned(),
                        namespace_hash: CanonicalHash::from_bytes([3u8; 32]),
                        grant: Box::new(historical.clone()),
                        request: Box::new(storage_test_request()),
                    },
                )
                .expect("admission");
        }

        let mut current = historical.clone();
        current.source_binding_hash = CanonicalHash::from_bytes([0xabu8; 32]);
        current.grant_hash = CanonicalHash::from_bytes([0xacu8; 32]);
        let mut table = AuthorityStorageGrantTableV1::new();
        table.register(current).expect("current grant");
        let result = AuthorityManagedStorageServiceV1::new_with_journal(
            table,
            historical.authority_generation,
            &path,
            header.bootstrap_manifest_hash,
            header.journal_instance_hash,
        );
        let error = match result {
            Ok(_) => panic!("pending grant binding drift must remain blocked"),
            Err(error) => error,
        };
        assert!(
            matches!(error, JournalErrorV1::Corrupt(message) if message.contains("binding mismatch"))
        );
    }

    #[test]
    fn r71_storage_physical_bridge_replays_complete_seven_record_chain() {
        let directory = tempfile::tempdir().expect("journal directory");
        let path = directory.path().join("authority-resources.journal.json");
        let header = storage_test_header();
        let grant = grant();
        let namespace_hash = CanonicalHash::from_bytes([3u8; 32]);
        {
            let mut journal = crate::journal::ResourceJournalFileV1::open(&path, header.clone())
                .expect("journal");
            journal
                .append_event(
                    crate::journal::ResourceJournalEventV1::StorageNamespaceAdmitted {
                        grant_hash: grant.grant_hash,
                        handle_id: "rehydrated-handle".to_owned(),
                        namespace_hash,
                        grant: Box::new(grant.clone()),
                        request: Box::new(storage_test_request()),
                    },
                )
                .expect("admission");
        }
        let namespace = directory.path().join("managed/session-log");
        std::fs::create_dir_all(&namespace).expect("managed root");
        std::fs::write(
            namespace.join("authority-admission.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "handle_id": "rehydrated-handle",
                "namespace_hash": namespace_hash,
            }))
            .expect("marker json"),
        )
        .expect("marker");
        std::fs::write(namespace.join("records.jsonl"), b"{\"seq\":1}\n").expect("records");

        let mut table = AuthorityStorageGrantTableV1::new();
        table.register(grant.clone()).expect("grant");
        let service = AuthorityManagedStorageServiceV1::new_with_journal(
            table,
            grant.authority_generation,
            &path,
            header.bootstrap_manifest_hash,
            header.journal_instance_hash,
        )
        .expect("rehydrate");
        let receipts = service
            .reconcile_unsettled_storage_grants_with_physical_bridge()
            .expect("physical bridge");
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].committed_sequence_or_version, Some(8));
        service
            .require_startup_reconciliation()
            .expect("settlement clears startup blocker");

        let journal =
            crate::journal::ResourceJournalFileV1::open(&path, header).expect("reopen journal");
        assert_eq!(journal.storage_recovery_records(grant.grant_hash).len(), 7);
        assert_eq!(
            std::fs::read(namespace.join("records.jsonl")).expect("retained records"),
            b"{\"seq\":1}\n"
        );
    }

    #[test]
    fn r71_storage_quota_journal_reserves_reconciles_and_releases_owner() {
        use sigil_kernel::capability_issuer::KernelCapabilityBrokerV1;

        let directory = tempfile::tempdir().expect("journal directory");
        let path = directory.path().join("authority-resources.journal.json");
        let header = storage_test_header();
        let grant = grant();
        let mut table = AuthorityStorageGrantTableV1::new();
        table.register(grant.clone()).expect("grant");
        let service = AuthorityManagedStorageServiceV1::new_with_journal(
            table,
            grant.authority_generation,
            &path,
            header.bootstrap_manifest_hash,
            header.journal_instance_hash,
        )
        .expect("service");
        let broker = KernelCapabilityBrokerV1::new();
        let capability = broker
            .issue_storage_namespace_capability(
                broker.seal_storage_namespace_proof(grant.capability_family, grant.namespace_hash),
            )
            .expect("capability");
        let handle = service
            .admit_namespace(storage_test_request(), capability)
            .expect("admit");

        let namespace = directory.path().join("managed/session-log");
        std::fs::create_dir_all(&namespace).expect("managed root");
        std::fs::write(
            namespace.join("authority-admission.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "handle_id": handle.handle_id.as_str(),
                "namespace_hash": handle.namespace_hash,
            }))
            .expect("marker json"),
        )
        .expect("marker");
        let bytes = b"{}\n";
        std::fs::write(namespace.join("records.jsonl"), bytes).expect("records");
        let receipt = service
            .finalize_namespace_with_physical_frontier(
                handle,
                bytes.len() as u64,
                1,
                hash_bytes(bytes),
                "quota-test".to_owned(),
            )
            .expect("settle");
        assert!(receipt.physical_frontier_hash.is_some());

        let quota_bytes = std::fs::read(
            directory
                .path()
                .join(".authority-quota")
                .join("managed-storage.json"),
        )
        .expect("quota journal");
        let quota_text = String::from_utf8(quota_bytes).expect("quota json");
        assert!(quota_text.contains("storage:"));
        assert!(quota_text.contains("Released"));
    }

    #[test]
    fn r71_storage_physical_bridge_rejects_pending_without_marker() {
        let directory = tempfile::tempdir().expect("journal directory");
        let path = directory.path().join("authority-resources.journal.json");
        let header = storage_test_header();
        let grant = grant();
        {
            let mut journal = crate::journal::ResourceJournalFileV1::open(&path, header.clone())
                .expect("journal");
            journal
                .append_event(
                    crate::journal::ResourceJournalEventV1::StorageNamespaceAdmitted {
                        grant_hash: grant.grant_hash,
                        handle_id: "missing-marker".to_owned(),
                        namespace_hash: CanonicalHash::from_bytes([3u8; 32]),
                        grant: Box::new(grant.clone()),
                        request: Box::new(storage_test_request()),
                    },
                )
                .expect("admission");
        }
        let mut table = AuthorityStorageGrantTableV1::new();
        table.register(grant.clone()).expect("grant");
        let service = AuthorityManagedStorageServiceV1::new_with_journal(
            table,
            grant.authority_generation,
            &path,
            header.bootstrap_manifest_hash,
            header.journal_instance_hash,
        )
        .expect("rehydrate");
        assert!(matches!(
            service.reconcile_unsettled_storage_grants_with_physical_bridge(),
            Err(ManagedStorageErrorV1::JournalUnavailable)
        ));
    }

    #[test]
    fn r71_storage_finalize_preserves_admission_when_settlement_persist_fails() {
        let directory = tempfile::tempdir().expect("journal directory");
        let path = directory.path().join("authority-resources.journal.json");
        let header = storage_test_header();
        let mut table = AuthorityStorageGrantTableV1::new();
        table.register(grant()).expect("grant");
        let service = AuthorityManagedStorageServiceV1::new_with_journal(
            table,
            grant().authority_generation,
            &path,
            header.bootstrap_manifest_hash,
            header.journal_instance_hash,
        )
        .expect("service");
        let handle = service
            .admit_namespace(
                storage_test_request(),
                ValidatedStorageAdmissionCapabilityV1::startup_probe(),
            )
            .expect("admit");
        let original_path = path.clone();
        service
            .journal
            .as_ref()
            .expect("journal")
            .lock()
            .expect("journal lock")
            .set_path_for_test(directory.path().join("missing-parent").join("journal.json"));
        assert!(matches!(
            service.finalize_namespace(handle, "failed".to_owned()),
            Err(ManagedStorageErrorV1::JournalUnavailable)
        ));
        service
            .journal
            .as_ref()
            .expect("journal")
            .lock()
            .expect("journal lock")
            .set_path_for_test(original_path);
        let receipt = service
            .finalize_namespace(
                ManagedStorageNamespaceHandleV1::new(
                    sigil_kernel::resource::OpaqueKernelCapabilityHandleId::new(
                        "handle-probe-storage-1".to_owned(),
                    ),
                    CanonicalHash::from_bytes({
                        let mut namespace = [0x9fu8; 32];
                        namespace[0] = 1;
                        namespace[24..].copy_from_slice(&1u64.to_be_bytes());
                        namespace
                    }),
                    grant().capability_family,
                    OpaqueKernelCapabilityAuthenticatorV1::new("retry".to_owned()),
                ),
                "recovered".to_owned(),
            )
            .expect("retry finalize");
        assert!(receipt.committed_sequence_or_version.is_some());
    }

    #[test]
    fn r71_storage_family_exact_closure() {
        use sigil_kernel::managed_storage::{
            ManagedStorageAdmissionRequestV1, ValidatedStorageAdmissionCapabilityV1,
        };
        use sigil_kernel::resource::{
            ManagedStorageCapabilityFamilyV1, ManagedStorageSemanticOwnerV1, OpaqueSessionId,
            ResourceJournalScopeV1, ResourceOwnerScopeV1,
        };
        let mut table = AuthorityStorageGrantTableV1::new();
        table.register(grant()).expect("register");
        let service = AuthorityManagedStorageServiceV1::new(
            table,
            AuthorityGeneration {
                epoch: 1,
                instance_hash: CanonicalHash::from_bytes([8u8; 32]),
            },
        );
        let exact = ManagedStorageAdmissionRequestV1 {
            semantic_owner: ManagedStorageSemanticOwnerV1::SessionLog,
            capability_family: ManagedStorageCapabilityFamilyV1::AppendLog,
            purpose: sigil_kernel::resource::ManagedStorageAdmissionPurposeV1::DurablePayload,
            source:
                sigil_kernel::managed_storage::StorageAdmissionSourceV1::ApplicationCutoverRoot {
                    cutover_manifest_hash: CanonicalHash::from_bytes([9u8; 32]),
                    application_generation: 1,
                },
            owner_scope: ResourceOwnerScopeV1::Session(OpaqueSessionId::new("s-1".to_owned())),
            journal_scope: sigil_kernel::resource::ResourceJournalScopeV1::Application,
        };
        service
            .admit_namespace(
                exact,
                ValidatedStorageAdmissionCapabilityV1::startup_probe(),
            )
            .expect("exact family+owner");
        // Same family but different semantic owner: refused (a different writer must not piggyback).
        let piggyback = ManagedStorageAdmissionRequestV1 {
            semantic_owner: ManagedStorageSemanticOwnerV1::InteractiveInputHistory,
            capability_family: ManagedStorageCapabilityFamilyV1::AppendLog,
            purpose: sigil_kernel::resource::ManagedStorageAdmissionPurposeV1::DurablePayload,
            source:
                sigil_kernel::managed_storage::StorageAdmissionSourceV1::ApplicationCutoverRoot {
                    cutover_manifest_hash: CanonicalHash::from_bytes([9u8; 32]),
                    application_generation: 1,
                },
            owner_scope: ResourceOwnerScopeV1::Session(OpaqueSessionId::new("s-1".to_owned())),
            journal_scope: ResourceJournalScopeV1::Application,
        };
        let error = service
            .admit_namespace(
                piggyback,
                ValidatedStorageAdmissionCapabilityV1::startup_probe(),
            )
            .expect_err("piggyback");
        assert!(matches!(error, ManagedStorageErrorV1::FamilyMismatch));
        // Unrelated family: refused, never a masqueraded readiness.
        let unrelated = ManagedStorageAdmissionRequestV1 {
            semantic_owner: ManagedStorageSemanticOwnerV1::SessionCatalog,
            capability_family: ManagedStorageCapabilityFamilyV1::JournaledAtomicProjection,
            purpose:
                sigil_kernel::resource::ManagedStorageAdmissionPurposeV1::RebuildableProjection,
            source:
                sigil_kernel::managed_storage::StorageAdmissionSourceV1::ApplicationCutoverRoot {
                    cutover_manifest_hash: CanonicalHash::from_bytes([9u8; 32]),
                    application_generation: 1,
                },
            owner_scope: ResourceOwnerScopeV1::Session(OpaqueSessionId::new("s-1".to_owned())),
            journal_scope: ResourceJournalScopeV1::Application,
        };
        let error = service
            .admit_namespace(
                unrelated,
                ValidatedStorageAdmissionCapabilityV1::startup_probe(),
            )
            .expect_err("unrelated");
        assert!(matches!(error, ManagedStorageErrorV1::FamilyMismatch));
    }

    #[test]
    fn r71_storage_broker_binding_rejects_namespace_and_family_drift() {
        use sigil_kernel::capability_issuer::KernelCapabilityBrokerV1;
        use sigil_kernel::managed_storage::StorageAdmissionSourceV1;
        use sigil_kernel::resource::{
            ManagedStorageCapabilityFamilyV1, ManagedStorageSemanticOwnerV1,
        };
        let mut table = AuthorityStorageGrantTableV1::new();
        table.register(grant()).expect("register");
        let service = AuthorityManagedStorageServiceV1::new(
            table,
            AuthorityGeneration {
                epoch: 1,
                instance_hash: CanonicalHash::from_bytes([8u8; 32]),
            },
        );
        let request = ManagedStorageAdmissionRequestV1 {
            semantic_owner: ManagedStorageSemanticOwnerV1::SessionLog,
            capability_family: ManagedStorageCapabilityFamilyV1::AppendLog,
            purpose: sigil_kernel::resource::ManagedStorageAdmissionPurposeV1::DurablePayload,
            source: StorageAdmissionSourceV1::ApplicationCutoverRoot {
                cutover_manifest_hash: CanonicalHash::from_bytes([9u8; 32]),
                application_generation: 1,
            },
            owner_scope: sigil_kernel::resource::ResourceOwnerScopeV1::Application,
            journal_scope: sigil_kernel::resource::ResourceJournalScopeV1::Application,
        };
        let broker = KernelCapabilityBrokerV1::new();
        let exact = broker
            .issue_storage_namespace_capability(broker.seal_storage_namespace_proof(
                ManagedStorageCapabilityFamilyV1::AppendLog,
                CanonicalHash::from_bytes([3u8; 32]),
            ))
            .expect("exact capability");
        service
            .admit_namespace(request.clone(), exact)
            .expect("exact binding");
        let wrong_namespace = broker
            .issue_storage_namespace_capability(broker.seal_storage_namespace_proof(
                ManagedStorageCapabilityFamilyV1::AppendLog,
                CanonicalHash::from_bytes([4u8; 32]),
            ))
            .expect("wrong namespace capability");
        let error = service
            .admit_namespace(request.clone(), wrong_namespace)
            .expect_err("namespace drift");
        assert!(matches!(error, ManagedStorageErrorV1::CapabilityMismatch));
        let wrong_family = broker
            .issue_storage_namespace_capability(broker.seal_storage_namespace_proof(
                ManagedStorageCapabilityFamilyV1::AtomicObject,
                CanonicalHash::from_bytes([3u8; 32]),
            ))
            .expect("wrong family capability");
        let error = service
            .admit_namespace(request, wrong_family)
            .expect_err("family drift");
        assert!(matches!(error, ManagedStorageErrorV1::CapabilityMismatch));
    }
}
