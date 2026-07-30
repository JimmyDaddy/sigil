use std::{fs, io::Write, path::Path};

use serde::{Deserialize, Serialize};

use crate::{UpdateCheckOutcome, UpdateError};

const CACHE_SCHEMA_VERSION: u16 = 1;
const MAX_CACHE_BYTES: u64 = 256 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UpdateCacheEntry {
    pub(crate) schema_version: u16,
    pub(crate) cache_key: String,
    pub(crate) checked_at_unix_seconds: u64,
    pub(crate) etag: Option<String>,
    pub(crate) outcome: UpdateCheckOutcome,
}

impl UpdateCacheEntry {
    pub(crate) fn new(
        cache_key: String,
        checked_at_unix_seconds: u64,
        etag: Option<String>,
        mut outcome: UpdateCheckOutcome,
    ) -> Self {
        outcome.cached = false;
        Self {
            schema_version: CACHE_SCHEMA_VERSION,
            cache_key,
            checked_at_unix_seconds,
            etag,
            outcome,
        }
    }
}

pub(crate) async fn load(path: &Path) -> Option<UpdateCacheEntry> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || load_sync(&path))
        .await
        .ok()
        .flatten()
}

pub(crate) async fn store(path: &Path, entry: &UpdateCacheEntry) -> Result<(), UpdateError> {
    let path = path.to_path_buf();
    let entry = entry.clone();
    tokio::task::spawn_blocking(move || store_sync(&path, &entry))
        .await
        .map_err(|error| UpdateError::Cache(format!("cache writer task failed: {error}")))?
}

fn load_sync(path: &Path) -> Option<UpdateCacheEntry> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > MAX_CACHE_BYTES
    {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    let entry = serde_json::from_slice::<UpdateCacheEntry>(&bytes).ok()?;
    (entry.schema_version == CACHE_SCHEMA_VERSION).then_some(entry)
}

fn store_sync(path: &Path, entry: &UpdateCacheEntry) -> Result<(), UpdateError> {
    let parent = path
        .parent()
        .ok_or_else(|| UpdateError::Cache("cache path has no parent".to_owned()))?;
    fs::create_dir_all(parent)
        .map_err(|error| UpdateError::Cache(format!("create cache directory: {error}")))?;
    let bytes = serde_json::to_vec(entry)
        .map_err(|error| UpdateError::Cache(format!("encode cache: {error}")))?;
    if bytes.len() as u64 > MAX_CACHE_BYTES {
        return Err(UpdateError::Cache("encoded cache exceeds limit".to_owned()));
    }
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .map_err(|error| UpdateError::Cache(format!("create cache staging file: {error}")))?;
    temporary
        .write_all(&bytes)
        .and_then(|()| temporary.as_file_mut().sync_all())
        .map_err(|error| UpdateError::Cache(format!("write cache staging file: {error}")))?;
    temporary
        .persist(path)
        .map_err(|error| UpdateError::Cache(format!("publish cache file: {}", error.error)))?;
    set_owner_only(path)?;
    sync_parent(parent)?;
    Ok(())
}

#[cfg(unix)]
fn set_owner_only(path: &Path) -> Result<(), UpdateError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|error| UpdateError::Cache(format!("secure cache permissions: {error}")))
}

#[cfg(not(unix))]
fn set_owner_only(_path: &Path) -> Result<(), UpdateError> {
    Ok(())
}

fn sync_parent(parent: &Path) -> Result<(), UpdateError> {
    let directory = fs::File::open(parent)
        .map_err(|error| UpdateError::Cache(format!("open cache directory: {error}")))?;
    directory
        .sync_all()
        .map_err(|error| UpdateError::Cache(format!("sync cache directory: {error}")))
}

#[cfg(test)]
#[path = "tests/cache_tests.rs"]
mod tests;
