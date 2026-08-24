use std::{
    fs,
    io::Write,
    path::Path,
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Serialize};

use crate::{UpdateCheckOutcome, UpdateError};

const CACHE_SCHEMA_VERSION: u16 = 1;
const MAX_CACHE_BYTES: u64 = 256 * 1024;

/// The single product-plane owner for the shared signed updater cache.
///
/// CLI, TUI, and Desktop callers may share this owner, but they do not receive the cache path or
/// implement their own temporary-file/replace protocol. The owner keeps the physical location and
/// atomic publication lifecycle private to this crate.
#[derive(Debug, Clone)]
pub struct ProductUpdaterState {
    cache_file: std::path::PathBuf,
    replace_lock: Arc<Mutex<()>>,
}

impl ProductUpdaterState {
    /// Creates the owner for Sigil's configured cache root.
    #[must_use]
    pub fn from_cache_root(cache_root: impl Into<std::path::PathBuf>) -> Self {
        Self {
            cache_file: cache_root.into().join(crate::UPDATE_CACHE_RELATIVE_PATH),
            replace_lock: Arc::new(Mutex::new(())),
        }
    }

    pub(crate) async fn load(&self) -> Option<UpdateCacheEntry> {
        load(&self.cache_file).await
    }

    pub(crate) async fn replace(
        &self,
        entry: &UpdateCacheEntry,
    ) -> Result<ProductUpdaterReceipt, UpdateError> {
        let path = self.cache_file.clone();
        let entry = entry.clone();
        let replace_lock = Arc::clone(&self.replace_lock);
        tokio::task::spawn_blocking(move || {
            let _guard = replace_lock
                .lock()
                .map_err(|_| UpdateError::Cache("updater owner lock poisoned".to_owned()))?;
            replace_sync(&path, &entry)
        })
        .await
        .map_err(|error| UpdateError::Cache(format!("cache writer task failed: {error}")))?
    }
}

/// Closed receipt returned after the product updater owner publishes one complete object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductUpdaterReceipt {
    object_hash: [u8; 32],
}

impl ProductUpdaterReceipt {
    /// Returns the content identity of the atomically published cache object.
    #[must_use]
    pub fn object_hash(&self) -> [u8; 32] {
        self.object_hash
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

fn object_hash(entry: &UpdateCacheEntry) -> [u8; 32] {
    use ring::digest::{SHA256, digest};

    let bytes = serde_json::to_vec(entry).expect("updater cache entry is serializable");
    let digest = digest(&SHA256, &bytes);
    let mut hash = [0u8; 32];
    hash.copy_from_slice(digest.as_ref());
    hash
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

fn replace_sync(
    path: &Path,
    entry: &UpdateCacheEntry,
) -> Result<ProductUpdaterReceipt, UpdateError> {
    let new_hash = object_hash(entry);
    if load_sync(path).is_some_and(|current| object_hash(&current) == new_hash) {
        return Err(UpdateError::Cache(
            "updater cache object is already current".to_owned(),
        ));
    }
    store_sync(path, entry)?;
    Ok(ProductUpdaterReceipt {
        object_hash: new_hash,
    })
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
