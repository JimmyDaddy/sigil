use super::*;

fn hash(seed: u8) -> CanonicalHash {
    let mut bytes = [0u8; 32];
    bytes[0] = seed;
    CanonicalHash::from_bytes(bytes)
}

fn prepared() -> BorrowedOutputPhysicalFactV1 {
    BorrowedOutputPhysicalFactV1::Prepared {
        admission_hash: hash(1),
        subject_binding_hash: hash(2),
        operation_digest: hash(3),
        tree_plan_hash: None,
    }
}

#[test]
fn r71_owner_ladder_allows_commit_after_prepared_initiated_entry() {
    validate_owner_ladder(None, &prepared()).expect("prepared");
    validate_owner_ladder(
        Some(prepared()),
        &BorrowedOutputPhysicalFactV1::Initiated {
            prepared_fact_hash: hash(4),
        },
    )
    .expect("initiated");
    validate_owner_ladder(
        Some(BorrowedOutputPhysicalFactV1::Initiated {
            prepared_fact_hash: hash(4),
        }),
        &BorrowedOutputPhysicalFactV1::Committed {
            initiated_fact_hash: hash(4),
            terminal_receipt_hash: hash(5),
        },
    )
    .expect("committed");
}

#[test]
fn r71_owner_ladder_rejects_jump_from_prepared_to_committed() {
    let error = validate_owner_ladder(
        Some(prepared()),
        &BorrowedOutputPhysicalFactV1::Committed {
            initiated_fact_hash: hash(4),
            terminal_receipt_hash: hash(5),
        },
    )
    .expect_err("jump");
    assert!(matches!(error, BorrowedMutationErrorV1::MissingTerminal));
}

#[test]
fn r71_entry_committed_rejects_empty_content() {
    let error = validate_entry_committed(&BorrowedOutputPhysicalFactV1::EntryCommitted {
        initiated_fact_hash: hash(4),
        relative_entry_digest: hash(5),
        content_digest: hash(6),
        byte_length: 0,
    })
    .expect_err("empty");
    assert!(matches!(
        error,
        BorrowedMutationErrorV1::EmptyEntryCommitted
    ));
}

#[test]
fn r71_recovery_successor_ladder_is_closed() {
    validate_owner_ladder(
        Some(BorrowedOutputPhysicalFactV1::RecoveryStarted {
            recovery_attempt_id: OpaqueBorrowedMutationRecoveryAttemptId::new("r1".to_owned()),
            admission_hash: hash(1),
            subject_resolution_hash: hash(2),
        }),
        &BorrowedOutputPhysicalFactV1::RecoverySettled {
            recovery_attempt_id: OpaqueBorrowedMutationRecoveryAttemptId::new("r1".to_owned()),
            recovery_receipt_hash: hash(3),
        },
    )
    .expect("settled");
}
