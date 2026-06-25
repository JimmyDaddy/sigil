use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use sigil_kernel::{
    AppearanceConfig, McpServerConfig, McpServerStartup, RootConfig, config::TerminalConfig,
    resolve_workspace_root,
};
use sigil_provider_anthropic::SIGIL_ANTHROPIC_API_KEY_ENV;
use sigil_provider_deepseek::SIGIL_API_KEY_ENV;
use sigil_provider_gemini::SIGIL_GEMINI_API_KEY_ENV;
use sigil_provider_openai_compat::OPENAI_COMPATIBLE_API_KEY_ENV;

use crate::{
    SecretResolution, SecretSource, load_anthropic_config, load_deepseek_config,
    load_gemini_config, load_openai_compat_config, provider_capabilities_for_name,
    provider_capability_view, provider_config_key, resolve_anthropic_api_key,
    resolve_deepseek_api_key, resolve_gemini_api_key, resolve_openai_compat_api_key,
    resolve_sigil_paths,
};

const WORKSPACE_CONFIG_FILE: &str = "sigil.toml";
const LEGACY_WORKSPACE_STATE_DIR: &str = ".sigil";
const LEGACY_SESSIONS_DIR: &str = "sessions";
const LEGACY_INPUT_HISTORY_FILE: &str = "input-history.jsonl";

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

/// Entrypoint-supplied appearance diagnostics hook.
pub type AppearanceDoctorChecks = dyn Fn(&AppearanceConfig) -> Vec<DoctorCheck>;

/// Optional diagnostics supplied by higher-level entrypoints.
#[derive(Clone, Copy, Default)]
pub struct DoctorReportOptions<'a> {
    pub appearance_checks: Option<&'a AppearanceDoctorChecks>,
}

/// Builds a local diagnostics report without starting providers or MCP servers.
#[must_use]
pub fn build_doctor_report(config_path: &Path, launch_cwd: &Path) -> DoctorReport {
    build_doctor_report_with_options(config_path, launch_cwd, DoctorReportOptions::default())
}

/// Builds a local diagnostics report with entrypoint-specific extension checks.
#[must_use]
pub fn build_doctor_report_with_options(
    config_path: &Path,
    launch_cwd: &Path,
    options: DoctorReportOptions<'_>,
) -> DoctorReport {
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
            Some("start `sigil-tui` to complete Quick Setup, or pass an explicit --config path"),
        );
        check_terminal(&mut report, None);
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
            check_terminal(&mut report, None);
            return report;
        }
    };

    if let Some(appearance_checks) = options.appearance_checks {
        report
            .checks
            .extend(appearance_checks(&root_config.appearance));
    }

    let workspace_root =
        resolve_workspace_root(config_path, launch_cwd, &root_config.workspace.root);
    let canonical_workspace = check_workspace(&mut report, &workspace_root);
    let sigil_paths =
        resolve_sigil_paths(&root_config.storage, &root_config.session, &workspace_root);
    check_storage_paths(&mut report, &sigil_paths);
    check_legacy_workspace_state(&mut report, config_path, &sigil_paths);
    check_provider(&mut report, &root_config);
    check_mcp_servers(&mut report, &root_config.mcp_servers, &workspace_root);
    check_code_intelligence(
        &mut report,
        &root_config,
        canonical_workspace.as_deref().unwrap_or(&workspace_root),
    );
    check_terminal(&mut report, Some(&root_config.terminal));
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

fn check_storage_paths(report: &mut DoctorReport, paths: &crate::SigilPaths) {
    report.push(
        DoctorStatus::Ok,
        "storage:state_root",
        paths.state_root.display().to_string(),
    );
    report.push(
        DoctorStatus::Ok,
        "storage:cache_root",
        paths.cache_root.display().to_string(),
    );
    report.push(
        DoctorStatus::Ok,
        "storage:workspace_state",
        paths.workspace_state_root.display().to_string(),
    );
    report.push(
        DoctorStatus::Ok,
        "storage:project_assets",
        paths.project_assets_root.display().to_string(),
    );
    check_session_log_dir(report, &paths.session_log_dir);
}

fn check_legacy_workspace_state(
    report: &mut DoctorReport,
    config_path: &Path,
    paths: &crate::SigilPaths,
) {
    let workspace_config = paths.workspace_root.join(WORKSPACE_CONFIG_FILE);
    if workspace_config.exists() && !same_path(&workspace_config, config_path) {
        report.push_with_remediation(
            DoctorStatus::Warn,
            "config:legacy_workspace",
            format!(
                "workspace {} is no longer loaded by default",
                workspace_config.display()
            ),
            Some(format!(
                "move local config to {}, pass --config explicitly, or delete the workspace copy",
                config_path.display()
            )),
        );
    }

    let legacy_sessions = paths
        .workspace_root
        .join(LEGACY_WORKSPACE_STATE_DIR)
        .join(LEGACY_SESSIONS_DIR);
    if legacy_sessions.exists() {
        report.push_with_remediation(
            DoctorStatus::Warn,
            "storage:legacy_sessions",
            format!(
                "legacy workspace sessions remain at {}",
                legacy_sessions.display()
            ),
            Some(format!(
                "migrate sessions to {} and remove the workspace copy",
                paths.session_log_dir.display()
            )),
        );
    }

    let legacy_input_history = paths
        .workspace_root
        .join(LEGACY_WORKSPACE_STATE_DIR)
        .join(LEGACY_INPUT_HISTORY_FILE);
    if legacy_input_history.exists() {
        report.push_with_remediation(
            DoctorStatus::Warn,
            "storage:legacy_input_history",
            format!(
                "legacy workspace input history remains at {}",
                legacy_input_history.display()
            ),
            Some(format!(
                "migrate input history to {} and remove the workspace copy",
                paths.input_history_file.display()
            )),
        );
    }
}

fn same_path(left: &Path, right: &Path) -> bool {
    match (fs::canonicalize(left), fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

fn check_session_log_dir(report: &mut DoctorReport, session_dir: &Path) {
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
            Some("create the parent directory, or use the default user state directory"),
        );
    }
}

fn check_provider(report: &mut DoctorReport, root_config: &RootConfig) {
    match provider_config_key(&root_config.agent.provider) {
        "deepseek" => check_deepseek_provider(report, root_config),
        "openai_compat" => check_openai_compat_provider(report, root_config),
        "anthropic" => check_anthropic_provider(report, root_config),
        "gemini" => check_gemini_provider(report, root_config),
        other => report.push_with_remediation(
            DoctorStatus::Error,
            "provider",
            format!("unsupported provider {other}"),
            Some("set [agent].provider to \"deepseek\", \"openai_compat\", \"anthropic\", or \"gemini\""),
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
    push_provider_capability_checks(report, "deepseek");
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
    push_provider_capability_checks(report, "openai_compat");
}

fn check_anthropic_provider(report: &mut DoctorReport, root_config: &RootConfig) {
    let config = match load_anthropic_config(root_config).and_then(|config| config.resolved()) {
        Ok(config) => config,
        Err(error) => {
            report.push_with_remediation(
                DoctorStatus::Error,
                "provider:anthropic",
                error.to_string(),
                Some("add a valid [providers.anthropic] block"),
            );
            return;
        }
    };
    report.push(
        DoctorStatus::Ok,
        "provider:anthropic",
        format!(
            "model={} base_url={} version={} max_tokens={}",
            config.model, config.base_url, config.anthropic_version, config.max_tokens
        ),
    );

    push_provider_auth_check(
        report,
        resolve_anthropic_api_key(&config),
        SIGIL_ANTHROPIC_API_KEY_ENV,
        "[providers.anthropic].api_key",
    );
    push_provider_capability_checks(report, "anthropic");
}

fn check_gemini_provider(report: &mut DoctorReport, root_config: &RootConfig) {
    let config = match load_gemini_config(root_config).and_then(|config| config.resolved()) {
        Ok(config) => config,
        Err(error) => {
            report.push_with_remediation(
                DoctorStatus::Error,
                "provider:gemini",
                error.to_string(),
                Some("add a valid [providers.gemini] block"),
            );
            return;
        }
    };
    report.push(
        DoctorStatus::Ok,
        "provider:gemini",
        format!("model={} base_url={}", config.model, config.base_url),
    );

    push_provider_auth_check(
        report,
        resolve_gemini_api_key(&config),
        SIGIL_GEMINI_API_KEY_ENV,
        "[providers.gemini].api_key",
    );
    push_provider_capability_checks(report, "gemini");
}

fn push_provider_capability_checks(report: &mut DoctorReport, provider_name: &str) {
    let Some(capabilities) = provider_capabilities_for_name(provider_name) else {
        return;
    };
    let view = provider_capability_view(provider_name, &capabilities);
    let supported = view
        .rows
        .iter()
        .filter(|row| row.status.as_str() == "supported")
        .count();
    let advanced = view
        .rows
        .iter()
        .filter(|row| row.status.as_str() == "advanced")
        .count();
    report.push(
        DoctorStatus::Ok,
        format!("provider:{provider_name}:capabilities"),
        format!(
            "{} supported, {} advanced, {} total",
            supported,
            advanced,
            view.rows.len()
        ),
    );
    for row in view.rows {
        report.push(
            DoctorStatus::Ok,
            format!("provider:{provider_name}:capability:{}", row.key),
            format!("{}: {} ({})", row.label, row.status.as_str(), row.detail),
        );
    }
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

fn check_terminal(report: &mut DoctorReport, config: Option<&TerminalConfig>) {
    let environment = TerminalEnvironment::from_env();
    check_terminal_with_env(report, config, &environment);
}

fn check_terminal_with_env(
    report: &mut DoctorReport,
    config: Option<&TerminalConfig>,
    environment: &TerminalEnvironment,
) {
    match environment
        .term
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        Some("dumb") => report.push_with_remediation(
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

    report.push(
        DoctorStatus::Ok,
        "terminal:profile",
        environment.profile_summary(),
    );

    if let Some(config) = config {
        report.push(
            DoctorStatus::Ok,
            "terminal:config",
            format!(
                "mouse_capture={} osc52_clipboard={} scroll_sensitivity={}",
                config.mouse_capture, config.osc52_clipboard, config.scroll_sensitivity
            ),
        );
        check_terminal_mouse(report, config, environment);
        check_terminal_clipboard(report, config, environment);
        report.push(
            DoctorStatus::Ok,
            "terminal:smoke",
            "run checklist: click, scroll, drag transcript, Ctrl-C copy; see docs/en/terminal-compatibility.md",
        );
    }
}

fn check_terminal_mouse(
    report: &mut DoctorReport,
    config: &TerminalConfig,
    environment: &TerminalEnvironment,
) {
    if !config.mouse_capture {
        report.push(
            DoctorStatus::Ok,
            "terminal:mouse",
            "mouse capture disabled by config; keyboard controls remain available",
        );
        return;
    }

    if environment.term_is_missing_or_dumb() {
        report.push_with_remediation(
            DoctorStatus::Warn,
            "terminal:mouse",
            "mouse capture enabled but TERM is missing or dumb",
            Some("fix TERM, or set [terminal].mouse_capture = false if this terminal cannot pass mouse events"),
        );
        return;
    }

    if environment.iterm_mouse_reporting == Some(false) {
        let profile = environment
            .iterm_profile
            .as_deref()
            .unwrap_or("current profile");
        report.push_with_remediation(
            DoctorStatus::Warn,
            "terminal:mouse",
            format!("mouse capture enabled but iTerm profile {profile} disables Mouse Reporting"),
            Some(
                "enable iTerm Settings > Profiles > Terminal > Mouse Reporting for this profile, or set [terminal].mouse_capture = false",
            ),
        );
        return;
    }

    if environment.has_multiplexer() {
        report.push_with_remediation(
            DoctorStatus::Warn,
            "terminal:mouse",
            format!(
                "mouse capture enabled through {}; verify multiplexer mouse pass-through",
                environment.multiplexer_label()
            ),
            Some(
                "enable mouse support in the multiplexer, or set [terminal].mouse_capture = false",
            ),
        );
        return;
    }

    report.push(
        DoctorStatus::Ok,
        "terminal:mouse",
        "mouse capture enabled; smoke: click controls, scroll transcript, drag-select text",
    );
}

fn check_terminal_clipboard(
    report: &mut DoctorReport,
    config: &TerminalConfig,
    environment: &TerminalEnvironment,
) {
    if !config.osc52_clipboard {
        report.push(
            DoctorStatus::Ok,
            "terminal:clipboard",
            "OSC52 clipboard disabled by config",
        );
        return;
    }

    if environment.term_is_missing_or_dumb() {
        report.push_with_remediation(
            DoctorStatus::Warn,
            "terminal:clipboard",
            "OSC52 clipboard enabled but TERM is missing or dumb",
            Some(
                "fix TERM, or set [terminal].osc52_clipboard = false if copy sequences are blocked",
            ),
        );
        return;
    }

    if environment.has_clipboard_bridge_risk() {
        report.push_with_remediation(
            DoctorStatus::Warn,
            "terminal:clipboard",
            format!(
                "OSC52 clipboard enabled through {}; verify clipboard pass-through",
                environment.clipboard_bridge_label()
            ),
            Some("smoke test Ctrl-C copy and paste; if blocked, set [terminal].osc52_clipboard = false"),
        );
        return;
    }

    report.push(
        DoctorStatus::Ok,
        "terminal:clipboard",
        "OSC52 clipboard enabled; smoke: drag-select transcript, press Ctrl-C, paste elsewhere",
    );
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct TerminalEnvironment {
    term: Option<String>,
    term_program: Option<String>,
    term_program_version: Option<String>,
    iterm_profile: Option<String>,
    iterm_mouse_reporting: Option<bool>,
    tmux: bool,
    screen: bool,
    ssh: bool,
    wsl: bool,
    wezterm: bool,
    kitty: bool,
    windows_terminal: bool,
}

impl TerminalEnvironment {
    fn from_env() -> Self {
        let term = non_empty_env("TERM");
        let term_program = non_empty_env("TERM_PROGRAM");
        let term_program_version = non_empty_env("TERM_PROGRAM_VERSION");
        let iterm_profile = non_empty_env("ITERM_PROFILE");
        let iterm_mouse_reporting = if term_program.as_deref() == Some("iTerm.app") {
            iterm_profile
                .as_deref()
                .and_then(iterm_mouse_reporting_for_profile)
        } else {
            None
        };
        Self {
            wezterm: non_empty_env("WEZTERM_EXECUTABLE").is_some()
                || term_program.as_deref() == Some("WezTerm"),
            kitty: non_empty_env("KITTY_WINDOW_ID").is_some()
                || term.as_deref().is_some_and(|term| term.contains("kitty")),
            windows_terminal: non_empty_env("WT_SESSION").is_some(),
            tmux: non_empty_env("TMUX").is_some(),
            screen: non_empty_env("STY").is_some()
                || term
                    .as_deref()
                    .is_some_and(|term| term.starts_with("screen")),
            ssh: non_empty_env("SSH_TTY").is_some() || non_empty_env("SSH_CONNECTION").is_some(),
            wsl: non_empty_env("WSL_DISTRO_NAME").is_some()
                || non_empty_env("WSL_INTEROP").is_some(),
            term,
            term_program,
            term_program_version,
            iterm_profile,
            iterm_mouse_reporting,
        }
    }

    fn term_is_missing_or_dumb(&self) -> bool {
        self.term
            .as_deref()
            .is_none_or(|term| term.trim().is_empty() || term == "dumb")
    }

    fn has_multiplexer(&self) -> bool {
        self.tmux || self.screen
    }

    fn has_clipboard_bridge_risk(&self) -> bool {
        self.has_multiplexer() || self.ssh || self.wsl
    }

    fn multiplexer_label(&self) -> &'static str {
        if self.tmux {
            "tmux"
        } else if self.screen {
            "screen"
        } else {
            "multiplexer"
        }
    }

    fn clipboard_bridge_label(&self) -> String {
        let mut layers = Vec::new();
        if self.tmux {
            layers.push("tmux");
        }
        if self.screen {
            layers.push("screen");
        }
        if self.ssh {
            layers.push("ssh");
        }
        if self.wsl {
            layers.push("wsl");
        }
        if layers.is_empty() {
            "terminal bridge".to_owned()
        } else {
            layers.join("+")
        }
    }

    fn profile_summary(&self) -> String {
        let mut parts = Vec::new();
        if let Some(term_program) = self.term_program.as_deref() {
            parts.push(format!("TERM_PROGRAM={term_program}"));
        }
        if let Some(version) = self.term_program_version.as_deref() {
            parts.push(format!("TERM_PROGRAM_VERSION={version}"));
        }
        if let Some(profile) = self.iterm_profile.as_deref() {
            parts.push(format!("ITERM_PROFILE={profile}"));
        }
        if let Some(mouse_reporting) = self.iterm_mouse_reporting {
            parts.push(format!("iterm_mouse_reporting={mouse_reporting}"));
        }
        if self.wezterm {
            parts.push("profile=wezterm".to_owned());
        }
        if self.kitty {
            parts.push("profile=kitty".to_owned());
        }
        if self.windows_terminal {
            parts.push("profile=windows_terminal".to_owned());
        }
        if self.tmux {
            parts.push("layer=tmux".to_owned());
        }
        if self.screen {
            parts.push("layer=screen".to_owned());
        }
        if self.ssh {
            parts.push("layer=ssh".to_owned());
        }
        if self.wsl {
            parts.push("layer=wsl".to_owned());
        }
        if parts.is_empty() {
            "profile=unknown".to_owned()
        } else {
            parts.join(" ")
        }
    }
}

fn iterm_mouse_reporting_for_profile(profile: &str) -> Option<bool> {
    let home = env::var_os("HOME")?;
    let plist = PathBuf::from(home)
        .join("Library")
        .join("Preferences")
        .join("com.googlecode.iterm2.plist");
    let bookmarks = plistbuddy_print(&plist, "Print :\"New Bookmarks\"")?;
    iterm_mouse_reporting_from_bookmarks(&bookmarks, profile)
}

fn iterm_mouse_reporting_from_bookmarks(bookmarks: &str, profile: &str) -> Option<bool> {
    let mut depth = 0usize;
    let mut in_profile = false;
    let mut profile_name = None;
    let mut mouse_reporting = None;

    for raw_line in bookmarks.lines() {
        let line = raw_line.trim();
        if line.ends_with('{') {
            if line == "Dict {" && depth == 1 {
                in_profile = true;
                profile_name = None;
                mouse_reporting = None;
            }
            depth = depth.saturating_add(1);
            continue;
        }
        if line == "}" {
            if in_profile && depth == 2 {
                if profile_name.as_deref() == Some(profile) {
                    return mouse_reporting;
                }
                in_profile = false;
            }
            depth = depth.saturating_sub(1);
            continue;
        }
        if !in_profile || depth != 2 {
            continue;
        }
        if let Some(value) = plistbuddy_line_value(line, "Name") {
            profile_name = Some(value.to_owned());
        }
        if let Some(value) = plistbuddy_line_value(line, "Mouse Reporting") {
            mouse_reporting = parse_plist_bool(value);
        }
    }
    None
}

fn plistbuddy_line_value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let value = line
        .strip_prefix(&format!("{key} = "))
        .or_else(|| line.strip_prefix(&format!("\"{key}\" = ")))?;
    Some(value.trim().trim_matches('"'))
}

fn parse_plist_bool(value: &str) -> Option<bool> {
    match value.trim() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn plistbuddy_print(plist: &Path, command: &str) -> Option<String> {
    let output = Command::new("/usr/libexec/PlistBuddy")
        .args(["-c", command])
        .arg(plist)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

fn non_empty_env(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
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
