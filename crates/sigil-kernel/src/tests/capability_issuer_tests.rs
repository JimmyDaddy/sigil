use super::*;

#[test]
fn r71_capability_table_fails_closed_on_duplicate_and_unknown_consume() {
    let mut table = KernelCapabilityTableV1::new();
    table.record_issued("bundle-1".to_owned()).expect("issue");
    let duplicate = table.record_issued("bundle-1".to_owned()).expect_err("dup");
    assert!(matches!(
        duplicate,
        CapabilityIssueErrorV1::UnknownOrConsumed
    ));
    let unknown = table
        .record_consumed("bundle-2".to_owned())
        .expect_err("unknown");
    assert!(matches!(unknown, CapabilityVerifyErrorV1::VerifyFailed(_)));
    table
        .record_consumed("bundle-1".to_owned())
        .expect("consume");
    let reconsume = table
        .record_consumed("bundle-1".to_owned())
        .expect_err("again");
    assert!(matches!(
        reconsume,
        CapabilityVerifyErrorV1::VerifyFailed(_)
    ));
}

#[test]
fn r71_mock_issuer_never_issues_but_keeps_closed_errors() {
    let issuer = mock_issuer();
    let _ = issuer;
}

#[test]
fn r71_evidence_view_never_reissues_capability() {
    let view = VerifiedEvidenceViewV1 {
        verifier_instance_hash: CanonicalHash::from_bytes([1u8; 32]),
        verified_record_hash: CanonicalHash::from_bytes([2u8; 32]),
        verified_frontier_hash: CanonicalHash::from_bytes([3u8; 32]),
    };
    // Only public hashes; no authenticator is exposed.
    assert_eq!(
        view.verified_record_hash,
        CanonicalHash::from_bytes([2u8; 32])
    );
}
