#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{env, ffi::OsString, fs, path::Path};

use anyhow::Result;
use sigil_kernel::{
    DurableEventType, EventClass, ExecutionBackendKind, ExecutionConfig, ExecutionSandboxFallback,
    ExecutionSandboxProfile, ExecutionSandboxStrategyConfig, JsonlSessionStore, ModelMessage,
    PluginTrustDecision, PluginTrustEntry, SessionLogEntry, WebSearchFailureClass,
};
use tempfile::tempdir;

use super::*;

#[test]
fn internal_web_snapshot_is_offline_unprobed_and_does_not_claim_public_activation() {
    let mut report = DoctorReport::default();
    append_web_doctor_snapshot(&mut report, &WebDoctorSnapshot::internal_only());

    assert!(report.checks.iter().any(|check| {
        check.name == "web:route"
            && check.message.contains("public_route=internal_only")
            && check.message.contains("binding=absent")
    }));
    assert!(report.checks.iter().any(|check| {
        check.name == "web:bundled"
            && check.message.contains("state=unprobed")
            && check.message.contains("network=offline")
    }));
}

#[test]
fn unavailable_configured_binding_remains_visible_without_a_bundled_fallback_claim() {
    let mut report = DoctorReport::default();
    let snapshot = WebDoctorSnapshot {
        binding: WebDoctorBindingState::Unavailable(WebSearchFailureClass::SchemaDrift),
        bundled_enabled: true,
        public_route_enabled: true,
        ..WebDoctorSnapshot::internal_only()
    };

    append_web_doctor_snapshot(&mut report, &snapshot);

    let binding = report
        .checks
        .iter()
        .find(|check| check.name == "web:binding")
        .expect("unavailable binding check");
    assert_eq!(binding.status, DoctorStatus::Warn);
    assert!(binding.message.contains("unavailable:schemadrift"));
    assert!(binding.message.contains("raw MCP tool remains separate"));
}

fn write_doctor_executable(workspace: &Path, stem: &str) -> Result<String> {
    let file_name = if cfg!(windows) {
        format!("{stem}.cmd")
    } else {
        stem.to_owned()
    };
    let path = workspace.join(&file_name);
    fs::write(
        &path,
        if cfg!(windows) {
            "@exit /B 0\r\n"
        } else {
            "#!/bin/sh\nexit 0\n"
        },
    )?;
    #[cfg(unix)]
    {
        let mut permissions = fs::metadata(&path)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions)?;
    }
    Ok(format!("./{file_name}"))
}

#[test]
fn doctor_reports_missing_config_without_panicking() {
    let temp = tempdir().expect("test workspace should be created");
    let workspace = temp.path().to_path_buf();
    let report = build_doctor_report(&workspace.join("missing.toml"), &workspace);

    assert!(report.has_errors());
    assert!(report.checks.iter().any(|check| check.name == "config:load"
        && check.status == DoctorStatus::Error
        && check.message.contains("missing config")));
    assert!(report.checks.iter().any(|check| check.name == "terminal"));
}

#[test]
fn doctor_report_overall_status_prioritizes_errors_then_warnings() {
    assert_eq!(DoctorReport::default().overall_status(), DoctorStatus::Ok);
    assert_eq!(DoctorStatus::Ok.as_str(), "ok");
    assert_eq!(DoctorStatus::Warn.as_str(), "warn");
    assert_eq!(DoctorStatus::Error.as_str(), "error");

    let warning_report = DoctorReport {
        checks: vec![DoctorCheck {
            status: DoctorStatus::Warn,
            name: "terminal".to_owned(),
            message: "TERM is not set".to_owned(),
            remediation: Some("set TERM in the shell".to_owned()),
        }],
    };
    assert!(!warning_report.has_errors());
    assert_eq!(warning_report.overall_status(), DoctorStatus::Warn);

    let error_report = DoctorReport {
        checks: vec![
            DoctorCheck {
                status: DoctorStatus::Warn,
                name: "terminal".to_owned(),
                message: "TERM is not set".to_owned(),
                remediation: Some("set TERM in the shell".to_owned()),
            },
            DoctorCheck {
                status: DoctorStatus::Error,
                name: "config:load".to_owned(),
                message: "invalid config".to_owned(),
                remediation: Some("fix sigil.toml syntax".to_owned()),
            },
        ],
    };
    assert!(error_report.has_errors());
    assert_eq!(error_report.overall_status(), DoctorStatus::Error);
}

#[test]
fn doctor_reports_invalid_config_parse_error() -> Result<()> {
    let temp = tempdir()?;
    let workspace = temp.path().to_path_buf();
    let config_path = workspace.join("sigil.toml");
    fs::write(&config_path, "[workspace\nroot = \".\"\n")?;

    let report = build_doctor_report(&config_path, &workspace);

    assert!(report.has_errors());
    assert!(report.checks.iter().any(|check| check.name == "config:load"
        && check.status == DoctorStatus::Error
        && !check.message.contains("missing config")));
    assert!(report.checks.iter().any(|check| check.name == "terminal"));
    Ok(())
}

#[test]
fn doctor_report_options_injects_appearance_checks() -> Result<()> {
    let temp = tempdir()?;
    let workspace = temp.path().to_path_buf();
    let config_path = workspace.join("sigil.toml");
    fs::write(
        &config_path,
        r#"[workspace]
root = "."

[agent]
provider = "deepseek"
model = "deepseek-v4-flash"

[providers.deepseek]
api_key = "test-secret-key"
"#,
    )?;

    let report = build_doctor_report_with_options(
        &config_path,
        &workspace,
        DoctorReportOptions {
            appearance_checks: Some(&|appearance| {
                vec![DoctorCheck {
                    status: DoctorStatus::Warn,
                    name: "appearance:test".to_owned(),
                    message: format!("theme={}", appearance.theme.as_str()),
                    remediation: Some("fix appearance".to_owned()),
                }]
            }),
            ..DoctorReportOptions::default()
        },
    );

    assert!(report.checks.iter().any(|check| {
        check.name == "appearance:test"
            && check.status == DoctorStatus::Warn
            && check.message == "theme=sigil_dark"
    }));
    Ok(())
}

#[test]
fn doctor_default_report_keeps_empty_appearance_extension() -> Result<()> {
    let temp = tempdir()?;
    let workspace = temp.path().to_path_buf();
    let config_path = workspace.join("sigil.toml");
    fs::write(
        &config_path,
        r#"[workspace]
root = "."

[agent]
provider = "deepseek"
model = "deepseek-v4-flash"

[providers.deepseek]
api_key = "test-secret-key"
"#,
    )?;

    let report = build_doctor_report(&config_path, &workspace);

    assert!(
        report
            .checks
            .iter()
            .all(|check| !check.name.starts_with("appearance:"))
    );
    Ok(())
}

#[test]
fn doctor_explains_how_to_install_the_missing_deepseek_v4_tokenizer() -> Result<()> {
    let temp = tempdir()?;
    let workspace = temp.path().to_path_buf();
    let cache_root = workspace.join("empty-cache");
    let cache_root = cache_root.to_string_lossy();
    let _env_lock = crate::test_env::lock();
    let _env_scope = EnvScope::set_many(&[(crate::SIGIL_CACHE_HOME_ENV, cache_root.as_ref())]);
    let config_path = workspace.join("sigil.toml");
    fs::write(
        &config_path,
        r#"[workspace]
root = "."

[agent]
provider = "deepseek"
model = "deepseek-v4-flash"

[providers.deepseek]
api_key = "test-secret-key"
"#,
    )?;

    let report = build_doctor_report(&config_path, &workspace);
    let tokenizer = report
        .checks
        .iter()
        .find(|check| check.name == "compaction:deepseek-v4-tokenizer")
        .expect("DeepSeek V4 Flash must receive a tokenizer readiness check");
    assert_eq!(tokenizer.status, DoctorStatus::Warn);
    assert!(tokenizer.remediation.as_deref().is_some_and(|remediation| {
        remediation.contains("sigil tokenizer install deepseek-v4-flash")
    }));
    Ok(())
}

#[test]
fn doctor_reports_valid_config_without_leaking_plaintext_secret() -> Result<()> {
    let temp = tempdir()?;
    let workspace = temp.path().to_path_buf();
    let _env_lock = crate::test_env::lock();
    let _env_scope = EnvScope::remove_many(&[SIGIL_API_KEY_ENV]);
    let mcp_command = write_doctor_executable(&workspace, "mcp-server")?;
    let rust_analyzer_command = write_doctor_executable(&workspace, "rust-analyzer")?;
    let config_path = workspace.join("sigil.toml");
    fs::write(
        &config_path,
        format!(
            r#"[workspace]
root = "."

[session]
log_dir = ".sigil/sessions"

[agent]
provider = "deepseek"
model = "deepseek-v4-flash"
tool_timeout_secs = 5

[code_intelligence]
enabled = true

[[code_intelligence.servers]]
name = "rust-analyzer"
languages = ["rust"]
command = {rust_analyzer_command:?}
file_extensions = ["rs"]
root_markers = ["Cargo.toml"]

[providers.deepseek]
base_url = "https://example.com"
beta_base_url = "https://example.com/beta"
anthropic_base_url = "https://example.com/anthropic"
fim_model = "deepseek-v4-pro"
api_key = "test-secret-key"

[[mcp_servers]]
name = "local"
transport = "stdio"
command = {mcp_command:?}
startup = "lazy"
required = false

[mcp_servers.trust]
trust_class = "self_hosted"
approval_default = "ask"
allow_secrets = false
"#
        ),
    )?;

    let report = build_doctor_report(&config_path, &workspace);
    let rendered = report
        .checks
        .iter()
        .map(|check| check.message.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(!report.has_errors(), "{report:#?}");
    assert_eq!(report.overall_status(), DoctorStatus::Warn);
    assert!(
        report
            .checks
            .iter()
            .any(|check| check.name == "execution:sandbox"
                && check.status == DoctorStatus::Ok
                && check.message.contains("backend=local")
                && check.message.contains("capabilities=none"))
    );
    assert!(!rendered.contains("test-secret-key"));
    assert!(rendered.contains("resolved from config plaintext"));
    assert!(report.checks.iter().any(|check| {
        check.name == "provider:auth"
            && check.status == DoctorStatus::Warn
            && check
                .remediation
                .as_deref()
                .is_some_and(|remediation| remediation.contains("SIGIL_API_KEY"))
    }));
    assert_eq!(
        secret_source_label(SecretSource::ConfigPlaintext),
        "config plaintext"
    );
    assert!(report.checks.iter().any(|check| {
        check.name == "mcp:local"
            && check.status == DoctorStatus::Ok
            && check.message.contains("command=available")
            && check.message.contains(
                "facets=(local=execute declared_network=unknown effective_network=runtime_preflight source_trust=self_hosted source_approval=ask)"
            )
            && check.message.contains("network_admission=run_scoped")
            && check
                .message
                .contains("boundary=local stdio outside local sandbox")
    }));
    assert!(
        report
            .checks
            .iter()
            .any(|check| check.name == "lsp:rust-analyzer"
                && check.status == DoctorStatus::Ok
                && check.message.contains("command=available"))
    );
    Ok(())
}

#[test]
fn doctor_reports_plugin_hook_runtime_summary_with_trust_and_effects() -> Result<()> {
    let temp = tempdir()?;
    let workspace = temp.path().to_path_buf();
    let plugin_root = workspace.join(".sigil/plugins/repo-review");
    fs::create_dir_all(&plugin_root)?;
    fs::write(
        plugin_root.join("plugin.toml"),
        r#"id = "repo-review"
name = "Repository Review"
version = "0.1.0"

[[hooks]]
id = "context-rules"
event = "context_rules"
kind = "context"
command = "scripts/context.sh"
declared_effect = "read_only"

[[hooks]]
id = "verify-repo"
event = "verify_repo"
kind = "verification"
command = "scripts/verify.sh"
declared_effect = "workspace_write"
"#,
    )?;
    let config_path = workspace.join("sigil.toml");
    fs::write(
        &config_path,
        r#"[workspace]
root = "."

[agent]
provider = "deepseek"
model = "deepseek-v4-flash"

[providers.deepseek]
api_key = "test-secret-key"
"#,
    )?;
    let pending = crate::discover_workspace_plugins(&workspace, &[])?;
    let trust =
        PluginTrustEntry::for_snapshot(&pending.manifests[0], PluginTrustDecision::Trusted, 42)?;

    let report = build_doctor_report_with_options(
        &config_path,
        &workspace,
        DoctorReportOptions {
            plugin_trust_entries: Some(&[trust]),
            ..DoctorReportOptions::default()
        },
    );

    let hook_check = report
        .checks
        .iter()
        .find(|check| check.name == "plugins:hooks")
        .expect("plugin hook doctor check should be present");
    assert_eq!(hook_check.status, DoctorStatus::Warn);
    assert!(hook_check.message.contains("hooks=2"));
    assert!(hook_check.message.contains("trusted=2"));
    assert!(hook_check.message.contains(
        "process_facets=local_execute:2 declared_network_unknown:2 effective_network_preflight:2"
    ));
    assert!(hook_check.message.contains("network_admission=run_scoped"));
    assert!(hook_check.message.contains("verification:1"));
    assert!(hook_check.message.contains("workspace_write:1"));
    assert_eq!(
        hook_check.remediation.as_deref(),
        Some(
            "source trust, declared effects and secret access do not authorize network; hooks require run-scoped network admission, backend isolation and mutation evidence"
        )
    );
    Ok(())
}

#[test]
fn doctor_reports_invalid_execution_sandbox_config() -> Result<()> {
    let temp = tempdir()?;
    let workspace = temp.path().to_path_buf();
    let config_path = workspace.join("sigil.toml");
    fs::write(
        &config_path,
        r#"[workspace]
root = "."

[agent]
provider = "deepseek"
model = "deepseek-v4-flash"

	[execution]
	strategy = "sandbox"

	[execution.sandbox]
	backend = "docker"
	profile = "build_offline"

[providers.deepseek]
api_key = "test-secret-key"
"#,
    )?;

    let report = build_doctor_report(&config_path, &workspace);

    assert!(report.has_errors(), "{report:#?}");
    assert!(
        report.checks.iter().any(|check| {
            check.name == "config:load"
                && check.status == DoctorStatus::Error
                && check.message.contains("failed to parse")
                && check
                    .remediation
                    .as_deref()
                    .is_some_and(|text| text.contains("sigil.toml"))
        }),
        "{report:#?}"
    );
    Ok(())
}

#[test]
fn doctor_warns_when_sandbox_falls_back_to_unconfined_local() -> Result<()> {
    let temp = tempdir()?;
    let workspace = temp.path().to_path_buf();
    let config_path = workspace.join("sigil.toml");
    fs::write(
        &config_path,
        r#"[workspace]
root = "."

[agent]
provider = "deepseek"
model = "deepseek-v4-flash"

[providers.deepseek]
api_key = "test-secret-key"
"#,
    )?;
    let mut root_config = RootConfig::load(&config_path)?;
    let mut sandbox = ExecutionSandboxStrategyConfig::new(ExecutionBackendKind::Docker);
    sandbox.profile = ExecutionSandboxProfile::WorkspaceWrite;
    sandbox.fallback = ExecutionSandboxFallback::Unconfined;
    root_config.execution = ExecutionConfig::sandbox(sandbox);
    let mut report = DoctorReport::default();
    check_execution_backend(&mut report, &root_config);

    assert!(report.checks.iter().any(|check| {
        check.name == "execution:sandbox"
            && check.status == DoctorStatus::Warn
            && check.message.contains("backend=local")
            && check
                .remediation
                .as_deref()
                .is_some_and(|text| text.contains("fallback relaxed"))
    }));
    Ok(())
}

#[test]
fn doctor_reports_workspace_file_and_missing_workspace_errors() -> Result<()> {
    let temp = tempdir()?;
    let workspace = temp.path().to_path_buf();
    let workspace_file = workspace.join("workspace-file");
    fs::write(&workspace_file, "not a directory")?;
    let file_config_path = workspace.join("file-workspace.toml");
    fs::write(
        &file_config_path,
        r#"[workspace]
root = "workspace-file"

[agent]
provider = "deepseek"
model = "deepseek-v4-flash"

[providers.deepseek]
api_key = "test-secret-key"
"#,
    )?;

    let file_report = build_doctor_report(&file_config_path, &workspace);

    assert!(
        file_report
            .checks
            .iter()
            .any(|check| check.name == "workspace"
                && check.status == DoctorStatus::Error
                && check.message.contains("not a directory"))
    );

    let missing_config_path = workspace.join("missing-workspace.toml");
    fs::write(
        &missing_config_path,
        r#"[workspace]
root = "missing-workspace"

[agent]
provider = "deepseek"
model = "deepseek-v4-flash"

[providers.deepseek]
api_key = "test-secret-key"
"#,
    )?;

    let missing_report = build_doctor_report(&missing_config_path, &workspace);

    assert!(
        missing_report
            .checks
            .iter()
            .any(|check| check.name == "workspace"
                && check.status == DoctorStatus::Error
                && check.message.contains("failed to resolve workspace root"))
    );
    Ok(())
}

#[test]
fn doctor_reports_empty_mcp_command_as_config_error() -> Result<()> {
    let temp = tempdir()?;
    let workspace = temp.path().to_path_buf();
    let config_path = workspace.join("sigil.toml");
    fs::write(
        &config_path,
        r#"[workspace]
root = "."

[session]
log_dir = "missing-parent/sessions"

[agent]
provider = "deepseek"
model = "deepseek-v4-flash"

[providers.deepseek]
api_key = "test-secret-key"

[[mcp_servers]]
name = "empty-command"
transport = "stdio"
command = "   "
startup = "lazy"
required = false

[mcp_servers.trust]
trust_class = "third_party"
approval_default = "ask"
egress_logging = true
allow_secrets = true
pin_version = true
"#,
    )?;

    let report = build_doctor_report(&config_path, &workspace);

    assert!(report.checks.iter().any(|check| check.name == "config:load"
        && check.status == DoctorStatus::Error
        && check.message.contains("failed to parse")));
    Ok(())
}

#[test]
fn session_log_dir_checks_cover_existing_creatable_absolute_and_parentless_paths() -> Result<()> {
    let temp = tempdir()?;
    let workspace = temp.path();
    let existing = workspace.join("existing-sessions");
    fs::create_dir(&existing)?;
    let creatable_parent = workspace.join("logs");
    fs::create_dir(&creatable_parent)?;

    let mut report = DoctorReport::default();
    check_session_log_dir(&mut report, &existing);
    check_session_log_dir(&mut report, &workspace.join("logs/new-session-dir"));
    check_session_log_dir(&mut report, Path::new(""));

    assert!(
        report
            .checks
            .iter()
            .any(|check| check.name == "session:log_dir"
                && check.status == DoctorStatus::Ok
                && check.message == existing.display().to_string())
    );
    assert!(
        report
            .checks
            .iter()
            .any(|check| check.name == "session:log_dir"
                && check.status == DoctorStatus::Ok
                && check.message.contains("will create"))
    );
    assert!(
        report
            .checks
            .iter()
            .any(|check| check.name == "session:log_dir"
                && check.status == DoctorStatus::Warn
                && check.message.contains("cannot determine parent"))
    );
    Ok(())
}

#[test]
fn doctor_session_stream_check_summarizes_valid_streams() -> Result<()> {
    let temp = tempdir()?;
    let session_dir = temp.path().join("sessions");
    fs::create_dir(&session_dir)?;
    let store = JsonlSessionStore::new(session_dir.join("session-1.jsonl"))?;
    store.append_event(
        DurableEventType::DiagnosticRecorded,
        EventClass::Critical,
        serde_json::json!({ "message": "ok" }),
    )?;
    store.append_event(
        DurableEventType::LogTailRecovered,
        EventClass::Critical,
        serde_json::json!({ "discarded_bytes": 2 }),
    )?;
    let mut report = DoctorReport::default();

    check_session_streams(&mut report, &session_dir);

    assert!(report.checks.iter().any(|check| {
        check.name == "session:stream"
            && check.status == DoctorStatus::Ok
            && check.message.contains("1 V2 streams checked")
            && check.message.contains("2 records")
            && check.message.contains("last_sequence=2")
            && check.message.contains("tail_recovered=1")
    }));
    Ok(())
}

#[test]
fn doctor_session_stream_check_handles_empty_non_directory_and_scan_limit() -> Result<()> {
    let temp = tempdir()?;
    let session_dir = temp.path().join("sessions");
    fs::create_dir(&session_dir)?;
    let session_file = temp.path().join("session-file");
    fs::write(&session_file, "not a directory")?;
    let mut report = DoctorReport::default();

    check_session_streams(&mut report, &temp.path().join("missing-sessions"));
    check_session_streams(&mut report, &session_file);
    assert!(report.checks.is_empty());

    check_session_streams(&mut report, &session_dir);
    assert!(report.checks.iter().any(|check| {
        check.name == "session:stream"
            && check.status == DoctorStatus::Ok
            && check.message == "no session logs yet"
    }));

    for index in 0..=super::MAX_SESSION_STREAMS_DOCTOR_SCAN {
        let store = JsonlSessionStore::new(session_dir.join(format!("session-{index:02}.jsonl")))?;
        store.append_event(
            DurableEventType::DiagnosticRecorded,
            EventClass::Critical,
            serde_json::json!({ "index": index }),
        )?;
    }
    let mut limited_report = DoctorReport::default();
    check_session_streams(&mut limited_report, &session_dir);

    assert!(limited_report.checks.iter().any(|check| {
        check.name == "session:stream"
            && check.status == DoctorStatus::Ok
            && check.message.contains("20 V2 streams checked")
            && check.message.contains("skipped 1 older streams")
    }));
    Ok(())
}

#[test]
fn doctor_session_stream_check_skips_oversized_streams() -> Result<()> {
    let temp = tempdir()?;
    let session_dir = temp.path().join("sessions");
    fs::create_dir(&session_dir)?;

    let store = JsonlSessionStore::new(session_dir.join("session-small.jsonl"))?;
    store.append_event(
        DurableEventType::DiagnosticRecorded,
        EventClass::Critical,
        serde_json::json!({ "message": "ok" }),
    )?;

    let oversized_path = session_dir.join("session-oversized.jsonl");
    let oversized_file = fs::File::create(&oversized_path)?;
    oversized_file.set_len(super::MAX_SESSION_STREAM_DOCTOR_BYTES + 1)?;

    let mut report = DoctorReport::default();
    check_session_streams(&mut report, &session_dir);

    assert!(report.checks.iter().any(|check| {
        check.name == "session:stream"
            && check.status == DoctorStatus::Warn
            && check.message.contains("1 V2 streams checked")
            && check.message.contains("skipped 1 oversized streams")
    }));
    Ok(())
}

#[test]
fn doctor_session_stream_check_reports_checksum_failure() -> Result<()> {
    let temp = tempdir()?;
    let session_dir = temp.path().join("sessions");
    fs::create_dir(&session_dir)?;
    let session_path = session_dir.join("session-1.jsonl");
    let store = JsonlSessionStore::new(&session_path)?;
    let event = store.append_event(
        DurableEventType::DiagnosticRecorded,
        EventClass::Critical,
        serde_json::json!({ "message": "ok" }),
    )?;
    let tampered =
        fs::read_to_string(&session_path)?.replace(&event.record_checksum, "sha256:jcs-v1:bad");
    fs::write(&session_path, tampered)?;
    let mut report = DoctorReport::default();

    check_session_streams(&mut report, &session_dir);

    assert!(report.checks.iter().any(|check| {
        check.name == "session:stream"
            && check.status == DoctorStatus::Error
            && check.message.contains("failed RFC-0001 stream validation")
            && check.message.contains("checksum mismatch")
            && check
                .remediation
                .as_deref()
                .is_some_and(|remediation| remediation.contains("checksum/sequence"))
    }));
    Ok(())
}

#[test]
fn doctor_session_stream_check_reports_legacy_format_without_rewriting_it() -> Result<()> {
    let temp = tempdir()?;
    let session_dir = temp.path().join("sessions");
    fs::create_dir(&session_dir)?;
    let session_path = session_dir.join("legacy.jsonl");
    let content = serde_json::to_string(&SessionLogEntry::User(ModelMessage::user("legacy")))?;
    fs::write(&session_path, &content)?;
    let mut report = DoctorReport::default();

    check_session_streams(&mut report, &session_dir);

    assert!(report.checks.iter().any(|check| {
        check.name == "session:stream"
            && check.status == DoctorStatus::Warn
            && check.message.contains("unsupported legacy session format")
            && check
                .remediation
                .as_deref()
                .is_some_and(|remediation| remediation.contains("archive the old log"))
    }));
    assert_eq!(
        report
            .checks
            .iter()
            .filter(|check| check.name == "session:stream")
            .count(),
        1
    );
    assert_eq!(fs::read_to_string(&session_path)?, content);
    Ok(())
}

#[cfg(unix)]
#[test]
fn doctor_session_stream_check_reports_unreadable_directory() -> Result<()> {
    let temp = tempdir()?;
    let session_dir = temp.path().join("sessions");
    fs::create_dir(&session_dir)?;
    let original_permissions = fs::metadata(&session_dir)?.permissions();
    fs::set_permissions(&session_dir, fs::Permissions::from_mode(0o000))?;
    let mut report = DoctorReport::default();

    check_session_streams(&mut report, &session_dir);

    fs::set_permissions(&session_dir, original_permissions)?;
    assert!(report.checks.iter().any(|check| {
        check.name == "session:stream"
            && check.status == DoctorStatus::Warn
            && check.message.contains("failed to inspect session log dir")
            && check
                .remediation
                .as_deref()
                .is_some_and(|remediation| remediation.contains("permissions"))
    }));
    Ok(())
}

#[test]
fn doctor_marks_required_eager_mcp_missing_as_error_and_lazy_missing_as_warning() -> Result<()> {
    let temp = tempdir()?;
    let workspace = temp.path().to_path_buf();
    let config_path = workspace.join("sigil.toml");
    fs::write(
        &config_path,
        r#"[workspace]
root = "."

[agent]
provider = "deepseek"
model = "deepseek-v4-flash"

[providers.deepseek]
api_key = "test-secret-key"

[[mcp_servers]]
name = "required"
transport = "stdio"
command = "./missing-required-command"
startup = "eager"
required = true

[[mcp_servers]]
name = "lazy"
transport = "stdio"
command = "./missing-lazy-command"
startup = "lazy"
required = true
"#,
    )?;

    let report = build_doctor_report(&config_path, &workspace);

    assert!(
        report
            .checks
            .iter()
            .any(|check| check.name == "mcp:required" && check.status == DoctorStatus::Error)
    );
    assert!(
        report
            .checks
            .iter()
            .any(|check| check.name == "mcp:lazy" && check.status == DoctorStatus::Warn)
    );
    Ok(())
}

#[test]
fn doctor_reports_mcp_environment_grant_names_and_missing_without_values() -> Result<()> {
    let Ok(home) = env::var("HOME") else {
        return Ok(());
    };
    let temp = tempdir()?;
    let workspace = temp.path().to_path_buf();
    let mcp_command = write_doctor_executable(&workspace, "mcp-server")?;
    let config_path = workspace.join("sigil.toml");
    fs::write(
        &config_path,
        format!(
            r#"[workspace]
root = "."

[agent]
provider = "deepseek"
model = "deepseek-v4-flash"

[providers.deepseek]
api_key = "test-secret-key"

[[mcp_servers]]
name = "ready-env"
transport = "stdio"
command = {mcp_command:?}
startup = "lazy"
required = false
inherit_env = ["HOME"]

[[mcp_servers]]
name = "missing-env"
transport = "stdio"
command = {mcp_command:?}
startup = "lazy"
required = false
inherit_env = ["SIGIL_E21_DOCTOR_MISSING_4D21"]
"#
        ),
    )?;

    let report = build_doctor_report(&config_path, &workspace);
    let ready = report
        .checks
        .iter()
        .find(|check| check.name == "mcp:ready-env")
        .expect("ready environment check should exist");
    assert!(ready.message.contains("grants=HOME"));
    assert!(ready.message.contains("missing=none"));
    assert!(ready.message.contains("live=hmac-sha256:"));
    assert!(!ready.message.contains(&home));

    let missing = report
        .checks
        .iter()
        .find(|check| check.name == "mcp:missing-env")
        .expect("missing environment check should exist");
    assert_eq!(missing.status, DoctorStatus::Error);
    assert!(missing.message.contains("configuration_invalid"));
    assert!(
        missing
            .remediation
            .as_deref()
            .is_some_and(|value| value.contains("SIGIL_E21_DOCTOR_MISSING_4D21"))
    );
    assert!(!missing.message.contains("test-secret-key"));
    Ok(())
}

#[test]
fn doctor_reports_provider_config_errors() -> Result<()> {
    let temp = tempdir()?;
    let workspace = temp.path().to_path_buf();
    let config_path = workspace.join("sigil.toml");
    fs::write(
        &config_path,
        r#"[workspace]
root = "."

[agent]
provider = "deepseek"
model = "deepseek-v4-flash"

[providers.deepseek]
base_url = 123
"#,
    )?;

    let report = build_doctor_report(&config_path, &workspace);

    assert!(
        report
            .checks
            .iter()
            .any(|check| check.name == "provider:deepseek"
                && check.status == DoctorStatus::Error
                && check.message.contains("invalid deepseek provider config"))
    );
    Ok(())
}

#[test]
fn doctor_reports_anthropic_and_gemini_provider_config_errors() -> Result<()> {
    let temp = tempdir()?;
    let workspace = temp.path().to_path_buf();
    let anthropic_config_path = workspace.join("anthropic.toml");
    fs::write(
        &anthropic_config_path,
        r#"[workspace]
root = "."

[agent]
provider = "anthropic"
model = "claude-test"
"#,
    )?;
    let anthropic_report = build_doctor_report(&anthropic_config_path, &workspace);
    assert!(
        anthropic_report.checks.iter().any(|check| {
            check.name == "provider:anthropic"
                && check.status == DoctorStatus::Error
                && check.message.contains("missing [providers.anthropic]")
                && check
                    .remediation
                    .as_deref()
                    .is_some_and(|value| value.contains("[providers.anthropic]"))
        }),
        "{anthropic_report:#?}"
    );

    let gemini_config_path = workspace.join("gemini.toml");
    fs::write(
        &gemini_config_path,
        r#"[workspace]
root = "."

[agent]
provider = "gemini"
model = "gemini-test"
"#,
    )?;
    let gemini_report = build_doctor_report(&gemini_config_path, &workspace);
    assert!(
        gemini_report.checks.iter().any(|check| {
            check.name == "provider:gemini"
                && check.status == DoctorStatus::Error
                && check.message.contains("missing [providers.gemini]")
                && check
                    .remediation
                    .as_deref()
                    .is_some_and(|value| value.contains("[providers.gemini]"))
        }),
        "{gemini_report:#?}"
    );
    Ok(())
}

#[test]
fn doctor_reports_openai_responses_provider_config_errors() -> Result<()> {
    let temp = tempdir()?;
    let workspace = temp.path().to_path_buf();
    let config_path = workspace.join("openai-responses.toml");
    fs::write(
        &config_path,
        r#"[workspace]
root = "."

[agent]
provider = "openai_responses"
model = "gpt-test"
"#,
    )?;

    let report = build_doctor_report(&config_path, &workspace);
    assert!(
        report.checks.iter().any(|check| {
            check.name == "provider:openai_responses"
                && check.status == DoctorStatus::Error
                && check
                    .message
                    .contains("missing [providers.openai_responses]")
                && check
                    .remediation
                    .as_deref()
                    .is_some_and(|value| value.contains("[providers.openai_responses]"))
        }),
        "{report:#?}"
    );
    Ok(())
}

#[test]
fn doctor_reports_openai_compat_provider_config_and_plaintext_auth() -> Result<()> {
    let temp = tempdir()?;
    let workspace = temp.path().to_path_buf();
    let config_path = workspace.join("sigil.toml");
    fs::write(
        &config_path,
        r#"[workspace]
root = "."

[agent]
provider = "openai_compat"
model = "gpt-test"

[providers.openai_compat]
base_url = "https://openai.example.com/v1"
api_key = "test-secret-key"
"#,
    )?;

    let report = build_doctor_report(&config_path, &workspace);

    assert!(
        report
            .checks
            .iter()
            .any(|check| check.name == "provider:openai_compat"
                && check.status == DoctorStatus::Ok
                && check.message.contains("model=gpt-test"))
    );
    assert!(
        report
            .checks
            .iter()
            .any(|check| check.name == "provider:auth"
                && check.status == DoctorStatus::Warn
                && check.message.contains("config plaintext"))
    );
    assert!(
        !report
            .checks
            .iter()
            .any(|check| check.message.contains("test-secret-key"))
    );
    assert!(report.checks.iter().any(|check| {
        check.name == "provider:openai_compat:capabilities"
            && check.status == DoctorStatus::Ok
            && check.message.contains("supported")
    }));
    Ok(())
}

#[test]
fn doctor_reports_anthropic_provider_config_and_capabilities() -> Result<()> {
    let temp = tempdir()?;
    let workspace = temp.path().to_path_buf();
    let config_path = workspace.join("sigil.toml");
    fs::write(
        &config_path,
        r#"[workspace]
root = "."

[agent]
provider = "anthropic"
model = "claude-test"

[providers.anthropic]
base_url = "https://anthropic.example.com"
api_key = "test-secret-key"
max_tokens = 2048
"#,
    )?;

    let report = build_doctor_report(&config_path, &workspace);

    assert!(report.checks.iter().any(|check| {
        check.name == "provider:anthropic"
            && check.status == DoctorStatus::Ok
            && check.message.contains("model=claude-test")
            && check.message.contains("max_tokens=2048")
    }));
    assert!(report.checks.iter().any(|check| {
        check.name == "provider:anthropic:capabilities"
            && check.status == DoctorStatus::Ok
            && check.message.contains("supported")
    }));
    assert!(report.checks.iter().any(|check| {
        check.name == "provider:anthropic:capability:tool_calls"
            && check.status == DoctorStatus::Ok
            && check.message.contains("Tool calls")
            && check.message.contains("supported")
    }));
    assert!(
        !report
            .checks
            .iter()
            .any(|check| check.message.contains("test-secret-key"))
    );
    Ok(())
}

#[test]
fn doctor_rejects_provider_aliases() -> Result<()> {
    let temp = tempdir()?;
    let workspace = temp.path().to_path_buf();
    let config_path = workspace.join("sigil.toml");
    fs::write(
        &config_path,
        r#"[workspace]
root = "."

[agent]
provider = "claude"
model = "claude-test"

[providers.anthropic]
base_url = "https://anthropic.example.com"
	api_key = "test-secret-key"
	"#,
    )?;

    let report = build_doctor_report(&config_path, &workspace);

    assert!(report.checks.iter().any(|check| {
        check.name == "provider"
            && check.status == DoctorStatus::Error
            && check.message.contains("unsupported provider claude")
    }));
    assert!(report.checks.iter().any(|check| {
        check.name == "provider"
            && check
                .remediation
                .as_deref()
                .is_some_and(|value| value.contains("\"anthropic\""))
    }));
    Ok(())
}

#[test]
fn doctor_reports_gemini_provider_config_and_capabilities() -> Result<()> {
    let temp = tempdir()?;
    let workspace = temp.path().to_path_buf();
    let config_path = workspace.join("sigil.toml");
    fs::write(
        &config_path,
        r#"[workspace]
root = "."

[agent]
provider = "gemini"
model = "gemini-test"

[providers.gemini]
base_url = "https://gemini.example.com/v1beta"
api_key = "test-secret-key"
"#,
    )?;

    let report = build_doctor_report(&config_path, &workspace);

    assert!(report.checks.iter().any(|check| {
        check.name == "provider:gemini"
            && check.status == DoctorStatus::Ok
            && check.message.contains("model=gemini-test")
    }));
    assert!(report.checks.iter().any(|check| {
        check.name == "provider:gemini:capabilities"
            && check.status == DoctorStatus::Ok
            && check.message.contains("supported")
    }));
    assert!(
        !report
            .checks
            .iter()
            .any(|check| check.message.contains("test-secret-key"))
    );
    Ok(())
}

#[test]
fn doctor_reports_openai_compat_provider_config_errors() -> Result<()> {
    let temp = tempdir()?;
    let workspace = temp.path().to_path_buf();
    let config_path = workspace.join("sigil.toml");
    fs::write(
        &config_path,
        r#"[workspace]
root = "."

[agent]
provider = "openai_compat"
model = "gpt-test"
"#,
    )?;

    let report = build_doctor_report(&config_path, &workspace);

    assert!(
        report
            .checks
            .iter()
            .any(|check| check.name == "provider:openai_compat"
                && check.status == DoctorStatus::Error
                && check.message.contains("missing [providers.openai_compat]"))
    );
    Ok(())
}

#[test]
fn provider_auth_check_reports_missing_api_key_remediation() {
    let mut report = DoctorReport::default();

    push_provider_auth_check(
        &mut report,
        None,
        "TEST_API_KEY",
        "[providers.test].api_key",
    );

    assert!(report.checks.iter().any(|check| {
        check.name == "provider:auth"
            && check.status == DoctorStatus::Error
            && check.message.contains("missing api key")
            && check
                .remediation
                .as_deref()
                .is_some_and(|remediation| remediation.contains("plaintext"))
    }));
}

#[test]
fn doctor_reports_code_intelligence_empty_plan_remediation() -> Result<()> {
    let temp = tempdir()?;
    let workspace = temp.path().to_path_buf();
    let config_path = workspace.join("sigil.toml");
    fs::write(
        &config_path,
        r#"[workspace]
root = "."

[agent]
provider = "deepseek"
model = "deepseek-v4-flash"

[providers.deepseek]
api_key = "test-secret-key"

	[code_intelligence]
	enabled = true
	auto_discover = true
	report_missing = false
	"#,
    )?;

    let report = build_doctor_report(&config_path, &workspace);

    assert!(report.checks.iter().any(|check| {
        check.name == "code_intelligence"
            && check.status == DoctorStatus::Warn
            && check.message.contains("no language server plan")
            && check
                .remediation
                .as_deref()
                .is_some_and(|remediation| remediation.contains("code_intelligence.servers"))
    }));
    Ok(())
}

#[test]
fn code_intelligence_checks_are_scoped_to_code_intelligence() -> Result<()> {
    let temp = tempdir()?;
    let workspace = temp.path().to_path_buf();
    let config_path = workspace.join("sigil.toml");
    fs::write(
        &config_path,
        r#"[workspace]
root = "."

[agent]
provider = "deepseek"
model = "deepseek-v4-flash"

[code_intelligence]
enabled = true

[[code_intelligence.servers]]
name = "missing-lsp"
languages = ["rust"]
command = "./missing-lsp"
file_extensions = ["rs"]
root_markers = ["Cargo.toml"]
"#,
    )?;
    let root_config = RootConfig::load(&config_path)?;

    let checks = build_code_intelligence_checks(&root_config, &workspace);

    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].name, "lsp:missing-lsp");
    assert_eq!(checks[0].status, DoctorStatus::Warn);
    assert!(checks[0].message.contains("command=missing"));
    assert!(
        checks[0]
            .remediation
            .as_deref()
            .is_some_and(|remediation| remediation.contains("missing-lsp"))
    );
    assert!(
        checks
            .iter()
            .all(|check| !check.name.starts_with("provider"))
    );
    Ok(())
}

#[test]
fn secret_source_labels_cover_environment_and_session_sources() {
    assert_eq!(
        secret_source_label(SecretSource::Environment("SIGIL_API_KEY")),
        "SIGIL_API_KEY"
    );
    assert_eq!(secret_source_label(SecretSource::Session), "session");
}

#[test]
fn doctor_reports_unsupported_provider() -> Result<()> {
    let temp = tempdir()?;
    let workspace = temp.path().to_path_buf();
    let config_path = workspace.join("sigil.toml");
    fs::write(
        &config_path,
        r#"[workspace]
root = "."

[agent]
provider = "other"
model = "other-model"
"#,
    )?;

    let report = build_doctor_report(&config_path, &workspace);

    assert!(report.checks.iter().any(|check| check.name == "provider"
        && check.status == DoctorStatus::Error
        && check.message.contains("unsupported provider other")));
    Ok(())
}

#[test]
fn doctor_reports_lsp_warnings_for_missing_and_empty_commands() -> Result<()> {
    let temp = tempdir()?;
    let workspace = temp.path().to_path_buf();
    let config_path = workspace.join("sigil.toml");
    fs::write(
        &config_path,
        r#"[workspace]
root = "."

[agent]
provider = "deepseek"
model = "deepseek-v4-flash"

[providers.deepseek]
api_key = "test-secret-key"

[code_intelligence]
enabled = true

[[code_intelligence.servers]]
name = "missing-lsp"
languages = ["rust"]
command = "./missing-lsp"
file_extensions = ["rs"]
root_markers = ["Cargo.toml"]

[[code_intelligence.servers]]
name = "empty-command-lsp"
languages = ["python"]
command = ""
file_extensions = ["py"]
root_markers = ["pyproject.toml"]
"#,
    )?;

    let report = build_doctor_report(&config_path, &workspace);

    assert!(
        report
            .checks
            .iter()
            .any(|check| check.name == "lsp:missing-lsp"
                && check.status == DoctorStatus::Warn
                && check.message.contains("command=missing"))
    );
    assert!(
        report
            .checks
            .iter()
            .any(|check| check.name == "lsp:empty-command-lsp"
                && check.status == DoctorStatus::Warn
                && check.message.contains("command=empty"))
    );
    Ok(())
}

#[test]
fn doctor_terminal_check_reports_dumb_and_missing_term() {
    let _env_lock = crate::test_env::lock();

    {
        let _env_scope = EnvScope::set_many(&[("TERM", "dumb")]);
        let mut report = DoctorReport::default();
        check_terminal(&mut report, None);
        assert_eq!(report.overall_status(), DoctorStatus::Warn);
        assert!(report.checks.iter().any(|check| check.name == "terminal"
            && check.status == DoctorStatus::Warn
            && check.message.contains("TERM=dumb")));
    }

    {
        let _env_scope = EnvScope::remove_many(&["TERM"]);
        let mut report = DoctorReport::default();
        check_terminal(&mut report, None);
        assert_eq!(report.overall_status(), DoctorStatus::Warn);
        assert!(report.checks.iter().any(|check| check.name == "terminal"
            && check.status == DoctorStatus::Warn
            && check.message == "TERM is not set"));
    }
}

#[test]
fn terminal_checks_report_disabled_config_and_smoke_checklist() {
    let mut report = DoctorReport::default();
    let config = TerminalConfig {
        mouse_capture: false,
        osc52_clipboard: false,
        ..TerminalConfig::default()
    };
    let environment = TerminalEnvironment {
        term: Some("xterm-256color".to_owned()),
        term_program: Some("Apple_Terminal".to_owned()),
        ..TerminalEnvironment::default()
    };

    check_terminal_with_env(&mut report, Some(&config), &environment);

    assert_eq!(report.overall_status(), DoctorStatus::Ok);
    assert!(report.checks.iter().any(|check| {
        check.name == "terminal:config"
            && check.message == "mouse_capture=false osc52_clipboard=false scroll_sensitivity=3"
    }));
    assert!(report.checks.iter().any(|check| {
        check.name == "terminal:mouse"
            && check.status == DoctorStatus::Ok
            && check.message.contains("disabled by config")
    }));
    assert!(report.checks.iter().any(|check| {
        check.name == "terminal:clipboard"
            && check.status == DoctorStatus::Ok
            && check.message.contains("disabled by config")
    }));
    assert!(
        report
            .checks
            .iter()
            .any(|check| check.name == "terminal:smoke"
                && check.message.contains("terminal-compatibility.md"))
    );
}

#[test]
fn terminal_checks_warn_for_multiplexer_and_remote_clipboard_bridges() {
    let mut report = DoctorReport::default();
    let config = TerminalConfig {
        mouse_capture: true,
        ..TerminalConfig::default()
    };
    let environment = TerminalEnvironment {
        term: Some("screen-256color".to_owned()),
        tmux: true,
        ssh: true,
        ..TerminalEnvironment::default()
    };

    check_terminal_with_env(&mut report, Some(&config), &environment);

    assert_eq!(report.overall_status(), DoctorStatus::Warn);
    assert!(
        report
            .checks
            .iter()
            .any(|check| check.name == "terminal:profile"
                && check.message.contains("layer=tmux")
                && check.message.contains("layer=ssh"))
    );
    assert!(report.checks.iter().any(|check| {
        check.name == "terminal:mouse"
            && check.status == DoctorStatus::Warn
            && check.message.contains("tmux")
            && check
                .remediation
                .as_deref()
                .is_some_and(|remediation| remediation.contains("mouse_capture"))
    }));
    assert!(report.checks.iter().any(|check| {
        check.name == "terminal:clipboard"
            && check.status == DoctorStatus::Warn
            && check.message.contains("tmux+ssh")
            && check
                .remediation
                .as_deref()
                .is_some_and(|remediation| remediation.contains("osc52_clipboard"))
    }));
}

#[test]
fn terminal_checks_warn_when_iterm_profile_disables_mouse_reporting() {
    let mut report = DoctorReport::default();
    let config = TerminalConfig {
        mouse_capture: true,
        ..TerminalConfig::default()
    };
    let environment = TerminalEnvironment {
        term: Some("xterm-256color".to_owned()),
        term_program: Some("iTerm.app".to_owned()),
        term_program_version: Some("3.6.10".to_owned()),
        iterm_profile: Some("Default".to_owned()),
        iterm_mouse_reporting: Some(false),
        ..TerminalEnvironment::default()
    };

    check_terminal_with_env(&mut report, Some(&config), &environment);

    assert_eq!(report.overall_status(), DoctorStatus::Warn);
    assert!(report.checks.iter().any(|check| {
        check.name == "terminal:profile"
            && check.message.contains("ITERM_PROFILE=Default")
            && check.message.contains("iterm_mouse_reporting=false")
    }));
    assert!(report.checks.iter().any(|check| {
        check.name == "terminal:mouse"
            && check.status == DoctorStatus::Warn
            && check
                .message
                .contains("iTerm profile Default disables Mouse Reporting")
            && check
                .remediation
                .as_deref()
                .is_some_and(|remediation| remediation.contains("Mouse Reporting"))
    }));
}

#[test]
fn iterm_bookmarks_parser_reads_mouse_reporting_from_single_dump() {
    let bookmarks = r#"
Array {
    Dict {
        Name = Default
        Mouse Reporting = true
        Nested = Dict {
            Name = Ignored
            Mouse Reporting = false
        }
    }
    Dict {
        Name = Work Profile
        Mouse Reporting = false
    }
    Dict {
        "Name" = Quoted Profile
        "Mouse Reporting" = true
    }
}
"#;

    assert_eq!(
        iterm_mouse_reporting_from_bookmarks(bookmarks, "Default"),
        Some(true)
    );
    assert_eq!(
        iterm_mouse_reporting_from_bookmarks(bookmarks, "Work Profile"),
        Some(false)
    );
    assert_eq!(
        iterm_mouse_reporting_from_bookmarks(bookmarks, "Quoted Profile"),
        Some(true)
    );
    assert_eq!(
        iterm_mouse_reporting_from_bookmarks(bookmarks, "Missing"),
        None
    );
}

#[test]
fn terminal_checks_warn_when_term_is_unusable_with_enabled_config() {
    let mut report = DoctorReport::default();
    let config = TerminalConfig {
        mouse_capture: true,
        ..TerminalConfig::default()
    };
    let environment = TerminalEnvironment {
        term: Some("dumb".to_owned()),
        ..TerminalEnvironment::default()
    };

    check_terminal_with_env(&mut report, Some(&config), &environment);

    assert_eq!(report.overall_status(), DoctorStatus::Warn);
    assert!(report.checks.iter().any(|check| {
        check.name == "terminal:mouse"
            && check.status == DoctorStatus::Warn
            && check.message.contains("TERM is missing or dumb")
            && check
                .remediation
                .as_deref()
                .is_some_and(|remediation| remediation.contains("mouse_capture"))
    }));
    assert!(report.checks.iter().any(|check| {
        check.name == "terminal:clipboard"
            && check.status == DoctorStatus::Warn
            && check.message.contains("TERM is missing or dumb")
            && check
                .remediation
                .as_deref()
                .is_some_and(|remediation| remediation.contains("osc52_clipboard"))
    }));
}

#[test]
fn terminal_environment_summary_covers_known_profiles_and_layers() {
    let environment = TerminalEnvironment {
        term: Some("xterm-kitty".to_owned()),
        term_program: Some("WezTerm".to_owned()),
        term_program_version: Some("20260201".to_owned()),
        kitty: true,
        wezterm: true,
        windows_terminal: true,
        screen: true,
        wsl: true,
        ..TerminalEnvironment::default()
    };

    let summary = environment.profile_summary();

    assert!(summary.contains("TERM_PROGRAM=WezTerm"));
    assert!(summary.contains("TERM_PROGRAM_VERSION=20260201"));
    assert!(summary.contains("profile=wezterm"));
    assert!(summary.contains("profile=kitty"));
    assert!(summary.contains("profile=windows_terminal"));
    assert!(summary.contains("layer=screen"));
    assert!(summary.contains("layer=wsl"));
    assert!(!environment.term_is_missing_or_dumb());
    assert!(TerminalEnvironment::default().term_is_missing_or_dumb());
    assert_eq!(
        TerminalEnvironment::default().profile_summary(),
        "profile=unknown"
    );
    assert_eq!(
        TerminalEnvironment::default().clipboard_bridge_label(),
        "terminal bridge"
    );
    assert_eq!(
        TerminalEnvironment {
            screen: true,
            ..TerminalEnvironment::default()
        }
        .multiplexer_label(),
        "screen"
    );
    assert_eq!(
        TerminalEnvironment {
            screen: true,
            wsl: true,
            ..TerminalEnvironment::default()
        }
        .clipboard_bridge_label(),
        "screen+wsl"
    );
}

#[test]
fn command_status_checks_path_and_relative_commands() -> Result<()> {
    let _env_lock = crate::test_env::lock();
    let _env_scope = EnvScope::remove_many(&["PATH"]);
    let temp = tempdir()?;
    let workspace = temp.path();
    let relative_command = workspace.join("bin").join("local-tool");
    fs::create_dir_all(
        relative_command
            .parent()
            .expect("command should have parent"),
    )?;
    fs::write(&relative_command, "#!/bin/sh\n")?;
    let absolute_command = workspace.join("absolute-tool");
    fs::write(&absolute_command, "#!/bin/sh\n")?;

    assert_eq!(command_status("", workspace), CommandStatus::Empty);
    assert_eq!(
        command_status(
            absolute_command
                .to_str()
                .expect("test path should be representable as utf-8"),
            workspace
        ),
        CommandStatus::Available
    );
    assert_eq!(
        command_status(
            workspace
                .join("missing-absolute-tool")
                .to_str()
                .expect("test path should be representable as utf-8"),
            workspace
        ),
        CommandStatus::Missing
    );
    assert_eq!(
        command_status("./bin/local-tool", workspace),
        CommandStatus::Available
    );
    assert_eq!(
        command_status("./bin/missing-tool", workspace),
        CommandStatus::Missing
    );
    assert_eq!(
        command_status("pathless-tool", workspace),
        CommandStatus::Missing
    );
    Ok(())
}

#[test]
fn command_status_finds_pathless_commands_on_path() -> Result<()> {
    let _env_lock = crate::test_env::lock();
    let temp = tempdir()?;
    let workspace = temp.path();
    let bin_dir = workspace.join("bin");
    fs::create_dir(&bin_dir)?;
    fs::write(bin_dir.join("path-tool"), "#!/bin/sh\n")?;
    let _env_scope = EnvScope::set_many(&[(
        "PATH",
        bin_dir
            .to_str()
            .expect("test path should be representable as utf-8"),
    )]);

    assert_eq!(
        command_status("path-tool", workspace),
        CommandStatus::Available
    );
    assert_eq!(
        command_status("missing-path-tool", workspace),
        CommandStatus::Missing
    );
    Ok(())
}

struct EnvScope {
    saved: Vec<(&'static str, Option<OsString>)>,
}

impl EnvScope {
    fn set_many(values: &[(&'static str, &str)]) -> Self {
        let mut saved = Vec::with_capacity(values.len());
        for (name, value) in values {
            saved.push((*name, env::var_os(name)));
            // SAFETY: tests serialize process-wide env mutation with crate::test_env.
            unsafe { env::set_var(name, value) };
        }
        Self { saved }
    }

    fn remove_many(names: &[&'static str]) -> Self {
        let mut saved = Vec::with_capacity(names.len());
        for name in names {
            saved.push((*name, env::var_os(name)));
            // SAFETY: tests serialize process-wide env mutation with crate::test_env.
            unsafe { env::remove_var(name) };
        }
        Self { saved }
    }
}

impl Drop for EnvScope {
    fn drop(&mut self) {
        for (name, value) in self.saved.drain(..).rev() {
            match value {
                Some(value) => {
                    // SAFETY: tests serialize process-wide env mutation with crate::test_env.
                    unsafe { env::set_var(name, value) };
                }
                None => {
                    // SAFETY: tests serialize process-wide env mutation with crate::test_env.
                    unsafe { env::remove_var(name) };
                }
            }
        }
    }
}
