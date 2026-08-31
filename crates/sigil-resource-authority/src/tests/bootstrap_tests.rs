use super::*;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[test]
fn r71_bootstrap_resolve_rejects_missing_state_home_without_cwd_fallback() {
    let resolver = BootstrapRootResolverV1::default();
    let error = resolver.resolve().expect_err("must fail closed");
    assert!(matches!(error, BootstrapErrorV1::StateRootUnavailable));
}

#[cfg(unix)]
#[test]
fn r71_bootstrap_rejects_symlinked_state_anchor() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().expect("tempdir");
    let state_target = temp.path().join("state-target");
    std::fs::create_dir_all(&state_target).expect("target");
    let state_link = temp.path().join("state-link");
    symlink(&state_target, &state_link).expect("link");

    let roots = AuthorityBootstrapRoots {
        state_anchor: state_link,
        cache_anchor: temp.path().join("cache"),
        execution_temp_anchor: temp.path().join("et"),
        state_identity: canonical_bootstrap_hash(b"x"),
        cache_identity: canonical_bootstrap_hash(b"y"),
        execution_temp_identity: canonical_bootstrap_hash(b"z"),
        manifest_hash: canonical_bootstrap_hash(b"m"),
        journal_instance_hash: canonical_bootstrap_hash(b"j"),
    };
    let error = roots.validate_anchors().expect_err("symlink must fail");
    assert!(matches!(error, BootstrapErrorV1::NotPlainDirectory(_)));
}

#[test]
fn r71_bootstrap_hash_is_stable() {
    assert_eq!(
        canonical_bootstrap_hash(b"payload"),
        canonical_bootstrap_hash(b"payload")
    );
}

fn publish_active_epoch_for_test(namespace: &std::path::Path, epoch: u64) -> std::path::PathBuf {
    let epochs = namespace.join(EPOCHS_DIRECTORY_NAME);
    ensure_owner_only_directory(&epochs).expect("epoch directory");
    let root = epochs.join(format!("epoch-{epoch}-fence-test"));
    ensure_owner_only_directory(&root).expect("epoch root");
    let recovery = AuthorityBootstrapRecoveryNamespaceV1 {
        namespace: namespace.to_path_buf(),
    };
    let transaction = recovery.acquire_transaction().expect("transaction");
    recovery
        .publish_active_epoch(&transaction, epoch, &root)
        .expect("active epoch pointer");
    drop(transaction);
    root
}

#[test]
fn r71_bootstrap_stale_handle_fence_rejects_before_old_root_hardening() {
    let temp = tempfile::tempdir().expect("tempdir");
    let base = std::fs::canonicalize(temp.path()).expect("canonical tempdir");
    let namespace = base.join("authority-namespace");
    let old_root = publish_active_epoch_for_test(&namespace, 2);
    let old_store = AuthorityBootstrapStoreV1::open(&namespace, &old_root, 2).expect("store");
    let sentinel = old_root.join("stale-handle-sentinel");
    std::fs::write(&sentinel, b"old-root-bytes").expect("sentinel");
    let new_root = publish_active_epoch_for_test(&namespace, 3);
    let bytes_before = std::fs::read(&sentinel).expect("sentinel bytes");

    // Make any accidental owner-only hardening observable after the cutover itself has completed.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&old_root, std::fs::Permissions::from_mode(0o750))
            .expect("old root permissions");
    }
    #[cfg(unix)]
    let old_root_metadata_before = std::fs::symlink_metadata(&old_root).expect("old root");
    let error = old_store
        .acquire_publication()
        .expect_err("stale store must be fenced");
    assert!(matches!(error, BootstrapErrorV1::IdentityDrift));
    assert_eq!(
        std::fs::read(&sentinel).expect("sentinel after fence"),
        bytes_before
    );
    assert!(
        !old_store
            .path(AuthorityBootstrapObjectClassV1::WriterLock)
            .exists()
    );
    #[cfg(unix)]
    assert_eq!(
        std::fs::symlink_metadata(&old_root)
            .expect("old root after fence")
            .permissions()
            .mode(),
        old_root_metadata_before.permissions().mode()
    );

    let new_store = AuthorityBootstrapStoreV1::open(&namespace, &new_root, 3).expect("new store");
    let publication = new_store.acquire_publication().expect("new publication");
    new_store
        .publish_bytes(
            &publication,
            AuthorityBootstrapObjectClassV1::BootstrapManifest,
            b"new-root-metadata",
        )
        .expect("new store writes");
}

#[test]
fn r71_bootstrap_stale_inventory_handle_is_fenced_and_initial_fresh_remains_valid() {
    use crate::process_inventory::AuthorityProcessInventoryPortV1;

    let temp = tempfile::tempdir().expect("tempdir");
    let base = std::fs::canonicalize(temp.path()).expect("canonical tempdir");
    let namespace = base.join("authority-namespace");
    let old_root = publish_active_epoch_for_test(&namespace, 2);
    let old_store = AuthorityBootstrapStoreV1::open(&namespace, &old_root, 2).expect("store");
    assert!(!old_store.was_created_for_this_open());
    let publication = old_store.acquire_publication().expect("publication");
    let inventory = crate::process_inventory::AuthorityManagedProcessInventoryV1::initialize(
        old_store,
        &publication,
        true,
    )
    .expect("initial inventory");
    drop(publication);

    let _new_root = publish_active_epoch_for_test(&namespace, 3);
    let error = inventory
        .prepare_spawn("stale-inventory-attempt")
        .expect_err("stale inventory must be fenced");
    assert!(matches!(
        error,
        crate::process_inventory::AuthorityProcessInventoryErrorV1::Bootstrap(
            BootstrapErrorV1::IdentityDrift
        )
    ));

    let fresh_namespace = base.join("fresh-authority-namespace");
    let fresh_store = AuthorityBootstrapStoreV1::open(&fresh_namespace, &fresh_namespace, 1)
        .expect("fresh store");
    assert!(fresh_store.was_created_for_this_open());
    let fresh_publication = fresh_store
        .acquire_publication()
        .expect("fresh publication");
    crate::process_inventory::AuthorityManagedProcessInventoryV1::initialize(
        fresh_store,
        &fresh_publication,
        false,
    )
    .expect("fresh inventory");
}

#[test]
fn r71_bootstrap_stale_handle_does_not_recreate_missing_old_root() {
    let temp = tempfile::tempdir().expect("tempdir");
    let base = std::fs::canonicalize(temp.path()).expect("canonical tempdir");
    let namespace = base.join("authority-namespace");
    let old_root = publish_active_epoch_for_test(&namespace, 2);
    let old_store = AuthorityBootstrapStoreV1::open(&namespace, &old_root, 2).expect("store");
    publish_active_epoch_for_test(&namespace, 3);
    // This empty, test-owned directory is removed to expose any pre-fence hardening on every
    // platform, including Windows where chmod-mode assertions are unavailable.
    std::fs::remove_dir(&old_root).expect("remove empty old fixture root");
    assert!(matches!(
        old_store.acquire_publication(),
        Err(BootstrapErrorV1::IdentityDrift)
    ));
    assert!(
        !old_root.exists(),
        "stale publication must not recreate the old root"
    );
}

#[cfg(unix)]
fn spawn_short_lived_child_for_e02_test() -> std::io::Result<std::process::Child> {
    std::process::Command::new("sleep").arg("1").spawn()
}

#[cfg(windows)]
fn spawn_short_lived_child_for_e02_test() -> std::io::Result<std::process::Child> {
    std::process::Command::new("ping")
        .args(["127.0.0.1", "-n", "2"])
        .spawn()
}

/// E02 integration guard: an Attached PID that has already been reaped remains unproven. The
/// current V1 observer deliberately returns a typed terminal-proof rejection until RA supplies
/// an authenticated birth/scope subject; this test must never turn PID absence into quiescence.
#[test]
fn r71_e02_reaped_attached_pid_never_proves_old_epoch_quiescence() {
    use crate::process_inventory::AuthorityProcessInventoryPortV1;

    let mut child = spawn_short_lived_child_for_e02_test().expect("spawn child");
    let process_id = child.id();
    let temp = tempfile::tempdir().expect("tempdir");
    let base = std::fs::canonicalize(temp.path()).expect("canonical tempdir");
    let namespace = base.join("authority-namespace");
    let store = AuthorityBootstrapStoreV1::open(&namespace, &namespace, 1).expect("store");
    let publication = store.acquire_publication().expect("publication");
    let inventory = crate::process_inventory::AuthorityManagedProcessInventoryV1::initialize(
        store,
        &publication,
        true,
    )
    .expect("inventory");
    drop(publication);
    let claim = inventory
        .prepare_spawn("e02-reaped-child")
        .expect("prepare");
    inventory.attach_spawn(&claim, process_id).expect("attach");
    child.wait().expect("wait and reap child");

    let factory = sigil_process_observer::ProcessObserverFactoryV1::new(canonical_bootstrap_hash(
        b"e02-reaped-attached-pid-test",
    ))
    .instantiate();
    let service = AuthorityBootstrapRecoveryServiceV1::from_namespace(
        AuthorityBootstrapRecoveryNamespaceV1 { namespace },
        factory,
    );
    let error = service
        .probe_old_epoch_quiescence(canonical_bootstrap_hash(b"e02-evidence"))
        .expect_err("reaped PID must not prove quiescence");
    assert!(matches!(
        error,
        AuthorityBootstrapRecoveryErrorV1::OldEpochStillLive(_)
            | AuthorityBootstrapRecoveryErrorV1::NoQuiescence
    ));
}

#[cfg(windows)]
#[test]
fn r71_bootstrap_verbatim_path_walk_skips_the_uninspectable_prefix() {
    let temp = tempfile::tempdir().expect("tempdir");
    let nested = temp.path().join("nested");
    std::fs::create_dir(&nested).expect("nested");
    let canonical = std::fs::canonicalize(&nested).expect("canonical nested path");

    reject_symlink_components(&canonical).expect("verbatim path prefix is not a filesystem entry");
}
