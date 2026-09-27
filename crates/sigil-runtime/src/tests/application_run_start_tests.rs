use super::*;

fn run_start_fixture() -> Result<(tempfile::TempDir, PathBuf, ApplicationSessionBinding)> {
    let temp = tempfile::tempdir()?;
    let config_path = temp.path().join("sigil.toml");
    let storage_root = temp.path().to_string_lossy().replace('\\', "/");
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
profile = "standard"
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
