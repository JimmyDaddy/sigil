use super::*;

#[test]
fn r71_reserved_standard_env_has_exactly_eight_keys_without_scratch() {
    let temp = Path::new("/et");
    let env = standard_reserved_environment(temp);
    assert_eq!(env.len(), 8, "standard reserved profile is eight keys");
    assert_eq!(env.get("TMPDIR").expect("tmpdir"), "/et/tmp");
    assert_eq!(env.get("HOME").expect("home"), "/et/home");
    assert!(!env.contains_key("SIGIL_SCRATCH_DIR"));
}

#[test]
fn r71_reserved_override_is_classified_and_standard_wins() {
    let temp = Path::new("/et");
    let standard = standard_reserved_environment(temp);
    let mut candidate = BTreeMap::new();
    candidate.insert("HOME".to_owned(), "/home/attacker".to_owned());
    let (classification, effective) =
        apply_reserved_environment(&mut candidate, &standard, None, false);
    assert_eq!(classification, ReservedEnvOverrideV1::OverrideAttempt);
    assert_eq!(effective.get("HOME").expect("home"), "/et/home");
}

#[test]
fn r71_scratch_dir_present_only_when_requirement_has_session_scratch() {
    let mut candidate = BTreeMap::new();
    let standard = standard_reserved_environment(Path::new("/et"));
    let (_, with_scratch) = apply_reserved_environment(
        &mut candidate,
        &standard,
        Some(Path::new("/scratch/data")),
        false,
    );
    assert_eq!(
        with_scratch.get("SIGIL_SCRATCH_DIR").expect("scratch"),
        "/scratch/data"
    );
    assert_ne!(
        with_scratch.get("TMPDIR").expect("tmpdir"),
        with_scratch.get("SIGIL_SCRATCH_DIR").expect("scratch"),
        "TMPDIR and SIGIL_SCRATCH_DIR may never alias"
    );
}
