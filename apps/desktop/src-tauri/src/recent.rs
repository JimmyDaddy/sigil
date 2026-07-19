use std::{
    collections::BTreeSet,
    io::Write,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

const RECENT_WORKSPACE_SCHEMA_VERSION: u16 = 1;
const MAX_RECENT_WORKSPACES: usize = 12;
const MAX_RECENT_FILE_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RecentWorkspaceSummary {
    pub(crate) id: String,
    pub(crate) display_name: String,
    pub(crate) is_open: bool,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RecentWorkspaceRecord {
    id: String,
    display_name: String,
    workspace_root: PathBuf,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RecentWorkspaceFile {
    schema_version: u16,
    entries: Vec<RecentWorkspaceRecord>,
}

pub(crate) struct RecentWorkspaceStore {
    path: PathBuf,
    loaded: bool,
    entries: Vec<RecentWorkspaceRecord>,
}

impl RecentWorkspaceStore {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self {
            path,
            loaded: false,
            entries: Vec::new(),
        }
    }

    pub(crate) async fn list(
        &mut self,
        open_workspace_ids: &BTreeSet<String>,
    ) -> Result<Vec<RecentWorkspaceSummary>, RecentWorkspaceStoreError> {
        self.load_if_needed().await?;
        Ok(self
            .entries
            .iter()
            .map(|entry| RecentWorkspaceSummary {
                id: entry.id.clone(),
                display_name: entry.display_name.clone(),
                is_open: open_workspace_ids.contains(&entry.id),
            })
            .collect())
    }

    pub(crate) async fn resolve(
        &mut self,
        id: &str,
    ) -> Result<(PathBuf, String), RecentWorkspaceStoreError> {
        self.load_if_needed().await?;
        let entry = self
            .entries
            .iter()
            .find(|entry| entry.id == id)
            .ok_or(RecentWorkspaceStoreError::UnknownWorkspace)?;
        Ok((entry.workspace_root.clone(), entry.display_name.clone()))
    }

    pub(crate) async fn upsert(
        &mut self,
        id: String,
        display_name: String,
        workspace_root: &Path,
    ) -> Result<(), RecentWorkspaceStoreError> {
        self.load_if_needed().await?;
        validate_identity(&id, &display_name)?;
        let canonical_root = tokio::fs::canonicalize(workspace_root)
            .await
            .map_err(|_| RecentWorkspaceStoreError::InvalidRecord)?;
        self.entries
            .retain(|entry| entry.id != id && entry.workspace_root != canonical_root);
        self.entries.insert(
            0,
            RecentWorkspaceRecord {
                id,
                display_name,
                workspace_root: canonical_root,
            },
        );
        self.entries.truncate(MAX_RECENT_WORKSPACES);
        self.persist().await
    }

    async fn load_if_needed(&mut self) -> Result<(), RecentWorkspaceStoreError> {
        if self.loaded {
            return Ok(());
        }
        let bytes = match tokio::fs::read(&self.path).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.loaded = true;
                return Ok(());
            }
            Err(_) => return Err(RecentWorkspaceStoreError::Unavailable),
        };
        if bytes.len() as u64 > MAX_RECENT_FILE_BYTES {
            return Err(RecentWorkspaceStoreError::InvalidFile);
        }
        let file: RecentWorkspaceFile =
            serde_json::from_slice(&bytes).map_err(|_| RecentWorkspaceStoreError::InvalidFile)?;
        if file.schema_version != RECENT_WORKSPACE_SCHEMA_VERSION
            || file.entries.len() > MAX_RECENT_WORKSPACES
        {
            return Err(RecentWorkspaceStoreError::InvalidFile);
        }
        for entry in &file.entries {
            validate_identity(&entry.id, &entry.display_name)?;
            if !entry.workspace_root.is_absolute() {
                return Err(RecentWorkspaceStoreError::InvalidRecord);
            }
        }
        self.entries = file.entries;
        self.loaded = true;
        Ok(())
    }

    async fn persist(&self) -> Result<(), RecentWorkspaceStoreError> {
        let path = self.path.clone();
        let bytes = serde_json::to_vec_pretty(&RecentWorkspaceFile {
            schema_version: RECENT_WORKSPACE_SCHEMA_VERSION,
            entries: self.entries.clone(),
        })
        .map_err(|_| RecentWorkspaceStoreError::Unavailable)?;
        if bytes.len() as u64 > MAX_RECENT_FILE_BYTES {
            return Err(RecentWorkspaceStoreError::InvalidFile);
        }
        tokio::task::spawn_blocking(move || persist_atomically(&path, &bytes))
            .await
            .map_err(|_| RecentWorkspaceStoreError::Unavailable)?
    }
}

fn persist_atomically(path: &Path, bytes: &[u8]) -> Result<(), RecentWorkspaceStoreError> {
    let parent = path
        .parent()
        .ok_or(RecentWorkspaceStoreError::Unavailable)?;
    std::fs::create_dir_all(parent).map_err(|_| RecentWorkspaceStoreError::Unavailable)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .map_err(|_| RecentWorkspaceStoreError::Unavailable)?;
    temporary
        .write_all(bytes)
        .and_then(|()| temporary.as_file_mut().sync_all())
        .map_err(|_| RecentWorkspaceStoreError::Unavailable)?;
    temporary
        .persist(path)
        .map_err(|_| RecentWorkspaceStoreError::Unavailable)?;
    Ok(())
}

fn validate_identity(id: &str, display_name: &str) -> Result<(), RecentWorkspaceStoreError> {
    if id.is_empty()
        || id.len() > 512
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        || display_name.trim().is_empty()
        || display_name.len() > 160
        || display_name.chars().any(char::is_control)
    {
        return Err(RecentWorkspaceStoreError::InvalidRecord);
    }
    Ok(())
}

#[derive(Debug, Error)]
pub(crate) enum RecentWorkspaceStoreError {
    #[error("recent workspace store is unavailable")]
    Unavailable,
    #[error("recent workspace store is invalid")]
    InvalidFile,
    #[error("recent workspace record is invalid")]
    InvalidRecord,
    #[error("recent workspace is unknown")]
    UnknownWorkspace,
}

#[cfg(test)]
#[path = "tests/recent_tests.rs"]
mod tests;
