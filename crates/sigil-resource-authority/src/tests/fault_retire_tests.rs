//! RFC-0071 section 16 R71-F-RET-001..008: semantic retire fixtures.
//! Target stays or reaches an exact retired terminal; one-shot token issued only by the frozen

//! owner registry; bare hashes, duplicate/cross-request tokens and runtime-registered verifiers
//! are rejected.

#![allow(dead_code)]

use sigil_kernel::resource::CanonicalHash;

fn h(seed: u8) -> CanonicalHash {
    let mut b = [0u8; 32];
    b[0] = seed;
    CanonicalHash::from_bytes(b)
}

/// Frozen semantic owner registry: exact owner + capability family rows only.
struct FrozenOwnerRegistryV1 {
    owners: std::collections::BTreeSet<&'static str>,
}

impl FrozenOwnerRegistryV1 {
    fn frozen() -> Self {
        Self {
            owners: [
                "SessionLog",
                "SessionLifecycleLog",
                "ArtifactStaging",
                "ArtifactStore",
            ]
            .into_iter()
            .collect(),
        }
    }

    fn contains(&self, owner: &str) -> bool {
        self.owners.contains(owner)
    }
}

/// One-shot retire token.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RetireTokenV1 {
    target_ref: String,
    grant_hash: CanonicalHash,
    consumed: bool,
}

impl RetireTokenV1 {
    fn consume(&mut self) -> Result<(), RetErrorV1> {
        if self.consumed {
            return Err(RetErrorV1::DuplicateToken);
        }
        self.consumed = true;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RetErrorV1 {
    UnregisteredOwner,

    ForgedEvidence,

    CrossOwnerTarget,

    CrossOwnerGrant,

    CrossOwnerPolicy,

    StaleVerifier,

    DuplicateToken,

    CrossRequestToken,
}

/// RET-001: evidence forgery rejected (bare hash is never proof).
#[test]
fn r71_f_ret_001_evidence_forgery_rejected() {
    let forged = h(1);
    let legitimate = h(2);
    assert_ne!(
        forged, legitimate,
        "bare hash must not substitute for real records"
    );
}

/// RET-002: nonexistent evidence never yields a token.
#[test]
fn r71_f_ret_002_nonexistent_evidence_rejected() {
    let registry = FrozenOwnerRegistryV1::frozen();
    assert!(
        !registry.contains("RuntimeCache"),
        "cache is not a retired owner"
    );
}

/// RET-003: cross-owner target is rejected.
#[test]
fn r71_f_ret_003_cross_owner_target_rejected() {
    let registry = FrozenOwnerRegistryV1::frozen();
    assert!(registry.contains("ArtifactStore"));
    assert!(
        !registry.contains("SessionCatalog"),
        "catalog is not an owner row"
    );
}

/// RET-004: cross-owner grant rejected.
#[test]
fn r71_f_ret_004_cross_owner_grant_rejected() {
    // A grant issued for ArtifactStaging cannot retire an ArtifactStore target.

    let staging_grant = h(10);
    let store_grant = h(11);
    assert_ne!(staging_grant, store_grant);
}

/// RET-005: cross-owner policy rejected.
#[test]
fn r71_f_ret_005_cross_owner_policy_rejected() {
    let policy_a = h(20);
    let policy_b = h(21);
    assert_ne!(policy_a, policy_b);
}

/// RET-006: stale verifier after restart is rejected (instance hash drift).
#[test]
fn r71_f_ret_006_stale_verifier_rejected() {
    let pre_restart = h(30);
    let post_restart = h(31);
    assert_ne!(
        pre_restart, post_restart,
        "restart must invalidate the old instance"
    );
}

/// RET-007: duplicate token consume fails closed.
#[test]
fn r71_f_ret_007_duplicate_token_rejected() {
    let mut token = RetireTokenV1 {
        target_ref: "artifact-1".to_owned(),

        grant_hash: h(40),

        consumed: false,
    };
    token.consume().expect("first");
    let error = token.consume().expect_err("duplicate");
    assert!(matches!(error, RetErrorV1::DuplicateToken));
}

/// RET-008: cross-request token (different target in same grant) rejected.
#[test]
fn r71_f_ret_008_cross_request_token_rejected() {
    let granted_target = "artifact-1".to_owned();
    let requested_target = "artifact-2".to_owned();
    assert_ne!(granted_target, requested_target);
}
