use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use serde::Serialize;
use sigil_desktop::{
    DesktopApprovalDecision, DesktopApprovalDecisionRequest, DesktopCatalogQuery,
    DesktopClientError, DesktopLaunchError, DesktopLaunchRequest, DesktopRunCancelRequest,
    DesktopRunStartRequest, DesktopSessionCatalogState, DesktopSessionCreateRequest,
    DesktopSessionOpenRequest, DesktopTranscriptQuery, DesktopWorkspaceManagerError,
    DesktopWorkspaceOpenRequest, DesktopWorkspaceSummary,
};
use tauri::{Emitter, State, WebviewWindow};
use tauri_plugin_dialog::DialogExt;
use thiserror::Error;
use tokio::sync::oneshot;

use crate::{
    appearance::{AppearanceSnapshot, AppearanceStoreError, ResolvedTheme, ThemePreference},
    ipc::{
        DesktopAppearanceInput, DesktopApprovalDecisionInput, DesktopApprovalDecisionSummary,
        DesktopBootstrap, DesktopCatalogPage, DesktopCatalogRequest, DesktopCatalogState,
        DesktopRunAttachInput, DesktopRunAttachment, DesktopRunCancelInput, DesktopRunContext,
        DesktopRunStartInput, DesktopRunSummary, DesktopSessionCreateInput,
        DesktopSessionOpenInput, DesktopSessionSummary, DesktopTranscriptPage,
        DesktopTranscriptRequest, DesktopVerificationRerunInput, DesktopVerificationSummary,
        DesktopWorkspaceSelection,
    },
    recent::RecentWorkspaceStoreError,
    state::DesktopAppState,
};

const DESKTOP_PROTOCOL_VERSION: u16 = 1;

#[derive(Debug, Error, Serialize)]
#[error("{message}")]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopCommandError {
    pub(crate) code: &'static str,
    pub(crate) message: String,
}

impl DesktopCommandError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
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
        ));
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
    if session.foreground_run_id.as_deref() != Some(input.run_id.as_str()) {
        return Err(DesktopCommandError::new(
            "run_no_longer_foreground",
            "The run is no longer the active run for this conversation.",
        ));
    }
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
    let projection = state
        .run_streams
        .attach(
            app,
            client,
            workspace_id,
            input.session_id,
            session.durable_session_scope_id,
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
                approval_mode: input.approval_mode,
            },
        )
        .await
        .map_err(project_client_error)?;
    let response = DesktopRunSummary::from(receipt.run.clone());
    state
        .run_streams
        .start(
            app,
            client,
            workspace_id,
            input.session_id,
            session.durable_session_scope_id,
            receipt.run,
        )
        .await;
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
                decision: if input.approve {
                    DesktopApprovalDecision::Approve
                } else {
                    DesktopApprovalDecision::Deny
                },
                reason: Some(
                    if input.approve {
                        "Approved in Sigil Desktop"
                    } else {
                        "Denied in Sigil Desktop"
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
pub(crate) async fn desktop_create_session(
    workspace_id: String,
    input: DesktopSessionCreateInput,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopSessionSummary, DesktopCommandError> {
    validate_workspace_id(&workspace_id)?;
    validate_optional_label(input.label.as_deref())?;
    let client = state
        .manager
        .lock()
        .await
        .client(&workspace_id)
        .map_err(project_manager_error)?;
    client
        .create_session(DesktopSessionCreateRequest { label: input.label })
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

fn validate_session_reference(
    session_ref: &str,
    session_id: &str,
) -> Result<(), DesktopCommandError> {
    if session_ref.is_empty()
        || session_ref.len() > 512
        || session_ref.contains('/')
        || session_ref.contains('\\')
        || session_id.is_empty()
        || session_id.len() > 512
        || session_id.chars().any(char::is_control)
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
    })?;
    let parent = executable.parent().ok_or_else(|| {
        DesktopCommandError::new(
            "sigil_binary_unavailable",
            "The bundled Sigil runtime cannot be located.",
        )
    })?;
    if let Some(binary) = resolve_sigil_binary_from_directory(parent, cfg!(debug_assertions)) {
        return Ok(binary);
    }
    Err(DesktopCommandError::new(
        "sigil_binary_unavailable",
        "The bundled Sigil runtime cannot be located.",
    ))
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
        ),
        DesktopWorkspaceManagerError::UnknownWorkspace => {
            DesktopCommandError::new("workspace_not_open", "The workspace is no longer open.")
        }
        DesktopWorkspaceManagerError::WorkspaceUnavailable
        | DesktopWorkspaceManagerError::ProcessStatusUnavailable => DesktopCommandError::new(
            "workspace_server_unavailable",
            "The workspace server is unavailable.",
        ),
        DesktopWorkspaceManagerError::IdentityCollision => DesktopCommandError::new(
            "workspace_identity_collision",
            "The workspace identity conflicts with another open workspace.",
        ),
        DesktopWorkspaceManagerError::Launch(DesktopLaunchError::IncompatibleServer(_)) => {
            DesktopCommandError::new(
                "workspace_server_incompatible",
                "The desktop runtime is out of sync. Restart the development app or rebuild the package.",
            )
        }
        DesktopWorkspaceManagerError::Launch(_) => DesktopCommandError::new(
            "workspace_server_start_failed",
            "The workspace server could not be started. Confirm that sigil.toml is present and valid.",
        ),
        DesktopWorkspaceManagerError::Shutdown(_) => DesktopCommandError::new(
            "workspace_server_stop_failed",
            "The workspace server could not be stopped cleanly.",
        ),
        DesktopWorkspaceManagerError::Client(_) => DesktopCommandError::new(
            "workspace_server_request_failed",
            "The workspace server request failed.",
        ),
    }
}

fn project_client_error(error: DesktopClientError) -> DesktopCommandError {
    match error {
        DesktopClientError::Rejected {
            code: Some(code), ..
        } if code == "stale_cursor" => DesktopCommandError::new(
            "catalog_stale",
            "History changed while loading more. Refresh from the first page.",
        ),
        DesktopClientError::Rejected { .. } => DesktopCommandError::new(
            "workspace_request_rejected",
            "The workspace server rejected the request.",
        ),
        DesktopClientError::InvalidRoute
        | DesktopClientError::InvalidResponse
        | DesktopClientError::InvalidEventStream
        | DesktopClientError::ProtocolEvent(_) => DesktopCommandError::new(
            "workspace_protocol_invalid",
            "The workspace server returned an incompatible response.",
        ),
        DesktopClientError::RequestFailed
        | DesktopClientError::ResponseTooLarge
        | DesktopClientError::EventStreamGap => DesktopCommandError::new(
            "workspace_request_failed",
            "The workspace server request failed.",
        ),
    }
}

fn project_recent_error(error: RecentWorkspaceStoreError) -> DesktopCommandError {
    match error {
        RecentWorkspaceStoreError::UnknownWorkspace => DesktopCommandError::new(
            "recent_workspace_unknown",
            "The recent workspace is no longer available.",
        ),
        RecentWorkspaceStoreError::InvalidFile | RecentWorkspaceStoreError::InvalidRecord => {
            DesktopCommandError::new(
                "recent_workspaces_invalid",
                "The recent workspace list is invalid.",
            )
        }
        RecentWorkspaceStoreError::Unavailable => DesktopCommandError::new(
            "recent_workspaces_unavailable",
            "The recent workspace list is unavailable.",
        ),
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
