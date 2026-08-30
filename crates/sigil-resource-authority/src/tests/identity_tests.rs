use super::*;

#[cfg(unix)]
#[test]
fn r71_identity_symlink_is_a_leaf_not_followed() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().expect("tempdir");
    let target = temp.path().join("target");
    std::fs::write(&target, b"data").expect("target");
    let link = temp.path().join("leaf");
    symlink(&target, &link).expect("link");
    let identity = canonical_identity(&link).expect("identity");
    assert!(
        identity.is_symlink,
        "symlink leaf must be identified as link"
    );
    assert_eq!(
        classify_alias(temp.path(), &link).expect("class"),
        AliasContainmentClassV1::DescendantSymlinkLeaf
    );
}

#[test]
fn r71_identity_plain_file_reports_contained() {
    let temp = tempfile::tempdir().expect("tempdir");
    let file = temp.path().join("plain.txt");
    std::fs::write(&file, b"data").expect("file");
    assert_eq!(
        classify_alias(temp.path(), &file).expect("class"),
        AliasContainmentClassV1::Contained
    );
}

#[test]
fn r71_identity_digest_is_stable() {
    assert_eq!(identity_digest(b"x"), identity_digest(b"x"));
}

#[test]
fn r71_directory_identity_survives_admitted_child_creation() {
    let temp = tempfile::tempdir().expect("tempdir");
    let before = canonical_identity(temp.path()).expect("directory identity");
    std::fs::create_dir(temp.path().join("child")).expect("child directory");
    let after = canonical_identity(temp.path()).expect("directory identity after child");
    assert_eq!(before, after, "before={before:?} after={after:?}");
}
