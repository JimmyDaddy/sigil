use super::*;

#[test]
fn authority_owns_session_namespace_and_quota() {
    let temp = tempfile::tempdir().expect("temp");
    let authority = SessionScratchAuthorityV1::new(temp.path().join("scratch"));
    let provision = authority
        .ensure(Some("session-a"), 10, 20)
        .expect("provision");
    std::fs::write(provision.directory.join("data"), b"12345678901").expect("write");
    let error = authority
        .ensure(Some("session-a"), 10, 20)
        .expect_err("quota");
    assert!(matches!(
        error,
        SessionScratchErrorV1::QuotaExceeded {
            scope: SessionScratchQuotaScopeV1::Session,
            ..
        }
    ));
}

#[test]
fn durable_scratch_quota_replays_across_authority_restart() {
    let temp = tempfile::tempdir().expect("temp");
    let root = temp.path().join("scratch");
    {
        let authority = SessionScratchAuthorityV1::new(&root);
        let provision = authority
            .ensure(Some("session-a"), 100, 1000)
            .expect("provision");
        std::fs::write(provision.directory.join("data"), b"durable").expect("write");
        authority
            .ensure(Some("session-a"), 100, 1000)
            .expect("record measured usage");
    }

    let restarted = SessionScratchAuthorityV1::new(&root);
    restarted
        .ensure(Some("session-a"), 100, 1000)
        .expect("replay and reconcile active owner");
    assert_eq!(
        restarted.delete(Some("session-a")).expect("delete"),
        SessionScratchDeleteOutcomeV1::Deleted
    );
    let reopened = SessionScratchAuthorityV1::new(&root);
    reopened
        .ensure(Some("session-a"), 100, 1000)
        .expect("released quota is reusable");
}

#[cfg(unix)]
#[test]
fn descendant_symlink_is_rejected_without_sibling_poisoning() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().expect("temp");
    let authority = SessionScratchAuthorityV1::new(temp.path().join("scratch"));
    let first = authority.ensure(Some("first"), 100, 1000).expect("first");
    let _second = authority.ensure(Some("second"), 100, 1000).expect("second");
    let target = temp.path().join("outside");
    std::fs::create_dir(&target).expect("target");
    symlink(&target, first.directory.join("escape")).expect("symlink");
    let error = authority
        .ensure(Some("first"), 100, 1000)
        .expect_err("symlink");
    assert!(matches!(error, SessionScratchErrorV1::Symlink { .. }));
    assert!(authority.session_directory(Some("second")).exists());
}

#[test]
fn active_lease_blocks_delete() {
    let temp = tempfile::tempdir().expect("temp");
    let authority = SessionScratchAuthorityV1::new(temp.path().join("scratch"));
    authority
        .ensure(Some("session-a"), 100, 1000)
        .expect("provision");
    let _lease = authority.acquire(Some("session-a")).expect("lease");
    assert_eq!(
        authority.delete(Some("session-a")).expect("delete"),
        SessionScratchDeleteOutcomeV1::SkippedLeased
    );
}

#[test]
fn lease_marker_creation_failure_is_fail_closed() {
    let temp = tempfile::tempdir().expect("temp");
    let root = temp.path().join("scratch");
    std::fs::write(&root, b"not a directory").expect("root file");
    let authority = SessionScratchAuthorityV1::new(root);
    assert!(authority.acquire(Some("session-a")).is_err());
}

#[test]
fn lease_marker_blocks_gc_after_authority_restarts() {
    let temp = tempfile::tempdir().expect("temp");
    let root = temp.path().join("scratch");
    let authority = SessionScratchAuthorityV1::new(&root);
    authority
        .ensure(Some("session-a"), 100, 1000)
        .expect("provision");
    let lease = authority.acquire(Some("session-a")).expect("lease");

    let restarted = SessionScratchAuthorityV1::new(&root);
    let report = restarted
        .gc(SessionScratchGcConfigV1::default(), u64::MAX)
        .expect("gc");
    assert_eq!(report.skipped_leased, 1);
    assert_eq!(report.deleted, 0);
    drop(lease);
    let report = restarted
        .gc(SessionScratchGcConfigV1::default(), u64::MAX)
        .expect("gc after lease release");
    assert_eq!(report.deleted, 1);
}

#[cfg(unix)]
#[test]
fn gc_quarantines_invalid_namespace_instead_of_silently_skipping() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().expect("temp");
    let root = temp.path().join("scratch");
    let authority = SessionScratchAuthorityV1::new(&root);
    authority
        .ensure(Some("valid"), 100, 1000)
        .expect("provision");
    let outside = temp.path().join("outside");
    fs::create_dir(&outside).expect("outside");
    let invalid = root.join(SESSION_NAMESPACE_DIR).join("invalid");
    symlink(&outside, &invalid).expect("invalid symlink");

    let report = authority
        .gc(SessionScratchGcConfigV1::default(), u64::MAX)
        .expect("gc");
    assert_eq!(report.quarantined, 1);
    assert_eq!(report.skipped_invalid, 1);
    assert!(
        !invalid.exists(),
        "invalid namespace moved out of the scan root"
    );
    assert_eq!(
        fs::read_dir(root.join(QUARANTINE_DIR))
            .expect("quarantine")
            .count(),
        1
    );
}
