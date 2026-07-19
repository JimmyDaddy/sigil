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
