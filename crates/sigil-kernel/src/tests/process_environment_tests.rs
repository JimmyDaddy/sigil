use super::*;

#[test]
fn extension_process_network_approval_error_has_stable_label_and_constructor() {
    let error =
        ExtensionProcessLaunchError::network_approval_required("extension", "network approval");
    assert_eq!(
        error.code,
        ExtensionProcessLaunchErrorCode::NetworkApprovalRequired
    );
    assert_eq!(error.code.as_str(), "network_approval_required");
    assert_eq!(error.code.to_string(), "network_approval_required");
    assert_eq!(error.subject, "extension");
}

#[test]
fn extension_environment_baseline_names_are_a_stable_platform_snapshot() {
    #[cfg(not(windows))]
    let expected = vec![
        "PATH", "LANG", "LC_ALL", "LC_CTYPE", "TZ", "TMPDIR", "TMP", "TEMP",
    ];
    #[cfg(windows)]
    let expected = vec![
        "PATH",
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
        "TZ",
        "TMPDIR",
        "TMP",
        "TEMP",
        "SystemRoot",
        "WINDIR",
        "ComSpec",
        "PATHEXT",
    ];

    assert_eq!(extension_baseline_environment_names(), expected);
    assert!(!expected.contains(&"HOME"));
    assert!(!expected.contains(&"SSH_AUTH_SOCK"));
    assert!(!expected.contains(&"HTTP_PROXY"));
    assert!(!expected.contains(&"HTTPS_PROXY"));
    assert!(!expected.contains(&"SIGIL_API_KEY"));
}

#[test]
fn extension_environment_normalizes_names_and_rejects_invalid_names() {
    let names = vec![
        "TOKEN_B".to_owned(),
        "TOKEN_A".to_owned(),
        "TOKEN_B".to_owned(),
    ];
    assert_eq!(
        normalize_environment_variable_names(&names).expect("names should normalize"),
        vec!["TOKEN_A", "TOKEN_B"]
    );
    let error = normalize_environment_variable_names(&["BAD-NAME".to_owned()])
        .expect_err("invalid name should fail");
    assert_eq!(
        error.code,
        ExtensionProcessLaunchErrorCode::ConfigurationInvalid
    );
}

#[test]
fn extension_environment_is_isolated_keyed_and_redacted() {
    let key = [7_u8; 32];
    let resolve = |token: &str| {
        resolve_extension_process_environment_with(
            &["SIGIL_API_KEY".to_owned()],
            |name| Ok((name == "PATH").then(|| "/usr/bin:/bin".to_owned())),
            |_name| Ok(Some(token.to_owned())),
            &key,
        )
    };
    let first = resolve("top-secret-one").expect("environment should resolve");
    let same = resolve("top-secret-one").expect("environment should resolve");
    let changed = resolve("top-secret-two").expect("environment should resolve");

    assert_eq!(first.policy(), ProcessEnvironmentPolicy::IsolatedExtension);
    assert_eq!(first.live_fingerprint(), same.live_fingerprint());
    assert_ne!(first.live_fingerprint(), changed.live_fingerprint());
    assert!(format!("{first:?}").contains("[redacted]"));
    assert!(!format!("{first:?}").contains("top-secret-one"));
    assert!(!first.baseline_names().iter().any(|name| name == "HOME"));
    assert_eq!(first.grant_names(), &["SIGIL_API_KEY"]);
}

#[test]
fn extension_environment_reports_missing_grant_without_value_material() {
    let error = resolve_extension_process_environment_with(
        &["MISSING_TOKEN".to_owned()],
        |_name| Ok(None),
        |_name| Ok(None),
        &[9_u8; 32],
    )
    .expect_err("missing grant should fail");
    assert_eq!(
        error.code,
        ExtensionProcessLaunchErrorCode::ConfigurationInvalid
    );
    assert!(error.message.contains("MISSING_TOKEN"));
}
