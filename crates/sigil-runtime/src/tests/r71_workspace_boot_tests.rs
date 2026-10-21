use super::*;
use sigil_resource_authority::bootstrap::{
    AuthorityBootstrapObjectClassV1 as Object, AuthorityBootstrapStoreV1 as Bootstrap,
};

#[test]
fn workspace_boot_adopts_exact_legacy_snapshot_without_replacing_identity_or_records() {
    let _environment_guard = crate::test_env::lock();
    let fixture = tempfile::tempdir().expect("fixture");
    let config = fixture.path().join("sigil.toml");
    write_r71_boot_config(&config);
    let boot = boot_current_schema(&config, fixture.path()).expect("initial boot");
    let manifest = boot.cutover().manifest().clone();
    let writer = &boot.composition().storage_writer;
    let lease = writer
        .acquire_named(StorageWriterChannelV1::SessionLog, "legacy-session")
        .expect("lease");
    writer
        .write_record(&lease, b"retained session record")
        .expect("record");
    writer.finalize(lease).expect("settle");
    drop(boot);
    let original = Bootstrap::for_config_path(&config).expect("original realm");
    // This is the exact pre-upgrade layout: all existing boot metadata, no workspace binding.
    std::fs::remove_file(original.path(Object::WorkspaceBinding)).expect("legacy fixture");
    let pointer = std::fs::read(original.path(Object::CutoverPointer)).expect("pointer");
    let adopted = boot_current_schema(&config, fixture.path()).expect("adopt original workspace");
    assert_eq!(*adopted.cutover().manifest(), manifest);
    assert_eq!(
        std::fs::read(original.path(Object::CutoverPointer)).expect("pointer"),
        pointer
    );
    let lease = adopted
        .composition()
        .storage_writer
        .acquire_named(StorageWriterChannelV1::SessionLog, "legacy-session")
        .expect("same source");
    assert_eq!(
        adopted
            .composition()
            .storage_writer
            .read_record_bytes(&lease, 1024)
            .expect("retained bytes"),
        b"retained session record\n"
    );
    adopted
        .composition()
        .storage_writer
        .finalize(lease)
        .expect("settle");
    let guard = original.acquire_publication().expect("publication");
    assert!(
        original
            .workspace_binding(&guard)
            .expect("binding")
            .is_some()
    );
    drop(guard);
    let b = fixture.path().join("new-workspace");
    std::fs::create_dir(&b).expect("workspace B");
    let new_workspace = boot_current_schema(&config, &b).expect("independent workspace B");
    let notice = new_workspace
        .composition()
        .legacy_shared_history
        .as_ref()
        .expect("legacy navigation notice");
    assert_eq!(
        notice.workspace_id,
        crate::workspace_id_for_root(fixture.path())
    );
    assert_eq!(
        notice.source_state,
        crate::LegacySharedSessionHistorySourceState::Present
    );
    let lifecycle =
        crate::LocalSessionLifecycleService::new("b", b.join("sessions"), b.join("exports"))
            .with_legacy_shared_history(Some(notice.clone()));
    let catalog = lifecycle.catalog().expect("empty new workspace catalog");
    assert!(catalog.entries.is_empty());
    assert_eq!(catalog.legacy_shared_history.as_ref(), Some(notice));
}

#[test]
fn workspace_boot_diagnostic_lookup_before_first_boot_does_not_create_legacy_failure() {
    let _environment_guard = crate::test_env::lock();
    let fixture = tempfile::tempdir().expect("fixture");
    let config = fixture.path().join("sigil.toml");
    write_r71_boot_config(&config);
    let path = authority_bootstrap_manifest_path(&config).expect("diagnostic pointer");
    assert!(!path.parent().expect("realm").exists());
    let boot = boot_current_schema(&config, fixture.path()).expect("first boot after diagnostics");
    assert_eq!(boot.cutover().manifest().application_generation, 1);
}

#[test]
fn workspace_boot_legacy_source_observation_keeps_unknown_distinct_from_empty() {
    use crate::LegacySharedSessionHistorySourceState as State;
    let fixture = tempfile::tempdir().expect("fixture");
    let root = fixture.path().join("sources");
    assert_eq!(observe_legacy_shared_sources(&root), None);
    std::fs::write(&root, "not a source directory").expect("invalid root");
    assert_eq!(
        observe_legacy_shared_sources(&root),
        Some(State::Unverified)
    );
    std::fs::remove_file(&root).expect("remove invalid root");
    std::fs::create_dir(&root).expect("empty root");
    assert_eq!(observe_legacy_shared_sources(&root), None);
    let entry = root.join("session");
    std::fs::create_dir(&entry).expect("source directory");
    std::fs::write(entry.join("records.jsonl"), "retained bytes\n").expect("source record");
    assert_eq!(observe_legacy_shared_sources(&root), Some(State::Present));
}

#[test]
fn workspace_boot_legacy_access_anchor_accepts_atomic_config_and_workspace_changes() {
    let _environment_guard = crate::test_env::lock();
    let fixture = tempfile::tempdir().expect("fixture");
    let config = fixture.path().join("sigil.toml");
    write_r71_boot_config(&config);
    let initial = boot_current_schema(&config, fixture.path()).expect("initial boot");
    let authority = initial.composition().authority_generation();
    let generation = initial.cutover().manifest().application_generation;
    let source = initial
        .composition()
        .storage_writer
        .acquire_named(StorageWriterChannelV1::SessionLog, "retained-source")
        .expect("legacy source");
    initial
        .composition()
        .storage_writer
        .write_record(&source, b"retained legacy bytes")
        .expect("records");
    initial
        .composition()
        .storage_writer
        .finalize(source)
        .expect("settle legacy source");
    drop(initial);
    let original = Bootstrap::for_config_path(&config).expect("realm");
    std::fs::remove_file(original.path(Object::WorkspaceBinding)).expect("legacy fixture");
    let other = fixture.path().join("other");
    std::fs::create_dir(&other).expect("other workspace");
    let replacement = fixture.path().join("replacement.toml");
    let updated = std::fs::read_to_string(&config)
        .expect("config")
        .replace("model = \"test\"", "model = \"updated\"");
    std::fs::write(&replacement, updated).expect("replacement");
    std::fs::rename(&replacement, &config).expect("atomic config replacement");
    let migrated = boot_current_schema(&config, &other).expect("legacy shared access anchor");
    assert_eq!(migrated.composition().authority_generation(), authority);
    assert_eq!(migrated.config().agent.model, "updated");
    assert_eq!(
        migrated.cutover().manifest().application_generation,
        generation + 1
    );
    let source = migrated
        .composition()
        .storage_writer
        .acquire_named(StorageWriterChannelV1::SessionLog, "retained-source")
        .expect("same legacy source");
    assert_eq!(
        migrated
            .composition()
            .storage_writer
            .read_record_bytes(&source, 1024)
            .expect("unchanged records"),
        b"retained legacy bytes\n"
    );
    migrated
        .composition()
        .storage_writer
        .finalize(source)
        .expect("settle retained source");
    let publication = original.acquire_publication().expect("publication");
    assert_eq!(
        original
            .workspace_binding(&publication)
            .expect("access anchor")
            .expect("binding")
            .canonical_root(),
        other.canonicalize().expect("other canonical root")
    );
    drop(publication);
    assert!(!original.root().join("workspace-realms-v1").exists());
    let prior_workspace = boot_current_schema(&config, fixture.path()).expect("prior workspace");
    let navigation = prior_workspace
        .composition()
        .legacy_shared_history
        .as_ref()
        .expect("shared history navigation");
    assert_eq!(
        navigation.workspace_id,
        crate::workspace_id_for_root(&other)
    );
    assert_eq!(navigation.workspace_label, "other");
}

#[test]
fn workspace_boot_isolates_storage_and_preserves_each_realm_across_config_change() {
    let _environment_guard = crate::test_env::lock();
    let fixture = tempfile::tempdir().expect("fixture");
    let config = fixture.path().join("sigil.toml");
    write_r71_boot_config(&config);
    let b = fixture.path().join("b");
    std::fs::create_dir(&b).expect("workspace B");
    let a_boot = boot_current_schema(&config, fixture.path()).expect("workspace A");
    let b_boot = boot_current_schema(&config, &b).expect("workspace B");
    let a_identity = a_boot.composition().authority_generation();
    let b_identity = b_boot.composition().authority_generation();
    assert_ne!(a_identity, b_identity);
    let a_root = a_boot
        .composition()
        .storage_writer
        .managed_leaf_path(StorageWriterChannelV1::SessionLog)
        .expect("A source");
    let b_root = b_boot
        .composition()
        .storage_writer
        .managed_leaf_path(StorageWriterChannelV1::SessionLog)
        .expect("B source");
    assert_ne!(a_root, b_root);
    assert!(
        a_root.starts_with(
            a_boot
                .resolved_paths()
                .state_root
                .canonicalize()
                .expect("A state anchor")
        )
    );
    assert!(
        b_root.starts_with(
            b_boot
                .resolved_paths()
                .workspace_state_root
                .join("authority-v1")
                .canonicalize()
                .expect("B state anchor")
        )
    );
    let changed = std::fs::read_to_string(&config)
        .expect("config")
        .replace("model = \"test\"", "model = \"updated\"");
    std::fs::write(&config, changed).expect("save config");
    assert_eq!(
        boot_current_schema(&config, &b)
            .expect("updated B")
            .composition()
            .authority_generation(),
        b_identity
    );
    assert_eq!(
        boot_current_schema(&config, fixture.path())
            .expect("updated A")
            .composition()
            .authority_generation(),
        a_identity
    );
}
