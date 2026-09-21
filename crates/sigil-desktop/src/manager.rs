use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    future::Future,
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::{
    sync::{Notify, oneshot},
    task::JoinHandle,
};
use uuid::Uuid;

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
#[serde(rename_all = "camelCase")]
pub struct DesktopWorkspaceSummary {
    pub id: String,
    pub display_name: String,
    pub server_version: String,
    pub state: DesktopConnectionState,
}

struct ManagedWorkspace {
    canonical_root: PathBuf,
    display_name: String,
    launch: DesktopLaunchRequest,
    state: DesktopConnectionState,
    process: DesktopServerProcess,
}

/// Owns at most one authenticated `sigil serve` process per canonical workspace.
#[derive(Clone)]
pub struct DesktopWorkspaceManager {
    launcher: DesktopLauncher,
    state: Arc<Mutex<DesktopWorkspaceManagerState>>,
    changed: Arc<Notify>,
    close_all_gate: Arc<tokio::sync::Mutex<()>>,
}

struct DesktopWorkspaceManagerState {
    workspaces: BTreeMap<String, ManagedWorkspace>,
    opening_roots: BTreeSet<PathBuf>,
    opening_workspace_ids: BTreeMap<String, PathBuf>,
    closing: bool,
    operations: BTreeMap<Uuid, JoinHandle<()>>,
    close_all_task: Option<JoinHandle<()>>,
    retained_cleanup: Vec<(String, ManagedWorkspace)>,
}

struct DesktopWorkspaceOpenTicket {
    canonical_root: PathBuf,
    display_name: String,
    launch: DesktopLaunchRequest,
    existing: Option<(String, ManagedWorkspace)>,
}

impl DesktopWorkspaceManager {
    /// Creates an empty manager around the provided launcher policy.
    #[must_use]
    pub fn new(launcher: DesktopLauncher) -> Self {
        Self {
            launcher,
            state: Arc::new(Mutex::new(DesktopWorkspaceManagerState {
                workspaces: BTreeMap::new(),
                opening_roots: BTreeSet::new(),
                opening_workspace_ids: BTreeMap::new(),
                closing: false,
                operations: BTreeMap::new(),
                close_all_task: None,
                retained_cleanup: Vec::new(),
            })),
            changed: Arc::new(Notify::new()),
            close_all_gate: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// Opens or reuses the one process assigned to a canonical workspace.
    pub async fn open(
        &self,
        request: DesktopWorkspaceOpenRequest,
    ) -> Result<DesktopWorkspaceSummary, DesktopWorkspaceManagerError> {
        validate_display_name(&request.display_name)?;
        let canonical_root = tokio::fs::canonicalize(&request.launch.workspace_root)
            .await
            .map_err(|_| DesktopWorkspaceManagerError::InvalidWorkspace)?;
        let completion = {
            let mut state = self.lock_state();
            if state.closing || state.opening_roots.contains(&canonical_root) {
                return Err(DesktopWorkspaceManagerError::WorkspaceOperationInProgress);
            }
            let existing_id = state
                .workspaces
                .iter()
                .find(|(_, workspace)| workspace.canonical_root == canonical_root)
                .map(|(id, _)| id.clone());
            let ticket = if let Some(id) = existing_id {
                let workspace = state
                    .workspaces
                    .get_mut(&id)
                    .expect("workspace id found during the same manager operation");
                refresh_workspace(workspace)?;
                if workspace.state == DesktopConnectionState::Ready {
                    return Ok(summary(&id, workspace));
                }
                state.opening_roots.insert(canonical_root.clone());
                let workspace = state
                    .workspaces
                    .remove(&id)
                    .expect("workspace id was checked immediately before removal");
                state
                    .opening_workspace_ids
                    .insert(id.clone(), canonical_root.clone());
                DesktopWorkspaceOpenTicket {
                    canonical_root,
                    display_name: request.display_name,
                    launch: request.launch,
                    existing: Some((id, workspace)),
                }
            } else {
                state.opening_roots.insert(canonical_root.clone());
                DesktopWorkspaceOpenTicket {
                    canonical_root,
                    display_name: request.display_name,
                    launch: request.launch,
                    existing: None,
                }
            };
            let manager = self.clone();
            self.spawn_operation(&mut state, async move {
                manager.execute_open_ticket(ticket).await
            })
        };
        completion
            .await
            .map_err(|_| DesktopWorkspaceManagerError::WorkspaceUnavailable)?
    }

    /// Returns current secret-free summaries after polling native child status.
    pub fn list(&self) -> Result<Vec<DesktopWorkspaceSummary>, DesktopWorkspaceManagerError> {
        let mut state = self.lock_state();
        state
            .workspaces
            .iter_mut()
            .map(|(id, workspace)| {
                refresh_workspace(workspace)?;
                Ok(summary(id, workspace))
            })
            .collect()
    }

    /// Returns a typed client only while the workspace process is ready.
    pub fn client(
        &self,
        workspace_id: &str,
    ) -> Result<DesktopHttpClient, DesktopWorkspaceManagerError> {
        let mut state = self.lock_state();
        let opening = state.opening_workspace_ids.contains_key(workspace_id);
        let workspace = state.workspaces.get_mut(workspace_id).ok_or(if opening {
            DesktopWorkspaceManagerError::WorkspaceUnavailable
        } else {
            DesktopWorkspaceManagerError::UnknownWorkspace
        })?;
        refresh_workspace(workspace)?;
        if workspace.state != DesktopConnectionState::Ready {
            return Err(DesktopWorkspaceManagerError::WorkspaceUnavailable);
        }
        Ok(workspace.process.client())
    }

    /// Restarts one workspace server so a configuration saved through the current recovery
    /// process becomes the configuration used by the long-lived runtime. The workspace identity
    /// must remain stable across the replacement process.
    pub async fn restart(
        &self,
        workspace_id: &str,
    ) -> Result<DesktopWorkspaceSummary, DesktopWorkspaceManagerError> {
        let completion = {
            let mut state = self.lock_state();
            if state.closing || state.opening_workspace_ids.contains_key(workspace_id) {
                return Err(DesktopWorkspaceManagerError::WorkspaceOperationInProgress);
            }
            let workspace = state
                .workspaces
                .remove(workspace_id)
                .ok_or(DesktopWorkspaceManagerError::UnknownWorkspace)?;
            state.opening_roots.insert(workspace.canonical_root.clone());
            state
                .opening_workspace_ids
                .insert(workspace_id.to_owned(), workspace.canonical_root.clone());
            let ticket = DesktopWorkspaceOpenTicket {
                canonical_root: workspace.canonical_root.clone(),
                display_name: workspace.display_name.clone(),
                launch: workspace.launch.clone(),
                existing: Some((workspace_id.to_owned(), workspace)),
            };
            let manager = self.clone();
            self.spawn_operation(&mut state, async move {
                manager.execute_open_ticket(ticket).await
            })
        };
        completion
            .await
            .map_err(|_| DesktopWorkspaceManagerError::WorkspaceUnavailable)?
    }

    /// Gracefully closes one workspace. Its root remains reserved until cleanup finishes,
    /// even when the caller drops the returned future.
    pub async fn close(
        &self,
        workspace_id: &str,
    ) -> Result<DesktopShutdownReport, DesktopWorkspaceManagerError> {
        let completion = {
            let mut state = self.lock_state();
            let retained = state
                .retained_cleanup
                .iter()
                .position(|(id, _)| id == workspace_id);
            if state.closing
                || (state.opening_workspace_ids.contains_key(workspace_id) && retained.is_none())
            {
                return Err(DesktopWorkspaceManagerError::WorkspaceOperationInProgress);
            }
            let workspace = if let Some(index) = retained {
                state.retained_cleanup.remove(index).1
            } else {
                state
                    .workspaces
                    .remove(workspace_id)
                    .ok_or(DesktopWorkspaceManagerError::UnknownWorkspace)?
            };
            state.opening_roots.insert(workspace.canonical_root.clone());
            state
                .opening_workspace_ids
                .insert(workspace_id.to_owned(), workspace.canonical_root.clone());
            let manager = self.clone();
            let workspace_id = workspace_id.to_owned();
            self.spawn_operation(&mut state, async move {
                manager
                    .close_workspace(workspace_id, workspace)
                    .await
                    .map_err(Into::into)
            })
        };
        completion
            .await
            .map_err(|_| DesktopWorkspaceManagerError::WorkspaceUnavailable)?
    }

    /// Closes admission, drains all admitted lifecycle operations, then closes every process.
    /// Caller cancellation does not cancel this manager-owned cleanup. Concurrent callers wait
    /// for the preceding drain before starting another one.
    pub async fn close_all(
        &self,
    ) -> Vec<(String, Result<DesktopShutdownReport, DesktopShutdownError>)> {
        let gate = self.close_all_gate.clone().lock_owned().await;
        let (sender, receiver) = oneshot::channel();
        {
            let mut state = self.lock_state();
            state.closing = true;
            let manager = self.clone();
            state.close_all_task = Some(tokio::spawn(async move {
                let _gate = gate;
                manager.wait_for_operations().await;
                let workspaces = {
                    let mut state = manager.lock_state();
                    let mut workspaces = std::mem::take(&mut state.workspaces)
                        .into_iter()
                        .collect::<Vec<_>>();
                    workspaces.append(&mut state.retained_cleanup);
                    workspaces
                };
                let mut results = Vec::with_capacity(workspaces.len());
                for (id, workspace) in workspaces {
                    let result = manager.close_workspace(id.clone(), workspace).await;
                    results.push((id, result));
                }
                {
                    let mut state = manager.lock_state();
                    state.closing = false;
                    state.close_all_task.take();
                }
                manager.changed.notify_waiters();
                let _ = sender.send(results);
            }));
        }
        receiver
            .await
            .unwrap_or_else(|_| vec![(String::new(), Err(DesktopShutdownError::WaitFailed))])
    }

    async fn close_workspace(
        &self,
        id: String,
        mut workspace: ManagedWorkspace,
    ) -> Result<DesktopShutdownReport, DesktopShutdownError> {
        let result = workspace.process.shutdown_in_place().await;
        let mut state = self.lock_state();
        if result.is_err() {
            // A failed wait is not proof of exit; keep both the handle and root reservation.
            state.opening_roots.insert(workspace.canonical_root.clone());
            state
                .opening_workspace_ids
                .entry(id.clone())
                .or_insert_with(|| workspace.canonical_root.clone());
            state.retained_cleanup.push((id, workspace));
        } else {
            state.opening_roots.remove(&workspace.canonical_root);
            if state.opening_workspace_ids.get(&id) == Some(&workspace.canonical_root) {
                state.opening_workspace_ids.remove(&id);
            }
        }
        result
    }

    fn spawn_operation<T: Send + 'static>(
        &self,
        state: &mut DesktopWorkspaceManagerState,
        operation: impl Future<Output = T> + Send + 'static,
    ) -> oneshot::Receiver<T> {
        let id = Uuid::new_v4();
        let manager = self.clone();
        let (sender, receiver) = oneshot::channel();
        let handle = tokio::spawn(async move {
            let result = operation.await;
            manager.lock_state().operations.remove(&id);
            manager.changed.notify_waiters();
            let _ = sender.send(result);
        });
        state.operations.insert(id, handle);
        receiver
    }

    async fn wait_for_operations(&self) {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.lock_state().operations.is_empty() {
                return;
            }
            notified.await;
        }
    }

    fn lock_state(&self) -> MutexGuard<'_, DesktopWorkspaceManagerState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    async fn execute_open_ticket(
        &self,
        mut ticket: DesktopWorkspaceOpenTicket,
    ) -> Result<DesktopWorkspaceSummary, DesktopWorkspaceManagerError> {
        if let Some((_, workspace)) = ticket.existing.as_mut()
            && workspace.process.is_running()
            && let Err(error) = workspace.process.shutdown_in_place().await
        {
            self.restore_open_ticket(ticket);
            return Err(error.into());
        }
        if let Some((_, workspace)) = ticket.existing.as_mut() {
            workspace.state = DesktopConnectionState::Exited;
        }
        let closing = self.lock_state().closing;
        if closing {
            self.restore_open_ticket(ticket);
            return Err(DesktopWorkspaceManagerError::WorkspaceOperationInProgress);
        }
        let launch = ticket.launch.clone();
        let process = match self.launcher.launch(launch).await {
            Ok(process) => process,
            Err(error) => {
                self.restore_open_ticket(ticket);
                return Err(error.into());
            }
        };
        let id = process.server_info().workspace_id.clone();
        let workspace = ManagedWorkspace {
            canonical_root: ticket.canonical_root.clone(),
            display_name: ticket.display_name.clone(),
            launch: ticket.launch.clone(),
            state: DesktopConnectionState::Ready,
            process,
        };
        let response = summary(&id, &workspace);
        let rejected = {
            let mut state = self.lock_state();
            let collision = state.workspaces.contains_key(&id)
                || state
                    .opening_workspace_ids
                    .get(&id)
                    .is_some_and(|root| root != &ticket.canonical_root)
                || ticket
                    .existing
                    .as_ref()
                    .is_some_and(|(existing_id, _)| existing_id != &id);
            if collision || state.closing {
                Some((
                    workspace,
                    if collision {
                        DesktopWorkspaceManagerError::IdentityCollision
                    } else {
                        DesktopWorkspaceManagerError::WorkspaceOperationInProgress
                    },
                ))
            } else {
                state.workspaces.insert(id.clone(), workspace);
                state.opening_roots.remove(&ticket.canonical_root);
                state.opening_workspace_ids.remove(&id);
                if let Some((existing_id, _)) = ticket.existing.as_ref() {
                    state.opening_workspace_ids.remove(existing_id);
                }
                None
            }
        };
        if let Some((workspace, error)) = rejected {
            let result = self.close_workspace(id, workspace).await;
            if result.is_ok() {
                self.discard_open_ticket(&ticket);
            }
            return Err(result.err().map_or(error, Into::into));
        }
        Ok(response)
    }

    fn restore_open_ticket(&self, ticket: DesktopWorkspaceOpenTicket) {
        let mut state = self.lock_state();
        if let Some((id, workspace)) = ticket.existing {
            state.workspaces.insert(id.clone(), workspace);
            state.opening_workspace_ids.remove(&id);
        }
        state.opening_roots.remove(&ticket.canonical_root);
    }

    fn discard_open_ticket(&self, ticket: &DesktopWorkspaceOpenTicket) {
        let mut state = self.lock_state();
        state.opening_roots.remove(&ticket.canonical_root);
        if let Some((id, _)) = ticket.existing.as_ref() {
            state.opening_workspace_ids.remove(id);
        }
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
            .field("workspace_count", &self.lock_state().workspaces.len())
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
    #[error("desktop workspace operation is already in progress")]
    WorkspaceOperationInProgress,
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
