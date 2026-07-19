use std::path::{Path, PathBuf};

use serde::Serialize;
use sigil_desktop::{
    DesktopLaunchRequest, DesktopWorkspaceManagerError, DesktopWorkspaceOpenRequest,
    DesktopWorkspaceSummary,
};
use tauri::State;
use tauri_plugin_dialog::DialogExt;
use thiserror::Error;
use tokio::sync::oneshot;

use crate::state::DesktopAppState;

const DESKTOP_PROTOCOL_VERSION: u16 = 1;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopBootstrap {
    protocol_version: u16,
    workspaces: Vec<DesktopWorkspaceSummary>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopWorkspaceSelection {
    cancelled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    workspace: Option<DesktopWorkspaceSummary>,
}

#[derive(Debug, Error, Serialize)]
#[error("{message}")]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopCommandError {
    pub(crate) code: &'static str,
    pub(crate) message: &'static str,
}

impl DesktopCommandError {
    fn new(code: &'static str, message: &'static str) -> Self {
        Self { code, message }
    }
}

#[tauri::command]
pub(crate) async fn desktop_bootstrap(
    state: State<'_, DesktopAppState>,
) -> Result<DesktopBootstrap, DesktopCommandError> {
    let workspaces = state
        .manager
        .lock()
        .await
        .list()
        .map_err(project_manager_error)?;
    Ok(DesktopBootstrap {
        protocol_version: DESKTOP_PROTOCOL_VERSION,
        workspaces,
    })
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
    Ok(DesktopWorkspaceSelection {
        cancelled: false,
        workspace: Some(workspace),
    })
}

#[tauri::command]
pub(crate) async fn desktop_close_workspace(
    workspace_id: String,
    state: State<'_, DesktopAppState>,
) -> Result<Vec<DesktopWorkspaceSummary>, DesktopCommandError> {
    if !valid_workspace_id(&workspace_id) {
        return Err(DesktopCommandError::new(
            "workspace_id_invalid",
            "The workspace identifier is invalid.",
        ));
    }
    let mut manager = state.manager.lock().await;
    manager
        .close(&workspace_id)
        .await
        .map_err(project_manager_error)?;
    manager.list().map_err(project_manager_error)
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
    let binary_name = if cfg!(windows) { "sigil.exe" } else { "sigil" };
    let sibling = executable
        .parent()
        .map(|parent| parent.join(binary_name))
        .filter(|path| path.is_file())
        .ok_or_else(|| {
            DesktopCommandError::new(
                "sigil_binary_unavailable",
                "The bundled Sigil runtime cannot be located.",
            )
        })?;
    Ok(sibling)
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

#[cfg(test)]
#[path = "tests/commands_tests.rs"]
mod tests;
