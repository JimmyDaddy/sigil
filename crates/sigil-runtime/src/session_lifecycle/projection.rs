use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sigil_kernel::{
    JsonlSessionStore, SessionListProjectionEntry, SessionListReadinessSummary,
    SessionListTaskSummary, SessionListUsageSummary, SessionStreamCompatibilityError,
    session_list_projection_from_records,
};
use thiserror::Error as ThisError;

use super::{
    LocalSessionCatalogState, LocalSessionLifecycleLimits, LocalSessionLifecycleService,
    SessionCandidate, direct_jsonl_candidates, hash_file_bounded, modified_at_unix_ms,
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

/// Workspace-bound owner of the rebuildable global SQLite session catalog.
#[derive(Debug, Clone)]
pub struct SessionCatalogProjectionService {
    lifecycle: LocalSessionLifecycleService,
    database_path: PathBuf,
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

    fn open_connection(&self) -> Result<Connection, SessionCatalogProjectionError> {
        prepare_database_parent(&self.database_path)?;
        if self.database_path.exists() {
            let metadata = fs::symlink_metadata(&self.database_path).map_err(|error| {
                SessionCatalogProjectionError::UnsafePath {
                    message: format!("failed to inspect database file: {error}"),
                }
            })?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(SessionCatalogProjectionError::UnsafePath {
                    message: "database must be a regular file and not a symlink".to_owned(),
                });
            }
        }
        let mut connection = Connection::open_with_flags(
            &self.database_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        connection.busy_timeout(SESSION_CATALOG_BUSY_TIMEOUT)?;
        connection.execute_batch(
            "PRAGMA trusted_schema = OFF; \
             PRAGMA foreign_keys = ON; \
             PRAGMA journal_mode = WAL; \
             PRAGMA synchronous = FULL;",
        )?;
        initialize_or_validate_schema(&mut connection)?;
        tighten_database_permissions(&self.database_path)?;
        Ok(connection)
    }

    fn reconcile_internal(
        &self,
        force_rebuild: bool,
    ) -> Result<SessionCatalogProjectionReconcileReport, SessionCatalogProjectionError> {
        for retry_count in 0..=MAX_RECONCILE_RETRIES {
            let mut connection = self.open_connection()?;
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
            let updated_source_count = next_by_ref
                .iter()
                .filter(|(session_ref, entry)| existing.get(*session_ref) != Some(*entry))
                .count();
            let removed_source_count = existing
                .keys()
                .filter(|session_ref| !next_by_ref.contains_key(*session_ref))
                .count();
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
                replace_workspace_entries(
                    &transaction,
                    &self.lifecycle.workspace_id,
                    generation,
                    reconciled_at_unix_ms,
                    degraded_source_count,
                    identity_conflict_count,
                    scan.truncated_source_count,
                    &scan.entries,
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
                scanned_source_count: scan.entries.len(),
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
        if !self.lifecycle.session_dir.exists() {
            return Ok(SessionCatalogScan::default());
        }
        let metadata = fs::symlink_metadata(&self.lifecycle.session_dir).map_err(source_error)?;
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
                entries.push(reused);
                reused_source_count += 1;
                continue;
            }
            entries.push(self.project_candidate(candidate, state, &pins, indexed_at_unix_ms)?);
        }
        Ok(SessionCatalogScan {
            entries,
            reused_source_count,
            truncated_source_count,
        })
    }

    fn project_candidate(
        &self,
        candidate: SessionCandidate,
        initial_state: LocalSessionCatalogState,
        pins: &BTreeMap<sigil_kernel::SessionRef, (String, bool)>,
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
        Ok(entry)
    }
}

#[derive(Debug, Default)]
struct SessionCatalogScan {
    entries: Vec<SessionCatalogProjectionEntry>,
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
    previous.source_state == state
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

fn prepare_database_parent(path: &Path) -> Result<(), SessionCatalogProjectionError> {
    let parent = path
        .parent()
        .ok_or_else(|| SessionCatalogProjectionError::UnsafePath {
            message: "database has no parent directory".to_owned(),
        })?;
    if parent.exists() {
        let metadata = fs::symlink_metadata(parent).map_err(|error| {
            SessionCatalogProjectionError::UnsafePath {
                message: format!("failed to inspect database parent: {error}"),
            }
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(SessionCatalogProjectionError::UnsafePath {
                message: "database parent must be a real directory".to_owned(),
            });
        }
    } else {
        fs::create_dir_all(parent).map_err(|error| SessionCatalogProjectionError::UnsafePath {
            message: format!("failed to create database parent: {error}"),
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
    }
    Ok(())
}

fn initialize_or_validate_schema(
    connection: &mut Connection,
) -> Result<(), SessionCatalogProjectionError> {
    let application_id: i32 =
        connection.pragma_query_value(None, "application_id", |row| row.get(0))?;
    let user_version: i32 =
        connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if application_id == 0 && user_version == 0 {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
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
    entry.provider_name = projection.provider_name;
    entry.model_name = projection.model_name;
    entry.title = projection.title;
    entry.user_message_count = projection.user_message_count;
    entry.assistant_message_count = projection.assistant_message_count;
    entry.tool_result_count = projection.tool_result_count;
    entry.control_entry_count = projection.control_entry_count;
    entry.latest_usage = projection.latest_usage;
    entry.latest_task = projection.latest_task;
    entry.latest_readiness = projection.latest_readiness;
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

fn replace_workspace_entries(
    transaction: &Transaction<'_>,
    workspace_id: &str,
    generation: u64,
    reconciled_at_unix_ms: u64,
    degraded_source_count: usize,
    identity_conflict_count: usize,
    truncated_source_count: usize,
    entries: &[SessionCatalogProjectionEntry],
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
    transaction.execute(
        "DELETE FROM session_catalog_entry_v1 WHERE workspace_id = ?1",
        params![workspace_id],
    )?;
    for entry in entries {
        insert_entry(transaction, entry)?;
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

fn insert_entry(
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
         )",
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
