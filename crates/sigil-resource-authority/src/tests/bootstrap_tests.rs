use super::*;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

fn process_inventory_factory()
-> std::sync::Arc<dyn sigil_kernel::process_observation::HostProcessObservationFactoryV1> {
    sigil_process_observer::ProcessObserverFactoryV1::new(canonical_bootstrap_hash(
        b"r71-bootstrap-process-inventory-test-observer",
    ))
    .expect("test process observer")
    .instantiate()
}

fn process_inventory_binding()
-> crate::process_inventory::AuthorityProcessInventoryBootstrapBindingV1 {
    crate::process_inventory::AuthorityProcessInventoryBootstrapBindingV1 {
        application_composition_epoch: 1,
        owner_execution_scope_hash: canonical_bootstrap_hash(b"r71-bootstrap-test-owner"),
    }
}

fn initialize_process_inventory(
    store: AuthorityBootstrapStoreV1,
    publication: &AuthorityBootstrapPublicationGuard,
) -> crate::process_inventory::AuthorityManagedProcessInventoryV1 {
    crate::process_inventory::AuthorityManagedProcessInventoryV1::initialize(
        store,
        publication,
        process_inventory_binding(),
        process_inventory_factory(),
    )
    .expect("process inventory")
}

fn process_spawn_request(
    attempt: &str,
) -> crate::process_inventory::AuthorityProcessSpawnRequestV1 {
    crate::process_inventory::AuthorityProcessSpawnRequestV1 {
        attempt_id: sigil_kernel::resource::PhysicalAttemptId::new(attempt.to_owned()),
        execution_scope_hash: canonical_bootstrap_hash(attempt.as_bytes()),
    }
}

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

/// Creates a fresh active epoch through the same pointer fence that production publication uses.
/// The returned store retains its create-new fact, allowing the inventory to create its initial
/// durable authenticator only after the epoch is active.
fn open_fresh_active_epoch_store_for_test(
    namespace: &std::path::Path,
    epoch: u64,
) -> AuthorityBootstrapStoreV1 {
    let epochs = namespace.join(EPOCHS_DIRECTORY_NAME);
    ensure_owner_only_directory(&epochs).expect("epoch directory");
    let root = epochs.join(format!("epoch-{epoch}-fresh-inventory-test"));
    let store = AuthorityBootstrapStoreV1::open(namespace, &root, epoch).expect("fresh store");
    assert!(store.was_created_for_this_open());
    let recovery = AuthorityBootstrapRecoveryNamespaceV1 {
        namespace: namespace.to_path_buf(),
    };
    let transaction = recovery.acquire_transaction().expect("transaction");
    recovery
        .publish_active_epoch(&transaction, epoch, &root)
        .expect("active epoch pointer");
    store
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
    let old_store = open_fresh_active_epoch_store_for_test(&namespace, 2);
    let publication = old_store.acquire_publication().expect("publication");
    let inventory = initialize_process_inventory(old_store, &publication);
    drop(publication);

    let _new_root = publish_active_epoch_for_test(&namespace, 3);
    let error = inventory
        .prepare_spawn(process_spawn_request("stale-inventory-attempt"))
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
    initialize_process_inventory(fresh_store, &fresh_publication);
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

const E02_REAPED_ATTACHED_OWNER_NAMESPACE_ENV: &str = "SIGIL_E02_REAPED_ATTACHED_OWNER_NAMESPACE";

/// Builds the durable state in a separate owned process so the parent recovery probe cannot
/// short-circuit on the bootstrap owner's liveness. The child keeps the successfully attached
/// entry after it has reaped its direct child: recovery must re-observe that exact subject before
/// the durable weak-native-exposure frontier refuses a fresh-epoch conclusion.
#[test]
#[ignore]
fn r71_e02_reaped_attached_owner_child_fixture() {
    use crate::process_inventory::AuthorityProcessInventoryPortV1;

    let namespace = std::path::PathBuf::from(
        std::env::var_os(E02_REAPED_ATTACHED_OWNER_NAMESPACE_ENV)
            .expect("fixture namespace supplied by parent test"),
    );
    let root = namespace
        .join(EPOCHS_DIRECTORY_NAME)
        .join("epoch-1-e02-reaped-attached-owner");
    let store =
        AuthorityBootstrapStoreV1::open(&namespace, &root, 1).expect("owned child bootstrap store");
    let recovery = AuthorityBootstrapRecoveryNamespaceV1 {
        namespace: namespace.clone(),
    };
    let transaction = recovery
        .acquire_transaction()
        .expect("child recovery transaction");
    recovery
        .publish_active_epoch(&transaction, 1, &root)
        .expect("publish child active root");
    drop(transaction);
    let publication = store.acquire_publication().expect("child publication");
    let inventory = initialize_process_inventory(store, &publication);
    drop(publication);

    let claim = inventory
        .prepare_spawn(process_spawn_request("e02-reaped-attached-child"))
        .expect("prepare durable target claim");
    let mut target = spawn_short_lived_child_for_e02_test().expect("owned target child");
    let registration = claim
        .register_spawned_process(target.id())
        .expect("register owned target birth");
    inventory
        .attach_spawn(&claim, registration)
        .expect("attach owned target birth");
    target.wait().expect("reap owned target child");
    // Deliberately do not settle the attached entry. The parent probe must see this exact
    // reaped subject, then reject the epoch because pre-spawn native exposure remains durable.
}

fn spawn_reaped_attached_owner_child(namespace: &std::path::Path) {
    let status =
        std::process::Command::new(std::env::current_exe().expect("current test executable"))
            .args([
                "--ignored",
                "--exact",
                "bootstrap::tests::r71_e02_reaped_attached_owner_child_fixture",
            ])
            .env(E02_REAPED_ATTACHED_OWNER_NAMESPACE_ENV, namespace)
            .status()
            .expect("run owned reaped-attached owner fixture child");
    assert!(
        status.success(),
        "owned reaped-attached owner fixture succeeds"
    );
}

/// E02 integration guard: an attached owned child may be re-observed as quiescent only against
/// its authenticated birth/scope subject, but that narrow fact still cannot prove a fresh epoch.
/// The durable weak-native-exposure frontier must block the recovery conclusion after reaping.
#[test]
fn r71_e02_reaped_attached_pid_never_proves_old_epoch_quiescence() {
    let temp = tempfile::tempdir().expect("tempdir");
    let base = std::fs::canonicalize(temp.path()).expect("canonical tempdir");
    let namespace = base.join("authority-namespace");
    spawn_reaped_attached_owner_child(&namespace);

    let factory = sigil_process_observer::ProcessObserverFactoryV1::new(canonical_bootstrap_hash(
        b"e02-reaped-attached-pid-test",
    ))
    .expect("observer")
    .instantiate();
    let service = AuthorityBootstrapRecoveryServiceV1::from_namespace(
        AuthorityBootstrapRecoveryNamespaceV1 { namespace },
        factory,
    );
    let error = service
        .probe_old_epoch_quiescence(canonical_bootstrap_hash(b"e02-evidence"))
        .expect_err("reaped attached target must not prove fresh-epoch quiescence");
    assert_eq!(error, AuthorityBootstrapRecoveryErrorV1::NoQuiescence);
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
