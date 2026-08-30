use super::*;

fn unit() -> GenerationReconcileUnitV1 {
    GenerationReconcileUnitV1 {
        resource_id: "exec-temp".to_owned(),
        generation: 1,
        journal_record_present: true,
        journal_record_activated: false,
        arena_leaf_present: true,
        active_holders: 0,
        quarantine_capacity_bytes: 0,
    }
}

#[test]
fn r71_reconcile_reaps_orphan_reservation_after_crash() {
    let u = unit();
    assert_eq!(
        decide_reconcile(&u).expect("reconcile"),
        ReconcileOutcomeV1::ReapedOrphanReservation
    );
}

#[test]
fn r71_reconcile_never_adopts_unknown_leaf_without_journal() {
    let mut u = unit();
    u.journal_record_present = false;
    assert_eq!(
        decide_reconcile(&u).expect("reconcile"),
        ReconcileOutcomeV1::NeedsOperatorConfirmation
    );
}

#[test]
fn r71_reconcile_active_holder_blocks_reaping() {
    let mut u = unit();
    u.active_holders = 1;
    let error = decide_reconcile(&u).expect_err("still leased");
    assert!(matches!(error, ReconcileErrorV1::StillLeased));
}

#[test]
fn r71_quarantine_cap_never_silent_deletes() {
    let error = guard_quarantine_capacity(90, 20, 100).expect_err("cap");
    assert!(matches!(error, ReconcileErrorV1::QuarantineCapReached));
    guard_quarantine_capacity(90, 10, 100).expect("within cap");
}
