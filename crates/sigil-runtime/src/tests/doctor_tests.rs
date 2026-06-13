use std::{
    env,
    ffi::OsString,
    fs,
    path::Path,
    sync::{Mutex, OnceLock},
};

use anyhow::Result;
use tempfile::tempdir;

use super::*;

static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

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
fn doctor_reports_valid_config_without_leaking_plaintext_secret() -> Result<()> {
    let temp = tempdir()?;
    let workspace = temp.path().to_path_buf();
    fs::write(workspace.join("mcp-server"), "#!/bin/sh\n")?;
    fs::write(workspace.join("rust-analyzer"), "#!/bin/sh\n")?;
    let config_path = workspace.join("sigil.toml");
    fs::write(
        &config_path,
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
command = "./rust-analyzer"
file_extensions = ["rs"]
root_markers = ["Cargo.toml"]

[providers.deepseek]
base_url = "https://example.com"
beta_base_url = "https://example.com/beta"
anthropic_base_url = "https://example.com/anthropic"
model = "deepseek-v4-flash"
fim_model = "deepseek-v4-pro"
api_key = "test-secret-key"

[[mcp_servers]]
name = "local"
command = "./mcp-server"
startup = "lazy"
required = false

[mcp_servers.trust]
trust_class = "self_hosted"
approval_default = "ask"
allow_secrets = false
"#,
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
    assert!(report.checks.iter().any(|check| check.name == "mcp:local"
        && check.status == DoctorStatus::Ok
        && check.message.contains("command=available")));
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
model = "deepseek-v4-flash"
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
model = "deepseek-v4-flash"
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
fn doctor_reports_missing_session_parent_and_empty_mcp_command() -> Result<()> {
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
model = "deepseek-v4-flash"
api_key = "test-secret-key"

[[mcp_servers]]
name = "empty-command"
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

    assert!(
        report
            .checks
            .iter()
            .any(|check| check.name == "session:log_dir"
                && check.status == DoctorStatus::Warn
                && check.message.contains("parent does not exist"))
    );
    assert!(
        report
            .checks
            .iter()
            .any(|check| check.name == "mcp:empty-command"
                && check.status == DoctorStatus::Error
                && check.message.contains("command=empty")
                && check.message.contains("secrets=allowed")
                && check.message.contains("pin=required"))
    );
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
    check_session_log_dir(
        &mut report,
        workspace,
        existing.to_str().expect("test path should be utf-8"),
    );
    check_session_log_dir(&mut report, workspace, "logs/new-session-dir");
    check_session_log_dir(&mut report, Path::new(""), "");

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
model = "deepseek-v4-flash"
api_key = "test-secret-key"

[[mcp_servers]]
name = "required"
command = "./missing-required-command"
startup = "eager"
required = true

[[mcp_servers]]
name = "lazy"
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
model = "deepseek-v4-flash"
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
model = "gpt-test"
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
model = "deepseek-v4-flash"
api_key = "test-secret-key"

[code_intelligence]
enabled = true

[code_intelligence.discovery]
enabled = true
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
model = "deepseek-v4-flash"
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
    let _env_lock = ENV_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("env lock should not be poisoned");

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
    let config = TerminalConfig::default();
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
fn terminal_checks_warn_when_term_is_unusable_with_enabled_config() {
    let mut report = DoctorReport::default();
    let config = TerminalConfig::default();
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
    let _env_lock = ENV_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("env lock should not be poisoned");
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
    let _env_lock = ENV_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("env lock should not be poisoned");
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
            // SAFETY: tests serialize process-wide env mutation with ENV_LOCK.
            unsafe { env::set_var(name, value) };
        }
        Self { saved }
    }

    fn remove_many(names: &[&'static str]) -> Self {
        let mut saved = Vec::with_capacity(names.len());
        for name in names {
            saved.push((*name, env::var_os(name)));
            // SAFETY: tests serialize process-wide env mutation with ENV_LOCK.
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
                    // SAFETY: tests serialize process-wide env mutation with ENV_LOCK.
                    unsafe { env::set_var(name, value) };
                }
                None => {
                    // SAFETY: tests serialize process-wide env mutation with ENV_LOCK.
                    unsafe { env::remove_var(name) };
                }
            }
        }
    }
}
