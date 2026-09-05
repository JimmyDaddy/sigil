use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use crate::{
    load_anthropic_config, load_deepseek_config, load_gemini_config, load_openai_compat_config,
    load_openai_responses_config, provider_capabilities_for_name, provider_capability_view,
    provider_config_key, resolve_sigil_paths,
};
use sigil_kernel::cutover_manifest::{
    CutoverAuthorityStateV1, CutoverBlockerCodeV1, CutoverSurfaceStatusV1,
};
use sigil_kernel::{
    AppearanceConfig, DurableEventType, JsonlSessionStore, McpServerConfig, McpServerStartup,
    PluginCapability, PluginHookKind, PluginTrustDecision, PluginTrustEntry, RootConfig,
    SessionStreamRecord, StorageRoot, ToolEffect, config::TerminalConfig, default_user_config_path,
    private_path_permissions_are_restricted, resolve_workspace_root,
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
    let process_factory = sigil_process_observer::ProcessObserverFactoryV1::new(verifier_hash)
        .map_err(|error| error.to_string())?
        .instantiate();
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
    let persisted_source = fs::read(config_path).map_err(|error| error.to_string())?;
    let root_config = RootConfig::load_persisted(config_path).map_err(|error| error.to_string())?;
    let workspace_root =
        resolve_workspace_root(config_path, launch_cwd, &root_config.workspace.root);
    let paths = resolve_sigil_paths(&root_config.storage, &root_config.session, &workspace_root);
    let persisted_source_hash =
        sigil_resource_authority::bootstrap::canonical_bootstrap_hash(&persisted_source);
    let pending_intent = service
        .pending_fresh_root_config_intent()
        .map_err(|error| error.to_string())?;
    if let Some(intent) = pending_intent.as_ref()
        && !recovery_paths_match_intent(&paths, intent)
    {
        if persisted_source_hash != intent.expected_config_source_hash {
            return Err(
                "a durable recovery root-config switch is pending but the current config no longer matches either its staged source or selected roots; reconcile the config explicitly and retry doctor"
                    .to_owned(),
            );
        }
        let mut replacement_config = root_config.clone();
        replacement_config.storage.state_root =
            StorageRoot::Path(intent.state_root.to_string_lossy().into_owned());
        replacement_config.storage.cache_root =
            StorageRoot::Path(intent.cache_root.to_string_lossy().into_owned());
        let replacement_paths = resolve_sigil_paths(
            &replacement_config.storage,
            &replacement_config.session,
            &workspace_root,
        );
        if !recovery_paths_match_intent(&replacement_paths, intent) {
            return Err(
                "durable recovery root-config intent cannot be represented by the current configuration"
                    .to_owned(),
            );
        }
    }
    if let Some(receipt) = service
        .reconcile_pending_fresh_epoch_with_config_source(persisted_source_hash)
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
    let (root_ref, replacement_config, staged_root_config_source_hash, mut fresh_recovery_roots) =
        if let Some(intent) = pending_intent {
            let root_ref = service
                .restore_pending_fresh_root_config_selection()
                .map_err(|error| error.to_string())?
                .ok_or_else(|| {
                    "durable recovery root-config intent disappeared before doctor could restore it"
                        .to_owned()
                })?;
            if recovery_paths_match_intent(&paths, &intent) {
                (
                    root_ref,
                    None,
                    Some(intent.expected_config_source_hash),
                    None,
                )
            } else if persisted_source_hash == intent.expected_config_source_hash {
                let mut replacement_config = root_config.clone();
                replacement_config.storage.state_root =
                    StorageRoot::Path(intent.state_root.to_string_lossy().into_owned());
                replacement_config.storage.cache_root =
                    StorageRoot::Path(intent.cache_root.to_string_lossy().into_owned());
                let replacement_paths = resolve_sigil_paths(
                    &replacement_config.storage,
                    &replacement_config.session,
                    &workspace_root,
                );
                if !recovery_paths_match_intent(&replacement_paths, &intent) {
                    return Err(
                        "durable recovery root-config intent cannot be represented by the current configuration"
                            .to_owned(),
                    );
                }
                (
                    root_ref,
                    Some((replacement_config, persisted_source)),
                    Some(intent.expected_config_source_hash),
                    None,
                )
            } else {
                return Err(
                    "a durable recovery root-config switch is pending but the current config no longer matches either its staged source or selected roots; reconcile the config explicitly and retry doctor"
                        .to_owned(),
                );
            }
        } else {
            match service.prepare_fresh_root_selection(
                &paths.state_root,
                &paths.cache_root,
                &paths.scratch_root,
            ) {
                Ok(root_ref) => (root_ref, None, Some(persisted_source_hash), None),
                Err(original_error) => {
                    let fresh = FreshRecoveryRoots::create(&paths, &workspace_root)
                        .map_err(|error| {
                            format!(
                                "existing authority roots are not fresh ({original_error}) and fresh recovery roots could not be prepared: {error}"
                            )
                        })?;
                    let mut replacement_config = root_config.clone();
                    replacement_config.storage.state_root =
                        StorageRoot::Path(fresh.state_root().to_string_lossy().into_owned());
                    replacement_config.storage.cache_root =
                        StorageRoot::Path(fresh.cache_root().to_string_lossy().into_owned());
                    let replacement_paths = resolve_sigil_paths(
                        &replacement_config.storage,
                        &replacement_config.session,
                        &workspace_root,
                    );
                    let root_ref = service
                        .prepare_fresh_root_selection(
                            &replacement_paths.state_root,
                            &replacement_paths.cache_root,
                            &replacement_paths.scratch_root,
                        )
                        .map_err(|error| {
                            format!(
                                "fresh recovery root selection failed after replacement: {error}"
                            )
                        })?;
                    (
                        root_ref,
                        Some((replacement_config, persisted_source)),
                        Some(persisted_source_hash),
                        Some(fresh),
                    )
                }
            }
        };
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

    // The authority transaction and config file have separate durability domains. The durable
    // root-config intent is staged before publication, so a failed execute leaves normal boot
    // blocked for doctor reconciliation rather than reopening the old epoch under new roots. The
    // exact source snapshot prevents overwriting a concurrent repair.
    if let Some(expected_config_source_hash) = staged_root_config_source_hash {
        service
            .stage_fresh_root_config_switch(&operation, &authorization, expected_config_source_hash)
            .map_err(|error| error.to_string())?;
    }
    if let Some((replacement_config, expected_source)) = replacement_config.as_ref() {
        if let Err(error) =
            publish_recovery_root_config(replacement_config, config_path, expected_source)
        {
            // An atomic publication can report an uncertainty after the target file was
            // replaced. Preserve fresh roots and the prepared intent for retry only when the
            // publication may have happened. A definite CAS failure must retire the intent
            // before the temporary roots are dropped; otherwise normal boot would retain a
            // pointer to deleted roots and make doctor itself unrecoverable.
            let publication_uncertain = config_publication_is_uncertain(&error)
                || config_points_to_recovery_roots(
                    config_path,
                    replacement_config,
                    &workspace_root,
                );
            if publication_uncertain {
                if let Some(fresh) = fresh_recovery_roots.take() {
                    fresh.persist();
                }
            } else if let Some(expected_config_source_hash) = staged_root_config_source_hash
                && let Err(abort_error) = service.abort_staged_root_config_switch(
                    &operation,
                    &authorization,
                    expected_config_source_hash,
                )
            {
                if let Some(fresh) = fresh_recovery_roots.take() {
                    fresh.persist();
                }
                return Err(format!(
                    "{error}; config publication was not confirmed and the recovery intent could not be retired; fresh roots were retained for doctor retry: {abort_error}"
                ));
            }
            return Err(error.to_string());
        }
        if let Some(fresh) = fresh_recovery_roots.take() {
            // From this point on the durable config may refer to these roots even if a later
            // authority execute or current-schema boot fails, so keep them for doctor retry.
            fresh.persist();
        }
    }
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

/// Fresh state/cache roots used only when operator recovery encounters a non-empty configured
/// root. The directories are intentionally created outside the authority namespace; the doctor
/// operation still binds their canonical identities into the recovery receipt.
struct FreshRecoveryRoots {
    state_root: tempfile::TempDir,
    cache_root: tempfile::TempDir,
}

impl FreshRecoveryRoots {
    fn create(paths: &crate::paths::SigilPaths, workspace_root: &Path) -> Result<Self, String> {
        let state_parent = paths
            .state_root
            .parent()
            .ok_or_else(|| "configured state root has no parent".to_owned())?;
        let cache_parent = paths
            .cache_root
            .parent()
            .ok_or_else(|| "configured cache root has no parent".to_owned())?;
        let state_root = tempfile::Builder::new()
            .prefix("sigil-recovery-state-")
            .tempdir_in(state_parent)
            .map_err(|error| format!("create state root: {error}"))?;
        let cache_root = tempfile::Builder::new()
            .prefix("sigil-recovery-cache-")
            .tempdir_in(cache_parent)
            .map_err(|error| format!("create cache root: {error}"))?;
        let workspace_id = crate::workspace_id_for_root(workspace_root);
        let workspaces_root = cache_root.path().join("workspaces");
        let workspace_cache_root = workspaces_root.join(workspace_id);
        let scratch_root = workspace_cache_root.join(crate::paths::DEFAULT_SCRATCH_DIR);
        fs::create_dir_all(&scratch_root)
            .map_err(|error| format!("create recovery execution-temp root: {error}"))?;
        for path in [
            state_root.path(),
            cache_root.path(),
            workspaces_root.as_path(),
            workspace_cache_root.as_path(),
            scratch_root.as_path(),
        ] {
            sigil_kernel::secure_private_path_permissions(path)
                .map_err(|error| format!("secure recovery root {}: {error}", path.display()))?;
        }
        Ok(Self {
            state_root,
            cache_root,
        })
    }

    fn state_root(&self) -> &Path {
        self.state_root.path()
    }

    fn cache_root(&self) -> &Path {
        self.cache_root.path()
    }

    fn persist(mut self) {
        // TempDir::keep is intentionally deferred until config publication succeeds. This
        // makes confirmation, quiescence, and CAS failures self-cleaning while retaining roots
        // for any later failure after the durable config can refer to them.
        self.state_root.disable_cleanup(true);
        self.cache_root.disable_cleanup(true);
    }
}

fn config_points_to_recovery_roots(
    config_path: &Path,
    replacement: &RootConfig,
    workspace_root: &Path,
) -> bool {
    let Ok(current) = RootConfig::load_persisted(config_path) else {
        return false;
    };
    let current_paths = resolve_sigil_paths(&current.storage, &current.session, workspace_root);
    let replacement_paths =
        resolve_sigil_paths(&replacement.storage, &replacement.session, workspace_root);
    current_paths.state_root == replacement_paths.state_root
        && current_paths.cache_root == replacement_paths.cache_root
        && current_paths.scratch_root == replacement_paths.scratch_root
}

fn recovery_paths_match_intent(
    paths: &crate::paths::SigilPaths,
    intent: &sigil_resource_authority::bootstrap::AuthorityBootstrapPendingRootConfigIntentV1,
) -> bool {
    paths.state_root == intent.state_root
        && paths.cache_root == intent.cache_root
        && paths.scratch_root == intent.execution_temp_root
}

fn publish_recovery_root_config(
    replacement: &RootConfig,
    config_path: &Path,
    expected_source: &[u8],
) -> anyhow::Result<()> {
    match replacement.save_if_source_bytes_unchanged(config_path, expected_source) {
        Ok(()) => Ok(()),
        Err(error)
            if error
                .downcast_ref::<sigil_kernel::ConfigPublishError>()
                .is_some_and(|publish| {
                    matches!(
                        publish,
                        sigil_kernel::ConfigPublishError::ReplacementPartiallyApplied { .. }
                            | sigil_kernel::ConfigPublishError::PublishedButDurabilityUncertain {
                                ..
                            }
                            | sigil_kernel::ConfigPublishError::PublishedButVisibilityUncertain { .. }
                    )
                }) => Err(error.context(
                    "fresh recovery roots were prepared, but config publication is uncertain; authority epoch was not advanced; reconcile the config publication and retry",
                )),
        Err(error) => Err(error.context(
            "fresh recovery roots were prepared, but config publication was not confirmed",
        )),
    }
}

fn config_publication_is_uncertain(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<sigil_kernel::ConfigPublishError>()
        .is_some_and(|publish| {
            matches!(
                publish,
                sigil_kernel::ConfigPublishError::ReplacementPartiallyApplied { .. }
                    | sigil_kernel::ConfigPublishError::PublishedButDurabilityUncertain { .. }
                    | sigil_kernel::ConfigPublishError::PublishedButVisibilityUncertain { .. }
            )
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
