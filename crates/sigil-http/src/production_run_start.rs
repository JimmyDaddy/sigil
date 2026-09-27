//! Narrow run-start routing shared by admission and cancellable preparation.

use super::*;
use anyhow::Context;

/// Runs exact recorded-source reads inside the supervisor's owned preparation future.
/// Its caller must retain/await preparation through cancellation, including deadline failure.
pub(super) async fn task_review_guidance(
    request: &ApplicationRunRequest,
    expected_session_scope_id: &str,
    guidance: Option<String>,
) -> Result<Option<String>> {
    if request.review_annotations.is_empty() {
        return Ok(guidance);
    }
    let config_path = request.config_path.clone();
    let launch_cwd = request.launch_cwd.clone();
    let session_path = request
        .session_path
        .clone()
        .ok_or_else(|| anyhow!("Task review session path is unavailable"))?;
    let scope = expected_session_scope_id.to_owned();
    let annotations = request.review_annotations.clone();
    tokio::task::spawn_blocking(move || {
        let workspace = application_recovery_workspace_root(&config_path, &launch_cwd)?;
        let context = sigil_runtime::materialize_queued_review_annotations(
            &session_path,
            &scope,
            &workspace,
            &annotations,
        )?;
        Ok(Some(format!("{}{context}", guidance.unwrap_or_default())))
    })
    .await
    .context("Task review materialization worker failed")?
}

pub(super) fn http_route_recovery(
    recovery: sigil_runtime::application_run::ApplicationSessionRouteRecoveryView,
) -> crate::HttpSessionRouteRecoveryView {
    use crate::{
        HttpSessionRouteRecoveryAction as HttpAction, HttpSessionRouteRecoveryCode as HttpCode,
    };
    use sigil_runtime::application_run::{
        ApplicationSessionRouteRecoveryAction as RuntimeAction,
        ApplicationSessionRouteRecoveryCode as RuntimeCode,
    };
    crate::HttpSessionRouteRecoveryView {
        code: match recovery.code {
            RuntimeCode::SessionRouteConfirmationRequired => {
                HttpCode::SessionRouteConfirmationRequired
            }
            RuntimeCode::SessionRouteSelectionRequired => HttpCode::SessionRouteSelectionRequired,
            RuntimeCode::ModelRouteNotConfigured => HttpCode::ModelRouteNotConfigured,
            RuntimeCode::ConnectionConfigInvalid => HttpCode::ConnectionConfigInvalid,
            RuntimeCode::ProviderUnavailable => HttpCode::ProviderUnavailable,
            RuntimeCode::AuthorityUnavailable => HttpCode::AuthorityUnavailable,
            RuntimeCode::SessionAlreadyActive => HttpCode::SessionAlreadyActive,
            RuntimeCode::SessionWriterBusy => HttpCode::SessionWriterBusy,
            RuntimeCode::SessionStreamInvalid => HttpCode::SessionStreamInvalid,
        },
        allowed_actions: recovery
            .allowed_actions
            .into_iter()
            .map(|action| match action {
                RuntimeAction::ConfirmCurrentRoute => HttpAction::ConfirmCurrentRoute,
                RuntimeAction::RepairConnection => HttpAction::RepairConnection,
                RuntimeAction::SelectReplacement => HttpAction::SelectReplacement,
                RuntimeAction::StartNewSession => HttpAction::StartNewSession,
                RuntimeAction::RetryProvider => HttpAction::RetryProvider,
                RuntimeAction::RetrySessionAttach => HttpAction::RetrySessionAttach,
                RuntimeAction::BackToSessionLibrary => HttpAction::BackToSessionLibrary,
            })
            .collect(),
        recovery_binding: recovery.recovery_binding,
        retryable: recovery.retryable,
    }
}

pub(super) async fn bind_run_start_route(
    request: &mut ApplicationRunRequest,
    expected_session_scope_id: &str,
) -> Result<()> {
    // Preserve the bound session route even when the global default no longer resolves.
    // The caller keeps this blocking task within its owned preparation/cancellation future.
    let config_path = request.config_path.clone();
    let session_path = request
        .session_path
        .clone()
        .ok_or_else(|| anyhow!("run preparation session path is unavailable"))?;
    let scope_id = expected_session_scope_id.to_owned();
    let context = tokio::task::spawn_blocking(move || {
        application_run_start_view(&config_path, &session_path, &scope_id, None)
    })
    .await
    .context("run-start route projection task failed")??;
    request.model_connection_id = Some(context.model_ref.connection_id);
    request.model_name = Some(context.model_ref.model_id);
    Ok(())
}
