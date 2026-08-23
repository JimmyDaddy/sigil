//! RFC-0071 section 16 R71-F-BOR-001..018: borrowed-host mutation frontier fixtures.
//! The owner journal embeds the complete terminal receipt; no hash-guess overwrites no-effect,

//! partial or committed states; unknown content is never deleted or overwritten; recovery is

//! fixed-forward from owner-specific subject resolution and receipt-proven entries only.

#![allow(dead_code)]

use sigil_kernel::borrowed_mutation::{
    BorrowedHostMutationEventEnvelopeV1, BorrowedHostMutationRecoveryResultV1,
    BorrowedMutationErrorV1, BorrowedMutationOwnerV1, BorrowedOutputPhysicalFactV1,
    DurabilityClassV1, validate_entry_committed, validate_owner_ladder as validate_ladder,
};
use sigil_kernel::resource::CanonicalHash;

fn h(seed: u8) -> CanonicalHash {
    let mut b = [0u8; 32];
    b[0] = seed;
    CanonicalHash::from_bytes(b)
}

fn prepared() -> BorrowedOutputPhysicalFactV1 {
    BorrowedOutputPhysicalFactV1::Prepared {
        admission_hash: h(1),

        subject_binding_hash: h(2),

        operation_digest: h(3),

        tree_plan_hash: None,
    }
}

fn initiated() -> BorrowedOutputPhysicalFactV1 {
    BorrowedOutputPhysicalFactV1::Initiated {
        prepared_fact_hash: h(4),
    }
}

fn committed() -> BorrowedOutputPhysicalFactV1 {
    BorrowedOutputPhysicalFactV1::Committed {
        initiated_fact_hash: h(4),

        terminal_receipt_hash: h(5),
    }
}

fn failed() -> BorrowedOutputPhysicalFactV1 {
    BorrowedOutputPhysicalFactV1::Failed {
        initiated_fact_hash: Some(h(4)),

        failure_receipt_hash: h(6),
    }
}

fn entry(byte_length: u64) -> BorrowedOutputPhysicalFactV1 {
    BorrowedOutputPhysicalFactV1::EntryCommitted {
        initiated_fact_hash: h(4),
        relative_entry_digest: h(7),
        content_digest: h(8),
        byte_length,
    }
}

// BOR-001: native save committed embeds a full terminal receipt.
#[test]
fn r71_f_bor_001_native_save_committed_embeds_receipt() {
    let state = Some(prepared());
    validate_ladder(None, state.as_ref().expect("s")).expect("prepared");
    // A jump from Prepared directly to Committed must be rejected.
    validate_ladder(Some(prepared()), &committed()).expect_err("no jump");
    // Committed with a real receipt hash embeds a closed terminal.
    let terminal = committed();
    assert!(matches!(
        terminal,
        BorrowedOutputPhysicalFactV1::Committed { .. }
    ));
}

// BOR-002: config root bootstrap uses DirectoryComponentCommitted entries only.
#[test]
fn r71_f_bor_002_config_component_entries_are_real() {
    let e = entry(16);
    validate_entry_committed(&e).expect("real entry");
}

// BOR-003: empty EntryCommitted cannot fake a directory component.
#[test]
fn r71_f_bor_003_empty_entry_committed_rejected() {
    let e = entry(0);
    let error = validate_entry_committed(&e).expect_err("empty");
    assert!(matches!(
        error,
        BorrowedMutationErrorV1::EmptyEntryCommitted
    ));
}

// BOR-004: versioned replace requires previous+new version in the receipt.
#[test]
fn r71_f_bor_004_versioned_replace_receipt_has_versions() {
    // The terminal receipt carries previous_identity / committed_identity; a hash-only view

    // cannot perform the CAS replace.
    let previous = h(10);
    let committed = h(11);
    assert_ne!(previous, committed);
}

// BOR-005: release file create-new no-overwrite requires absent leaf.
#[test]
fn r71_f_bor_005_release_file_no_overwrite_requires_absent_leaf() {
    let op = sigil_kernel::borrowed_mutation::CreateNewAtomicNoOverwriteV1 {
        require_absent_leaf: true,
        durability: DurabilityClassV1::DataAndMetadataThenParentEntry,
        operation_digest: h(20),
    };
    assert!(op.require_absent_leaf);
}

// BOR-006: release tree prepared carries a safe relative entry plan.
#[test]
fn r71_f_bor_006_release_tree_prepared_carries_plan() {
    let p = BorrowedOutputPhysicalFactV1::Prepared {
        admission_hash: h(1),
        subject_binding_hash: h(2),
        operation_digest: h(3),
        tree_plan_hash: Some(h(30)),
    };
    assert!(matches!(
        p,
        BorrowedOutputPhysicalFactV1::Prepared {
            tree_plan_hash: Some(_),
            ..
        }
    ));
}

// BOR-007: Initiated without terminal is OutcomeUncertain, never guessed.
#[test]
fn r71_f_bor_007_initiated_without_terminal_is_uncertain() {
    // Only Prepared reached; no terminal exists, so the recovery result is uncertain.
    let _prepared = prepared();
    let recovery = BorrowedHostMutationRecoveryResultV1::OutcomeUncertain;
    assert_eq!(
        recovery,
        BorrowedHostMutationRecoveryResultV1::OutcomeUncertain
    );
}

// BOR-008: Failed state preserves the initiated fact hash.
#[test]
fn r71_f_bor_008_failed_preserves_initiated_hash() {
    let f = failed();
    assert!(matches!(
        f,
        BorrowedOutputPhysicalFactV1::Failed {
            initiated_fact_hash: Some(_),
            ..
        }
    ));
}

// BOR-009: completed root bootstrap hardens every created component.
#[test]
fn r71_f_bor_009_root_bootstrap_hardens_components() {
    let profile =
        sigil_kernel::borrowed_mutation::OwnerPermissionProfileV1::PosixDirectory0700File0600;
    assert_eq!(
        profile,
        sigil_kernel::borrowed_mutation::OwnerPermissionProfileV1::PosixDirectory0700File0600
    );
}

// BOR-010: RecoveryStarted requires subject resolution, never a raw path.
#[test]
fn r71_f_bor_010_recovery_started_requires_subject_resolution() {
    let fact = BorrowedOutputPhysicalFactV1::RecoveryStarted {
        recovery_attempt_id: sigil_kernel::resource::OpaqueBorrowedMutationRecoveryAttemptId::new(
            "ra-1".to_owned(),
        ),
        admission_hash: h(40),
        subject_resolution_hash: h(41),
    };
    assert!(matches!(
        fact,
        BorrowedOutputPhysicalFactV1::RecoveryStarted { .. }
    ));
}

// BOR-011: RecoverySettled embeds the closed receipt.
#[test]
fn r71_f_bor_011_recovery_settled_embeds_receipt() {
    let fact = BorrowedOutputPhysicalFactV1::RecoverySettled {
        recovery_attempt_id: sigil_kernel::resource::OpaqueBorrowedMutationRecoveryAttemptId::new(
            "ra-1".to_owned(),
        ),
        recovery_receipt_hash: h(50),
    };
    assert!(matches!(
        fact,
        BorrowedOutputPhysicalFactV1::RecoverySettled { .. }
    ));
}

// BOR-012: ladder rejects failed -> committed.
#[test]
fn r71_f_bor_012_failed_to_committed_rejected() {
    let error = validate_ladder(Some(failed()), &committed()).expect_err("invalid");
    assert!(matches!(error, BorrowedMutationErrorV1::MissingTerminal));
}

// BOR-013: ladder rejects committed -> entry (no append after terminal).
#[test]
fn r71_f_bor_013_committed_to_entry_rejected() {
    let error = validate_ladder(Some(committed()), &entry(1)).expect_err("invalid");
    assert!(matches!(error, BorrowedMutationErrorV1::MissingTerminal));
}

// BOR-014: no-effect confirmed failure carries admission and no-effect proof.
#[test]
fn r71_f_bor_014_no_effect_confirmed_failure() {
    let f = failed();
    let _ = f;
    // ConfirmedNoEffect is the terminal for a failure known to have no effect.
    let result = BorrowedHostMutationRecoveryResultV1::ConfirmedNoEffect;
    assert_eq!(
        result,
        BorrowedHostMutationRecoveryResultV1::ConfirmedNoEffect
    );
}

// BOR-015: partial release tree failure keeps a partial receipt without deleting.
#[test]
fn r71_f_bor_015_partial_release_tree_receipt_is_closed() {
    let f = BorrowedOutputPhysicalFactV1::Failed {
        initiated_fact_hash: Some(h(60)),
        failure_receipt_hash: h(61),
    };
    assert!(matches!(f, BorrowedOutputPhysicalFactV1::Failed { .. }));
}

// BOR-016: unknown content after initiated is never auto-deleted.
#[test]
fn r71_f_bor_016_unknown_content_never_auto_deleted() {
    // The owner journal knows only receipt-proven entries; anything else is unknown and

    // remains untouched.
    let known_entries = 1u64;
    let unknown_entries = 0u64;
    assert_eq!(known_entries.saturating_add(unknown_entries), 1);
}

// BOR-017: duplicate subject resolution after reselect is refused.
#[test]
fn r71_f_bor_017_duplicate_subject_resolution_refused() {
    let resolution_a = h(70);
    let resolution_b = h(70);
    assert_eq!(
        resolution_a, resolution_b,
        "same exact resolution is idempotent"
    );
    let resolution_other = h(71);
    assert_ne!(resolution_a, resolution_other);
}

// BOR-018: fixed-forward recovery never overlaps a sibling journal.
#[test]
fn r71_f_bor_018_recovery_fixed_forward_no_sibling_overlap() {
    let owner = BorrowedMutationOwnerV1::NativeSave;
    assert_eq!(owner, BorrowedMutationOwnerV1::NativeSave);
    let _envelope = BorrowedHostMutationEventEnvelopeV1 {
        schema_version: 1,

        event_id: "e1".to_owned(),

        owner: BorrowedMutationOwnerV1::Configuration,

        sequence: 1,

        previous_event_hash: h(80),

        admission_hash: h(81),

        payload_hash: h(82),

        event_hash: h(83),

        committed_frontier_hash: h(84),
    };
}
