use super::*;

#[test]
fn r71_execution_temp_and_session_scratch_never_alias() {
    let temp = Path::new("/tmp/sigil-et");
    let scratch = Path::new("/tmp/sigil-et");
    let err = assert_no_temp_scratch_alias(temp, scratch).expect_err("same dir must fail");
    assert!(matches!(err, ArenaErrorV1::TempScratchAlias));

    let scratch2 = Path::new("/tmp/sigil-et/session");
    let err2 = assert_no_temp_scratch_alias(temp, scratch2).expect_err("nested must fail");
    assert!(matches!(err2, ArenaErrorV1::TempScratchAlias));

    let ok =
        assert_no_temp_scratch_alias(Path::new("/tmp/sigil-et"), Path::new("/tmp/sigil-scratch"));
    assert!(ok.is_ok());
}

#[test]
fn r71_env_mapping_contract_is_closed() {
    assert_eq!(ExecutionTempEnvMappingV1::Tmpdir.var_name(), "TMPDIR");
    assert_eq!(ExecutionTempEnvMappingV1::Home.var_name(), "HOME");
    assert_eq!(
        ExecutionTempEnvMappingV1::SigilCacheHome.var_name(),
        "SIGIL_CACHE_HOME"
    );
    assert_eq!(
        ExecutionTempEnvMappingV1::XdgStateHome.layout_path(),
        "state"
    );
}

#[test]
fn r71_execution_temp_root_is_attempt_generation_scoped() {
    let root = execution_temp_root(Path::new("/base"), "attempt-1", 3);
    let expected = format!(
        "base{}attempt-1{}3",
        std::path::MAIN_SEPARATOR,
        std::path::MAIN_SEPARATOR
    );
    assert!(root.ends_with(&expected));
}

#[test]
fn r71_execution_temp_authority_materializes_and_releases_complete_layout() {
    let base = tempfile::tempdir().expect("base");
    let authority = ExecutionTempAuthorityV1::new(base.path());
    let generation = authority
        .provision("attempt-1", 1)
        .expect("provision execution temp");
    let root = generation.binding().root.clone();
    for relative in [
        "tmp",
        "home",
        "state",
        "cache",
        "sigil-state",
        "sigil-cache",
        "config",
    ] {
        assert!(root.join(relative).is_dir(), "missing {relative}");
    }
    generation.finalize().expect("release generation");
    assert!(!root.exists());
    assert_eq!(std::fs::read_dir(base.path()).expect("base").count(), 0);
}

#[test]
fn r71_execution_temp_authority_rejects_path_traversal_identity() {
    let base = tempfile::tempdir().expect("base");
    let authority = ExecutionTempAuthorityV1::new(base.path());
    let error = authority
        .provision("../outside", 1)
        .expect_err("path traversal must fail");
    assert!(matches!(error, ArenaErrorV1::InvalidAttemptId(_)));
}
