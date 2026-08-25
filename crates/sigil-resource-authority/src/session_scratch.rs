//! RFC-0071: Resource Authority owner for the persistent SessionScratch namespace.
//!
//! The authority owns namespace allocation, no-follow measurement, quota admission, leases and
//! cleanup. Consumers receive only the exact directory selected for the admitted session scope;
//! they do not derive or create sibling roots themselves.

use std::sync::atomic::{AtomicU64, Ordering};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

#[cfg(not(unix))]
use std::time::SystemTime;

use fs2::FileExt;
use sigil_kernel::secure_private_path_permissions;

use crate::quota::{QuotaBookV1, QuotaErrorV1};

const SESSION_NAMESPACE_DIR: &str = "sessions";
const LEASE_MARKER_DIR: &str = ".leases";
const LEASE_LOCK_DIR: &str = ".lease-locks";
const QUARANTINE_DIR: &str = ".quarantine";
const MAX_QUARANTINE_ENTRIES: usize = 4_096;
const DEFAULT_MAX_ENTRIES: usize = 250_000;
static LEASE_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SessionScratchErrorV1 {
    #[error("session scratch path is not a plain directory: {path}")]
    NotPlainDirectory { path: String },
    #[error("session scratch contains a symlink at {path}")]
    Symlink { path: String },
    #[error("session scratch contains an unsupported entry at {path}")]
    UnsupportedEntry { path: String },
    #[error("session scratch measurement exceeded {limit} entries after {observed} entries")]
    EntryLimitExceeded { limit: usize, observed: usize },
    #[error("session scratch {scope} quota exceeded: {used} bytes used of {limit} allowed")]
    QuotaExceeded {
        scope: SessionScratchQuotaScopeV1,
        used: u64,
        limit: u64,
    },
    #[error("session scratch filesystem operation failed: {0}")]
    Filesystem(String),
    #[error("session scratch lease registry is unavailable")]
    LeaseRegistryUnavailable,
    #[error("session scratch lease lock is unavailable: {0}")]
    LeaseUnavailable(String),
    #[error("invalid session scratch namespace could not be quarantined: {path}: {reason}")]
    QuarantineFailed { path: String, reason: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionScratchQuotaScopeV1 {
    Session,
    Workspace,
}

impl std::fmt::Display for SessionScratchQuotaScopeV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Session => "session",
            Self::Workspace => "workspace",
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SessionScratchUsageV1 {
    pub session_bytes: u64,
    pub workspace_bytes: u64,
    pub session_entry_count: usize,
    pub workspace_entry_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionScratchProvisionV1 {
    pub directory: PathBuf,
    pub usage: SessionScratchUsageV1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionScratchGcConfigV1 {
    pub ttl_ms: u64,
    pub max_entries: usize,
}

impl Default for SessionScratchGcConfigV1 {
    fn default() -> Self {
        Self {
            ttl_ms: 24 * 60 * 60 * 1000,
            max_entries: DEFAULT_MAX_ENTRIES,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionScratchGcReportV1 {
    pub scanned: usize,
    pub deleted: usize,
    pub skipped_leased: usize,
    pub skipped_recent: usize,
    pub skipped_invalid: usize,
    pub quarantined: usize,
    pub deleted_bytes: u64,
    pub workspace_usage_bytes: u64,
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionScratchDeleteOutcomeV1 {
    Deleted,
    NotPresent,
    SkippedLeased,
}

#[derive(Debug, Default)]
struct SessionScratchLeaseRegistryV1 {
    keys: Mutex<BTreeMap<String, usize>>,
}

#[derive(Debug)]
pub struct SessionScratchLeaseV1 {
    registry: Arc<SessionScratchLeaseRegistryV1>,
    key: String,
    marker: PathBuf,
    marker_file: File,
}

impl Drop for SessionScratchLeaseV1 {
    fn drop(&mut self) {
        if let Ok(mut keys) = self.registry.keys.lock()
            && let Some(count) = keys.get_mut(&self.key)
        {
            if *count <= 1 {
                keys.remove(&self.key);
            } else {
                *count -= 1;
            }
        }
        let _ = self.marker_file.unlock();
        let _ = fs::remove_file(&self.marker);
    }
}

#[derive(Debug, Clone)]
pub struct SessionScratchAuthorityV1 {
    root: PathBuf,
    leases: Arc<SessionScratchLeaseRegistryV1>,
    quota: Arc<Mutex<Option<QuotaBookV1>>>,
}

impl SessionScratchAuthorityV1 {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            leases: Arc::new(SessionScratchLeaseRegistryV1::default()),
            quota: Arc::new(Mutex::new(None)),
        }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn session_directory(&self, session_scope_id: Option<&str>) -> PathBuf {
        self.root
            .join(SESSION_NAMESPACE_DIR)
            .join(session_scope_key(session_scope_id))
    }

    pub fn acquire(
        &self,
        session_scope_id: Option<&str>,
    ) -> Result<SessionScratchLeaseV1, SessionScratchErrorV1> {
        let key = session_scope_key(session_scope_id);
        let namespace_lock = self.lock_namespace(&key)?;
        let (marker, marker_file) = self.create_lease_marker(&key)?;
        let mut keys = self
            .leases
            .keys
            .lock()
            .map_err(|_| SessionScratchErrorV1::LeaseRegistryUnavailable)?;
        *keys.entry(key.clone()).or_default() += 1;
        drop(namespace_lock);
        Ok(SessionScratchLeaseV1 {
            registry: Arc::clone(&self.leases),
            key,
            marker,
            marker_file,
        })
    }

    pub fn ensure(
        &self,
        session_scope_id: Option<&str>,
        per_session_bytes: u64,
        workspace_hard_bytes: u64,
    ) -> Result<SessionScratchProvisionV1, SessionScratchErrorV1> {
        let key = session_scope_key(session_scope_id);
        let sessions = self.root.join(SESSION_NAMESPACE_DIR);
        let directory = sessions.join(&key);
        ensure_directory(&self.root)?;
        ensure_directory(&sessions)?;
        ensure_directory(&directory)?;
        let usage = self.measure(&key)?;
        if usage.session_bytes > per_session_bytes {
            return Err(SessionScratchErrorV1::QuotaExceeded {
                scope: SessionScratchQuotaScopeV1::Session,
                used: usage.session_bytes,
                limit: per_session_bytes,
            });
        }
        if usage.workspace_bytes > workspace_hard_bytes {
            return Err(SessionScratchErrorV1::QuotaExceeded {
                scope: SessionScratchQuotaScopeV1::Workspace,
                used: usage.workspace_bytes,
                limit: workspace_hard_bytes,
            });
        }
        let profile = scratch_quota_profile(workspace_hard_bytes);
        self.with_quota(Some(workspace_hard_bytes), |quota| {
            quota
                .reconcile_owned(
                    format!("session-scratch:{key}"),
                    &profile,
                    usage.session_bytes,
                    usage.session_entry_count as u64,
                )
                .map(|_| ())
        })?;
        Ok(SessionScratchProvisionV1 { directory, usage })
    }

    pub fn measure(
        &self,
        session_key: &str,
    ) -> Result<SessionScratchUsageV1, SessionScratchErrorV1> {
        let sessions = self.root.join(SESSION_NAMESPACE_DIR);
        if !sessions.is_dir() {
            return Ok(SessionScratchUsageV1::default());
        }
        let session_directory = sessions.join(session_key);
        let session = if is_plain_directory(&session_directory) {
            walk(&session_directory, DEFAULT_MAX_ENTRIES)?
        } else {
            WalkState::default()
        };
        let mut usage = SessionScratchUsageV1 {
            session_bytes: session.bytes,
            session_entry_count: session.entries,
            ..SessionScratchUsageV1::default()
        };
        for entry in fs::read_dir(&sessions).map_err(fs_error)? {
            let path = entry.map_err(fs_error)?.path();
            let metadata = fs::symlink_metadata(&path).map_err(fs_error)?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(SessionScratchErrorV1::NotPlainDirectory {
                    path: path.display().to_string(),
                });
            }
            if path == session_directory {
                continue;
            }
            let sibling = walk(&path, DEFAULT_MAX_ENTRIES)?;
            usage.workspace_bytes = usage.workspace_bytes.saturating_add(sibling.bytes);
            usage.workspace_entry_count =
                usage.workspace_entry_count.saturating_add(sibling.entries);
        }
        usage.workspace_bytes = usage.workspace_bytes.saturating_add(usage.session_bytes);
        usage.workspace_entry_count = usage
            .workspace_entry_count
            .saturating_add(usage.session_entry_count);
        Ok(usage)
    }

    pub fn gc(
        &self,
        config: SessionScratchGcConfigV1,
        now_ms: u64,
    ) -> Result<SessionScratchGcReportV1, SessionScratchErrorV1> {
        let sessions = self.root.join(SESSION_NAMESPACE_DIR);
        let mut report = SessionScratchGcReportV1::default();
        let entries = match fs::read_dir(&sessions) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(report),
            Err(error) => return Err(fs_error(error)),
        };
        for entry in entries {
            let path = match entry.map_err(fs_error) {
                Ok(entry) => entry.path(),
                Err(error) => return Err(error),
            };
            let metadata = match fs::symlink_metadata(&path).map_err(fs_error) {
                Ok(metadata) => metadata,
                Err(error) => {
                    return Err(error);
                }
            };
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                self.quarantine_invalid(
                    &path,
                    &mut report,
                    "namespace is not a plain directory".to_owned(),
                )?;
                continue;
            }
            let Some(key) = path.file_name().and_then(|value| value.to_str()) else {
                self.quarantine_invalid(
                    &path,
                    &mut report,
                    "namespace name is not valid UTF-8".to_owned(),
                )?;
                continue;
            };
            report.scanned += 1;
            let leased = self.is_leased(key)?;
            if leased {
                report.skipped_leased += 1;
                continue;
            }
            let namespace_lock = self.lock_namespace(key)?;
            if self.is_leased(key)? {
                report.skipped_leased += 1;
                drop(namespace_lock);
                continue;
            }
            let state = match walk(&path, config.max_entries) {
                Ok(state) => state,
                Err(error) => {
                    self.quarantine_invalid(&path, &mut report, error.to_string())?;
                    drop(namespace_lock);
                    continue;
                }
            };
            report.workspace_usage_bytes = report.workspace_usage_bytes.saturating_add(state.bytes);
            if now_ms.saturating_sub(state.newest_ms) < config.ttl_ms {
                report.skipped_recent += 1;
                drop(namespace_lock);
                continue;
            }
            if self.is_leased(key)? {
                report.skipped_leased += 1;
                drop(namespace_lock);
                continue;
            }
            fs::remove_dir_all(&path).map_err(fs_error)?;
            drop(namespace_lock);
            self.release_quota_for_key(key)?;
            report.deleted += 1;
            report.deleted_bytes = report.deleted_bytes.saturating_add(state.bytes);
        }
        Ok(report)
    }

    pub fn delete(
        &self,
        session_scope_id: Option<&str>,
    ) -> Result<SessionScratchDeleteOutcomeV1, SessionScratchErrorV1> {
        let key = session_scope_key(session_scope_id);
        let directory = self.session_directory(session_scope_id);
        if self.is_leased(&key)? {
            return Ok(SessionScratchDeleteOutcomeV1::SkippedLeased);
        }
        let namespace_lock = self.lock_namespace(&key)?;
        if self.is_leased(&key)? {
            drop(namespace_lock);
            return Ok(SessionScratchDeleteOutcomeV1::SkippedLeased);
        }
        let metadata = match fs::symlink_metadata(&directory) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(SessionScratchDeleteOutcomeV1::NotPresent);
            }
            Err(error) => return Err(fs_error(error)),
        };
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            drop(namespace_lock);
            return Err(SessionScratchErrorV1::NotPlainDirectory {
                path: directory.display().to_string(),
            });
        }
        fs::remove_dir_all(&directory).map_err(fs_error)?;
        drop(namespace_lock);
        self.release_quota_for_key(&key)?;
        Ok(SessionScratchDeleteOutcomeV1::Deleted)
    }

    fn quota_path(&self) -> PathBuf {
        self.root
            .join(".authority-quota")
            .join("session-scratch.json")
    }

    fn with_quota<T>(
        &self,
        workspace_cap: Option<u64>,
        operation: impl FnOnce(&mut QuotaBookV1) -> Result<T, QuotaErrorV1>,
    ) -> Result<T, SessionScratchErrorV1> {
        let mut quota = self
            .quota
            .lock()
            .map_err(|_| SessionScratchErrorV1::LeaseRegistryUnavailable)?;
        if quota.is_none() {
            let path = self.quota_path();
            let book = match workspace_cap {
                Some(cap) => QuotaBookV1::open(path, cap),
                None => QuotaBookV1::open_existing(path),
            }
            .map_err(quota_error)?;
            *quota = Some(book);
        }
        operation(
            quota
                .as_mut()
                .ok_or(SessionScratchErrorV1::LeaseRegistryUnavailable)?,
        )
        .map_err(quota_error)
    }

    fn release_quota_for_key(&self, key: &str) -> Result<(), SessionScratchErrorV1> {
        let path = self.quota_path();
        let quota_is_loaded = self
            .quota
            .lock()
            .map_err(|_| SessionScratchErrorV1::LeaseRegistryUnavailable)?
            .is_some();
        if !path.exists() && !quota_is_loaded {
            return Ok(());
        }
        self.with_quota(None, |quota| {
            quota.release_owner(&format!("session-scratch:{key}"))
        })
    }

    fn lock_namespace(&self, key: &str) -> Result<File, SessionScratchErrorV1> {
        let directory = self.root.join(LEASE_LOCK_DIR);
        ensure_directory(&directory)?;
        let path = directory.join(format!("{}.lock", key_digest(key)));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(fs_error)?;
        secure_private_path_permissions(&path)
            .map_err(|error| SessionScratchErrorV1::Filesystem(error.to_string()))?;
        file.lock_exclusive()
            .map_err(|error| SessionScratchErrorV1::LeaseUnavailable(error.to_string()))?;
        Ok(file)
    }

    fn create_lease_marker(&self, key: &str) -> Result<(PathBuf, File), SessionScratchErrorV1> {
        let directory = self.root.join(LEASE_MARKER_DIR);
        ensure_directory(&directory)?;
        let marker = directory.join(format!(
            "{}-{}-{}.lease",
            key_digest(key),
            std::process::id(),
            LEASE_SEQUENCE.fetch_add(1, Ordering::SeqCst)
        ));
        let marker_file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&marker)
            .map_err(fs_error)?;
        if let Err(error) = secure_private_path_permissions(&marker) {
            let _ = fs::remove_file(&marker);
            return Err(SessionScratchErrorV1::Filesystem(error.to_string()));
        }
        if let Err(error) = marker_file.lock_exclusive() {
            let _ = fs::remove_file(&marker);
            return Err(SessionScratchErrorV1::LeaseUnavailable(error.to_string()));
        }
        Ok((marker, marker_file))
    }

    fn lease_marker_exists(&self, key: &str) -> Result<bool, SessionScratchErrorV1> {
        let directory = self.root.join(LEASE_MARKER_DIR);
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(fs_error(error)),
        };
        let prefix = format!("{}-", key_digest(key));
        for entry in entries {
            let path = entry.map_err(fs_error)?.path();
            let metadata = fs::symlink_metadata(&path).map_err(fs_error)?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(SessionScratchErrorV1::NotPlainDirectory {
                    path: path.display().to_string(),
                });
            }
            if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(&prefix))
            {
                let marker = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&path)
                    .map_err(fs_error)?;
                match marker.try_lock_exclusive() {
                    Ok(()) => {
                        marker.unlock().map_err(|error| {
                            SessionScratchErrorV1::LeaseUnavailable(error.to_string())
                        })?;
                        fs::remove_file(&path).map_err(fs_error)?;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        return Ok(true);
                    }
                    Err(error) => {
                        return Err(SessionScratchErrorV1::LeaseUnavailable(error.to_string()));
                    }
                }
            }
        }
        Ok(false)
    }

    fn is_leased(&self, key: &str) -> Result<bool, SessionScratchErrorV1> {
        let local = self
            .leases
            .keys
            .lock()
            .map_err(|_| SessionScratchErrorV1::LeaseRegistryUnavailable)?
            .contains_key(key);
        Ok(local || self.lease_marker_exists(key)?)
    }

    fn quarantine_invalid(
        &self,
        path: &Path,
        report: &mut SessionScratchGcReportV1,
        reason: String,
    ) -> Result<(), SessionScratchErrorV1> {
        let quarantine = self.root.join(QUARANTINE_DIR);
        ensure_directory(&quarantine)?;
        let count = fs::read_dir(&quarantine).map_err(fs_error)?.count();
        if count >= MAX_QUARANTINE_ENTRIES {
            return Err(SessionScratchErrorV1::QuarantineFailed {
                path: path.display().to_string(),
                reason: "quarantine capacity exhausted".to_owned(),
            });
        }
        let name = format!(
            "{}-{}",
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("invalid"),
            LEASE_SEQUENCE.fetch_add(1, Ordering::SeqCst)
        );
        let destination = quarantine.join(name);
        fs::rename(path, &destination).map_err(|error| {
            SessionScratchErrorV1::QuarantineFailed {
                path: path.display().to_string(),
                reason: error.to_string(),
            }
        })?;
        report.skipped_invalid += 1;
        report.quarantined += 1;
        report
            .diagnostics
            .push(format!("quarantined {}: {reason}", path.display()));
        Ok(())
    }
}

fn key_digest(key: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(key.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn scratch_quota_profile(
    workspace_hard_bytes: u64,
) -> sigil_kernel::resource::ResourceQuotaProfileV1 {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"session-scratch-quota-v1");
    hasher.update(workspace_hard_bytes.to_be_bytes());
    sigil_kernel::resource::ResourceQuotaProfileV1 {
        class: sigil_kernel::resource::ResourceQuotaClassV1::SessionScratch,
        max_bytes: workspace_hard_bytes,
        max_entries: DEFAULT_MAX_ENTRIES as u64,
        max_open_holders: 1,
        max_age_ms: None,
        hard_runtime_enforcement_required: true,
        profile_hash: sigil_kernel::resource::CanonicalHash::from_bytes(hasher.finalize().into()),
    }
}

fn quota_error(error: QuotaErrorV1) -> SessionScratchErrorV1 {
    match error {
        QuotaErrorV1::ReservationExceeded { reserved, max, .. }
        | QuotaErrorV1::WorkspaceOvercommit {
            used: reserved,
            cap: max,
            ..
        } => SessionScratchErrorV1::QuotaExceeded {
            scope: SessionScratchQuotaScopeV1::Workspace,
            used: reserved,
            limit: max,
        },
        QuotaErrorV1::EntryExceeded { reserved, max, .. } => SessionScratchErrorV1::QuotaExceeded {
            scope: SessionScratchQuotaScopeV1::Session,
            used: reserved,
            limit: max,
        },
        other => SessionScratchErrorV1::Filesystem(other.to_string()),
    }
}

fn is_plain_directory(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
        .unwrap_or(false)
}

fn session_scope_key(session_scope_id: Option<&str>) -> String {
    session_scope_id
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("no-session")
        .to_owned()
}

fn ensure_directory(path: &Path) -> Result<(), SessionScratchErrorV1> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(SessionScratchErrorV1::NotPlainDirectory {
                path: path.display().to_string(),
            });
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(path).map_err(fs_error)?;
        }
        Err(error) => return Err(fs_error(error)),
    }
    secure_private_path_permissions(path)
        .map_err(|error| SessionScratchErrorV1::Filesystem(error.to_string()))
}

#[derive(Debug, Default, Clone, Copy)]
struct WalkState {
    bytes: u64,
    entries: usize,
    newest_ms: u64,
}

fn walk(root: &Path, max_entries: usize) -> Result<WalkState, SessionScratchErrorV1> {
    let mut state = WalkState::default();
    let mut pending = vec![root.to_path_buf()];
    let mut observed = 0usize;
    while let Some(path) = pending.pop() {
        if path != root {
            observed = observed.saturating_add(1);
            if observed > max_entries {
                return Err(SessionScratchErrorV1::EntryLimitExceeded {
                    limit: max_entries,
                    observed,
                });
            }
        }
        let metadata = fs::symlink_metadata(&path).map_err(fs_error)?;
        if metadata.file_type().is_symlink() {
            return Err(SessionScratchErrorV1::Symlink {
                path: path.display().to_string(),
            });
        }
        if metadata.is_file() {
            state.bytes = state.bytes.saturating_add(metadata.len());
            state.entries = state.entries.saturating_add(1);
            state.newest_ms = state.newest_ms.max(modified_ms(&metadata));
            continue;
        }
        if !metadata.is_dir() {
            return Err(SessionScratchErrorV1::UnsupportedEntry {
                path: path.display().to_string(),
            });
        }
        state.newest_ms = state.newest_ms.max(modified_ms(&metadata));
        let mut children = fs::read_dir(&path)
            .map_err(fs_error)?
            .map(|entry| entry.map(|entry| entry.path()).map_err(fs_error))
            .collect::<Result<Vec<_>, _>>()?;
        children.sort();
        pending.extend(children.into_iter().rev());
    }
    Ok(state)
}

fn modified_ms(metadata: &fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        metadata.mtime().max(0) as u64 * 1000 + metadata.mtime_nsec() as u64 / 1_000_000
    }
    #[cfg(not(unix))]
    {
        metadata
            .modified()
            .ok()
            .and_then(|value| value.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|value| value.as_millis().try_into().unwrap_or(u64::MAX))
            .unwrap_or(0)
    }
}

fn fs_error(error: std::io::Error) -> SessionScratchErrorV1 {
    SessionScratchErrorV1::Filesystem(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authority_owns_session_namespace_and_quota() {
        let temp = tempfile::tempdir().expect("temp");
        let authority = SessionScratchAuthorityV1::new(temp.path().join("scratch"));
        let provision = authority
            .ensure(Some("session-a"), 10, 20)
            .expect("provision");
        std::fs::write(provision.directory.join("data"), b"12345678901").expect("write");
        let error = authority
            .ensure(Some("session-a"), 10, 20)
            .expect_err("quota");
        assert!(matches!(
            error,
            SessionScratchErrorV1::QuotaExceeded {
                scope: SessionScratchQuotaScopeV1::Session,
                ..
            }
        ));
    }

    #[test]
    fn durable_scratch_quota_replays_across_authority_restart() {
        let temp = tempfile::tempdir().expect("temp");
        let root = temp.path().join("scratch");
        {
            let authority = SessionScratchAuthorityV1::new(&root);
            let provision = authority
                .ensure(Some("session-a"), 100, 1000)
                .expect("provision");
            std::fs::write(provision.directory.join("data"), b"durable").expect("write");
            authority
                .ensure(Some("session-a"), 100, 1000)
                .expect("record measured usage");
        }

        let restarted = SessionScratchAuthorityV1::new(&root);
        restarted
            .ensure(Some("session-a"), 100, 1000)
            .expect("replay and reconcile active owner");
        assert_eq!(
            restarted.delete(Some("session-a")).expect("delete"),
            SessionScratchDeleteOutcomeV1::Deleted
        );
        let reopened = SessionScratchAuthorityV1::new(&root);
        reopened
            .ensure(Some("session-a"), 100, 1000)
            .expect("released quota is reusable");
    }

    #[cfg(unix)]
    #[test]
    fn descendant_symlink_is_rejected_without_sibling_poisoning() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().expect("temp");
        let authority = SessionScratchAuthorityV1::new(temp.path().join("scratch"));
        let first = authority.ensure(Some("first"), 100, 1000).expect("first");
        let _second = authority.ensure(Some("second"), 100, 1000).expect("second");
        let target = temp.path().join("outside");
        std::fs::create_dir(&target).expect("target");
        symlink(&target, first.directory.join("escape")).expect("symlink");
        let error = authority
            .ensure(Some("first"), 100, 1000)
            .expect_err("symlink");
        assert!(matches!(error, SessionScratchErrorV1::Symlink { .. }));
        assert!(authority.session_directory(Some("second")).exists());
    }

    #[test]
    fn active_lease_blocks_delete() {
        let temp = tempfile::tempdir().expect("temp");
        let authority = SessionScratchAuthorityV1::new(temp.path().join("scratch"));
        authority
            .ensure(Some("session-a"), 100, 1000)
            .expect("provision");
        let _lease = authority.acquire(Some("session-a")).expect("lease");
        assert_eq!(
            authority.delete(Some("session-a")).expect("delete"),
            SessionScratchDeleteOutcomeV1::SkippedLeased
        );
    }

    #[test]
    fn lease_marker_creation_failure_is_fail_closed() {
        let temp = tempfile::tempdir().expect("temp");
        let root = temp.path().join("scratch");
        std::fs::write(&root, b"not a directory").expect("root file");
        let authority = SessionScratchAuthorityV1::new(root);
        assert!(authority.acquire(Some("session-a")).is_err());
    }

    #[test]
    fn lease_marker_blocks_gc_after_authority_restarts() {
        let temp = tempfile::tempdir().expect("temp");
        let root = temp.path().join("scratch");
        let authority = SessionScratchAuthorityV1::new(&root);
        authority
            .ensure(Some("session-a"), 100, 1000)
            .expect("provision");
        let lease = authority.acquire(Some("session-a")).expect("lease");

        let restarted = SessionScratchAuthorityV1::new(&root);
        let report = restarted
            .gc(SessionScratchGcConfigV1::default(), u64::MAX)
            .expect("gc");
        assert_eq!(report.skipped_leased, 1);
        assert_eq!(report.deleted, 0);
        drop(lease);
        let report = restarted
            .gc(SessionScratchGcConfigV1::default(), u64::MAX)
            .expect("gc after lease release");
        assert_eq!(report.deleted, 1);
    }

    #[cfg(unix)]
    #[test]
    fn gc_quarantines_invalid_namespace_instead_of_silently_skipping() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().expect("temp");
        let root = temp.path().join("scratch");
        let authority = SessionScratchAuthorityV1::new(&root);
        authority
            .ensure(Some("valid"), 100, 1000)
            .expect("provision");
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).expect("outside");
        let invalid = root.join(SESSION_NAMESPACE_DIR).join("invalid");
        symlink(&outside, &invalid).expect("invalid symlink");

        let report = authority
            .gc(SessionScratchGcConfigV1::default(), u64::MAX)
            .expect("gc");
        assert_eq!(report.quarantined, 1);
        assert_eq!(report.skipped_invalid, 1);
        assert!(
            !invalid.exists(),
            "invalid namespace moved out of the scan root"
        );
        assert_eq!(
            fs::read_dir(root.join(QUARANTINE_DIR))
                .expect("quarantine")
                .count(),
            1
        );
    }
}
