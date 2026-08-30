use super::*;

fn request(destination: PathBuf, content: &str) -> BorrowedNativeSaveRequestV1 {
    BorrowedNativeSaveRequestV1 {
        schema_version: BORROWED_NATIVE_SAVE_SCHEMA_VERSION,
        purpose: BorrowedNativeSavePurposeV1::SupportBundle,
        capsule_id: OpaqueRegistrationCapsuleId::new(format!(
            "capsule-{}",
            digest_bytes(content.as_bytes()).to_hex()
        )),
        raw_destination: destination,
        content: content.to_owned(),
        content_hash: digest_bytes(content.as_bytes()),
    }
}

#[test]
fn r71_native_save_writes_real_closed_receipt_and_rejects_overwrite() {
    let temp = tempfile::tempdir().expect("tempdir");
    let registry = std::sync::Arc::new(Mutex::new(BorrowedSubjectRegistryV1::new()));
    let service = AuthorityBorrowedNativeSaveServiceV1::new(registry);
    let destination = temp.path().join("sigil-support-123.json");
    let receipt = service
        .save(request(destination.clone(), "{\"schema_version\":1}"))
        .expect("native save");
    assert_eq!(receipt.byte_length, 20);
    assert_eq!(
        std::fs::read_to_string(&destination).expect("saved"),
        "{\"schema_version\":1}"
    );
    let error = service
        .save(request(destination, "{\"schema_version\":2}"))
        .expect_err("overwrite must be rejected");
    assert_eq!(error, BorrowedNativeSaveErrorV1::DestinationOccupied);
}

#[cfg(unix)]
#[test]
fn r71_native_save_rejects_symlink_destination() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().expect("tempdir");
    let target = temp.path().join("target.json");
    std::fs::write(&target, "private").expect("target");
    let destination = temp.path().join("sigil-support-123.json");
    symlink(&target, &destination).expect("link");
    let service = AuthorityBorrowedNativeSaveServiceV1::new(std::sync::Arc::new(Mutex::new(
        BorrowedSubjectRegistryV1::new(),
    )));
    let error = service
        .save(request(destination, "{}"))
        .expect_err("symlink must be rejected");
    assert_eq!(error, BorrowedNativeSaveErrorV1::SymlinkAtBoundary);
    assert_eq!(std::fs::read_to_string(target).expect("target"), "private");
}
