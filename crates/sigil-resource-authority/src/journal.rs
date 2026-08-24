//! RFC-0071 section 10.4: private append-only resource journal (application/workspace shards).
//!
//! The journal is owner-only, single-writer and append-only. Its first writable fact may only
//! reference an already-verified bootstrap_manifest_hash; bootstrap proof and managed-resource
//! journal are two separate trust anchors (no self-allocation recursion).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

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
    },
    GenerationQuarantined {
        resource_id: String,
        generation: u64,
        quarantine_ref: String,
    },
}

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

    /// Returns grant identities with an admitted namespace that has no durable settlement yet.
    /// A restarted authority must not silently reuse such a physical namespace.
    pub fn unsettled_storage_grants(&self) -> std::collections::BTreeSet<String> {
        let mut pending = std::collections::BTreeSet::new();
        for durable in &self.records {
            match &durable.payload {
                ResourceJournalEventV1::StorageNamespaceAdmitted { grant_hash } => {
                    let key = grant_hash.to_hex();
                    pending.insert(key);
                }
                ResourceJournalEventV1::GenerationSettled { grant_hash, .. } => {
                    pending.remove(&grant_hash.to_hex());
                }
                _ => {}
            }
        }
        pending
    }

    /// Appends one event using the exact current tail as the precondition and persists it.
    pub fn append_event(
        &mut self,
        payload: ResourceJournalEventV1,
    ) -> Result<ResourceJournalRecordV1, JournalErrorV1> {
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
            self.records.pop();
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
        Ok(())
    }
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
                .append_event(ResourceJournalEventV1::StorageNamespaceAdmitted { grant_hash })
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
}
