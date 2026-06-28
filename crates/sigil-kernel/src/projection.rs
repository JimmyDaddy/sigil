//! Persistent projection store primitives for RFC-0001 materialized views.
//!
//! JSONL remains the source of truth. This module stores rebuildable projection views together
//! with their cursor so event application and cursor advancement persist as one atomic replace.

use std::{
    collections::BTreeMap,
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
    ControlEntry, EvidenceScope, ModelMessage, ProjectionApplyDecision, ProjectionCursor,
    RunStatus, TaskRunStatus, VerificationStateProjection, VerificationStateProjectionSnapshot,
    VerificationVerdict, VisibleCompletionState, projection_apply_decision_for_record,
    session::{SessionLogEntry, SessionStreamRecord},
};

#[cfg(test)]
#[path = "tests/projection_tests.rs"]
mod tests;

pub const FILE_PROJECTION_STORE_SCHEMA_VERSION: u16 = 1;
pub const SESSION_LIST_PROJECTION_SCHEMA_VERSION: u16 = 1;

const SESSION_LIST_PROJECTION_NAME: &str = "session_list";
const SESSION_LIST_TITLE_MAX_CHARS: usize = 160;

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

/// Diagnostics for one projection rebuild.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ProjectionRebuildReport {
    pub applied_records: u64,
    pub ignored_records: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<ProjectionCursor>,
}

/// Rebuild result including persisted state and replay diagnostics.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ProjectionRebuildOutput<T> {
    pub state: ProjectionStoreState<T>,
    pub report: ProjectionRebuildReport,
}

/// Common interface for rebuildable projection stores.
pub trait ProjectionStore<T> {
    fn load_state(&self) -> Result<ProjectionStoreState<T>>;

    fn apply_stream_record<F>(
        &self,
        record: &SessionStreamRecord,
        apply: F,
    ) -> Result<ProjectionApplyDecision>
    where
        F: FnOnce(&mut T, &SessionStreamRecord) -> Result<()>;

    fn rebuild_stream_records<F>(
        &self,
        records: &[SessionStreamRecord],
        apply: F,
    ) -> Result<ProjectionRebuildOutput<T>>
    where
        F: FnMut(&mut T, &SessionStreamRecord) -> Result<()>;
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
        apply: F,
    ) -> Result<ProjectionStoreState<T>>
    where
        F: FnMut(&mut T, &SessionStreamRecord) -> Result<()>,
    {
        Ok(self.rebuild_from_records_with_report(records, apply)?.state)
    }

    /// Rebuilds the projection store and returns replay diagnostics.
    ///
    /// # Errors
    ///
    /// Returns the first reducer or cursor error encountered while replaying the records.
    pub fn rebuild_from_records_with_report<F>(
        &self,
        records: &[SessionStreamRecord],
        mut apply: F,
    ) -> Result<ProjectionRebuildOutput<T>>
    where
        F: FnMut(&mut T, &SessionStreamRecord) -> Result<()>,
    {
        let mut state = ProjectionStoreState::<T>::default();
        let mut report = ProjectionRebuildReport::default();
        for record in records {
            let next_cursor = record.projection_cursor(self.projection_schema_version);
            match projection_apply_decision_for_record(
                state.cursor.as_ref(),
                &next_cursor.session_id,
                next_cursor.last_applied_stream_sequence,
                &next_cursor.last_applied_event_id,
                &next_cursor.last_applied_record_checksum,
            )? {
                ProjectionApplyDecision::IgnoreAlreadyApplied => {
                    report.ignored_records += 1;
                    continue;
                }
                ProjectionApplyDecision::Apply => {
                    apply(&mut state.projection, record)?;
                    state.cursor = Some(next_cursor);
                    report.applied_records += 1;
                }
            }
        }
        report.cursor = state.cursor.clone();
        self.save_state(&state)?;
        Ok(ProjectionRebuildOutput { state, report })
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

impl<T> ProjectionStore<T> for FileProjectionStore<T>
where
    T: Clone + Default + DeserializeOwned + Serialize,
{
    fn load_state(&self) -> Result<ProjectionStoreState<T>> {
        self.load()
    }

    fn apply_stream_record<F>(
        &self,
        record: &SessionStreamRecord,
        apply: F,
    ) -> Result<ProjectionApplyDecision>
    where
        F: FnOnce(&mut T, &SessionStreamRecord) -> Result<()>,
    {
        self.apply_record(record, apply)
    }

    fn rebuild_stream_records<F>(
        &self,
        records: &[SessionStreamRecord],
        apply: F,
    ) -> Result<ProjectionRebuildOutput<T>>
    where
        F: FnMut(&mut T, &SessionStreamRecord) -> Result<()>,
    {
        self.rebuild_from_records_with_report(records, apply)
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

impl FileProjectionStore<SessionListProjectionSnapshot> {
    pub fn session_list(path: impl AsRef<Path>) -> Self {
        Self::new(
            path,
            SESSION_LIST_PROJECTION_NAME,
            SESSION_LIST_PROJECTION_SCHEMA_VERSION,
        )
    }

    pub fn apply_session_list_record(
        &self,
        record: &SessionStreamRecord,
    ) -> Result<ProjectionApplyDecision> {
        self.apply_record(record, apply_session_list_projection_snapshot_record)
    }

    pub fn rebuild_session_list_from_records(
        &self,
        records: &[SessionStreamRecord],
    ) -> Result<ProjectionStoreState<SessionListProjectionSnapshot>> {
        self.rebuild_from_records(records, apply_session_list_projection_snapshot_record)
    }
}

/// JSON-friendly projection used by product surfaces to list durable sessions without decoding
/// session internals at the UI layer.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct SessionListProjectionSnapshot {
    #[serde(default)]
    pub sessions: Vec<SessionListProjectionEntry>,
}

impl SessionListProjectionSnapshot {
    pub fn session(&self, session_id: &str) -> Option<&SessionListProjectionEntry> {
        self.sessions
            .iter()
            .find(|entry| entry.session_id == session_id)
    }

    pub fn latest_session(&self) -> Option<&SessionListProjectionEntry> {
        self.sessions
            .iter()
            .max_by_key(|entry| entry.last_stream_sequence)
    }
}

/// One materialized session row. This is intentionally compact: large tool output and message
/// bodies remain in the durable stream and are not duplicated into the projection.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct SessionListProjectionEntry {
    pub session_id: String,
    pub first_stream_sequence: u64,
    pub last_stream_sequence: u64,
    pub last_event_id: String,
    pub last_event_type: String,
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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct SessionListUsageSummary {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cache_hit_tokens: u64,
    pub cache_miss_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct SessionListTaskSummary {
    pub task_id: String,
    pub objective: String,
    pub status: TaskRunStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct SessionListReadinessSummary {
    pub scope: EvidenceScope,
    pub run_status: RunStatus,
    pub verification_verdict: VerificationVerdict,
    pub visible_state: VisibleCompletionState,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct SessionListProjection {
    sessions: BTreeMap<String, SessionListProjectionEntry>,
}

impl SessionListProjection {
    fn apply_record(&mut self, record: &SessionStreamRecord) -> Result<()> {
        let session_id = record.session_id().to_owned();
        let event_type = session_list_event_type(record);
        let entry =
            self.sessions
                .entry(session_id.clone())
                .or_insert_with(|| SessionListProjectionEntry {
                    session_id,
                    first_stream_sequence: record.stream_sequence(),
                    last_stream_sequence: record.stream_sequence(),
                    last_event_id: record.event_id().to_owned(),
                    last_event_type: event_type.clone(),
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
                });

        entry.last_stream_sequence = record.stream_sequence();
        entry.last_event_id = record.event_id().to_owned();
        entry.last_event_type = event_type;

        if let Some(domain_record) = record.domain_event_record()?
            && let Some(session_entry) =
                crate::session::session_entry_from_domain_event(&domain_record.event)?
        {
            apply_session_entry_to_session_list(entry, &session_entry);
        }
        Ok(())
    }
}

impl From<SessionListProjectionSnapshot> for SessionListProjection {
    fn from(snapshot: SessionListProjectionSnapshot) -> Self {
        Self {
            sessions: snapshot
                .sessions
                .into_iter()
                .map(|entry| (entry.session_id.clone(), entry))
                .collect(),
        }
    }
}

impl From<&SessionListProjection> for SessionListProjectionSnapshot {
    fn from(projection: &SessionListProjection) -> Self {
        Self {
            sessions: projection.sessions.values().cloned().collect(),
        }
    }
}

pub fn session_list_projection_from_records(
    records: &[SessionStreamRecord],
) -> Result<SessionListProjectionSnapshot> {
    let mut projection = SessionListProjection::default();
    let mut cursor: Option<ProjectionCursor> = None;
    for record in records {
        apply_session_list_projection_record(&mut projection, &mut cursor, record)?;
    }
    Ok(SessionListProjectionSnapshot::from(&projection))
}

fn apply_session_list_projection_snapshot_record(
    snapshot: &mut SessionListProjectionSnapshot,
    record: &SessionStreamRecord,
) -> Result<()> {
    let mut projection = SessionListProjection::from(snapshot.clone());
    projection.apply_record(record)?;
    *snapshot = SessionListProjectionSnapshot::from(&projection);
    Ok(())
}

fn apply_session_list_projection_record(
    projection: &mut SessionListProjection,
    cursor: &mut Option<ProjectionCursor>,
    record: &SessionStreamRecord,
) -> Result<()> {
    let next_cursor = record.projection_cursor(SESSION_LIST_PROJECTION_SCHEMA_VERSION);
    match projection_apply_decision_for_record(
        cursor.as_ref(),
        &next_cursor.session_id,
        next_cursor.last_applied_stream_sequence,
        &next_cursor.last_applied_event_id,
        &next_cursor.last_applied_record_checksum,
    )? {
        ProjectionApplyDecision::IgnoreAlreadyApplied => return Ok(()),
        ProjectionApplyDecision::Apply => {}
    }
    projection.apply_record(record)?;
    *cursor = Some(next_cursor);
    Ok(())
}

fn session_list_event_type(record: &SessionStreamRecord) -> String {
    match record {
        SessionStreamRecord::Legacy { .. } => "legacy".to_owned(),
        SessionStreamRecord::Stored(event) => event.event_type.clone(),
    }
}

fn apply_session_entry_to_session_list(
    projection: &mut SessionListProjectionEntry,
    entry: &SessionLogEntry,
) {
    match entry {
        SessionLogEntry::User(message) => {
            projection.user_message_count += 1;
            if projection.title.is_none()
                && let Some(title) = session_title_from_message(message)
            {
                projection.title = Some(title);
            }
        }
        SessionLogEntry::Assistant(_) => {
            projection.assistant_message_count += 1;
        }
        SessionLogEntry::ToolResult(_) => {
            projection.tool_result_count += 1;
        }
        SessionLogEntry::Control(control) => {
            apply_control_entry_to_session_list(projection, control)
        }
    }
}

fn apply_control_entry_to_session_list(
    projection: &mut SessionListProjectionEntry,
    control: &ControlEntry,
) {
    projection.control_entry_count += 1;
    match control {
        ControlEntry::SessionIdentity {
            provider_name,
            model_name,
        } => {
            projection.provider_name = Some(provider_name.clone());
            projection.model_name = Some(model_name.clone());
        }
        ControlEntry::UsageSnapshot(usage) => {
            projection.latest_usage = Some(SessionListUsageSummary {
                prompt_tokens: usage.prompt_tokens,
                completion_tokens: usage.completion_tokens,
                cache_hit_tokens: usage.cache_hit_tokens,
                cache_miss_tokens: usage.cache_miss_tokens,
            });
        }
        ControlEntry::TaskRun(task) => {
            projection.latest_task = Some(SessionListTaskSummary {
                task_id: task.task_id.as_str().to_owned(),
                objective: truncate_title(&task.objective),
                status: task.status,
            });
        }
        ControlEntry::ReadinessEvaluated(readiness) => {
            projection.latest_readiness = Some(SessionListReadinessSummary {
                scope: readiness.scope.clone(),
                run_status: readiness.evaluation.run_status,
                verification_verdict: readiness.evaluation.verification_verdict,
                visible_state: readiness.evaluation.visible_state,
            });
        }
        _ => {}
    }
}

fn session_title_from_message(message: &ModelMessage) -> Option<String> {
    let content = message.content.as_deref()?.trim();
    (!content.is_empty()).then(|| truncate_title(content))
}

fn truncate_title(value: &str) -> String {
    let mut output = value
        .chars()
        .take(SESSION_LIST_TITLE_MAX_CHARS)
        .collect::<String>();
    if output.len() < value.len() {
        output.push_str("...");
    }
    output
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
