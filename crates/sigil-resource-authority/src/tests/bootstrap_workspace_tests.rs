use super::*;

fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
    let fixture = tempfile::tempdir().expect("fixture");
    let config = fixture.path().join("sigil.toml");
    fs::write(&config, "fixture").expect("config");
    let a = fixture.path().join("a");
    let b = fixture.path().join("b");
    fs::create_dir(&a).expect("workspace A");
    fs::create_dir(&b).expect("workspace B");
    (
        fixture,
        fs::canonicalize(config).expect("config path"),
        a,
        b,
    )
}

#[test]
fn workspace_realms_diagnostic_pointer_does_not_initialize_a_realm() {
    let (_fixture, config, _, _) = fixture();
    let pointer = AuthorityBootstrapStoreV1::cutover_manifest_path_for_config(&config)
        .expect("diagnostic path");
    assert!(!pointer.parent().expect("realm").exists());
    let (store, _guard) =
        AuthorityBootstrapStoreV1::for_canonical_config_path_with_publication(&config)
            .expect("first real boot");
    assert!(store.was_created_for_this_open());
}

#[test]
fn workspace_realms_keep_original_identity_and_isolate_other_workspaces() {
    let (_fixture, config, a, b) = fixture();
    let a = AuthorityBootstrapWorkspaceV1::observe(&a).expect("A identity");
    let b = AuthorityBootstrapWorkspaceV1::observe(&b).expect("B identity");
    let (original, guard) =
        AuthorityBootstrapStoreV1::for_canonical_config_path_with_publication(&config)
            .expect("original realm");
    let original_identity = original.authority_instance_hash(&guard).expect("identity");
    original.bind_workspace(&guard, &a).expect("bind A");
    let binding_bytes = fs::read(original.path(AuthorityBootstrapObjectClassV1::WorkspaceBinding))
        .expect("binding");
    assert!(matches!(
        original.bind_workspace(&guard, &b),
        Err(BootstrapErrorV1::IdentityDrift)
    ));
    drop(guard);
    let (second, guard) =
        AuthorityBootstrapStoreV1::for_canonical_config_workspace_with_publication(&config, &b)
            .expect("B realm");
    assert_ne!(second.root(), original.root());
    assert_ne!(
        second.authority_instance_hash(&guard).expect("B identity"),
        original_identity
    );
    second.bind_workspace(&guard, &b).expect("bind B");
    let second_identity = second.authority_instance_hash(&guard).expect("B identity");
    drop(guard);
    for (workspace, expected_root, expected_identity) in [
        (&a, original.root(), original_identity),
        (&b, second.root(), second_identity),
    ] {
        let (reopened, guard) =
            AuthorityBootstrapStoreV1::for_canonical_config_workspace_with_publication(
                &config, workspace,
            )
            .expect("reopen");
        assert_eq!(reopened.root(), expected_root);
        assert_eq!(
            reopened.authority_instance_hash(&guard).expect("identity"),
            expected_identity
        );
    }
    assert_eq!(
        fs::read(original.path(AuthorityBootstrapObjectClassV1::WorkspaceBinding))
            .expect("binding"),
        binding_bytes
    );
}

#[test]
fn workspace_realms_reject_replaced_directory_and_corrupt_binding() {
    let (fixture, config, a, _) = fixture();
    let workspace = AuthorityBootstrapWorkspaceV1::observe(&a).expect("identity");
    let (store, guard) =
        AuthorityBootstrapStoreV1::for_canonical_config_workspace_with_publication(
            &config, &workspace,
        )
        .expect("realm");
    store.bind_workspace(&guard, &workspace).expect("binding");
    drop(guard);
    fs::rename(&a, fixture.path().join("original-a")).expect("keep old inode");
    fs::create_dir(&a).expect("replacement directory");
    let replacement = AuthorityBootstrapWorkspaceV1::observe(&a).expect("replacement identity");
    assert!(matches!(
        AuthorityBootstrapStoreV1::for_canonical_config_workspace_with_publication(
            &config,
            &replacement
        ),
        Err(BootstrapErrorV1::IdentityDrift)
    ));
    let guard = store.acquire_publication().expect("publication");
    store
        .publish_bytes(
            &guard,
            AuthorityBootstrapObjectClassV1::WorkspaceBinding,
            br#"{"schema_version":99}"#,
        )
        .expect("corrupt fixture");
    drop(guard);
    assert!(matches!(
        AuthorityBootstrapStoreV1::for_canonical_config_workspace_with_publication(
            &config, &workspace
        ),
        Err(BootstrapErrorV1::MetadataCorrupted(_))
    ));
}

#[cfg(unix)]
#[test]
fn workspace_realms_reject_symlinked_binding_without_touching_target() {
    use std::os::unix::fs::symlink;
    let (fixture, config, a, _) = fixture();
    let workspace = AuthorityBootstrapWorkspaceV1::observe(&a).expect("identity");
    let (store, guard) =
        AuthorityBootstrapStoreV1::for_canonical_config_path_with_publication(&config)
            .expect("realm");
    let target = fixture.path().join("unchanged");
    fs::write(&target, b"unchanged").expect("target");
    symlink(
        &target,
        store.path(AuthorityBootstrapObjectClassV1::WorkspaceBinding),
    )
    .expect("symlink");
    drop(guard);
    assert!(
        AuthorityBootstrapStoreV1::for_canonical_config_workspace_with_publication(
            &config, &workspace
        )
        .is_err()
    );
    assert_eq!(fs::read(target).expect("target"), b"unchanged");
}
