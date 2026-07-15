use std::{fs, path::PathBuf};

#[cfg(unix)]
use std::os::unix::fs::{PermissionsExt, symlink};

use anyhow::Result;
use serde_json::json;
use sigil_kernel::SecretRedactor;
use tempfile::tempdir;

use super::*;

fn build_info() -> SupportBuildInfo {
    SupportBuildInfo::new("0.0.1-alpha.3", "abc123", "aarch64-apple-darwin", "release")
}

fn environment() -> SupportEnvironmentV1 {
    SupportEnvironmentV1::from_terminal_values(
        "macos",
        "aarch64",
        Some("iTerm.app"),
        Some("xterm-256color"),
    )
}

fn support_bundle() -> Result<SupportBundleV1> {
    let report = DoctorReport {
        checks: vec![DoctorCheck {
            status: DoctorStatus::Ok,
            name: "config:load".to_owned(),
            message: "config parsed".to_owned(),
            remediation: None,
        }],
    };
    let build = build_info();
    let environment = environment();
    let redactor = SecretRedactor::empty();
    let doctor = project_doctor_support_report_v1(
        &report,
        DoctorSupportProjectionContext {
            generated_at_unix_ms: 123,
            build: &build,
            environment: &environment,
            redactor: &redactor,
            path_redactions: &[],
        },
    )?;
    let session = project_support_session_summary_v1(
        "session-123",
        7,
        "deepseek",
        "deepseek-v4-flash",
        SupportRunPhase::Idle,
        false,
        SupportSessionProjectionContext {
            redactor: &redactor,
            path_redactions: &[],
        },
    )?;
    Ok(SupportBundleV1::new(doctor, Some(session)))
}

#[test]
fn doctor_support_schema_v1_matches_exact_fixture_and_rejects_unknown_fields() -> Result<()> {
    let report = DoctorReport {
        checks: vec![
            DoctorCheck {
                status: DoctorStatus::Ok,
                name: "config:load".to_owned(),
                message: "config parsed".to_owned(),
                remediation: None,
            },
            DoctorCheck {
                status: DoctorStatus::Warn,
                name: "execution:sandbox".to_owned(),
                message: "sandbox fallback is local".to_owned(),
                remediation: Some("choose a sandbox backend".to_owned()),
            },
        ],
    };
    let build = build_info();
    let environment = environment();
    let redactor = SecretRedactor::empty();
    let projected = project_doctor_support_report_v1(
        &report,
        DoctorSupportProjectionContext {
            generated_at_unix_ms: 123,
            build: &build,
            environment: &environment,
            redactor: &redactor,
            path_redactions: &[],
        },
    )?;
    let value = serde_json::to_value(&projected)?;
    assert_eq!(
        value,
        json!({
            "schema_version": 1,
            "generated_at_unix_ms": 123,
            "build": {
                "version": "0.0.1-alpha.3",
                "commit": "abc123",
                "target": "aarch64-apple-darwin",
                "profile": "release"
            },
            "environment": {
                "os": "macos",
                "architecture": "aarch64",
                "terminal_family": "iterm2"
            },
            "summary": {
                "overall_status": "warn",
                "ok": 1,
                "warn": 1,
                "error": 0
            },
            "checks": [
                {
                    "status": "ok",
                    "name": "config:load",
                    "summary": "config parsed",
                    "remediation": null
                },
                {
                    "status": "warn",
                    "name": "execution:sandbox",
                    "summary": "sandbox fallback is local",
                    "remediation": "choose a sandbox backend"
                }
            ],
            "privacy": {
                "included": [
                    "build_metadata",
                    "os_arch",
                    "terminal_family",
                    "doctor_status_and_redacted_checks",
                    "provider_and_model_labels",
                    "mcp_aliases",
                    "credential_environment_variable_names",
                    "capability_and_sandbox_status"
                ],
                "excluded": [
                    "conversation_content",
                    "tool_input_output",
                    "file_content_and_diff",
                    "config_file_content",
                    "credential_and_environment_values",
                    "local_paths_and_private_endpoints",
                    "session_log_content"
                ],
                "review_before_sharing": true
            }
        })
    );
    let round_trip: DoctorSupportReportV1 = serde_json::from_value(value.clone())?;
    assert_eq!(round_trip, projected);

    let mut with_unknown = value;
    with_unknown
        .as_object_mut()
        .expect("report is an object")
        .insert("future_field".to_owned(), json!(true));
    assert!(serde_json::from_value::<DoctorSupportReportV1>(with_unknown).is_err());
    Ok(())
}

#[test]
fn terminal_projection_uses_only_coarse_allowlisted_values() -> Result<()> {
    let canaries = [
        "TERM-CANARY-xterm-private",
        "PROGRAM-CANARY-private-terminal",
        "VERSION-CANARY-99.88.77",
        "PROFILE-CANARY-client-project",
    ];
    let report = DoctorReport {
        checks: vec![DoctorCheck {
            status: DoctorStatus::Warn,
            name: "terminal:profile".to_owned(),
            message: format!(
                "TERM={} TERM_PROGRAM={} TERM_PROGRAM_VERSION={} ITERM_PROFILE={}",
                canaries[0], canaries[1], canaries[2], canaries[3]
            ),
            remediation: Some(format!("remove {}", canaries[3])),
        }],
    };
    let build = build_info();
    let environment = SupportEnvironmentV1::from_terminal_values(
        "macos",
        "aarch64",
        Some("PROGRAM-CANARY-private-terminal"),
        Some("TERM-CANARY-xterm-private"),
    );
    let redactor = SecretRedactor::empty();
    let projected = project_doctor_support_report_v1(
        &report,
        DoctorSupportProjectionContext {
            generated_at_unix_ms: 123,
            build: &build,
            environment: &environment,
            redactor: &redactor,
            path_redactions: &[],
        },
    )?;
    let json = projected.to_pretty_json()?;
    for canary in canaries {
        assert!(!json.contains(canary), "terminal canary leaked: {canary}");
    }
    assert_eq!(
        projected.environment.terminal_family,
        SupportTerminalFamily::Other
    );
    assert_eq!(
        projected.checks[0].summary,
        "terminal compatibility check needs attention"
    );
    Ok(())
}

#[test]
fn doctor_projection_redacts_known_secrets_paths_endpoints_and_unknown_categories() -> Result<()> {
    let secret = "support-secret-canary-12345";
    let home = PathBuf::from("/Users/private-person");
    let workspace = home.join("Client Project");
    let config = home.join(".config/sigil/sigil.toml");
    let report = DoctorReport {
        checks: vec![
            DoctorCheck {
                status: DoctorStatus::Error,
                name: "provider:private".to_owned(),
                message: format!(
                    "token={secret} workspace={} config={} base_url=https://private.internal.example/v1?token={secret}",
                    workspace.display(),
                    config.display()
                ),
                remediation: Some(format!(
                    "inspect {}/logs and Bearer {secret}",
                    home.display()
                )),
            },
            DoctorCheck {
                status: DoctorStatus::Warn,
                name: format!("future:{secret}"),
                message: format!("unknown detail {secret}"),
                remediation: None,
            },
        ],
    };
    let build = build_info();
    let environment = environment();
    let redactor = SecretRedactor::from_values([secret]);
    let path_redactions = [
        SupportPathRedaction::new(&home, SupportPathKind::Home),
        SupportPathRedaction::new(&workspace, SupportPathKind::Workspace),
        SupportPathRedaction::new(&config, SupportPathKind::Config),
    ];
    let projected = project_doctor_support_report_v1(
        &report,
        DoctorSupportProjectionContext {
            generated_at_unix_ms: 123,
            build: &build,
            environment: &environment,
            redactor: &redactor,
            path_redactions: &path_redactions,
        },
    )?;
    let json = projected.to_pretty_json()?;
    for canary in [secret, "/Users/private-person", "private.internal.example"] {
        assert!(!json.contains(canary), "private value leaked: {canary}");
    }
    assert!(json.contains("<workspace_path>"));
    assert!(json.contains("<config_path>"));
    assert!(json.contains("<home_path>"));
    assert!(json.contains("<endpoint>"));
    assert_eq!(projected.checks[1].name, "other");
    assert_eq!(
        projected.checks[1].summary,
        "details omitted for an unrecognized doctor category"
    );
    Ok(())
}

#[test]
fn doctor_projection_fails_closed_at_check_and_field_bounds() {
    let build = build_info();
    let environment = environment();
    let redactor = SecretRedactor::empty();
    let oversized_checks = DoctorReport {
        checks: (0..=MAX_DOCTOR_SUPPORT_CHECKS)
            .map(|index| DoctorCheck {
                status: DoctorStatus::Ok,
                name: format!("config:{index}"),
                message: "ok".to_owned(),
                remediation: None,
            })
            .collect(),
    };
    assert!(
        project_doctor_support_report_v1(
            &oversized_checks,
            DoctorSupportProjectionContext {
                generated_at_unix_ms: 123,
                build: &build,
                environment: &environment,
                redactor: &redactor,
                path_redactions: &[],
            }
        )
        .is_err()
    );

    let oversized_field = DoctorReport {
        checks: vec![DoctorCheck {
            status: DoctorStatus::Ok,
            name: "config:load".to_owned(),
            message: "x".repeat(MAX_DOCTOR_SUPPORT_TEXT_BYTES + 1),
            remediation: None,
        }],
    };
    assert!(
        project_doctor_support_report_v1(
            &oversized_field,
            DoctorSupportProjectionContext {
                generated_at_unix_ms: 123,
                build: &build,
                environment: &environment,
                redactor: &redactor,
                path_redactions: &[],
            }
        )
        .is_err()
    );
}

#[test]
fn terminal_family_mapping_is_frozen_to_coarse_tokens() {
    for (program, term, expected) in [
        (Some("iTerm.app"), None, SupportTerminalFamily::Iterm2),
        (
            Some("Apple_Terminal"),
            None,
            SupportTerminalFamily::AppleTerminal,
        ),
        (Some("WezTerm"), None, SupportTerminalFamily::Wezterm),
        (Some("vscode"), None, SupportTerminalFamily::Vscode),
        (Some("private-terminal"), None, SupportTerminalFamily::Other),
        (None, None, SupportTerminalFamily::Unknown),
    ] {
        assert_eq!(terminal_family(program, term), expected);
    }
}

#[test]
fn support_bundle_schema_and_writer_are_private_bounded_and_non_overwriting() -> Result<()> {
    let temp = tempdir()?;
    let cache_root = temp.path().join("cache");
    let bundle = support_bundle()?;
    let value = serde_json::to_value(&bundle)?;
    assert_eq!(value["schema_version"], SUPPORT_BUNDLE_SCHEMA_VERSION);
    assert_eq!(value["session"]["run_phase"], "idle");
    assert_eq!(value["session"]["durable_entry_count"], 7);
    let round_trip: SupportBundleV1 = serde_json::from_value(value.clone())?;
    assert_eq!(round_trip, bundle);
    let mut with_unknown = value;
    with_unknown
        .as_object_mut()
        .expect("bundle object")
        .insert("future_field".to_owned(), json!(true));
    assert!(serde_json::from_value::<SupportBundleV1>(with_unknown).is_err());

    let first = write_support_bundle(&cache_root, &bundle)?;
    let second = write_support_bundle(&cache_root, &bundle)?;
    assert_ne!(first, second);
    assert!(first.starts_with(cache_root.canonicalize()?));
    assert!(second.exists());
    assert_eq!(
        serde_json::from_str::<SupportBundleV1>(&fs::read_to_string(&first)?)?,
        bundle
    );
    let files = fs::read_dir(first.parent().expect("bundle parent"))?
        .collect::<std::io::Result<Vec<_>>>()?;
    assert_eq!(files.len(), 2);
    assert!(
        files
            .iter()
            .all(|entry| !entry.file_name().to_string_lossy().ends_with(".tmp"))
    );

    #[cfg(unix)]
    {
        let directory_mode = fs::metadata(first.parent().expect("bundle parent"))?
            .permissions()
            .mode()
            & 0o777;
        let file_mode = fs::metadata(&first)?.permissions().mode() & 0o777;
        assert_eq!(directory_mode, 0o700);
        assert_eq!(file_mode, 0o600);
    }
    Ok(())
}

#[test]
fn support_session_projection_redacts_private_labels() -> Result<()> {
    let secret = "session-label-secret";
    let private_path = PathBuf::from("/Users/private/project");
    let redactor = SecretRedactor::from_values([secret]);
    let path_redactions = [SupportPathRedaction::new(
        &private_path,
        SupportPathKind::Workspace,
    )];
    let session = project_support_session_summary_v1(
        "session-123",
        2,
        &format!("provider-{secret}"),
        &format!("{}", private_path.display()),
        SupportRunPhase::Agent,
        true,
        SupportSessionProjectionContext {
            redactor: &redactor,
            path_redactions: &path_redactions,
        },
    )?;
    let json = serde_json::to_string(&session)?;
    assert!(!json.contains(secret));
    assert!(!json.contains("/Users/private"));
    assert!(json.contains("<workspace_path>"));
    Ok(())
}

#[test]
fn invalid_support_bundle_fails_before_creating_cache_files() -> Result<()> {
    let temp = tempdir()?;
    let cache_root = temp.path().join("missing-cache");
    let mut value = serde_json::to_value(support_bundle()?)?;
    value["session"]["provider"] = json!("x".repeat(MAX_DOCTOR_SUPPORT_NAME_BYTES + 1));
    let bundle: SupportBundleV1 = serde_json::from_value(value)?;

    assert!(write_support_bundle(&cache_root, &bundle).is_err());
    assert!(!cache_root.exists());
    Ok(())
}

#[cfg(unix)]
#[test]
fn support_bundle_writer_rejects_symlinked_destination_directory() -> Result<()> {
    let temp = tempdir()?;
    let cache_root = temp.path().join("cache");
    let external = temp.path().join("external");
    fs::create_dir_all(&cache_root)?;
    fs::create_dir_all(&external)?;
    symlink(&external, cache_root.join(SUPPORT_BUNDLES_DIRECTORY_NAME))?;

    let error = write_support_bundle(&cache_root, &support_bundle()?)
        .expect_err("symlinked support directory must be rejected");
    assert!(error.to_string().contains("symbolic link"));
    assert_eq!(fs::read_dir(&external)?.count(), 0);
    Ok(())
}
