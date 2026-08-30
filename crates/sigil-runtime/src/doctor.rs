use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use sigil_kernel::cutover_manifest::{
    CutoverAuthorityStateV1, CutoverBlockerCodeV1, CutoverSurfaceStatusV1,
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

/// Constructs the independent doctor/operator authority bootstrap recovery service. Normal boot
/// never receives this service and no tool/model path is wired to it; the returned service owns
/// its ephemeral authorization table and uses the real host process observer factory.
pub fn authority_bootstrap_recovery_service(
    config_path: &Path,
) -> Result<sigil_resource_authority::AuthorityBootstrapRecoveryServiceV1, String> {
    let canonical_config_path = fs::canonicalize(config_path).map_err(|error| error.to_string())?;
    let verifier_hash = sigil_process_observer::canonical_digest(
        format!(
            "sigil-authority-bootstrap-recovery-process-observer-v1\0{}",
            canonical_config_path.display()
        )
        .as_bytes(),
    );
    let process_factory =
        sigil_process_observer::ProcessObserverFactoryV1::new(verifier_hash).instantiate();
    sigil_resource_authority::AuthorityBootstrapRecoveryServiceV1::for_canonical_config_path(
        &canonical_config_path,
        process_factory,
    )
    .map_err(|error| error.to_string())
}

/// Safe operator-facing projection of one completed bootstrap recovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorityBootstrapRecoverySummaryV1 {
    pub old_authority_epoch: u64,
    pub new_authority_epoch: u64,
    pub receipt_hash: String,
    pub reconciled_after_crash: bool,
}

/// Runs the complete doctor/operator recovery flow. The callback receives the exact challenge
/// digest and must return the operator's typed confirmation. Journal evidence comes only from the
/// durable failed-boot record; process evidence comes only from the authority inventory.
pub fn recover_authority_bootstrap_with_confirmation<F>(
    config_path: &Path,
    launch_cwd: &Path,
    confirm: F,
) -> Result<AuthorityBootstrapRecoverySummaryV1, String>
where
    F: FnOnce(&str) -> Result<String, String>,
{
    let service = authority_bootstrap_recovery_service(config_path)?;
    if let Some(receipt) = service
        .reconcile_pending_fresh_epoch()
        .map_err(|error| error.to_string())?
    {
        crate::r71_authority_composition::boot_current_schema(config_path, launch_cwd)
            .map_err(|error| format!("reconciled epoch failed current-schema boot: {error}"))?;
        return Ok(AuthorityBootstrapRecoverySummaryV1 {
            old_authority_epoch: receipt.old_authority_epoch,
            new_authority_epoch: receipt.new_authority_epoch,
            receipt_hash: receipt.receipt_hash.to_hex(),
            reconciled_after_crash: true,
        });
    }

    let root_config = RootConfig::load(config_path).map_err(|error| error.to_string())?;
    let workspace_root =
        resolve_workspace_root(config_path, launch_cwd, &root_config.workspace.root);
    let paths = resolve_sigil_paths(&root_config.storage, &root_config.session, &workspace_root);
    let root_ref = service
        .prepare_fresh_root_selection(&paths.state_root, &paths.cache_root, &paths.scratch_root)
        .map_err(|error| error.to_string())?;
    let selection_hash = service
        .prepared_root_selection_hash(&root_ref)
        .map_err(|error| error.to_string())?;
    let (failed_bootstrap_hash, evidence) = service
        .observed_failed_journal_evidence()
        .map_err(|error| error.to_string())?;
    let evidence_set_hash =
        sigil_resource_authority::AuthorityBootstrapRecoveryServiceV1::evidence_set_hash(&evidence)
            .map_err(|error| error.to_string())?;
    let quiescence = service
        .probe_old_epoch_quiescence(evidence_set_hash)
        .map_err(|error| error.to_string())?;
    let operation =
        sigil_resource_authority::AuthorityBootstrapRecoveryOperationV1::SelectFreshAuthorityEpoch {
            explicit_root_config: root_ref,
            expected_failed_bootstrap_hash: Some(failed_bootstrap_hash),
            failed_journal_evidence: evidence,
            evidence_set_hash,
            old_epoch_quiescence: Box::new(quiescence.clone()),
        };
    let now_ms = operator_epoch_ms();
    let challenge = service
        .issue_operator_challenge(&operation, now_ms, 5 * 60 * 1000)
        .map_err(|error| error.to_string())?;
    let expected = challenge.challenge_hash.to_hex();
    let supplied = confirm(&expected)?;
    if supplied.trim() != expected {
        return Err("operator confirmation does not match the recovery challenge".to_owned());
    }
    let confirmed_at_ms = operator_epoch_ms();
    let confirmation =
        sigil_resource_authority::ExactBootstrapOperatorConfirmationV1::for_challenge(
            &challenge,
            evidence_set_hash,
            Some(quiescence.proof_hash),
            Some(selection_hash),
            confirmed_at_ms,
        );
    let authorization = service
        .authorize(&operation, confirmation, confirmed_at_ms)
        .map_err(|error| error.to_string())?;
    let receipt = service
        .execute(operation, authorization)
        .map_err(|error| error.to_string())?;
    crate::r71_authority_composition::boot_current_schema(config_path, launch_cwd)
        .map_err(|error| format!("fresh epoch failed current-schema boot: {error}"))?;
    Ok(AuthorityBootstrapRecoverySummaryV1 {
        old_authority_epoch: receipt.old_authority_epoch,
        new_authority_epoch: receipt.new_authority_epoch,
        receipt_hash: receipt.receipt_hash.to_hex(),
        reconciled_after_crash: false,
    })
}

fn operator_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            duration.as_millis().min(u128::from(u64::MAX)) as u64
        })
}

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
    check_plan_execution_spine, check_plan_review_compatibility, check_session_route_compatibility,
    check_session_streams, check_storage_paths, check_workspace,
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
    pub cutover: CutoverSurfaceStatusV1,
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

/// RFC-0071 R71.6: cutover epoch state for the shared doctor (four surfaces read the same
/// manifest next to the config). Read-only; a corrupted manifest is an Error so the startup
/// blocker is visible in doctor before any run.
pub(crate) fn check_cutover(
    report: &mut DoctorReport,
    config_path: &Path,
) -> CutoverSurfaceStatusV1 {
    let status = match crate::r71_global_cutover::inspect_cutover_manifest(config_path) {
        Ok(None) => CutoverSurfaceStatusV1::default(),
        Ok(Some(manifest)) => CutoverSurfaceStatusV1::from_manifest(&manifest),
        Err(_) => CutoverSurfaceStatusV1::unavailable(),
    };
    let epoch = match status.epoch {
        sigil_kernel::cutover_manifest::CutoverSurfaceEpochV1::Legacy => "legacy",
        sigil_kernel::cutover_manifest::CutoverSurfaceEpochV1::NewCurrentSchema => {
            "new-current-schema"
        }
        sigil_kernel::cutover_manifest::CutoverSurfaceEpochV1::Unavailable => "unavailable",
    };
    let authority = match status.authority {
        CutoverAuthorityStateV1::Legacy => "legacy",
        CutoverAuthorityStateV1::Ready => "ready",
        CutoverAuthorityStateV1::Blocked => "blocked",
        CutoverAuthorityStateV1::Unavailable => "unavailable",
    };
    report.push(
        if status.authority == CutoverAuthorityStateV1::Unavailable {
            DoctorStatus::Error
        } else {
            DoctorStatus::Ok
        },
        "cutover:epoch",
        format!("epoch={epoch}"),
    );
    report.push(
        match status.authority {
            CutoverAuthorityStateV1::Blocked | CutoverAuthorityStateV1::Unavailable => {
                DoctorStatus::Error
            }
            CutoverAuthorityStateV1::Legacy | CutoverAuthorityStateV1::Ready => DoctorStatus::Ok,
        },
        "cutover:authority",
        format!("authority={authority}"),
    );
    if status.blockers.is_empty() {
        report.push(
            DoctorStatus::Ok,
            "cutover:blocker",
            "no active cutover blockers",
        );
    } else {
        for blocker in &status.blockers {
            let adapter = blocker
                .adapter
                .map(|value| format!(" adapter={value:?}"))
                .unwrap_or_default();
            let (message, remediation) = match blocker.code {
                CutoverBlockerCodeV1::ManifestCorrupt => (
                    "persisted cutover manifest is unavailable or corrupt",
                    "restore or remove the manifest before selecting the current-schema epoch",
                ),
                CutoverBlockerCodeV1::MissingReadinessProbe => (
                    "current-schema readiness probe is missing",
                    "recompose the mandatory adapter and rerun doctor",
                ),
                CutoverBlockerCodeV1::AdapterNotReady => (
                    "mandatory adapter readiness probe failed",
                    "repair the reported adapter before starting current-schema boot",
                ),
                CutoverBlockerCodeV1::UnsupportedLegacyData => (
                    "persisted legacy data is unsupported for current-schema boot",
                    "migrate or create a current-schema session before starting a run",
                ),
            };
            report.push_with_remediation(
                DoctorStatus::Error,
                "cutover:blocker",
                format!("{message}{adapter}"),
                Some(remediation),
            );
        }
    }
    status
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
    report.cutover = check_cutover(&mut report, config_path);

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
    check_plan_execution_spine(&mut report, &sigil_paths.session_log_dir);
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
