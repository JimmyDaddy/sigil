//! RFC-0062 14.1: session-scoped model-visible scratch namespace.
//!
//! `$SIGIL_SCRATCH_DIR` is the one model-visible temporary directory. Every durable session gets
//! an independent namespace derived from its stable session scope id:
//!
//! ```text
//! <scratch root>/sessions/<session scope id>
//! ```
//!
//! The workspace-wide `scratch_root` is only the base; it is never handed to a child as the
//! scratch directory itself. Namespaces are created owner-only before any file can be written,
//! metered against a per-session quota plus a workspace hard cap, and reclaimed by TTL GC that
//! never deletes a namespace with an active tool or terminal lease.
//!
//! This module is deliberately free of session/artifact kernel types: it only needs the session
//! scope id string and the base path, so `bash`, `terminal_start`, TUI maintenance and the
//! Desktop/application runtime share exactly one derivation rule.

use std::{
    collections::{BTreeSet, HashMap},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

#[cfg(not(unix))]
use std::time::SystemTime;

use anyhow::{Context, Result, bail};
use sigil_kernel::secure_private_path_permissions;

use crate::constants::{
    NO_SESSION_SCRATCH_KEY, SCRATCH_NAMESPACE_TTL_MS, SCRATCH_QUOTA_PER_SESSION_BYTES,
    SCRATCH_QUOTA_WORKSPACE_HARD_BYTES, SCRATCH_WALK_MAX_DEPTH, SCRATCH_WALK_MAX_ENTRIES,
    SESSION_SCRATCH_NAMESPACE_DIR,
};

/// Stable, filesystem-safe namespace key for one session scope. Falls back to a fixed
/// `no-session` key for direct tool invocations without a durable session.
#[must_use]
pub fn session_scratch_key(session_scope_id: Option<&str>) -> String {
    match session_scope_id {
        Some(id) if !id.trim().is_empty() => id.to_owned(),
        _ => NO_SESSION_SCRATCH_KEY.to_owned(),
    }
}

/// RFC-0062 14.1: the session-scoped scratch directory for one session scope.
#[must_use]
pub fn session_scratch_dir(scratch_root: &Path, session_scope_id: Option<&str>) -> PathBuf {
    scratch_root
        .join(SESSION_SCRATCH_NAMESPACE_DIR)
        .join(session_scratch_key(session_scope_id))
}

/// Capacity limits for the scratch namespaces of one workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScratchQuota {
    /// Per-session namespace cap, enforced before every scratch-using spawn.
    pub per_session_bytes: u64,
    /// Aggregate cap across all session namespaces under the same workspace scratch root.
    pub workspace_hard_bytes: u64,
}

impl Default for ScratchQuota {
    fn default() -> Self {
        Self {
            per_session_bytes: SCRATCH_QUOTA_PER_SESSION_BYTES,
            workspace_hard_bytes: SCRATCH_QUOTA_WORKSPACE_HARD_BYTES,
        }
    }
}

/// Which quota bound was exceeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScratchQuotaScope {
    Session,
    Workspace,
}

impl ScratchQuotaScope {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Workspace => "workspace",
        }
    }
}

impl std::fmt::Display for ScratchQuotaScope {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Structured, diagnosable quota failure. The tool layer maps this to a recoverable tool error;
/// it never falls back to the system temp directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "scratch quota exceeded ({scope}): {usage_bytes} bytes used of {quota_bytes} bytes allowed; \
     delete files under $SIGIL_SCRATCH_DIR or start a new session"
)]
pub struct ScratchQuotaExceededError {
    pub scope: ScratchQuotaScope,
    pub usage_bytes: u64,
    pub quota_bytes: u64,
}

/// Deterministic usage snapshot of one workspace scratch root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ScratchUsage {
    pub session_bytes: u64,
    pub workspace_bytes: u64,
    pub session_entry_count: usize,
    pub workspace_entry_count: usize,
}

/// Outcome of provisioning one session namespace before a scratch-using spawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionScratchProvision {
    pub dir: PathBuf,
    pub usage: ScratchUsage,
}

/// Walk state for the bounded deterministic scratch sweep.
#[derive(Debug, Clone, Copy, Default)]
struct WalkState {
    bytes: u64,
    entries: usize,
    newest_ms: u64,
}

/// In-process lease registry keyed by session namespace key.
///
/// Deletion is executed under the same lock as lease acquisition, so GC can never delete a
/// namespace between a tool's lease acquisition and its registration.
#[derive(Debug, Default)]
pub struct ScratchNamespaceLeaseRegistry {
    inner: Mutex<BTreeSet<String>>,
}

/// RAII lease guard; releases the namespace lease on drop.
#[derive(Debug)]
pub struct ScratchNamespaceLease {
    registry: Arc<ScratchNamespaceLeaseRegistry>,
    key: String,
}

impl Drop for ScratchNamespaceLease {
    fn drop(&mut self) {
        if let Ok(mut inner) = self.registry.inner.lock() {
            inner.remove(&self.key);
        }
    }
}

impl ScratchNamespaceLeaseRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Acquires an exclusive-in-process lease for one session namespace.
    #[must_use]
    pub fn acquire(self: &Arc<Self>, session_key: &str) -> ScratchNamespaceLease {
        if let Ok(mut inner) = self.inner.lock() {
            inner.insert(session_key.to_owned());
        }
        ScratchNamespaceLease {
            registry: Arc::clone(self),
            key: session_key.to_owned(),
        }
    }

    #[must_use]
    pub fn is_leased(&self, session_key: &str) -> bool {
        self.inner
            .lock()
            .map(|inner| inner.contains(session_key))
            .unwrap_or(true)
    }

    /// Deletes `session_key` only when no lease is held. The deletion runs while the registry
    /// lock is held, so a concurrent `acquire` cannot race between the check and the delete.
    ///
    /// Returns `Ok(true)` when the namespace was deleted, `Ok(false)` when it is leased or
    /// absent and no deletion was attempted.
    pub fn delete_if_unleased(
        &self,
        session_key: &str,
        delete: impl FnOnce() -> Result<()>,
    ) -> Result<bool> {
        let inner = self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("scratch lease registry lock poisoned"))?;
        if inner.contains(session_key) {
            return Ok(false);
        }
        delete()?;
        Ok(true)
    }
}

/// Task-scoped terminal scratch leases: one lease per live terminal task, backed by the shared
/// namespace registry. Released when the task terminalizes or is cancelled.
#[derive(Debug, Default)]
pub struct ScratchTaskLeaseRegistry {
    inner: Mutex<HashMap<String, (String, ScratchNamespaceLease)>>,
}

impl ScratchTaskLeaseRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a live terminal task against its session namespace. Idempotent per task id.
    pub fn register(
        &self,
        task_id: &str,
        session_key: &str,
        namespaces: &Arc<ScratchNamespaceLeaseRegistry>,
    ) {
        let lease = namespaces.acquire(session_key);
        if let Ok(mut inner) = self.inner.lock() {
            inner.insert(task_id.to_owned(), (session_key.to_owned(), lease));
        }
    }

    /// Releases the lease of one terminal task. Idempotent.
    pub fn release(&self, task_id: &str) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.remove(task_id);
        }
    }

    #[must_use]
    pub fn is_leased(&self, task_id: &str) -> bool {
        self.inner
            .lock()
            .map(|inner| inner.contains_key(task_id))
            .unwrap_or(true)
    }
}

/// Shared scratch authority for one process: namespace leases used by GC and task leases held
/// by live terminal tasks.
#[derive(Debug, Clone, Default)]
pub struct ScratchNamespaceControl {
    pub namespaces: Arc<ScratchNamespaceLeaseRegistry>,
    pub tasks: Arc<ScratchTaskLeaseRegistry>,
}

impl ScratchNamespaceControl {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg(not(unix))]
fn system_time_ms(time: Option<SystemTime>) -> u64 {
    time.and_then(|value| value.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis().try_into().unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn entry_modified_ms(metadata: &fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        metadata.mtime() as u64 * 1000 + (metadata.mtime_nsec() / 1_000_000) as u64
    }
    #[cfg(not(unix))]
    {
        system_time_ms(metadata.modified().ok())
    }
}

/// Bounded deterministic walk of one namespace. Refuses symlinks and any entry that cannot be
/// measured, so quota/activity numbers are never silently fabricated.
fn walk_namespace(namespace_root: &Path, max_entries: usize, max_depth: u32) -> Result<WalkState> {
    let mut state = WalkState::default();
    walk_entries(
        namespace_root,
        namespace_root,
        max_entries,
        max_depth,
        &mut state,
    )?;
    Ok(state)
}

fn walk_entries(
    root: &Path,
    current: &Path,
    max_entries: usize,
    max_depth: u32,
    state: &mut WalkState,
) -> Result<()> {
    let metadata = fs::symlink_metadata(current)
        .with_context(|| format!("failed to inspect scratch entry {}", current.display()))?;
    if metadata.file_type().is_symlink() {
        bail!(
            "scratch namespace contains a symlink, refusing to measure: {}",
            current.display()
        );
    }
    if metadata.is_file() {
        state.bytes = state.bytes.saturating_add(metadata.len());
        state.entries += 1;
        state.newest_ms = state.newest_ms.max(entry_modified_ms(&metadata));
        if state.entries > max_entries {
            bail!(
                "scratch namespace walk exceeded the entry bound at {}",
                root.display()
            );
        }
        return Ok(());
    }
    if !metadata.is_dir() {
        bail!(
            "scratch namespace contains a non-file, non-directory entry: {}",
            current.display()
        );
    }
    state.newest_ms = state.newest_ms.max(entry_modified_ms(&metadata));
    let entries = fs::read_dir(current)
        .with_context(|| format!("failed to read scratch directory {}", current.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| {
            format!(
                "failed to read scratch directory entry in {}",
                current.display()
            )
        })?;
        let path = entry.path();
        if path == root {
            continue;
        }
        let entry_metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("failed to inspect scratch entry {}", path.display()))?;
        if entry_metadata.file_type().is_symlink() {
            bail!(
                "scratch namespace contains a symlink, refusing to measure: {}",
                path.display()
            );
        }
        if entry_metadata.is_dir() {
            if max_depth == 0 {
                bail!(
                    "scratch namespace walk exceeded the depth bound at {}",
                    path.display()
                );
            }
            walk_entries(root, &path, max_entries, max_depth - 1, state)?;
        } else {
            state.bytes = state.bytes.saturating_add(entry_metadata.len());
            state.entries += 1;
            state.newest_ms = state.newest_ms.max(entry_modified_ms(&entry_metadata));
        }
        if state.entries > max_entries {
            bail!(
                "scratch namespace walk exceeded the entry bound at {}",
                root.display()
            );
        }
    }
    Ok(())
}

/// Measures usage of one session namespace plus the aggregate workspace scratch usage.
///
/// # Errors
///
/// Returns an error for symlinks, unreadable entries or walk bounds; quota numbers are never
/// silently guessed.
pub fn measure_scratch_usage(scratch_root: &Path, session_key: &str) -> Result<ScratchUsage> {
    let sessions_root = scratch_root.join(SESSION_SCRATCH_NAMESPACE_DIR);
    let mut usage = ScratchUsage::default();
    if !sessions_root.is_dir() {
        return Ok(usage);
    }
    let namespace_root = sessions_root.join(session_key);
    if namespace_root.is_dir() {
        let session_state = walk_namespace(
            &namespace_root,
            SCRATCH_WALK_MAX_ENTRIES,
            SCRATCH_WALK_MAX_DEPTH,
        )?;
        usage.session_bytes = session_state.bytes;
        usage.session_entry_count = session_state.entries;
    }
    for entry in fs::read_dir(&sessions_root).with_context(|| {
        format!(
            "failed to read scratch sessions dir {}",
            sessions_root.display()
        )
    })? {
        let path = entry
            .with_context(|| {
                format!(
                    "failed to read scratch sessions dir entry in {}",
                    sessions_root.display()
                )
            })?
            .path();
        let metadata = fs::symlink_metadata(&path).with_context(|| {
            format!("failed to inspect scratch session entry {}", path.display())
        })?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            bail!(
                "scratch sessions dir contains an invalid entry, refusing to measure: {}",
                path.display()
            );
        }
        if path == namespace_root {
            continue;
        }
        let state = walk_namespace(&path, SCRATCH_WALK_MAX_ENTRIES, SCRATCH_WALK_MAX_DEPTH)?;
        usage.workspace_bytes = usage.workspace_bytes.saturating_add(state.bytes);
        usage.workspace_entry_count += state.entries;
    }
    usage.workspace_bytes = usage.workspace_bytes.saturating_add(usage.session_bytes);
    usage.workspace_entry_count += usage.session_entry_count;
    Ok(usage)
}

/// Creates (or revalidates) the session scratch namespace with owner-only permissions and
/// enforces the capacity quota before any child can write into it.
///
/// # Errors
///
/// Returns [`ScratchQuotaExceededError`] when the session or workspace quota is already reached,
/// and a contextual error for symlink attacks or permission hardening failures. Never falls back
/// to the system temp directory.
pub fn ensure_session_scratch(
    scratch_root: &Path,
    session_scope_id: Option<&str>,
    quota: &ScratchQuota,
) -> Result<SessionScratchProvision> {
    let session_key = session_scratch_key(session_scope_id);
    let sessions_root = scratch_root.join(SESSION_SCRATCH_NAMESPACE_DIR);
    let namespace_root = sessions_root.join(&session_key);
    for (index, path) in [
        scratch_root,
        sessions_root.as_path(),
        namespace_root.as_path(),
    ]
    .into_iter()
    .enumerate()
    {
        match fs::symlink_metadata(path) {
            Ok(metadata) => {
                if !metadata.is_dir() || metadata.file_type().is_symlink() {
                    bail!("scratch path is not a plain directory: {}", path.display());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if index == 0 {
                    // The scratch root may sit under cache directories that do not exist yet.
                    fs::create_dir_all(path).with_context(|| {
                        format!("failed to create scratch dir {}", path.display())
                    })?;
                } else {
                    fs::create_dir(path).with_context(|| {
                        format!("failed to create scratch dir {}", path.display())
                    })?;
                }
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to inspect scratch dir {}", path.display()));
            }
        }
        secure_private_path_permissions(path)?;
    }
    let usage = measure_scratch_usage(scratch_root, &session_key)?;
    if usage.session_bytes > quota.per_session_bytes {
        return Err(ScratchQuotaExceededError {
            scope: ScratchQuotaScope::Session,
            usage_bytes: usage.session_bytes,
            quota_bytes: quota.per_session_bytes,
        }
        .into());
    }
    if usage.workspace_bytes > quota.workspace_hard_bytes {
        return Err(ScratchQuotaExceededError {
            scope: ScratchQuotaScope::Workspace,
            usage_bytes: usage.workspace_bytes,
            quota_bytes: quota.workspace_hard_bytes,
        }
        .into());
    }
    Ok(SessionScratchProvision {
        dir: namespace_root,
        usage,
    })
}

/// TTL/GC configuration for one workspace scratch root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScratchGcConfig {
    pub ttl_ms: u64,
}

impl Default for ScratchGcConfig {
    fn default() -> Self {
        Self {
            ttl_ms: SCRATCH_NAMESPACE_TTL_MS,
        }
    }
}

/// Structured result of one scratch GC sweep.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScratchGcReport {
    pub scanned: usize,
    pub deleted: usize,
    pub skipped_leased: usize,
    pub skipped_recent: usize,
    pub skipped_invalid: usize,
    pub deleted_bytes: u64,
    pub workspace_usage_bytes: u64,
    /// Bounded diagnostics for namespaces that could not be measured or deleted.
    pub diagnostics: Vec<String>,
}

/// TTL sweep over all session namespaces under one scratch root.
///
/// A namespace is deleted only when it has no active lease (checked atomically with the
/// deletion) and its last measured activity is older than `ttl_ms`. Failures are collected as
/// bounded diagnostics instead of aborting the sweep.
pub fn gc_scratch_namespaces(
    scratch_root: &Path,
    control: &ScratchNamespaceControl,
    config: &ScratchGcConfig,
    now_ms: u64,
) -> Result<ScratchGcReport> {
    let mut report = ScratchGcReport::default();
    let sessions_root = scratch_root.join(SESSION_SCRATCH_NAMESPACE_DIR);
    let entries = match fs::read_dir(&sessions_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(report),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "failed to read scratch sessions dir {}",
                    sessions_root.display()
                )
            });
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                report.skipped_invalid += 1;
                report
                    .diagnostics
                    .push(format!("scratch GC: unreadable session entry: {error}"));
                continue;
            }
        };
        let path = entry.path();
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) => {
                report.skipped_invalid += 1;
                report.diagnostics.push(format!(
                    "scratch GC: cannot inspect {}: {error}",
                    path.display()
                ));
                continue;
            }
        };
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            report.skipped_invalid += 1;
            report.diagnostics.push(format!(
                "scratch GC: invalid namespace entry {} (not a plain directory)",
                path.display()
            ));
            continue;
        }
        let Some(key) = path.file_name().and_then(|name| name.to_str()) else {
            report.skipped_invalid += 1;
            continue;
        };
        report.scanned += 1;
        if control.namespaces.is_leased(key) {
            report.skipped_leased += 1;
            continue;
        }
        let state = match walk_namespace(&path, SCRATCH_WALK_MAX_ENTRIES, SCRATCH_WALK_MAX_DEPTH) {
            Ok(state) => state,
            Err(error) => {
                report.skipped_invalid += 1;
                report.diagnostics.push(format!("scratch GC: {error:#}"));
                continue;
            }
        };
        report.workspace_usage_bytes = report.workspace_usage_bytes.saturating_add(state.bytes);
        if now_ms.saturating_sub(state.newest_ms) < config.ttl_ms {
            report.skipped_recent += 1;
            continue;
        }
        let key_for_delete = key.to_owned();
        let path_for_delete = path.clone();
        let deleted = control.namespaces.delete_if_unleased(&key_for_delete, || {
            fs::remove_dir_all(&path_for_delete).with_context(|| {
                format!(
                    "failed to remove expired scratch namespace {}",
                    path_for_delete.display()
                )
            })
        })?;
        if deleted {
            report.deleted += 1;
            report.deleted_bytes = report.deleted_bytes.saturating_add(state.bytes);
        } else {
            report.skipped_leased += 1;
        }
    }
    Ok(report)
}

/// Outcome of deleting one session scratch namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScratchDeleteOutcome {
    Deleted,
    NotPresent,
    SkippedLeased,
}

/// Deletes the scratch namespace of one session (session delete / fork cleanup). The namespace
/// is removed only when no active tool or terminal lease is held.
///
/// # Errors
///
/// Returns an error when the namespace exists but cannot be removed safely.
pub fn delete_session_scratch_namespace(
    scratch_root: &Path,
    session_scope_id: Option<&str>,
    control: &ScratchNamespaceControl,
) -> Result<ScratchDeleteOutcome> {
    let key = session_scratch_key(session_scope_id);
    let namespace_root = session_scratch_dir(scratch_root, session_scope_id);
    let metadata = match fs::symlink_metadata(&namespace_root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ScratchDeleteOutcome::NotPresent);
        }
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "failed to inspect scratch namespace {}",
                    namespace_root.display()
                )
            });
        }
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        bail!(
            "scratch namespace is not a plain directory: {}",
            namespace_root.display()
        );
    }
    let deleted = control.namespaces.delete_if_unleased(&key, || {
        fs::remove_dir_all(&namespace_root).with_context(|| {
            format!(
                "failed to remove scratch namespace {}",
                namespace_root.display()
            )
        })
    })?;
    Ok(if deleted {
        ScratchDeleteOutcome::Deleted
    } else {
        ScratchDeleteOutcome::SkippedLeased
    })
}

#[cfg(test)]
#[path = "tests/scratch_namespace_tests.rs"]
mod tests;
