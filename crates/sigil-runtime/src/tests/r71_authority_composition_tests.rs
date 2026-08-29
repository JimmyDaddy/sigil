use super::*;
use crate::r71_global_cutover::{
    RuntimeExecutionSeamV1, RuntimeFileAccessSeamV1, RuntimeGlobalCutoverV1,
    probe_mandatory_adapters,
};
use sigil_application::ApplicationResourceRecoveryFacadeV1;
use sigil_kernel::cutover_manifest::MandatoryAdapterKindV1;

fn write_r71_boot_config(config: &Path) {
    let fixture_root = config.parent().expect("config parent");
    let state = fixture_root.join(".r71-test-state");
    let cache = fixture_root.join(".r71-test-cache");
    std::fs::create_dir_all(&cache).expect("test cache");
    let state_text = toml_path(&state);
    let cache_text = toml_path(&cache);
    std::fs::write(
            config,
            format!(
                "config_version = 2\n[workspace]\nroot = \".\"\n[storage]\nstate_root = \"{}\"\ncache_root = \"{}\"\n[agent]\nconnection = \"local-test\"\nmodel = \"test\"\n[connections.local-test]\nlabel = \"local\"\nprovider = \"custom\"\nprotocol = \"chat_completions\"\nbase_url = \"http://127.0.0.1:1\"\ncredential = {{ source = \"none\" }}\n",
                state_text,
                cache_text,
            ),
        )
        .expect("config");
}

fn toml_path(path: &Path) -> String {
    // TOML basic strings treat Windows backslashes as escapes. Forward slashes are accepted
    // by Windows path APIs and keep absolute test roots valid without producing invalid TOML.
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(unix)]
fn boot_terminal_command() -> (String, Vec<String>) {
    (
        "/bin/sh".to_owned(),
        vec![
            "-lc".to_owned(),
            "printf 'boot-terminal-ready\\n'; while :; do sleep 1; done".to_owned(),
        ],
    )
}

#[cfg(windows)]
fn boot_terminal_command() -> (String, Vec<String>) {
    (
        "cmd.exe".to_owned(),
        vec![
            "/D".to_owned(),
            "/C".to_owned(),
            "echo boot-terminal-ready& ping -n 30 127.0.0.1 >NUL".to_owned(),
        ],
    )
}

#[test]
fn r71_composition_tool_authority_facade_is_wired() {
    use crate::managed_storage_writer::StorageWriterChannelV1 as Ch;
    let dir = tempfile::tempdir().expect("tempdir");
    let state = dir.path().join("state");
    let exec = dir.path().join("exec");
    std::fs::create_dir_all(&state).expect("state dir");
    std::fs::create_dir_all(state.join("cache")).expect("cache dir");
    std::fs::create_dir_all(&exec).expect("exec dir");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).expect("mode");
        std::fs::set_permissions(&exec, std::fs::Permissions::from_mode(0o700)).expect("mode");
    }
    let planner: Arc<dyn sigil_kernel::managed_execution::ManagedExecutionPlannerV1> =
        Arc::new(crate::r71_shadow_planner::ShadowPlannerV1::new(
            crate::r71_shadow_planner::ShadowPlannerConfigV1::default(),
        ));
    let composition = compose_runtime_authority(
        &state,
        &exec,
        CanonicalHash::from_bytes([0x55; 32]),
        planner,
        &[Ch::SessionLog],
    )
    .expect("compose");
    let binding =
        sigil_kernel::managed_file_access::ManagedFileAdmissionBindingV1::ToolPermissionPlan {
            permission_plan_hash: CanonicalHash::from_bytes([0xa1; 32]),
            decision_hash: CanonicalHash::from_bytes([0xa2; 32]),
            approval_continuity_hash: CanonicalHash::from_bytes([0xa3; 32]),
            tool_start_event_digest: CanonicalHash::from_bytes([0xa4; 32]),
            file_access_plan_hash: CanonicalHash::from_bytes([0xa5; 32]),
            file_subject_binding_hash: CanonicalHash::from_bytes([0xa6; 32]),
            file_resolver_proof_digest: CanonicalHash::from_bytes([0xa7; 32]),
            file_authority_generation: sigil_kernel::resource::AuthorityGeneration {
                epoch: 1,
                instance_hash: CanonicalHash::from_bytes([0xa8; 32]),
            },
            workspace_mutation_activation: None,
        };
    let subject = sigil_kernel::resource::OpaquePermissionSubjectRef::new("ws-1".to_owned());
    let outcome = composition.tool_authority.adjudicate_tool_file_access(
        binding,
        &subject,
        sigil_kernel::managed_file_access::ManagedFileOperationV1::Read,
    );
    // Observable subjects are registered by the surface bootstrap; an unobserved subject
    // fails closed through the real wire.
    let error = outcome.expect_err("unregistered subject");
    assert!(matches!(
        error,
        sigil_kernel::tool_authority::KernelToolAuthorityErrorV1::Access(
            sigil_kernel::managed_file_access::ManagedFileAccessErrorV1::OperationNotPermitted
        )
    ));
}

#[test]
fn r71_composition_declared_channel_writes_and_probes_exactly() {
    use crate::managed_storage_writer::StorageWriterChannelV1 as Ch;
    let dir = tempfile::tempdir().expect("tempdir");
    let state = dir.path().join("state");
    let exec = dir.path().join("exec");
    std::fs::create_dir_all(&state).expect("state dir");
    std::fs::create_dir_all(state.join("cache")).expect("cache dir");
    std::fs::create_dir_all(&exec).expect("exec dir");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).expect("mode");
        std::fs::set_permissions(&exec, std::fs::Permissions::from_mode(0o700)).expect("mode");
    }
    let planner: Arc<dyn sigil_kernel::managed_execution::ManagedExecutionPlannerV1> =
        Arc::new(crate::r71_shadow_planner::ShadowPlannerV1::new(
            crate::r71_shadow_planner::ShadowPlannerConfigV1::default(),
        ));
    let composition = crate::r71_authority_composition::compose_runtime_authority(
        &state,
        &exec,
        CanonicalHash::from_bytes([0x55; 32]),
        planner,
        &[Ch::SessionLog],
    )
    .expect("compose");
    let lease = composition
        .storage_writer
        .acquire(Ch::SessionLog)
        .expect("acquire");
    composition
        .storage_writer
        .write_record(&lease, b"seq=1")
        .expect("write");
    composition
        .storage_writer
        .finalize(lease)
        .expect("finalize");
    let recovery = ApplicationResourceRecoveryFacadeV1::new();
    let probes = probe_mandatory_adapters(
        &composition.services,
        &recovery,
        CanonicalHash::from_bytes([0x56; 32]),
        1,
    );
    let session_log = probes
        .iter()
        .find(|p| p.adapter == MandatoryAdapterKindV1::StorageSessionLog)
        .expect("session log probe");
    assert!(session_log.passed);
    let input_history = probes
        .iter()
        .find(|p| p.adapter == MandatoryAdapterKindV1::StorageInputHistory)
        .expect("input history probe");
    assert!(!input_history.passed);
    assert!(matches!(
        composition.services.execution_seam,
        RuntimeExecutionSeamV1::SandboxBacked
    ));
    assert!(matches!(
        composition.services.file_access_seam,
        RuntimeFileAccessSeamV1::AuthorityBacked
    ));
}

#[test]
fn r71_current_boot_publishes_one_green_current_manifest_and_replays_it() {
    // Composition boot resolves shared state/cache defaults in a few nested adapters. Keep
    // this replay assertion behind the runtime test environment lock so parallel fixtures
    // that temporarily override those process-wide roots cannot invalidate the second boot.
    let _environment_guard = crate::test_env::lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("sigil.toml");
    std::fs::write(
            &config,
            "config_version = 2\n[workspace]\nroot = \".\"\n[agent]\nconnection = \"local-test\"\nmodel = \"test\"\n[connections.local-test]\nlabel = \"local\"\nprovider = \"custom\"\nprotocol = \"chat_completions\"\nbase_url = \"http://127.0.0.1:1\"\ncredential = { source = \"none\" }\n",
        )
        .expect("config");
    let state = dir.path().join("state");
    let cache = state.join("cache");
    std::fs::create_dir_all(&cache).expect("cache");
    let _state_env = crate::test_env::EnvScope::set(crate::SIGIL_STATE_HOME_ENV, &state);
    let _cache_env = crate::test_env::EnvScope::set(crate::SIGIL_CACHE_HOME_ENV, &cache);
    let config_snapshot = ValidatedAuthorityConfigSnapshotV1::load(&config, dir.path())
        .expect("load snapshot")
        .expect("snapshot");
    let exec = config_snapshot.resolved_paths().scratch_root.clone();
    std::fs::create_dir_all(&exec).expect("exec");
    let (first, _composition) =
        compose_current_boot_authority(&config_snapshot, &state, &cache, &exec)
            .expect("current boot");
    assert!(first.is_current_schema_ready());
    assert_eq!(first.manifest().mandatory_readiness.len(), 18);
    assert_eq!(
        first.manifest().selected_epoch,
        sigil_kernel::cutover_manifest::StartupEpochV1::NewCurrentSchema
    );
    let (second, _composition) =
        compose_current_boot_authority(&config_snapshot, &state, &cache, &exec).expect("replay");
    assert_eq!(first.manifest(), second.manifest());
}

#[test]
fn r71_validated_snapshot_binding_is_stable_across_reload() {
    let _environment_guard = crate::test_env::lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("sigil.toml");
    write_r71_boot_config(&config);

    let first = ValidatedAuthorityConfigSnapshotV1::load(&config, dir.path())
        .expect("load first snapshot")
        .expect("first snapshot");
    let second = ValidatedAuthorityConfigSnapshotV1::load(&config, dir.path())
        .expect("load second snapshot")
        .expect("second snapshot");

    assert_eq!(first.config_hash(), second.config_hash());
    assert_eq!(first.workspace_root(), second.workspace_root());
    assert_eq!(first.resolved_paths(), second.resolved_paths());
}

#[test]
fn r71_session_model_request_override_does_not_advance_authority_binding() {
    let _environment_guard = crate::test_env::lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("sigil.toml");
    write_r71_boot_config(&config);
    let persisted = ValidatedAuthorityConfigSnapshotV1::load(&config, dir.path())
        .expect("persisted snapshot")
        .expect("config");
    let _override =
        crate::test_env::EnvScope::set(sigil_kernel::SIGIL_MODEL_REQUEST_TIMEOUT_SECS_ENV, "17");
    let session = ValidatedAuthorityConfigSnapshotV1::load(&config, dir.path())
        .expect("session snapshot")
        .expect("config");
    assert_eq!(session.config_hash(), persisted.config_hash());
}

#[test]
fn r71_current_boot_advances_manifest_for_a_new_persisted_config_generation() {
    let _environment_guard = crate::test_env::lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("sigil.toml");
    let state_a = dir.path().join("state-a");
    let cache_a = state_a.join("cache");
    let state_b = dir.path().join("state-b");
    let cache_b = state_b.join("cache");
    std::fs::create_dir_all(&cache_a).expect("cache a");
    std::fs::create_dir_all(&cache_b).expect("cache b");
    let state_a_text = toml_path(&state_a);
    let cache_a_text = toml_path(&cache_a);
    let state_b_text = toml_path(&state_b);
    let cache_b_text = toml_path(&cache_b);
    let config_a = format!(
        "config_version = 2\n[workspace]\nroot = \".\"\n[storage]\nstate_root = \"{}\"\ncache_root = \"{}\"\n[agent]\nconnection = \"local-test\"\nmodel = \"old-model\"\n[connections.local-test]\nlabel = \"local\"\nprovider = \"custom\"\nprotocol = \"chat_completions\"\nbase_url = \"http://127.0.0.1:1\"\ncredential = {{ source = \"none\" }}\n",
        state_a_text, cache_a_text,
    );
    std::fs::write(&config, config_a).expect("config");

    let first = boot_current_schema(&config, dir.path()).expect("first boot");
    let first_instance = first.cutover().manifest().application_instance_id.clone();
    let first_generation = first.cutover().manifest().application_generation;
    let updated = std::fs::read_to_string(&config)
        .expect("read config")
        .replace("old-model", "new-model")
        .replace(&state_a_text, &state_b_text)
        .replace(&cache_a_text, &cache_b_text);
    std::fs::write(&config, updated).expect("update config");
    let second = boot_current_schema(&config, dir.path()).expect("updated config boot");

    assert_eq!(
        first_instance,
        second.cutover().manifest().application_instance_id
    );
    assert_eq!(second.config().agent.model, "new-model");
    assert_eq!(second.resolved_paths().state_root, state_b);
    assert_eq!(
        second.cutover().manifest().application_generation,
        first_generation + 1
    );
    assert_eq!(
        sigil_kernel::RootConfig::load(&config)
            .expect("reload persisted config")
            .agent
            .model,
        "new-model"
    );
    let published = RuntimeGlobalCutoverV1::load_and_validate_manifest(
        &authority_bootstrap_manifest_path(&config).expect("authority bootstrap manifest path"),
    )
    .expect("published manifest");
    assert_eq!(published, *second.cutover().manifest());
}

#[tokio::test]
async fn r71_current_boot_command_route_starts_terminal_with_durable_inventory()
-> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let config = dir.path().join("sigil.toml");
    let boot = {
        let _environment_guard = crate::test_env::lock();
        write_r71_boot_config(&config);
        boot_current_schema(&config, dir.path())?
    };
    let scratch = boot.resolved_paths().scratch_root.clone();
    std::fs::create_dir_all(&scratch)?;

    use sigil_tools_builtin::{ManagedTerminalExecutionPortV1, ManagedTerminalStartRequestV1};
    let (program, args) = boot_terminal_command();
    let mut handle = boot
        .composition()
        .command_execution
        .start_persistent(ManagedTerminalStartRequestV1 {
            program,
            args,
            cwd: boot.workspace_root().to_path_buf(),
            environment: [(
                "SIGIL_SCRATCH_DIR".to_owned(),
                scratch.to_string_lossy().into_owned(),
            )]
            .into_iter()
            .collect(),
            pty_size: None,
        })
        .await
        .map_err(|error| anyhow::anyhow!("durable boot terminal route: {error}"))?;
    let mut stream = handle
        .take_output_stream()
        .map_err(|error| anyhow::anyhow!("durable boot terminal output stream: {error}"))?;
    let frame = stream
        .next_frame()
        .await
        .map_err(|error| anyhow::anyhow!("durable boot terminal output: {error}"))?
        .ok_or_else(|| anyhow::anyhow!("durable boot terminal ended before readiness"))?;
    assert!(String::from_utf8_lossy(&frame.payload).contains("boot-terminal-ready"));
    handle
        .cancel(sigil_kernel::managed_execution::ProcessCancelReasonV1::UserCancelled)
        .await
        .map_err(|error| anyhow::anyhow!("durable boot terminal cancel: {error}"))?;
    handle
        .wait_and_finalize()
        .await
        .map_err(|error| anyhow::anyhow!("durable boot terminal finalize: {error}"))?;
    Ok(())
}

#[test]
fn r71_bootstrap_metadata_missing_requires_typed_reconciliation() {
    let _environment_guard = crate::test_env::lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("sigil.toml");
    write_r71_boot_config(&config);
    let first = boot_current_schema(&config, dir.path()).expect("first boot");
    drop(first);
    let bootstrap =
        sigil_resource_authority::bootstrap::AuthorityBootstrapStoreV1::for_config_path(&config)
            .expect("bootstrap store");
    std::fs::remove_file(bootstrap.path(
            sigil_resource_authority::bootstrap::AuthorityBootstrapObjectClassV1::AuthorityConfigGeneration,
        ))
        .expect("remove generation metadata");

    let result = boot_current_schema(&config, dir.path());
    assert!(matches!(
        result,
        Err(BootAuthorityErrorV1::Bootstrap(
            sigil_resource_authority::bootstrap::BootstrapErrorV1::ReconciliationRequired(_)
        ))
    ));
    std::fs::remove_dir_all(bootstrap.root()).expect("cleanup bootstrap fixture");
}

#[test]
fn r71_process_inventory_cutover_is_one_time_then_missing_state_fails_closed() {
    let _environment_guard = crate::test_env::lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().join("home");
    let xdg_config = home.join(".config");
    std::fs::create_dir_all(&xdg_config).expect("isolated config home");
    let _home_env = crate::test_env::EnvScope::set("HOME", &home);
    let _xdg_env = crate::test_env::EnvScope::set("XDG_CONFIG_HOME", &xdg_config);
    let config = dir.path().join("sigil.toml");
    write_r71_boot_config(&config);
    drop(boot_current_schema(&config, dir.path()).expect("first boot"));

    let bootstrap =
        sigil_resource_authority::bootstrap::AuthorityBootstrapStoreV1::for_config_path(&config)
            .expect("bootstrap store");
    let publication = bootstrap.acquire_publication().expect("publication");
    let current = load_authority_config_generation(&bootstrap, &publication)
        .expect("generation")
        .expect("generation record");
    let legacy = serde_json::json!({
        "schema_version": 1,
        "config_hash": current.config_hash,
        "generation": current.generation,
    });
    bootstrap
            .publish_bytes(
                &publication,
                sigil_resource_authority::bootstrap::AuthorityBootstrapObjectClassV1::AuthorityConfigGeneration,
                &serde_json::to_vec(&legacy).expect("legacy generation"),
            )
            .expect("publish legacy generation");
    drop(publication);
    for object in [
            sigil_resource_authority::bootstrap::AuthorityBootstrapObjectClassV1::ProcessInventory,
            sigil_resource_authority::bootstrap::AuthorityBootstrapObjectClassV1::ProcessInventoryRequirement,
        ] {
            std::fs::remove_file(bootstrap.path(object)).expect("remove process inventory state");
        }

    drop(boot_current_schema(&config, dir.path()).expect("one-time inventory cutover"));
    let publication = bootstrap.acquire_publication().expect("publication");
    let migrated = load_authority_config_generation(&bootstrap, &publication)
        .expect("generation")
        .expect("generation record");
    assert_eq!(
        migrated.schema_version,
        AUTHORITY_CONFIG_GENERATION_SCHEMA_VERSION
    );
    assert!(migrated.process_inventory_required);
    drop(publication);

    for object in [
            sigil_resource_authority::bootstrap::AuthorityBootstrapObjectClassV1::ProcessInventory,
            sigil_resource_authority::bootstrap::AuthorityBootstrapObjectClassV1::ProcessInventoryRequirement,
        ] {
            std::fs::remove_file(bootstrap.path(object)).expect("remove required inventory state");
        }
    assert!(matches!(
        boot_current_schema(&config, dir.path()),
        Err(BootAuthorityErrorV1::Bootstrap(
            sigil_resource_authority::bootstrap::BootstrapErrorV1::MetadataCorrupted(_)
        ))
    ));
}

#[test]
fn r71_bootstrap_metadata_corruption_is_typed_and_fail_closed() {
    let _environment_guard = crate::test_env::lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("sigil.toml");
    write_r71_boot_config(&config);
    let first = boot_current_schema(&config, dir.path()).expect("first boot");
    drop(first);
    let bootstrap =
        sigil_resource_authority::bootstrap::AuthorityBootstrapStoreV1::for_config_path(&config)
            .expect("bootstrap store");
    std::fs::write(
            bootstrap.path(
                sigil_resource_authority::bootstrap::AuthorityBootstrapObjectClassV1::AuthorityConfigGeneration,
            ),
            b"{corrupt",
        )
        .expect("corrupt generation metadata");

    let result = boot_current_schema(&config, dir.path());
    assert!(matches!(
        result,
        Err(BootAuthorityErrorV1::Bootstrap(
            sigil_resource_authority::bootstrap::BootstrapErrorV1::MetadataCorrupted(_)
        ))
    ));
    std::fs::remove_dir_all(bootstrap.root()).expect("cleanup bootstrap fixture");
}

#[test]
fn r71_bootstrap_pointer_corruption_is_typed_and_fail_closed() {
    let _environment_guard = crate::test_env::lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("sigil.toml");
    write_r71_boot_config(&config);
    let first = boot_current_schema(&config, dir.path()).expect("first boot");
    drop(first);
    let bootstrap =
        sigil_resource_authority::bootstrap::AuthorityBootstrapStoreV1::for_config_path(&config)
            .expect("bootstrap store");
    std::fs::write(
        bootstrap.path(
            sigil_resource_authority::bootstrap::AuthorityBootstrapObjectClassV1::CutoverPointer,
        ),
        b"{corrupt",
    )
    .expect("corrupt pointer metadata");

    let result = boot_current_schema(&config, dir.path());
    assert!(matches!(
        result,
        Err(BootAuthorityErrorV1::Bootstrap(
            sigil_resource_authority::bootstrap::BootstrapErrorV1::MetadataCorrupted(_)
        ))
    ));
    std::fs::remove_dir_all(bootstrap.root()).expect("cleanup bootstrap fixture");
}

#[cfg(unix)]
#[test]
fn r71_bootstrap_pointer_symlink_is_rejected_before_publish() {
    use std::os::unix::fs::symlink;

    let _environment_guard = crate::test_env::lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("sigil.toml");
    write_r71_boot_config(&config);
    let first = boot_current_schema(&config, dir.path()).expect("first boot");
    drop(first);
    let bootstrap =
        sigil_resource_authority::bootstrap::AuthorityBootstrapStoreV1::for_config_path(&config)
            .expect("bootstrap store");
    let pointer = bootstrap
        .path(sigil_resource_authority::bootstrap::AuthorityBootstrapObjectClassV1::CutoverPointer);
    std::fs::remove_file(&pointer).expect("remove pointer");
    let target = dir.path().join("pointer-target");
    std::fs::write(&target, b"not authority metadata").expect("target");
    symlink(&target, &pointer).expect("pointer symlink");

    let result = boot_current_schema(&config, dir.path());
    assert!(matches!(
        result,
        Err(BootAuthorityErrorV1::Bootstrap(
            sigil_resource_authority::bootstrap::BootstrapErrorV1::MetadataCorrupted(_)
        ))
    ));
    std::fs::remove_dir_all(bootstrap.root()).expect("cleanup bootstrap fixture");
}

#[test]
fn r71_bootstrap_pointer_loss_after_generation_publication_reconciles_on_reboot() {
    let _environment_guard = crate::test_env::lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("sigil.toml");
    write_r71_boot_config(&config);
    let first = boot_current_schema(&config, dir.path()).expect("first boot");
    let expected = first.cutover().manifest().clone();
    drop(first);
    let bootstrap =
        sigil_resource_authority::bootstrap::AuthorityBootstrapStoreV1::for_config_path(&config)
            .expect("bootstrap store");
    let persisted_snapshot = ValidatedAuthorityConfigSnapshotV1::load(&config, dir.path())
        .expect("reload persisted snapshot")
        .expect("persisted snapshot");
    let publication = bootstrap.acquire_publication().expect("publication");
    let generation = load_authority_config_generation(&bootstrap, &publication)
        .expect("generation record")
        .expect("generation record exists");
    assert_eq!(generation.config_hash, persisted_snapshot.config_hash());
    assert_eq!(generation.generation, expected.application_generation);
    drop(publication);
    std::fs::remove_file(bootstrap.path(
        sigil_resource_authority::bootstrap::AuthorityBootstrapObjectClassV1::CutoverPointer,
    ))
    .expect("simulate crash before pointer publication");

    let replay_snapshot = ValidatedAuthorityConfigSnapshotV1::load(&config, dir.path())
        .expect("reload replay snapshot")
        .expect("replay snapshot");
    assert_eq!(
        replay_snapshot.config_hash(),
        persisted_snapshot.config_hash()
    );
    let replay = boot_current_schema(&config, dir.path()).expect("reconcile reboot");
    assert_eq!(*replay.cutover().manifest(), expected);
    std::fs::remove_dir_all(bootstrap.root()).expect("cleanup bootstrap fixture");
}

#[test]
fn r71_bootstrap_pointer_is_outside_an_explicit_config_parent() {
    let _environment_guard = crate::test_env::lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("workspace").join("sigil.toml");
    std::fs::create_dir_all(config.parent().expect("config parent")).expect("parent");
    std::fs::write(
            &config,
            "config_version = 2\n[workspace]\nroot = \".\"\n[agent]\nconnection = \"local-test\"\nmodel = \"test\"\n[connections.local-test]\nlabel = \"local\"\nprovider = \"custom\"\nprotocol = \"chat_completions\"\nbase_url = \"http://127.0.0.1:1\"\ncredential = { source = \"none\" }\n",
        )
        .expect("config");
    let pointer = authority_bootstrap_manifest_path(&config).expect("pointer path");
    assert!(!pointer.starts_with(config.parent().expect("config parent")));
    assert!(pointer.ends_with(".sigil-cutover-manifest.json"));
    let bootstrap =
        sigil_resource_authority::bootstrap::AuthorityBootstrapStoreV1::for_config_path(&config)
            .expect("bootstrap store");
    std::fs::remove_dir_all(bootstrap.root()).expect("cleanup bootstrap fixture");
}

#[test]
fn r71_bootstrap_ignores_config_parent_metadata_replacement() {
    let _environment_guard = crate::test_env::lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let config_parent = dir.path().join("workspace");
    std::fs::create_dir_all(&config_parent).expect("config parent");
    let config = config_parent.join("sigil.toml");
    std::fs::write(
            &config,
            "config_version = 2\n[workspace]\nroot = \".\"\n[agent]\nconnection = \"local-test\"\nmodel = \"test\"\n[connections.local-test]\nlabel = \"local\"\nprovider = \"custom\"\nprotocol = \"chat_completions\"\nbase_url = \"http://127.0.0.1:1\"\ncredential = { source = \"none\" }\n",
        )
        .expect("config");
    let first = boot_current_schema(&config, dir.path()).expect("first boot");
    let expected = first.cutover().manifest().clone();
    drop(first);

    std::fs::write(
        config_parent.join("authority-config-generation.json"),
        b"{corrupt",
    )
    .expect("replace config-parent generation");
    std::fs::write(
        config_parent.join(".sigil-cutover-manifest.json"),
        b"{corrupt",
    )
    .expect("replace config-parent pointer");

    let replay = boot_current_schema(&config, dir.path()).expect("replay boot");
    assert_eq!(*replay.cutover().manifest(), expected);
    let bootstrap =
        sigil_resource_authority::bootstrap::AuthorityBootstrapStoreV1::for_config_path(&config)
            .expect("bootstrap store");
    std::fs::remove_dir_all(bootstrap.root()).expect("cleanup bootstrap fixture");
}

#[test]
fn r71_concurrent_boots_publish_monotonic_generation_under_one_lock() {
    use std::sync::{Arc, Barrier};

    let _environment_guard = crate::test_env::lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("sigil.toml");
    let state_a = dir.path().join("state-a");
    let cache_a = state_a.join("cache");
    let state_b = dir.path().join("state-b");
    let cache_b = state_b.join("cache");
    std::fs::create_dir_all(&cache_a).expect("cache a");
    std::fs::create_dir_all(&cache_b).expect("cache b");
    let config_text = |state: &Path, cache: &Path, model: &str| {
        format!(
            "config_version = 2\n[workspace]\nroot = \".\"\n[storage]\nstate_root = \"{}\"\ncache_root = \"{}\"\n[agent]\nconnection = \"local-test\"\nmodel = \"{}\"\n[connections.local-test]\nlabel = \"local\"\nprovider = \"custom\"\nprotocol = \"chat_completions\"\nbase_url = \"http://127.0.0.1:1\"\ncredential = {{ source = \"none\" }}\n",
            toml_path(state),
            toml_path(cache),
            model,
        )
    };
    std::fs::write(&config, config_text(&state_a, &cache_a, "model-a")).expect("config a");
    let snapshot_a = ValidatedAuthorityConfigSnapshotV1::load(&config, dir.path())
        .expect("load snapshot a")
        .expect("snapshot a");
    std::fs::write(&config, config_text(&state_b, &cache_b, "model-b")).expect("config b");
    let snapshot_b = ValidatedAuthorityConfigSnapshotV1::load(&config, dir.path())
        .expect("load snapshot b")
        .expect("snapshot b");

    let barrier = Arc::new(Barrier::new(3));
    let barrier_a = Arc::clone(&barrier);
    let handle_a = std::thread::spawn(move || {
        barrier_a.wait();
        build_current_boot_transaction(snapshot_a)
    });
    let barrier_b = Arc::clone(&barrier);
    let handle_b = std::thread::spawn(move || {
        barrier_b.wait();
        build_current_boot_transaction(snapshot_b)
    });
    barrier.wait();
    assert!(handle_a.join().expect("boot a thread").is_ok());
    assert!(handle_b.join().expect("boot b thread").is_ok());

    let bootstrap =
        sigil_resource_authority::bootstrap::AuthorityBootstrapStoreV1::for_config_path(&config)
            .expect("bootstrap store");
    let pointer = RuntimeGlobalCutoverV1::load_and_validate_manifest(&bootstrap.path(
        sigil_resource_authority::bootstrap::AuthorityBootstrapObjectClassV1::CutoverPointer,
    ))
    .expect("published pointer");
    assert_eq!(pointer.application_generation, 2);
    std::fs::remove_dir_all(bootstrap.root()).expect("cleanup bootstrap fixture");
}

#[test]
fn r71_authority_snapshot_survives_config_replacement_without_split_brain() {
    let _environment_guard = crate::test_env::lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace_a = dir.path().join("workspace-a");
    let workspace_b = dir.path().join("workspace-b");
    std::fs::create_dir_all(&workspace_a).expect("workspace a");
    std::fs::create_dir_all(&workspace_b).expect("workspace b");
    let config = dir.path().join("sigil.toml");
    let config_a = "config_version = 2\n[workspace]\nroot = \"workspace-a\"\n[agent]\nconnection = \"local-test\"\nmodel = \"test\"\n[connections.local-test]\nlabel = \"local\"\nprovider = \"custom\"\nprotocol = \"chat_completions\"\nbase_url = \"http://127.0.0.1:1\"\ncredential = { source = \"none\" }\n";
    let config_b = config_a.replace("workspace-a", "workspace-b");
    let state = dir.path().join("state");
    let cache = state.join("cache");
    std::fs::create_dir_all(&cache).expect("cache");
    let _state_env = crate::test_env::EnvScope::set(crate::SIGIL_STATE_HOME_ENV, &state);
    let _cache_env = crate::test_env::EnvScope::set(crate::SIGIL_CACHE_HOME_ENV, &cache);
    std::fs::write(&config, config_a).expect("config a");
    let snapshot = ValidatedAuthorityConfigSnapshotV1::load(&config, dir.path())
        .expect("load snapshot")
        .expect("valid snapshot");
    let original_hash = snapshot.config_hash();
    std::fs::write(&config, config_b).expect("config b");

    assert_eq!(
        snapshot.workspace_root(),
        workspace_a
            .canonicalize()
            .expect("canonical workspace a")
            .as_path()
    );
    assert_eq!(snapshot.config_hash(), original_hash);
    let live_snapshot = ValidatedAuthorityConfigSnapshotV1::load(&config, dir.path())
        .expect("load live snapshot")
        .expect("live snapshot");
    assert_ne!(snapshot.config_hash(), live_snapshot.config_hash());
    let exec = snapshot.resolved_paths().scratch_root.clone();
    std::fs::create_dir_all(&exec).expect("exec");

    let (_cutover, composition) = compose_current_boot_authority(&snapshot, &state, &cache, &exec)
        .expect("compose from frozen snapshot");
    let capsule = composition
        .activate_workspace(snapshot.workspace_root())
        .expect("activate frozen workspace");
    let workspace_identity = sigil_resource_authority::identity::canonical_identity(
        &workspace_a.canonicalize().expect("canonical workspace"),
    )
    .expect("workspace identity")
    .digest;
    assert_eq!(capsule.root_identity_hash, workspace_identity);
    assert_ne!(capsule.root_identity_hash, snapshot.config_hash());
}

#[test]
fn r71_snapshot_binding_changes_with_launch_cwd_for_default_workspace() {
    let _environment_guard = crate::test_env::lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace_a = dir.path().join("workspace-a");
    let workspace_b = dir.path().join("workspace-b");
    std::fs::create_dir_all(&workspace_a).expect("workspace a");
    std::fs::create_dir_all(&workspace_b).expect("workspace b");
    let config = dir.path().join("sigil.toml");
    std::fs::write(
            &config,
            "config_version = 2\n[workspace]\nroot = \".\"\n[agent]\nconnection = \"local-test\"\nmodel = \"test\"\n[connections.local-test]\nlabel = \"local\"\nprovider = \"custom\"\nprotocol = \"chat_completions\"\nbase_url = \"http://127.0.0.1:1\"\ncredential = { source = \"none\" }\n",
        )
        .expect("config");
    let snapshot_a = ValidatedAuthorityConfigSnapshotV1::load(&config, &workspace_a)
        .expect("load a")
        .expect("snapshot a");
    let snapshot_b = ValidatedAuthorityConfigSnapshotV1::load(&config, &workspace_b)
        .expect("load b")
        .expect("snapshot b");
    assert_ne!(snapshot_a.workspace_root(), snapshot_b.workspace_root());
    assert_ne!(snapshot_a.config_hash(), snapshot_b.config_hash());
    assert_ne!(snapshot_a.resolved_paths(), snapshot_b.resolved_paths());
}

#[test]
fn r71_reopen_ignores_settled_admission_after_source_bound_grant_rollover() {
    use crate::managed_storage_writer::StorageWriterChannelV1 as Ch;

    let dir = tempfile::tempdir().expect("tempdir");
    let state = dir.path().join("state");
    let exec = dir.path().join("exec");
    std::fs::create_dir_all(state.join("cache")).expect("cache dir");
    std::fs::create_dir_all(&exec).expect("exec dir");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))
            .expect("state mode");
        std::fs::set_permissions(&exec, std::fs::Permissions::from_mode(0o700)).expect("exec mode");
    }

    let planner: Arc<dyn sigil_kernel::managed_execution::ManagedExecutionPlannerV1> =
        Arc::new(crate::r71_shadow_planner::ShadowPlannerV1::new(
            crate::r71_shadow_planner::ShadowPlannerConfigV1::default(),
        ));
    let first = compose_runtime_authority(
        &state,
        &exec,
        CanonicalHash::from_bytes([0x55; 32]),
        planner,
        &[Ch::SessionLog],
    )
    .expect("first composition");
    let recovery = ApplicationResourceRecoveryFacadeV1::new();
    let first_probes = probe_mandatory_adapters(
        &first.services,
        &recovery,
        CanonicalHash::from_bytes([0x55; 32]),
        1,
    );
    assert!(
        first_probes
            .iter()
            .find(|probe| probe.adapter == MandatoryAdapterKindV1::StorageSessionLog)
            .is_some_and(|probe| probe.passed)
    );
    drop(first);

    // A new cutover manifest rolls the source-bound grants. The old startup admission is
    // already terminal and must remain historical evidence, not make the journal look
    // corrupt during the next authority composition.
    let planner: Arc<dyn sigil_kernel::managed_execution::ManagedExecutionPlannerV1> =
        Arc::new(crate::r71_shadow_planner::ShadowPlannerV1::new(
            crate::r71_shadow_planner::ShadowPlannerConfigV1::default(),
        ));
    let second = compose_runtime_authority(
        &state,
        &exec,
        CanonicalHash::from_bytes([0x56; 32]),
        planner,
        &[Ch::SessionLog],
    )
    .expect("settled historical admission must not block grant rollover");
    let second_probes = probe_mandatory_adapters(
        &second.services,
        &recovery,
        CanonicalHash::from_bytes([0x56; 32]),
        1,
    );
    assert!(
        second_probes
            .iter()
            .find(|probe| probe.adapter == MandatoryAdapterKindV1::StorageSessionLog)
            .is_some_and(|probe| probe.passed)
    );
}

#[test]
fn r71_reopen_recovers_pending_v2_admission_after_source_bound_grant_rollover() {
    use crate::managed_storage_writer::StorageWriterChannelV1 as Ch;

    let dir = tempfile::tempdir().expect("tempdir");
    let state = dir.path().join("state");
    let exec = dir.path().join("exec");
    std::fs::create_dir_all(state.join("cache")).expect("cache dir");
    std::fs::create_dir_all(&exec).expect("exec dir");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))
            .expect("state mode");
        std::fs::set_permissions(&exec, std::fs::Permissions::from_mode(0o700)).expect("exec mode");
    }

    let planner: Arc<dyn sigil_kernel::managed_execution::ManagedExecutionPlannerV1> =
        Arc::new(crate::r71_shadow_planner::ShadowPlannerV1::new(
            crate::r71_shadow_planner::ShadowPlannerConfigV1::default(),
        ));
    let first = compose_runtime_authority(
        &state,
        &exec,
        CanonicalHash::from_bytes([0x55; 32]),
        planner,
        &[Ch::SessionLog],
    )
    .expect("first composition");
    let pending = first
        .storage_writer
        .acquire_named(Ch::SessionLog, "pending-session")
        .expect("pending admission");
    first
        .storage_writer
        .write_record(&pending, b"{\"seq\":1}")
        .expect("pending physical write");
    let pending_path = pending.path().to_path_buf();
    let records_before =
        std::fs::read(pending_path.join("records.jsonl")).expect("pending physical records");
    drop(pending);
    drop(first);

    let planner: Arc<dyn sigil_kernel::managed_execution::ManagedExecutionPlannerV1> =
        Arc::new(crate::r71_shadow_planner::ShadowPlannerV1::new(
            crate::r71_shadow_planner::ShadowPlannerConfigV1::default(),
        ));
    let second = compose_runtime_authority(
        &state,
        &exec,
        CanonicalHash::from_bytes([0x56; 32]),
        planner,
        &[Ch::SessionLog],
    )
    .expect("pending historical admission must be reconciled across grant rollover");
    assert_eq!(
        std::fs::read(pending_path.join("records.jsonl")).expect("records retained"),
        records_before
    );
    let new_lease = second
        .storage_writer
        .acquire_named(Ch::SessionLog, "after-recovery")
        .expect("new admissions unblocked");
    second
        .storage_writer
        .finalize(new_lease)
        .expect("new admission finalizes");
}
