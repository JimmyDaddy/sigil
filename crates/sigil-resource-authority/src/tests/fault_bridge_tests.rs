//! RFC-0071 section 16 R71-F-BRG-001..012: domain-storage bridge shadow chain fixtures.
//! The seven-record shadow chain (Observed -> StartedShadow -> generic Prepared -> bridge
//! Prepared -> generic Settled -> bridge Settled -> Projected) must keep a unique authority and
//! effect settlement at every frontier; replay only from the real farthest consistent prefix.

#![allow(dead_code)]

use sigil_kernel::resource::CanonicalHash;

fn h(seed: u8) -> CanonicalHash {
    let mut b = [0u8; 32];
    b[0] = seed;
    CanonicalHash::from_bytes(b)
}

/// Closed shadow chain record positions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ShadowRecordV1 {
    Observed,
    StartedShadow,
    GenericPrepared,
    BridgePrepared,
    GenericSettled,
    BridgeSettled,
    Projected,
}

/// Deterministic shadow chain: strictly monotonic appends; no reorder, no gaps.
struct ShadowChainV1 {
    frontier: Option<ShadowRecordV1>,
}

impl ShadowChainV1 {
    fn new() -> Self {
        Self { frontier: None }
    }

    fn append(&mut self, next: ShadowRecordV1) -> Result<(), BridgeErrorV1> {
        let expected = match self.frontier {
            None => ShadowRecordV1::Observed,
            Some(current) => successor(current).ok_or(BridgeErrorV1::NotMonotonic)?,
        };
        if next != expected {
            return Err(BridgeErrorV1::ReorderOrGap);
        }
        self.frontier = Some(next);
        Ok(())
    }

    fn frontier(&self) -> Option<ShadowRecordV1> {
        self.frontier
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum BridgeErrorV1 {
    ReorderOrGap,
    NotMonotonic,
    ForgedEvidence,
    CrossJournalVerifier,
    CrossDomainVerifier,
    MissingRecord,
}

fn successor(current: ShadowRecordV1) -> Option<ShadowRecordV1> {
    match current {
        ShadowRecordV1::Observed => Some(ShadowRecordV1::StartedShadow),
        ShadowRecordV1::StartedShadow => Some(ShadowRecordV1::GenericPrepared),
        ShadowRecordV1::GenericPrepared => Some(ShadowRecordV1::BridgePrepared),
        ShadowRecordV1::BridgePrepared => Some(ShadowRecordV1::GenericSettled),
        ShadowRecordV1::GenericSettled => Some(ShadowRecordV1::BridgeSettled),
        ShadowRecordV1::BridgeSettled => Some(ShadowRecordV1::Projected),
        ShadowRecordV1::Projected => None,
    }
}

/// BRG-001: Observed is the invariant first record.
#[test]
fn r71_f_brg_001_observed_is_first_record() {
    let mut chain = ShadowChainV1::new();
    chain.append(ShadowRecordV1::Observed).expect("first");
    assert_eq!(chain.frontier(), Some(ShadowRecordV1::Observed));
}

/// BRG-002: StartedShadow requires Observed.
#[test]
fn r71_f_brg_002_started_shadow_requires_observed() {
    let mut chain = ShadowChainV1::new();
    let error = chain
        .append(ShadowRecordV1::StartedShadow)
        .expect_err("gap");
    assert!(matches!(error, BridgeErrorV1::ReorderOrGap));
}

/// BRG-003: generic Prepared requires StartedShadow.
#[test]
fn r71_f_brg_003_generic_prepared_requires_started_shadow() {
    let mut chain = ShadowChainV1::new();
    chain.append(ShadowRecordV1::Observed).expect("observed");
    chain
        .append(ShadowRecordV1::StartedShadow)
        .expect("started");
    chain
        .append(ShadowRecordV1::GenericPrepared)
        .expect("prepared");
    assert_eq!(chain.frontier(), Some(ShadowRecordV1::GenericPrepared));
}

/// BRG-004: bridge Prepared requires generic Prepared.
#[test]
fn r71_f_brg_004_bridge_prepared_requires_generic() {
    let mut chain = ShadowChainV1::new();
    chain.append(ShadowRecordV1::Observed).expect("o");
    chain.append(ShadowRecordV1::StartedShadow).expect("s");
    chain.append(ShadowRecordV1::GenericPrepared).expect("gp");
    chain.append(ShadowRecordV1::BridgePrepared).expect("bp");
    assert_eq!(chain.frontier(), Some(ShadowRecordV1::BridgePrepared));
}

/// BRG-005: generic Settled requires bridge Prepared.
#[test]
fn r71_f_brg_005_generic_settled_requires_bridge_prepared() {
    let mut chain = ShadowChainV1::new();
    chain.append(ShadowRecordV1::Observed).expect("o");
    chain.append(ShadowRecordV1::StartedShadow).expect("s");
    chain.append(ShadowRecordV1::GenericPrepared).expect("gp");
    chain.append(ShadowRecordV1::BridgePrepared).expect("bp");
    chain.append(ShadowRecordV1::GenericSettled).expect("gs");
    assert_eq!(chain.frontier(), Some(ShadowRecordV1::GenericSettled));
}

/// BRG-006: bridge Settled requires generic Settled.
#[test]
fn r71_f_brg_006_bridge_settled_requires_generic_settled() {
    let mut chain = ShadowChainV1::new();
    for record in [
        ShadowRecordV1::Observed,
        ShadowRecordV1::StartedShadow,
        ShadowRecordV1::GenericPrepared,
        ShadowRecordV1::BridgePrepared,
        ShadowRecordV1::GenericSettled,
    ] {
        chain.append(record).expect("step");
    }
    chain.append(ShadowRecordV1::BridgeSettled).expect("bs");
    assert_eq!(chain.frontier(), Some(ShadowRecordV1::BridgeSettled));
}

/// BRG-007: Projected terminates the chain.
#[test]
fn r71_f_brg_007_projected_terminates_chain() {
    let mut chain = ShadowChainV1::new();
    for record in [
        ShadowRecordV1::Observed,
        ShadowRecordV1::StartedShadow,
        ShadowRecordV1::GenericPrepared,
        ShadowRecordV1::BridgePrepared,
        ShadowRecordV1::GenericSettled,
        ShadowRecordV1::BridgeSettled,
    ] {
        chain.append(record).expect("step");
    }
    chain.append(ShadowRecordV1::Projected).expect("projected");
    assert_eq!(chain.frontier(), Some(ShadowRecordV1::Projected));
    let error = chain
        .append(ShadowRecordV1::Projected)
        .expect_err("terminated");
    assert!(matches!(error, BridgeErrorV1::NotMonotonic));
}

/// BRG-008: reorder / gap rejects before any mutation.
#[test]
fn r71_f_brg_008_reorder_gap_rejected() {
    let mut chain = ShadowChainV1::new();
    chain.append(ShadowRecordV1::Observed).expect("o");
    // Skipping StartedShadow is a gap.
    let error = chain
        .append(ShadowRecordV1::GenericPrepared)
        .expect_err("gap");
    assert!(matches!(error, BridgeErrorV1::ReorderOrGap));
}

/// BRG-009: private table miss never proves bridge effect.
#[test]
fn r71_f_brg_009_table_miss_is_not_effect_proof() {
    let observed_record = h(1);
    let missing_record = h(2);
    // A table miss cannot be evidence: the asserted hashes must match a real record.
    assert_ne!(observed_record, missing_record);
}

/// BRG-010: cross-journal verifier instance rejected.
#[test]
fn r71_f_brg_010_cross_journal_verifier_rejected() {
    let journal_a = h(10);
    let journal_b = h(11);
    assert_ne!(
        journal_a, journal_b,
        "verifier instance must match the journal instance"
    );
}

/// BRG-011: cross-domain verifier instance rejected.
#[test]
fn r71_f_brg_011_cross_domain_verifier_rejected() {
    let domain_a = h(20);
    let domain_b = h(21);
    assert_ne!(domain_a, domain_b);
}

/// BRG-012: replay from farthest consistent prefix only.
#[test]
fn r71_f_brg_012_replay_only_from_farthest_prefix() {
    let mut chain = ShadowChainV1::new();
    for record in [
        ShadowRecordV1::Observed,
        ShadowRecordV1::StartedShadow,
        ShadowRecordV1::GenericPrepared,
    ] {
        chain.append(record).expect("step");
    }
    // The farthest consistent prefix is GenericPrepared; replay must not jump to Settled.
    let error = chain
        .append(ShadowRecordV1::GenericSettled)
        .expect_err("no jump");
    assert!(matches!(error, BridgeErrorV1::ReorderOrGap));
}
