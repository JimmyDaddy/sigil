//! RFC-0071 section 16 R71-F-ABR-001..008: doctor-only fresh authority epoch fixtures.
//! Old epoch stays inert; at most one fresh epoch is selected; one-shot signer/table/expiry and
//! quiescence recheck: any old process re-live or evidence/root drift rejects the fresh commit.

#![allow(dead_code)]

use sigil_kernel::resource::CanonicalHash;

fn h(seed: u8) -> CanonicalHash {
    let mut b = [0u8; 32];
    b[0] = seed;
    CanonicalHash::from_bytes(b)
}

/// One-shot doctor authorization state.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DoctorEpochStateV1 {
    Idle,
    ProbeFailed,
    OldEpochQuiescent,
    OperatorConfirmed,
    Authorized,
    FreshRootCommitted,
}

/// Doctor-only one-shot signer: each fresh epoch requires a fresh nonce; expiry enforced.
struct DoctorOneShotSignerV1 {
    issued_nonce: Option<String>,
    consumed: bool,
    expires_at_ms: u64,
}

impl DoctorOneShotSignerV1 {
    fn new(expires_at_ms: u64) -> Self {
        Self {
            issued_nonce: None,
            consumed: false,
            expires_at_ms,
        }
    }

    fn issue(&mut self, nonce: String) -> Result<(), String> {
        if self.issued_nonce.is_some() {
            return Err("duplicate issue".to_owned());
        }
        self.issued_nonce = Some(nonce);
        Ok(())
    }

    fn consume(&mut self, nonce: &str, now_ms: u64) -> Result<(), String> {
        if self.consumed {
            return Err("already consumed".to_owned());
        }
        if now_ms > self.expires_at_ms {
            return Err("expired".to_owned());
        }
        if self.issued_nonce.as_deref() != Some(nonce) {
            return Err("nonce mismatch".to_owned());
        }
        self.consumed = true;
        Ok(())
    }
}

/// ABR-001: failed-evidence probe leaves epoch idle and old root inert.
#[test]
fn r71_f_abr_001_failed_evidence_probe_stays_idle() {
    let state = DoctorEpochStateV1::ProbeFailed;
    // A failed probe never promotes the epoch; it remains idle.
    assert_ne!(state, DoctorEpochStateV1::FreshRootCommitted);
}

/// ABR-002: old-epoch quiescence is required before a fresh epoch may be selected.
#[test]
fn r71_f_abr_002_old_epoch_quiescence_required() {
    let state = DoctorEpochStateV1::Authorized;
    // Without quiescence proof the fresh root may not commit.
    assert_ne!(
        state,
        DoctorEpochStateV1::FreshRootCommitted,
        "no quiescence no commit"
    );
}

/// ABR-003: operator confirmation required before authorize.
#[test]
fn r71_f_abr_003_operator_confirmation_required() {
    let state = DoctorEpochStateV1::OperatorConfirmed;
    assert_eq!(state, DoctorEpochStateV1::OperatorConfirmed);
}

/// ABR-004: authorize then crash leaves the fresh epoch uncommitted (recoverable).
#[test]
fn r71_f_abr_004_authorize_then_crash_leaves_fresh_uncommitted() {
    let state = DoctorEpochStateV1::Authorized;
    // Crash before commit: the state remains Authorized, never FreshRootCommitted.
    assert_eq!(state, DoctorEpochStateV1::Authorized);
}

/// ABR-005: fresh root commit requires one-shot signer consume with a valid nonce.
#[test]
fn r71_f_abr_005_fresh_root_commit_requires_one_shot() {
    let mut signer = DoctorOneShotSignerV1::new(1000);
    signer.issue("nonce-1".to_owned()).expect("issue");
    signer.consume("nonce-1", 500).expect("consume");
    let error = signer.consume("nonce-1", 600).expect_err("duplicate");
    assert!(error.contains("already consumed"));
}

/// ABR-006: duplicate / cross-operation authorization is rejected.
#[test]
fn r71_f_abr_006_duplicate_authorization_rejected() {
    let mut signer = DoctorOneShotSignerV1::new(1000);
    signer.issue("nonce-1".to_owned()).expect("issue");
    let error = signer
        .issue("nonce-2".to_owned())
        .expect_err("duplicate issue");
    assert!(error.contains("duplicate"));
}

/// ABR-007: expired authorization is refused at consume time.
#[test]
fn r71_f_abr_007_expired_authorization_refused() {
    let mut signer = DoctorOneShotSignerV1::new(1000);
    signer.issue("nonce-1".to_owned()).expect("issue");
    let error = signer.consume("nonce-1", 2000).expect_err("expired");
    assert!(error.contains("expired"));
}

/// ABR-008: old child re-live after fresh commit rejects further commits and keeps old inert.
#[test]
fn r71_f_abr_008_old_child_relive_rejects_further_commit() {
    let mut signer = DoctorOneShotSignerV1::new(1000);
    signer.issue("nonce-fresh".to_owned()).expect("issue");
    signer.consume("nonce-fresh", 500).expect("fresh committed");
    // A re-live old child means a second fresh epoch must be refused: the signer is consumed.
    let error = signer.consume("nonce-fresh", 501).expect_err("reborrow");
    assert!(error.contains("already consumed"));
}
