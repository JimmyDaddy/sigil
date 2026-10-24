//! Fresh route-only facts for run admission and preparation; no display catalog I/O.

use super::*;

#[cfg(test)]
#[path = "../tests/application_run_start_tests.rs"]
mod tests;

/// Host-only facts required before starting one run. This projection grants no execution authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationRunStartView {
    pub model_ref: ModelRef,
    pub provider_name: String,
    pub model_selection_binding: String,
    pub default_permission_mode: PermissionMode,
    pub available_reasoning_efforts: Vec<ReasoningEffort>,
    pub default_reasoning_effort: Option<ReasoningEffort>,
    pub reasoning_effort_binding: Option<String>,
    pub route_recovery: Option<ApplicationSessionRouteRecoveryView>,
    /// Resolves only the requested replacement against current connection configuration.
    pub requested_model_available: bool,
    pub(super) source_model_ref: ModelRef,
}

/// Reads fresh durable route and configuration facts without scanning extensions or model catalogs.
///
/// # Errors
/// Returns an error for malformed durable state, a scope mismatch, or unreadable configuration.
pub fn application_run_start_view(
    config_path: &Path,
    session_path: &Path,
    expected_session_scope_id: &str,
    requested_model: Option<&ModelRef>,
) -> Result<ApplicationRunStartView> {
    let started = std::time::Instant::now();
    let result = (|| {
        let root_config = RootConfig::load(config_path)?.with_effective_composition()?;
        let entries = application_bound_session_entries(session_path, expected_session_scope_id)?;
        resolve_run_start_view(
            &root_config,
            &entries,
            expected_session_scope_id,
            requested_model,
        )
    })();
    tracing::debug!(target: "sigil_run_latency", phase = "run_route_projection",
        session_scope_id = expected_session_scope_id,
        elapsed_ms = started.elapsed().as_secs_f64() * 1000.0,
        succeeded = result.is_ok(), "run route projection ended");
    result
}

/// Inspects immutable execution selection through a scope-checked observer, without acquiring a
/// session attachment, opening a writer, or recovering execution state.
///
/// # Errors
/// Returns an error for unreadable configuration, malformed composition or changed source identity.
pub fn application_session_composition_recovery_view(
    config_path: &Path,
    session_path: &Path,
    expected_session_scope_id: &str,
) -> Result<Option<ApplicationSessionRouteRecoveryView>> {
    let config = RootConfig::load(config_path)?.with_effective_composition()?;
    let entries = application_bound_session_entries(session_path, expected_session_scope_id)?;
    composition_recovery_view(&config, &entries, expected_session_scope_id)
}

/// Checks only persistence of an already authorized Plan against the immutable boot selection.
/// A readable TOML syntax error may not erase that local Save action; every valid configuration
/// still supplies its fresh selection. Missing config, invalid schema/selection and corrupt or
/// unbound durable history remain errors. This grants no model, tool or provider execution.
///
/// # Errors
/// Returns an error for unreadable config, invalid durable identity/binding or invalid selection.
pub fn application_session_plan_save_recovery_view(
    config_path: &Path,
    session_path: &Path,
    expected_session_scope_id: &str,
    boot_composition: &sigil_kernel::RuntimeCompositionConfig,
) -> Result<Option<ApplicationSessionRouteRecoveryView>> {
    let raw = std::fs::read_to_string(config_path).with_context(|| {
        format!(
            "failed to read Plan save configuration at {}",
            config_path.display()
        )
    })?;
    let entries = application_bound_session_entries(session_path, expected_session_scope_id)?;
    anyhow::ensure!(
        entries.iter().any(|entry| matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::SessionCompositionBound(_))
        )),
        "plan save requires an existing session composition binding"
    );
    if toml::from_str::<toml::Value>(&raw).is_ok() {
        let config =
            RootConfig::parse_with_model_request_env(&raw)?.with_effective_composition()?;
        return composition_recovery_view(&config, &entries, expected_session_scope_id);
    }
    composition_recovery_view_for_selection(
        &sigil_kernel::SessionCompositionSnapshotV1::new(boot_composition.selected_capabilities()),
        &entries,
        expected_session_scope_id,
    )
}

fn composition_recovery_view(
    config: &RootConfig,
    entries: &[SessionLogEntry],
    scope: &str,
) -> Result<Option<ApplicationSessionRouteRecoveryView>> {
    composition_recovery_view_for_selection(
        &sigil_kernel::SessionCompositionSnapshotV1::new(config.selected_capabilities()),
        entries,
        scope,
    )
}

fn composition_recovery_view_for_selection(
    expected: &sigil_kernel::SessionCompositionSnapshotV1,
    entries: &[SessionLogEntry],
    scope: &str,
) -> Result<Option<ApplicationSessionRouteRecoveryView>> {
    use sha2::Digest as _;

    match crate::session_composition::validate_session_composition_entries(entries, expected) {
        Ok(()) => return Ok(None),
        Err(error) if error.is::<crate::SessionCompositionMismatch>() => {}
        Err(error) => return Err(error),
    }
    let actual = entries.iter().find_map(|entry| match entry {
        SessionLogEntry::Control(ControlEntry::SessionCompositionBound(snapshot)) => Some(snapshot),
        _ => None,
    });
    let binding = sha2::Sha256::digest(serde_json::to_vec(&(scope, actual, expected))?);
    Ok(Some(ApplicationSessionRouteRecoveryView {
        code: ApplicationSessionRouteRecoveryCode::SessionCompositionChanged,
        allowed_actions: vec![
            ApplicationSessionRouteRecoveryAction::StartNewSession,
            ApplicationSessionRouteRecoveryAction::BackToSessionLibrary,
        ],
        recovery_binding: format!("session-composition:{binding:x}"),
        retryable: false,
    }))
}

pub(super) fn resolve_run_start_view(
    root_config: &RootConfig,
    entries: &[SessionLogEntry],
    expected_session_scope_id: &str,
    requested_model: Option<&ModelRef>,
) -> Result<ApplicationRunStartView> {
    if expected_session_scope_id.is_empty() {
        bail!("expected run-context session scope must not be empty");
    }
    let composition_recovery =
        composition_recovery_view(root_config, entries, expected_session_scope_id)?;
    let route = application_session_route(entries).ok_or_else(|| {
        anyhow!(
            "session_route_missing: restore the referenced connection or fork with current route"
        )
    })?;
    let config_snapshot =
        crate::provider_connections::ResolvedRouteConfigSnapshot::from_root_config(root_config);
    let plan = crate::provider_connections::plan_session_route_resume(
        &config_snapshot,
        &crate::provider_connections::SessionRouteResumeInput {
            route: route.clone(),
            egress_trust_binding: application_session_route_trust_binding(entries),
        },
    );
    let recovery_binding = || {
        let frontier = crate::provider_connections::session_route_frontier_binding(entries);
        let authority_generation =
            crate::provider_connections::session_route_authority_generation_binding(entries);
        config_snapshot.recovery_binding(
            expected_session_scope_id,
            &route,
            &frontier,
            &authority_generation,
        )
    };
    let (provider_name, effective_route, route_recovery) = match plan {
        crate::provider_connections::SessionRouteResumePlan::Exact {
            provider_name,
            route,
        }
        | crate::provider_connections::SessionRouteResumePlan::RebindCurrentModel {
            provider_name,
            target_route: route,
            ..
        } => (provider_name, route, None),
        crate::provider_connections::SessionRouteResumePlan::NeedsConfirmation {
            provider_name,
            target_route,
            ..
        } => (
            provider_name,
            target_route,
            Some(ApplicationSessionRouteRecoveryView {
                code: ApplicationSessionRouteRecoveryCode::SessionRouteConfirmationRequired,
                allowed_actions: vec![
                    ApplicationSessionRouteRecoveryAction::ConfirmCurrentRoute,
                    ApplicationSessionRouteRecoveryAction::RepairConnection,
                    ApplicationSessionRouteRecoveryAction::SelectReplacement,
                    ApplicationSessionRouteRecoveryAction::StartNewSession,
                    ApplicationSessionRouteRecoveryAction::BackToSessionLibrary,
                ],
                recovery_binding: recovery_binding(),
                retryable: true,
            }),
        ),
        crate::provider_connections::SessionRouteResumePlan::NeedsReplacement {
            reason: crate::provider_connections::SessionRouteUnavailableReason::ConnectionNotFound,
            ..
        } => (
            application_session_identity(entries)
                .map(|(provider, _)| provider)
                .unwrap_or_else(|| "unavailable".to_owned()),
            route.clone(),
            Some(ApplicationSessionRouteRecoveryView {
                code: ApplicationSessionRouteRecoveryCode::SessionRouteSelectionRequired,
                allowed_actions: vec![
                    ApplicationSessionRouteRecoveryAction::RepairConnection,
                    ApplicationSessionRouteRecoveryAction::SelectReplacement,
                    ApplicationSessionRouteRecoveryAction::StartNewSession,
                    ApplicationSessionRouteRecoveryAction::BackToSessionLibrary,
                ],
                recovery_binding: recovery_binding(),
                retryable: true,
            }),
        ),
        crate::provider_connections::SessionRouteResumePlan::NeedsReplacement {
            reason:
                crate::provider_connections::SessionRouteUnavailableReason::ConnectionConfigInvalid,
            ..
        }
        | crate::provider_connections::SessionRouteResumePlan::NeedsSetup {
            reason: crate::provider_connections::ModelRouteSetupReason::ConfigurationInvalid,
        } => (
            application_session_identity(entries)
                .map(|(provider, _)| provider)
                .unwrap_or_else(|| "unavailable".to_owned()),
            route.clone(),
            Some(ApplicationSessionRouteRecoveryView {
                code: ApplicationSessionRouteRecoveryCode::ConnectionConfigInvalid,
                allowed_actions: vec![
                    ApplicationSessionRouteRecoveryAction::RepairConnection,
                    ApplicationSessionRouteRecoveryAction::SelectReplacement,
                    ApplicationSessionRouteRecoveryAction::StartNewSession,
                    ApplicationSessionRouteRecoveryAction::BackToSessionLibrary,
                ],
                recovery_binding: recovery_binding(),
                retryable: false,
            }),
        ),
        crate::provider_connections::SessionRouteResumePlan::NeedsSetup {
            reason: crate::provider_connections::ModelRouteSetupReason::RouteNotConfigured,
        } => (
            application_session_identity(entries)
                .map(|(provider, _)| provider)
                .unwrap_or_else(|| "unavailable".to_owned()),
            route.clone(),
            Some(ApplicationSessionRouteRecoveryView {
                code: ApplicationSessionRouteRecoveryCode::ModelRouteNotConfigured,
                allowed_actions: vec![
                    ApplicationSessionRouteRecoveryAction::RepairConnection,
                    ApplicationSessionRouteRecoveryAction::StartNewSession,
                    ApplicationSessionRouteRecoveryAction::BackToSessionLibrary,
                ],
                recovery_binding: recovery_binding(),
                retryable: false,
            }),
        ),
    };
    let route_recovery = composition_recovery.or(route_recovery);
    let model_name = effective_route.model_ref.model_id.clone();
    let available_reasoning_efforts = if route_recovery.as_ref().is_some_and(|recovery| {
        recovery.code != ApplicationSessionRouteRecoveryCode::SessionRouteConfirmationRequired
    }) {
        Vec::new()
    } else {
        crate::reasoning_effort::supported_reasoning_efforts(&provider_name, &model_name)
    };
    let mut identity_config = root_config.clone();
    identity_config.agent.runtime_provider = provider_name.clone();
    identity_config.agent.model = model_name.clone();
    let default_reasoning_effort =
        crate::reasoning_effort::configured_default_reasoning_effort(&identity_config);
    let reasoning_effort_binding = crate::reasoning_effort::reasoning_effort_binding(
        &provider_name,
        &model_name,
        &available_reasoning_efforts,
    );
    Ok(ApplicationRunStartView {
        model_ref: effective_route.model_ref,
        provider_name,
        model_selection_binding: application_model_selection_binding(&route.model_ref),
        default_permission_mode: root_config.permission.mode,
        available_reasoning_efforts,
        default_reasoning_effort,
        reasoning_effort_binding,
        route_recovery,
        requested_model_available: requested_model.is_some_and(|model| {
            crate::provider_connections::resolve_model_route(root_config, model).is_ok()
        }),
        source_model_ref: route.model_ref,
    })
}
