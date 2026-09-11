use super::*;

#[test]
fn workspace_open_request_debug_redacts_every_native_path() {
    let request = DesktopWorkspaceOpenRequest::new(
        DesktopLaunchRequest::new(
            "/private/canary/sigil",
            "/private/canary/sigil.toml",
            "/private/canary/workspace",
        ),
        "workspace",
    );
    let debug = format!("{request:?}");

    assert!(!debug.contains("/private/canary"));
    assert!(debug.contains("<local path>"));
}

#[test]
fn display_name_validation_rejects_empty_control_and_oversized_values() {
    assert!(validate_display_name("workspace").is_ok());
    assert!(validate_display_name(" ").is_err());
    assert!(validate_display_name("bad\nname").is_err());
    assert!(validate_display_name(&"x".repeat(161)).is_err());
}

#[test]
fn discarded_open_ticket_releases_only_its_lifecycle_markers() {
    let manager = DesktopWorkspaceManager::default();
    let canonical_root = PathBuf::from("/private/canary/workspace");
    {
        let mut state = manager.lock_state();
        state.opening_roots.insert(canonical_root.clone());
    }
    let ticket = DesktopWorkspaceOpenTicket {
        canonical_root: canonical_root.clone(),
        display_name: "workspace".to_owned(),
        launch: DesktopLaunchRequest::with_implicit_user_config(
            "/private/canary/sigil",
            &canonical_root,
        ),
        existing: None,
    };

    manager.discard_open_ticket(&ticket);

    let state = manager.lock_state();
    assert!(!state.opening_roots.contains(&canonical_root));
    assert!(state.workspaces.is_empty());
}
