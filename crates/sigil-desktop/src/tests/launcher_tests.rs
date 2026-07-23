use super::*;

#[test]
fn launch_request_debug_redacts_local_paths() {
    let canary = "/private/canary/workspace";
    let request = DesktopLaunchRequest::new(
        "/private/canary/sigil",
        "/private/canary/sigil.toml",
        canary,
    );
    let debug = format!("{request:?}");

    assert!(!debug.contains(canary));
    assert!(!debug.contains("sigil.toml"));
    assert!(debug.contains("<local path>"));
}

#[test]
fn implicit_user_config_launch_does_not_require_or_pass_workspace_config() {
    let binary = std::env::current_exe().expect("current test binary should resolve");
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let request = DesktopLaunchRequest::with_implicit_user_config(&binary, &workspace);

    request
        .validate()
        .expect("workspace launch should not require a local config");
    let command = build_server_command(&request, "test-bearer");
    let args = command
        .as_std()
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();

    assert_eq!(
        args,
        [
            "serve",
            "--startup-output",
            "json",
            "--shutdown-on-stdin-close"
        ]
    );
    assert!(!args.iter().any(|arg| arg == "--config"));
    assert!(!workspace.join("sigil.toml").exists());
}

#[test]
fn explicit_config_launch_keeps_the_config_argument() {
    let request = DesktopLaunchRequest::new(
        "/private/canary/sigil",
        "/private/canary/custom.toml",
        "/private/canary/workspace",
    );
    let command = build_server_command(&request, "test-bearer");
    let args = command
        .as_std()
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();

    assert_eq!(
        args,
        [
            "--config",
            "/private/canary/custom.toml",
            "serve",
            "--startup-output",
            "json",
            "--shutdown-on-stdin-close"
        ]
    );
}

#[tokio::test]
async fn startup_line_reader_enforces_single_record_cap() {
    let mut valid = &b"{\"schema_version\":1}\nignored"[..];
    assert_eq!(
        read_startup_line(&mut valid)
            .await
            .expect("line should decode"),
        br#"{"schema_version":1}"#
    );

    let oversized = vec![b'x'; MAX_BOOTSTRAP_BYTES + 1];
    let mut oversized = oversized.as_slice();
    assert!(matches!(
        read_startup_line(&mut oversized).await,
        Err(DesktopLaunchError::ReadinessTooLarge)
    ));
}
