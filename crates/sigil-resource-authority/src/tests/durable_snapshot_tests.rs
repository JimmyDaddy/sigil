use super::*;

#[test]
fn snapshot_writer_lock_excludes_competing_writers_until_owner_drops() {
    let fixture = tempfile::tempdir().expect("fixture");
    let snapshot = fixture.path().join("journal.json");
    fs::write(&snapshot, b"original snapshot").expect("snapshot");
    let owner = open_owner_only_snapshot_writer_lock(&snapshot).expect("first writer");

    let error = open_owner_only_snapshot_writer_lock(&snapshot)
        .expect_err("a separate file description must not acquire the held writer lock");
    assert_eq!(error.kind(), fs2::lock_contended_error().kind());
    assert_eq!(
        fs::read(&snapshot).expect("snapshot bytes"),
        b"original snapshot"
    );

    drop(owner);
    let _successor = open_owner_only_snapshot_writer_lock(&snapshot)
        .expect("dropping the owner releases exclusion for the next writer");
}

#[cfg(unix)]
#[test]
fn snapshot_writer_owner_release_is_not_extended_by_a_duplicated_descriptor() {
    let fixture = tempfile::tempdir().expect("fixture");
    let snapshot = fixture.path().join("journal.json");
    fs::write(&snapshot, b"original snapshot").expect("snapshot");
    let owner = open_owner_only_snapshot_writer_lock(&snapshot).expect("first writer");
    // dup shares the exact open file description that a forked child inherits, without needing
    // an unsafe fork from the multithreaded test process or any timing-dependent spawn window.
    let inherited_descriptor = owner.file.try_clone().expect("duplicated descriptor");

    let held = open_owner_only_snapshot_writer_lock(&snapshot)
        .expect_err("the active writer must still exclude competing file descriptions");
    assert_eq!(held.kind(), fs2::lock_contended_error().kind());

    drop(owner);
    let successor = open_owner_only_snapshot_writer_lock(&snapshot)
        .expect("writer release must not wait for an unrelated inherited descriptor to close");
    drop(inherited_descriptor);
    let still_held = open_owner_only_snapshot_writer_lock(&snapshot)
        .expect_err("closing the predecessor's duplicate must not release the successor's lock");
    assert_eq!(still_held.kind(), fs2::lock_contended_error().kind());
    assert_eq!(
        fs::read(&snapshot).expect("snapshot bytes"),
        b"original snapshot"
    );

    drop(successor);
    let _next = open_owner_only_snapshot_writer_lock(&snapshot)
        .expect("the next writer acquires after the successor releases");
}
