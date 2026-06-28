//! Persistent projection store primitives for RFC-0001 materialized views.
//!
//! JSONL remains the source of truth. This module stores rebuildable projection views together
//! with their cursor so event application and cursor advancement persist as one atomic replace.

use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    marker::PhantomData,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{
    ProjectionApplyDecision, ProjectionCursor, VerificationStateProjection,
    VerificationStateProjectionSnapshot, projection_apply_decision_for_record,
    session::{SessionLogEntry, SessionStreamRecord},
};

#[cfg(test)]
#[path = "tests/projection_tests.rs"]
mod tests;

pub const FILE_PROJECTION_STORE_SCHEMA_VERSION: u16 = 1;

static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Persisted projection state plus the cursor that proves how far it has consumed a stream.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ProjectionStoreState<T> {
    pub projection: T,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<ProjectionCursor>,
}

impl<T> Default for ProjectionStoreState<T>
where
    T: Default,
{
    fn default() -> Self {
        Self {
            projection: T::default(),
            cursor: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
struct ProjectionStoreEnvelope<T> {
    schema_version: u16,
    projection_name: String,
    projection_schema_version: u16,
    state: ProjectionStoreState<T>,
}

/// File-backed projection store.
///
/// The implementation deliberately stores projection and cursor in one envelope and writes the
/// envelope through a temporary file followed by atomic rename. That keeps the first productized
/// projection store independent from a database while preserving the RFC-0001 transaction rule.
#[derive(Debug, Clone)]
pub struct FileProjectionStore<T> {
    path: PathBuf,
    projection_name: String,
    projection_schema_version: u16,
    _marker: PhantomData<T>,
}

impl<T> FileProjectionStore<T>
where
    T: Clone + Default + DeserializeOwned + Serialize,
{
    pub fn new(
        path: impl AsRef<Path>,
        projection_name: impl Into<String>,
        projection_schema_version: u16,
    ) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            projection_name: projection_name.into(),
            projection_schema_version,
            _marker: PhantomData,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Loads the latest persisted projection state, or returns an empty state when no store exists.
    ///
    /// # Errors
    ///
    /// Returns an error when the persisted envelope is malformed, belongs to another projection,
    /// or was written with an incompatible schema version.
    pub fn load(&self) -> Result<ProjectionStoreState<T>> {
        if !self.path.exists() {
            return Ok(ProjectionStoreState::default());
        }
        let bytes = fs::read(&self.path)
            .with_context(|| format!("failed to read projection store {}", self.path.display()))?;
        let envelope: ProjectionStoreEnvelope<T> = serde_json::from_slice(&bytes)
            .with_context(|| format!("failed to parse projection store {}", self.path.display()))?;
        self.validate_envelope(&envelope)?;
        Ok(envelope.state)
    }

    /// Applies one stream record and persists projection + cursor in the same atomic file update.
    ///
    /// The reducer is only called when RFC-0001 cursor rules say the record is new. Duplicate
    /// replay with matching id/checksum is ignored without rewriting the projection store.
    ///
    /// # Errors
    ///
    /// Fails closed on session mismatch, sequence gaps, cursor conflicts, reducer failures, or
    /// failed durable writes.
    pub fn apply_record<F>(
        &self,
        record: &SessionStreamRecord,
        apply: F,
    ) -> Result<ProjectionApplyDecision>
    where
        F: FnOnce(&mut T, &SessionStreamRecord) -> Result<()>,
    {
        let mut state = self.load()?;
        let next_cursor = record.projection_cursor(self.projection_schema_version);
        match projection_apply_decision_for_record(
            state.cursor.as_ref(),
            &next_cursor.session_id,
            next_cursor.last_applied_stream_sequence,
            &next_cursor.last_applied_event_id,
            &next_cursor.last_applied_record_checksum,
        )? {
            ProjectionApplyDecision::IgnoreAlreadyApplied => {
                Ok(ProjectionApplyDecision::IgnoreAlreadyApplied)
            }
            ProjectionApplyDecision::Apply => {
                let mut projection = state.projection.clone();
                apply(&mut projection, record)?;
                state.projection = projection;
                state.cursor = Some(next_cursor);
                self.save_state(&state)?;
                Ok(ProjectionApplyDecision::Apply)
            }
        }
    }

    /// Rebuilds the projection store from a stream snapshot and atomically replaces the store.
    ///
    /// # Errors
    ///
    /// Returns the first reducer or cursor error encountered while replaying the records.
    pub fn rebuild_from_records<F>(
        &self,
        records: &[SessionStreamRecord],
        mut apply: F,
    ) -> Result<ProjectionStoreState<T>>
    where
        F: FnMut(&mut T, &SessionStreamRecord) -> Result<()>,
    {
        let mut state = ProjectionStoreState::<T>::default();
        for record in records {
            let next_cursor = record.projection_cursor(self.projection_schema_version);
            match projection_apply_decision_for_record(
                state.cursor.as_ref(),
                &next_cursor.session_id,
                next_cursor.last_applied_stream_sequence,
                &next_cursor.last_applied_event_id,
                &next_cursor.last_applied_record_checksum,
            )? {
                ProjectionApplyDecision::IgnoreAlreadyApplied => continue,
                ProjectionApplyDecision::Apply => {
                    apply(&mut state.projection, record)?;
                    state.cursor = Some(next_cursor);
                }
            }
        }
        self.save_state(&state)?;
        Ok(state)
    }

    fn save_state(&self, state: &ProjectionStoreState<T>) -> Result<()> {
        if let Some(cursor) = &state.cursor
            && cursor.projection_schema_version != self.projection_schema_version
        {
            bail!(
                "projection cursor schema {} does not match store schema {}",
                cursor.projection_schema_version,
                self.projection_schema_version
            );
        }
        let envelope = ProjectionStoreEnvelope {
            schema_version: FILE_PROJECTION_STORE_SCHEMA_VERSION,
            projection_name: self.projection_name.clone(),
            projection_schema_version: self.projection_schema_version,
            state: state.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&envelope)
            .context("failed to serialize projection store envelope")?;
        write_atomic(&self.path, &bytes)
    }

    fn validate_envelope(&self, envelope: &ProjectionStoreEnvelope<T>) -> Result<()> {
        if envelope.schema_version != FILE_PROJECTION_STORE_SCHEMA_VERSION {
            bail!(
                "unsupported projection store schema {}",
                envelope.schema_version
            );
        }
        if envelope.projection_name != self.projection_name {
            bail!(
                "projection store contains {} but expected {}",
                envelope.projection_name,
                self.projection_name
            );
        }
        if envelope.projection_schema_version != self.projection_schema_version {
            bail!(
                "projection schema {} does not match expected {}",
                envelope.projection_schema_version,
                self.projection_schema_version
            );
        }
        if let Some(cursor) = &envelope.state.cursor
            && cursor.projection_schema_version != self.projection_schema_version
        {
            bail!(
                "projection cursor schema {} does not match envelope schema {}",
                cursor.projection_schema_version,
                self.projection_schema_version
            );
        }
        Ok(())
    }
}

impl FileProjectionStore<VerificationStateProjectionSnapshot> {
    pub fn verification(path: impl AsRef<Path>) -> Self {
        Self::new(
            path,
            "verification_state",
            crate::session::VERIFICATION_STATE_PROJECTION_SCHEMA_VERSION,
        )
    }

    pub fn apply_verification_record(
        &self,
        record: &SessionStreamRecord,
    ) -> Result<ProjectionApplyDecision> {
        self.apply_record(record, apply_verification_projection_snapshot_record)
    }

    pub fn rebuild_verification_from_records(
        &self,
        records: &[SessionStreamRecord],
    ) -> Result<ProjectionStoreState<VerificationStateProjectionSnapshot>> {
        self.rebuild_from_records(records, apply_verification_projection_snapshot_record)
    }
}

fn apply_verification_projection_snapshot_record(
    snapshot: &mut VerificationStateProjectionSnapshot,
    record: &SessionStreamRecord,
) -> Result<()> {
    let mut projection = VerificationStateProjection::from(snapshot.clone());
    if let Some(domain_record) = record.domain_event_record()?
        && let Some(SessionLogEntry::Control(control)) =
            crate::session::session_entry_from_domain_event(&domain_record.event)?
    {
        projection.apply_control_entry(&control);
    }
    *snapshot = VerificationStateProjectionSnapshot::from(&projection);
    Ok(())
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("projection.json");
    let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let temp_path = path.with_file_name(format!(
        ".{file_name}.{}.{}.{}.tmp",
        std::process::id(),
        timestamp,
        counter
    ));
    let write_result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temp_path)
            .with_context(|| {
                format!(
                    "failed to create temporary projection store {}",
                    temp_path.display()
                )
            })?;
        file.write_all(bytes).with_context(|| {
            format!(
                "failed to write temporary projection store {}",
                temp_path.display()
            )
        })?;
        file.sync_all().with_context(|| {
            format!(
                "failed to sync temporary projection store {}",
                temp_path.display()
            )
        })?;
        drop(file);
        fs::rename(&temp_path, path).with_context(|| {
            format!(
                "failed to replace projection store {} with {}",
                path.display(),
                temp_path.display()
            )
        })?;
        sync_parent_dir(path)?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    write_result
}

#[cfg(unix)]
fn sync_parent_dir(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        File::open(parent)
            .and_then(|file| file.sync_all())
            .with_context(|| format!("failed to sync projection dir {}", parent.display()))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn sync_parent_dir(_path: &Path) -> Result<()> {
    Ok(())
}
