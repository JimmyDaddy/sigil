use std::{collections::BTreeMap, fmt, path::PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    DesktopClientError, DesktopHttpClient, DesktopLaunchError, DesktopLaunchRequest,
    DesktopLauncher, DesktopServerProcess, DesktopShutdownError, DesktopShutdownReport,
};

/// Exact native-only inputs for opening one workspace connection.
#[derive(Clone)]
pub struct DesktopWorkspaceOpenRequest {
    pub launch: DesktopLaunchRequest,
    pub display_name: String,
}

impl DesktopWorkspaceOpenRequest {
    /// Creates a native-only request. Paths are never serialized or returned to a renderer.
    #[must_use]
    pub fn new(launch: DesktopLaunchRequest, display_name: impl Into<String>) -> Self {
        Self {
            launch,
            display_name: display_name.into(),
        }
    }
}

impl fmt::Debug for DesktopWorkspaceOpenRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DesktopWorkspaceOpenRequest")
            .field("launch", &self.launch)
            .field("display_name", &self.display_name)
            .finish()
    }
}

/// Renderer-safe lifecycle state for one workspace-owned server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopConnectionState {
    Ready,
    Exited,
    Crashed,
}

/// Renderer-safe workspace summary with no local path, token, address, or process handle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DesktopWorkspaceSummary {
    pub id: String,
    pub display_name: String,
    pub server_version: String,
    pub state: DesktopConnectionState,
}

struct ManagedWorkspace {
    canonical_root: PathBuf,
    display_name: String,
    state: DesktopConnectionState,
    process: DesktopServerProcess,
}

/// Owns at most one authenticated `sigil serve` process per canonical workspace.
pub struct DesktopWorkspaceManager {
    launcher: DesktopLauncher,
    workspaces: BTreeMap<String, ManagedWorkspace>,
}

impl DesktopWorkspaceManager {
    /// Creates an empty manager around the provided launcher policy.
    #[must_use]
    pub fn new(launcher: DesktopLauncher) -> Self {
        Self {
            launcher,
            workspaces: BTreeMap::new(),
        }
    }

    /// Opens or reuses the one process assigned to a canonical workspace.
    pub async fn open(
        &mut self,
        request: DesktopWorkspaceOpenRequest,
    ) -> Result<DesktopWorkspaceSummary, DesktopWorkspaceManagerError> {
        validate_display_name(&request.display_name)?;
        let canonical_root = tokio::fs::canonicalize(&request.launch.workspace_root)
            .await
            .map_err(|_| DesktopWorkspaceManagerError::InvalidWorkspace)?;
        if let Some((id, workspace)) = self
            .workspaces
            .iter_mut()
            .find(|(_, workspace)| workspace.canonical_root == canonical_root)
        {
            refresh_workspace(workspace)?;
            return Ok(summary(id, workspace));
        }

        let process = self.launcher.launch(request.launch).await?;
        let id = process.server_info().workspace_id.clone();
        if self.workspaces.contains_key(&id) {
            process.shutdown().await?;
            return Err(DesktopWorkspaceManagerError::IdentityCollision);
        }
        let workspace = ManagedWorkspace {
            canonical_root,
            display_name: request.display_name,
            state: DesktopConnectionState::Ready,
            process,
        };
        let response = summary(&id, &workspace);
        self.workspaces.insert(id, workspace);
        Ok(response)
    }

    /// Returns current secret-free summaries after polling native child status.
    pub fn list(&mut self) -> Result<Vec<DesktopWorkspaceSummary>, DesktopWorkspaceManagerError> {
        self.workspaces
            .iter_mut()
            .map(|(id, workspace)| {
                refresh_workspace(workspace)?;
                Ok(summary(id, workspace))
            })
            .collect()
    }

    /// Returns a typed client only while the workspace process is ready.
    pub fn client(
        &mut self,
        workspace_id: &str,
    ) -> Result<DesktopHttpClient, DesktopWorkspaceManagerError> {
        let workspace = self
            .workspaces
            .get_mut(workspace_id)
            .ok_or(DesktopWorkspaceManagerError::UnknownWorkspace)?;
        refresh_workspace(workspace)?;
        if workspace.state != DesktopConnectionState::Ready {
            return Err(DesktopWorkspaceManagerError::WorkspaceUnavailable);
        }
        Ok(workspace.process.client())
    }

    /// Gracefully closes and removes one workspace-owned process.
    pub async fn close(
        &mut self,
        workspace_id: &str,
    ) -> Result<DesktopShutdownReport, DesktopWorkspaceManagerError> {
        let workspace = self
            .workspaces
            .remove(workspace_id)
            .ok_or(DesktopWorkspaceManagerError::UnknownWorkspace)?;
        workspace.process.shutdown().await.map_err(Into::into)
    }

    /// Closes every process without admitting new work between shutdowns.
    pub async fn close_all(
        &mut self,
    ) -> Vec<(String, Result<DesktopShutdownReport, DesktopShutdownError>)> {
        let workspaces = std::mem::take(&mut self.workspaces);
        let mut results = Vec::with_capacity(workspaces.len());
        for (id, workspace) in workspaces {
            results.push((id, workspace.process.shutdown().await));
        }
        results
    }
}

impl Default for DesktopWorkspaceManager {
    fn default() -> Self {
        Self::new(DesktopLauncher::default())
    }
}

impl fmt::Debug for DesktopWorkspaceManager {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DesktopWorkspaceManager")
            .field("workspace_count", &self.workspaces.len())
            .finish_non_exhaustive()
    }
}

fn refresh_workspace(workspace: &mut ManagedWorkspace) -> Result<(), DesktopWorkspaceManagerError> {
    if workspace.state != DesktopConnectionState::Ready {
        return Ok(());
    }
    if let Some(status) = workspace
        .process
        .try_exit_status()
        .map_err(|_| DesktopWorkspaceManagerError::ProcessStatusUnavailable)?
    {
        workspace.state = if status.success() {
            DesktopConnectionState::Exited
        } else {
            DesktopConnectionState::Crashed
        };
    }
    Ok(())
}

fn summary(id: &str, workspace: &ManagedWorkspace) -> DesktopWorkspaceSummary {
    DesktopWorkspaceSummary {
        id: id.to_owned(),
        display_name: workspace.display_name.clone(),
        server_version: workspace.process.server_info().server_version.clone(),
        state: workspace.state,
    }
}

fn validate_display_name(value: &str) -> Result<(), DesktopWorkspaceManagerError> {
    if value.trim().is_empty()
        || value.len() > 160
        || value.chars().any(|character| character.is_control())
    {
        return Err(DesktopWorkspaceManagerError::InvalidDisplayName);
    }
    Ok(())
}

/// Typed, path-free workspace-manager failures safe for native-shell projection.
#[derive(Debug, Error)]
pub enum DesktopWorkspaceManagerError {
    #[error("desktop workspace is invalid")]
    InvalidWorkspace,
    #[error("desktop workspace display name is invalid")]
    InvalidDisplayName,
    #[error("desktop workspace identity collided")]
    IdentityCollision,
    #[error("desktop workspace is not open")]
    UnknownWorkspace,
    #[error("desktop workspace process is unavailable")]
    WorkspaceUnavailable,
    #[error("desktop workspace process status is unavailable")]
    ProcessStatusUnavailable,
    #[error(transparent)]
    Launch(#[from] DesktopLaunchError),
    #[error(transparent)]
    Shutdown(#[from] DesktopShutdownError),
    #[error(transparent)]
    Client(#[from] DesktopClientError),
}

#[cfg(test)]
#[path = "tests/manager_tests.rs"]
mod tests;
