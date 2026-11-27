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

#[test]
fn workspace_realms_readonly_cutover_lookup_supports_uninitialized_multi_workspace_and_drift() {
    let (fixture, config, a, b) = fixture();
    let c = fixture.path().join("c");
    fs::create_dir(&c).expect("workspace C");

    // Before first boot: read-only inspection returns None and must not create the realm.
    let pointer = AuthorityBootstrapStoreV1::cutover_manifest_path_for_config(&config)
        .expect("diagnostic path");
    assert!(!pointer.parent().expect("realm").exists());
    assert_eq!(
        AuthorityBootstrapStoreV1::read_cutover_manifest_bytes_readonly(&config, Some(&a))
            .expect("uninitialized lookup"),
        None
    );
    assert!(
        !pointer.parent().expect("realm").exists(),
        "readonly lookup must not create bootstrap directories"
    );

    // Boot workspace A (original realm) and workspace B (workspace-realms-v1 child).
    let ws_a = AuthorityBootstrapWorkspaceV1::observe(&a).expect("A identity");
    let ws_b = AuthorityBootstrapWorkspaceV1::observe(&b).expect("B identity");
    let (store_a, guard_a) =
        AuthorityBootstrapStoreV1::for_canonical_config_workspace_with_publication(&config, &ws_a)
            .expect("A realm");
    store_a
        .publish_bytes(
            &guard_a,
            AuthorityBootstrapObjectClassV1::CutoverPointer,
            br#"{"realm":"a"}"#,
        )
        .expect("publish A pointer");
    store_a.bind_workspace(&guard_a, &ws_a).expect("bind A");
    drop(guard_a);

    let (store_b, guard_b) =
        AuthorityBootstrapStoreV1::for_canonical_config_workspace_with_publication(&config, &ws_b)
            .expect("B realm");
    store_b
        .publish_bytes(
            &guard_b,
            AuthorityBootstrapObjectClassV1::CutoverPointer,
            br#"{"realm":"b"}"#,
        )
        .expect("publish B pointer");
    store_b.bind_workspace(&guard_b, &ws_b).expect("bind B");
    drop(guard_b);

    assert_eq!(
        AuthorityBootstrapStoreV1::read_cutover_manifest_bytes_readonly(&config, Some(&a))
            .expect("read A"),
        Some(br#"{"realm":"a"}"#.to_vec())
    );
    assert_eq!(
        AuthorityBootstrapStoreV1::read_cutover_manifest_bytes_readonly(&config, Some(&b))
            .expect("read B"),
        Some(br#"{"realm":"b"}"#.to_vec())
    );
    assert_eq!(
        AuthorityBootstrapStoreV1::read_cutover_manifest_bytes_readonly(&config, Some(&c))
            .expect("uninitialized C"),
        None
    );
    assert!(
        !store_a
            .namespace()
            .join("workspace-realms-v1")
            .join(
                AuthorityBootstrapWorkspaceV1::observe(&c)
                    .expect("C")
                    .path_hash
                    .to_hex()
            )
            .exists(),
        "readonly lookup for an uninitialized workspace must not create its realm directory"
    );

    // Replacing workspace B triggers identity drift instead of silently returning None.
    fs::rename(&b, fixture.path().join("original-b")).expect("keep old B inode");
    fs::create_dir(&b).expect("replacement B");
    assert!(matches!(
        AuthorityBootstrapStoreV1::read_cutover_manifest_bytes_readonly(&config, Some(&b)),
        Err(BootstrapErrorV1::IdentityDrift)
    ));
}
