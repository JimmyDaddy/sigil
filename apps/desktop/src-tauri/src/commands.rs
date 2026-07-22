use std::{
    collections::BTreeSet,
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use serde::Serialize;
use sigil_desktop::{
    DesktopApprovalDecision, DesktopApprovalDecisionRequest, DesktopCatalogQuery,
    DesktopClientError, DesktopConversationDisplayQuery, DesktopLaunchError, DesktopLaunchRequest,
    DesktopRunCancelRequest, DesktopRunStartRequest, DesktopSessionCatalogBatchExecuteRequest,
    DesktopSessionCatalogBatchItem, DesktopSessionCatalogBatchPlanRequest,
    DesktopSessionCatalogState, DesktopSessionCreateRequest, DesktopSessionDeleteRequest,
    DesktopSessionInvalidSourceDeleteRequest, DesktopSessionOpenRequest,
    DesktopSessionQuarantineRequest, DesktopSessionRenameRequest, DesktopTranscriptQuery,
    DesktopWorkspaceManagerError, DesktopWorkspaceOpenRequest, DesktopWorkspaceSummary,
};
use tauri::{AppHandle, Emitter, State, WebviewWindow};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;
use thiserror::Error;
use tokio::sync::oneshot;

use crate::{
    appearance::{AppearanceSnapshot, AppearanceStoreError, ResolvedTheme, ThemePreference},
    ipc::{
        DesktopAgentActivitySummary, DesktopAppearanceInput, DesktopApprovalActionInput,
        DesktopApprovalDecisionInput, DesktopApprovalDecisionSummary, DesktopBootstrap,
        DesktopCatalogPage, DesktopCatalogRequest, DesktopCatalogState,
        DesktopConversationContinuity, DesktopConversationDisplayPage,
        DesktopConversationDisplayRequest, DesktopExternalUrlInput, DesktopRunAttachInput,
        DesktopRunAttachment, DesktopRunCancelInput, DesktopRunContext, DesktopRunStartInput,
        DesktopRunSummary, DesktopSessionCatalogBatchExecuteInput,
        DesktopSessionCatalogBatchPlanInput, DesktopSessionCatalogBatchPlanSummary,
        DesktopSessionCatalogBatchReceiptSummary, DesktopSessionCreateInput,
        DesktopSessionDeleteInput, DesktopSessionInvalidSourceDeleteInput,
        DesktopSessionInvalidSourceDeleteSummary, DesktopSessionMutationSummary,
        DesktopSessionOpenInput, DesktopSessionQuarantineInput, DesktopSessionQuarantineSummary,
        DesktopSessionRenameInput, DesktopSessionSummary, DesktopSupportDoctorSummary,
        DesktopSupportSaveSummary, DesktopTranscriptPage, DesktopTranscriptRequest,
        DesktopVerificationRerunInput, DesktopVerificationSummary, DesktopWorkspaceSelection,
    },
    recent::RecentWorkspaceStoreError,
    state::DesktopAppState,
};

const DESKTOP_PROTOCOL_VERSION: u16 = 1;
const MAX_EXTERNAL_URL_BYTES: usize = 2_048;
const MAX_SUPPORT_BUNDLE_BYTES: usize = 256 * 1024;

#[derive(Debug, Error, Serialize)]
#[error("{message}")]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopCommandError {
    pub(crate) code: &'static str,
    pub(crate) message: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) recovery_actions: Vec<DesktopRecoveryAction>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DesktopRecoveryAction {
    RetryCurrent,
    OpenAnotherWorkspace,
    OpenDiagnostics,
    ShowDetails,
}

impl DesktopCommandError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            recovery_actions: Vec::new(),
        }
    }

    fn with_recovery_actions(
        mut self,
        actions: impl IntoIterator<Item = DesktopRecoveryAction>,
    ) -> Self {
        self.recovery_actions = actions
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        self
    }
}

#[tauri::command]
pub(crate) fn desktop_open_external_url(
    app: AppHandle,
    input: DesktopExternalUrlInput,
) -> Result<(), DesktopCommandError> {
    let url = admit_external_https_url(&input.url)?;
    app.opener().open_url(url, None::<&str>).map_err(|_| {
        DesktopCommandError::new(
            "external_url_unavailable",
            "The link could not be opened. Copy it and open it in your browser.",
        )
    })
}

#[tauri::command]
pub(crate) async fn desktop_support_doctor(
    workspace_id: String,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopSupportDoctorSummary, DesktopCommandError> {
    validate_workspace_id(&workspace_id)?;
    let client = state
        .manager
        .lock()
        .await
        .client(&workspace_id)
        .map_err(project_manager_error)?;
    client
        .support_doctor()
        .await
        .map(Into::into)
        .map_err(project_client_error)
}

#[tauri::command]
pub(crate) async fn desktop_export_support_bundle(
    app: AppHandle,
    workspace_id: String,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopSupportSaveSummary, DesktopCommandError> {
    validate_workspace_id(&workspace_id)?;
    let client = state
        .manager
        .lock()
        .await
        .client(&workspace_id)
        .map_err(project_manager_error)?;
    let bundle = client
        .support_bundle()
        .await
        .map_err(project_client_error)?;
    validate_support_bundle(&bundle.suggested_file_name, &bundle.content)?;
    let suggested_file_name = bundle.suggested_file_name.clone();
    let (sender, receiver) = oneshot::channel();
    app.dialog()
        .file()
        .set_file_name(&suggested_file_name)
        .add_filter("JSON", &["json"])
        .save_file(move |selection| {
            let _ = sender.send(selection);
        });
    let Some(selection) = receiver.await.map_err(|_| {
        DesktopCommandError::new(
            "support_save_unavailable",
            "The native save dialog is unavailable.",
        )
    })?
    else {
        return Ok(DesktopSupportSaveSummary {
            cancelled: true,
            file_name: None,
        });
    };
    let destination = selection.into_path().map_err(|_| {
        DesktopCommandError::new(
            "support_save_invalid",
            "The selected support report destination is invalid.",
        )
    })?;
    let file_name = destination
        .file_name()
        .and_then(|value| value.to_str())
        .map(str::to_owned)
        .ok_or_else(|| {
            DesktopCommandError::new(
                "support_save_invalid",
                "The selected support report destination is invalid.",
            )
        })?;
    tokio::task::spawn_blocking(move || {
        write_private_support_bundle(&destination, &bundle.content)
    })
    .await
    .map_err(|_| {
        DesktopCommandError::new(
            "support_save_failed",
            "The private support report could not be saved.",
        )
    })??;
    Ok(DesktopSupportSaveSummary {
        cancelled: false,
        file_name: Some(file_name),
    })
}

fn validate_support_bundle(file_name: &str, content: &str) -> Result<(), DesktopCommandError> {
    if file_name.is_empty()
        || file_name.len() > 160
        || !file_name.starts_with("sigil-support-")
        || !file_name.ends_with(".json")
        || file_name.contains(['/', '\\'])
        || content.is_empty()
        || content.len() > MAX_SUPPORT_BUNDLE_BYTES
    {
        return Err(DesktopCommandError::new(
            "support_bundle_invalid",
            "The workspace server returned an invalid support report.",
        ));
    }
    serde_json::from_str::<serde_json::Value>(content).map_err(|_| {
        DesktopCommandError::new(
            "support_bundle_invalid",
            "The workspace server returned an invalid support report.",
        )
    })?;
    Ok(())
}

fn write_private_support_bundle(
    destination: &Path,
    content: &str,
) -> Result<(), DesktopCommandError> {
    if destination
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err(DesktopCommandError::new(
            "support_save_invalid",
            "A private support report cannot replace a symbolic link.",
        ));
    }
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(destination).map_err(|_| {
        DesktopCommandError::new(
            "support_save_failed",
            "The private support report could not be saved.",
        )
    })?;
    file.write_all(content.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|_| {
            DesktopCommandError::new(
                "support_save_failed",
                "The private support report could not be saved.",
            )
        })
}

fn admit_external_https_url(candidate: &str) -> Result<String, DesktopCommandError> {
    if candidate.len() > MAX_EXTERNAL_URL_BYTES {
        return Err(DesktopCommandError::new(
            "external_url_invalid",
            "Only a bounded HTTPS link can be opened.",
        ));
    }
    let parsed = tauri::Url::parse(candidate).map_err(|_| {
        DesktopCommandError::new(
            "external_url_invalid",
            "Only a valid HTTPS link can be opened.",
        )
    })?;
    if parsed.scheme() != "https"
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err(DesktopCommandError::new(
            "external_url_invalid",
            "Only an HTTPS link without embedded credentials can be opened.",
        ));
    }
    Ok(parsed.to_string())
}

#[tauri::command]
pub(crate) async fn desktop_bootstrap(
    window: WebviewWindow,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopBootstrap, DesktopCommandError> {
    let workspaces = state
        .manager
        .lock()
        .await
        .list()
        .map_err(project_manager_error)?;
    let open_workspace_ids = workspaces
        .iter()
        .map(|workspace| workspace.id.clone())
        .collect::<BTreeSet<_>>();
    let recent_workspaces = state
        .recent_workspaces
        .lock()
        .await
        .list(&open_workspace_ids)
        .await
        .map_err(project_recent_error)?;
    let preference = state
        .appearance
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .preference();
    Ok(DesktopBootstrap {
        protocol_version: DESKTOP_PROTOCOL_VERSION,
        workspaces,
        recent_workspaces,
        appearance: appearance_snapshot(&window, preference),
    })
}

#[tauri::command]
pub(crate) fn desktop_set_appearance(
    window: WebviewWindow,
    input: DesktopAppearanceInput,
    state: State<'_, DesktopAppState>,
) -> Result<AppearanceSnapshot, DesktopCommandError> {
    let mut store = state
        .appearance
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let previous = store.preference();
    window
        .set_theme(input.preference.native_theme())
        .map_err(|_| project_appearance_error(AppearanceStoreError::Unavailable))?;
    let snapshot = appearance_snapshot(&window, input.preference);
    if let Err(error) = store.set(input.preference) {
        let _ = window.set_theme(previous.native_theme());
        return Err(project_appearance_error(error));
    }
    let _ = window.emit(crate::appearance::DESKTOP_APPEARANCE_EVENT_NAME, snapshot);
    Ok(snapshot)
}

fn appearance_snapshot(window: &WebviewWindow, preference: ThemePreference) -> AppearanceSnapshot {
    let resolved_theme = match preference {
        ThemePreference::Light => ResolvedTheme::Light,
        ThemePreference::Dark => ResolvedTheme::Dark,
        ThemePreference::System => window
            .theme()
            .map(ResolvedTheme::from)
            .unwrap_or(ResolvedTheme::Dark),
    };
    AppearanceSnapshot {
        preference,
        resolved_theme,
    }
}

#[tauri::command]
pub(crate) async fn desktop_pick_workspace(
    app: tauri::AppHandle,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopWorkspaceSelection, DesktopCommandError> {
    let (sender, receiver) = oneshot::channel();
    app.dialog().file().pick_folder(move |selection| {
        let _ = sender.send(selection);
    });
    let Some(selection) = receiver.await.map_err(|_| {
        DesktopCommandError::new(
            "workspace_picker_unavailable",
            "The native workspace picker is unavailable.",
        )
        .with_recovery_actions([
            DesktopRecoveryAction::RetryCurrent,
            DesktopRecoveryAction::OpenAnotherWorkspace,
            DesktopRecoveryAction::ShowDetails,
        ])
    })?
    else {
        return Ok(DesktopWorkspaceSelection {
            cancelled: true,
            workspace: None,
        });
    };
    let workspace_root = selection.into_path().map_err(|_| {
        DesktopCommandError::new(
            "workspace_selection_invalid",
            "The selected workspace cannot be opened as a local folder.",
        )
        .with_recovery_actions([
            DesktopRecoveryAction::OpenAnotherWorkspace,
            DesktopRecoveryAction::ShowDetails,
        ])
    })?;
    let display_name = workspace_display_name(&workspace_root)?;
    let config_path = workspace_root.join("sigil.toml");
    let request = DesktopWorkspaceOpenRequest::new(
        DesktopLaunchRequest::new(&state.sigil_binary, config_path, &workspace_root),
        display_name,
    );
    let workspace = state
        .manager
        .lock()
        .await
        .open(request)
        .await
        .map_err(project_manager_error)?;
    state
        .recent_workspaces
        .lock()
        .await
        .upsert(
            workspace.id.clone(),
            workspace.display_name.clone(),
            &workspace_root,
        )
        .await
        .map_err(project_recent_error)?;
    Ok(DesktopWorkspaceSelection {
        cancelled: false,
        workspace: Some(workspace),
    })
}

#[tauri::command]
pub(crate) async fn desktop_open_recent_workspace(
    recent_id: String,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopWorkspaceSummary, DesktopCommandError> {
    if !valid_workspace_id(&recent_id) {
        return Err(DesktopCommandError::new(
            "recent_workspace_id_invalid",
            "The recent workspace identifier is invalid.",
        )
        .with_recovery_actions([
            DesktopRecoveryAction::OpenAnotherWorkspace,
            DesktopRecoveryAction::ShowDetails,
        ]));
    }
    let (workspace_root, display_name) = state
        .recent_workspaces
        .lock()
        .await
        .resolve(&recent_id)
        .await
        .map_err(project_recent_error)?;
    let workspace = state
        .manager
        .lock()
        .await
        .open(DesktopWorkspaceOpenRequest::new(
            DesktopLaunchRequest::new(
                &state.sigil_binary,
                workspace_root.join("sigil.toml"),
                &workspace_root,
            ),
            display_name,
        ))
        .await
        .map_err(project_manager_error)?;
    state
        .recent_workspaces
        .lock()
        .await
        .upsert(
            workspace.id.clone(),
            workspace.display_name.clone(),
            &workspace_root,
        )
        .await
        .map_err(project_recent_error)?;
    Ok(workspace)
}

#[tauri::command]
pub(crate) async fn desktop_close_workspace(
    workspace_id: String,
    confirm_active_runs: Option<bool>,
    state: State<'_, DesktopAppState>,
) -> Result<Vec<DesktopWorkspaceSummary>, DesktopCommandError> {
    if !valid_workspace_id(&workspace_id) {
        return Err(DesktopCommandError::new(
            "workspace_id_invalid",
            "The workspace identifier is invalid.",
        ));
    }
    if confirm_active_runs != Some(true) {
        let client = state.manager.lock().await.client(&workspace_id);
        match client {
            Ok(client) => {
                let sessions = client.list_sessions().await.map_err(|_| {
                    DesktopCommandError::new(
                        "workspace_run_state_unavailable",
                        "Active-run state could not be verified. Confirm before closing the runtime.",
                    )
                })?;
                let active_run_count = sessions
                    .sessions
                    .iter()
                    .filter(|session| session.foreground_run_id.is_some())
                    .count();
                if active_run_count > 0 {
                    return Err(DesktopCommandError::new(
                        "workspace_active_runs",
                        format!(
                            "{active_run_count} active run(s) still belong to this workspace. Confirm before closing the runtime."
                        ),
                    ));
                }
            }
            Err(DesktopWorkspaceManagerError::WorkspaceUnavailable) => {}
            Err(error) => return Err(project_manager_error(error)),
        }
    }
    state.run_streams.stop_workspace(&workspace_id).await;
    let mut manager = state.manager.lock().await;
    manager
        .close(&workspace_id)
        .await
        .map_err(project_manager_error)?;
    manager.list().map_err(project_manager_error)
}

#[tauri::command]
pub(crate) async fn desktop_attach_run(
    app: tauri::AppHandle,
    workspace_id: String,
    input: DesktopRunAttachInput,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopRunAttachment, DesktopCommandError> {
    validate_workspace_id(&workspace_id)?;
    validate_session_id(&input.session_id)?;
    validate_session_id(&input.run_id)?;
    validate_owner_revision(&input.owner_revision)?;
    let client = state
        .manager
        .lock()
        .await
        .client(&workspace_id)
        .map_err(project_manager_error)?;
    let run = client
        .run(&input.run_id)
        .await
        .map_err(project_client_error)?;
    if run.session_id != input.session_id {
        return Err(DesktopCommandError::new(
            "run_session_mismatch",
            "The run does not belong to this conversation.",
        ));
    }
    // Keep this fresh owner probe as the final server read before opening the follower. The
    // opaque revision is valid only for this exact foreground ownership transition.
    let continuity = client
        .continuity(&input.session_id)
        .await
        .map_err(project_client_error)?;
    let Some(owner) = continuity.foreground_owner.as_ref() else {
        return Err(DesktopCommandError::new(
            "run_no_longer_foreground",
            "The run is no longer the active run for this conversation.",
        ));
    };
    if owner.run_id != input.run_id {
        return Err(DesktopCommandError::new(
            "run_no_longer_foreground",
            "The run is no longer the active run for this conversation.",
        ));
    }
    if owner.owner_revision != input.owner_revision {
        return Err(DesktopCommandError::new(
            "run_owner_changed",
            "The active run owner changed. Refresh the conversation before reconnecting.",
        ));
    }
    let projection = state
        .run_streams
        .attach(
            app,
            client,
            workspace_id,
            input.session_id,
            continuity.durable_session_scope_id,
            input.owner_revision,
            run.clone(),
        )
        .await;
    Ok(DesktopRunAttachment {
        run: run.into(),
        events: projection.events,
        stream_state: projection.stream_state,
        stream_message: projection.stream_message,
        has_gap: projection.has_gap,
    })
}

#[tauri::command]
pub(crate) async fn desktop_continuity(
    workspace_id: String,
    session_id: String,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopConversationContinuity, DesktopCommandError> {
    validate_workspace_id(&workspace_id)?;
    validate_session_id(&session_id)?;
    let client = state
        .manager
        .lock()
        .await
        .client(&workspace_id)
        .map_err(project_manager_error)?;
    client
        .continuity(&session_id)
        .await
        .map(Into::into)
        .map_err(project_client_error)
}

#[tauri::command]
pub(crate) async fn desktop_start_run(
    app: tauri::AppHandle,
    workspace_id: String,
    input: DesktopRunStartInput,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopRunSummary, DesktopCommandError> {
    validate_workspace_id(&workspace_id)?;
    validate_session_id(&input.session_id)?;
    validate_prompt(&input.prompt)?;
    let client = state
        .manager
        .lock()
        .await
        .client(&workspace_id)
        .map_err(project_manager_error)?;
    let session = client
        .session(&input.session_id)
        .await
        .map_err(project_client_error)?;
    let receipt = client
        .start_run(
            &input.session_id,
            DesktopRunStartRequest {
                prompt: input.prompt,
                permission_mode: input.permission_mode,
                model_name: input.model_name,
                model_selection_binding: input.model_selection_binding,
                reasoning_effort: input.reasoning_effort,
                reasoning_effort_binding: input.reasoning_effort_binding,
                skill_binding: input.skill_binding.map(|binding| {
                    sigil_desktop::DesktopApplicationSkillBinding {
                        skill_id: binding.skill_id,
                        skill_sha256: binding.skill_sha256,
                        index_fingerprint: binding.index_fingerprint,
                    }
                }),
                agent_binding: input.agent_binding.map(|binding| {
                    sigil_desktop::DesktopApplicationAgentBinding {
                        profile_id: binding.profile_id,
                        snapshot_id: binding.snapshot_id,
                    }
                }),
            },
        )
        .await
        .map_err(project_client_error)?;
    let response = DesktopRunSummary::from(receipt.run.clone());
    if let Some(owner) = receipt
        .foreground_owner
        .filter(|owner| owner.run_id == receipt.run.id)
    {
        state
            .run_streams
            .start(
                app,
                client,
                workspace_id,
                input.session_id,
                session.durable_session_scope_id,
                owner.owner_revision,
                receipt.run,
            )
            .await;
    }
    Ok(response)
}

#[tauri::command]
pub(crate) async fn desktop_run_context(
    workspace_id: String,
    session_id: String,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopRunContext, DesktopCommandError> {
    validate_workspace_id(&workspace_id)?;
    validate_session_id(&session_id)?;
    let client = state
        .manager
        .lock()
        .await
        .client(&workspace_id)
        .map_err(project_manager_error)?;
    client
        .run_context(&session_id)
        .await
        .map(Into::into)
        .map_err(project_client_error)
}

#[tauri::command]
pub(crate) async fn desktop_agent_activity(
    workspace_id: String,
    session_id: String,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopAgentActivitySummary, DesktopCommandError> {
    validate_workspace_id(&workspace_id)?;
    validate_session_id(&session_id)?;
    let client = state
        .manager
        .lock()
        .await
        .client(&workspace_id)
        .map_err(project_manager_error)?;
    client
        .agent_activity(&session_id)
        .await
        .map(Into::into)
        .map_err(project_client_error)
}

#[tauri::command]
pub(crate) async fn desktop_cancel_run(
    workspace_id: String,
    input: DesktopRunCancelInput,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopRunSummary, DesktopCommandError> {
    validate_workspace_id(&workspace_id)?;
    validate_session_id(&input.session_id)?;
    validate_session_id(&input.run_id)?;
    let client = state
        .manager
        .lock()
        .await
        .client(&workspace_id)
        .map_err(project_manager_error)?;
    let snapshot = client
        .run(&input.run_id)
        .await
        .map_err(project_client_error)?;
    if snapshot.session_id != input.session_id {
        return Err(DesktopCommandError::new(
            "run_session_mismatch",
            "The run does not belong to this conversation.",
        ));
    }
    client
        .cancel_run(
            &input.session_id,
            &input.run_id,
            snapshot.stream_sequence,
            DesktopRunCancelRequest {
                reason: Some("Desktop user requested cancellation".to_owned()),
            },
        )
        .await
        .map(|receipt| receipt.run.into())
        .map_err(project_client_error)
}

#[tauri::command]
pub(crate) async fn desktop_resolve_approval(
    workspace_id: String,
    input: DesktopApprovalDecisionInput,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopApprovalDecisionSummary, DesktopCommandError> {
    validate_workspace_id(&workspace_id)?;
    for value in [
        input.session_id.as_str(),
        input.run_id.as_str(),
        input.call_id.as_str(),
        input.approval_request_id.as_str(),
        input.tool_call_hash.as_str(),
        input.policy_version.as_str(),
    ] {
        validate_session_id(value)?;
    }
    let client = state
        .manager
        .lock()
        .await
        .client(&workspace_id)
        .map_err(project_manager_error)?;
    let snapshot = client
        .run(&input.run_id)
        .await
        .map_err(project_client_error)?;
    if snapshot.session_id != input.session_id {
        return Err(DesktopCommandError::new(
            "approval_session_mismatch",
            "The approval does not belong to this conversation.",
        ));
    }
    client
        .resolve_approval(
            &input.session_id,
            &input.run_id,
            &input.call_id,
            snapshot.stream_sequence,
            DesktopApprovalDecisionRequest {
                approval_request_id: input.approval_request_id,
                tool_call_hash: input.tool_call_hash,
                policy_version: input.policy_version,
                expires_at_ms: input.expires_at_ms,
                decision: match input.decision {
                    DesktopApprovalActionInput::ApproveOnce => DesktopApprovalDecision::Approve,
                    DesktopApprovalActionInput::ApproveSession => {
                        DesktopApprovalDecision::ApproveForSession
                    }
                    DesktopApprovalActionInput::Deny => DesktopApprovalDecision::Deny,
                },
                reason: Some(
                    match input.decision {
                        DesktopApprovalActionInput::ApproveOnce => "Approved once in Sigil Desktop",
                        DesktopApprovalActionInput::ApproveSession => {
                            "Approved for this session in Sigil Desktop"
                        }
                        DesktopApprovalActionInput::Deny => "Denied in Sigil Desktop",
                    }
                    .to_owned(),
                ),
            },
        )
        .await
        .map(|receipt| receipt.decision.into())
        .map_err(project_client_error)
}

#[tauri::command]
pub(crate) async fn desktop_verification(
    workspace_id: String,
    session_id: String,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopVerificationSummary, DesktopCommandError> {
    validate_workspace_id(&workspace_id)?;
    validate_session_id(&session_id)?;
    let client = state
        .manager
        .lock()
        .await
        .client(&workspace_id)
        .map_err(project_manager_error)?;
    client
        .verification(&session_id)
        .await
        .map(Into::into)
        .map_err(project_client_error)
}

#[tauri::command]
pub(crate) async fn desktop_rerun_verification(
    workspace_id: String,
    input: DesktopVerificationRerunInput,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopVerificationSummary, DesktopCommandError> {
    validate_workspace_id(&workspace_id)?;
    validate_session_id(&input.session_id)?;
    validate_verification_rerun(&input)?;
    let client = state
        .manager
        .lock()
        .await
        .client(&workspace_id)
        .map_err(project_manager_error)?;
    client
        .rerun_verification(&input.session_id, input.request.into())
        .await
        .map(|receipt| receipt.verification.into())
        .map_err(project_client_error)
}

#[tauri::command]
pub(crate) async fn desktop_catalog(
    workspace_id: String,
    request: DesktopCatalogRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopCatalogPage, DesktopCommandError> {
    validate_workspace_id(&workspace_id)?;
    let query = validate_catalog_request(request)?;
    let client = state
        .manager
        .lock()
        .await
        .client(&workspace_id)
        .map_err(project_manager_error)?;
    client
        .catalog(&query)
        .await
        .map(Into::into)
        .map_err(project_client_error)
}

#[tauri::command]
pub(crate) async fn desktop_plan_session_catalog_batch(
    workspace_id: String,
    input: DesktopSessionCatalogBatchPlanInput,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopSessionCatalogBatchPlanSummary, DesktopCommandError> {
    validate_workspace_id(&workspace_id)?;
    let items = validate_batch_items(input.action, input.items)?;
    let client = state
        .manager
        .lock()
        .await
        .client(&workspace_id)
        .map_err(project_manager_error)?;
    client
        .plan_session_catalog_batch(DesktopSessionCatalogBatchPlanRequest {
            action: input.action,
            items,
        })
        .await
        .map(Into::into)
        .map_err(project_session_mutation_client_error)
}

#[tauri::command]
pub(crate) async fn desktop_execute_session_catalog_batch(
    workspace_id: String,
    input: DesktopSessionCatalogBatchExecuteInput,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopSessionCatalogBatchReceiptSummary, DesktopCommandError> {
    validate_workspace_id(&workspace_id)?;
    if input.plan_id.is_empty() || input.plan_id.len() > 128 {
        return Err(DesktopCommandError::new(
            "session_batch_plan_invalid",
            "The conversation batch preview is invalid or stale.",
        ));
    }
    let items = validate_batch_items(input.action, input.items)?;
    let client = state
        .manager
        .lock()
        .await
        .client(&workspace_id)
        .map_err(project_manager_error)?;
    client
        .execute_session_catalog_batch(DesktopSessionCatalogBatchExecuteRequest {
            plan_id: input.plan_id,
            action: input.action,
            items,
        })
        .await
        .map(Into::into)
        .map_err(project_session_mutation_client_error)
}

#[tauri::command]
pub(crate) async fn desktop_create_session(
    workspace_id: String,
    input: DesktopSessionCreateInput,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopSessionSummary, DesktopCommandError> {
    validate_workspace_id(&workspace_id)?;
    validate_optional_label(input.label.as_deref())?;
    validate_optional_model(input.model_name.as_deref())?;
    let client = state
        .manager
        .lock()
        .await
        .client(&workspace_id)
        .map_err(project_manager_error)?;
    client
        .create_session(DesktopSessionCreateRequest {
            label: input.label,
            model_name: input.model_name,
        })
        .await
        .map(Into::into)
        .map_err(project_client_error)
}

#[tauri::command]
pub(crate) async fn desktop_open_session(
    workspace_id: String,
    input: DesktopSessionOpenInput,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopSessionSummary, DesktopCommandError> {
    validate_workspace_id(&workspace_id)?;
    validate_session_reference(&input.session_ref, &input.session_id)?;
    validate_optional_label(input.label.as_deref())?;
    let client = state
        .manager
        .lock()
        .await
        .client(&workspace_id)
        .map_err(project_manager_error)?;
    client
        .open_session(DesktopSessionOpenRequest {
            session_ref: input.session_ref,
            session_id: input.session_id,
            label: input.label,
        })
        .await
        .map(Into::into)
        .map_err(project_client_error)
}

#[tauri::command]
pub(crate) async fn desktop_rename_session(
    workspace_id: String,
    input: DesktopSessionRenameInput,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopSessionMutationSummary, DesktopCommandError> {
    validate_workspace_id(&workspace_id)?;
    validate_session_reference(&input.session_ref, &input.session_id)?;
    validate_display_name(&input.display_name)?;
    let client = state
        .manager
        .lock()
        .await
        .client(&workspace_id)
        .map_err(project_manager_error)?;
    client
        .rename_session(DesktopSessionRenameRequest {
            session_ref: input.session_ref,
            session_id: input.session_id,
            display_name: input.display_name,
        })
        .await
        .map(|receipt| DesktopSessionMutationSummary {
            session_ref: receipt.session_ref,
            session_id: receipt.session_id,
            projection_generation: receipt.projection_generation,
        })
        .map_err(project_session_mutation_client_error)
}

#[tauri::command]
pub(crate) async fn desktop_delete_session(
    workspace_id: String,
    input: DesktopSessionDeleteInput,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopSessionMutationSummary, DesktopCommandError> {
    validate_workspace_id(&workspace_id)?;
    validate_session_reference(&input.session_ref, &input.session_id)?;
    let client = state
        .manager
        .lock()
        .await
        .client(&workspace_id)
        .map_err(project_manager_error)?;
    client
        .delete_session(DesktopSessionDeleteRequest {
            session_ref: input.session_ref,
            session_id: input.session_id,
        })
        .await
        .map(|receipt| DesktopSessionMutationSummary {
            session_ref: receipt.session_ref,
            session_id: receipt.session_id,
            projection_generation: receipt.projection_generation,
        })
        .map_err(project_session_mutation_client_error)
}

#[tauri::command]
pub(crate) async fn desktop_quarantine_session(
    workspace_id: String,
    input: DesktopSessionQuarantineInput,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopSessionQuarantineSummary, DesktopCommandError> {
    validate_workspace_id(&workspace_id)?;
    validate_session_ref(&input.session_ref)?;
    let client = state
        .manager
        .lock()
        .await
        .client(&workspace_id)
        .map_err(project_manager_error)?;
    client
        .quarantine_session(DesktopSessionQuarantineRequest {
            session_ref: input.session_ref,
            source_bytes: input.source_bytes,
            source_modified_at_unix_ms: input.source_modified_at_unix_ms,
        })
        .await
        .map(|receipt| DesktopSessionQuarantineSummary {
            session_ref: receipt.session_ref,
            quarantine_name: receipt.quarantine_name,
            projection_generation: receipt.projection_generation,
        })
        .map_err(project_session_mutation_client_error)
}

#[tauri::command]
pub(crate) async fn desktop_delete_invalid_session_source(
    workspace_id: String,
    input: DesktopSessionInvalidSourceDeleteInput,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopSessionInvalidSourceDeleteSummary, DesktopCommandError> {
    validate_workspace_id(&workspace_id)?;
    validate_session_ref(&input.session_ref)?;
    let client = state
        .manager
        .lock()
        .await
        .client(&workspace_id)
        .map_err(project_manager_error)?;
    client
        .delete_invalid_source(DesktopSessionInvalidSourceDeleteRequest {
            session_ref: input.session_ref,
            source_bytes: input.source_bytes,
            source_modified_at_unix_ms: input.source_modified_at_unix_ms,
        })
        .await
        .map(|receipt| DesktopSessionInvalidSourceDeleteSummary {
            session_ref: receipt.session_ref,
            projection_generation: receipt.projection_generation,
        })
        .map_err(project_session_mutation_client_error)
}

#[tauri::command]
pub(crate) async fn desktop_transcript(
    workspace_id: String,
    session_id: String,
    request: DesktopTranscriptRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopTranscriptPage, DesktopCommandError> {
    validate_workspace_id(&workspace_id)?;
    validate_session_id(&session_id)?;
    if request.before == Some(0)
        || request
            .limit
            .is_some_and(|limit| !(1..=100).contains(&limit))
    {
        return Err(DesktopCommandError::new(
            "transcript_query_invalid",
            "The conversation history query is invalid.",
        ));
    }
    let client = state
        .manager
        .lock()
        .await
        .client(&workspace_id)
        .map_err(project_manager_error)?;
    client
        .transcript(
            &session_id,
            &DesktopTranscriptQuery {
                before: request.before,
                limit: request.limit,
            },
        )
        .await
        .map(Into::into)
        .map_err(project_client_error)
}

#[tauri::command]
pub(crate) async fn desktop_display(
    workspace_id: String,
    session_id: String,
    request: DesktopConversationDisplayRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopConversationDisplayPage, DesktopCommandError> {
    validate_workspace_id(&workspace_id)?;
    validate_session_id(&session_id)?;
    validate_conversation_display_request(&request)?;
    let client = state
        .manager
        .lock()
        .await
        .client(&workspace_id)
        .map_err(project_manager_error)?;
    client
        .conversation_display(
            &session_id,
            &DesktopConversationDisplayQuery {
                cursor: request.cursor,
                limit: request.limit,
            },
        )
        .await
        .map(Into::into)
        .map_err(project_conversation_display_client_error)
}

fn validate_conversation_display_request(
    request: &DesktopConversationDisplayRequest,
) -> Result<(), DesktopCommandError> {
    if request
        .limit
        .is_some_and(|limit| !(1..=100).contains(&limit))
        || request.cursor.as_deref().is_some_and(|cursor| {
            cursor.is_empty() || cursor.len() > 4_096 || cursor.chars().any(char::is_control)
        })
    {
        return Err(DesktopCommandError::new(
            "conversation_display_query_invalid",
            "The conversation history query is invalid.",
        ));
    }
    Ok(())
}

fn validate_catalog_request(
    request: DesktopCatalogRequest,
) -> Result<DesktopCatalogQuery, DesktopCommandError> {
    if request
        .limit
        .is_some_and(|limit| !(1..=100).contains(&limit))
        || request
            .cursor
            .as_ref()
            .is_some_and(|value| value.len() > 4096)
        || request
            .query
            .as_ref()
            .is_some_and(|value| value.len() > 200)
        || request
            .provider
            .as_ref()
            .is_some_and(|value| value.len() > 120)
    {
        return Err(DesktopCommandError::new(
            "catalog_query_invalid",
            "The history query is invalid.",
        ));
    }
    Ok(DesktopCatalogQuery {
        limit: request.limit,
        cursor: request.cursor,
        query: request.query,
        provider: request.provider,
        pinned: request.pinned,
        state: request.state.map(Into::into),
    })
}

fn validate_batch_items(
    action: sigil_desktop::DesktopSessionCatalogBatchAction,
    items: Vec<crate::ipc::DesktopSessionCatalogBatchItemInput>,
) -> Result<Vec<DesktopSessionCatalogBatchItem>, DesktopCommandError> {
    if items.is_empty() || items.len() > 100 {
        return Err(DesktopCommandError::new(
            "session_batch_invalid",
            "Select between 1 and 100 conversations.",
        ));
    }
    items
        .into_iter()
        .map(|item| {
            validate_session_ref(&item.session_ref)?;
            let valid_shape = match action {
                sigil_desktop::DesktopSessionCatalogBatchAction::DeleteSessions => {
                    item.session_id
                        .as_deref()
                        .is_some_and(|session_id| validate_session_id(session_id).is_ok())
                        && item.source_bytes.is_none()
                        && item.source_modified_at_unix_ms.is_none()
                }
                sigil_desktop::DesktopSessionCatalogBatchAction::QuarantineInvalidSources
                | sigil_desktop::DesktopSessionCatalogBatchAction::DeleteInvalidSources => {
                    item.session_id.is_none()
                        && item.source_bytes.is_some()
                        && item.source_modified_at_unix_ms.is_some()
                }
            };
            if !valid_shape {
                return Err(DesktopCommandError::new(
                    "session_batch_invalid",
                    "The selected conversations no longer match this batch action.",
                ));
            }
            Ok(DesktopSessionCatalogBatchItem {
                session_ref: item.session_ref,
                session_id: item.session_id,
                source_bytes: item.source_bytes,
                source_modified_at_unix_ms: item.source_modified_at_unix_ms,
            })
        })
        .collect()
}

fn validate_workspace_id(value: &str) -> Result<(), DesktopCommandError> {
    if !valid_workspace_id(value) {
        return Err(DesktopCommandError::new(
            "workspace_id_invalid",
            "The workspace identifier is invalid.",
        ));
    }
    Ok(())
}

fn validate_optional_label(value: Option<&str>) -> Result<(), DesktopCommandError> {
    if value.is_some_and(|label| label.len() > 160 || label.chars().any(char::is_control)) {
        return Err(DesktopCommandError::new(
            "session_label_invalid",
            "The conversation label is invalid.",
        ));
    }
    Ok(())
}

fn validate_optional_model(value: Option<&str>) -> Result<(), DesktopCommandError> {
    if value.is_some_and(|model| {
        model.is_empty()
            || model.trim() != model
            || model.len() > 160
            || model.chars().any(char::is_control)
    }) {
        return Err(DesktopCommandError::new(
            "session_model_invalid",
            "The selected model is invalid.",
        ));
    }
    Ok(())
}

fn validate_display_name(value: &str) -> Result<(), DesktopCommandError> {
    if value.is_empty()
        || value.trim() != value
        || value.len() > 160
        || value.chars().any(char::is_control)
    {
        return Err(DesktopCommandError::new(
            "session_display_name_invalid",
            "Enter a conversation name between 1 and 160 characters.",
        ));
    }
    Ok(())
}

fn validate_session_reference(
    session_ref: &str,
    session_id: &str,
) -> Result<(), DesktopCommandError> {
    validate_session_ref(session_ref)?;
    if session_id.is_empty() || session_id.len() > 512 || session_id.chars().any(char::is_control) {
        return Err(DesktopCommandError::new(
            "session_reference_invalid",
            "The conversation reference is invalid.",
        ));
    }
    Ok(())
}

fn validate_session_ref(session_ref: &str) -> Result<(), DesktopCommandError> {
    if session_ref.is_empty()
        || session_ref.len() > 128
        || session_ref.contains('/')
        || session_ref.contains('\\')
        || !session_ref.ends_with(".jsonl")
        || session_ref.chars().any(char::is_control)
    {
        return Err(DesktopCommandError::new(
            "session_reference_invalid",
            "The conversation reference is invalid.",
        ));
    }
    Ok(())
}

fn validate_session_id(value: &str) -> Result<(), DesktopCommandError> {
    if value.is_empty() || value.len() > 512 || value.chars().any(char::is_control) {
        return Err(DesktopCommandError::new(
            "session_id_invalid",
            "The conversation identifier is invalid.",
        ));
    }
    Ok(())
}

fn validate_owner_revision(value: &str) -> Result<(), DesktopCommandError> {
    let Some(hash) = value.strip_prefix("sha256:") else {
        return Err(DesktopCommandError::new(
            "run_owner_revision_invalid",
            "The active run owner revision is invalid.",
        ));
    };
    if hash.len() != 64
        || !hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(DesktopCommandError::new(
            "run_owner_revision_invalid",
            "The active run owner revision is invalid.",
        ));
    }
    Ok(())
}

fn validate_prompt(value: &str) -> Result<(), DesktopCommandError> {
    if value.trim().is_empty()
        || value.len() > 256 * 1024
        || value.chars().any(|character| character == '\0')
    {
        return Err(DesktopCommandError::new(
            "run_prompt_invalid",
            "The prompt is empty or exceeds the desktop input limit.",
        ));
    }
    Ok(())
}

fn validate_verification_rerun(
    input: &DesktopVerificationRerunInput,
) -> Result<(), DesktopCommandError> {
    for value in [
        input.request.task_id.as_str(),
        input.request.step_id.as_str(),
        input.request.check_spec_id.as_str(),
        input.request.check_spec_hash.as_str(),
        input.request.policy_hash.as_str(),
        input.request.workspace_snapshot_id.as_str(),
    ] {
        if value.is_empty() || value.len() > 512 || value.chars().any(char::is_control) {
            return Err(DesktopCommandError::new(
                "verification_request_invalid",
                "The verification recommendation is invalid or stale.",
            ));
        }
    }
    Ok(())
}

fn workspace_display_name(path: &Path) -> Result<String, DesktopCommandError> {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty() && name.len() <= 160)
        .map(str::to_owned)
        .ok_or_else(|| {
            DesktopCommandError::new(
                "workspace_name_invalid",
                "The selected workspace does not have a usable display name.",
            )
            .with_recovery_actions([
                DesktopRecoveryAction::OpenAnotherWorkspace,
                DesktopRecoveryAction::ShowDetails,
            ])
        })
}

fn valid_workspace_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 512
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

pub(crate) fn resolve_sigil_binary() -> Result<PathBuf, DesktopCommandError> {
    #[cfg(debug_assertions)]
    if let Some(path) = std::env::var_os("SIGIL_DESKTOP_SIGIL_BINARY") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
    }

    let executable = std::env::current_exe().map_err(|_| {
        DesktopCommandError::new(
            "sigil_binary_unavailable",
            "The bundled Sigil runtime cannot be located.",
        )
        .with_recovery_actions([DesktopRecoveryAction::ShowDetails])
    })?;
    let parent = executable.parent().ok_or_else(|| {
        DesktopCommandError::new(
            "sigil_binary_unavailable",
            "The bundled Sigil runtime cannot be located.",
        )
        .with_recovery_actions([DesktopRecoveryAction::ShowDetails])
    })?;
    if let Some(binary) = resolve_sigil_binary_from_directory(parent, cfg!(debug_assertions)) {
        return Ok(binary);
    }
    Err(DesktopCommandError::new(
        "sigil_binary_unavailable",
        "The bundled Sigil runtime cannot be located.",
    )
    .with_recovery_actions([DesktopRecoveryAction::ShowDetails]))
}

fn resolve_sigil_binary_from_directory(parent: &Path, prefer_developer: bool) -> Option<PathBuf> {
    let developer = parent.join(if cfg!(windows) { "sigil.exe" } else { "sigil" });
    if prefer_developer && developer.is_file() {
        return Some(developer);
    }
    let bundled = parent.join(bundled_sigil_binary_name());
    bundled.is_file().then_some(bundled)
}

const fn bundled_sigil_binary_name() -> &'static str {
    if cfg!(windows) {
        "sigil-runtime.exe"
    } else {
        "sigil-runtime"
    }
}

fn project_manager_error(error: DesktopWorkspaceManagerError) -> DesktopCommandError {
    match error {
        DesktopWorkspaceManagerError::InvalidWorkspace
        | DesktopWorkspaceManagerError::InvalidDisplayName => DesktopCommandError::new(
            "workspace_invalid",
            "The selected workspace is not available.",
        )
        .with_recovery_actions([
            DesktopRecoveryAction::OpenAnotherWorkspace,
            DesktopRecoveryAction::ShowDetails,
        ]),
        DesktopWorkspaceManagerError::UnknownWorkspace => {
            DesktopCommandError::new("workspace_not_open", "The workspace is no longer open.")
                .with_recovery_actions([
                    DesktopRecoveryAction::OpenAnotherWorkspace,
                    DesktopRecoveryAction::ShowDetails,
                ])
        }
        DesktopWorkspaceManagerError::WorkspaceUnavailable
        | DesktopWorkspaceManagerError::ProcessStatusUnavailable => DesktopCommandError::new(
            "workspace_server_unavailable",
            "The workspace server is unavailable.",
        )
        .with_recovery_actions([
            DesktopRecoveryAction::RetryCurrent,
            DesktopRecoveryAction::OpenAnotherWorkspace,
            DesktopRecoveryAction::ShowDetails,
        ]),
        DesktopWorkspaceManagerError::IdentityCollision => DesktopCommandError::new(
            "workspace_identity_collision",
            "The workspace identity conflicts with another open workspace.",
        )
        .with_recovery_actions([
            DesktopRecoveryAction::OpenAnotherWorkspace,
            DesktopRecoveryAction::ShowDetails,
        ]),
        DesktopWorkspaceManagerError::Launch(DesktopLaunchError::IncompatibleServer(_)) => {
            DesktopCommandError::new(
                "workspace_server_incompatible",
                "The desktop runtime is out of sync. Restart the development app or rebuild the package.",
            )
            .with_recovery_actions([
                DesktopRecoveryAction::OpenAnotherWorkspace,
                DesktopRecoveryAction::ShowDetails,
            ])
        }
        DesktopWorkspaceManagerError::Launch(_) => DesktopCommandError::new(
            "workspace_server_start_failed",
            "The workspace server could not be started. Confirm that sigil.toml is present and valid.",
        )
        .with_recovery_actions([
            DesktopRecoveryAction::RetryCurrent,
            DesktopRecoveryAction::OpenAnotherWorkspace,
            DesktopRecoveryAction::ShowDetails,
        ]),
        DesktopWorkspaceManagerError::Shutdown(_) => DesktopCommandError::new(
            "workspace_server_stop_failed",
            "The workspace server could not be stopped cleanly.",
        )
        .with_recovery_actions([
            DesktopRecoveryAction::RetryCurrent,
            DesktopRecoveryAction::OpenAnotherWorkspace,
            DesktopRecoveryAction::ShowDetails,
        ]),
        DesktopWorkspaceManagerError::Client(_) => DesktopCommandError::new(
            "workspace_server_request_failed",
            "The workspace server request failed.",
        )
        .with_recovery_actions([
            DesktopRecoveryAction::RetryCurrent,
            DesktopRecoveryAction::OpenAnotherWorkspace,
            DesktopRecoveryAction::ShowDetails,
        ]),
    }
}

fn project_client_error(error: DesktopClientError) -> DesktopCommandError {
    match error {
        DesktopClientError::Rejected {
            code: Some(code), ..
        } if code == "stale_cursor" => DesktopCommandError::new(
            "catalog_stale",
            "History changed while loading more. Refresh from the first page.",
        )
        .with_recovery_actions([
            DesktopRecoveryAction::RetryCurrent,
            DesktopRecoveryAction::ShowDetails,
        ]),
        DesktopClientError::Rejected { .. } => DesktopCommandError::new(
            "workspace_request_rejected",
            "The workspace server rejected the request.",
        )
        .with_recovery_actions([DesktopRecoveryAction::ShowDetails]),
        DesktopClientError::InvalidRoute
        | DesktopClientError::InvalidResponse
        | DesktopClientError::InvalidEventStream
        | DesktopClientError::ProtocolEvent(_) => DesktopCommandError::new(
            "workspace_protocol_invalid",
            "The workspace server returned an incompatible response.",
        )
        .with_recovery_actions([
            DesktopRecoveryAction::OpenDiagnostics,
            DesktopRecoveryAction::ShowDetails,
        ]),
        DesktopClientError::RequestFailed
        | DesktopClientError::ResponseTooLarge
        | DesktopClientError::EventStreamGap => DesktopCommandError::new(
            "workspace_request_failed",
            "The workspace server request failed.",
        )
        .with_recovery_actions([
            DesktopRecoveryAction::RetryCurrent,
            DesktopRecoveryAction::OpenDiagnostics,
            DesktopRecoveryAction::ShowDetails,
        ]),
    }
}

fn project_conversation_display_client_error(error: DesktopClientError) -> DesktopCommandError {
    match error {
        DesktopClientError::Rejected {
            code: Some(code), ..
        } if code == "invalid_display_cursor" => DesktopCommandError::new(
            "conversation_display_cursor_invalid",
            "The conversation history cursor is invalid. Refresh from the latest page.",
        )
        .with_recovery_actions([DesktopRecoveryAction::ShowDetails]),
        DesktopClientError::Rejected {
            code: Some(code), ..
        } if code == "display_cursor_stale" => DesktopCommandError::new(
            "conversation_display_stale",
            "Conversation history changed while loading more. Refresh from the latest page.",
        )
        .with_recovery_actions([DesktopRecoveryAction::ShowDetails]),
        DesktopClientError::Rejected {
            code: Some(code), ..
        } if code == "conversation_display_unavailable" => DesktopCommandError::new(
            "conversation_display_unavailable",
            "Canonical conversation history is temporarily unavailable.",
        )
        .with_recovery_actions([
            DesktopRecoveryAction::RetryCurrent,
            DesktopRecoveryAction::OpenDiagnostics,
            DesktopRecoveryAction::ShowDetails,
        ]),
        other => project_client_error(other),
    }
}

fn project_session_mutation_client_error(error: DesktopClientError) -> DesktopCommandError {
    if let DesktopClientError::Rejected {
        code: Some(code), ..
    } = &error
    {
        return match code.as_str() {
            "invalid_session_mutation_request" => DesktopCommandError::new(
                "session_mutation_invalid",
                "The conversation change is invalid.",
            ),
            "durable_session_not_found" => DesktopCommandError::new(
                "session_not_found",
                "This conversation no longer exists. Refresh the list.",
            ),
            "durable_session_identity_changed" => DesktopCommandError::new(
                "session_changed",
                "This conversation changed. Refresh the list and try again.",
            ),
            "durable_session_not_ready" => DesktopCommandError::new(
                "session_not_ready",
                "This conversation is not ready for that change.",
            ),
            "durable_session_pinned" => DesktopCommandError::new(
                "session_pinned",
                "Unpin this conversation before deleting it.",
            ),
            "registry_error" => DesktopCommandError::new(
                "session_busy",
                "Wait for the active run or verification to finish, then try again.",
            ),
            _ => project_client_error(error),
        };
    }
    project_client_error(error)
}

fn project_recent_error(error: RecentWorkspaceStoreError) -> DesktopCommandError {
    match error {
        RecentWorkspaceStoreError::UnknownWorkspace => DesktopCommandError::new(
            "recent_workspace_unknown",
            "The recent workspace is no longer available.",
        )
        .with_recovery_actions([
            DesktopRecoveryAction::OpenAnotherWorkspace,
            DesktopRecoveryAction::ShowDetails,
        ]),
        RecentWorkspaceStoreError::InvalidFile | RecentWorkspaceStoreError::InvalidRecord => {
            DesktopCommandError::new(
                "recent_workspaces_invalid",
                "The recent workspace list is invalid.",
            )
            .with_recovery_actions([
                DesktopRecoveryAction::OpenAnotherWorkspace,
                DesktopRecoveryAction::ShowDetails,
            ])
        }
        RecentWorkspaceStoreError::Unavailable => DesktopCommandError::new(
            "recent_workspaces_unavailable",
            "The recent workspace list is unavailable.",
        )
        .with_recovery_actions([
            DesktopRecoveryAction::RetryCurrent,
            DesktopRecoveryAction::OpenAnotherWorkspace,
            DesktopRecoveryAction::ShowDetails,
        ]),
    }
}

fn project_appearance_error(_error: AppearanceStoreError) -> DesktopCommandError {
    DesktopCommandError::new(
        "appearance_unavailable",
        "The appearance preference could not be saved. The previous theme is still active.",
    )
}

impl From<DesktopCatalogState> for DesktopSessionCatalogState {
    fn from(value: DesktopCatalogState) -> Self {
        match value {
            DesktopCatalogState::Ready => Self::Ready,
            DesktopCatalogState::Oversized => Self::Oversized,
            DesktopCatalogState::ScanBudgetExceeded => Self::ScanBudgetExceeded,
            DesktopCatalogState::UnsupportedLegacy => Self::UnsupportedLegacy,
            DesktopCatalogState::Invalid => Self::Invalid,
        }
    }
}

#[cfg(test)]
#[path = "tests/commands_tests.rs"]
mod tests;
