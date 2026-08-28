use super::*;

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
