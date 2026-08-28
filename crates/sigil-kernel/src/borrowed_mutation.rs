//! RFC-0071 section 9.5: borrowed-host mutation contract.
//!
//! Three owner-local journal services (Desktop native save, runtime configuration, release
//! output) share the closed envelope / receipt / no-follow semantics but never a union
//! dispatcher or an authority instance. Owner journals are append-only and store only bounded
//! schema/hash/identity/frontier, never destination paths or user content.

use serde::{Deserialize, Serialize};

use crate::resource::{CanonicalHash, OpaqueBorrowedMutationRecoveryAttemptId};

/// Closed owner classification (never a cross-owner union).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BorrowedMutationOwnerV1 {
    NativeSave,
    Configuration,
    ReleaseTool,
}

/// Closed durability class (platform must prove it or the operation is unsupported).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DurabilityClassV1 {
    DataAndMetadataThenParentEntry,
    DataMetadataAndReplaceBarrier,
    EachEntryThenDirectoryChain,
    AggregateEntriesThenDirectoryChain,
}

/// Closed permission profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OwnerPermissionProfileV1 {
    PosixDirectory0700File0600,
    WindowsProtectedCurrentUserDacl,
}

/// Create-new atomic no-overwrite operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateNewAtomicNoOverwriteV1 {
    pub require_absent_leaf: bool,
    pub durability: DurabilityClassV1,
    pub operation_digest: CanonicalHash,
}

/// Versioned atomic replace operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionedAtomicReplaceV1 {
    pub expected_object_version: u64,
    pub expected_identity: Option<CanonicalHash>,
    pub same_arena_replace_required: bool,
    pub durability: DurabilityClassV1,
    pub operation_digest: CanonicalHash,
}

/// Atomic bounded tree create (absent root + safe relative entry plan).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateNewBoundedTreeV1 {
    pub require_absent_root: bool,
    pub allowed_relative_entry_plan_hash: CanonicalHash,
    pub max_entries: u64,
    pub max_total_bytes: u64,
    pub no_follow_each_component: bool,
    pub durability: DurabilityClassV1,
    pub operation_digest: CanonicalHash,
}

/// Closed operation enum (owner-local; no cross-owner union).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BorrowedMutationOperationV1 {
    CreateNewAtomicNoOverwrite(CreateNewAtomicNoOverwriteV1),
    VersionedAtomicReplace(VersionedAtomicReplaceV1),
    CreateNewBoundedTree(CreateNewBoundedTreeV1),
    BootstrapConfigurationRoot {
        planned_missing_component_hash: CanonicalHash,
        max_missing_components: u32,
    },
}

/// Event envelope: append-only, hash-chained, no raw path or user content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BorrowedHostMutationEventEnvelopeV1 {
    pub schema_version: u32,
    pub event_id: String,
    pub owner: BorrowedMutationOwnerV1,
    pub sequence: u64,
    pub previous_event_hash: CanonicalHash,
    pub admission_hash: CanonicalHash,
    pub payload_hash: CanonicalHash,
    pub event_hash: CanonicalHash,
    pub committed_frontier_hash: CanonicalHash,
}

/// Closed payload variants (bounded, hash-only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BorrowedOutputPhysicalFactV1 {
    Prepared {
        admission_hash: CanonicalHash,
        subject_binding_hash: CanonicalHash,
        operation_digest: CanonicalHash,
        tree_plan_hash: Option<CanonicalHash>,
    },
    Initiated {
        prepared_fact_hash: CanonicalHash,
    },
    EntryCommitted {
        initiated_fact_hash: CanonicalHash,
        relative_entry_digest: CanonicalHash,
        content_digest: CanonicalHash,
        byte_length: u64,
    },
    Committed {
        initiated_fact_hash: CanonicalHash,
        terminal_receipt_hash: CanonicalHash,
    },
    Failed {
        initiated_fact_hash: Option<CanonicalHash>,
        failure_receipt_hash: CanonicalHash,
    },
    RecoveryStarted {
        recovery_attempt_id: OpaqueBorrowedMutationRecoveryAttemptId,
        admission_hash: CanonicalHash,
        subject_resolution_hash: CanonicalHash,
    },
    RecoverySettled {
        recovery_attempt_id: OpaqueBorrowedMutationRecoveryAttemptId,
        recovery_receipt_hash: CanonicalHash,
    },
}

/// Closed result classification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BorrowedHostMutationRecoveryResultV1 {
    ReconciledExistingEffect,
    ResumedAndCommitted,
    ConfirmedNoEffect,
    Superseded,
    OutcomeUncertain,
}

/// Closed error classification.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BorrowedMutationErrorV1 {
    #[error(
        "owner / type / admission / content / version / identity mismatch is journal corruption"
    )]
    JournalCorruption,
    #[error("an EntryCommitted with empty content cannot pretend to be a directory component")]
    EmptyEntryCommitted,
    #[error("duplicate event id with same payload is idempotent; different payload fails closed")]
    DuplicateEventIdConflict,
    #[error("platform cannot prove the declared durability class")]
    DurabilityUnsupported,
    #[error("missing terminal after Initiated: outcome is uncertain")]
    MissingTerminal,
}

/// Validates the closed owner-local ladder (Prepared -> Initiated -> EntryCommitted* -> Committed|Failed).
pub fn validate_owner_ladder(
    previous: Option<BorrowedOutputPhysicalFactV1>,
    next: &BorrowedOutputPhysicalFactV1,
) -> Result<(), BorrowedMutationErrorV1> {
    use BorrowedOutputPhysicalFactV1::*;
    match (previous, next) {
        (None, Prepared { .. })
        | (Some(Prepared { .. }), Initiated { .. })
        | (Some(Initiated { .. }), EntryCommitted { .. })
        | (Some(EntryCommitted { .. }), EntryCommitted { .. })
        | (Some(Initiated { .. }), Committed { .. })
        | (Some(EntryCommitted { .. }), Committed { .. })
        | (Some(Initiated { .. }), RecoveryStarted { .. }) => Ok(()),
        (Some(Initiated { .. }), Failed { .. })
        | (Some(Committed { .. }), RecoveryStarted { .. }) => Ok(()),
        (Some(RecoveryStarted { .. }), RecoverySettled { .. }) => Ok(()),
        (None, Failed { .. }) => Ok(()),
        _ => Err(BorrowedMutationErrorV1::MissingTerminal),
    }
}

/// Entry content guard: an EntryCommitted must carry real content.
pub fn validate_entry_committed(
    fact: &BorrowedOutputPhysicalFactV1,
) -> Result<(), BorrowedMutationErrorV1> {
    if let BorrowedOutputPhysicalFactV1::EntryCommitted { byte_length, .. } = fact
        && *byte_length == 0
    {
        return Err(BorrowedMutationErrorV1::EmptyEntryCommitted);
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/borrowed_mutation_tests.rs"]
mod tests;
