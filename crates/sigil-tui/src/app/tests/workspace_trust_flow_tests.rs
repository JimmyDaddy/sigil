use super::*;
use sigil_kernel::{WorkspaceTrust, WorkspaceTrustDecisionEntry, stable_workspace_id};

#[test]
fn workspace_trust_gate_enter_persists_decision() -> Result<()> {
    let temp = tempdir()?;
    let config = RootConfig {
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
