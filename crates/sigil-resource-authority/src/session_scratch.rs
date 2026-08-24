//! RFC-0071: Resource Authority owner for the persistent SessionScratch namespace.
//!
//! The authority owns namespace allocation, no-follow measurement, quota admission, leases and
//! cleanup. Consumers receive only the exact directory selected for the admitted session scope;
//! they do not derive or create sibling roots themselves.

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

#[cfg(not(unix))]
use std::time::SystemTime;

use sigil_kernel::secure_private_path_permissions;

const SESSION_NAMESPACE_DIR: &str = "sessions";
const DEFAULT_MAX_ENTRIES: usize = 250_000;

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
    keys: Mutex<BTreeSet<String>>,
}

#[derive(Debug)]
pub struct SessionScratchLeaseV1 {
    registry: Arc<SessionScratchLeaseRegistryV1>,
    key: String,
}

impl Drop for SessionScratchLeaseV1 {
    fn drop(&mut self) {
        if let Ok(mut keys) = self.registry.keys.lock() {
            keys.remove(&self.key);
        }
    }
}

#[derive(Debug, Clone)]
pub struct SessionScratchAuthorityV1 {
    root: PathBuf,
    leases: Arc<SessionScratchLeaseRegistryV1>,
}

impl SessionScratchAuthorityV1 {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            leases: Arc::new(SessionScratchLeaseRegistryV1::default()),
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

    pub fn acquire(&self, session_scope_id: Option<&str>) -> SessionScratchLeaseV1 {
        let key = session_scope_key(session_scope_id);
        if let Ok(mut keys) = self.leases.keys.lock() {
            keys.insert(key.clone());
        }
        SessionScratchLeaseV1 {
            registry: Arc::clone(&self.leases),
            key,
        }
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
        let session = if session_directory.is_dir() {
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
                Err(error) => {
                    report.skipped_invalid += 1;
                    report.diagnostics.push(error.to_string());
                    continue;
                }
            };
            let metadata = match fs::symlink_metadata(&path).map_err(fs_error) {
                Ok(metadata) => metadata,
                Err(error) => {
                    report.skipped_invalid += 1;
                    report.diagnostics.push(error.to_string());
                    continue;
                }
            };
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                report.skipped_invalid += 1;
                report
                    .diagnostics
                    .push(format!("invalid namespace {}", path.display()));
                continue;
            }
            let Some(key) = path.file_name().and_then(|value| value.to_str()) else {
                report.skipped_invalid += 1;
                continue;
            };
            report.scanned += 1;
            let leased = self
                .leases
                .keys
                .lock()
                .map_err(|_| SessionScratchErrorV1::LeaseRegistryUnavailable)?
                .contains(key);
            if leased {
                report.skipped_leased += 1;
                continue;
            }
            let state = match walk(&path, config.max_entries) {
                Ok(state) => state,
                Err(error) => {
                    report.skipped_invalid += 1;
                    report.diagnostics.push(error.to_string());
                    continue;
                }
            };
            report.workspace_usage_bytes = report.workspace_usage_bytes.saturating_add(state.bytes);
            if now_ms.saturating_sub(state.newest_ms) < config.ttl_ms {
                report.skipped_recent += 1;
                continue;
            }
            let mut keys = self
                .leases
                .keys
                .lock()
                .map_err(|_| SessionScratchErrorV1::LeaseRegistryUnavailable)?;
            if keys.contains(key) {
                report.skipped_leased += 1;
                continue;
            }
            fs::remove_dir_all(&path).map_err(fs_error)?;
            keys.remove(key);
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
        let metadata = match fs::symlink_metadata(&directory) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(SessionScratchDeleteOutcomeV1::NotPresent);
            }
            Err(error) => return Err(fs_error(error)),
        };
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(SessionScratchErrorV1::NotPlainDirectory {
                path: directory.display().to_string(),
            });
        }
        let mut keys = self
            .leases
            .keys
            .lock()
            .map_err(|_| SessionScratchErrorV1::LeaseRegistryUnavailable)?;
        if keys.contains(&key) {
            return Ok(SessionScratchDeleteOutcomeV1::SkippedLeased);
        }
        fs::remove_dir_all(&directory).map_err(fs_error)?;
        keys.remove(&key);
        Ok(SessionScratchDeleteOutcomeV1::Deleted)
    }
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
        let _lease = authority.acquire(Some("session-a"));
        assert_eq!(
            authority.delete(Some("session-a")).expect("delete"),
            SessionScratchDeleteOutcomeV1::SkippedLeased
        );
    }
}
