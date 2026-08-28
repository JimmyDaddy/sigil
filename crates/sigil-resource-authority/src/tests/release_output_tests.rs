use super::*;

fn capsule(value: &str) -> OpaqueRegistrationCapsuleId {
    OpaqueRegistrationCapsuleId::new(value.to_owned())
}

#[test]
fn r71_release_file_is_create_new_and_returns_closed_receipt() {
    let temp = tempfile::tempdir().expect("tempdir");
    let service = AuthorityBorrowedReleaseOutputServiceV1::new(temp.path());
    let destination = temp.path().join("route.toml");
    let receipt = service
        .publish(BorrowedReleaseOutputRequestV1 {
            schema_version: BORROWED_RELEASE_OUTPUT_SCHEMA_VERSION,
            capsule_id: capsule("release-file"),
            operation: BorrowedReleaseOutputOperationV1::File,
            destination: destination.clone(),
            content: b"route = \"v1\"\n".to_vec(),
            entries: Vec::new(),
        })
        .expect("file publish");
    assert!(!receipt.partial);
    assert_eq!(receipt.committed_entry_count, 1);
    assert_eq!(fs::read(&destination).expect("output"), b"route = \"v1\"\n");
    let replay = service
        .publish(BorrowedReleaseOutputRequestV1 {
            schema_version: BORROWED_RELEASE_OUTPUT_SCHEMA_VERSION,
            capsule_id: capsule("release-file"),
            operation: BorrowedReleaseOutputOperationV1::File,
            destination: temp.path().join("other.toml"),
            content: b"other".to_vec(),
            entries: Vec::new(),
        })
        .expect_err("replay");
    assert_eq!(replay, BorrowedReleaseOutputErrorV1::CapsuleReplay);
}

#[test]
fn r71_release_tree_commits_bounded_entries_without_adopting_root() {
    let temp = tempfile::tempdir().expect("tempdir");
    let service = AuthorityBorrowedReleaseOutputServiceV1::new(temp.path());
    let root = temp.path().join("campaign");
    let receipt = service
        .publish(BorrowedReleaseOutputRequestV1 {
            schema_version: BORROWED_RELEASE_OUTPUT_SCHEMA_VERSION,
            capsule_id: capsule("release-tree"),
            operation: BorrowedReleaseOutputOperationV1::Tree,
            destination: root.clone(),
            content: Vec::new(),
            entries: vec![
                BorrowedReleaseOutputEntryV1 {
                    relative_path: PathBuf::from("nested/results.jsonl"),
                    content: b"{}\n".to_vec(),
                },
                BorrowedReleaseOutputEntryV1 {
                    relative_path: PathBuf::from("summary.md"),
                    content: b"# report\n".to_vec(),
                },
            ],
        })
        .expect("tree publish");
    assert!(!receipt.partial);
    assert_eq!(receipt.committed_entry_count, 2);
    assert_eq!(
        fs::read(root.join("nested/results.jsonl")).expect("nested output"),
        b"{}\n"
    );
    assert_eq!(
        fs::read(root.join("summary.md")).expect("summary"),
        b"# report\n"
    );
    let occupied = service
        .publish(BorrowedReleaseOutputRequestV1 {
            schema_version: BORROWED_RELEASE_OUTPUT_SCHEMA_VERSION,
            capsule_id: capsule("release-tree-occupied"),
            operation: BorrowedReleaseOutputOperationV1::Tree,
            destination: root,
            content: Vec::new(),
            entries: vec![BorrowedReleaseOutputEntryV1 {
                relative_path: PathBuf::from("new.txt"),
                content: b"new".to_vec(),
            }],
        })
        .expect_err("occupied root");
    assert_eq!(occupied, BorrowedReleaseOutputErrorV1::DestinationOccupied);
}

#[test]
fn r71_release_tree_root_reservation_allows_parent_entry_creation() {
    let temp = tempfile::tempdir().expect("tempdir");
    let service = AuthorityBorrowedReleaseOutputServiceV1::new(temp.path());
    let root = temp.path().join("campaign");
    service
        .prepare_tree_root(&root)
        .expect("tree root reservation");
    assert!(root.is_dir());
}

#[test]
fn r71_release_output_rejects_aliases_and_invalid_tree_entries_before_effect() {
    let temp = tempfile::tempdir().expect("tempdir");
    let service = AuthorityBorrowedReleaseOutputServiceV1::new(temp.path());
    let error = service
        .publish(BorrowedReleaseOutputRequestV1 {
            schema_version: BORROWED_RELEASE_OUTPUT_SCHEMA_VERSION,
            capsule_id: capsule("release-invalid"),
            operation: BorrowedReleaseOutputOperationV1::Tree,
            destination: temp.path().join("invalid"),
            content: Vec::new(),
            entries: vec![BorrowedReleaseOutputEntryV1 {
                relative_path: PathBuf::from("../escape.txt"),
                content: b"escape".to_vec(),
            }],
        })
        .expect_err("traversal");
    assert_eq!(error, BorrowedReleaseOutputErrorV1::EntryInvalid);
    assert!(!temp.path().join("invalid").exists());
}

#[test]
fn r71_release_tree_returns_closed_partial_frontier_after_late_conflict() {
    let temp = tempfile::tempdir().expect("tempdir");
    let service = AuthorityBorrowedReleaseOutputServiceV1::new(temp.path());
    let root = temp.path().join("partial");
    let error = service
        .publish(BorrowedReleaseOutputRequestV1 {
            schema_version: BORROWED_RELEASE_OUTPUT_SCHEMA_VERSION,
            capsule_id: capsule("release-partial"),
            operation: BorrowedReleaseOutputOperationV1::Tree,
            destination: root.clone(),
            content: Vec::new(),
            entries: vec![
                BorrowedReleaseOutputEntryV1 {
                    relative_path: PathBuf::from("a.txt"),
                    content: b"first".to_vec(),
                },
                BorrowedReleaseOutputEntryV1 {
                    relative_path: PathBuf::from("a.txt/nested.txt"),
                    content: b"conflict".to_vec(),
                },
            ],
        })
        .expect_err("late tree conflict");
    match error {
        BorrowedReleaseOutputErrorV1::Partial { receipt, .. } => {
            assert!(receipt.partial);
            assert_eq!(receipt.committed_entry_count, 1);
            assert_eq!(receipt.committed_total_bytes, 5);
        }
        other => panic!("expected partial receipt, got {other:?}"),
    }
    assert_eq!(
        fs::read(root.join("a.txt")).expect("partial file"),
        b"first"
    );
    assert!(!root.join("a.txt/nested.txt").exists());
}
