//! RFC-0071 section 10.4: private append-only resource journal (application/workspace shards).
//!
//! The journal is owner-only, single-writer and append-only. Its first writable fact may only
//! reference an already-verified bootstrap_manifest_hash; bootstrap proof and managed-resource
//! journal are two separate trust anchors (no self-allocation recursion).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use sigil_kernel::managed_storage::{ManagedStorageAdmissionRequestV1, StorageAdmissionGrantV1};
use sigil_kernel::resource::{CanonicalHash, ResourceJournalScopeV1};

/// Journal header (frozen per shard instance).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceJournalHeaderV1 {
    pub schema_version: u32,
    pub shard_name: String,
    pub bootstrap_manifest_hash: CanonicalHash,
    pub journal_instance_hash: CanonicalHash,
    pub header_hash: CanonicalHash,
}

/// Append precondition: exact chain position.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResourceJournalAppendPreconditionV1 {
    Empty {
        expected_header_hash: CanonicalHash,
        expected_journal_instance_hash: CanonicalHash,
    },
    Existing {
        expected_sequence: u64,
        expected_record_hash: CanonicalHash,
        expected_journal_instance_hash: CanonicalHash,
    },
}

/// One append record (hash-chained).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceJournalRecordV1 {
    pub sequence: u64,
    pub previous_record_hash: CanonicalHash,
    pub payload_hash: CanonicalHash,
    pub record_hash: CanonicalHash,
    pub committed_frontier_hash: CanonicalHash,
}

/// Bounded journal event set (closed variants; first sequence is 1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResourceJournalEventV1 {
    BootstrapBound {
        bootstrap_manifest_hash: CanonicalHash,
    },
    StorageNamespaceAdmitted {
        grant_hash: CanonicalHash,
        handle_id: String,
        namespace_hash: CanonicalHash,
        grant: Box<StorageAdmissionGrantV1>,
        request: Box<ManagedStorageAdmissionRequestV1>,
    },
    GenerationReserved {
        resource_id: String,
        generation: u64,
    },
    GenerationActivated {
        resource_id: String,
        generation: u64,
        manifest_hash: CanonicalHash,
    },
    GenerationSettled {
        grant_hash: CanonicalHash,
        resource_id: String,
        generation: u64,
        cleanup_status: String,
        #[serde(default)]
        physical_frontier_hash: Option<CanonicalHash>,
        #[serde(default)]
        physical_observation_record_hash: Option<CanonicalHash>,
    },
    GenerationQuarantined {
        resource_id: String,
        generation: u64,
        quarantine_ref: String,
    },
    /// Physical writer frontier observed before a normal settlement. This is deliberately
    /// separate from `GenerationSettled`: the latter is the authority lifecycle fact, while
    /// this record binds it to the bytes that were actually observed on disk.
    DomainStoragePhysicalFrontierObserved {
        grant_hash: CanonicalHash,
        namespace_hash: CanonicalHash,
        byte_length: u64,
        record_count: u64,
        content_hash: CanonicalHash,
        frontier_hash: CanonicalHash,
    },
    /// RFC-0071 section 22.4: domain-storage failure/recovery chain, record 1.
    DomainStorageFailureObserved {
        grant_hash: CanonicalHash,
        namespace_hash: CanonicalHash,
        raised_envelope: Vec<u8>,
        physical_frontier_hash: CanonicalHash,
        request_hash: CanonicalHash,
    },
    /// RFC-0071 section 22.4: domain-storage failure/recovery chain, record 2.
    DomainStorageResolutionStartedShadow {
        grant_hash: CanonicalHash,
        observed_record_hash: CanonicalHash,
        action_token_hash: CanonicalHash,
        started_envelope: Vec<u8>,
        request_hash: CanonicalHash,
    },
    /// RFC-0071 section 22.4: generic recovery Prepared record 3.
    RecoveryOperationPrepared {
        grant_hash: CanonicalHash,
        recovery_operation_id: String,
        authorized_operation: Vec<u8>,
        target_frontier_hash: CanonicalHash,
    },
    /// RFC-0071 section 22.4: domain-storage bridge Prepared record 4.
    DomainStorageResolutionPrepared {
        grant_hash: CanonicalHash,
        started_shadow_record_hash: CanonicalHash,
        recovery_prepared_record_hash: CanonicalHash,
        bridge_frontier_hash: CanonicalHash,
    },
    /// RFC-0071 section 22.4: generic recovery Settled record 5.
    RecoveryOperationSettled {
        grant_hash: CanonicalHash,
        recovery_operation_id: String,
        repair_receipt: Vec<u8>,
        settled_frontier_hash: CanonicalHash,
    },
    /// RFC-0071 section 22.4: domain-storage bridge Settled record 6.
    DomainStorageResolutionSettled {
        grant_hash: CanonicalHash,
        resolution_prepared_record_hash: CanonicalHash,
        recovery_settled_record_hash: CanonicalHash,
        receipt_event: Vec<u8>,
        terminal_or_successor_event: Vec<u8>,
    },
    /// RFC-0071 section 22.4: domain blocker projection record 7.
    DomainBlockerProjected {
        grant_hash: CanonicalHash,
        resolution_settled_record_hash: CanonicalHash,
        projected_event_ids_hash: CanonicalHash,
        final_frontier_hash: CanonicalHash,
        projected_event: Vec<u8>,
    },
}

/// Maximum opaque envelope size accepted by the domain-storage recovery bridge.
pub const MAX_DOMAIN_STORAGE_RECOVERY_ENVELOPE_BYTES: usize = 64 * 1024;

/// Closed journal error classification.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JournalErrorV1 {
    #[error("journal append precondition mismatch (chain drift or duplicate sequence)")]
    PreconditionMismatch,
    #[error("journal record is not hash-chained to the previous record")]
    HashChainBroken,
    #[error("journal instance does not match the frozen header")]
    InstanceMismatch,
    #[error("first record must reference the verified bootstrap manifest hash")]
    FirstRecordNotBootstrapBound,
    #[error("journal range is full: emergency reserve must be consumed")]
    JournalFull,
    #[error("durable journal filesystem operation failed: {0}")]
    Filesystem(String),
    #[error("durable journal payload is corrupt: {0}")]
    Corrupt(String),
    #[error("durable journal commit is uncertain after rename: {0}")]
    DurabilityUncertain(String),
}

/// In-memory deterministic journal (testing of append protocol before durable file backing).
#[derive(Debug, Default)]
pub struct ResourceJournalMemoryV1 {
    header: Option<ResourceJournalHeaderV1>,
    records: BTreeMap<u64, ResourceJournalRecordV1>,
}

impl ResourceJournalMemoryV1 {
    pub const fn new() -> Self {
        Self {
            header: None,
            records: BTreeMap::new(),
        }
    }

    pub fn install_header(
        &mut self,
        header: ResourceJournalHeaderV1,
    ) -> Result<(), JournalErrorV1> {
        if self.header.is_some() {
            return Err(JournalErrorV1::InstanceMismatch);
        }
        self.header = Some(header);
        Ok(())
    }

    pub fn header(&self) -> Option<&ResourceJournalHeaderV1> {
        self.header.as_ref()
    }

    pub fn tail(&self) -> Option<&ResourceJournalRecordV1> {
        self.records.values().last()
    }

    /// Appends one record after verifying the exact precondition and hash chain.
    pub fn append(
        &mut self,
        precondition: &ResourceJournalAppendPreconditionV1,
        payload: &ResourceJournalEventV1,
    ) -> Result<ResourceJournalRecordV1, JournalErrorV1> {
        let header = self
            .header
            .as_ref()
            .ok_or(JournalErrorV1::InstanceMismatch)?;
        let next_sequence = self.records.len() as u64 + 1;
        match precondition {
            ResourceJournalAppendPreconditionV1::Empty {
                expected_header_hash,
                expected_journal_instance_hash,
            } => {
                if !self.records.is_empty() {
                    return Err(JournalErrorV1::PreconditionMismatch);
                }
                if *expected_header_hash != header.header_hash {
                    return Err(JournalErrorV1::PreconditionMismatch);
                }
                if *expected_journal_instance_hash != header.journal_instance_hash {
                    return Err(JournalErrorV1::InstanceMismatch);
                }
                let ResourceJournalEventV1::BootstrapBound {
                    bootstrap_manifest_hash,
                } = payload
                else {
                    return Err(JournalErrorV1::FirstRecordNotBootstrapBound);
                };
                if *bootstrap_manifest_hash != header.bootstrap_manifest_hash {
                    return Err(JournalErrorV1::FirstRecordNotBootstrapBound);
                }
            }
            ResourceJournalAppendPreconditionV1::Existing {
                expected_sequence,
                expected_record_hash,
                expected_journal_instance_hash,
            } => {
                let tail = self.tail().ok_or(JournalErrorV1::PreconditionMismatch)?;
                if *expected_sequence != tail.sequence || *expected_record_hash != tail.record_hash
                {
                    return Err(JournalErrorV1::PreconditionMismatch);
                }
                if *expected_journal_instance_hash != header.journal_instance_hash {
                    return Err(JournalErrorV1::InstanceMismatch);
                }
                if next_sequence != tail.sequence + 1 {
                    return Err(JournalErrorV1::PreconditionMismatch);
                }
            }
        }
        let payload_hash = journal_encode(payload);
        let previous_record_hash = self
            .tail()
            .map(|tail| tail.record_hash)
            .unwrap_or(header.header_hash);
        let record_hash = journal_encode(&(next_sequence, previous_record_hash, payload_hash));
        let committed = journal_encode(&(record_hash, next_sequence));
        let record = ResourceJournalRecordV1 {
            sequence: next_sequence,
            previous_record_hash,
            payload_hash,
            record_hash,
            committed_frontier_hash: committed,
        };
        self.records.insert(next_sequence, record.clone());
        Ok(record)
    }

    fn rollback_tail(&mut self, record: &ResourceJournalRecordV1) {
        if self
            .records
            .get(&record.sequence)
            .is_some_and(|current| current == record)
        {
            self.records.remove(&record.sequence);
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DurableJournalRecordV1 {
    record: ResourceJournalRecordV1,
    payload: ResourceJournalEventV1,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DurableJournalSnapshotV1 {
    header: ResourceJournalHeaderV1,
    records: Vec<DurableJournalRecordV1>,
}

/// Owner-only file-backed journal used by production authority composition.
///
/// The in-memory journal remains the protocol reference and this wrapper persists the exact
/// event plus hash-chain record after every append. Writes use a same-directory owner-only
/// temporary file, `sync_all`, and rename, so a process crash exposes either the previous valid
/// snapshot or the next complete valid snapshot; a partially written snapshot is never adopted.
pub struct ResourceJournalFileV1 {
    path: PathBuf,
    journal: ResourceJournalMemoryV1,
    records: Vec<DurableJournalRecordV1>,
    poisoned: bool,
}

impl std::fmt::Debug for ResourceJournalFileV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ResourceJournalFileV1")
            .field("path", &"[private]")
            .field(
                "sequence",
                &self.journal.tail().map(|record| record.sequence),
            )
            .finish()
    }
}

impl ResourceJournalFileV1 {
    /// Opens or creates one frozen journal instance and ensures its bootstrap-bound genesis.
    pub fn open(
        path: impl Into<PathBuf>,
        header: ResourceJournalHeaderV1,
    ) -> Result<Self, JournalErrorV1> {
        let path = path.into();
        let parent = path.parent().ok_or_else(|| {
            JournalErrorV1::Filesystem("journal has no parent directory".to_owned())
        })?;
        fs::create_dir_all(parent).map_err(map_io)?;
        harden_directory(parent)?;

        if path.exists() {
            let metadata = fs::symlink_metadata(&path).map_err(map_io)?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(JournalErrorV1::Corrupt(
                    "journal path is not a regular file".to_owned(),
                ));
            }
            let bytes = fs::read(&path).map_err(map_io)?;
            let snapshot: DurableJournalSnapshotV1 = serde_json::from_slice(&bytes)
                .map_err(|error| JournalErrorV1::Corrupt(error.to_string()))?;
            if snapshot.header != header {
                return Err(JournalErrorV1::InstanceMismatch);
            }
            let (journal, records) = replay_snapshot(&snapshot)?;
            let mut durable = Self {
                path,
                journal,
                records,
                poisoned: false,
            };
            if durable.records.is_empty() {
                durable.append_event(ResourceJournalEventV1::BootstrapBound {
                    bootstrap_manifest_hash: header.bootstrap_manifest_hash,
                })?;
            }
            Ok(durable)
        } else {
            let mut journal = ResourceJournalMemoryV1::new();
            journal.install_header(header.clone())?;
            let mut durable = Self {
                path,
                journal,
                records: Vec::new(),
                poisoned: false,
            };
            durable.append_event(ResourceJournalEventV1::BootstrapBound {
                bootstrap_manifest_hash: header.bootstrap_manifest_hash,
            })?;
            Ok(durable)
        }
    }

    pub fn header(&self) -> Option<&ResourceJournalHeaderV1> {
        self.journal.header()
    }

    pub fn tail(&self) -> Option<&ResourceJournalRecordV1> {
        self.journal.tail()
    }

    #[cfg(test)]
    pub(crate) fn set_path_for_test(&mut self, path: PathBuf) {
        self.path = path;
    }

    /// Returns grant identities with an admitted namespace that has no durable settlement yet.
    /// A restarted authority must not silently reuse such a physical namespace.
    pub fn unsettled_storage_grants(&self) -> std::collections::BTreeSet<String> {
        let mut pending = std::collections::BTreeSet::new();
        for durable in &self.records {
            match &durable.payload {
                ResourceJournalEventV1::StorageNamespaceAdmitted { grant_hash, .. } => {
                    let key = grant_hash.to_hex();
                    pending.insert(key);
                }
                ResourceJournalEventV1::GenerationSettled { grant_hash, .. } => {
                    pending.remove(&grant_hash.to_hex());
                }
                ResourceJournalEventV1::RecoveryOperationSettled { grant_hash, .. } => {
                    pending.remove(&grant_hash.to_hex());
                }
                _ => {}
            }
        }
        pending
    }

    /// Replays every storage admission and its settlement frontier for authority startup.
    pub fn storage_admission_state(
        &self,
    ) -> (
        Vec<ResourceJournalStorageAdmissionV1>,
        std::collections::BTreeSet<String>,
    ) {
        let mut admissions = Vec::new();
        let mut settled = std::collections::BTreeSet::new();
        for durable in &self.records {
            match &durable.payload {
                ResourceJournalEventV1::StorageNamespaceAdmitted {
                    grant_hash,
                    handle_id,
                    namespace_hash,
                    grant,
                    request,
                } => admissions.push(ResourceJournalStorageAdmissionV1 {
                    admission_sequence: durable.record.sequence,
                    grant_hash: *grant_hash,
                    handle_id: handle_id.clone(),
                    namespace_hash: *namespace_hash,
                    grant: (**grant).clone(),
                    request: (**request).clone(),
                }),
                ResourceJournalEventV1::GenerationSettled { grant_hash, .. } => {
                    settled.insert(grant_hash.to_hex());
                }
                ResourceJournalEventV1::RecoveryOperationSettled { grant_hash, .. } => {
                    settled.insert(grant_hash.to_hex());
                }
                _ => {}
            }
        }
        (admissions, settled)
    }

    pub fn settled_storage_record(
        &self,
        grant_hash: CanonicalHash,
    ) -> Option<ResourceJournalRecordV1> {
        self.records
            .iter()
            .rev()
            .find_map(|durable| {
                matches!(
                    durable.payload,
                    ResourceJournalEventV1::GenerationSettled {
                        grant_hash: settled_hash,
                        ..
                    } if settled_hash == grant_hash
                )
                .then_some(durable.record.clone())
            })
            .or_else(|| {
                self.records.iter().rev().find_map(|durable| {
                    matches!(
                        durable.payload,
                        ResourceJournalEventV1::RecoveryOperationSettled {
                            grant_hash: settled_hash,
                            ..
                        } if settled_hash == grant_hash
                    )
                    .then_some(durable.record.clone())
                })
            })
    }

    /// Returns every physical frontier observation for one exact admitted grant/namespace.
    /// Callers must compare all returned facts; a different second observation is evidence of
    /// physical drift rather than a new valid frontier for the same one-shot namespace.
    pub fn storage_physical_frontier_records(
        &self,
        grant_hash: CanonicalHash,
        namespace_hash: CanonicalHash,
    ) -> Vec<(
        ResourceJournalRecordV1,
        u64,
        u64,
        CanonicalHash,
        CanonicalHash,
    )> {
        self.records
            .iter()
            .filter_map(|durable| match &durable.payload {
                ResourceJournalEventV1::DomainStoragePhysicalFrontierObserved {
                    grant_hash: current_grant,
                    namespace_hash: current_namespace,
                    byte_length,
                    record_count,
                    content_hash,
                    frontier_hash,
                } if *current_grant == grant_hash && *current_namespace == namespace_hash => {
                    Some((
                        durable.record.clone(),
                        *byte_length,
                        *record_count,
                        *content_hash,
                        *frontier_hash,
                    ))
                }
                _ => None,
            })
            .collect()
    }

    /// Returns the physical binding carried by the terminal normal settlement, if one exists.
    pub fn storage_settlement_binding(
        &self,
        grant_hash: CanonicalHash,
    ) -> Option<(Option<CanonicalHash>, Option<CanonicalHash>)> {
        self.records
            .iter()
            .rev()
            .find_map(|durable| match &durable.payload {
                ResourceJournalEventV1::GenerationSettled {
                    grant_hash: current,
                    physical_frontier_hash,
                    physical_observation_record_hash,
                    ..
                } if *current == grant_hash => {
                    Some((*physical_frontier_hash, *physical_observation_record_hash))
                }
                _ => None,
            })
    }

    /// Returns the durable domain-storage recovery prefix for one grant. The caller must
    /// compare the prefix against physical evidence before appending the next state; a table
    /// miss is intentionally represented as an empty prefix, never as proof of no effect.
    pub fn storage_recovery_records(
        &self,
        grant_hash: CanonicalHash,
    ) -> Vec<(ResourceJournalRecordV1, ResourceJournalEventV1)> {
        self.records
            .iter()
            .filter(|durable| match &durable.payload {
                ResourceJournalEventV1::DomainStorageFailureObserved {
                    grant_hash: current,
                    ..
                }
                | ResourceJournalEventV1::DomainStorageResolutionStartedShadow {
                    grant_hash: current,
                    ..
                }
                | ResourceJournalEventV1::RecoveryOperationPrepared {
                    grant_hash: current,
                    ..
                }
                | ResourceJournalEventV1::DomainStorageResolutionPrepared {
                    grant_hash: current,
                    ..
                }
                | ResourceJournalEventV1::RecoveryOperationSettled {
                    grant_hash: current,
                    ..
                }
                | ResourceJournalEventV1::DomainStorageResolutionSettled {
                    grant_hash: current,
                    ..
                }
                | ResourceJournalEventV1::DomainBlockerProjected {
                    grant_hash: current,
                    ..
                } => *current == grant_hash,
                _ => false,
            })
            .map(|durable| (durable.record.clone(), durable.payload.clone()))
            .collect()
    }

    /// Appends one event using the exact current tail as the precondition and persists it.
    pub fn append_event(
        &mut self,
        payload: ResourceJournalEventV1,
    ) -> Result<ResourceJournalRecordV1, JournalErrorV1> {
        if self.poisoned {
            return Err(JournalErrorV1::DurabilityUncertain(
                "journal is poisoned after a prior post-rename durability failure".to_owned(),
            ));
        }
        let precondition = match self.journal.tail() {
            Some(tail) => ResourceJournalAppendPreconditionV1::Existing {
                expected_sequence: tail.sequence,
                expected_record_hash: tail.record_hash,
                expected_journal_instance_hash: self
                    .journal
                    .header()
                    .ok_or(JournalErrorV1::InstanceMismatch)?
                    .journal_instance_hash,
            },
            None => {
                let header = self
                    .journal
                    .header()
                    .ok_or(JournalErrorV1::InstanceMismatch)?;
                ResourceJournalAppendPreconditionV1::Empty {
                    expected_header_hash: header.header_hash,
                    expected_journal_instance_hash: header.journal_instance_hash,
                }
            }
        };
        let record = self.journal.append(&precondition, &payload)?;
        self.records.push(DurableJournalRecordV1 {
            record: record.clone(),
            payload,
        });
        if let Err(error) = self.persist() {
            if matches!(error, JournalErrorV1::DurabilityUncertain(_)) {
                self.poisoned = true;
            } else {
                self.records.pop();
                self.journal.rollback_tail(&record);
            }
            return Err(error);
        }
        Ok(record)
    }

    fn persist(&self) -> Result<(), JournalErrorV1> {
        let snapshot = DurableJournalSnapshotV1 {
            header: self
                .journal
                .header()
                .ok_or(JournalErrorV1::InstanceMismatch)?
                .clone(),
            records: self.records.clone(),
        };
        let parent = self.path.parent().ok_or_else(|| {
            JournalErrorV1::Filesystem("journal has no parent directory".to_owned())
        })?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(map_io)?;
        harden_file(temporary.path())?;
        serde_json::to_writer(&mut temporary, &snapshot)
            .map_err(|error| JournalErrorV1::Corrupt(error.to_string()))?;
        temporary.as_file().sync_all().map_err(map_io)?;
        temporary
            .persist(&self.path)
            .map_err(|error| map_io(error.error))?;
        sync_parent_directory(parent).map_err(|error| {
            JournalErrorV1::DurabilityUncertain(format!(
                "journal snapshot was renamed but parent directory sync failed: {error}"
            ))
        })?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceJournalStorageAdmissionV1 {
    pub admission_sequence: u64,
    pub grant_hash: CanonicalHash,
    pub handle_id: String,
    pub namespace_hash: CanonicalHash,
    pub grant: StorageAdmissionGrantV1,
    pub request: ManagedStorageAdmissionRequestV1,
}

#[cfg(unix)]
fn sync_parent_directory(parent: &Path) -> std::io::Result<()> {
    fs::File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent_directory(_parent: &Path) -> std::io::Result<()> {
    Ok(())
}

fn replay_snapshot(
    snapshot: &DurableJournalSnapshotV1,
) -> Result<(ResourceJournalMemoryV1, Vec<DurableJournalRecordV1>), JournalErrorV1> {
    let mut journal = ResourceJournalMemoryV1::new();
    journal.install_header(snapshot.header.clone())?;
    for durable in &snapshot.records {
        let precondition = match journal.tail() {
            Some(tail) => ResourceJournalAppendPreconditionV1::Existing {
                expected_sequence: tail.sequence,
                expected_record_hash: tail.record_hash,
                expected_journal_instance_hash: snapshot.header.journal_instance_hash,
            },
            None => ResourceJournalAppendPreconditionV1::Empty {
                expected_header_hash: snapshot.header.header_hash,
                expected_journal_instance_hash: snapshot.header.journal_instance_hash,
            },
        };
        let expected = journal.append(&precondition, &durable.payload)?;
        if expected != durable.record {
            return Err(JournalErrorV1::HashChainBroken);
        }
    }
    Ok((journal, snapshot.records.clone()))
}

fn map_io(error: std::io::Error) -> JournalErrorV1 {
    JournalErrorV1::Filesystem(error.to_string())
}

fn harden_directory(path: &Path) -> Result<(), JournalErrorV1> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(map_io)?;
    }
    Ok(())
}

fn harden_file(path: &Path) -> Result<(), JournalErrorV1> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(map_io)?;
    }
    Ok(())
}

/// Deterministic canonical encoding used for hashes (no path separators involved).
fn journal_encode(payload: &impl std::fmt::Debug) -> CanonicalHash {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(format!("{payload:?}").as_bytes());
    CanonicalHash::from_bytes(hasher.finalize().into())
}

/// Closed journal scope class for diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalScopeKindV1 {
    Application,
    Workspace,
}

impl JournalScopeKindV1 {
    pub const fn classify(scope: &ResourceJournalScopeV1) -> Self {
        match scope {
            ResourceJournalScopeV1::Application => Self::Application,
            ResourceJournalScopeV1::Workspace(_) => Self::Workspace,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header() -> ResourceJournalHeaderV1 {
        let instance = journal_encode(b"instance-1");
        ResourceJournalHeaderV1 {
            schema_version: 1,
            shard_name: "application-resources".to_owned(),
            bootstrap_manifest_hash: journal_encode(b"manifest-1"),
            journal_instance_hash: instance,
            header_hash: journal_encode(b"header-1"),
        }
    }

    #[test]
    fn r71_journal_first_record_must_be_bootstrap_bound() {
        let mut journal = ResourceJournalMemoryV1::new();
        journal.install_header(header()).expect("header");
        let precondition = ResourceJournalAppendPreconditionV1::Empty {
            expected_header_hash: journal.header().expect("h").header_hash,
            expected_journal_instance_hash: journal.header().expect("h").journal_instance_hash,
        };
        let error = journal
            .append(
                &precondition,
                &ResourceJournalEventV1::GenerationReserved {
                    resource_id: "r".to_owned(),
                    generation: 1,
                },
            )
            .expect_err("must reject non-bootstrap first record");
        assert!(matches!(
            error,
            JournalErrorV1::FirstRecordNotBootstrapBound
        ));
    }

    #[test]
    fn r71_journal_genesis_sequence_is_one_and_chain_is_unique() {
        let mut journal = ResourceJournalMemoryV1::new();
        journal.install_header(header()).expect("header");
        let h = journal.header().expect("h").clone();
        let first = journal
            .append(
                &ResourceJournalAppendPreconditionV1::Empty {
                    expected_header_hash: h.header_hash,
                    expected_journal_instance_hash: h.journal_instance_hash,
                },
                &ResourceJournalEventV1::BootstrapBound {
                    bootstrap_manifest_hash: h.bootstrap_manifest_hash,
                },
            )
            .expect("genesis");
        assert_eq!(first.sequence, 1);
        // Duplicate genesis precondition fails.
        let error = journal
            .append(
                &ResourceJournalAppendPreconditionV1::Empty {
                    expected_header_hash: h.header_hash,
                    expected_journal_instance_hash: h.journal_instance_hash,
                },
                &ResourceJournalEventV1::BootstrapBound {
                    bootstrap_manifest_hash: h.bootstrap_manifest_hash,
                },
            )
            .expect_err("duplicate genesis must fail");
        assert!(matches!(error, JournalErrorV1::PreconditionMismatch));
    }

    #[test]
    fn r71_journal_chain_must_match_exact_tail() {
        let mut journal = ResourceJournalMemoryV1::new();
        journal.install_header(header()).expect("header");
        let h = journal.header().expect("h").clone();
        journal
            .append(
                &ResourceJournalAppendPreconditionV1::Empty {
                    expected_header_hash: h.header_hash,
                    expected_journal_instance_hash: h.journal_instance_hash,
                },
                &ResourceJournalEventV1::BootstrapBound {
                    bootstrap_manifest_hash: h.bootstrap_manifest_hash,
                },
            )
            .expect("genesis");
        // Wrong expected tail is rejected.
        let error = journal
            .append(
                &ResourceJournalAppendPreconditionV1::Existing {
                    expected_sequence: 999,
                    expected_record_hash: journal_encode(b"wrong"),
                    expected_journal_instance_hash: h.journal_instance_hash,
                },
                &ResourceJournalEventV1::GenerationReserved {
                    resource_id: "r".to_owned(),
                    generation: 1,
                },
            )
            .expect_err("wrong precondition must fail");
        assert!(matches!(error, JournalErrorV1::PreconditionMismatch));
    }

    #[test]
    fn r71_durable_journal_replays_after_process_restart_and_rejects_corruption() {
        let directory = tempfile::tempdir().expect("journal directory");
        let path = directory.path().join("authority.journal.json");
        let h = header();
        let grant_hash = journal_encode(b"grant-1");
        {
            let mut journal = ResourceJournalFileV1::open(&path, h.clone()).expect("create");
            let record = journal
                .append_event(ResourceJournalEventV1::StorageNamespaceAdmitted {
                    grant_hash,
                    handle_id: "handle-1".to_owned(),
                    namespace_hash: journal_encode(b"namespace-1"),
                    grant: Box::new(sample_grant()),
                    request: Box::new(sample_request()),
                })
                .expect("append admission");
            assert_eq!(record.sequence, 2, "genesis is persisted before admissions");
        }
        let reopened = ResourceJournalFileV1::open(&path, h.clone()).expect("replay");
        assert_eq!(reopened.tail().expect("tail").sequence, 2);
        assert_eq!(reopened.header().expect("header"), &h);
        assert!(
            reopened
                .unsettled_storage_grants()
                .contains(&grant_hash.to_hex())
        );

        std::fs::write(&path, b"truncated").expect("corrupt journal");
        let error = ResourceJournalFileV1::open(&path, h).expect_err("corruption fails closed");
        assert!(matches!(error, JournalErrorV1::Corrupt(_)));
    }

    fn sample_grant() -> StorageAdmissionGrantV1 {
        StorageAdmissionGrantV1 {
            grant_id: sigil_kernel::resource::OpaqueStorageGrantId::new("journal-grant".to_owned()),
            admission_hash: journal_encode(b"admission"),
            semantic_owner: sigil_kernel::resource::ManagedStorageSemanticOwnerV1::SessionLog,
            purpose: sigil_kernel::resource::ManagedStorageAdmissionPurposeV1::DurablePayload,
            purpose_hash: journal_encode(b"purpose"),
            source_class:
                sigil_kernel::resource::StorageAdmissionSourceClassV1::ApplicationCutoverRoot,
            source_binding_hash: journal_encode(b"source"),
            namespace_hash: journal_encode(b"namespace-1"),
            journal_scope: ResourceJournalScopeV1::Application,
            journal_scope_hash: journal_encode(b"scope"),
            resource_ref: sigil_kernel::resource::ResourceRefV1 {
                resource_id: sigil_kernel::resource::OpaqueResourceId::new("resource".to_owned()),
                kind: sigil_kernel::resource::ResourceKindV1::RuntimeState,
                owner_scope: sigil_kernel::resource::ResourceOwnerScopeV1::Application,
                journal_scope: ResourceJournalScopeV1::Application,
                generation: 1,
            },
            resource_binding_digest: journal_encode(b"resource-binding"),
            physical_binding_hash: journal_encode(b"physical"),
            resource_kind: sigil_kernel::resource::ResourceKindV1::RuntimeState,
            owner_scope: sigil_kernel::resource::ResourceOwnerScopeV1::Application,
            capability_family: sigil_kernel::resource::ManagedStorageCapabilityFamilyV1::AppendLog,
            retention_policy: sigil_kernel::resource::ResourceRetentionPolicyV1::SessionPolicy,
            quota_profile: sigil_kernel::resource::ResourceQuotaProfileV1 {
                class: sigil_kernel::resource::ResourceQuotaClassV1::RuntimeState,
                max_bytes: 1024,
                max_entries: 8,
                max_open_holders: 1,
                max_age_ms: None,
                hard_runtime_enforcement_required: true,
                profile_hash: journal_encode(b"quota"),
            },
            semantic_schema: sigil_kernel::resource::OpaqueSemanticSchemaId::new(
                "schema".to_owned(),
            ),
            authority_generation: sigil_kernel::resource::AuthorityGeneration {
                epoch: 1,
                instance_hash: journal_encode(b"authority"),
            },
            journal_admission_sequence: 1,
            grant_hash: journal_encode(b"grant-hash"),
        }
    }

    fn sample_request() -> ManagedStorageAdmissionRequestV1 {
        ManagedStorageAdmissionRequestV1 {
            semantic_owner: sigil_kernel::resource::ManagedStorageSemanticOwnerV1::SessionLog,
            capability_family: sigil_kernel::resource::ManagedStorageCapabilityFamilyV1::AppendLog,
            purpose: sigil_kernel::resource::ManagedStorageAdmissionPurposeV1::DurablePayload,
            source:
                sigil_kernel::managed_storage::StorageAdmissionSourceV1::ApplicationCutoverRoot {
                    cutover_manifest_hash: journal_encode(b"source"),
                    application_generation: 1,
                },
            owner_scope: sigil_kernel::resource::ResourceOwnerScopeV1::Application,
            journal_scope: ResourceJournalScopeV1::Application,
        }
    }

    #[test]
    fn r71_durable_journal_rolls_back_memory_after_pre_rename_persist_failure() {
        let directory = tempfile::tempdir().expect("journal directory");
        let path = directory.path().join("authority.journal.json");
        let mut journal = ResourceJournalFileV1::open(path.clone(), header()).expect("create");
        journal.path = directory.path().join("missing-parent").join("journal.json");
        let error = journal
            .append_event(ResourceJournalEventV1::GenerationReserved {
                resource_id: "resource".to_owned(),
                generation: 2,
            })
            .expect_err("persist failure");
        assert!(matches!(error, JournalErrorV1::Filesystem(_)));
        assert_eq!(journal.tail().expect("genesis").sequence, 1);
        assert_eq!(journal.records.len(), 1);

        journal.path = path.clone();
        journal
            .append_event(ResourceJournalEventV1::GenerationReserved {
                resource_id: "resource".to_owned(),
                generation: 3,
            })
            .expect("retry after rollback");
        let reopened = ResourceJournalFileV1::open(path, header()).expect("reopen");
        assert_eq!(reopened.tail().expect("tail").sequence, 2);
    }
}
