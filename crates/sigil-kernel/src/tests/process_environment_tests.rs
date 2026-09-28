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

#[test]
fn extension_lifetime_readiness_requires_exact_confirmed_stop() -> anyhow::Result<()> {
    use crate::{JsonlSessionStore, MutationEventRecorder, VerificationScope};
    let fixture = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(fixture.path().join("session.jsonl"))?;
    let recorder = MutationEventRecorder::new(store.clone());
    let append = |subject: &str, generation: &str, status, read_only: bool| {
        recorder.append_extension_process_lifecycle(&ExtensionProcessLifecycleAudit {
            process_kind: "mcp_stdio".to_owned(),
            subject: subject.to_owned(),
            phase: ExtensionProcessLaunchPhase::PostSpawn,
            status,
            safe_metadata: BTreeMap::from([
                ("process_generation".to_owned(), generation.to_owned()),
                (
                    "workspace_effect".to_owned(),
                    if read_only { "read_only" } else { "unknown" }.to_owned(),
                ),
            ]),
        })
    };
    let scope = VerificationScope::all_tracked("fixture-scope");
    append(
        "records",
        "old",
        ExtensionProcessLifecycleStatus::Starting,
        false,
    )?;
    append(
        "records",
        "old",
        ExtensionProcessLifecycleStatus::Running,
        false,
    )?;
    append(
        "records",
        "replacement",
        ExtensionProcessLifecycleStatus::Running,
        false,
    )?;
    append(
        "records",
        "old",
        ExtensionProcessLifecycleStatus::Stopped,
        false,
    )?;
    let evidence =
        active_extension_mutation_evidence(&store.read_event_records_coordinated()?, &scope);
    assert_eq!(
        evidence.len(),
        1,
        "old generation stop cannot clear its replacement"
    );
    assert!(evidence[0].unknown_dirty);
    append(
        "records",
        "replacement",
        ExtensionProcessLifecycleStatus::StopUnconfirmed,
        false,
    )?;
    assert_eq!(
        active_extension_mutation_evidence(&store.read_event_records_coordinated()?, &scope).len(),
        1
    );
    append(
        "records",
        "replacement",
        ExtensionProcessLifecycleStatus::Stopped,
        false,
    )?;
    assert!(
        active_extension_mutation_evidence(&store.read_event_records_coordinated()?, &scope)
            .is_empty()
    );
    append(
        "readonly",
        "sandboxed",
        ExtensionProcessLifecycleStatus::Starting,
        false,
    )?;
    append(
        "readonly",
        "sandboxed",
        ExtensionProcessLifecycleStatus::Running,
        true,
    )?;
    assert!(
        active_extension_mutation_evidence(&store.read_event_records_coordinated()?, &scope)
            .is_empty(),
        "actual enforced read-only receipt does not dirty the workspace"
    );
    Ok(())
}
