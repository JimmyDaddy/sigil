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
fn explicit_config_launch_allows_first_run_path_that_does_not_exist_yet() {
    let binary = std::env::current_exe().expect("current test binary should resolve");
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let config = std::env::temp_dir().join(format!(
        "sigil-desktop-missing-{}.toml",
        uuid::Uuid::new_v4()
    ));
    let request = DesktopLaunchRequest::new(&binary, &config, &workspace);

    request
        .validate()
        .expect("first-run launch should allow a config path before its first publish");
    assert!(!config.exists());
}

#[test]
fn explicit_config_launch_rejects_an_existing_directory() {
    let binary = std::env::current_exe().expect("current test binary should resolve");
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let request = DesktopLaunchRequest::new(&binary, &workspace, &workspace);

    assert!(matches!(
        request.validate(),
        Err(DesktopLaunchError::InvalidRequest(
            "configuration is not a file"
        ))
    ));
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

#[test]
fn startup_stderr_classification_is_bounded_and_path_free() {
    assert_eq!(
        classify_startup_stderr(
            b"error: failed to acquire durable lease /private/path.lock: Resource temporarily unavailable"
        ),
        Some(DesktopStartupFailure::WorkspaceBusy)
    );
    assert_eq!(
        classify_startup_stderr(
            b"error: http protocol journal is corrupt: non-canonical durable event"
        ),
        Some(DesktopStartupFailure::AdapterStateInvalid)
    );
    assert_eq!(
        classify_startup_stderr(b"error: Address already in use (os error 48)"),
        Some(DesktopStartupFailure::LoopbackUnavailable)
    );
    assert_eq!(classify_startup_stderr(b"arbitrary private failure"), None);
}
