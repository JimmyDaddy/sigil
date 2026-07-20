use super::*;

#[test]
fn appearance_store_defaults_invalid_missing_and_oversized_files_to_system() {
    let temp = tempfile::tempdir().expect("temporary directory should create");
    let missing = AppearanceStore::load(temp.path().join("missing.json"));
    assert_eq!(missing.preference(), ThemePreference::System);

    let invalid_path = temp.path().join("invalid.json");
    std::fs::write(
        &invalid_path,
        br#"{"schemaVersion":2,"themePreference":"dark"}"#,
    )
    .expect("invalid fixture should write");
    assert_eq!(
        AppearanceStore::load(invalid_path).preference(),
        ThemePreference::System
    );

    let oversized_path = temp.path().join("oversized.json");
    std::fs::write(
        &oversized_path,
        vec![b'x'; MAX_APPEARANCE_FILE_BYTES as usize + 1],
    )
    .expect("oversized fixture should write");
    assert_eq!(
        AppearanceStore::load(oversized_path).preference(),
        ThemePreference::System
    );
}

#[test]
fn appearance_store_persists_only_the_bounded_versioned_enum() {
    let temp = tempfile::tempdir().expect("temporary directory should create");
    let path = temp.path().join("state/appearance-v1.json");
    let mut store = AppearanceStore::load(path.clone());
    store
        .set(ThemePreference::Dark)
        .expect("appearance preference should persist");
    store
        .set(ThemePreference::Light)
        .expect("appearance preference should replace atomically");
    store
        .set(ThemePreference::Dark)
        .expect("appearance preference should replace repeatedly");
    assert_eq!(store.preference(), ThemePreference::Dark);
    assert_eq!(
        AppearanceStore::load(path.clone()).preference(),
        ThemePreference::Dark
    );

    let value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path).expect("appearance preference should read"))
            .expect("appearance preference should be valid JSON");
    assert_eq!(value["schemaVersion"], 1);
    assert_eq!(value["themePreference"], "dark");
    assert_eq!(value.as_object().map(serde_json::Map::len), Some(2));
}

#[test]
fn appearance_store_keeps_the_previous_preference_when_persistence_fails() {
    let temp = tempfile::tempdir().expect("temporary directory should create");
    let parent_file = temp.path().join("not-a-directory");
    std::fs::write(&parent_file, b"occupied").expect("parent fixture should write");
    let mut store = AppearanceStore::load(parent_file.join("appearance.json"));
    let error = store
        .set(ThemePreference::Light)
        .expect_err("invalid parent should fail");
    assert_eq!(error.to_string(), "appearance preference is unavailable");
    assert_eq!(store.preference(), ThemePreference::System);
}

#[test]
fn initialization_script_contains_only_the_frozen_enum() {
    let script = initialization_script(ThemePreference::Light);
    assert_eq!(
        script,
        "Object.defineProperty(window, '__SIGIL_THEME_PREFERENCE__', { value: 'light', writable: false, configurable: false });"
    );
}
