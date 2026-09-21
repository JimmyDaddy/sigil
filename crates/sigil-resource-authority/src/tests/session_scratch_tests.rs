use super::*;
#[test]
fn scratch_thresholds_do_not_deny_current_or_sibling_namespace() {
    let temp = tempfile::tempdir().expect("temp");
    let root = temp.path().join("scratch");
    let authority = SessionScratchAuthorityV1::new(&root);
    let first = authority.ensure(Some("a"), 1, 1).expect("namespace");
    fs::write(first.directory.join("data"), b"larger than both thresholds").expect("write");
    authority
        .ensure(Some("a"), 1, 1)
        .expect("reuse over threshold");
    authority
        .ensure(Some("b"), 1, 1)
        .expect("independent sibling");
    // Retired historical charges are not read, overwritten or treated as current capacity.
    let journal = root.join(".authority-quota/session-scratch.json");
    fs::create_dir_all(journal.parent().expect("parent")).expect("parent dir");
    fs::write(&journal, b"historical or corrupt journal").expect("journal");
    SessionScratchAuthorityV1::new(&root)
        .ensure(Some("b"), 1, 1)
        .expect("restart");
    assert_eq!(
        fs::read(&journal).expect("retained journal"),
        b"historical or corrupt journal"
    );
}

#[cfg(unix)]
#[test]
fn unknown_measurement_retains_history_and_does_not_deny_any_namespace() {
    let temp = tempfile::tempdir().expect("temp");
    let authority = SessionScratchAuthorityV1::new(temp.path().join("scratch"));
    let a = authority.ensure(Some("a"), 1, 1).expect("a");
    let b = authority.ensure(Some("b"), 1, 1).expect("b");
    fs::write(a.directory.join("data"), b"history").expect("data");
    fs::write(b.directory.join("data"), b"known").expect("data");
    assert_eq!(authority.observe(1, 100).known_subtotal_bytes, 12);
    std::os::unix::fs::symlink(temp.path(), a.directory.join("unreadable")).expect("symlink");
    let unknown = authority.observe(2, 100);
    assert_eq!(unknown.known_subtotal_bytes, 5);
    assert_eq!(unknown.unknown_owners, vec!["sessions/a"]);
    assert!(matches!(
        unknown.owners.get("sessions/a"),
        Some(SessionScratchObservationV1::Unknown {
            observed_at_ms: 2,
            last_successful_measurement: Some(SessionScratchMeasurementV1 {
                bytes: 7,
                observed_at_ms: 1,
                ..
            }),
            ..
        })
    ));
    authority
        .ensure(Some("a"), 1, 1)
        .expect("cleanup can run in A");
    authority.ensure(Some("b"), 1, 1).expect("B remains usable");
    assert!(
        authority.measure("b").is_err(),
        "unknown aggregate must not be exposed as exact totals"
    );
    // Root identity is still an admission boundary.
    fs::remove_dir_all(&b.directory).expect("remove b");
    std::os::unix::fs::symlink(temp.path(), &b.directory).expect("bad root");
    assert!(authority.ensure(Some("b"), 1, 1).is_err());
}

#[test]
fn bounded_measurement_and_missing_owner_remain_unknown_until_authenticated_cleanup() {
    let temp = tempfile::tempdir().expect("temp");
    let authority = SessionScratchAuthorityV1::new(temp.path().join("scratch"));
    let a = authority.ensure(Some("a"), 1, 1).expect("a");
    fs::write(a.directory.join("data"), b"old").expect("data");
    authority.observe(1, 100);
    assert!(!authority.observe(2, 0).unknown_owners.is_empty());
    fs::remove_dir_all(a.directory).expect("external removal");
    assert!(!authority.observe(3, 100).unknown_owners.is_empty());
    authority
        .delete(Some("a"))
        .expect("authenticated missing owner");
    assert!(!authority.observe(4, 100).owners.contains_key("sessions/a"));
    assert!(authority.ensure(Some("../escape"), 1, 1).is_err());
}

#[test]
fn maintenance_budget_counts_directories_across_namespaces_and_keeps_quarantine_visible() {
    let temp = tempfile::tempdir().expect("temp");
    let root = temp.path().join("scratch");
    let authority = SessionScratchAuthorityV1::new(&root);
    for key in ["a", "b"] {
        let namespace = authority.ensure(Some(key), 1, 1).expect("namespace");
        fs::create_dir_all(namespace.directory.join("nested/leaf")).expect("directories");
    }
    assert!(authority.observe(1, 100).unknown_owners.is_empty());
    assert!(!authority.observe(2, 4).unknown_owners.is_empty());
    authority.delete(Some("a")).expect("delete a");
    authority.delete(Some("b")).expect("delete b");
    fs::remove_dir(root.join(SESSION_NAMESPACE_DIR)).expect("no sessions");
    let quarantine = root.join(QUARANTINE_DIR).join("retained");
    fs::create_dir_all(&quarantine).expect("quarantine");
    fs::write(quarantine.join("data"), b"retained bytes").expect("retained bytes");
    let report = authority
        .gc(SessionScratchGcConfigV1::default(), 42)
        .expect("gc");
    assert_eq!(report.workspace_usage_bytes, Some(14));
    assert_eq!(report.observed_at_ms, 42);
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
