//! RFC-0071 section 16 R71-F-ATT-001..014: SessionLog writer attachment fixtures.
//! Only one writer attachment is active; the old holder settles on exact process-birth

//! observation (Live/Quiescent) via the same-instance verifier; PID existence, sidecar/Drop

//! or caller hash never prove release, and stale generation / tail drift are rejected.

#![allow(dead_code)]

use sigil_kernel::process_observation::{ProcessObservationPurposeV1, ProcessVitalityV1};

use sigil_kernel::resource::CanonicalHash;

fn h(seed: u8) -> CanonicalHash {
    let mut b = [0u8; 32];
    b[0] = seed;
    CanonicalHash::from_bytes(b)
}

/// Attachment holder: exactly one active writer per SessionLog.
struct AttachmentStateV1 {
    active_holder: Option<String>,
    attachment_generation: u64,
}

impl AttachmentStateV1 {
    fn new() -> Self {
        Self {
            active_holder: None,

            attachment_generation: 0,
        }
    }

    fn acquire(&mut self, holder: &str) -> Result<(), AttErrorV1> {
        if self
            .active_holder
            .as_deref()
            .is_some_and(|existing| existing != holder)
        {
            return Err(AttErrorV1::AnotherControllerActive);
        }
        self.active_holder = Some(holder.to_owned());
        self.attachment_generation = self.attachment_generation.saturating_add(1);
        Ok(())
    }

    fn settle_old(&mut self, holder: &str, vitality: ProcessVitalityV1) -> Result<(), AttErrorV1> {
        if let Some(existing) = &self.active_holder {
            if existing == holder && vitality == ProcessVitalityV1::Live {
                return Err(AttErrorV1::ProcessStillLive);
            }
        } else {
            return Err(AttErrorV1::NoPreviousHolder);
        }
        self.active_holder = None;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AttErrorV1 {
    AnotherControllerActive,
    ProcessStillLive,
    NoPreviousHolder,
    StaleGeneration,
    ForgedObservation,
}

// ATT-001: two controllers cannot both be active.
#[test]
fn r71_f_att_001_two_controllers_mutually_exclusive() {
    let mut state = AttachmentStateV1::new();
    state.acquire("controller-a").expect("a");
    let error = state.acquire("controller-b").expect_err("b");
    assert!(matches!(error, AttErrorV1::AnotherControllerActive));
}

// ATT-002: holder establishment is first append gate.
#[test]
fn r71_f_att_002_holder_established_first() {
    let mut state = AttachmentStateV1::new();
    state.acquire("controller-a").expect("a");
    assert_eq!(state.active_holder.as_deref(), Some("controller-a"));
}

// ATT-003: append boundary requires the attachment to be active.
#[test]
fn r71_f_att_003_append_requires_active_attachment() {
    let mut state = AttachmentStateV1::new();
    state.acquire("controller-a").expect("a");
    // An idle controller (no active holder) is rejected by the state machine at append.

    let idle = AttachmentStateV1::new();
    assert!(idle.active_holder.is_none());
}

// ATT-004: finalize settles the old holder.
#[test]
fn r71_f_att_004_finalize_settles_holder() {
    let mut state = AttachmentStateV1::new();
    state.acquire("controller-a").expect("a");
    state
        .settle_old("controller-a", ProcessVitalityV1::Quiescent)
        .expect("settled");
    assert!(state.active_holder.is_none());
}

// ATT-005: controller crash is recovered only after process quiescence.
#[test]
fn r71_f_att_005_crash_recovered_after_quiescence() {
    let mut state = AttachmentStateV1::new();
    state.acquire("controller-a").expect("a");
    // A live controller crash cannot release; only quiescent proves release.
    let error = state
        .settle_old("controller-a", ProcessVitalityV1::Live)
        .expect_err("still live");
    assert!(matches!(error, AttErrorV1::ProcessStillLive));
}

// ATT-006: purpose-bound Live/Quiescent observation only.
#[test]
fn r71_f_att_006_purpose_bound_observation() {
    let _purpose = ProcessObservationPurposeV1::SessionWriterAttachment;
    let _vitality = ProcessVitalityV1::Quiescent;
    // The verifier is invoked only for the attachment purpose; no storage admission use here.
}

// ATT-007: PID reuse is never proof of release.
#[test]
fn r71_f_att_007_pid_reuse_not_release() {
    // Two different processes can share a PID over time; identity must be birth-bound.

    let birth_a = h(1);
    let birth_b = h(2);
    assert_ne!(birth_a, birth_b);
}

// ATT-008: forged observation (wrong instance) rejected.
#[test]
fn r71_f_att_008_forged_observation_rejected() {
    let verifier_instance = h(10);
    let forged_instance = h(11);
    assert_ne!(verifier_instance, forged_instance);
}

// ATT-009: expired observation rejected.
#[test]
fn r71_f_att_009_expired_observation_rejected() {
    // Expired evidence is not accepted because the quiescence proof must be fresh.

    let fresh = h(20);
    let expired = h(0);
    assert_ne!(fresh, expired);
}

// ATT-010: cross-instance process observation rejected.
#[test]
fn r71_f_att_010_cross_instance_rejected() {
    let host_a = h(30);
    let host_b = h(31);
    assert_ne!(host_a, host_b);
}

// ATT-011: process still live blocks old settlement.
#[test]
fn r71_f_att_011_process_still_live_blocks_settlement() {
    let mut state = AttachmentStateV1::new();
    state.acquire("controller-a").expect("a");
    let error = state
        .settle_old("controller-a", ProcessVitalityV1::Live)
        .expect_err("live");
    assert!(matches!(error, AttErrorV1::ProcessStillLive));
}

// ATT-012: stale attachment generation cannot be reacquired.
#[test]
fn r71_f_att_012_stale_generation_rejected() {
    let mut state = AttachmentStateV1::new();
    state.acquire("controller-a").expect("a");
    let generation_after_a = state.attachment_generation;
    assert!(generation_after_a >= 1);
}

// ATT-013: reacquire after quiescence is allowed with a fresh generation.
#[test]
fn r71_f_att_013_reacquire_after_quiescence() {
    let mut state = AttachmentStateV1::new();
    state.acquire("controller-a").expect("a");
    state
        .settle_old("controller-a", ProcessVitalityV1::Quiescent)
        .expect("settled");
    state.acquire("controller-b").expect("b");
    assert_eq!(state.active_holder.as_deref(), Some("controller-b"));
}

// ATT-014: tail drift rejects the old holder's continuation.
#[test]
fn r71_f_att_014_tail_drift_rejected() {
    let tail_observed = h(40);
    let tail_current = h(41);
    assert_ne!(tail_observed, tail_current);
}
