//! RFC-0071 section 10.8 / 18 R71.2: startup reconciliation and crash-resilient reservation.
//!
//! A crash can happen at reserve, mkdir, hardening, journal append or cleanup. This module
//! reconciles: an unreserved generation journaled as Reserved but never Activated is reaped;
//! a generation that is Active without a durable journal record is never adopted; quarantine
//! capacity never triggers silent delete.

/// Closed reconciliation classification for one generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileOutcomeV1 {
    ReapedOrphanReservation,
    AdoptedVerified,
    Quarantined,
    KeptPendingCleanup,
    NeedsOperatorConfirmation,
}

/// Closed reconcile error classification.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReconcileErrorV1 {
    #[error("journal record for the generation is missing or instance-mismatched")]
    JournalMissing,
    #[error("active unknown child prevents reaping; remain blocked")]
    ActiveUnknownChild,
    #[error("quarantine cap reached; refusing silent delete")]
    QuarantineCapReached,
    #[error("generation identity does not match the reservation record")]
    IdentityMismatch,
    #[error("generation is still leased by an active holder")]
    StillLeased,
}

/// One reconciliation unit. The coordinator feeds these from the journal and arena inventory.
#[derive(Debug, Clone)]
pub struct GenerationReconcileUnitV1 {
    pub resource_id: String,
    pub generation: u64,
    pub journal_record_present: bool,
    pub journal_record_activated: bool,
    pub arena_leaf_present: bool,
    pub active_holders: u64,
    pub quarantine_capacity_bytes: u64,
}

/// Deterministic reconciliation decision (no side effects here; executor performs the action).
pub fn decide_reconcile(
    unit: &GenerationReconcileUnitV1,
) -> Result<ReconcileOutcomeV1, ReconcileErrorV1> {
    if !unit.journal_record_present {
        // A present leaf without a journal record is unknown; never adopt or delete it silently.
        if unit.arena_leaf_present {
            return Ok(ReconcileOutcomeV1::NeedsOperatorConfirmation);
        }
        return Ok(ReconcileOutcomeV1::NeedsOperatorConfirmation);
    }
    if unit.active_holders > 0 {
        return Err(ReconcileErrorV1::StillLeased);
    }
    if unit.journal_record_activated && unit.arena_leaf_present {
        return Ok(ReconcileOutcomeV1::AdoptedVerified);
    }
    if !unit.journal_record_activated && unit.arena_leaf_present {
        return Ok(ReconcileOutcomeV1::ReapedOrphanReservation);
    }
    if unit.journal_record_activated && !unit.arena_leaf_present {
        return Ok(ReconcileOutcomeV1::KeptPendingCleanup);
    }
    Ok(ReconcileOutcomeV1::NeedsOperatorConfirmation)
}

/// Quarantine cap guard: refusing to quarantine silently deletes or ignores.
pub fn guard_quarantine_capacity(
    current_quarantine_bytes: u64,
    incoming_bytes: u64,
    cap_bytes: u64,
) -> Result<(), ReconcileErrorV1> {
    if current_quarantine_bytes.saturating_add(incoming_bytes) > cap_bytes {
        return Err(ReconcileErrorV1::QuarantineCapReached);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
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
}
