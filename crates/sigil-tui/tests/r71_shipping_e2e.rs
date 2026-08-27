//! R71 shipping checks must link the TUI library as a normal dependency. This keeps the
//! production launcher replacement path out of the `#[cfg(test)]` unit-test helper.

use anyhow::Result;
use sigil_kernel::RootConfig;
use sigil_tui::{app::AppState, launcher};

fn fixture() -> Result<(tempfile::TempDir, std::path::PathBuf, RootConfig)> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let state = temp.path().join("state");
    let cache = temp.path().join("cache");
    std::fs::create_dir_all(&workspace)?;
    std::fs::create_dir_all(&state)?;
    std::fs::create_dir_all(&cache)?;
    let config_path = temp.path().join("sigil.toml");
    let config_text = format!(
        "config_version = 2\n[workspace]\nroot = \"{}\"\n[storage]\nstate_root = \"{}\"\ncache_root = \"{}\"\n[agent]\nconnection = \"local-test\"\nmodel = \"persisted-model\"\n[connections.local-test]\nlabel = \"local\"\nprovider = \"custom\"\nprotocol = \"chat_completions\"\nbase_url = \"http://127.0.0.1:1\"\ncredential = {{ source = \"none\" }}\n",
        workspace.display(),
        state.display(),
        cache.display(),
    );
    std::fs::write(&config_path, config_text)?;
    let persisted = RootConfig::parse_persisted(&std::fs::read_to_string(&config_path)?)?;
    Ok((temp, config_path, persisted))
}

#[test]
fn production_boot_current_schema_uses_real_authority_transaction() -> Result<()> {
    let (_temp, config_path, persisted) = fixture()?;
    let transaction = sigil_runtime::r71_authority_composition::boot_current_schema(
        &config_path,
        config_path.parent().expect("config parent"),
    )?;

    assert_eq!(transaction.config().agent.model, persisted.agent.model);
    assert!(transaction.cutover().is_current_schema_ready());
    assert!(
        sigil_runtime::r71_authority_composition::authority_bootstrap_manifest_path(&config_path)?
            .is_file()
    );
    Ok(())
}

#[test]
fn production_launcher_replacement_keeps_session_runtime_config() -> Result<()> {
    let (temp, config_path, persisted) = fixture()?;
    let first =
        sigil_runtime::r71_authority_composition::boot_current_schema(&config_path, temp.path())?;
    let first_manifest = first.cutover().manifest().clone();
    let mut app = AppState::from_root_config(&config_path, &persisted);
    let (_, session_route) = sigil_runtime::provider_connections::resolve_model_route(
        &persisted,
        &sigil_kernel::ModelRef::new(
            sigil_kernel::ConnectionId::new("local-test".to_owned())?,
            "session-model".to_owned(),
        )?,
    )?;

    let returned = launcher::install_current_boot_transaction(
        &mut app,
        &config_path,
        Some(session_route),
        config_path.parent().expect("config parent"),
    )?;

    assert_eq!(returned.agent.model, "session-model");
    assert_eq!(
        app.workspace_root,
        temp.path().join("workspace").canonicalize()?
    );
    assert_eq!(app.sigil_paths.state_root, temp.path().join("state"));
    assert_eq!(app.sigil_paths.cache_root, temp.path().join("cache"));
    assert_eq!(
        RootConfig::parse_persisted(&std::fs::read_to_string(&config_path)?)?
            .agent
            .model,
        "persisted-model"
    );
    let published =
        sigil_runtime::r71_global_cutover::RuntimeGlobalCutoverV1::load_and_validate_manifest(
            &sigil_runtime::r71_authority_composition::authority_bootstrap_manifest_path(
                &config_path,
            )?,
        )?;
    assert_eq!(published, first_manifest);
    Ok(())
}
