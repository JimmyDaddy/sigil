//! Native file selection and safe preview for MCP configuration import.

use serde::Serialize;
use sigil_desktop::{DesktopMcpImportApplyRequest, DesktopMcpImportPreview};
use tauri::State;
use tauri_plugin_dialog::DialogExt;

use crate::{commands::DesktopCommandError, state::DesktopAppState};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct McpImportPreview {
    preview_id: String,
    candidates: Vec<McpImportCandidate>,
    root_fields_ignored: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct McpImportCandidate {
    index: usize,
    name: String,
    transport: Option<String>,
    description: String,
    importable: bool,
    issues: Vec<String>,
}

impl From<DesktopMcpImportPreview> for McpImportPreview {
    fn from(preview: DesktopMcpImportPreview) -> Self {
        Self {
            preview_id: preview.preview_id,
            root_fields_ignored: preview.root_fields_ignored,
            candidates: preview
                .candidates
                .into_iter()
                .map(|candidate| McpImportCandidate {
                    index: candidate.index,
                    name: candidate.name,
                    transport: candidate.transport,
                    description: candidate.description,
                    importable: candidate.importable,
                    issues: candidate.issues,
                })
                .collect(),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct McpImportSaveResult {
    imported_names: Vec<String>,
    reload_required: bool,
}

fn import_error(code: &'static str, message: &'static str) -> DesktopCommandError {
    DesktopCommandError {
        code,
        message: message.to_owned(),
        recovery_actions: Vec::new(),
        route_recovery: None,
    }
}

#[tauri::command]
pub(crate) async fn desktop_pick_mcp_import(
    app: tauri::AppHandle,
    workspace_id: String,
    state: State<'_, DesktopAppState>,
) -> Result<Option<McpImportPreview>, DesktopCommandError> {
    let client = state.manager.client(&workspace_id).map_err(|_| {
        import_error(
            "workspace_unavailable",
            "Open a workspace before importing MCP servers.",
        )
    })?;
    let (sender, receiver) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .add_filter("MCP configuration", &["json"])
        .pick_file(move |file| {
            let _ = sender.send(file);
        });
    let Some(file) = receiver.await.map_err(|_| {
        import_error(
            "mcp_import_picker_unavailable",
            "The file picker could not be opened.",
        )
    })?
    else {
        return Ok(None);
    };
    let path = file
        .into_path()
        .map_err(|_| import_error("mcp_import_invalid", "Select a local JSON file."))?;
    let bytes = tokio::task::spawn_blocking(move || {
        crate::selected_file::read_selected_file(&path, 1024 * 1024)
    })
    .await
    .map_err(|_| {
        import_error(
            "mcp_import_read_failed",
            "The selected file could not be read.",
        )
    })?
    .map_err(|_| {
        import_error(
            "mcp_import_read_failed",
            "Select a regular JSON file no larger than 1 MiB.",
        )
    })?;
    client.preview_mcp_import(bytes).await.map(|preview| Some(preview.into()))
        .map_err(|_| import_error("mcp_import_invalid", "Use a JSON document containing mcpServers. The current Sigil configuration must be readable."))
}

#[tauri::command]
pub(crate) async fn desktop_apply_mcp_import(
    workspace_id: String,
    preview_id: String,
    selected_indices: Vec<usize>,
    state: State<'_, DesktopAppState>,
) -> Result<McpImportSaveResult, DesktopCommandError> {
    let client =
        crate::commands::configuration_change_client(&state.manager, &workspace_id).await?;
    let result = client.apply_mcp_import_host_private(DesktopMcpImportApplyRequest {
        preview_id, selected_indices,
    }).await.map_err(|_| import_error(
        "mcp_import_not_saved", "Import was not saved. If configuration changed or a server name already exists, preview the file again and select new entries.",
    ))?;
    // Saving a reviewed configuration is independent of interrupting a foreground run.
    // If a restart is not currently safe, keep the durable save and let the user reload later.
    if crate::commands::ensure_workspace_restart_safe(&client)
        .await
        .is_err()
    {
        return Ok(McpImportSaveResult {
            imported_names: result.imported_names,
            reload_required: true,
        });
    }
    state.run_streams.stop_workspace(&workspace_id).await;
    let reload_required = state.manager.restart(&workspace_id).await.is_err();
    Ok(McpImportSaveResult {
        imported_names: result.imported_names,
        reload_required,
    })
}
