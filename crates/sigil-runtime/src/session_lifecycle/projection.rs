use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    ops::{Deref, DerefMut},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sigil_kernel::{
    JsonlSessionStore, SessionListProjectionEntry, SessionListReadinessSummary,
    SessionListTaskSummary, SessionListUsageSummary, SessionRef, SessionStreamCompatibilityError,
    safe_persistence_text, session_list_projection_from_records,
};
use thiserror::Error as ThisError;

use super::{
    LocalSessionCatalogState, LocalSessionLifecycleLimits, LocalSessionLifecycleService,
    LocalSessionMutationError, SessionCandidate, direct_jsonl_candidates, hash_file_bounded,
    modified_at_unix_ms,
};

mod query;

pub use query::{
    DEFAULT_SESSION_CATALOG_PAGE_SIZE, MAX_SESSION_CATALOG_PAGE_SIZE, SessionCatalogProjectionPage,
    SessionCatalogProjectionQuery,
};

pub const SESSION_CATALOG_SCHEMA_VERSION: u16 = 1;
pub const SESSION_CATALOG_APPLICATION_ID: i32 = 0x5347_494c;
const SESSION_CATALOG_BUSY_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_RECONCILE_RETRIES: usize = 2;
const SESSION_CATALOG_TITLE_MAX_BYTES: usize = 160;
const SESSION_CATALOG_IDENTITY_MAX_BYTES: usize = 128;

const CREATE_SCHEMA_SQL: &str = r#"
CREATE TABLE session_catalog_workspace_v1 (
    workspace_id TEXT PRIMARY KEY NOT NULL,
    generation INTEGER NOT NULL CHECK (generation >= 0),
    reconciled_at_unix_ms INTEGER NOT NULL CHECK (reconciled_at_unix_ms >= 0),
    degraded_source_count INTEGER NOT NULL CHECK (degraded_source_count >= 0),
    identity_conflict_count INTEGER NOT NULL CHECK (identity_conflict_count >= 0),
    truncated_source_count INTEGER NOT NULL CHECK (truncated_source_count >= 0)
) STRICT;

CREATE TABLE session_catalog_entry_v1 (
    workspace_id TEXT NOT NULL,
    session_ref TEXT NOT NULL,
    session_id TEXT,
    source_state TEXT NOT NULL,
    source_bytes INTEGER NOT NULL CHECK (source_bytes >= 0),
    source_modified_at_unix_ms INTEGER NOT NULL CHECK (source_modified_at_unix_ms >= 0),
    source_content_sha256 TEXT,
    first_stream_sequence INTEGER,
    last_stream_sequence INTEGER,
    last_event_id TEXT,
    last_record_checksum TEXT,
    provider_name TEXT,
    model_name TEXT,
    title TEXT,
    title_search TEXT,
    user_message_count INTEGER NOT NULL CHECK (user_message_count >= 0),
    assistant_message_count INTEGER NOT NULL CHECK (assistant_message_count >= 0),
    tool_result_count INTEGER NOT NULL CHECK (tool_result_count >= 0),
    control_entry_count INTEGER NOT NULL CHECK (control_entry_count >= 0),
    latest_usage_json TEXT,
    latest_task_json TEXT,
    latest_readiness_json TEXT,
    pinned INTEGER NOT NULL CHECK (pinned IN (0, 1)),
    indexed_at_unix_ms INTEGER NOT NULL CHECK (indexed_at_unix_ms >= 0),
    PRIMARY KEY (workspace_id, session_ref),
    FOREIGN KEY (workspace_id) REFERENCES session_catalog_workspace_v1(workspace_id)
        ON DELETE CASCADE
) STRICT;

CREATE INDEX session_catalog_entry_workspace_sort_v1
    ON session_catalog_entry_v1(
        workspace_id,
        source_modified_at_unix_ms DESC,
        session_id DESC,
        session_ref DESC
    );
CREATE INDEX session_catalog_entry_workspace_provider_v1
    ON session_catalog_entry_v1(workspace_id, provider_name);
CREATE INDEX session_catalog_entry_workspace_state_v1
    ON session_catalog_entry_v1(workspace_id, source_state);
CREATE INDEX session_catalog_entry_workspace_pinned_v1
    ON session_catalog_entry_v1(workspace_id, pinned);
"#;

/// Stable failures returned by the rebuildable SQLite session catalog.
#[derive(Debug, ThisError)]
pub enum SessionCatalogProjectionError {
    #[error("session catalog path is unsafe: {message}")]
    UnsafePath { message: String },
    #[error(
        "session catalog schema is incompatible: application_id={application_id}, user_version={user_version}"
    )]
    IncompatibleSchema {
        application_id: i32,
        user_version: i32,
    },
    #[error("session catalog SQLite operation failed: {source}")]
    Sqlite {
        #[from]
        source: rusqlite::Error,
    },
    #[error("session catalog source reconciliation failed: {message}")]
    Source { message: String },
    #[error("session catalog value exceeds SQLite integer range: {field}")]
    IntegerRange { field: &'static str },
    #[error("session catalog projection encoding failed: {message}")]
    Encoding { message: String },
    #[error("session catalog query is invalid: {message}")]
    InvalidQuery { message: String },
    #[error("session catalog cursor is invalid: {message}")]
    InvalidCursor { message: String },
    #[error(
        "session catalog cursor is stale: cursor generation {cursor_generation}, current generation {current_generation}"
    )]
    StaleCursor {
        cursor_generation: u64,
        current_generation: u64,
    },
    #[error("session catalog reconcile conflicted with another writer after bounded retries")]
    ReconcileConflict,
    #[error("session catalog recovery lease is busy")]
    RecoveryBusy,
    #[error("session catalog recovery failed: {message}")]
    Recovery { message: String },
}

/// Safe, compact historical session row. Message and tool bodies are never materialized here.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct SessionCatalogProjectionEntry {
    pub workspace_id: String,
    pub session_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub source_state: LocalSessionCatalogState,
    pub source_bytes: u64,
    pub source_modified_at_unix_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_content_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_stream_sequence: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_stream_sequence: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_record_checksum: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub user_message_count: u64,
    pub assistant_message_count: u64,
    pub tool_result_count: u64,
    pub control_entry_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_usage: Option<SessionListUsageSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_task: Option<SessionListTaskSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_readiness: Option<SessionListReadinessSummary>,
    #[serde(default)]
    pub pinned: bool,
    pub indexed_at_unix_ms: u64,
}

/// Diagnostics for one explicit full rebuild of a workspace catalog projection.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct SessionCatalogProjectionRebuildReport {
    pub workspace_id: String,
    pub generation: u64,
    pub scanned_source_count: usize,
    pub indexed_source_count: usize,
    pub degraded_source_count: usize,
    pub identity_conflict_count: usize,
    pub truncated_source_count: usize,
    pub reconciled_at_unix_ms: u64,
}

/// Diagnostics for one metadata-aware incremental workspace reconciliation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct SessionCatalogProjectionReconcileReport {
    pub workspace_id: String,
    pub generation: u64,
    pub generation_changed: bool,
    pub scanned_source_count: usize,
    pub reused_source_count: usize,
    pub updated_source_count: usize,
    pub removed_source_count: usize,
    pub degraded_source_count: usize,
    pub identity_conflict_count: usize,
    pub truncated_source_count: usize,
    pub retry_count: usize,
    pub reconciled_at_unix_ms: u64,
}

/// Result of explicitly quarantining a projection and rebuilding it from durable sources.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct SessionCatalogProjectionRecoveryReport {
    pub workspace_id: String,
    pub generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invalidated_workspace_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quarantine_directory_name: Option<String>,
    pub quarantined_file_count: usize,
    pub rebuilt_source_count: usize,
    pub degraded_source_count: usize,
    pub identity_conflict_count: usize,
    pub truncated_source_count: usize,
    pub recovered_at_unix_ms: u64,
}

/// Durable mutation receipt. Projection refresh is best-effort because the lifecycle decision is
/// already committed when this value is returned.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct SessionCatalogMutationReceipt {
    pub session_ref: String,
    pub session_id: String,
    pub operation_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection_generation: Option<u64>,
}

/// Receipt for moving one exact invalid source out of the active catalog scan.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct SessionCatalogQuarantineReceipt {
    pub session_ref: String,
    pub operation_id: String,
    pub quarantine_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection_generation: Option<u64>,
}

/// Receipt for permanently removing one exact invalid source from the active catalog.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct SessionCatalogInvalidSourceDeleteReceipt {
    pub session_ref: String,
    pub operation_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection_generation: Option<u64>,
}

/// Workspace-bound owner of the rebuildable global SQLite session catalog.
#[derive(Debug, Clone)]
pub struct SessionCatalogProjectionService {
    lifecycle: LocalSessionLifecycleService,
    database_path: PathBuf,
}

struct SessionCatalogConnection {
    connection: Connection,
    _usage_lease: Option<File>,
}

impl Deref for SessionCatalogConnection {
    type Target = Connection;

    fn deref(&self) -> &Self::Target {
        &self.connection
    }
}

impl DerefMut for SessionCatalogConnection {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.connection
    }
}

struct SessionCatalogRecoveryLease {
    _file: File,
}

impl SessionCatalogProjectionService {
    #[must_use]
    pub fn new(lifecycle: LocalSessionLifecycleService, database_path: impl Into<PathBuf>) -> Self {
        Self {
            lifecycle,
            database_path: database_path.into(),
        }
    }

    #[must_use]
    pub fn database_path(&self) -> &Path {
        &self.database_path
    }

    /// Replaces this workspace's materialized rows from bounded durable JSONL and lifecycle input.
    ///
    /// Other workspace rows in the same global database are not modified.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the database path/schema is unsafe, a source drifts while being
    /// read, durable input cannot be validated, or the SQLite transaction cannot commit.
    pub fn rebuild(
        &self,
    ) -> Result<SessionCatalogProjectionRebuildReport, SessionCatalogProjectionError> {
        let report = self.reconcile_internal(true)?;
        Ok(SessionCatalogProjectionRebuildReport {
            workspace_id: report.workspace_id,
            generation: report.generation,
            scanned_source_count: report.scanned_source_count,
            indexed_source_count: report
                .scanned_source_count
                .saturating_sub(report.degraded_source_count),
            degraded_source_count: report.degraded_source_count,
            identity_conflict_count: report.identity_conflict_count,
            truncated_source_count: report.truncated_source_count,
            reconciled_at_unix_ms: report.reconciled_at_unix_ms,
        })
    }

    /// Reconciles changed direct session sources while reusing metadata-stable rows.
    ///
    /// A generation compare-and-swap prevents an older concurrent scan from replacing a newer
    /// commit. The bounded retry count prevents writer contention from becoming an unbounded wait.
    ///
    /// # Errors
    ///
    /// Returns a typed error for unsafe inputs, source drift, SQLite failures, or repeated
    /// generation conflicts.
    pub fn reconcile(
        &self,
    ) -> Result<SessionCatalogProjectionReconcileReport, SessionCatalogProjectionError> {
        self.reconcile_internal(false)
    }

    /// Reads this workspace's current rows in deterministic catalog order.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the database cannot be opened or a stored row is invalid.
    pub fn list_workspace_entries(
        &self,
    ) -> Result<Vec<SessionCatalogProjectionEntry>, SessionCatalogProjectionError> {
        let connection = self.open_connection()?;
        list_workspace_entries(&connection, &self.lifecycle.workspace_id)
    }

    /// Persists a display-name override for one exact catalog identity and refreshes SQLite.
    ///
    /// # Errors
    ///
    /// Returns a stable mutation error when the reference/name is invalid or durable identity has
    /// changed. A projection refresh failure does not erase or misreport the committed rename.
    pub fn rename_session(
        &self,
        session_ref: &str,
        session_id: &str,
        display_name: &str,
    ) -> Result<SessionCatalogMutationReceipt, LocalSessionMutationError> {
        let session_ref = exact_session_ref(session_ref)?;
        exact_session_id(session_id)?;
        let record = self.lifecycle.rename_session(
            &session_ref,
            session_id,
            display_name,
            current_unix_ms(),
        )?;
        let projection_generation = self.reconcile().ok().map(|report| report.generation);
        Ok(SessionCatalogMutationReceipt {
            session_ref: session_ref.as_path().to_string_lossy().into_owned(),
            session_id: session_id.to_owned(),
            operation_id: record.operation_id,
            projection_generation,
        })
    }

    /// Applies an audited delete for one exact, unpinned catalog identity and refreshes SQLite.
    ///
    /// # Errors
    ///
    /// Returns a stable mutation error when the source is missing, changed, pinned, or otherwise
    /// unavailable for deletion.
    pub fn delete_session(
        &self,
        session_ref: &str,
        session_id: &str,
    ) -> Result<SessionCatalogMutationReceipt, LocalSessionMutationError> {
        let session_ref = exact_session_ref(session_ref)?;
        exact_session_id(session_id)?;
        let output =
            self.lifecycle
                .delete_session(&session_ref, session_id, &[], current_unix_ms())?;
        let projection_generation = self.reconcile().ok().map(|report| report.generation);
        Ok(SessionCatalogMutationReceipt {
            session_ref: session_ref.as_path().to_string_lossy().into_owned(),
            session_id: session_id.to_owned(),
            operation_id: output.operation_id,
            projection_generation,
        })
    }

    /// Moves one exact invalid source into the local quarantine directory and refreshes SQLite.
    ///
    /// The source metadata is revalidated under the session-maintenance lease before the atomic
    /// rename. Ready, oversized, scan-limited, and legacy sources are never accepted by this
    /// recovery operation.
    ///
    /// # Errors
    ///
    /// Returns a stable mutation error when the reference is unsafe, the source is no longer
    /// invalid, its metadata changed, or the quarantine directory cannot be used safely.
    pub fn quarantine_invalid_source(
        &self,
        session_ref: &str,
        expected_source_bytes: u64,
        expected_modified_at_unix_ms: u64,
    ) -> Result<SessionCatalogQuarantineReceipt, LocalSessionMutationError> {
        let session_ref = exact_session_ref(session_ref)?;
        let session_ref_text = session_ref.as_path().to_string_lossy().into_owned();
        let entry = self
            .list_workspace_entries()
            .map_err(|source| LocalSessionMutationError::Unavailable {
                source: anyhow::Error::new(source),
            })?
            .into_iter()
            .find(|entry| entry.session_ref == session_ref_text)
            .ok_or(LocalSessionMutationError::NotFound)?;
        if entry.source_state != LocalSessionCatalogState::Invalid || entry.session_id.is_some() {
            return Err(LocalSessionMutationError::NotReady);
        }
        if entry.source_bytes != expected_source_bytes
            || entry.source_modified_at_unix_ms != expected_modified_at_unix_ms
        {
            return Err(LocalSessionMutationError::IdentityChanged);
        }

        let _lease = self
            .lifecycle
            .acquire_maintenance_lease()
            .map_err(|source| LocalSessionMutationError::Unavailable { source })?;
        let session_dir = canonical_real_directory(&self.lifecycle.session_dir)?;
        let source_path = session_ref.resolve(&session_dir);
        let metadata = fs::symlink_metadata(&source_path).map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => LocalSessionMutationError::NotFound,
            _ => LocalSessionMutationError::Unavailable {
                source: anyhow::Error::new(error),
            },
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(LocalSessionMutationError::NotReady);
        }
        if metadata.len() != expected_source_bytes
            || modified_at_unix_ms(&metadata) != expected_modified_at_unix_ms
        {
            return Err(LocalSessionMutationError::IdentityChanged);
        }

        let quarantine_dir = session_dir.join(".quarantine");
        ensure_real_quarantine_directory(&quarantine_dir)?;
        let operation_id = format!("session-quarantine:{}", uuid::Uuid::new_v4());
        let quarantine_name = format!(
            "{}--{}",
            operation_id.trim_start_matches("session-quarantine:"),
            session_ref_text
        );
        fs::rename(&source_path, quarantine_dir.join(&quarantine_name)).map_err(|source| {
            LocalSessionMutationError::Unavailable {
                source: anyhow::Error::new(source),
            }
        })?;
        let projection_generation = self.reconcile().ok().map(|report| report.generation);
        Ok(SessionCatalogQuarantineReceipt {
            session_ref: session_ref_text,
            operation_id,
            quarantine_name,
            projection_generation,
        })
    }

    /// Permanently removes one exact invalid source and refreshes SQLite.
    ///
    /// This operation is intentionally separate from audited durable-session deletion: malformed
    /// input has no trustworthy session identity to bind to the lifecycle journal. The direct-child
    /// reference, catalog state, byte length, and modified timestamp are all revalidated under the
    /// session-maintenance lease before the regular file is removed.
    ///
    /// # Errors
    ///
    /// Returns a stable mutation error when the reference is unsafe, the selected catalog row is
    /// no longer invalid, its source fingerprint changed, or the file cannot be removed safely.
    pub fn delete_invalid_source(
        &self,
        session_ref: &str,
        expected_source_bytes: u64,
        expected_modified_at_unix_ms: u64,
    ) -> Result<SessionCatalogInvalidSourceDeleteReceipt, LocalSessionMutationError> {
        let session_ref = exact_session_ref(session_ref)?;
        let session_ref_text = session_ref.as_path().to_string_lossy().into_owned();
        let entry = self
            .list_workspace_entries()
            .map_err(|source| LocalSessionMutationError::Unavailable {
                source: anyhow::Error::new(source),
            })?
            .into_iter()
            .find(|entry| entry.session_ref == session_ref_text)
            .ok_or(LocalSessionMutationError::NotFound)?;
        if entry.source_state != LocalSessionCatalogState::Invalid || entry.session_id.is_some() {
            return Err(LocalSessionMutationError::NotReady);
        }
        if entry.source_bytes != expected_source_bytes
            || entry.source_modified_at_unix_ms != expected_modified_at_unix_ms
        {
            return Err(LocalSessionMutationError::IdentityChanged);
        }

        let _lease = self
            .lifecycle
            .acquire_maintenance_lease()
            .map_err(|source| LocalSessionMutationError::Unavailable { source })?;
        let session_dir = canonical_real_directory(&self.lifecycle.session_dir)?;
        let source_path = session_ref.resolve(&session_dir);
        let metadata = fs::symlink_metadata(&source_path).map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => LocalSessionMutationError::NotFound,
            _ => LocalSessionMutationError::Unavailable {
                source: anyhow::Error::new(error),
            },
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(LocalSessionMutationError::NotReady);
        }
        if metadata.len() != expected_source_bytes
            || modified_at_unix_ms(&metadata) != expected_modified_at_unix_ms
        {
            return Err(LocalSessionMutationError::IdentityChanged);
        }

        fs::remove_file(&source_path).map_err(|source| LocalSessionMutationError::Unavailable {
            source: anyhow::Error::new(source),
        })?;
        let projection_generation = self.reconcile().ok().map(|report| report.generation);
        Ok(SessionCatalogInvalidSourceDeleteReceipt {
            session_ref: session_ref_text,
            operation_id: format!("invalid-source-delete:{}", uuid::Uuid::new_v4()),
            projection_generation,
        })
    }

    /// Quarantines the global SQLite files under an exclusive process lease and rebuilds the
    /// current workspace from durable JSONL/lifecycle sources before releasing that lease.
    ///
    /// This is an explicit owner recovery action. Automatic queries never move or delete a
    /// projection. Because the database is global, this invalidates cached rows for every
    /// workspace; their JSONL truth remains intact and each workspace is restored on its next
    /// reconcile. The report includes the previous workspace count when it could be read safely.
    /// The quarantine directory remains beside the database for inspection.
    ///
    /// # Errors
    ///
    /// Returns [`SessionCatalogProjectionError::RecoveryBusy`] while another Sigil process holds
    /// a catalog connection, or a typed filesystem/rebuild error without deleting the durable
    /// session sources.
    pub fn quarantine_global_catalog_and_rebuild_workspace(
        &self,
    ) -> Result<SessionCatalogProjectionRecoveryReport, SessionCatalogProjectionError> {
        prepare_database_parent(&self.database_path)?;
        let recovery_lease = self.acquire_exclusive_recovery_lease()?;
        let invalidated_workspace_count = self.workspace_count_under_recovery();
        let quarantine = quarantine_sqlite_files(&self.database_path)?;
        let rebuilt = self.reconcile_internal_with_recovery(true, Some(&recovery_lease));
        match rebuilt {
            Ok(rebuilt) => Ok(SessionCatalogProjectionRecoveryReport {
                workspace_id: rebuilt.workspace_id,
                generation: rebuilt.generation,
                invalidated_workspace_count,
                quarantine_directory_name: quarantine
                    .as_ref()
                    .and_then(|quarantine| quarantine.path.file_name())
                    .map(|name| name.to_string_lossy().into_owned()),
                quarantined_file_count: quarantine
                    .as_ref()
                    .map_or(0, |quarantine| quarantine.file_count),
                rebuilt_source_count: rebuilt.scanned_source_count,
                degraded_source_count: rebuilt.degraded_source_count,
                identity_conflict_count: rebuilt.identity_conflict_count,
                truncated_source_count: rebuilt.truncated_source_count,
                recovered_at_unix_ms: rebuilt.reconciled_at_unix_ms,
            }),
            Err(error) => {
                quarantine_failed_rebuild(&self.database_path, quarantine.as_ref());
                Err(error)
            }
        }
    }

    fn open_connection(&self) -> Result<SessionCatalogConnection, SessionCatalogProjectionError> {
        prepare_database_parent(&self.database_path)?;
        let usage_lease = self.acquire_shared_usage_lease()?;
        let connection = self.open_database_connection()?;
        Ok(SessionCatalogConnection {
            connection,
            _usage_lease: Some(usage_lease),
        })
    }

    fn open_connection_under_recovery(
        &self,
        _recovery_lease: &SessionCatalogRecoveryLease,
    ) -> Result<SessionCatalogConnection, SessionCatalogProjectionError> {
        let connection = self.open_database_connection()?;
        Ok(SessionCatalogConnection {
            connection,
            _usage_lease: None,
        })
    }

    fn open_database_connection(&self) -> Result<Connection, SessionCatalogProjectionError> {
        ensure_secure_database_file(&self.database_path)?;
        for sidecar in [
            sqlite_sidecar_path(&self.database_path, "-wal"),
            sqlite_sidecar_path(&self.database_path, "-shm"),
        ] {
            validate_optional_regular_projection_file(&sidecar, "SQLite sidecar")?;
        }
        let canonical_database_path = fs::canonicalize(&self.database_path).map_err(|error| {
            SessionCatalogProjectionError::UnsafePath {
                message: format!("failed to resolve database path: {error}"),
            }
        })?;
        let mut connection = Connection::open_with_flags(
            &canonical_database_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        connection.busy_timeout(SESSION_CATALOG_BUSY_TIMEOUT)?;
        connection.execute_batch("PRAGMA trusted_schema = OFF; PRAGMA foreign_keys = ON;")?;
        initialize_or_validate_schema(&mut connection, &self.database_path)?;
        connection.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = FULL;")?;
        tighten_catalog_permissions(&self.database_path)?;
        Ok(connection)
    }

    fn workspace_count_under_recovery(&self) -> Option<usize> {
        if !matches!(
            validate_optional_regular_projection_file(&self.database_path, "database"),
            Ok(true)
        ) {
            return Some(0);
        }
        let connection = self.open_database_connection().ok()?;
        let count = connection
            .query_row(
                "SELECT COUNT(*) FROM session_catalog_workspace_v1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .ok()?;
        usize::try_from(count).ok()
    }

    fn acquire_shared_usage_lease(&self) -> Result<File, SessionCatalogProjectionError> {
        let lease = open_recovery_lease_file(&self.database_path)?;
        lease.try_lock_shared().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => SessionCatalogProjectionError::RecoveryBusy,
            std::fs::TryLockError::Error(error) => SessionCatalogProjectionError::Recovery {
                message: format!("failed to acquire shared usage lease: {error}"),
            },
        })?;
        Ok(lease)
    }

    fn acquire_exclusive_recovery_lease(
        &self,
    ) -> Result<SessionCatalogRecoveryLease, SessionCatalogProjectionError> {
        let lease = open_recovery_lease_file(&self.database_path)?;
        lease.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => SessionCatalogProjectionError::RecoveryBusy,
            std::fs::TryLockError::Error(error) => SessionCatalogProjectionError::Recovery {
                message: format!("failed to acquire exclusive recovery lease: {error}"),
            },
        })?;
        Ok(SessionCatalogRecoveryLease { _file: lease })
    }

    fn reconcile_internal(
        &self,
        force_rebuild: bool,
    ) -> Result<SessionCatalogProjectionReconcileReport, SessionCatalogProjectionError> {
        self.reconcile_internal_with_recovery(force_rebuild, None)
    }

    fn reconcile_internal_with_recovery(
        &self,
        force_rebuild: bool,
        recovery_lease: Option<&SessionCatalogRecoveryLease>,
    ) -> Result<SessionCatalogProjectionReconcileReport, SessionCatalogProjectionError> {
        for retry_count in 0..=MAX_RECONCILE_RETRIES {
            let mut connection = match recovery_lease {
                Some(recovery_lease) => self.open_connection_under_recovery(recovery_lease)?,
                None => self.open_connection()?,
            };
            let base_metadata = workspace_metadata(&connection, &self.lifecycle.workspace_id)?;
            let base_generation = base_metadata
                .as_ref()
                .map_or(0, |metadata| metadata.generation);
            let existing = list_workspace_entries(&connection, &self.lifecycle.workspace_id)?
                .into_iter()
                .map(|entry| (entry.session_ref.clone(), entry))
                .collect::<BTreeMap<_, _>>();
            let reconciled_at_unix_ms = current_unix_ms();
            let scan = self.scan_entries(&existing, force_rebuild, reconciled_at_unix_ms)?;
            let degraded_source_count = scan
                .entries
                .iter()
                .filter(|entry| entry.source_state != LocalSessionCatalogState::Ready)
                .count();
            let identity_conflict_count = identity_conflict_count(&scan.entries);
            let next_by_ref = scan
                .entries
                .iter()
                .map(|entry| (entry.session_ref.clone(), entry))
                .collect::<BTreeMap<_, _>>();
            let updated_entries = next_by_ref
                .iter()
                .filter(|(session_ref, entry)| existing.get(*session_ref) != Some(*entry))
                .map(|(_, entry)| *entry)
                .collect::<Vec<_>>();
            let removed_session_refs = existing
                .keys()
                .filter(|session_ref| !next_by_ref.contains_key(*session_ref))
                .map(String::as_str)
                .collect::<Vec<_>>();
            let updated_source_count = updated_entries.len();
            let removed_source_count = removed_session_refs.len();
            let metadata_changed = base_metadata.as_ref().is_none_or(|metadata| {
                metadata.degraded_source_count != degraded_source_count
                    || metadata.identity_conflict_count != identity_conflict_count
                    || metadata.truncated_source_count != scan.truncated_source_count
            });
            let generation_changed = force_rebuild
                || updated_source_count > 0
                || removed_source_count > 0
                || metadata_changed;
            let generation = if generation_changed {
                base_generation.checked_add(1).ok_or(
                    SessionCatalogProjectionError::IntegerRange {
                        field: "generation",
                    },
                )?
            } else {
                base_generation
            };
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            if !workspace_generation_matches(
                &transaction,
                &self.lifecycle.workspace_id,
                base_metadata.as_ref().map(|metadata| metadata.generation),
            )? {
                transaction.rollback()?;
                continue;
            }
            if generation_changed {
                apply_workspace_entry_changes(
                    &transaction,
                    &self.lifecycle.workspace_id,
                    generation,
                    reconciled_at_unix_ms,
                    degraded_source_count,
                    identity_conflict_count,
                    scan.truncated_source_count,
                    &updated_entries,
                    &removed_session_refs,
                )?;
            } else {
                upsert_workspace_metadata(
                    &transaction,
                    &self.lifecycle.workspace_id,
                    generation,
                    reconciled_at_unix_ms,
                    degraded_source_count,
                    identity_conflict_count,
                    scan.truncated_source_count,
                )?;
            }
            transaction.commit()?;
            return Ok(SessionCatalogProjectionReconcileReport {
                workspace_id: self.lifecycle.workspace_id.clone(),
                generation,
                generation_changed,
                scanned_source_count: scan.scanned_source_count,
                reused_source_count: scan.reused_source_count,
                updated_source_count,
                removed_source_count,
                degraded_source_count,
                identity_conflict_count,
                truncated_source_count: scan.truncated_source_count,
                retry_count,
                reconciled_at_unix_ms,
            });
        }
        Err(SessionCatalogProjectionError::ReconcileConflict)
    }

    fn scan_entries(
        &self,
        existing: &BTreeMap<String, SessionCatalogProjectionEntry>,
        force_rebuild: bool,
        indexed_at_unix_ms: u64,
    ) -> Result<SessionCatalogScan, SessionCatalogProjectionError> {
        let metadata = match fs::symlink_metadata(&self.lifecycle.session_dir) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(SessionCatalogScan::default());
            }
            Err(error) => return Err(source_error(error)),
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(SessionCatalogProjectionError::Source {
                message: "configured session directory must be a real directory".to_owned(),
            });
        }
        let session_dir = fs::canonicalize(&self.lifecycle.session_dir).map_err(source_error)?;
        let mut candidates = direct_jsonl_candidates(&session_dir).map_err(source_error)?;
        if let Ok(journal_path) = fs::canonicalize(&self.lifecycle.lifecycle_journal_path) {
            candidates.retain(|candidate| candidate.path != journal_path);
        }
        candidates.sort_by(|left, right| {
            right
                .modified_at_unix_ms
                .cmp(&left.modified_at_unix_ms)
                .then_with(|| left.path.cmp(&right.path))
        });
        let truncated_source_count = candidates
            .len()
            .saturating_sub(self.lifecycle.limits.max_catalog_entries);
        candidates.truncate(self.lifecycle.limits.max_catalog_entries);
        let pins = self
            .lifecycle
            .session_pin_projection()
            .map_err(source_error)?;
        let display_names = self
            .lifecycle
            .session_display_name_projection()
            .map_err(source_error)?;
        let mut validated_bytes = 0_u64;
        let mut entries = Vec::with_capacity(candidates.len());
        let mut reused_source_count = 0;
        for candidate in candidates {
            let state = candidate_state(&candidate, &self.lifecycle.limits, &mut validated_bytes);
            let session_ref = candidate
                .session_ref
                .as_path()
                .to_string_lossy()
                .into_owned();
            if !force_rebuild
                && let Some(previous) = existing.get(&session_ref)
                && source_fingerprint_matches(previous, &candidate, state)
            {
                let mut reused = previous.clone();
                reused.pinned = reused.session_id.as_deref().is_some_and(|session_id| {
                    pins.get(&candidate.session_ref)
                        .is_some_and(|(pinned_session_id, pinned)| {
                            pinned_session_id == session_id && *pinned
                        })
                });
                if let Some(session_id) = reused.session_id.as_deref()
                    && let Some((named_session_id, display_name)) =
                        display_names.get(&candidate.session_ref)
                    && named_session_id == session_id
                {
                    reused.title = Some(display_name.clone());
                }
                entries.push(reused);
                reused_source_count += 1;
                continue;
            }
            entries.push(self.project_candidate(
                candidate,
                state,
                &pins,
                &display_names,
                indexed_at_unix_ms,
            )?);
        }
        let scanned_source_count = entries.len();
        if truncated_source_count > 0 && !force_rebuild {
            let scanned_refs = entries
                .iter()
                .map(|entry| entry.session_ref.clone())
                .collect::<BTreeSet<_>>();
            entries.extend(
                existing
                    .values()
                    .filter(|entry| !scanned_refs.contains(&entry.session_ref))
                    .cloned(),
            );
        }
        Ok(SessionCatalogScan {
            entries,
            scanned_source_count,
            reused_source_count,
            truncated_source_count,
        })
    }

    fn project_candidate(
        &self,
        candidate: SessionCandidate,
        initial_state: LocalSessionCatalogState,
        pins: &BTreeMap<sigil_kernel::SessionRef, (String, bool)>,
        display_names: &BTreeMap<sigil_kernel::SessionRef, (String, String)>,
        indexed_at_unix_ms: u64,
    ) -> Result<SessionCatalogProjectionEntry, SessionCatalogProjectionError> {
        let session_ref = candidate
            .session_ref
            .as_path()
            .to_string_lossy()
            .into_owned();
        let mut entry = empty_projection_entry(
            &self.lifecycle.workspace_id,
            session_ref,
            initial_state,
            &candidate,
            indexed_at_unix_ms,
        );
        if initial_state != LocalSessionCatalogState::Ready {
            return Ok(entry);
        }
        let records = match JsonlSessionStore::read_event_records(&candidate.path) {
            Ok(records) => records,
            Err(error) => {
                entry.source_state = if error
                    .downcast_ref::<SessionStreamCompatibilityError>()
                    .is_some()
                {
                    LocalSessionCatalogState::UnsupportedLegacy
                } else {
                    LocalSessionCatalogState::Invalid
                };
                return Ok(entry);
            }
        };
        ensure_source_stable(&candidate)?;
        let snapshot = match session_list_projection_from_records(&records) {
            Ok(snapshot) if snapshot.sessions.len() == 1 => snapshot,
            Ok(_) | Err(_) => {
                entry.source_state = LocalSessionCatalogState::Invalid;
                return Ok(entry);
            }
        };
        let projection = snapshot.sessions.into_iter().next().ok_or_else(|| {
            SessionCatalogProjectionError::Source {
                message: "ready session projection was unexpectedly empty".to_owned(),
            }
        })?;
        entry.source_content_sha256 = Some(
            hash_file_bounded(&candidate.path, self.lifecycle.limits.max_stream_bytes)
                .map_err(source_error)?,
        );
        ensure_source_stable(&candidate)?;
        entry.pinned = pins
            .get(&candidate.session_ref)
            .is_some_and(|(session_id, pinned)| session_id == &projection.session_id && *pinned);
        apply_session_projection(&mut entry, projection, records.last());
        if let Some(session_id) = entry.session_id.as_deref()
            && let Some((named_session_id, display_name)) =
                display_names.get(&candidate.session_ref)
            && named_session_id == session_id
        {
            entry.title = Some(display_name.clone());
        }
        Ok(entry)
    }
}

fn exact_session_ref(value: &str) -> Result<SessionRef, LocalSessionMutationError> {
    if value.is_empty()
        || value.trim() != value
        || value.len() > SESSION_CATALOG_IDENTITY_MAX_BYTES
        || value.contains(['/', '\\'])
    {
        return Err(LocalSessionMutationError::InvalidRequest);
    }
    let session_ref =
        SessionRef::new_relative(value).map_err(|_| LocalSessionMutationError::InvalidRequest)?;
    if session_ref
        .as_path()
        .extension()
        .and_then(|extension| extension.to_str())
        != Some("jsonl")
    {
        return Err(LocalSessionMutationError::InvalidRequest);
    }
    Ok(session_ref)
}

fn exact_session_id(value: &str) -> Result<(), LocalSessionMutationError> {
    if value.is_empty()
        || value.trim() != value
        || value.len() > 512
        || safe_persistence_text(value) != value
    {
        return Err(LocalSessionMutationError::InvalidRequest);
    }
    Ok(())
}

fn canonical_real_directory(path: &Path) -> Result<PathBuf, LocalSessionMutationError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|source| LocalSessionMutationError::Unavailable {
            source: anyhow::Error::new(source),
        })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(LocalSessionMutationError::NotReady);
    }
    fs::canonicalize(path).map_err(|source| LocalSessionMutationError::Unavailable {
        source: anyhow::Error::new(source),
    })
}

fn ensure_real_quarantine_directory(path: &Path) -> Result<(), LocalSessionMutationError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(LocalSessionMutationError::NotReady);
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path).map_err(|source| LocalSessionMutationError::Unavailable {
                source: anyhow::Error::new(source),
            })?;
        }
        Err(source) => {
            return Err(LocalSessionMutationError::Unavailable {
                source: anyhow::Error::new(source),
            });
        }
    }
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|source| {
        LocalSessionMutationError::Unavailable {
            source: anyhow::Error::new(source),
        }
    })?;
    Ok(())
}

#[derive(Debug, Default)]
struct SessionCatalogScan {
    entries: Vec<SessionCatalogProjectionEntry>,
    scanned_source_count: usize,
    reused_source_count: usize,
    truncated_source_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionCatalogWorkspaceMetadata {
    generation: u64,
    reconciled_at_unix_ms: u64,
    degraded_source_count: usize,
    identity_conflict_count: usize,
    truncated_source_count: usize,
}

fn source_fingerprint_matches(
    previous: &SessionCatalogProjectionEntry,
    candidate: &SessionCandidate,
    state: LocalSessionCatalogState,
) -> bool {
    (previous.source_state == state
        || (state == LocalSessionCatalogState::Ready
            && matches!(
                previous.source_state,
                LocalSessionCatalogState::Invalid | LocalSessionCatalogState::UnsupportedLegacy
            )))
        && previous.source_bytes == candidate.bytes
        && previous.source_modified_at_unix_ms == candidate.modified_at_unix_ms
}

fn list_workspace_entries(
    connection: &Connection,
    workspace_id: &str,
) -> Result<Vec<SessionCatalogProjectionEntry>, SessionCatalogProjectionError> {
    let mut statement = connection.prepare(
        "SELECT workspace_id, session_ref, session_id, source_state, source_bytes, \
         source_modified_at_unix_ms, source_content_sha256, first_stream_sequence, \
         last_stream_sequence, last_event_id, last_record_checksum, provider_name, model_name, \
         title, user_message_count, assistant_message_count, tool_result_count, \
         control_entry_count, latest_usage_json, latest_task_json, latest_readiness_json, \
         pinned, indexed_at_unix_ms \
         FROM session_catalog_entry_v1 WHERE workspace_id = ?1 \
         ORDER BY source_modified_at_unix_ms DESC, COALESCE(session_id, '') DESC, \
         session_ref DESC",
    )?;
    let rows = statement.query_map(params![workspace_id], decode_entry_row)?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(SessionCatalogProjectionError::from)
}

fn workspace_metadata(
    connection: &Connection,
    workspace_id: &str,
) -> Result<Option<SessionCatalogWorkspaceMetadata>, SessionCatalogProjectionError> {
    connection
        .query_row(
            "SELECT generation, reconciled_at_unix_ms, degraded_source_count, \
             identity_conflict_count, truncated_source_count \
             FROM session_catalog_workspace_v1 WHERE workspace_id = ?1",
            params![workspace_id],
            |row| {
                Ok(SessionCatalogWorkspaceMetadata {
                    generation: decode_u64(row.get(0)?, 0)?,
                    reconciled_at_unix_ms: decode_u64(row.get(1)?, 1)?,
                    degraded_source_count: decode_usize(row.get(2)?, 2)?,
                    identity_conflict_count: decode_usize(row.get(3)?, 3)?,
                    truncated_source_count: decode_usize(row.get(4)?, 4)?,
                })
            },
        )
        .optional()
        .map_err(SessionCatalogProjectionError::from)
}

#[derive(Debug)]
struct SessionCatalogQuarantine {
    path: PathBuf,
    file_count: usize,
}

fn open_recovery_lease_file(database_path: &Path) -> Result<File, SessionCatalogProjectionError> {
    let lease_path = sqlite_sidecar_path(database_path, ".recovery-lock");
    validate_optional_regular_projection_file(&lease_path, "recovery lease")?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    let lease =
        options
            .open(&lease_path)
            .map_err(|error| SessionCatalogProjectionError::Recovery {
                message: format!("failed to open recovery lease: {error}"),
            })?;
    validate_regular_projection_file(&lease_path, "recovery lease")?;
    tighten_open_file_permissions(&lease)?;
    Ok(lease)
}

fn quarantine_sqlite_files(
    database_path: &Path,
) -> Result<Option<SessionCatalogQuarantine>, SessionCatalogProjectionError> {
    let mut sources = Vec::new();
    for path in sqlite_projection_files(database_path) {
        if validate_optional_regular_projection_file(&path, "SQLite projection file")? {
            sources.push(path);
        }
    }
    if sources.is_empty() {
        return Ok(None);
    }
    let quarantine_path = create_quarantine_directory(database_path)?;
    let mut moved = Vec::with_capacity(sources.len());
    for source in sources {
        let file_name =
            source
                .file_name()
                .ok_or_else(|| SessionCatalogProjectionError::Recovery {
                    message: "SQLite projection file has no filename".to_owned(),
                })?;
        let destination = quarantine_path.join(file_name);
        if let Err(error) = fs::rename(&source, &destination) {
            for (original, quarantined) in moved.iter().rev() {
                let _ = fs::rename(quarantined, original);
            }
            let _ = fs::remove_dir(&quarantine_path);
            return Err(SessionCatalogProjectionError::Recovery {
                message: format!("failed to quarantine SQLite projection file: {error}"),
            });
        }
        moved.push((source, destination));
    }
    Ok(Some(SessionCatalogQuarantine {
        path: quarantine_path,
        file_count: moved.len(),
    }))
}

fn quarantine_failed_rebuild(
    database_path: &Path,
    existing_quarantine: Option<&SessionCatalogQuarantine>,
) {
    let quarantine_path = existing_quarantine
        .map(|quarantine| Ok(quarantine.path.clone()))
        .unwrap_or_else(|| create_quarantine_directory(database_path));
    let Ok(quarantine_path) = quarantine_path else {
        return;
    };
    for source in sqlite_projection_files(database_path) {
        if !matches!(
            validate_optional_regular_projection_file(&source, "failed rebuild"),
            Ok(true)
        ) {
            continue;
        }
        let Some(file_name) = source.file_name() else {
            continue;
        };
        let mut failed_name = "failed-rebuild-".to_owned();
        failed_name.push_str(&file_name.to_string_lossy());
        let _ = fs::rename(source, quarantine_path.join(failed_name));
    }
}

fn create_quarantine_directory(
    database_path: &Path,
) -> Result<PathBuf, SessionCatalogProjectionError> {
    let parent = database_path
        .parent()
        .ok_or_else(|| SessionCatalogProjectionError::Recovery {
            message: "database has no parent directory".to_owned(),
        })?;
    let database_name = database_path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_else(|| "session-catalog".into());
    for attempt in 0..100_u8 {
        let path = parent.join(format!(
            ".{database_name}-quarantine-{}-{}-{attempt}",
            current_unix_ms(),
            std::process::id()
        ));
        match fs::create_dir(&path) {
            Ok(()) => {
                tighten_directory_permissions(&path)?;
                return Ok(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(SessionCatalogProjectionError::Recovery {
                    message: format!("failed to create quarantine directory: {error}"),
                });
            }
        }
    }
    Err(SessionCatalogProjectionError::Recovery {
        message: "failed to allocate a unique quarantine directory".to_owned(),
    })
}

fn sqlite_projection_files(database_path: &Path) -> [PathBuf; 3] {
    [
        database_path.to_path_buf(),
        sqlite_sidecar_path(database_path, "-wal"),
        sqlite_sidecar_path(database_path, "-shm"),
    ]
}

fn sqlite_sidecar_path(database_path: &Path, suffix: &str) -> PathBuf {
    let mut path = database_path.as_os_str().to_os_string();
    path.push(suffix);
    PathBuf::from(path)
}

fn validate_regular_projection_file(
    path: &Path,
    label: &'static str,
) -> Result<(), SessionCatalogProjectionError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|error| SessionCatalogProjectionError::Recovery {
            message: format!("failed to inspect {label}: {error}"),
        })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(SessionCatalogProjectionError::UnsafePath {
            message: format!("{label} must be a regular file and not a symlink"),
        });
    }
    Ok(())
}

fn validate_optional_regular_projection_file(
    path: &Path,
    label: &'static str,
) -> Result<bool, SessionCatalogProjectionError> {
    match fs::symlink_metadata(path) {
        Ok(_) => {
            validate_regular_projection_file(path, label)?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(SessionCatalogProjectionError::UnsafePath {
            message: format!("failed to inspect {label}: {error}"),
        }),
    }
}

#[cfg(unix)]
fn tighten_open_file_permissions(file: &File) -> Result<(), SessionCatalogProjectionError> {
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|error| SessionCatalogProjectionError::Recovery {
            message: format!("failed to restrict recovery lease permissions: {error}"),
        })
}

#[cfg(not(unix))]
fn tighten_open_file_permissions(_file: &File) -> Result<(), SessionCatalogProjectionError> {
    Ok(())
}

#[cfg(unix)]
fn tighten_directory_permissions(path: &Path) -> Result<(), SessionCatalogProjectionError> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| {
        SessionCatalogProjectionError::Recovery {
            message: format!("failed to restrict projection directory permissions: {error}"),
        }
    })
}

#[cfg(not(unix))]
fn tighten_directory_permissions(_path: &Path) -> Result<(), SessionCatalogProjectionError> {
    Ok(())
}

fn prepare_database_parent(path: &Path) -> Result<(), SessionCatalogProjectionError> {
    let parent = path
        .parent()
        .ok_or_else(|| SessionCatalogProjectionError::UnsafePath {
            message: "database has no parent directory".to_owned(),
        })?;
    match fs::symlink_metadata(parent) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(SessionCatalogProjectionError::UnsafePath {
                    message: "database parent must be a real directory".to_owned(),
                });
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(parent).map_err(|error| {
                SessionCatalogProjectionError::UnsafePath {
                    message: format!("failed to create database parent: {error}"),
                }
            })?;
            let metadata = fs::symlink_metadata(parent).map_err(|error| {
                SessionCatalogProjectionError::UnsafePath {
                    message: format!("failed to inspect created database parent: {error}"),
                }
            })?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(SessionCatalogProjectionError::UnsafePath {
                    message: "created database parent must be a real directory".to_owned(),
                });
            }
            tighten_directory_permissions(parent)?;
        }
        Err(error) => {
            return Err(SessionCatalogProjectionError::UnsafePath {
                message: format!("failed to inspect database parent: {error}"),
            });
        }
    }
    Ok(())
}

fn initialize_or_validate_schema(
    connection: &mut Connection,
    database_path: &Path,
) -> Result<(), SessionCatalogProjectionError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let application_id: i32 =
        transaction.pragma_query_value(None, "application_id", |row| row.get(0))?;
    let user_version: i32 =
        transaction.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if application_id == 0 && user_version == 0 {
        let user_object_count: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE substr(name, 1, 7) <> 'sqlite_'",
            [],
            |row| row.get(0),
        )?;
        if user_object_count != 0 {
            return Err(SessionCatalogProjectionError::IncompatibleSchema {
                application_id,
                user_version,
            });
        }
        tighten_catalog_permissions(database_path)?;
        transaction.execute_batch(CREATE_SCHEMA_SQL)?;
        transaction.pragma_update(None, "application_id", SESSION_CATALOG_APPLICATION_ID)?;
        transaction.pragma_update(None, "user_version", SESSION_CATALOG_SCHEMA_VERSION)?;
        transaction.commit()?;
        return Ok(());
    }
    if application_id != SESSION_CATALOG_APPLICATION_ID
        || user_version != i32::from(SESSION_CATALOG_SCHEMA_VERSION)
    {
        return Err(SessionCatalogProjectionError::IncompatibleSchema {
            application_id,
            user_version,
        });
    }
    validate_schema_object_names(&transaction).map_err(|()| {
        SessionCatalogProjectionError::IncompatibleSchema {
            application_id,
            user_version,
        }
    })?;
    tighten_catalog_permissions(database_path)?;
    transaction.commit()?;
    Ok(())
}

fn validate_schema_object_names(connection: &Connection) -> Result<(), ()> {
    let expected = BTreeSet::from([
        (
            "index".to_owned(),
            "session_catalog_entry_workspace_pinned_v1".to_owned(),
        ),
        (
            "index".to_owned(),
            "session_catalog_entry_workspace_provider_v1".to_owned(),
        ),
        (
            "index".to_owned(),
            "session_catalog_entry_workspace_sort_v1".to_owned(),
        ),
        (
            "index".to_owned(),
            "session_catalog_entry_workspace_state_v1".to_owned(),
        ),
        ("table".to_owned(), "session_catalog_entry_v1".to_owned()),
        (
            "table".to_owned(),
            "session_catalog_workspace_v1".to_owned(),
        ),
    ]);
    let mut statement = connection
        .prepare(
            "SELECT type, name FROM sqlite_schema \
             WHERE substr(name, 1, 7) <> 'sqlite_' ORDER BY type, name",
        )
        .map_err(|_| ())?;
    let actual = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|_| ())?
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(|_| ())?;
    (actual == expected).then_some(()).ok_or(())
}

fn ensure_secure_database_file(path: &Path) -> Result<(), SessionCatalogProjectionError> {
    match validate_optional_regular_projection_file(path, "database")? {
        true => Ok(()),
        false => {
            let mut options = OpenOptions::new();
            options.read(true).write(true).create_new(true);
            #[cfg(unix)]
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
            match options.open(path) {
                Ok(file) => tighten_open_file_permissions(&file),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    validate_regular_projection_file(path, "database")
                }
                Err(error) => Err(SessionCatalogProjectionError::UnsafePath {
                    message: format!("failed to create database securely: {error}"),
                }),
            }
        }
    }
}

fn tighten_catalog_permissions(path: &Path) -> Result<(), SessionCatalogProjectionError> {
    let parent = path
        .parent()
        .ok_or_else(|| SessionCatalogProjectionError::UnsafePath {
            message: "database has no parent directory".to_owned(),
        })?;
    tighten_directory_permissions(parent)?;
    for candidate in sqlite_projection_files(path) {
        if validate_optional_regular_projection_file(&candidate, "SQLite projection file")? {
            tighten_database_permissions(&candidate)?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn tighten_database_permissions(path: &Path) -> Result<(), SessionCatalogProjectionError> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|error| {
        SessionCatalogProjectionError::UnsafePath {
            message: format!("failed to restrict database permissions: {error}"),
        }
    })
}

#[cfg(not(unix))]
fn tighten_database_permissions(_path: &Path) -> Result<(), SessionCatalogProjectionError> {
    Ok(())
}

fn candidate_state(
    candidate: &SessionCandidate,
    limits: &LocalSessionLifecycleLimits,
    validated_bytes: &mut u64,
) -> LocalSessionCatalogState {
    if candidate.symlink_or_non_file {
        LocalSessionCatalogState::Invalid
    } else if candidate.bytes > limits.max_stream_bytes {
        LocalSessionCatalogState::Oversized
    } else if validated_bytes.saturating_add(candidate.bytes) > limits.max_total_validation_bytes {
        LocalSessionCatalogState::ScanBudgetExceeded
    } else {
        *validated_bytes = validated_bytes.saturating_add(candidate.bytes);
        LocalSessionCatalogState::Ready
    }
}

fn empty_projection_entry(
    workspace_id: &str,
    session_ref: String,
    source_state: LocalSessionCatalogState,
    candidate: &SessionCandidate,
    indexed_at_unix_ms: u64,
) -> SessionCatalogProjectionEntry {
    SessionCatalogProjectionEntry {
        workspace_id: workspace_id.to_owned(),
        session_ref,
        session_id: None,
        source_state,
        source_bytes: candidate.bytes,
        source_modified_at_unix_ms: candidate.modified_at_unix_ms,
        source_content_sha256: None,
        first_stream_sequence: None,
        last_stream_sequence: None,
        last_event_id: None,
        last_record_checksum: None,
        provider_name: None,
        model_name: None,
        title: None,
        user_message_count: 0,
        assistant_message_count: 0,
        tool_result_count: 0,
        control_entry_count: 0,
        latest_usage: None,
        latest_task: None,
        latest_readiness: None,
        pinned: false,
        indexed_at_unix_ms,
    }
}

fn ensure_source_stable(candidate: &SessionCandidate) -> Result<(), SessionCatalogProjectionError> {
    let metadata = fs::symlink_metadata(&candidate.path).map_err(source_error)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() != candidate.bytes
        || modified_at_unix_ms(&metadata) != candidate.modified_at_unix_ms
    {
        return Err(SessionCatalogProjectionError::Source {
            message: "session source changed while projection was being rebuilt".to_owned(),
        });
    }
    Ok(())
}

fn apply_session_projection(
    entry: &mut SessionCatalogProjectionEntry,
    projection: SessionListProjectionEntry,
    last_record: Option<&sigil_kernel::SessionStreamRecord>,
) {
    entry.session_id = Some(projection.session_id);
    entry.first_stream_sequence = Some(projection.first_stream_sequence);
    entry.last_stream_sequence = Some(projection.last_stream_sequence);
    entry.last_event_id = Some(projection.last_event_id);
    entry.last_record_checksum = last_record.map(|record| record.record_checksum().to_owned());
    entry.provider_name = projection
        .provider_name
        .map(|value| safe_bounded_projection_text(&value, SESSION_CATALOG_IDENTITY_MAX_BYTES));
    entry.model_name = projection
        .model_name
        .map(|value| safe_bounded_projection_text(&value, SESSION_CATALOG_IDENTITY_MAX_BYTES));
    entry.title = projection
        .title
        .map(|value| safe_bounded_projection_text(&value, SESSION_CATALOG_TITLE_MAX_BYTES));
    entry.user_message_count = projection.user_message_count;
    entry.assistant_message_count = projection.assistant_message_count;
    entry.tool_result_count = projection.tool_result_count;
    entry.control_entry_count = projection.control_entry_count;
    entry.latest_usage = projection.latest_usage;
    entry.latest_task = projection.latest_task.map(|mut task| {
        task.objective =
            safe_bounded_projection_text(&task.objective, SESSION_CATALOG_TITLE_MAX_BYTES);
        task
    });
    entry.latest_readiness = projection.latest_readiness;
}

fn safe_bounded_projection_text(value: &str, max_bytes: usize) -> String {
    let safe = safe_persistence_text(value);
    if safe.len() <= max_bytes {
        return safe;
    }
    let suffix = "…";
    let mut end = max_bytes.saturating_sub(suffix.len());
    while !safe.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    let mut bounded = safe[..end].to_owned();
    bounded.push_str(suffix);
    bounded
}

fn identity_conflict_count(entries: &[SessionCatalogProjectionEntry]) -> usize {
    let mut seen = BTreeSet::new();
    let mut conflicts = BTreeSet::new();
    for session_id in entries
        .iter()
        .filter(|entry| entry.source_state == LocalSessionCatalogState::Ready)
        .filter_map(|entry| entry.session_id.as_deref())
    {
        if !seen.insert(session_id) {
            conflicts.insert(session_id);
        }
    }
    conflicts.len()
}

fn workspace_generation_matches(
    transaction: &Transaction<'_>,
    workspace_id: &str,
    expected: Option<u64>,
) -> Result<bool, SessionCatalogProjectionError> {
    let current: Option<i64> = transaction
        .query_row(
            "SELECT generation FROM session_catalog_workspace_v1 WHERE workspace_id = ?1",
            params![workspace_id],
            |row| row.get(0),
        )
        .optional()?;
    let current = current
        .map(|value| {
            u64::try_from(value).map_err(|_| SessionCatalogProjectionError::IntegerRange {
                field: "generation",
            })
        })
        .transpose()?;
    Ok(current == expected)
}

fn apply_workspace_entry_changes(
    transaction: &Transaction<'_>,
    workspace_id: &str,
    generation: u64,
    reconciled_at_unix_ms: u64,
    degraded_source_count: usize,
    identity_conflict_count: usize,
    truncated_source_count: usize,
    updated_entries: &[&SessionCatalogProjectionEntry],
    removed_session_refs: &[&str],
) -> Result<(), SessionCatalogProjectionError> {
    upsert_workspace_metadata(
        transaction,
        workspace_id,
        generation,
        reconciled_at_unix_ms,
        degraded_source_count,
        identity_conflict_count,
        truncated_source_count,
    )?;
    let mut delete_statement = transaction.prepare(
        "DELETE FROM session_catalog_entry_v1 WHERE workspace_id = ?1 AND session_ref = ?2",
    )?;
    for session_ref in removed_session_refs {
        delete_statement.execute(params![workspace_id, session_ref])?;
    }
    drop(delete_statement);
    for entry in updated_entries {
        upsert_entry(transaction, entry)?;
    }
    Ok(())
}

fn upsert_workspace_metadata(
    transaction: &Transaction<'_>,
    workspace_id: &str,
    generation: u64,
    reconciled_at_unix_ms: u64,
    degraded_source_count: usize,
    identity_conflict_count: usize,
    truncated_source_count: usize,
) -> Result<(), SessionCatalogProjectionError> {
    transaction.execute(
        "INSERT INTO session_catalog_workspace_v1(\
             workspace_id, generation, reconciled_at_unix_ms, degraded_source_count, \
             identity_conflict_count, truncated_source_count\
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
         ON CONFLICT(workspace_id) DO UPDATE SET \
             generation = excluded.generation, \
             reconciled_at_unix_ms = excluded.reconciled_at_unix_ms, \
             degraded_source_count = excluded.degraded_source_count, \
             identity_conflict_count = excluded.identity_conflict_count, \
             truncated_source_count = excluded.truncated_source_count",
        params![
            workspace_id,
            to_i64(generation, "generation")?,
            to_i64(reconciled_at_unix_ms, "reconciled_at_unix_ms")?,
            usize_to_i64(degraded_source_count, "degraded_source_count")?,
            usize_to_i64(identity_conflict_count, "identity_conflict_count")?,
            usize_to_i64(truncated_source_count, "truncated_source_count")?,
        ],
    )?;
    Ok(())
}

fn upsert_entry(
    transaction: &Transaction<'_>,
    entry: &SessionCatalogProjectionEntry,
) -> Result<(), SessionCatalogProjectionError> {
    transaction.execute(
        "INSERT INTO session_catalog_entry_v1(\
             workspace_id, session_ref, session_id, source_state, source_bytes, \
             source_modified_at_unix_ms, source_content_sha256, first_stream_sequence, \
             last_stream_sequence, last_event_id, last_record_checksum, provider_name, model_name, \
             title, title_search, user_message_count, assistant_message_count, tool_result_count, \
             control_entry_count, latest_usage_json, latest_task_json, latest_readiness_json, \
             pinned, indexed_at_unix_ms\
         ) VALUES (\
             ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, \
             ?18, ?19, ?20, ?21, ?22, ?23, ?24\
         ) ON CONFLICT(workspace_id, session_ref) DO UPDATE SET \
             session_id = excluded.session_id, \
             source_state = excluded.source_state, \
             source_bytes = excluded.source_bytes, \
             source_modified_at_unix_ms = excluded.source_modified_at_unix_ms, \
             source_content_sha256 = excluded.source_content_sha256, \
             first_stream_sequence = excluded.first_stream_sequence, \
             last_stream_sequence = excluded.last_stream_sequence, \
             last_event_id = excluded.last_event_id, \
             last_record_checksum = excluded.last_record_checksum, \
             provider_name = excluded.provider_name, \
             model_name = excluded.model_name, \
             title = excluded.title, \
             title_search = excluded.title_search, \
             user_message_count = excluded.user_message_count, \
             assistant_message_count = excluded.assistant_message_count, \
             tool_result_count = excluded.tool_result_count, \
             control_entry_count = excluded.control_entry_count, \
             latest_usage_json = excluded.latest_usage_json, \
             latest_task_json = excluded.latest_task_json, \
             latest_readiness_json = excluded.latest_readiness_json, \
             pinned = excluded.pinned, \
             indexed_at_unix_ms = excluded.indexed_at_unix_ms",
        params![
            entry.workspace_id,
            entry.session_ref,
            entry.session_id,
            catalog_state_name(entry.source_state),
            to_i64(entry.source_bytes, "source_bytes")?,
            to_i64(
                entry.source_modified_at_unix_ms,
                "source_modified_at_unix_ms",
            )?,
            entry.source_content_sha256,
            optional_u64_to_i64(entry.first_stream_sequence, "first_stream_sequence")?,
            optional_u64_to_i64(entry.last_stream_sequence, "last_stream_sequence")?,
            entry.last_event_id,
            entry.last_record_checksum,
            entry.provider_name,
            entry.model_name,
            entry.title,
            entry.title.as_deref().map(str::to_lowercase),
            to_i64(entry.user_message_count, "user_message_count")?,
            to_i64(entry.assistant_message_count, "assistant_message_count")?,
            to_i64(entry.tool_result_count, "tool_result_count")?,
            to_i64(entry.control_entry_count, "control_entry_count")?,
            encode_optional(&entry.latest_usage)?,
            encode_optional(&entry.latest_task)?,
            encode_optional(&entry.latest_readiness)?,
            i64::from(entry.pinned),
            to_i64(entry.indexed_at_unix_ms, "indexed_at_unix_ms")?,
        ],
    )?;
    Ok(())
}

fn decode_entry_row(
    row: &rusqlite::Row<'_>,
) -> Result<SessionCatalogProjectionEntry, rusqlite::Error> {
    Ok(SessionCatalogProjectionEntry {
        workspace_id: row.get(0)?,
        session_ref: row.get(1)?,
        session_id: row.get(2)?,
        source_state: decode_catalog_state(row.get::<_, String>(3)?, 3)?,
        source_bytes: decode_u64(row.get(4)?, 4)?,
        source_modified_at_unix_ms: decode_u64(row.get(5)?, 5)?,
        source_content_sha256: row.get(6)?,
        first_stream_sequence: decode_optional_u64(row.get(7)?, 7)?,
        last_stream_sequence: decode_optional_u64(row.get(8)?, 8)?,
        last_event_id: row.get(9)?,
        last_record_checksum: row.get(10)?,
        provider_name: row.get(11)?,
        model_name: row.get(12)?,
        title: row.get(13)?,
        user_message_count: decode_u64(row.get(14)?, 14)?,
        assistant_message_count: decode_u64(row.get(15)?, 15)?,
        tool_result_count: decode_u64(row.get(16)?, 16)?,
        control_entry_count: decode_u64(row.get(17)?, 17)?,
        latest_usage: decode_optional(row.get(18)?, 18)?,
        latest_task: decode_optional(row.get(19)?, 19)?,
        latest_readiness: decode_optional(row.get(20)?, 20)?,
        pinned: row.get::<_, i64>(21)? != 0,
        indexed_at_unix_ms: decode_u64(row.get(22)?, 22)?,
    })
}

fn catalog_state_name(state: LocalSessionCatalogState) -> &'static str {
    match state {
        LocalSessionCatalogState::Ready => "ready",
        LocalSessionCatalogState::Oversized => "oversized",
        LocalSessionCatalogState::ScanBudgetExceeded => "scan_budget_exceeded",
        LocalSessionCatalogState::UnsupportedLegacy => "unsupported_legacy",
        LocalSessionCatalogState::Invalid => "invalid",
    }
}

fn decode_catalog_state(
    value: String,
    column: usize,
) -> Result<LocalSessionCatalogState, rusqlite::Error> {
    match value.as_str() {
        "ready" => Ok(LocalSessionCatalogState::Ready),
        "oversized" => Ok(LocalSessionCatalogState::Oversized),
        "scan_budget_exceeded" => Ok(LocalSessionCatalogState::ScanBudgetExceeded),
        "unsupported_legacy" => Ok(LocalSessionCatalogState::UnsupportedLegacy),
        "invalid" => Ok(LocalSessionCatalogState::Invalid),
        _ => Err(rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("unknown session catalog source state {value}"),
            )),
        )),
    }
}

fn encode_optional<T: Serialize>(
    value: &Option<T>,
) -> Result<Option<String>, SessionCatalogProjectionError> {
    value
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|error| SessionCatalogProjectionError::Encoding {
            message: error.to_string(),
        })
}

fn decode_optional<T: for<'de> Deserialize<'de>>(
    value: Option<String>,
    column: usize,
) -> Result<Option<T>, rusqlite::Error> {
    value
        .map(|value| {
            serde_json::from_str(&value).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    column,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })
        })
        .transpose()
}

fn decode_u64(value: i64, column: usize) -> Result<u64, rusqlite::Error> {
    u64::try_from(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })
}

fn decode_usize(value: i64, column: usize) -> Result<usize, rusqlite::Error> {
    usize::try_from(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })
}

fn decode_optional_u64(value: Option<i64>, column: usize) -> Result<Option<u64>, rusqlite::Error> {
    value.map(|value| decode_u64(value, column)).transpose()
}

fn to_i64(value: u64, field: &'static str) -> Result<i64, SessionCatalogProjectionError> {
    i64::try_from(value).map_err(|_| SessionCatalogProjectionError::IntegerRange { field })
}

fn usize_to_i64(value: usize, field: &'static str) -> Result<i64, SessionCatalogProjectionError> {
    i64::try_from(value).map_err(|_| SessionCatalogProjectionError::IntegerRange { field })
}

fn optional_u64_to_i64(
    value: Option<u64>,
    field: &'static str,
) -> Result<Option<i64>, SessionCatalogProjectionError> {
    value.map(|value| to_i64(value, field)).transpose()
}

fn source_error(error: impl std::fmt::Display) -> SessionCatalogProjectionError {
    SessionCatalogProjectionError::Source {
        message: error.to_string(),
    }
}

fn current_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().try_into().unwrap_or(u64::MAX))
        .unwrap_or(0)
}

use rusqlite::OptionalExtension as _;

#[cfg(test)]
#[path = "../tests/session_projection_tests.rs"]
mod tests;
