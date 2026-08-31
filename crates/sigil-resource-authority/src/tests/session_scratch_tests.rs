use super::*;
use sigil_kernel::resource::{ScratchQuotaExceededError, ScratchQuotaScope};

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
    assert_eq!(
        error,
        SessionScratchErrorV1::QuotaExceeded(ScratchQuotaExceededError {
            scope: ScratchQuotaScope::Session,
            usage_bytes: 11,
            quota_bytes: 10,
        })
    );
}

#[test]
fn quota_adapter_preserves_byte_totals_and_keeps_entry_counts_out_of_byte_payloads() {
    assert_eq!(
        quota_error(QuotaErrorV1::ReservationExceeded {
            class: "session_scratch",
            reserved: 17,
            max: 16,
        }),
        SessionScratchErrorV1::QuotaExceeded(ScratchQuotaExceededError {
            scope: ScratchQuotaScope::Workspace,
            usage_bytes: 17,
            quota_bytes: 16,
        })
    );
    assert_eq!(
        quota_error(QuotaErrorV1::WorkspaceOvercommit {
            used: 11,
            incoming: 7,
            cap: 16,
        }),
        SessionScratchErrorV1::QuotaExceeded(ScratchQuotaExceededError {
            scope: ScratchQuotaScope::Workspace,
            usage_bytes: 18,
            quota_bytes: 16,
        })
    );
    assert_eq!(
        quota_error(QuotaErrorV1::WorkspaceOvercommit {
            used: u64::MAX - 1,
            incoming: 2,
            cap: u64::MAX,
        }),
        SessionScratchErrorV1::QuotaExceeded(ScratchQuotaExceededError {
            scope: ScratchQuotaScope::Workspace,
            usage_bytes: u64::MAX,
            quota_bytes: u64::MAX,
        })
    );
    assert_eq!(
        quota_error(QuotaErrorV1::EntryExceeded {
            class: "session_scratch",
            reserved: 7,
            max: 6,
        }),
        SessionScratchErrorV1::EntryLimitExceeded {
            limit: 6,
            observed: 7,
        }
    );
}

#[cfg(target_pointer_width = "64")]
#[test]
fn quota_adapter_does_not_truncate_platform_sized_entry_counts() {
    assert_eq!(
        quota_error(QuotaErrorV1::EntryExceeded {
            class: "session_scratch",
            reserved: u64::MAX,
            max: u64::MAX - 1,
        }),
        SessionScratchErrorV1::EntryLimitExceeded {
            limit: usize::MAX - 1,
            observed: usize::MAX,
        }
    );
}

#[cfg(target_pointer_width = "32")]
#[test]
fn quota_adapter_does_not_truncate_entry_counts_that_exceed_platform_range() {
    let error = quota_error(QuotaErrorV1::EntryExceeded {
        class: "session_scratch",
        reserved: u64::MAX,
        max: u64::MAX - 1,
    });
    let SessionScratchErrorV1::Filesystem(message) = error else {
        panic!("out-of-range entry counts must not be truncated into a byte quota");
    };
    assert!(message.contains(&u64::MAX.to_string()));
    assert!(message.contains(&(u64::MAX - 1).to_string()));
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
