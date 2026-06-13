use std::{
    env, fs,
    path::{Path, PathBuf},
};

use sigil_kernel::{McpServerConfig, McpServerStartup, RootConfig, resolve_workspace_root};

use crate::{SecretSource, load_deepseek_config, resolve_deepseek_api_key};

/// Severity for one local diagnostics check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoctorStatus {
    Ok,
    Warn,
    Error,
}

impl DoctorStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

/// One line item in a Sigil local diagnostics report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorCheck {
    pub status: DoctorStatus,
    pub name: String,
    pub message: String,
}

/// Aggregated local diagnostics for config, provider, tools, and terminal readiness.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DoctorReport {
    pub checks: Vec<DoctorCheck>,
}

impl DoctorReport {
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.checks
            .iter()
            .any(|check| check.status == DoctorStatus::Error)
    }

    #[must_use]
    pub fn overall_status(&self) -> DoctorStatus {
        if self.has_errors() {
            return DoctorStatus::Error;
        }
        if self
            .checks
            .iter()
            .any(|check| check.status == DoctorStatus::Warn)
        {
            return DoctorStatus::Warn;
        }
        DoctorStatus::Ok
    }

    fn push(&mut self, status: DoctorStatus, name: impl Into<String>, message: impl Into<String>) {
        self.checks.push(DoctorCheck {
            status,
            name: name.into(),
            message: message.into(),
        });
    }
}

/// Builds a local diagnostics report without starting providers or MCP servers.
#[must_use]
pub fn build_doctor_report(config_path: &Path, launch_cwd: &Path) -> DoctorReport {
    let mut report = DoctorReport::default();
    report.push(
        DoctorStatus::Ok,
        "config:path",
        config_path.display().to_string(),
    );

    if !config_path.exists() {
        report.push(
            DoctorStatus::Error,
            "config:load",
            format!("missing config at {}", config_path.display()),
        );
        check_terminal(&mut report);
        return report;
    }

    let root_config = match RootConfig::load(config_path) {
        Ok(config) => {
            report.push(DoctorStatus::Ok, "config:load", "config parsed");
            config
        }
        Err(error) => {
            report.push(DoctorStatus::Error, "config:load", error.to_string());
            check_terminal(&mut report);
            return report;
        }
    };

    let workspace_root =
        resolve_workspace_root(config_path, launch_cwd, &root_config.workspace.root);
    let canonical_workspace = check_workspace(&mut report, &workspace_root);
    check_session_log_dir(&mut report, &workspace_root, &root_config.session.log_dir);
    check_provider(&mut report, &root_config);
    check_mcp_servers(&mut report, &root_config.mcp_servers, &workspace_root);
    check_code_intelligence(
        &mut report,
        &root_config,
        canonical_workspace.as_deref().unwrap_or(&workspace_root),
    );
    check_terminal(&mut report);
    report
}

fn check_workspace(report: &mut DoctorReport, workspace_root: &Path) -> Option<PathBuf> {
    match fs::canonicalize(workspace_root) {
        Ok(canonical) if canonical.is_dir() => {
            report.push(
                DoctorStatus::Ok,
                "workspace",
                canonical.display().to_string(),
            );
            Some(canonical)
        }
        Ok(canonical) => {
            report.push(
                DoctorStatus::Error,
                "workspace",
                format!("workspace root is not a directory: {}", canonical.display()),
            );
            None
        }
        Err(error) => {
            report.push(
                DoctorStatus::Error,
                "workspace",
                format!(
                    "failed to resolve workspace root {}: {error}",
                    workspace_root.display()
                ),
            );
            None
        }
    }
}

fn check_session_log_dir(
    report: &mut DoctorReport,
    workspace_root: &Path,
    configured_log_dir: &str,
) {
    let configured = Path::new(configured_log_dir);
    let session_dir = if configured.is_absolute() {
        configured.to_path_buf()
    } else {
        workspace_root.join(configured)
    };
    if session_dir.is_dir() {
        report.push(
            DoctorStatus::Ok,
            "session:log_dir",
            session_dir.display().to_string(),
        );
        return;
    }
    let Some(parent) = session_dir.parent() else {
        report.push(
            DoctorStatus::Warn,
            "session:log_dir",
            format!("cannot determine parent for {}", session_dir.display()),
        );
        return;
    };
    if parent.exists() {
        report.push(
            DoctorStatus::Ok,
            "session:log_dir",
            format!("will create {}", session_dir.display()),
        );
    } else {
        report.push(
            DoctorStatus::Warn,
            "session:log_dir",
            format!("parent does not exist for {}", session_dir.display()),
        );
    }
}

fn check_provider(report: &mut DoctorReport, root_config: &RootConfig) {
    match root_config.agent.provider.as_str() {
        "deepseek" => check_deepseek_provider(report, root_config),
        other => report.push(
            DoctorStatus::Error,
            "provider",
            format!("unsupported provider {other}"),
        ),
    }
}

fn check_deepseek_provider(report: &mut DoctorReport, root_config: &RootConfig) {
    let config = match load_deepseek_config(root_config).and_then(|config| config.resolved()) {
        Ok(config) => config,
        Err(error) => {
            report.push(DoctorStatus::Error, "provider:deepseek", error.to_string());
            return;
        }
    };
    report.push(
        DoctorStatus::Ok,
        "provider:deepseek",
        format!("model={} base_url={}", config.model, config.base_url),
    );

    match resolve_deepseek_api_key(&config) {
        Some(secret) => report.push(
            DoctorStatus::Ok,
            "provider:auth",
            format!("resolved from {}", secret_source_label(secret.source)),
        ),
        None => report.push(
            DoctorStatus::Error,
            "provider:auth",
            "missing api key; set SIGIL_API_KEY or [providers.deepseek].api_key",
        ),
    }
}

fn secret_source_label(source: SecretSource) -> &'static str {
    match source {
        SecretSource::Environment(name) => name,
        SecretSource::ConfigPlaintext => "config plaintext",
        SecretSource::Session => "session",
    }
}

fn check_mcp_servers(
    report: &mut DoctorReport,
    servers: &[McpServerConfig],
    workspace_root: &Path,
) {
    if servers.is_empty() {
        report.push(DoctorStatus::Ok, "mcp", "no servers configured");
        return;
    }

    for server in servers {
        let command_status = command_status(&server.command, workspace_root);
        let status = match command_status {
            CommandStatus::Available => DoctorStatus::Ok,
            CommandStatus::Empty => DoctorStatus::Error,
            CommandStatus::Missing
                if server.required && server.startup == McpServerStartup::Eager =>
            {
                DoctorStatus::Error
            }
            CommandStatus::Missing => DoctorStatus::Warn,
        };
        report.push(
            status,
            format!("mcp:{}", server.name),
            format!(
                "{} required={} command={} trust={} approval={} secrets={} pin={}",
                server.startup.as_str(),
                server.required,
                command_status.as_str(),
                server.trust.trust_class.as_str(),
                server.trust.approval_default.as_str(),
                if server.trust.allow_secrets {
                    "allowed"
                } else {
                    "blocked"
                },
                if server.trust.pin_version {
                    "required"
                } else {
                    "off"
                },
            ),
        );
    }
}

fn check_code_intelligence(
    report: &mut DoctorReport,
    root_config: &RootConfig,
    workspace_root: &Path,
) {
    if !sigil_code_intel::workspace::config_enabled(&root_config.code_intelligence) {
        report.push(DoctorStatus::Ok, "code_intelligence", "disabled");
        return;
    }

    let plan = sigil_code_intel::workspace::effective_server_plan(
        &root_config.code_intelligence,
        workspace_root,
    );
    if plan.statuses.is_empty() {
        report.push(
            DoctorStatus::Warn,
            "code_intelligence",
            "enabled but no language server plan was produced",
        );
        return;
    }

    for status in &plan.statuses {
        let server = plan
            .servers
            .iter()
            .find(|server| server.name == status.server);
        let command_status = server
            .map(|server| command_status(&server.command, workspace_root))
            .unwrap_or(CommandStatus::Missing);
        let status_level = match status.status.as_str() {
            "installed" | "configured" if command_status == CommandStatus::Available => {
                DoctorStatus::Ok
            }
            "installed" | "configured" => DoctorStatus::Warn,
            "missing" | "disabled" => DoctorStatus::Warn,
            value if value.starts_with("degraded") => DoctorStatus::Warn,
            _ => DoctorStatus::Warn,
        };
        report.push(
            status_level,
            format!("lsp:{}", status.server),
            format!(
                "{} languages={} command={}",
                status.status,
                status.languages.join(","),
                command_status.as_str()
            ),
        );
    }
}

fn check_terminal(report: &mut DoctorReport) {
    match env::var("TERM")
        .ok()
        .filter(|value| !value.trim().is_empty())
    {
        Some(term) if term == "dumb" => report.push(
            DoctorStatus::Warn,
            "terminal",
            "TERM=dumb; TUI rendering may be limited",
        ),
        Some(term) => report.push(DoctorStatus::Ok, "terminal", format!("TERM={term}")),
        None => report.push(DoctorStatus::Warn, "terminal", "TERM is not set"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandStatus {
    Available,
    Missing,
    Empty,
}

impl CommandStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Missing => "missing",
            Self::Empty => "empty",
        }
    }
}

fn command_status(command: &str, base_dir: &Path) -> CommandStatus {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return CommandStatus::Empty;
    }
    let command_path = Path::new(trimmed);
    if command_path.is_absolute() || command_path.components().count() > 1 {
        let candidate = if command_path.is_absolute() {
            command_path.to_path_buf()
        } else {
            base_dir.join(command_path)
        };
        return if candidate.exists() {
            CommandStatus::Available
        } else {
            CommandStatus::Missing
        };
    }
    let Some(paths) = env::var_os("PATH") else {
        return CommandStatus::Missing;
    };
    if env::split_paths(&paths).any(|path| path.join(trimmed).exists()) {
        CommandStatus::Available
    } else {
        CommandStatus::Missing
    }
}

#[cfg(test)]
#[path = "tests/doctor_tests.rs"]
mod tests;
