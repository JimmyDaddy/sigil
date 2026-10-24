use super::*;

fn run_start_fixture() -> Result<(tempfile::TempDir, PathBuf, ApplicationSessionBinding)> {
    run_start_fixture_with_composition(sigil_kernel::RuntimeCompositionConfig::standard())
}

fn run_start_fixture_with_composition(
    composition: sigil_kernel::RuntimeCompositionConfig,
) -> Result<(tempfile::TempDir, PathBuf, ApplicationSessionBinding)> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    let storage_root = temp.path().to_string_lossy().replace('\\', "/");
    let composition_toml = toml::to_string(&composition.selection_only())?;
    std::fs::write(
        &config_path,
        format!(
            r#"config_version = 2
[workspace]
root = "."
[storage]
state_root = "{storage_root}/state"
cache_root = "{storage_root}/cache"
[composition]
{composition_toml}
[agent]
connection = "local-test"
model = "gpt-test"
[connections.local-test]
label = "Local test"
provider = "custom"
protocol = "chat_completions"
base_url = "http://127.0.0.1:1"
credential = {{ source = "none" }}
"#
        ),
    )?;
    let binding = bind_application_session(&config_path, temp.path(), None)?;
    Ok((temp, config_path, binding))
}

#[test]
fn run_start_keeps_nonempty_display_catalogs_out_of_admission_reads() -> Result<()> {
    use crate::provider_connections::{
        ModelAvailability, ModelCatalogEntry, ModelCatalogProvenance, ModelRecommendation,
        load_provider_connections, seed_unauthenticated_catalog_cache_for_test,
    };

    let (temp, config_path, binding) = run_start_fixture()?;
    let skill_root = temp.path().join(".sigil/skills/explain");
    std::fs::create_dir_all(&skill_root)?;
    std::fs::write(
        skill_root.join("SKILL.md"),
        "---\nname: explain\ndescription: Explain a change\n---\nDescribe the change.\n",
    )?;
    let config = RootConfig::load(&config_path)?;
    let connections = load_provider_connections(&config);
    let connection = &connections.connections[&ConnectionId::new("local-test")?].config;
    let cached_model = ModelRef::new(connection.id.clone(), "gpt-catalog-only")?;
    seed_unauthenticated_catalog_cache_for_test(
        &temp.path().join("cache"),
        connection,
        &[ModelCatalogEntry {
            model_ref: cached_model.clone(),
            display_name: "Catalog model".to_owned(),
            availability: ModelAvailability::Available,
            recommendation: ModelRecommendation::Standard,
            provenance: ModelCatalogProvenance::Remote,
        }],
    )?;
    let display = application_run_context_view(
        &config_path,
        temp.path(),
        &binding.session_log_path,
        &binding.session_scope_id,
    )?;
    assert!(
        display
            .extension_catalog
            .skills
            .iter()
            .any(|skill| skill.name == "explain")
    );
    assert!(
        display
            .model_options
            .iter()
            .any(|option| option.model_ref == cached_model)
    );

    // Catalog readers evict malformed persisted catalogs. This real observable effect proves
    // admission does not read the display catalog, with nonempty discovery established above.
    let cache_file = std::fs::read_dir(temp.path().join("cache/provider-models/v1/local-test"))?
        .next()
        .expect("seeded catalog must exist")?
        .path();
    std::fs::write(&cache_file, b"invalid catalog")?;
    let start = application_run_start_view(
        &config_path,
        &binding.session_log_path,
        &binding.session_scope_id,
        None,
    )?;
    assert_eq!(start.model_ref, display.model_ref);
    assert_eq!(
        start.model_selection_binding,
        display.model_selection_binding
    );
    assert_eq!(
        start.reasoning_effort_binding,
        display.reasoning_effort_binding
    );
    assert_eq!(
        start.default_permission_mode,
        display.default_permission_mode
    );
    assert_eq!(std::fs::read(&cache_file)?, b"invalid catalog");
    let refreshed_display = application_run_context_view(
        &config_path,
        temp.path(),
        &binding.session_log_path,
        &binding.session_scope_id,
    )?;
    assert!(
        !cache_file.exists(),
        "display still reads and repairs its catalog"
    );
    assert!(
        !refreshed_display
            .model_options
            .iter()
            .any(|option| option.model_ref == cached_model)
    );
    assert!(!refreshed_display.extension_catalog.skills.is_empty());
    Ok(())
}

#[test]
fn run_start_rejects_foreign_scope_and_refreshes_exact_recovery_frontier() -> Result<()> {
    let (temp, config_path, binding) = run_start_fixture()?;
    let before = std::fs::read(&binding.session_log_path)?;
    for scope in ["", "another-session"] {
        assert!(
            application_run_start_view(&config_path, &binding.session_log_path, scope, None)
                .is_err()
        );
    }
    assert_eq!(std::fs::read(&binding.session_log_path)?, before);
    let source = std::fs::read_to_string(&config_path)?;
    std::fs::write(&config_path, source.replace("127.0.0.1:1", "127.0.0.1:2"))?;
    let start = application_run_start_view(
        &config_path,
        &binding.session_log_path,
        &binding.session_scope_id,
        None,
    )?;
    let recovery = start
        .route_recovery
        .expect("changed egress requires confirmation");
    assert_eq!(
        recovery.code,
        ApplicationSessionRouteRecoveryCode::SessionRouteConfirmationRequired
    );
    let display = application_run_context_view(
        &config_path,
        temp.path(),
        &binding.session_log_path,
        &binding.session_scope_id,
    )?;
    assert_eq!(display.route_recovery, Some(recovery.clone()));
    JsonlSessionStore::new(&binding.session_log_path)?.append(&SessionLogEntry::User(
        sigil_kernel::ModelMessage::user("new durable frontier"),
    ))?;
    let changed = application_run_start_view(
        &config_path,
        &binding.session_log_path,
        &binding.session_scope_id,
        None,
    )?;
    assert_ne!(
        changed
            .route_recovery
            .expect("confirmation is still required")
            .recovery_binding,
        recovery.recovery_binding,
        "a previously displayed recovery cannot authorize a changed durable frontier"
    );
    Ok(())
}

#[test]
fn run_start_validates_requested_connection_without_catalog_eligibility() -> Result<()> {
    let (_temp, config_path, binding) = run_start_fixture()?;
    let source = std::fs::read_to_string(&config_path)?;
    std::fs::write(&config_path, source.replace("local-test", "replacement"))?;
    let replacement = ModelRef::new(ConnectionId::new("replacement")?, "custom-unlisted-model")?;
    let start = application_run_start_view(
        &config_path,
        &binding.session_log_path,
        &binding.session_scope_id,
        Some(&replacement),
    )?;
    assert_eq!(
        start.route_recovery.as_ref().map(|recovery| recovery.code),
        Some(ApplicationSessionRouteRecoveryCode::SessionRouteSelectionRequired)
    );
    assert!(
        start.requested_model_available,
        "custom models need a real route, not a listing"
    );
    let missing = ModelRef::new(ConnectionId::new("missing")?, "gpt-test")?;
    let unavailable = application_run_start_view(
        &config_path,
        &binding.session_log_path,
        &binding.session_scope_id,
        Some(&missing),
    )?;
    assert!(!unavailable.requested_model_available);
    assert_eq!(start.route_recovery, unavailable.route_recovery);
    Ok(())
}

#[test]
fn run_start_composition_recovery_distinguishes_valid_mismatch_from_invalid_history() -> Result<()>
{
    let mut config = crate::provider_connections::default_setup_root_config();
    config.composition = sigil_kernel::RuntimeCompositionConfig::core();
    let original = sigil_kernel::SessionCompositionSnapshotV1::new(config.selected_capabilities());
    config
        .composition
        .enhancements
        .insert(sigil_kernel::OptionalCapability::Mcp);
    let expected = sigil_kernel::SessionCompositionSnapshotV1::new(config.selected_capabilities());
    let binding = SessionLogEntry::Control(ControlEntry::SessionCompositionBound(original.clone()));
    let entries = vec![binding.clone()];
    let before = serde_json::to_vec(&entries)?;
    let recovery = composition_recovery_view(&config, &entries, "fixture-scope")?
        .expect("valid capability change has its own recovery");
    assert_eq!(
        recovery.code,
        ApplicationSessionRouteRecoveryCode::SessionCompositionChanged
    );
    assert_eq!(
        recovery.allowed_actions,
        [
            ApplicationSessionRouteRecoveryAction::StartNewSession,
            ApplicationSessionRouteRecoveryAction::BackToSessionLibrary,
        ]
    );
    assert!(!recovery.retryable);
    let mismatch =
        crate::session_composition::validate_session_composition_entries(&entries, &expected)
            .expect_err("capability mismatch");
    assert_eq!(
        ApplicationRunPrepareError::execution(mismatch).class(),
        ApplicationRunPrepareErrorClass::SessionCompositionChanged
    );
    assert_eq!(serde_json::to_vec(&entries)?, before);

    let execution = SessionLogEntry::Assistant(sigil_kernel::ModelMessage::assistant(
        Some("historical answer".to_owned()),
        Vec::new(),
    ));
    let mut corrupt = original;
    corrupt.schema_version = 0;
    for (label, invalid) in [
        ("missing", vec![execution.clone()]),
        ("duplicate", vec![binding.clone(), binding.clone()]),
        ("late", vec![execution, binding]),
        (
            "corrupt",
            vec![SessionLogEntry::Control(
                ControlEntry::SessionCompositionBound(corrupt),
            )],
        ),
    ] {
        let before = serde_json::to_vec(&invalid)?;
        let error = composition_recovery_view(&config, &invalid, "fixture-scope")
            .expect_err("invalid history must not get capability-change recovery");
        assert!(!error.is::<crate::SessionCompositionMismatch>(), "{label}");
        assert_eq!(
            ApplicationRunPrepareError::execution(error).class(),
            ApplicationRunPrepareErrorClass::Execution,
            "{label}"
        );
        assert_eq!(serde_json::to_vec(&invalid)?, before, "{label}");
    }
    Ok(())
}

#[test]
fn run_start_composition_change_keeps_history_readable_and_requires_a_new_session() -> Result<()> {
    let (temp, config_path, binding) =
        run_start_fixture_with_composition(sigil_kernel::RuntimeCompositionConfig::new(
            sigil_kernel::RuntimeCompositionProfile::Core,
            [sigil_kernel::OptionalCapability::TaskOrchestration],
        ))?;
    let store = JsonlSessionStore::new(&binding.session_log_path)?;
    store.append(&SessionLogEntry::User(sigil_kernel::ModelMessage::user(
        "original request",
    )))?;
    store.append(&SessionLogEntry::Assistant(
        sigil_kernel::ModelMessage::assistant(Some("original answer".to_owned()), Vec::new()),
    ))?;
    drop(store);
    let before = std::fs::read(&binding.session_log_path)?;
    let original = RootConfig::load_persisted(&config_path)?;
    assert!(
        !original
            .composition
            .allows(sigil_kernel::OptionalCapability::Mcp)
    );
    let import = crate::mcp_import::preview_mcp_configuration_import(
        br#"{"mcpServers":{"selected":{"command":"must-not-start-during-context-read"}}}"#,
    )?;
    let selected = import.selected_root_configuration(&[0], &original)?;
    selected.save(&config_path)?;

    let recovery = application_session_composition_recovery_view(
        &config_path,
        &binding.session_log_path,
        &binding.session_scope_id,
    )?
    .expect("old session needs a new execution selection");
    assert_eq!(
        recovery.code,
        ApplicationSessionRouteRecoveryCode::SessionCompositionChanged
    );
    assert_eq!(
        recovery.allowed_actions,
        [
            ApplicationSessionRouteRecoveryAction::StartNewSession,
            ApplicationSessionRouteRecoveryAction::BackToSessionLibrary,
        ]
    );
    assert!(!recovery.retryable);
    assert!(
        recovery
            .recovery_binding
            .starts_with("session-composition:")
    );
    for forbidden in [
        "must-not-start",
        "127.0.0.1",
        binding.session_scope_id.as_str(),
    ] {
        assert!(!recovery.recovery_binding.contains(forbidden));
    }
    let start = application_run_start_view(
        &config_path,
        &binding.session_log_path,
        &binding.session_scope_id,
        None,
    )?;
    let context = application_run_context_view(
        &config_path,
        temp.path(),
        &binding.session_log_path,
        &binding.session_scope_id,
    )?;
    assert_eq!(start.route_recovery, Some(recovery.clone()));
    assert_eq!(context.route_recovery, Some(recovery.clone()));
    assert_eq!(
        application_session_composition_recovery_view(
            &config_path,
            &binding.session_log_path,
            &binding.session_scope_id,
        )?,
        Some(recovery)
    );
    assert_eq!(
        std::fs::read(&binding.session_log_path)?,
        before,
        "context reads must not append a replacement binding or recover the writer"
    );
    for scope in ["", "foreign-scope"] {
        assert!(
            application_session_composition_recovery_view(
                &config_path,
                &binding.session_log_path,
                scope,
            )
            .is_err()
        );
    }

    let mut request = ApplicationRunRequest::non_interactive(
        &config_path,
        temp.path(),
        "continue original session",
        "composition-old-run",
    );
    request.session_path = Some(binding.session_log_path.clone());
    let error = match super::super::prepare_application_run_blocking(
        request,
        std::sync::Arc::new(ApplicationSessionLeaseManager::new()),
        false,
        None,
    ) {
        Ok(_) => panic!("old session must not prepare with newly enabled capabilities"),
        Err(error) => error,
    };
    assert_eq!(
        error.class(),
        ApplicationRunPrepareErrorClass::SessionCompositionChanged
    );
    assert_eq!(std::fs::read(&binding.session_log_path)?, before);

    let new_path = temp.path().join("new-capabilities.jsonl");
    let fresh = bind_application_session(&config_path, temp.path(), Some(&new_path))?;
    assert_ne!(fresh.session_scope_id, binding.session_scope_id);
    assert!(
        application_session_composition_recovery_view(
            &config_path,
            &fresh.session_log_path,
            &fresh.session_scope_id,
        )?
        .is_none()
    );
    let fresh_start = application_run_start_view(
        &config_path,
        &fresh.session_log_path,
        &fresh.session_scope_id,
        None,
    )?;
    assert!(fresh_start.route_recovery.is_none());
    let mut fresh_request = ApplicationRunRequest::non_interactive(
        &config_path,
        temp.path(),
        "inspect selected capabilities",
        "composition-new-run",
    );
    fresh_request.session_path = Some(fresh.session_log_path.clone());
    let prepared = super::super::prepare_application_run_blocking(
        fresh_request,
        std::sync::Arc::new(ApplicationSessionLeaseManager::new()),
        false,
        None,
    )?;
    assert!(
        prepared
            .root_config
            .selected_capabilities()
            .contains(&sigil_kernel::OptionalCapability::Mcp)
    );
    crate::validate_session_composition(&prepared.session, &prepared.root_config)?;
    assert!(
        prepared
            .session
            .entries()
            .iter()
            .any(|entry| matches!(entry,
                SessionLogEntry::Control(ControlEntry::SessionCompositionBound(snapshot))
                    if snapshot.capabilities.contains(&sigil_kernel::OptionalCapability::Mcp)
            ))
    );
    drop(prepared);
    assert_eq!(
        std::fs::read(&binding.session_log_path)?,
        before,
        "new-session preparation must leave the old transcript untouched"
    );
    Ok(())
}

#[test]
fn run_start_composition_recovery_does_not_repair_a_corrupt_session_stream() -> Result<()> {
    let (temp, config_path, binding) = run_start_fixture()?;
    let mut corrupt = std::fs::read(&binding.session_log_path)?;
    assert!(!corrupt.is_empty());
    corrupt[0] = b'!';
    std::fs::write(&binding.session_log_path, &corrupt)?;
    for observed in [
        application_session_composition_recovery_view(
            &config_path,
            &binding.session_log_path,
            &binding.session_scope_id,
        )
        .map(|_| ()),
        application_run_start_view(
            &config_path,
            &binding.session_log_path,
            &binding.session_scope_id,
            None,
        )
        .map(|_| ()),
        application_run_context_view(
            &config_path,
            temp.path(),
            &binding.session_log_path,
            &binding.session_scope_id,
        )
        .map(|_| ()),
    ] {
        let error = observed.expect_err("corrupt streams must not get composition recovery");
        assert!(!error.is::<crate::SessionCompositionMismatch>());
        assert_eq!(
            std::fs::read(&binding.session_log_path)?,
            corrupt,
            "read-only recovery must preserve the corrupt source for diagnosis"
        );
    }
    Ok(())
}
