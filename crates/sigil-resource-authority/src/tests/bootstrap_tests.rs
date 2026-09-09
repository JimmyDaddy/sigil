use super::*;

#[test]
fn bootstrap_resolver_requires_explicit_roots() {
    let error = BootstrapRootResolverV1::default()
        .resolve()
        .expect_err("bootstrap must fail closed without explicit roots");
    assert!(matches!(error, BootstrapErrorV1::StateRootUnavailable));
}

#[cfg(unix)]
#[test]
fn bootstrap_rejects_symlinked_anchor() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("tempdir");
    let target = temp.path().join("state-target");
    std::fs::create_dir(&target).expect("target");
    let link = temp.path().join("state-link");
    symlink(&target, &link).expect("link");
    let roots = AuthorityBootstrapRoots {
        state_anchor: link,
        cache_anchor: temp.path().join("cache"),
        execution_temp_anchor: temp.path().join("execution-temp"),
        state_identity: canonical_bootstrap_hash(b"state"),
        cache_identity: canonical_bootstrap_hash(b"cache"),
        execution_temp_identity: canonical_bootstrap_hash(b"execution-temp"),
        manifest_hash: canonical_bootstrap_hash(b"manifest"),
    };
    assert!(matches!(
        roots.validate_anchors(),
        Err(BootstrapErrorV1::NotPlainDirectory(_))
    ));
}

#[test]
fn current_bootstrap_store_has_stable_identity_and_private_objects() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = AuthorityBootstrapStoreV1::open_owned_temp_fixture(&temp, "current-authority", 1)
        .expect("current store");
    assert!(store.was_created_for_this_open());
    let publication = store.acquire_publication().expect("publication");
    let first = store
        .authority_instance_hash(&publication)
        .expect("authority identity");
    store
        .publish_bytes(
            &publication,
            AuthorityBootstrapObjectClassV1::BootstrapManifest,
            br#"{\"schema_version\":1}"#,
        )
        .expect("publish current object");
    let bytes = store
        .read_bytes(
            &publication,
            AuthorityBootstrapObjectClassV1::BootstrapManifest,
        )
        .expect("read current object")
        .expect("object exists");
    assert_eq!(bytes, br#"{\"schema_version\":1}"#);
    assert_eq!(
        first,
        store
            .authority_instance_hash(&publication)
            .expect("stable identity")
    );
}

#[test]
fn bootstrap_hash_is_stable() {
    assert_eq!(
        canonical_bootstrap_hash(b"payload"),
        canonical_bootstrap_hash(b"payload")
    );
}

#[test]
fn bootstrap_admission_keeps_fresh_open_ahead_of_a_competing_publisher() {
    use std::sync::mpsc;
    use std::time::Duration;

    let temp = tempfile::tempdir().expect("fixture");
    let root = temp
        .path()
        .canonicalize()
        .expect("canonical fixture")
        .join("boot");
    let (created_tx, created_rx) = mpsc::channel();
    let (admit_tx, admit_rx) = mpsc::channel();
    let (published_tx, published_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let first_root = root.clone();
    let first = std::thread::spawn(move || {
        ADMITTED_BOOTSTRAP_OPEN_TEST_HOOK.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                created_tx.send(()).expect("report fresh root");
                admit_rx
                    .recv_timeout(Duration::from_secs(5))
                    .expect("admit first publisher");
            }));
        });
        let (store, publication) = AuthorityBootstrapStoreV1::open_with_publication(first_root, 1)
            .expect("first admitted open");
        assert!(store.was_created_for_this_open());
        store
            .publish_bytes(
                &publication,
                AuthorityBootstrapObjectClassV1::BootstrapManifest,
                br#"{"first_publication":true}"#,
            )
            .expect("first publication");
        published_tx.send(()).expect("report first publication");
        release_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("release first publication");
        drop(publication);
        store
    });
    created_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("fresh root was created");
    assert!(root.is_dir());
    let (attempted_tx, attempted_rx) = mpsc::channel();
    let (second_opened_tx, second_opened_rx) = mpsc::channel();
    let (finished_tx, finished_rx) = mpsc::channel();
    let second_root = root.clone();
    let second = std::thread::spawn(move || {
        let probe = open_private_lock_file(
            &bootstrap_admission_lock_path(&second_root).expect("admission path"),
        )
        .expect("independent admission descriptor");
        let error = probe
            .try_lock_exclusive()
            .expect_err("fresh initializer owns admission");
        assert!(
            error.kind() == std::io::ErrorKind::WouldBlock
                || error.raw_os_error() == fs2::lock_contended_error().raw_os_error()
        );
        drop(probe);
        attempted_tx.send(()).expect("report contended admission");
        ADMITTED_BOOTSTRAP_OPEN_TEST_HOOK.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                second_opened_tx
                    .send(())
                    .expect("report second admitted open");
            }));
        });
        let (store, publication) = AuthorityBootstrapStoreV1::open_with_publication(second_root, 1)
            .expect("second admitted open");
        assert!(!store.was_created_for_this_open());
        assert_eq!(
            store
                .read_bytes(
                    &publication,
                    AuthorityBootstrapObjectClassV1::BootstrapManifest
                )
                .expect("read first publication"),
            Some(br#"{"first_publication":true}"#.to_vec()),
        );
        finished_tx
            .send(())
            .expect("report second publication guard");
        drop(publication);
        store
    });
    attempted_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("second open attempted admission");
    assert!(matches!(
        second_opened_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    admit_tx
        .send(())
        .expect("allow first publication acquisition");
    published_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("first publication was written");
    second_opened_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("second root opened after admission");
    assert!(matches!(
        finished_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    release_tx
        .send(())
        .expect("allow second publication acquisition");
    finished_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("second publisher observed first state");
    let first = first.join().expect("first publisher thread");
    let second = second.join().expect("second publisher thread");
    assert_eq!(first.root(), second.root());
    assert!(first.was_created_for_this_open());
    assert!(!second.was_created_for_this_open());
}

#[cfg(unix)]
#[test]
fn bootstrap_admission_rejects_symlink_and_shared_permission_lock_files() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    for symlink_lock in [true, false] {
        let temp = tempfile::tempdir().expect("fixture");
        let parent = temp.path().canonicalize().expect("canonical fixture");
        let root = parent.join("boot");
        let admission_path = bootstrap_admission_lock_path(&root).expect("admission path");
        let target = parent.join("untouched");
        std::fs::write(&target, b"retained bytes").expect("write target");
        if symlink_lock {
            symlink(&target, &admission_path).expect("symlink admission lock");
        } else {
            std::fs::write(&admission_path, b"").expect("write admission lock");
            std::fs::set_permissions(&admission_path, std::fs::Permissions::from_mode(0o644))
                .expect("shared admission permissions");
        }
        assert!(AuthorityBootstrapStoreV1::open_with_publication(root.clone(), 1).is_err());
        assert!(
            !root.exists(),
            "unsafe admission must not initialize a root"
        );
        assert_eq!(
            std::fs::read(&target).expect("unchanged target"),
            b"retained bytes"
        );
        if symlink_lock {
            assert!(
                std::fs::symlink_metadata(&admission_path)
                    .expect("retained symlink")
                    .file_type()
                    .is_symlink()
            );
        } else {
            assert_eq!(
                std::fs::metadata(&admission_path)
                    .expect("retained lock")
                    .permissions()
                    .mode()
                    & 0o777,
                0o644
            );
        }
    }
}
