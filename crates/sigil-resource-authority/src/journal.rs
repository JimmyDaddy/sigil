//! RFC-0071 section 10.4: private append-only resource journal (application/workspace shards).
//!
//! The journal is owner-only, single-writer and append-only. Its first writable fact may only
//! reference an already-verified bootstrap_manifest_hash; bootstrap proof and managed-resource
//! journal are two separate trust anchors (no self-allocation recursion).

use std::collections::BTreeMap;

use sigil_kernel::resource::{CanonicalHash, ResourceJournalScopeV1};

/// Journal header (frozen per shard instance).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceJournalHeaderV1 {
    pub schema_version: u32,
    pub shard_name: String,
    pub bootstrap_manifest_hash: CanonicalHash,
    pub journal_instance_hash: CanonicalHash,
    pub header_hash: CanonicalHash,
}

/// Append precondition: exact chain position.
#[derive(Debug, Clone, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceJournalRecordV1 {
    pub sequence: u64,
    pub previous_record_hash: CanonicalHash,
    pub payload_hash: CanonicalHash,
    pub record_hash: CanonicalHash,
    pub committed_frontier_hash: CanonicalHash,
}

/// Bounded journal event set (closed variants; first sequence is 1).
#[derive(Debug, Clone, PartialEq, Eq)]
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
                if !matches!(payload, ResourceJournalEventV1::BootstrapBound { .. }) {
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
}
