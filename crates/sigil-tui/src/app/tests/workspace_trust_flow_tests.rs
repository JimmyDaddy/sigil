use super::*;
use sigil_kernel::{
    StorageConfig, WorkspaceTrust, WorkspaceTrustDecisionEntry, stable_workspace_id,
};

#[test]
fn workspace_trust_gate_enter_persists_decision() -> Result<()> {
    let temp = tempdir()?;
    let config = RootConfig {
        config_version: 2,
        workspace: WorkspaceConfig {
            root: temp.path().display().to_string(),
        },
        ..test_config()
    };
    let mut app = AppState::from_root_config(temp.path().join("sigil.toml").as_path(), &config);

    app.enter_workspace_trust_gate()?;
    assert!(app.is_workspace_trust_gate_mode());
    assert!(
        app.workspace_trust_gate_lines()
            .join("\n")
            .contains("Enter trust and continue")
    );

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE))?;
    assert!(action.is_none());
    assert!(!app.should_quit);

    let action = app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
    assert!(matches!(action, Some(AppAction::TrustWorkspace)));
    app.confirm_workspace_trust_gate()?;

    assert!(!app.is_workspace_trust_gate_mode());
    let workspace_id = stable_workspace_id(temp.path())?;
    let entries = JsonlSessionStore::read_entries(&app.session_log_path)?;
    assert!(entries.iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::WorkspaceTrustDecision(decision))
                if decision.workspace_id == workspace_id
                    && decision.trust == WorkspaceTrust::Trusted
                    && decision.reason.as_deref()
                        == Some("trusted by user at workspace entry")
        )
    }));
    assert_eq!(app.last_notice(), Some("workspace trusted"));
    Ok(())
}

#[test]
fn workspace_trust_history_detects_prior_trusted_session() -> Result<()> {
    let temp = tempdir()?;
    let config = RootConfig {
        config_version: 2,
        workspace: WorkspaceConfig {
            root: temp.path().display().to_string(),
        },
        ..test_config()
    };
    let workspace_id = stable_workspace_id(temp.path())?;
    let session_dir = resolved_session_log_dir(&config, temp.path());
    let prior_session = session_dir.join("session-trusted.jsonl");
    write_session_log(
        &prior_session,
        &[SessionLogEntry::Control(
            ControlEntry::WorkspaceTrustDecision(WorkspaceTrustDecisionEntry {
                workspace_id: workspace_id.clone(),
                workspace_trust_snapshot_id: format!("workspace-trust:{workspace_id}"),
                trust: WorkspaceTrust::Trusted,
                decided_by_event_id: Some("event-trust".to_owned()),
                reason: Some("test prior trust".to_owned()),
            }),
        )],
    )?;

    let app = AppState::from_root_config(temp.path().join("sigil.toml").as_path(), &config);

    assert!(app.workspace_is_trusted_from_history());
    Ok(())
}

#[test]
fn workspace_trust_history_detects_prior_managed_session() -> Result<()> {
    use sigil_runtime::managed_storage_writer::StorageWriterChannelV1 as Ch;

    let temp = tempdir()?;
    let state = temp.path().join("state");
    let cache = state.join("cache");
    let exec = temp.path().join("exec");
    for path in [&state, &cache, &exec] {
        std::fs::create_dir_all(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    let planner = std::sync::Arc::new(sigil_runtime::r71_shadow_planner::ShadowPlannerV1::new(
        sigil_runtime::r71_shadow_planner::ShadowPlannerConfigV1::default(),
    ));
    let composition = std::sync::Arc::new(
        sigil_runtime::r71_authority_composition::compose_runtime_authority(
            &state,
            &exec,
            sigil_kernel::resource::CanonicalHash::from_bytes([0x44; 32]),
            sigil_kernel::resource::AuthorityGeneration {
                epoch: 1,
                instance_hash: sigil_kernel::resource::CanonicalHash::from_bytes([0x75; 32]),
            },
            planner,
            &[Ch::SessionLog],
        )?,
    );
    let config = RootConfig {
        config_version: 2,
        workspace: WorkspaceConfig {
            root: temp.path().display().to_string(),
        },
        storage: StorageConfig {
            state_root: sigil_kernel::StorageRoot::Path(state.display().to_string()),
            cache_root: sigil_kernel::StorageRoot::Path(cache.display().to_string()),
            ..StorageConfig::default()
        },
        ..test_config()
    };

    let mut trusted_app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    trusted_app.set_authority_composition(std::sync::Arc::clone(&composition));
    trusted_app.enter_workspace_trust_gate()?;
    trusted_app.confirm_workspace_trust_gate()?;
    let trusted_path = trusted_app.session_log_path.clone();
    assert!(
        trusted_path
            .to_string_lossy()
            .contains("/managed/session-log/")
    );

    let mut app = AppState::from_root_config(Path::new("sigil.toml"), &config);
    app.set_authority_composition(composition);
    assert!(app.session_log_path != trusted_path);
    assert!(app.workspace_is_trusted_from_history());
    Ok(())
}
