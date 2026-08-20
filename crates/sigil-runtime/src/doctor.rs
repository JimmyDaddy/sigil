use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use sigil_kernel::{
    AppearanceConfig, DurableEventType, JsonlSessionStore, McpServerConfig, McpServerStartup,
    PluginCapability, PluginHookKind, PluginTrustDecision, PluginTrustEntry, RootConfig,
    SessionStreamRecord, ToolEffect, config::TerminalConfig, default_user_config_path,
    private_path_permissions_are_restricted, resolve_workspace_root,
};
use sigil_provider_anthropic::SIGIL_ANTHROPIC_API_KEY_ENV;
use sigil_provider_deepseek::SIGIL_API_KEY_ENV;
use sigil_provider_gemini::SIGIL_GEMINI_API_KEY_ENV;
use sigil_provider_openai_compat::OPENAI_COMPATIBLE_API_KEY_ENV;
use sigil_provider_openai_responses::OPENAI_RESPONSES_API_KEY_ENV;

use crate::{
    SecretResolution, SecretSource, load_anthropic_config, load_deepseek_config,
    load_gemini_config, load_openai_compat_config, load_openai_responses_config,
    provider_capabilities_for_name, provider_capability_view, provider_config_key,
    resolve_anthropic_api_key, resolve_deepseek_api_key, resolve_gemini_api_key,
    resolve_openai_compat_api_key, resolve_openai_responses_api_key, resolve_sigil_paths,
};

const MAX_SESSION_STREAMS_DOCTOR_SCAN: usize = 20;
const MAX_SESSION_STREAM_DOCTOR_BYTES: u64 = 16 * 1024 * 1024;

mod code_intel; // code-intelligence and LSP readiness checks.
mod mcp; // MCP server, plugin hook, and command availability checks.
mod orchestration; // release rollout and coarse orchestration rollback state.
mod providers; // provider config, auth, capability, and sandbox checks.
mod session; // workspace, storage, and session stream checks.
mod terminal; // terminal profile, mouse, and clipboard checks.
mod web; // offline Web V1 capability and route diagnostics.

pub use code_intel::build_code_intelligence_checks;
use code_intel::check_code_intelligence;
use mcp::{CommandStatus, check_mcp_servers, check_plugin_hooks, command_status};
use orchestration::check_orchestration_rollout;
use providers::{check_execution_backend, check_provider};
use session::{
    check_cache_runtime_invariants, check_orchestration_route_disablement,
    check_plan_review_compatibility, check_session_route_compatibility, check_session_streams,
    check_storage_paths, check_workspace,
};
use terminal::check_terminal;
pub use web::{
    WebDoctorBindingState, WebDoctorHostedCapability, WebDoctorSnapshot, append_web_doctor_snapshot,
};

#[cfg(test)]
use providers::secret_source_label;
#[cfg(test)]
use session::check_session_log_dir;
#[cfg(test)]
use terminal::{
    TerminalEnvironment, check_terminal_with_env, iterm_mouse_reporting_from_bookmarks,
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

/// Entrypoint-supplied appearance diagnostics hook.
pub type AppearanceDoctorChecks = dyn Fn(&AppearanceConfig) -> Vec<DoctorCheck>;

/// Optional diagnostics supplied by higher-level entrypoints.
#[derive(Clone, Copy, Default)]
pub struct DoctorReportOptions<'a> {
    pub appearance_checks: Option<&'a AppearanceDoctorChecks>,
    pub plugin_trust_entries: Option<&'a [PluginTrustEntry]>,
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

    if matches!(
        private_path_permissions_are_restricted(config_path),
        Ok(false)
    ) {
        report.push_with_remediation(
            DoctorStatus::Warn,
            "config:permissions",
            "config permissions allow access beyond the current user",
            Some("save Provider settings again to atomically tighten config permissions"),
        );
    }
    if default_user_config_path()
        .ok()
        .as_deref()
        .is_some_and(|default_path| default_path == config_path)
        && config_path.parent().is_some_and(|parent| {
            matches!(private_path_permissions_are_restricted(parent), Ok(false))
        })
    {
        report.push_with_remediation(
            DoctorStatus::Warn,
            "config:parent_permissions",
            "the Sigil config directory allows access beyond the current user",
            Some("save Provider settings again to tighten the Sigil config directory"),
        );
    }
    if let Ok(credential_path) =
        crate::provider_connections::FileProviderCredentialStore::default_path()
    {
        match fs::symlink_metadata(&credential_path) {
            Ok(_) => {
                match private_path_permissions_are_restricted(&credential_path) {
                    Ok(true) => {}
                    Ok(false) => report.push_with_remediation(
                        DoctorStatus::Warn,
                        "credential_store:permissions",
                        "the Sigil credential file allows access beyond the current user",
                        Some(
                            "open Provider settings and save a credential again to tighten permissions",
                        ),
                    ),
                    Err(_) => report.push_with_remediation(
                        DoctorStatus::Error,
                        "credential_store:path_invalid",
                        "the Sigil credential path is unsafe or could not be inspected",
                        Some(
                            "replace the credential path with a regular owner-only file, then save Provider settings again",
                        ),
                    ),
                }
                if let Some(parent) = credential_path.parent() {
                    match private_path_permissions_are_restricted(parent) {
                        Ok(true) => {}
                        Ok(false) => report.push_with_remediation(
                            DoctorStatus::Warn,
                            "credential_store:parent_permissions",
                            "the Sigil credential directory allows access beyond the current user",
                            Some(
                                "open Provider settings and save a credential again to tighten the directory",
                            ),
                        ),
                        Err(_) => report.push_with_remediation(
                            DoctorStatus::Error,
                            "credential_store:parent_invalid",
                            "the Sigil credential directory could not be inspected safely",
                            Some(
                                "repair the credential directory and rerun `sigil doctor`",
                            ),
                        ),
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => report.push_with_remediation(
                DoctorStatus::Error,
                "credential_store:inspection_failed",
                "the Sigil credential path could not be inspected",
                Some("repair the credential path and rerun `sigil doctor`"),
            ),
        }
    }

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
    check_session_streams(&mut report, &sigil_paths.session_log_dir);
    check_cache_runtime_invariants(&mut report, &sigil_paths.session_log_dir);
    check_plan_review_compatibility(&mut report, &sigil_paths.session_log_dir);
    check_session_route_compatibility(&mut report, &sigil_paths.session_log_dir, &root_config);
    check_orchestration_rollout(&mut report, &root_config);
    let runtime_provider = crate::provider_connections::resolve_default_model_route(&root_config)
        .map(|(provider, _)| provider)
        .unwrap_or_else(|_| "unknown".to_owned());
    check_orchestration_route_disablement(
        &mut report,
        &sigil_paths.session_log_dir,
        &crate::OrchestrationRouteGuard::new(
            &runtime_provider,
            &root_config.agent.model,
            crate::ORCHESTRATION_RUNTIME_BUILD_ID,
        ),
    );
    check_provider(&mut report, &root_config, &sigil_paths.cache_root);
    check_mcp_servers(&mut report, &root_config, &workspace_root);
    append_web_doctor_snapshot(
        &mut report,
        &WebDoctorSnapshot::from_root_config(&root_config),
    );
    check_plugin_hooks(
        &mut report,
        canonical_workspace.as_deref().unwrap_or(&workspace_root),
        options.plugin_trust_entries.unwrap_or_default(),
    );
    check_code_intelligence(
        &mut report,
        &root_config,
        canonical_workspace.as_deref().unwrap_or(&workspace_root),
    );
    check_terminal(&mut report, Some(&root_config.terminal));
    check_execution_backend(&mut report, &root_config);
    report
}

#[cfg(test)]
#[path = "tests/doctor_tests.rs"]
mod tests;
