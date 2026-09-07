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
