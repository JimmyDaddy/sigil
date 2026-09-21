//! RFC-0071: Resource Authority owner for the persistent SessionScratch namespace.
//!
//! The authority owns namespace allocation, no-follow measurement, soft observations, leases and
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
    #[error("session scratch filesystem operation failed: {0}")]
    Filesystem(String),
    #[error("session scratch lease registry is unavailable")]
    LeaseRegistryUnavailable,
    #[error("session scratch lease lock is unavailable: {0}")]
    LeaseUnavailable(String),
    #[error("invalid session scratch namespace could not be quarantined: {path}: {reason}")]
    QuarantineFailed { path: String, reason: String },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SessionScratchUsageV1 {
    pub session_bytes: u64,
    pub workspace_bytes: u64,
    pub session_entry_count: usize,
    pub workspace_entry_count: usize,
}

/// One bounded sample, never a capacity reservation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionScratchMeasurementV1 {
    pub bytes: u64,
    pub entries: usize,
    pub observed_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionScratchObservationV1 {
    Known(SessionScratchMeasurementV1),
    Unknown {
        reason: String,
        observed_at_ms: u64,
        last_successful_measurement: Option<SessionScratchMeasurementV1>,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionScratchWorkspaceObservationV1 {
    pub observed_at_ms: u64,
    pub known_subtotal_bytes: u64,
    pub known_subtotal_entries: usize,
    pub unknown_owners: Vec<String>,
    pub owners: BTreeMap<String, SessionScratchObservationV1>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionScratchProvisionV1 {
    pub directory: PathBuf,
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
    pub workspace_usage_bytes: Option<u64>,
    pub workspace_known_subtotal_bytes: u64,
    pub unknown_owners: Vec<String>,
    pub observed_at_ms: u64,
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
    observations: Arc<Mutex<BTreeMap<String, SessionScratchObservationV1>>>,
}

impl SessionScratchAuthorityV1 {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            leases: Arc::new(SessionScratchLeaseRegistryV1::default()),
            observations: Arc::new(Mutex::new(BTreeMap::new())),
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
        let key = checked_session_scope_key(session_scope_id)?;
        let namespace_lock = self.lock_namespace(&key)?;
        // Close the prepare/acquire gap against GC and reject a namespace swapped to a symlink.
        ensure_directory(&self.root.join(SESSION_NAMESPACE_DIR))?;
        ensure_directory(&self.session_directory(Some(&key)))?;
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

    /// Prepares only this namespace. Thresholds are retained for observation/configuration;
    /// a directory measurement cannot grant or revoke physical writing capacity.
    pub fn ensure(
        &self,
        session_scope_id: Option<&str>,
        _per_session_bytes: u64,
        _workspace_warning_bytes: u64,
    ) -> Result<SessionScratchProvisionV1, SessionScratchErrorV1> {
        let key = checked_session_scope_key(session_scope_id)?;
        let _lock = self.lock_namespace(&key)?;
        let sessions = self.root.join(SESSION_NAMESPACE_DIR);
        let directory = sessions.join(&key);
        ensure_directory(&self.root)?;
        ensure_directory(&sessions)?;
        ensure_directory(&directory)?;
        Ok(SessionScratchProvisionV1 { directory })
    }

    /// Bounded maintenance observation. Unknown owners retain their last successful sample;
    /// it is never included in the current known subtotal or used as an admission condition.
    pub fn observe(&self, now_ms: u64, max_entries: usize) -> SessionScratchWorkspaceObservationV1 {
        let mut observations = match self.observations.lock() {
            Ok(observations) => observations,
            Err(poisoned) => poisoned.into_inner(),
        };
        let mut samples = BTreeMap::new();
        let mut remaining_entries = max_entries;
        for namespace in [SESSION_NAMESPACE_DIR, QUARANTINE_DIR] {
            let directory = self.root.join(namespace);
            let entries = match fs::read_dir(&directory) {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    samples.insert(namespace.to_owned(), Err(fs_error(error)));
                    continue;
                }
            };
            for (index, entry) in entries.enumerate() {
                if remaining_entries == 0 {
                    samples.insert(
                        format!("{namespace}:unscanned"),
                        Err(SessionScratchErrorV1::EntryLimitExceeded {
                            limit: max_entries,
                            observed: index + 1,
                        }),
                    );
                    break;
                }
                remaining_entries = remaining_entries.saturating_sub(1);
                match entry {
                    Ok(entry) => {
                        let key = format!("{namespace}/{}", entry.file_name().to_string_lossy());
                        let sample = if is_plain_directory(&entry.path()) {
                            walk_with_budget(&entry.path(), &mut remaining_entries, max_entries)
                        } else {
                            Err(SessionScratchErrorV1::NotPlainDirectory {
                                path: entry.path().display().to_string(),
                            })
                        };
                        samples.insert(key, sample);
                    }
                    Err(error) => {
                        samples.insert(format!("{namespace}:entry-{index}"), Err(fs_error(error)));
                    }
                }
            }
        }
        // A missing previously observed owner is not proof of deletion: retain Unknown until an
        // owner/lease-checked cleanup or a later successful sample resolves its state.
        for key in observations.keys().filter(|key| key.contains('/')) {
            samples.entry(key.clone()).or_insert_with(|| {
                Err(SessionScratchErrorV1::Filesystem(
                    "previously observed namespace is missing or was not scanned".to_owned(),
                ))
            });
        }
        observations.retain(|key, _| key.contains('/'));
        let mut result = SessionScratchWorkspaceObservationV1 {
            observed_at_ms: now_ms,
            ..Default::default()
        };
        for (key, sample) in samples {
            let observation = match sample {
                Ok(state) => {
                    result.known_subtotal_bytes =
                        result.known_subtotal_bytes.saturating_add(state.bytes);
                    result.known_subtotal_entries =
                        result.known_subtotal_entries.saturating_add(state.entries);
                    SessionScratchObservationV1::Known(SessionScratchMeasurementV1 {
                        bytes: state.bytes,
                        entries: state.entries,
                        observed_at_ms: now_ms,
                    })
                }
                Err(error) => {
                    let last_successful_measurement =
                        observations.get(&key).and_then(|value| match value {
                            SessionScratchObservationV1::Known(sample) => Some(*sample),
                            SessionScratchObservationV1::Unknown {
                                last_successful_measurement,
                                ..
                            } => *last_successful_measurement,
                        });
                    result.unknown_owners.push(key.clone());
                    SessionScratchObservationV1::Unknown {
                        reason: error.to_string(),
                        observed_at_ms: now_ms,
                        last_successful_measurement,
                    }
                }
            };
            observations.insert(key.clone(), observation.clone());
            result.owners.insert(key, observation);
        }
        result
    }

    /// Returns exact totals only when every bounded observation succeeded.
    pub fn measure(
        &self,
        session_key: &str,
    ) -> Result<SessionScratchUsageV1, SessionScratchErrorV1> {
        let observed = self.observe(observation_time_ms(), DEFAULT_MAX_ENTRIES);
        if !observed.unknown_owners.is_empty() {
            return Err(SessionScratchErrorV1::Filesystem(format!(
                "scratch usage is unknown for {} owner(s)",
                observed.unknown_owners.len()
            )));
        }
        let session = observed
            .owners
            .get(&format!("{SESSION_NAMESPACE_DIR}/{session_key}"));
        let (session_bytes, session_entry_count) = match session {
            Some(SessionScratchObservationV1::Known(sample)) => (sample.bytes, sample.entries),
            None => (0, 0),
            Some(SessionScratchObservationV1::Unknown { .. }) => {
                return Err(SessionScratchErrorV1::Filesystem(
                    "session measurement is unknown".to_owned(),
                ));
            }
        };
        Ok(SessionScratchUsageV1 {
            session_bytes,
            session_entry_count,
            workspace_bytes: observed.known_subtotal_bytes,
            workspace_entry_count: observed.known_subtotal_entries,
        })
    }

    fn forget_deleted_observation(&self, key: &str) {
        if let Ok(mut observations) = self.observations.lock() {
            observations.remove(&format!("{SESSION_NAMESPACE_DIR}/{key}"));
        }
    }

    pub fn gc(
        &self,
        config: SessionScratchGcConfigV1,
        now_ms: u64,
    ) -> Result<SessionScratchGcReportV1, SessionScratchErrorV1> {
        let sessions = self.root.join(SESSION_NAMESPACE_DIR);
        let mut report = SessionScratchGcReportV1::default();
        let entries = match fs::read_dir(&sessions) {
            Ok(entries) => Some(entries),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(fs_error(error)),
        };
        for entry in entries.into_iter().flatten() {
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
            self.forget_deleted_observation(key);
            report.deleted += 1;
            report.deleted_bytes = report.deleted_bytes.saturating_add(state.bytes);
        }
        let observation = self.observe(now_ms, config.max_entries);
        report.workspace_usage_bytes = observation
            .unknown_owners
            .is_empty()
            .then_some(observation.known_subtotal_bytes);
        report.workspace_known_subtotal_bytes = observation.known_subtotal_bytes;
        report.unknown_owners = observation.unknown_owners;
        report.observed_at_ms = now_ms;
        Ok(report)
    }

    pub fn delete(
        &self,
        session_scope_id: Option<&str>,
    ) -> Result<SessionScratchDeleteOutcomeV1, SessionScratchErrorV1> {
        let key = checked_session_scope_key(session_scope_id)?;
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
                self.forget_deleted_observation(&key);
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
        self.forget_deleted_observation(&key);
        Ok(SessionScratchDeleteOutcomeV1::Deleted)
    }

    fn lock_namespace(&self, key: &str) -> Result<File, SessionScratchErrorV1> {
        ensure_directory(&self.root)?;
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
            if !path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(&prefix))
            {
                continue;
            }
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

fn checked_session_scope_key(
    session_scope_id: Option<&str>,
) -> Result<String, SessionScratchErrorV1> {
    let key = session_scope_key(session_scope_id);
    if key == "." || key == ".." || key.contains(['/', '\\']) || key.contains('\0') {
        return Err(SessionScratchErrorV1::Filesystem(
            "invalid session scratch namespace identity".to_owned(),
        ));
    }
    Ok(key)
}

fn observation_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
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
    let mut remaining_entries = max_entries;
    walk_with_budget(root, &mut remaining_entries, max_entries)
}

fn walk_with_budget(
    root: &Path,
    remaining_entries: &mut usize,
    max_entries: usize,
) -> Result<WalkState, SessionScratchErrorV1> {
    let mut state = WalkState::default();
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
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
        for entry in fs::read_dir(&path).map_err(fs_error)? {
            if *remaining_entries == 0 {
                return Err(SessionScratchErrorV1::EntryLimitExceeded {
                    limit: max_entries,
                    observed: max_entries.saturating_add(1),
                });
            }
            *remaining_entries -= 1;
            pending.push(entry.map_err(fs_error)?.path());
        }
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
#[path = "tests/session_scratch_tests.rs"]
mod tests;
