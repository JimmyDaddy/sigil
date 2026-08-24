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
use serde_json::json;
use sigil_kernel::{ToolErrorKind, ToolResult, secure_private_path_permissions};

use crate::constants::{
    NO_SESSION_SCRATCH_KEY, SCRATCH_NAMESPACE_TTL_MS, SCRATCH_QUOTA_PER_SESSION_BYTES,
    SCRATCH_QUOTA_WORKSPACE_HARD_BYTES, SCRATCH_WALK_MAX_ENTRIES, SESSION_SCRATCH_NAMESPACE_DIR,
    WORKSPACE_TEMP_ROOT,
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
     ask the user to reset scratch storage or remove unneeded scratch files"
)]
pub struct ScratchQuotaExceededError {
    pub scope: ScratchQuotaScope,
    pub usage_bytes: u64,
    pub quota_bytes: u64,
}

/// Stable failures produced while measuring a scratch namespace.
///
/// Paths are relative to the session namespace so these errors can be projected to a tool result
/// without disclosing the host cache layout.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScratchMeasurementError {
    #[error(
        "scratch namespace measurement exceeded the {limit}-entry safety bound after at least {observed_entries} entries"
    )]
    EntryLimitExceeded {
        limit: usize,
        observed_entries: usize,
    },
    #[error("scratch namespace contains a symlink at {relative_path}")]
    Symlink { relative_path: String },
    #[error("scratch namespace contains an unsupported filesystem entry at {relative_path}")]
    UnsupportedEntry { relative_path: String },
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

/// Physical SessionScratch owner seam. Production runtime composition supplies the authority
/// implementation; the local implementation is retained only for compatibility fixtures and
/// isolated tests. Consumers use this seam instead of deriving roots or performing filesystem
/// lifecycle operations themselves.
pub trait ScratchNamespaceProvider: Send + Sync + std::fmt::Debug {
    fn acquire(&self, session_key: &str) -> Box<dyn ScratchNamespaceProviderLease>;
    fn session_scratch_dir(&self, session_scope_id: Option<&str>) -> PathBuf;
    fn ensure_session_scratch(
        &self,
        session_scope_id: Option<&str>,
        quota: &ScratchQuota,
    ) -> Result<SessionScratchProvision>;
    fn measure_scratch_usage(&self, session_key: &str) -> Result<ScratchUsage>;
    fn gc_scratch_namespaces(
        &self,
        control: &ScratchNamespaceControl,
        config: &ScratchGcConfig,
        now_ms: u64,
    ) -> Result<ScratchGcReport>;
    fn delete_session_scratch_namespace(
        &self,
        session_scope_id: Option<&str>,
        control: &ScratchNamespaceControl,
    ) -> Result<ScratchDeleteOutcome>;
}

/// Provider-owned lease held for the lifetime of one tool or terminal task.
pub trait ScratchNamespaceProviderLease: Send + Sync + std::fmt::Debug {}

#[derive(Debug)]
struct NoopScratchNamespaceProviderLease;

impl ScratchNamespaceProviderLease for NoopScratchNamespaceProviderLease {}

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
#[derive(Debug)]
pub struct ScratchNamespaceLeaseRegistry {
    inner: Mutex<BTreeSet<String>>,
    provider: Option<Arc<dyn ScratchNamespaceProvider>>,
}

/// RAII lease guard; releases the namespace lease on drop.
#[derive(Debug)]
pub struct ScratchNamespaceLease {
    registry: Arc<ScratchNamespaceLeaseRegistry>,
    key: String,
    _provider_lease: Option<Box<dyn ScratchNamespaceProviderLease>>,
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
        Self {
            inner: Mutex::new(BTreeSet::new()),
            provider: None,
        }
    }

    #[must_use]
    pub fn with_provider(provider: Arc<dyn ScratchNamespaceProvider>) -> Self {
        Self {
            inner: Mutex::new(BTreeSet::new()),
            provider: Some(provider),
        }
    }

    /// Acquires an exclusive-in-process lease for one session namespace.
    #[must_use]
    pub fn acquire(self: &Arc<Self>, session_key: &str) -> ScratchNamespaceLease {
        if let Ok(mut inner) = self.inner.lock() {
            inner.insert(session_key.to_owned());
        }
        let provider_lease = self
            .provider
            .as_ref()
            .map(|provider| provider.acquire(session_key));
        ScratchNamespaceLease {
            registry: Arc::clone(self),
            key: session_key.to_owned(),
            _provider_lease: provider_lease,
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

impl Default for ScratchNamespaceLeaseRegistry {
    fn default() -> Self {
        Self::new()
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
#[derive(Debug, Clone)]
pub struct ScratchNamespaceControl {
    provider: Arc<dyn ScratchNamespaceProvider>,
    pub namespaces: Arc<ScratchNamespaceLeaseRegistry>,
    pub tasks: Arc<ScratchTaskLeaseRegistry>,
}

#[derive(Debug, Clone)]
struct LocalScratchNamespaceProvider {
    root: PathBuf,
}

impl ScratchNamespaceProvider for LocalScratchNamespaceProvider {
    fn acquire(&self, _session_key: &str) -> Box<dyn ScratchNamespaceProviderLease> {
        Box::new(NoopScratchNamespaceProviderLease)
    }

    fn session_scratch_dir(&self, session_scope_id: Option<&str>) -> PathBuf {
        session_scratch_dir(&self.root, session_scope_id)
    }

    fn ensure_session_scratch(
        &self,
        session_scope_id: Option<&str>,
        quota: &ScratchQuota,
    ) -> Result<SessionScratchProvision> {
        ensure_session_scratch(&self.root, session_scope_id, quota)
    }

    fn measure_scratch_usage(&self, session_key: &str) -> Result<ScratchUsage> {
        measure_scratch_usage(&self.root, session_key)
    }

    fn gc_scratch_namespaces(
        &self,
        control: &ScratchNamespaceControl,
        config: &ScratchGcConfig,
        now_ms: u64,
    ) -> Result<ScratchGcReport> {
        gc_scratch_namespaces(&self.root, control, config, now_ms)
    }

    fn delete_session_scratch_namespace(
        &self,
        session_scope_id: Option<&str>,
        control: &ScratchNamespaceControl,
    ) -> Result<ScratchDeleteOutcome> {
        delete_session_scratch_namespace(&self.root, session_scope_id, control)
    }
}

impl ScratchNamespaceControl {
    #[must_use]
    pub fn new() -> Self {
        Self::for_local_root(PathBuf::from(WORKSPACE_TEMP_ROOT))
    }

    #[must_use]
    pub fn for_local_root(root: impl Into<PathBuf>) -> Self {
        Self::from_provider(Arc::new(LocalScratchNamespaceProvider {
            root: root.into(),
        }))
    }

    #[must_use]
    pub fn from_provider(provider: Arc<dyn ScratchNamespaceProvider>) -> Self {
        let namespaces = Arc::new(ScratchNamespaceLeaseRegistry::with_provider(Arc::clone(
            &provider,
        )));
        Self {
            provider,
            namespaces,
            tasks: Arc::new(ScratchTaskLeaseRegistry::new()),
        }
    }

    #[must_use]
    pub fn session_scratch_dir(&self, session_scope_id: Option<&str>) -> PathBuf {
        self.provider.session_scratch_dir(session_scope_id)
    }

    pub fn ensure_session_scratch(
        &self,
        session_scope_id: Option<&str>,
        quota: &ScratchQuota,
    ) -> Result<SessionScratchProvision> {
        self.provider
            .ensure_session_scratch(session_scope_id, quota)
    }

    pub fn measure_scratch_usage(&self, session_key: &str) -> Result<ScratchUsage> {
        self.provider.measure_scratch_usage(session_key)
    }

    pub fn gc_scratch_namespaces(
        &self,
        config: &ScratchGcConfig,
        now_ms: u64,
    ) -> Result<ScratchGcReport> {
        self.provider.gc_scratch_namespaces(self, config, now_ms)
    }

    pub fn delete_session_scratch_namespace(
        &self,
        session_scope_id: Option<&str>,
    ) -> Result<ScratchDeleteOutcome> {
        self.provider
            .delete_session_scratch_namespace(session_scope_id, self)
    }
}

impl Default for ScratchNamespaceControl {
    fn default() -> Self {
        Self::new()
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

/// Entry-bounded deterministic walk of one namespace. Refuses symlinks and any entry that cannot
/// be measured, so quota/activity numbers are never silently fabricated. Directory depth is not a
/// validity constraint; traversal work is bounded by the total entry budget instead.
fn walk_namespace(namespace_root: &Path, max_entries: usize) -> Result<WalkState> {
    let mut state = WalkState::default();
    let mut pending = vec![namespace_root.to_path_buf()];
    let mut observed_entries = 0usize;
    while let Some(current) = pending.pop() {
        if current != namespace_root {
            observed_entries = observed_entries.saturating_add(1);
            if observed_entries > max_entries {
                return Err(ScratchMeasurementError::EntryLimitExceeded {
                    limit: max_entries,
                    observed_entries,
                }
                .into());
            }
        }
        let metadata = fs::symlink_metadata(&current)
            .with_context(|| format!("failed to inspect scratch entry {}", current.display()))?;
        let relative_path = scratch_relative_label(namespace_root, &current);
        if metadata.file_type().is_symlink() {
            return Err(ScratchMeasurementError::Symlink { relative_path }.into());
        }
        if metadata.is_file() {
            state.bytes = state.bytes.saturating_add(metadata.len());
            state.entries = state.entries.saturating_add(1);
            state.newest_ms = state.newest_ms.max(entry_modified_ms(&metadata));
            continue;
        }
        if !metadata.is_dir() {
            return Err(ScratchMeasurementError::UnsupportedEntry { relative_path }.into());
        }
        state.newest_ms = state.newest_ms.max(entry_modified_ms(&metadata));
        let entries = fs::read_dir(&current)
            .with_context(|| format!("failed to read scratch directory {}", current.display()))?;
        let mut children = entries
            .map(|entry| {
                entry.map(|entry| entry.path()).with_context(|| {
                    format!(
                        "failed to read scratch directory entry in {}",
                        current.display()
                    )
                })
            })
            .collect::<Result<Vec<_>>>()?;
        children.sort();
        pending.extend(children.into_iter().rev());
    }
    Ok(state)
}

fn scratch_relative_label(namespace_root: &Path, path: &Path) -> String {
    path.strip_prefix(namespace_root)
        .ok()
        .filter(|relative| !relative.as_os_str().is_empty())
        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|| ".".to_owned())
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
        let session_state = walk_namespace(&namespace_root, SCRATCH_WALK_MAX_ENTRIES)?;
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
        let state = walk_namespace(&path, SCRATCH_WALK_MAX_ENTRIES)?;
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

/// Projects a provisioning failure into a bounded, actionable tool error without exposing the
/// host cache path.
pub(crate) fn scratch_provision_error_result(
    call_id: String,
    tool_name: String,
    scratch_label: &str,
    error: anyhow::Error,
) -> ToolResult {
    if let Some(quota_error) = error.downcast_ref::<ScratchQuotaExceededError>() {
        return ToolResult::error(
            call_id,
            tool_name,
            ToolErrorKind::ScratchQuotaExceeded,
            quota_error.to_string(),
        )
        .with_error_details(
            false,
            json!({
                "scope": quota_error.scope.as_str(),
                "usage_bytes": quota_error.usage_bytes,
                "quota_bytes": quota_error.quota_bytes,
                "scratch_label": scratch_label,
                "recovery": {
                    "user_action": "reset_scratch_storage",
                    "automatic": false,
                    "requires_confirmation": true,
                },
            }),
        );
    }
    if let Some(measurement_error) = error.downcast_ref::<ScratchMeasurementError>() {
        let (kind, reason_code, details) = match measurement_error {
            ScratchMeasurementError::EntryLimitExceeded {
                limit,
                observed_entries,
            } => (
                ToolErrorKind::ResourceLimit,
                "scratch_measurement_limit_exceeded",
                json!({
                    "limit_kind": "entries",
                    "limit": limit,
                    "observed_entries": observed_entries,
                }),
            ),
            ScratchMeasurementError::Symlink { relative_path } => (
                ToolErrorKind::Io,
                "scratch_namespace_symlink",
                json!({ "relative_path": relative_path }),
            ),
            ScratchMeasurementError::UnsupportedEntry { relative_path } => (
                ToolErrorKind::Io,
                "scratch_namespace_unsupported_entry",
                json!({ "relative_path": relative_path }),
            ),
        };
        return ToolResult::error(
            call_id,
            tool_name,
            kind,
            format!(
                "failed to provision {scratch_label}: {measurement_error}; ask the user to reset this workspace scratch storage"
            ),
        )
        .with_error_details(
            false,
            json!({
                "reason_code": reason_code,
                "measurement": details,
                "scratch_label": scratch_label,
                "recovery": {
                    "user_action": "reset_scratch_storage",
                    "automatic": false,
                    "requires_confirmation": true,
                },
            }),
        );
    }
    ToolResult::error(
        call_id,
        tool_name,
        ToolErrorKind::Io,
        format!(
            "failed to provision {scratch_label}: scratch storage could not be prepared safely; ask the user to reset this workspace scratch storage"
        ),
    )
    .with_error_details(
        false,
        json!({
            "reason_code": "scratch_provisioning_failed",
            "scratch_label": scratch_label,
            "recovery": {
                "user_action": "reset_scratch_storage",
                "automatic": false,
                "requires_confirmation": true,
            },
        }),
    )
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
        let state = match walk_namespace(&path, SCRATCH_WALK_MAX_ENTRIES) {
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

/// RFC-0071 R71.0 characterization fixtures. They lock the observed causal edges of session
/// 5ff39a6d-5225-4533-8c1f-b64c0c81abb7 without changing production semantics: descendant
/// symlink poisoning, workspace-wide sibling blast radius, GC permanently skipping invalid
/// namespaces, same-session double lease early release, and repeated provisioning failures across
/// distinct tool call ids without a durable active-blocker admission gate. Strict decoders,
/// real directory state and deterministic walks only -- no fabricated numbers.
#[cfg(test)]
#[path = "tests/r71_characterization_tests.rs"]
mod r71_characterization;
