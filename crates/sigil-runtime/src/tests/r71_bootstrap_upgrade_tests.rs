use super::*;
use sigil_kernel::cutover_manifest::{
    CutoverManifestV1, validate_bootstrap_cutover_predecessor_v1,
    validate_optional_terminal_cutover_predecessor_v2,
};
use sigil_resource_authority::bootstrap::{
    AuthorityBootstrapObjectClassV1 as Object, AuthorityBootstrapStoreV1 as Bootstrap,
    BootstrapErrorV1,
};

// An independent fixture encoder retains the historical six-field hash order. It never
// invokes the production predecessor codec to create or repair historical evidence.
fn schema1_pointer(mut fields: serde_json::Value) -> Vec<u8> {
    fields["schema_version"] = 1.into();
    let probes: Vec<String> = fields["mandatory_readiness"]
        .as_array()
        .expect("readiness rows")
        .iter()
        .map(|row| {
            format!(
                "{{\"adapter\":{},\"passed\":{},\"evidence_digest\":{}}}",
                row["adapter"], row["passed"], row["evidence_digest"]
            )
        })
        .collect();
    let mut bytes = format!(
        "{{\"schema_version\":1,\"application_instance_id\":{},\"selected_epoch\":{},\"application_generation\":{},\"authority_generation_digest\":{},\"mandatory_readiness\":[{}]}}",
        fields["application_instance_id"],
        fields["selected_epoch"],
        fields["application_generation"],
        fields["authority_generation_digest"],
        probes.join(","),
    );
    let hash = sigil_kernel::external::sha256_hex(bytes.as_bytes());
    bytes.pop();
    bytes.push_str(&format!(",\"manifest_hash\":\"{hash}\"}}"));
    bytes.into_bytes()
}

fn publish_fixture_pointer(bootstrap: &Bootstrap, bytes: &[u8]) {
    let publication = bootstrap
        .acquire_publication()
        .expect("fixture publication");
    bootstrap
        .publish_bytes(&publication, Object::CutoverPointer, bytes)
        .expect("fixture pointer");
}

fn metadata_snapshot(bootstrap: &Bootstrap) -> Vec<Option<Vec<u8>>> {
    [
        Object::BootstrapManifest,
        Object::AuthorityConfigGeneration,
        Object::CutoverPointer,
        Object::ProcessInventory,
        Object::ProcessInventoryRequirement,
        Object::ProcessInventoryAuthenticator,
    ]
    .into_iter()
    .map(|class| match std::fs::read(bootstrap.path(class)) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => panic!("read fixture metadata: {error}"),
    })
    .collect()
}

#[test]
fn r71_bootstrap_interrupted_after_root_creation_keeps_missing_metadata_fail_closed() {
    let _environment_guard = crate::test_env::lock();
    let dir = tempfile::tempdir().expect("fixture");
    let config = dir.path().join("sigil.toml");
    write_r71_boot_config(&config);
    let snapshot = ValidatedAuthorityConfigSnapshotV1::load(&config, dir.path())
        .expect("load snapshot")
        .expect("valid snapshot");
    let (bootstrap, publication) =
        Bootstrap::for_canonical_config_path_with_publication(snapshot.config_path())
            .expect("admitted fresh boot");
    assert!(bootstrap.was_created_for_this_open());
    let before = metadata_snapshot(&bootstrap);
    assert!(before.iter().all(Option::is_none));
    drop(publication);

    let result = build_current_boot_transaction(snapshot);
    assert!(matches!(
        result,
        Err(BootAuthorityErrorV1::Bootstrap(
            BootstrapErrorV1::MetadataCorrupted(_)
        ))
    ));
    assert_eq!(metadata_snapshot(&bootstrap), before);
    let reopened = Bootstrap::for_config_path(&config).expect("existing bootstrap");
    assert!(!reopened.was_created_for_this_open());
    std::fs::remove_dir_all(bootstrap.root()).expect("cleanup bootstrap fixture");
}

fn with_schema1_fixture(run: impl FnOnce(&Path, &Path, &Bootstrap, &CutoverManifestV1, &[u8])) {
    let _environment_guard = crate::test_env::lock();
    let dir = tempfile::tempdir().expect("fixture");
    let config = dir.path().join("sigil.toml");
    write_r71_boot_config(&config);
    let first = boot_current_schema(&config, dir.path()).expect("initial boot");
    let current = first.cutover().manifest().clone();
    assert_eq!(current.mandatory_readiness.len(), 18);
    assert!(current.mandatory_readiness.iter().all(|probe| probe.passed));
    drop(first);
    let bootstrap = Bootstrap::for_config_path(&config).expect("fixture bootstrap");
    let historical = schema1_pointer(serde_json::to_value(&current).expect("manifest fields"));
    validate_bootstrap_cutover_predecessor_v1(&historical).expect("valid historical fixture");
    publish_fixture_pointer(&bootstrap, &historical);
    run(dir.path(), &config, &bootstrap, &current, &historical);
}

#[test]
fn r71_bootstrap_upgrade_unchanged_config_advances_once_and_rebinds_real_storage() {
    with_schema1_fixture(|cwd, config, bootstrap, old, historical| {
        let raw_config = std::fs::read(config).expect("original config");
        let old_hash = validate_bootstrap_cutover_predecessor_v1(historical)
            .expect("predecessor")
            .manifest_hash();
        let upgraded = boot_current_schema(config, cwd).expect("schema-1 upgrade");
        let manifest = upgraded.cutover().manifest().clone();
        assert_eq!(manifest.schema_version, 2);
        assert_eq!(
            manifest.application_instance_id,
            old.application_instance_id
        );
        assert_eq!(
            manifest.authority_generation_digest,
            old.authority_generation_digest
        );
        assert_eq!(
            manifest.application_generation,
            old.application_generation + 1
        );
        assert_eq!(manifest.mandatory_readiness.len(), 18);
        assert!(
            manifest
                .mandatory_readiness
                .iter()
                .all(|probe| probe.passed)
        );
        assert_ne!(manifest.manifest_hash, old_hash);
        assert_eq!(std::fs::read(config).expect("unchanged config"), raw_config);
        assert_eq!(
            upgraded.resolved_paths().state_root,
            cwd.join(".r71-test-state")
        );
        assert_eq!(
            upgraded.resolved_paths().cache_root,
            cwd.join(".r71-test-cache")
        );
        let persisted = RuntimeGlobalCutoverV1::load_and_validate_manifest(
            &bootstrap.path(Object::CutoverPointer),
        )
        .expect("strict current pointer");
        assert_eq!(persisted, manifest);

        let writer = &upgraded.composition().storage_writer;
        let lease = writer
            .acquire_named(StorageWriterChannelV1::SessionLog, "schema-1-upgrade")
            .expect("source-bound admission");
        writer
            .write_record(&lease, b"upgrade-record")
            .expect("managed write");
        assert_eq!(
            writer
                .read_record_bytes(&lease, 1024)
                .expect("managed read"),
            b"upgrade-record\n"
        );
        writer.finalize(lease).expect("managed settlement");
        let wrong_source = probe_mandatory_adapters(
            &upgraded.composition().services,
            &ApplicationResourceRecoveryFacadeV1::new(),
            old_hash,
            old.application_generation,
        );
        assert!(wrong_source.iter().any(|probe| {
            probe.adapter == MandatoryAdapterKindV1::StorageSessionLog && !probe.passed
        }));
        drop(upgraded);

        let pointer_before =
            std::fs::read(bootstrap.path(Object::CutoverPointer)).expect("pointer");
        let generation_before =
            std::fs::read(bootstrap.path(Object::AuthorityConfigGeneration)).expect("generation");
        let replay = boot_current_schema(config, cwd).expect("idempotent replay");
        assert_eq!(*replay.cutover().manifest(), manifest);
        assert_eq!(
            std::fs::read(bootstrap.path(Object::CutoverPointer)).expect("replayed pointer"),
            pointer_before
        );
        assert_eq!(
            std::fs::read(bootstrap.path(Object::AuthorityConfigGeneration))
                .expect("replayed generation"),
            generation_before
        );
    });
}

#[test]
fn r71_bootstrap_upgrade_changed_config_qualifies_current_core_composition() {
    with_schema1_fixture(|cwd, config, bootstrap, old, _| {
        let standard_source = std::fs::read_to_string(config).expect("config source");
        let core_source = format!("{standard_source}\n[composition]\nprofile = \"core\"\n");
        std::fs::write(config, &core_source).expect("core config");
        let upgraded = boot_current_schema(config, cwd).expect("core upgrade");
        let manifest = upgraded.cutover().manifest();
        assert_eq!(
            manifest.application_generation,
            old.application_generation + 1
        );
        assert_eq!(manifest.composition, RuntimeCompositionConfig::core());
        assert_eq!(manifest.mandatory_readiness.len(), 14);
        assert!(
            manifest
                .mandatory_readiness
                .iter()
                .all(|probe| probe.passed)
        );
        assert!(upgraded.composition().extension_execution.is_none());
        assert!(
            !upgraded
                .composition()
                .declared_channels
                .contains(&StorageWriterChannelV1::DurableMemory)
        );
        assert_eq!(
            RuntimeGlobalCutoverV1::load_and_validate_manifest(
                &bootstrap.path(Object::CutoverPointer)
            )
            .expect("current pointer"),
            *manifest
        );
        let core_lease = upgraded
            .composition()
            .storage_writer
            .acquire_named(StorageWriterChannelV1::SessionLog, "warm-profile-switch")
            .expect("active core lease");
        upgraded
            .composition()
            .storage_writer
            .write_record(&core_lease, b"retained-core-record")
            .expect("core record before profile switch");

        // Both directions must work with the prior composition still alive. Its active
        // reservation stays valid; rebuilding the service must not release shared quota.
        std::fs::write(config, &standard_source).expect("restore standard");
        let standard = boot_current_schema(config, cwd).expect("warm Core to Standard");
        assert_eq!(standard.cutover().manifest().mandatory_readiness.len(), 18);
        assert!(
            standard
                .composition()
                .declared_channels
                .contains(&StorageWriterChannelV1::DurableMemory)
        );
        assert_eq!(
            upgraded
                .composition()
                .storage_writer
                .read_record_bytes(&core_lease, 1024)
                .expect("old lease retains written bytes"),
            b"retained-core-record\n"
        );
        upgraded
            .composition()
            .storage_writer
            .finalize(core_lease)
            .expect("old active core lease survives warm composition replacement");
        std::fs::write(config, &core_source).expect("select core again");
        let core_again = boot_current_schema(config, cwd).expect("warm Standard to Core");
        assert_eq!(
            core_again.cutover().manifest().mandatory_readiness.len(),
            14
        );
        assert_eq!(
            core_again.cutover().manifest().application_generation,
            old.application_generation + 3
        );
        drop(core_again);
        drop(standard);
        drop(upgraded);

        std::fs::write(config, &standard_source).expect("restore standard after shutdown");
        let cold_standard = boot_current_schema(config, cwd).expect("cold Core to Standard");
        assert_eq!(
            cold_standard.cutover().manifest().mandatory_readiness.len(),
            18
        );
        assert_eq!(
            cold_standard.cutover().manifest().application_generation,
            old.application_generation + 4
        );
        let quota: serde_json::Value = serde_json::from_slice(
            &std::fs::read(cwd.join(".r71-test-state/.authority-quota/managed-storage.json"))
                .expect("quota snapshot"),
        )
        .expect("quota fields");
        assert_eq!(quota["workspace_cap"], MANAGED_STORAGE_WORKSPACE_CAP_V1);
    });
}

#[test]
fn r71_bootstrap_upgrade_reuses_already_advanced_generation_after_interrupted_boot() {
    with_schema1_fixture(|cwd, config, bootstrap, old, _| {
        let publication = bootstrap.acquire_publication().expect("publication");
        let mut record = load_authority_config_generation(bootstrap, &publication)
            .expect("read generation")
            .expect("generation");
        record.generation = old.application_generation + 8;
        bootstrap
            .publish_bytes(
                &publication,
                Object::AuthorityConfigGeneration,
                &serde_json::to_vec(&record).expect("generation bytes"),
            )
            .expect("simulate interrupted newer composition");
        drop(publication);
        let upgraded = boot_current_schema(config, cwd).expect("resume upgrade");
        assert_eq!(
            upgraded.cutover().manifest().application_generation,
            record.generation
        );
        drop(upgraded);
        let replay = boot_current_schema(config, cwd).expect("replay resumed upgrade");
        assert_eq!(
            replay.cutover().manifest().application_generation,
            record.generation
        );
    });
}

#[test]
fn r71_bootstrap_upgrade_rejects_invalid_predecessors_before_metadata_or_root_mutation() {
    with_schema1_fixture(|cwd, config, bootstrap, old, historical| {
        let base = serde_json::to_value(old).expect("current fields");
        let mut invalid = Vec::new();
        let mut tampered: serde_json::Value =
            serde_json::from_slice(historical).expect("old fields");
        tampered["application_instance_id"] = "tampered".into();
        invalid.push((
            "hash mismatch",
            serde_json::to_vec(&tampered).expect("tampered"),
        ));
        tampered["schema_version"] = 99.into();
        invalid.push((
            "unknown schema",
            serde_json::to_vec(&tampered).expect("unknown"),
        ));
        let mut foreign = base.clone();
        foreign["application_instance_id"] = "sigil:another-owner".into();
        invalid.push(("foreign owner", schema1_pointer(foreign)));
        let mut authority = base.clone();
        authority["authority_generation_digest"] =
            serde_json::to_value(CanonicalHash::from_bytes([0xab; 32])).expect("digest");
        invalid.push(("foreign authority", schema1_pointer(authority)));
        let mut ahead = base.clone();
        ahead["application_generation"] = (old.application_generation + 1).into();
        invalid.push(("generation rollback", schema1_pointer(ahead)));
        let mut duplicate = base.clone();
        duplicate["mandatory_readiness"][1] = duplicate["mandatory_readiness"][0].clone();
        invalid.push(("duplicate readiness", schema1_pointer(duplicate)));
        let mut failed = base;
        failed["mandatory_readiness"][0]["passed"] = false.into();
        invalid.push(("failed readiness", schema1_pointer(failed)));

        // The invalid predecessor must be checked before a newly selected root is created.
        let new_state = cwd.join("must-not-create-state");
        let new_cache = cwd.join("must-not-create-cache");
        let source = std::fs::read_to_string(config)
            .expect("config")
            .replace(
                &toml_path(&cwd.join(".r71-test-state")),
                &toml_path(&new_state),
            )
            .replace(
                &toml_path(&cwd.join(".r71-test-cache")),
                &toml_path(&new_cache),
            );
        std::fs::write(config, source).expect("changed roots");
        for (case, pointer) in invalid {
            publish_fixture_pointer(bootstrap, &pointer);
            let before = metadata_snapshot(bootstrap);
            for _ in 0..2 {
                assert!(
                    matches!(
                        boot_current_schema(config, cwd),
                        Err(BootAuthorityErrorV1::Bootstrap(
                            BootstrapErrorV1::MetadataCorrupted(_)
                        ))
                    ),
                    "{case} must reject before composing"
                );
                assert_eq!(metadata_snapshot(bootstrap), before, "{case}");
                assert!(!new_state.exists(), "{case} created state root");
                assert!(!new_cache.exists(), "{case} created cache root");
            }
        }
    });
}

// Independently preserve the old seven-field schema-2 hash encoding. Only fixture creation
// removes the newly mandatory probe; invalid-evidence tests retain their exact supplied rows.
fn optional_terminal_schema2_pointer(fields: serde_json::Value) -> Vec<u8> {
    let probes: Vec<String> = fields["mandatory_readiness"]
        .as_array()
        .expect("readiness rows")
        .iter()
        .map(|row| {
            format!(
                "{{\"adapter\":{},\"passed\":{},\"evidence_digest\":{}}}",
                row["adapter"], row["passed"], row["evidence_digest"]
            )
        })
        .collect();
    let mut bytes = format!(
        "{{\"schema_version\":2,\"application_instance_id\":{},\"selected_epoch\":{},\"application_generation\":{},\"authority_generation_digest\":{},\"composition\":{{\"profile\":{},\"enhancements\":{}}},\"mandatory_readiness\":[{}]}}",
        fields["application_instance_id"],
        fields["selected_epoch"],
        fields["application_generation"],
        fields["authority_generation_digest"],
        fields["composition"]["profile"],
        fields["composition"]["enhancements"],
        probes.join(","),
    );
    let hash = sigil_kernel::external::sha256_hex(bytes.as_bytes());
    bytes.pop();
    bytes.push_str(&format!(",\"manifest_hash\":\"{hash}\"}}"));
    bytes.into_bytes()
}

fn with_optional_terminal_core_fixture(
    run: impl FnOnce(&Path, &Path, &Bootstrap, &CutoverManifestV1, &[u8], &Path),
) {
    let _environment_guard = crate::test_env::lock();
    let dir = tempfile::tempdir().expect("fixture");
    let config = dir.path().join("sigil.toml");
    write_r71_boot_config(&config);
    let source = std::fs::read_to_string(&config).expect("config");
    std::fs::write(
        &config,
        format!("{source}\n[composition]\nprofile = \"core\"\n"),
    )
    .expect("Core selection");
    let first = boot_current_schema(&config, dir.path()).expect("real Core boot");
    let current = first.cutover().manifest().clone();
    assert_eq!(current.composition, RuntimeCompositionConfig::core());
    assert_eq!(current.mandatory_readiness.len(), 14);
    assert!(current.mandatory_readiness.iter().all(|probe| probe.passed));
    let writer = &first.composition().storage_writer;
    let lease = writer
        .acquire_named(StorageWriterChannelV1::SessionLog, "retained-upgrade-log")
        .expect("real managed historical storage");
    writer
        .write_record(&lease, b"retained-before-terminal-upgrade")
        .expect("durable record");
    let record_path = lease.path().join("records.jsonl");
    writer.finalize(lease).expect("settle historical writer");
    drop(first);
    let bootstrap = Bootstrap::for_config_path(&config).expect("fixture bootstrap");
    let mut historical_fields = serde_json::to_value(&current).expect("manifest fields");
    historical_fields["mandatory_readiness"]
        .as_array_mut()
        .expect("probes")
        .retain(|row| row["adapter"] != "ExecutionTerminal");
    let historical = optional_terminal_schema2_pointer(historical_fields);
    validate_optional_terminal_cutover_predecessor_v2(&historical)
        .expect("complete 13-probe old Core predecessor");
    assert!(RuntimeGlobalCutoverV1::validate_manifest_bytes(&historical).is_err());
    publish_fixture_pointer(&bootstrap, &historical);
    run(
        dir.path(),
        &config,
        &bootstrap,
        &current,
        &historical,
        &record_path,
    );
}

#[test]
fn r71_bootstrap_optional_terminal_core_upgrades_cold_with_real_probes_and_retained_log() {
    with_optional_terminal_core_fixture(|cwd, config, bootstrap, old, historical, record_path| {
        let historical_identity = validate_optional_terminal_cutover_predecessor_v2(historical)
            .expect("historical identity");
        let retained = std::fs::read(record_path).expect("historical log");
        assert_eq!(retained, b"retained-before-terminal-upgrade\n");
        let upgraded = boot_current_schema(config, cwd).expect("cold old Core upgrade");
        let current = upgraded.cutover().manifest().clone();
        assert_eq!(
            current.application_generation,
            old.application_generation + 1
        );
        assert_eq!(current.application_instance_id, old.application_instance_id);
        assert_eq!(
            current.authority_generation_digest,
            old.authority_generation_digest
        );
        assert_eq!(current.composition, RuntimeCompositionConfig::core());
        assert_eq!(current.mandatory_readiness.len(), 14);
        assert!(current.mandatory_readiness.iter().all(|probe| probe.passed));
        assert!(current.mandatory_readiness.iter().any(|probe| {
            probe.adapter == MandatoryAdapterKindV1::ExecutionTerminal && probe.passed
        }));
        assert_ne!(current.manifest_hash, historical_identity.manifest_hash());
        let writer = &upgraded.composition().storage_writer;
        let lease = writer
            .acquire_named(StorageWriterChannelV1::SessionLog, "after-terminal-upgrade")
            .expect("new source-bound writer");
        writer
            .write_record(&lease, b"new-generation-record")
            .expect("new write");
        assert_eq!(
            writer.read_record_bytes(&lease, 1024).expect("new read"),
            b"new-generation-record\n"
        );
        writer.finalize(lease).expect("new settlement");
        assert_eq!(
            std::fs::read(record_path).expect("retained old log"),
            retained
        );
        assert_eq!(
            RuntimeGlobalCutoverV1::load_and_validate_manifest(
                &bootstrap.path(Object::CutoverPointer)
            )
            .expect("current pointer"),
            current
        );
        drop(upgraded);
        let before = metadata_snapshot(bootstrap);
        let replay = boot_current_schema(config, cwd).expect("cold replay");
        assert_eq!(*replay.cutover().manifest(), current);
        let after = metadata_snapshot(bootstrap);
        for (index, class) in [
            (0, Object::BootstrapManifest),
            (1, Object::AuthorityConfigGeneration),
            (2, Object::CutoverPointer),
            (4, Object::ProcessInventoryRequirement),
            (5, Object::ProcessInventoryAuthenticator),
        ] {
            assert!(after[index] == before[index], "replay changed {class:?}");
        }
        // Reopening an authority re-observes its controller and appends an authenticated
        // owner-registration fact. That required inventory refresh is independent of the
        // unchanged cutover decision; it must preserve the prior inventory history.
        let previous_inventory: serde_json::Value =
            serde_json::from_slice(before[3].as_deref().expect("previous inventory"))
                .expect("previous inventory fields");
        let replay_inventory: serde_json::Value =
            serde_json::from_slice(after[3].as_deref().expect("replay inventory"))
                .expect("replay inventory fields");
        assert_eq!(
            replay_inventory["sequence"]
                .as_u64()
                .expect("replay sequence"),
            previous_inventory["sequence"]
                .as_u64()
                .expect("previous sequence")
                + 1
        );
        assert_eq!(
            replay_inventory["previous_record_hash"],
            previous_inventory["record_authenticator"]
        );
        for field in [
            "authority_epoch",
            "authentication_realm_id",
            "authentication_key_id",
            "entries",
            "bounded_native_exposure_count",
            "bounded_native_exposure_frontier",
        ] {
            assert_eq!(
                replay_inventory[field], previous_inventory[field],
                "{field}"
            );
        }
        assert_ne!(
            replay_inventory["owner_subject"]["registration_nonce"],
            previous_inventory["owner_subject"]["registration_nonce"]
        );
        assert_eq!(
            std::fs::read(record_path).expect("old log after replay"),
            retained
        );
    });
}

#[test]
fn r71_bootstrap_optional_terminal_core_reuses_interrupted_forward_generation() {
    with_optional_terminal_core_fixture(|cwd, config, bootstrap, old, _, _| {
        let publication = bootstrap.acquire_publication().expect("publication");
        let mut record = load_authority_config_generation(bootstrap, &publication)
            .expect("read generation")
            .expect("generation");
        record.generation = old.application_generation + 8;
        bootstrap
            .publish_bytes(
                &publication,
                Object::AuthorityConfigGeneration,
                &serde_json::to_vec(&record).expect("generation bytes"),
            )
            .expect("interrupted forward boot");
        drop(publication);
        let upgraded = boot_current_schema(config, cwd).expect("resume old Core upgrade");
        assert_eq!(
            upgraded.cutover().manifest().application_generation,
            record.generation
        );
        assert_eq!(upgraded.cutover().manifest().mandatory_readiness.len(), 14);
        drop(upgraded);
        let replay = boot_current_schema(config, cwd).expect("replay resumed old Core upgrade");
        assert_eq!(
            replay.cutover().manifest().application_generation,
            record.generation
        );
    });
}

#[test]
fn r71_bootstrap_optional_terminal_core_rejects_other_gaps_failures_and_tampering() {
    with_optional_terminal_core_fixture(|cwd, config, bootstrap, old, historical, record_path| {
        let base: serde_json::Value = serde_json::from_slice(historical).expect("old fields");
        let mut invalid = Vec::new();
        let mut missing = base.clone();
        missing["mandatory_readiness"]
            .as_array_mut()
            .expect("probes")
            .pop();
        invalid.push((
            "missing another required probe",
            optional_terminal_schema2_pointer(missing),
        ));
        let mut failed = base.clone();
        failed["mandatory_readiness"][0]["passed"] = false.into();
        invalid.push((
            "failed historical probe",
            optional_terminal_schema2_pointer(failed),
        ));
        let mut duplicate = base.clone();
        duplicate["mandatory_readiness"][1] = duplicate["mandatory_readiness"][0].clone();
        invalid.push((
            "duplicate probe",
            optional_terminal_schema2_pointer(duplicate),
        ));
        let mut altered = base.clone();
        altered["application_generation"] = (old.application_generation + 1).into();
        invalid.push((
            "hash tampering",
            serde_json::to_vec(&altered).expect("tamper bytes"),
        ));
        invalid.push((
            "generation ahead",
            optional_terminal_schema2_pointer(altered),
        ));
        let mut altered = base.clone();
        altered["application_instance_id"] = "sigil:foreign-owner".into();
        invalid.push(("foreign owner", optional_terminal_schema2_pointer(altered)));
        let mut altered = base.clone();
        altered["authority_generation_digest"] =
            serde_json::to_value(CanonicalHash::from_bytes([0xab; 32])).expect("foreign digest");
        invalid.push((
            "foreign authority",
            optional_terminal_schema2_pointer(altered),
        ));
        let mut altered = base.clone();
        altered["composition"]["enhancements"] = serde_json::json!(["terminal"]);
        invalid.push((
            "selected Terminal requires its probe",
            optional_terminal_schema2_pointer(altered),
        ));
        let mut altered = base;
        altered["composition"]["enhancements"] = serde_json::json!(["memory"]);
        invalid.push((
            "selected Memory requires its probe",
            optional_terminal_schema2_pointer(altered),
        ));
        let new_state = cwd.join("must-not-create-state");
        let new_cache = cwd.join("must-not-create-cache");
        let source = std::fs::read_to_string(config)
            .expect("config")
            .replace(
                &toml_path(&cwd.join(".r71-test-state")),
                &toml_path(&new_state),
            )
            .replace(
                &toml_path(&cwd.join(".r71-test-cache")),
                &toml_path(&new_cache),
            );
        std::fs::write(config, source).expect("new roots");
        let retained = std::fs::read(record_path).expect("old log");
        for (case, pointer) in invalid {
            publish_fixture_pointer(bootstrap, &pointer);
            let before = metadata_snapshot(bootstrap);
            for _ in 0..2 {
                assert!(
                    matches!(
                        boot_current_schema(config, cwd),
                        Err(BootAuthorityErrorV1::Bootstrap(
                            BootstrapErrorV1::MetadataCorrupted(_)
                        ))
                    ),
                    "{case} must reject before composing"
                );
                assert_eq!(metadata_snapshot(bootstrap), before, "{case}");
                assert!(!new_state.exists(), "{case} created state root");
                assert!(!new_cache.exists(), "{case} created cache root");
                assert_eq!(
                    std::fs::read(record_path).expect("retained log"),
                    retained,
                    "{case}"
                );
            }
        }
    });
}
