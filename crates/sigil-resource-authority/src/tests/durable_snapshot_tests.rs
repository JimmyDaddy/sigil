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
