use super::*;

#[test]
fn recovery_record_recheck_rejects_a_replaced_marker() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let config_path = temp.path().join("sigil.toml");
    let first_id = CredentialId::random();
    persist_legacy_migration_recovery(
        &config_path,
        LegacyMigrationRecoveryState::RollbackIncomplete,
        CredentialStorageMode::File,
        std::slice::from_ref(&first_id),
    )
    .expect("first recovery record should publish");
    let first = read_legacy_migration_recovery(&config_path)
        .expect("first recovery record should read")
        .expect("first recovery record should exist");

    persist_legacy_migration_recovery(
        &config_path,
        LegacyMigrationRecoveryState::RollbackIncomplete,
        CredentialStorageMode::Keyring,
        &[CredentialId::random()],
    )
    .expect("replacement recovery record should publish");

    assert!(
        !legacy_migration_recovery_matches(&config_path, &first)
            .expect("replacement comparison should be available")
    );
}

#[test]
fn recovery_record_binds_cleanup_to_the_original_storage_mode() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let config_path = temp.path().join("sigil.toml");
    persist_legacy_migration_recovery(
        &config_path,
        LegacyMigrationRecoveryState::RollbackIncomplete,
        CredentialStorageMode::File,
        &[CredentialId::random()],
    )
    .expect("recovery record should publish");

    let recovery = read_legacy_migration_recovery(&config_path)
        .expect("recovery record should read")
        .expect("recovery record should exist");

    assert_eq!(recovery.credential_store, Some(CredentialStorageMode::File));
    let bytes = recovery
        .encode()
        .expect("current recovery record should encode");
    assert!(bytes.starts_with(b"sigil-provider-migration-recovery-v3\n"));
    assert!(
        bytes
            .windows(b"credential_store=file".len())
            .any(|window| { window == b"credential_store=file" })
    );
}
