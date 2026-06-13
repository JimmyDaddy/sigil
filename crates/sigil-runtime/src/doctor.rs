use std::{
    env, fs,
    path::{Path, PathBuf},
};

use sigil_kernel::{McpServerConfig, McpServerStartup, RootConfig, resolve_workspace_root};
use sigil_provider_deepseek::SIGIL_API_KEY_ENV;
use sigil_provider_openai_compat::OPENAI_COMPATIBLE_API_KEY_ENV;

use crate::{
    SecretResolution, SecretSource, load_deepseek_config, load_openai_compat_config,
    resolve_deepseek_api_key, resolve_openai_compat_api_key,
};

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
    pub remediation: Option<String>,
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
        self.push_with_remediation(status, name, message, None::<String>);
    }

    fn push_with_remediation(
        &mut self,
        status: DoctorStatus,
        name: impl Into<String>,
        message: impl Into<String>,
        remediation: Option<impl Into<String>>,
    ) {
        self.checks.push(DoctorCheck {
            status,
            name: name.into(),
            message: message.into(),
            remediation: remediation.map(Into::into),
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
        report.push_with_remediation(
            DoctorStatus::Error,
            "config:load",
            format!("missing config at {}", config_path.display()),
            Some("start `sigil-tui` to complete Quick Setup, or create sigil.toml at this path"),
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
            report.push_with_remediation(
                DoctorStatus::Error,
                "config:load",
                error.to_string(),
                Some("fix sigil.toml syntax, or rerun Quick Setup to regenerate the config"),
            );
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

/// Builds only the code-intelligence diagnostics without starting language servers.
#[must_use]
pub fn build_code_intelligence_checks(
    root_config: &RootConfig,
    workspace_root: &Path,
) -> Vec<DoctorCheck> {
    let mut report = DoctorReport::default();
    check_code_intelligence(&mut report, root_config, workspace_root);
    report.checks
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
            report.push_with_remediation(
                DoctorStatus::Error,
                "workspace",
                format!("workspace root is not a directory: {}", canonical.display()),
                Some(
                    "set [workspace].root to an existing directory, or launch Sigil from the intended workspace",
                ),
            );
            None
        }
        Err(error) => {
            report.push_with_remediation(
                DoctorStatus::Error,
                "workspace",
                format!(
                    "failed to resolve workspace root {}: {error}",
                    workspace_root.display()
                ),
                Some(
                    "create the workspace directory, update [workspace].root, or launch Sigil from the intended repository",
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
        report.push_with_remediation(
            DoctorStatus::Warn,
            "session:log_dir",
            format!("cannot determine parent for {}", session_dir.display()),
            Some("set [session].log_dir to a normal directory path"),
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
        report.push_with_remediation(
            DoctorStatus::Warn,
            "session:log_dir",
            format!("parent does not exist for {}", session_dir.display()),
            Some("create the parent directory, or set [session].log_dir under the workspace"),
        );
    }
}

fn check_provider(report: &mut DoctorReport, root_config: &RootConfig) {
    match root_config.agent.provider.as_str() {
        "deepseek" => check_deepseek_provider(report, root_config),
        "openai_compat" | "openai-compatible" | "openai_compatible" => {
            check_openai_compat_provider(report, root_config);
        }
        other => report.push_with_remediation(
            DoctorStatus::Error,
            "provider",
            format!("unsupported provider {other}"),
            Some("set [agent].provider to \"deepseek\" or \"openai_compat\""),
        ),
    }
}

fn check_deepseek_provider(report: &mut DoctorReport, root_config: &RootConfig) {
    let config = match load_deepseek_config(root_config).and_then(|config| config.resolved()) {
        Ok(config) => config,
        Err(error) => {
            report.push_with_remediation(
                DoctorStatus::Error,
                "provider:deepseek",
                error.to_string(),
                Some("add a valid [providers.deepseek] block, or rerun Quick Setup"),
            );
            return;
        }
    };
    report.push(
        DoctorStatus::Ok,
        "provider:deepseek",
        format!("model={} base_url={}", config.model, config.base_url),
    );

    push_provider_auth_check(
        report,
        resolve_deepseek_api_key(&config),
        SIGIL_API_KEY_ENV,
        "[providers.deepseek].api_key",
    );
}

fn check_openai_compat_provider(report: &mut DoctorReport, root_config: &RootConfig) {
    let config = match load_openai_compat_config(root_config).and_then(|config| config.resolved()) {
        Ok(config) => config,
        Err(error) => {
            report.push_with_remediation(
                DoctorStatus::Error,
                "provider:openai_compat",
                error.to_string(),
                Some("add a valid [providers.openai_compat] block"),
            );
            return;
        }
    };
    report.push(
        DoctorStatus::Ok,
        "provider:openai_compat",
        format!("model={} base_url={}", config.model, config.base_url),
    );

    push_provider_auth_check(
        report,
        resolve_openai_compat_api_key(&config),
        OPENAI_COMPATIBLE_API_KEY_ENV,
        "[providers.openai_compat].api_key",
    );
}

fn push_provider_auth_check(
    report: &mut DoctorReport,
    secret: Option<SecretResolution>,
    preferred_env: &'static str,
    config_key: &'static str,
) {
    match secret {
        Some(secret) if secret.source == SecretSource::ConfigPlaintext => report.push_with_remediation(
            DoctorStatus::Warn,
            "provider:auth",
            "resolved from config plaintext",
            Some(format!(
                "prefer {preferred_env} for temporary use; if api_key stays in sigil.toml, keep the file private and never commit it",
            )),
        ),
        Some(secret) => report.push(
            DoctorStatus::Ok,
            "provider:auth",
            format!("resolved from {}", secret_source_label(secret.source)),
        ),
        None => report.push_with_remediation(
            DoctorStatus::Error,
            "provider:auth",
            format!(
                "missing api key; set {preferred_env} or {config_key}",
            ),
            Some(format!(
                "for temporary use, export {preferred_env}; if you save api_key in sigil.toml, it is plaintext",
            )),
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
        let remediation = mcp_remediation(server, command_status);
        report.push_with_remediation(
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
            remediation,
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
        report.push_with_remediation(
            DoctorStatus::Warn,
            "code_intelligence",
            "enabled but no language server plan was produced",
            Some("add code_intelligence.servers entries, or disable [code_intelligence].enabled"),
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
        let remediation = lsp_remediation(status.status.as_str(), command_status, &status.server);
        report.push_with_remediation(
            status_level,
            format!("lsp:{}", status.server),
            format!(
                "{} languages={} command={}",
                status.status,
                status.languages.join(","),
                command_status.as_str()
            ),
            remediation,
        );
    }
}

fn check_terminal(report: &mut DoctorReport) {
    match env::var("TERM")
        .ok()
        .filter(|value| !value.trim().is_empty())
    {
        Some(term) if term == "dumb" => report.push_with_remediation(
            DoctorStatus::Warn,
            "terminal",
            "TERM=dumb; TUI rendering may be limited",
            Some("launch Sigil from a terminal that sets TERM, such as xterm-256color"),
        ),
        Some(term) => report.push(DoctorStatus::Ok, "terminal", format!("TERM={term}")),
        None => report.push_with_remediation(
            DoctorStatus::Warn,
            "terminal",
            "TERM is not set",
            Some("set TERM in the shell before launching the TUI"),
        ),
    }
}

fn mcp_remediation(
    server: &McpServerConfig,
    command_status: CommandStatus,
) -> Option<&'static str> {
    match command_status {
        CommandStatus::Empty => {
            Some("set command to the stdio server executable, or remove this MCP server")
        }
        CommandStatus::Missing if server.required && server.startup == McpServerStartup::Eager => {
            Some(
                "install the command, use a valid absolute or workspace-relative path, switch startup to lazy, or set required = false",
            )
        }
        CommandStatus::Missing => Some(
            "install the command, use a valid path, or remove this MCP server until it is available",
        ),
        CommandStatus::Available => None,
    }
}

fn lsp_remediation(
    status: &str,
    command_status: CommandStatus,
    server_name: &str,
) -> Option<String> {
    if command_status == CommandStatus::Available && matches!(status, "installed" | "configured") {
        return None;
    }
    Some(format!(
        "install or configure the {server_name} language server command, or disable code intelligence for this workspace",
    ))
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
