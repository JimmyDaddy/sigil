use super::*;
use crate::identity::canonical_identity;
use std::collections::BTreeSet;

#[test]
fn r71_borrowed_observation_generation_is_per_subject() {
    let mut registry = BorrowedSubjectRegistryV1::new();
    let subject = OpaquePermissionSubjectRef::new("workspace:root".to_owned());
    registry
        .observe(&subject, BorrowedSubjectClassV1::Workspace, 1)
        .expect("observe");
    registry
        .observe(&subject, BorrowedSubjectClassV1::Workspace, 2)
        .expect("re-observe");
    assert_eq!(registry.generation_for(&subject), Some(2));
    // Cross-class re-observation fails closed.
    let error = registry
        .observe(&subject, BorrowedSubjectClassV1::ExternalUserPath, 3)
        .expect_err("cross-class");
    assert!(matches!(error, BorrowedErrorV1::NoAdmission));
}

#[test]
fn r71_borrowed_identity_drift_is_rejected() {
    let temp = tempfile::tempdir().expect("tempdir");
    let file = temp.path().join("existing.txt");
    std::fs::write(&file, b"one").expect("write");
    let identity = canonical_identity(&file).expect("identity");
    let generation = BorrowedIdentityGenerationV1 {
        subject_class: BorrowedSubjectClassV1::Workspace,
        subject_ref: OpaquePermissionSubjectRef::new("subject-1".to_owned()),
        observation_generation: 1,
        identity,
        admitted_access: BTreeSet::from([ResourceAccessV1::Read]),
    };
    // Mutate the file; the observed identity no longer matches.
    std::fs::write(&file, b"two-two-totally-different").expect("rewrite");
    let mutated = canonical_identity(&file).expect("identity 2");
    let error = validate_borrowed_access(&generation, ResourceAccessV1::Read, &mutated, false)
        .expect_err("drift must fail");
    assert!(matches!(error, BorrowedErrorV1::IdentityDrift));
}

#[test]
fn r71_workspace_activation_is_idempotent_and_detects_root_drift() {
    let first = tempfile::tempdir().expect("first workspace");
    let second = tempfile::tempdir().expect("second workspace");
    let generation = AuthorityGeneration {
        epoch: 3,
        instance_hash: CanonicalHash::from_bytes([7; 32]),
    };
    let mut registry = BorrowedSubjectRegistryV1::new();
    let capsule = registry
        .activate_workspace("app", "workspace", first.path(), generation)
        .expect("activate");
    let repeated = registry
        .activate_workspace("app", "workspace", first.path(), generation)
        .expect("repeat activation");
    assert_eq!(repeated, capsule);
    let error = registry
        .activate_workspace("app", "workspace", second.path(), generation)
        .expect_err("root replacement must drift");
    assert!(matches!(error, BorrowedErrorV1::IdentityDrift));
}

#[cfg(unix)]
#[test]
fn r71_workspace_activation_rejects_symlink_root() {
    let workspace = tempfile::tempdir().expect("workspace");
    let parent = tempfile::tempdir().expect("parent");
    let link = parent.path().join("workspace-link");
    std::os::unix::fs::symlink(workspace.path(), &link).expect("symlink");
    let generation = AuthorityGeneration {
        epoch: 1,
        instance_hash: CanonicalHash::from_bytes([8; 32]),
    };
    let error = BorrowedSubjectRegistryV1::new()
        .activate_workspace("app", "workspace", &link, generation)
        .expect_err("symlink root must fail closed");
    assert!(matches!(error, BorrowedErrorV1::SymlinkAtBoundary));
}

#[test]
fn r71_system_temp_write_is_denied_unless_approved() {
    let temp = tempfile::tempdir().expect("tempdir");
    let dir = temp.path();
    let identity = canonical_identity(dir).expect("identity");
    let generation = BorrowedIdentityGenerationV1 {
        subject_class: BorrowedSubjectClassV1::SystemTemp,
        subject_ref: OpaquePermissionSubjectRef::new("system-temp".to_owned()),
        observation_generation: 1,
        identity,
        admitted_access: BTreeSet::from([ResourceAccessV1::Write]),
    };
    let identity = canonical_identity(dir).expect("identity 2");
    let error = validate_borrowed_access(&generation, ResourceAccessV1::Write, &identity, false)
        .expect_err("default deny write");
    assert!(matches!(error, BorrowedErrorV1::SystemTempWriteDenied));
    // Explicit unconfined approval permits the write grant.
    let lease = validate_borrowed_access(&generation, ResourceAccessV1::Write, &identity, true)
        .expect("explicit allow");
    assert_eq!(
        lease.cleanup_status,
        ResourceCleanupStatusV1::NotApplicableBorrowed
    );
}
