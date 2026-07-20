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
    AgentThreadStateProjection, ApprovalMode, ControlEntry, EvidenceScope, ModelMessage,
    NetworkEffect, PermissionRisk, ProjectionApplyDecision, ProjectionCursor, RunStatus,
    TaskRunStatus, ToolApprovalAuditAction, ToolApprovalUserDecision, ToolErrorKind,
    ToolExecutionStatus, ToolOperation, ToolSubjectScope, UsageStats, VerificationStateProjection,
    VerificationStateProjectionSnapshot, VerificationVerdict, VisibleCompletionState,
    projection_apply_decision_for_record,
    session::{SessionLogEntry, SessionStreamRecord},
};

#[cfg(test)]
#[path = "tests/projection_tests.rs"]
mod tests;

pub const FILE_PROJECTION_STORE_SCHEMA_VERSION: u16 = 1;
pub const AGENT_GRAPH_PROJECTION_SCHEMA_VERSION: u16 = 1;
pub const DISPATCH_TRACE_PROJECTION_SCHEMA_VERSION: u16 = 1;
pub const SESSION_LIST_PROJECTION_SCHEMA_VERSION: u16 = 1;

const AGENT_GRAPH_PROJECTION_NAME: &str = "agent_graph";
const DISPATCH_TRACE_PROJECTION_NAME: &str = "dispatch_trace";
const SESSION_LIST_PROJECTION_NAME: &str = "session_list";
const SESSION_LIST_TITLE_MAX_CHARS: usize = 160;
const DISPATCH_TRACE_DESTINATION_LIMIT: usize = 8;

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

/// Product-facing query families that projection views are expected to serve.
///
/// This is a contract layer, not a storage backend decision. Query pressure is measured against
/// these stable shapes before Sigil considers escalating from file-backed projections to a
/// materialized database view.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionQueryFamily {
    SessionList,
    SessionDetail,
    TaskState,
    AgentGraph,
    DispatchTrace,
    VerificationState,
    CostSummary,
    Checkpoint,
    ContextSource,
}

/// Logical scope of a projection query.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionQueryScope {
    SingleSession,
    Workspace,
    CrossSession,
}

/// Product surface that asks for a projection query.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionQuerySurface {
    Tui,
    Cli,
    Http,
    Desktop,
    Ide,
    Daemon,
    DeveloperTool,
}

/// Stable query contract used by TUI, HTTP and future desktop adapters.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ProjectionQueryContract {
    pub family: ProjectionQueryFamily,
    pub scope: ProjectionQueryScope,
    pub surface: ProjectionQuerySurface,
    #[serde(default)]
    pub requires_pagination: bool,
    #[serde(default)]
    pub requires_filtering: bool,
    #[serde(default)]
    pub requires_sorting: bool,
    #[serde(default)]
    pub requires_search: bool,
    #[serde(default)]
    pub requires_fresh_live_state: bool,
}

impl ProjectionQueryContract {
    pub fn new(
        family: ProjectionQueryFamily,
        scope: ProjectionQueryScope,
        surface: ProjectionQuerySurface,
    ) -> Self {
        Self {
            family,
            scope,
            surface,
            requires_pagination: false,
            requires_filtering: false,
            requires_sorting: false,
            requires_search: false,
            requires_fresh_live_state: false,
        }
    }

    #[must_use]
    pub fn with_pagination(mut self, value: bool) -> Self {
        self.requires_pagination = value;
        self
    }

    #[must_use]
    pub fn with_filtering(mut self, value: bool) -> Self {
        self.requires_filtering = value;
        self
    }

    #[must_use]
    pub fn with_sorting(mut self, value: bool) -> Self {
        self.requires_sorting = value;
        self
    }

    #[must_use]
    pub fn with_search(mut self, value: bool) -> Self {
        self.requires_search = value;
        self
    }

    #[must_use]
    pub fn with_fresh_live_state(mut self, value: bool) -> Self {
        self.requires_fresh_live_state = value;
        self
    }
}

/// Thresholds used to decide whether projection pressure is only worth measuring or already
/// strong enough to justify a materialized-view design.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ProjectionPressureThresholds {
    pub sessions_scanned_warn: u64,
    pub sessions_scanned_escalate: u64,
    pub records_scanned_warn: u64,
    pub records_scanned_escalate: u64,
    pub query_latency_warn_ms: u64,
    pub query_latency_escalate_ms: u64,
    pub rebuild_latency_warn_ms: u64,
    pub rebuild_latency_escalate_ms: u64,
    pub repeated_log_scan_warn: u64,
    pub repeated_log_scan_escalate: u64,
    pub product_surface_warn: u64,
    pub product_surface_escalate: u64,
}

impl Default for ProjectionPressureThresholds {
    fn default() -> Self {
        Self {
            sessions_scanned_warn: 50,
            sessions_scanned_escalate: 250,
            records_scanned_warn: 10_000,
            records_scanned_escalate: 50_000,
            query_latency_warn_ms: 100,
            query_latency_escalate_ms: 300,
            rebuild_latency_warn_ms: 500,
            rebuild_latency_escalate_ms: 2_000,
            repeated_log_scan_warn: 2,
            repeated_log_scan_escalate: 5,
            product_surface_warn: 2,
            product_surface_escalate: 3,
        }
    }
}

/// One measured product query sample.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ProjectionPressureSample {
    pub contract: ProjectionQueryContract,
    #[serde(default)]
    pub sessions_scanned: u64,
    #[serde(default)]
    pub records_scanned: u64,
    #[serde(default)]
    pub repeated_log_scans: u64,
    #[serde(default)]
    pub product_surface_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_latency_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rebuild_latency_ms: Option<u64>,
}

impl ProjectionPressureSample {
    pub fn new(contract: ProjectionQueryContract) -> Self {
        Self {
            contract,
            sessions_scanned: 0,
            records_scanned: 0,
            repeated_log_scans: 0,
            product_surface_count: 1,
            query_latency_ms: None,
            rebuild_latency_ms: None,
        }
    }
}

/// Recommendation derived from projection pressure metrics.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionStoreRecommendation {
    KeepFileBacked,
    MeasureMore,
    EscalateMaterializedView,
}

/// Stable reasons attached to pressure recommendations.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionPressureReason {
    NoPressure,
    LiveStateBoundary,
    CrossSessionQuery,
    PaginationRequired,
    FilteringRequired,
    SortingRequired,
    SearchRequired,
    SessionScanHigh,
    RecordScanHigh,
    QueryLatencyHigh,
    RebuildLatencyHigh,
    RepeatedLogScanHigh,
    MultiSurfaceDemand,
}

/// Evaluation result for one projection pressure sample.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ProjectionPressureEvaluation {
    pub recommendation: ProjectionStoreRecommendation,
    #[serde(default)]
    pub reasons: Vec<ProjectionPressureReason>,
}

pub fn evaluate_projection_pressure(
    sample: &ProjectionPressureSample,
    thresholds: &ProjectionPressureThresholds,
) -> ProjectionPressureEvaluation {
    if sample.contract.requires_fresh_live_state {
        return ProjectionPressureEvaluation {
            recommendation: ProjectionStoreRecommendation::KeepFileBacked,
            reasons: vec![ProjectionPressureReason::LiveStateBoundary],
        };
    }

    let mut reasons = Vec::new();
    let mut should_escalate = false;
    let mut should_measure = false;

    push_contract_pressure_reasons(&sample.contract, &mut reasons, &mut should_measure);

    evaluate_pressure_value(
        sample.sessions_scanned,
        thresholds.sessions_scanned_warn,
        thresholds.sessions_scanned_escalate,
        ProjectionPressureReason::SessionScanHigh,
        &mut reasons,
        &mut should_measure,
        &mut should_escalate,
    );
    evaluate_pressure_value(
        sample.records_scanned,
        thresholds.records_scanned_warn,
        thresholds.records_scanned_escalate,
        ProjectionPressureReason::RecordScanHigh,
        &mut reasons,
        &mut should_measure,
        &mut should_escalate,
    );
    evaluate_pressure_value(
        sample.repeated_log_scans,
        thresholds.repeated_log_scan_warn,
        thresholds.repeated_log_scan_escalate,
        ProjectionPressureReason::RepeatedLogScanHigh,
        &mut reasons,
        &mut should_measure,
        &mut should_escalate,
    );
    evaluate_pressure_value(
        sample.product_surface_count,
        thresholds.product_surface_warn,
        thresholds.product_surface_escalate,
        ProjectionPressureReason::MultiSurfaceDemand,
        &mut reasons,
        &mut should_measure,
        &mut should_escalate,
    );
    if let Some(latency) = sample.query_latency_ms {
        evaluate_pressure_value(
            latency,
            thresholds.query_latency_warn_ms,
            thresholds.query_latency_escalate_ms,
            ProjectionPressureReason::QueryLatencyHigh,
            &mut reasons,
            &mut should_measure,
            &mut should_escalate,
        );
    }
    if let Some(latency) = sample.rebuild_latency_ms {
        evaluate_pressure_value(
            latency,
            thresholds.rebuild_latency_warn_ms,
            thresholds.rebuild_latency_escalate_ms,
            ProjectionPressureReason::RebuildLatencyHigh,
            &mut reasons,
            &mut should_measure,
            &mut should_escalate,
        );
    }

    reasons.sort();
    reasons.dedup();
    if reasons.is_empty() {
        reasons.push(ProjectionPressureReason::NoPressure);
    }

    let recommendation = if should_escalate {
        ProjectionStoreRecommendation::EscalateMaterializedView
    } else if should_measure {
        ProjectionStoreRecommendation::MeasureMore
    } else {
        ProjectionStoreRecommendation::KeepFileBacked
    };

    ProjectionPressureEvaluation {
        recommendation,
        reasons,
    }
}

fn push_contract_pressure_reasons(
    contract: &ProjectionQueryContract,
    reasons: &mut Vec<ProjectionPressureReason>,
    should_measure: &mut bool,
) {
    if contract.scope == ProjectionQueryScope::CrossSession {
        reasons.push(ProjectionPressureReason::CrossSessionQuery);
        *should_measure = true;
    }
    if contract.requires_pagination {
        reasons.push(ProjectionPressureReason::PaginationRequired);
        *should_measure = true;
    }
    if contract.requires_filtering {
        reasons.push(ProjectionPressureReason::FilteringRequired);
        *should_measure = true;
    }
    if contract.requires_sorting {
        reasons.push(ProjectionPressureReason::SortingRequired);
        *should_measure = true;
    }
    if contract.requires_search {
        reasons.push(ProjectionPressureReason::SearchRequired);
        *should_measure = true;
    }
}

fn evaluate_pressure_value(
    value: u64,
    warn_threshold: u64,
    escalate_threshold: u64,
    reason: ProjectionPressureReason,
    reasons: &mut Vec<ProjectionPressureReason>,
    should_measure: &mut bool,
    should_escalate: &mut bool,
) {
    if value >= escalate_threshold {
        reasons.push(reason);
        *should_escalate = true;
    } else if value >= warn_threshold {
        reasons.push(reason);
        *should_measure = true;
    }
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

impl FileProjectionStore<AgentThreadStateProjection> {
    pub fn agent_graph(path: impl AsRef<Path>) -> Self {
        Self::new(
            path,
            AGENT_GRAPH_PROJECTION_NAME,
            AGENT_GRAPH_PROJECTION_SCHEMA_VERSION,
        )
    }

    pub fn apply_agent_graph_record(
        &self,
        record: &SessionStreamRecord,
    ) -> Result<ProjectionApplyDecision> {
        self.apply_record(record, apply_agent_graph_projection_snapshot_record)
    }

    pub fn rebuild_agent_graph_from_records(
        &self,
        records: &[SessionStreamRecord],
    ) -> Result<ProjectionStoreState<AgentThreadStateProjection>> {
        let mut output = self.rebuild_from_records_with_report(
            records,
            apply_agent_graph_projection_snapshot_record,
        )?;
        output.state.projection.finalize_replay();
        self.save_state(&output.state)?;
        Ok(output.state)
    }
}

impl FileProjectionStore<DispatchTraceProjectionSnapshot> {
    pub fn dispatch_trace(path: impl AsRef<Path>) -> Self {
        Self::new(
            path,
            DISPATCH_TRACE_PROJECTION_NAME,
            DISPATCH_TRACE_PROJECTION_SCHEMA_VERSION,
        )
    }

    pub fn apply_dispatch_trace_record(
        &self,
        record: &SessionStreamRecord,
    ) -> Result<ProjectionApplyDecision> {
        self.apply_record(record, apply_dispatch_trace_projection_snapshot_record)
    }

    pub fn rebuild_dispatch_trace_from_records(
        &self,
        records: &[SessionStreamRecord],
    ) -> Result<ProjectionStoreState<DispatchTraceProjectionSnapshot>> {
        self.rebuild_from_records(records, apply_dispatch_trace_projection_snapshot_record)
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

pub fn agent_graph_projection_from_records(
    records: &[SessionStreamRecord],
) -> Result<AgentThreadStateProjection> {
    let mut projection = AgentThreadStateProjection::default();
    let mut cursor: Option<ProjectionCursor> = None;
    for record in records {
        apply_agent_graph_projection_record(&mut projection, &mut cursor, record)?;
    }
    projection.finalize_replay();
    Ok(projection)
}

pub fn dispatch_trace_projection_from_records(
    records: &[SessionStreamRecord],
) -> Result<DispatchTraceProjectionSnapshot> {
    let mut projection = DispatchTraceProjection::default();
    let mut cursor: Option<ProjectionCursor> = None;
    for record in records {
        apply_dispatch_trace_projection_record(&mut projection, &mut cursor, record)?;
    }
    Ok(DispatchTraceProjectionSnapshot::from(&projection))
}

/// Redacted, query-friendly projection for tool and agent dispatch observability.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct DispatchTraceProjectionSnapshot {
    #[serde(default)]
    pub traces: Vec<DispatchTraceEntry>,
    pub summary: DispatchTraceSummary,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_usage: Option<DispatchTraceUsageSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_readiness: Option<SessionListReadinessSummary>,
}

impl DispatchTraceProjectionSnapshot {
    pub fn trace(&self, trace_id: &str) -> Option<&DispatchTraceEntry> {
        self.traces.iter().find(|entry| entry.trace_id == trace_id)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct DispatchTraceSummary {
    pub total_traces: u64,
    pub tool_traces: u64,
    pub agent_traces: u64,
    pub running_traces: u64,
    pub completed_traces: u64,
    pub failed_traces: u64,
    pub blocked_traces: u64,
    pub egress_events: u64,
    pub redacted_egress_events: u64,
    pub truncated_observations: u64,
    pub changed_file_traces: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct DispatchTraceUsageSummary {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cache_hit_tokens: u64,
    pub cache_miss_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct DispatchTraceEntry {
    pub trace_id: String,
    pub kind: DispatchTraceKind,
    pub status: DispatchTraceStatus,
    pub first_stream_sequence: u64,
    pub last_stream_sequence: u64,
    pub last_event_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_agent_thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_profile_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_approval_action: Option<ToolApprovalAuditAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_user_decision: Option<ToolApprovalUserDecision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_policy_decision: Option<ApprovalMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_network_effect: Option<NetworkEffect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_local_policy_decision: Option<ApprovalMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_network_policy_decision: Option<ApprovalMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_source_policy_decision: Option<ApprovalMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_operation: Option<ToolOperation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_risk: Option<PermissionRisk>,
    pub subject_count: u64,
    pub external_subject_count: u64,
    pub changed_file_count: u64,
    pub egress_count: u64,
    pub egress_redacted_count: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub egress_destinations: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub observation_truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omitted_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub returned_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_content_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<ToolErrorKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_retryable: Option<bool>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DispatchTraceKind {
    Tool,
    Agent,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DispatchTraceStatus {
    #[default]
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
    Blocked,
    Unknown,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct DispatchTraceProjection {
    traces: BTreeMap<String, DispatchTraceEntry>,
    latest_usage: Option<DispatchTraceUsageSummary>,
    latest_readiness: Option<SessionListReadinessSummary>,
}

impl DispatchTraceProjection {
    fn apply_record(&mut self, record: &SessionStreamRecord) -> Result<()> {
        if let Some(domain_record) = record.domain_event_record()?
            && let Some(SessionLogEntry::Control(control)) =
                crate::session::session_entry_from_domain_event(&domain_record.event)?
        {
            self.apply_control_entry(control, record);
        }
        Ok(())
    }

    fn apply_control_entry(&mut self, control: ControlEntry, record: &SessionStreamRecord) {
        match control {
            ControlEntry::ToolApproval(entry) => self.apply_tool_approval(entry, record),
            ControlEntry::ToolExecution(entry) => self.apply_tool_execution(*entry, record),
            ControlEntry::ToolEgress(entry) => self.apply_tool_egress(*entry, record),
            ControlEntry::AgentThreadStarted(entry) => self.apply_agent_started(entry, record),
            ControlEntry::AgentThreadStatusChanged(entry) => {
                let trace_id = agent_trace_id(entry.thread_id.as_str());
                let trace =
                    self.ensure_agent_trace(&trace_id, entry.thread_id.as_str().to_owned(), record);
                trace.status = dispatch_status_from_agent_status(entry.status);
                update_trace_cursor(trace, record);
            }
            ControlEntry::AgentThreadResultRecorded(entry) => {
                let trace_id = agent_trace_id(entry.result.thread_id.as_str());
                let trace = self.ensure_agent_trace(
                    &trace_id,
                    entry.result.thread_id.as_str().to_owned(),
                    record,
                );
                trace.status = dispatch_status_from_agent_terminal(entry.result.status);
                trace.changed_file_count = entry.result.changed_paths.len() as u64;
                if let Some(usage) = entry.result.usage {
                    trace.input_tokens = Some(usage.input_tokens);
                    trace.output_tokens = Some(usage.output_tokens);
                    trace.total_tokens = Some(usage.total_tokens);
                }
                update_trace_cursor(trace, record);
            }
            ControlEntry::UsageSnapshot(usage) => {
                self.latest_usage = Some(DispatchTraceUsageSummary::from_usage(&usage));
            }
            ControlEntry::ReadinessEvaluated(entry) => {
                self.latest_readiness = Some(SessionListReadinessSummary {
                    scope: entry.scope,
                    run_status: entry.evaluation.run_status,
                    verification_verdict: entry.evaluation.verification_verdict,
                    visible_state: entry.evaluation.visible_state,
                });
            }
            _ => {}
        }
    }

    fn apply_tool_approval(
        &mut self,
        entry: crate::ToolApprovalEntry,
        record: &SessionStreamRecord,
    ) {
        let trace_id = tool_trace_id(&entry.call_id);
        let trace = self.ensure_tool_trace(&trace_id, entry.call_id.clone(), record);
        trace.tool_name = Some(entry.tool_name);
        trace.latest_approval_action = Some(entry.action);
        trace.latest_user_decision = entry.user_decision;
        trace.latest_policy_decision = Some(entry.policy_decision);
        trace.latest_network_effect = entry.network_effect;
        trace.latest_local_policy_decision = Some(entry.local_policy_decision);
        trace.latest_network_policy_decision = Some(entry.network_policy_decision);
        trace.latest_source_policy_decision = Some(entry.source_policy_decision);
        trace.latest_operation = entry.operation;
        trace.latest_risk = entry.risk;
        trace.subject_count = entry.subjects.len() as u64;
        trace.external_subject_count = external_subject_count(&entry.subjects);
        trace.status = dispatch_status_from_approval(entry.action, entry.user_decision);
        update_trace_cursor(trace, record);
    }

    fn apply_tool_execution(
        &mut self,
        entry: crate::ToolExecutionEntry,
        record: &SessionStreamRecord,
    ) {
        let trace_id = tool_trace_id(&entry.call_id);
        let trace = self.ensure_tool_trace(&trace_id, entry.call_id.clone(), record);
        trace.tool_name = Some(entry.tool_name);
        trace.status = dispatch_status_from_tool_execution(entry.status);
        trace.duration_ms = entry.duration_ms.or(entry.metadata.duration_ms);
        trace.subject_count = entry.subjects.len() as u64;
        trace.external_subject_count = external_subject_count(&entry.subjects);
        trace.changed_file_count = entry
            .changed_files
            .len()
            .max(entry.metadata.changed_files.len()) as u64;
        trace.exit_code = entry.metadata.exit_code;
        trace.observation_truncated = entry.metadata.truncated;
        trace.omitted_bytes = entry.metadata.omitted_bytes;
        trace.returned_bytes = entry.metadata.returned_bytes.or(entry.metadata.bytes);
        trace.total_bytes = entry.metadata.total_bytes;
        trace.model_content_hash = entry.model_content_hash;
        if let Some(error) = entry.error {
            trace.error_kind = Some(error.kind);
            trace.error_retryable = Some(error.retryable);
        }
        update_trace_cursor(trace, record);
    }

    fn apply_tool_egress(&mut self, entry: crate::ToolEgressEntry, record: &SessionStreamRecord) {
        let trace_id = tool_trace_id(&entry.call_id);
        let trace = self.ensure_tool_trace(&trace_id, entry.call_id.clone(), record);
        trace.tool_name = Some(entry.tool_name);
        trace.egress_count += 1;
        trace.egress_redacted_count += u64::from(entry.redacted);
        trace.subject_count = trace.subject_count.max(entry.subjects.len() as u64);
        trace.external_subject_count = trace
            .external_subject_count
            .max(external_subject_count(&entry.subjects));
        push_limited_unique_destination(&mut trace.egress_destinations, entry.destination);
        update_trace_cursor(trace, record);
    }

    fn apply_agent_started(
        &mut self,
        entry: crate::AgentThreadStartedEntry,
        record: &SessionStreamRecord,
    ) {
        let trace_id = agent_trace_id(entry.thread_id.as_str());
        let trace = self.ensure_agent_trace(&trace_id, entry.thread_id.as_str().to_owned(), record);
        trace.agent_profile_id = Some(entry.profile_id.as_str().to_owned());
        trace.parent_agent_thread_id = entry
            .parent_thread_id
            .as_ref()
            .map(|thread_id| thread_id.as_str().to_owned());
        trace.status = DispatchTraceStatus::Running;
        update_trace_cursor(trace, record);
    }

    fn ensure_tool_trace(
        &mut self,
        trace_id: &str,
        call_id: String,
        record: &SessionStreamRecord,
    ) -> &mut DispatchTraceEntry {
        self.traces
            .entry(trace_id.to_owned())
            .or_insert_with(|| DispatchTraceEntry::new_tool(trace_id, call_id, record))
    }

    fn ensure_agent_trace(
        &mut self,
        trace_id: &str,
        thread_id: String,
        record: &SessionStreamRecord,
    ) -> &mut DispatchTraceEntry {
        self.traces
            .entry(trace_id.to_owned())
            .or_insert_with(|| DispatchTraceEntry::new_agent(trace_id, thread_id, record))
    }
}

impl From<DispatchTraceProjectionSnapshot> for DispatchTraceProjection {
    fn from(snapshot: DispatchTraceProjectionSnapshot) -> Self {
        Self {
            traces: snapshot
                .traces
                .into_iter()
                .map(|entry| (entry.trace_id.clone(), entry))
                .collect(),
            latest_usage: snapshot.latest_usage,
            latest_readiness: snapshot.latest_readiness,
        }
    }
}

impl From<&DispatchTraceProjection> for DispatchTraceProjectionSnapshot {
    fn from(projection: &DispatchTraceProjection) -> Self {
        let traces = projection.traces.values().cloned().collect::<Vec<_>>();
        Self {
            summary: dispatch_trace_summary(&traces),
            traces,
            latest_usage: projection.latest_usage.clone(),
            latest_readiness: projection.latest_readiness.clone(),
        }
    }
}

impl DispatchTraceEntry {
    fn new_tool(trace_id: &str, call_id: String, record: &SessionStreamRecord) -> Self {
        Self {
            trace_id: trace_id.to_owned(),
            kind: DispatchTraceKind::Tool,
            status: DispatchTraceStatus::Pending,
            first_stream_sequence: record.stream_sequence(),
            last_stream_sequence: record.stream_sequence(),
            last_event_id: record.event_id().to_owned(),
            call_id: Some(call_id),
            tool_name: None,
            agent_thread_id: None,
            parent_agent_thread_id: None,
            agent_profile_id: None,
            latest_approval_action: None,
            latest_user_decision: None,
            latest_policy_decision: None,
            latest_network_effect: None,
            latest_local_policy_decision: None,
            latest_network_policy_decision: None,
            latest_source_policy_decision: None,
            latest_operation: None,
            latest_risk: None,
            subject_count: 0,
            external_subject_count: 0,
            changed_file_count: 0,
            egress_count: 0,
            egress_redacted_count: 0,
            egress_destinations: Vec::new(),
            duration_ms: None,
            exit_code: None,
            observation_truncated: false,
            omitted_bytes: None,
            returned_bytes: None,
            total_bytes: None,
            input_tokens: None,
            output_tokens: None,
            total_tokens: None,
            model_content_hash: None,
            error_kind: None,
            error_retryable: None,
        }
    }

    fn new_agent(trace_id: &str, thread_id: String, record: &SessionStreamRecord) -> Self {
        let mut trace = Self::new_tool(trace_id, String::new(), record);
        trace.kind = DispatchTraceKind::Agent;
        trace.call_id = None;
        trace.agent_thread_id = Some(thread_id);
        trace
    }
}

impl DispatchTraceUsageSummary {
    fn from_usage(usage: &UsageStats) -> Self {
        Self {
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            cache_hit_tokens: usage.cache_hit_tokens,
            cache_miss_tokens: usage.cache_miss_tokens,
        }
    }
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

fn apply_agent_graph_projection_snapshot_record(
    projection: &mut AgentThreadStateProjection,
    record: &SessionStreamRecord,
) -> Result<()> {
    if let Some(domain_record) = record.domain_event_record()?
        && let Some(SessionLogEntry::Control(control)) =
            crate::session::session_entry_from_domain_event(&domain_record.event)?
    {
        projection.apply_control_entry(&control);
    }
    Ok(())
}

fn apply_agent_graph_projection_record(
    projection: &mut AgentThreadStateProjection,
    cursor: &mut Option<ProjectionCursor>,
    record: &SessionStreamRecord,
) -> Result<()> {
    let next_cursor = record.projection_cursor(AGENT_GRAPH_PROJECTION_SCHEMA_VERSION);
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
    apply_agent_graph_projection_snapshot_record(projection, record)?;
    *cursor = Some(next_cursor);
    Ok(())
}

fn apply_dispatch_trace_projection_snapshot_record(
    snapshot: &mut DispatchTraceProjectionSnapshot,
    record: &SessionStreamRecord,
) -> Result<()> {
    let mut projection = DispatchTraceProjection::from(snapshot.clone());
    projection.apply_record(record)?;
    *snapshot = DispatchTraceProjectionSnapshot::from(&projection);
    Ok(())
}

fn apply_dispatch_trace_projection_record(
    projection: &mut DispatchTraceProjection,
    cursor: &mut Option<ProjectionCursor>,
    record: &SessionStreamRecord,
) -> Result<()> {
    let next_cursor = record.projection_cursor(DISPATCH_TRACE_PROJECTION_SCHEMA_VERSION);
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

fn tool_trace_id(call_id: &str) -> String {
    format!("tool:{call_id}")
}

fn agent_trace_id(thread_id: &str) -> String {
    format!("agent:{thread_id}")
}

fn update_trace_cursor(trace: &mut DispatchTraceEntry, record: &SessionStreamRecord) {
    trace.last_stream_sequence = record.stream_sequence();
    trace.last_event_id = record.event_id().to_owned();
}

fn external_subject_count(subjects: &[crate::ToolSubjectAudit]) -> u64 {
    subjects
        .iter()
        .filter(|subject| subject.scope == ToolSubjectScope::External)
        .count() as u64
}

fn push_limited_unique_destination(destinations: &mut Vec<String>, destination: String) {
    if destinations.iter().any(|existing| existing == &destination) {
        return;
    }
    if destinations.len() < DISPATCH_TRACE_DESTINATION_LIMIT {
        destinations.push(destination);
    }
}

fn dispatch_status_from_approval(
    action: ToolApprovalAuditAction,
    decision: Option<ToolApprovalUserDecision>,
) -> DispatchTraceStatus {
    match (action, decision) {
        (ToolApprovalAuditAction::Resolved, Some(ToolApprovalUserDecision::Denied))
        | (ToolApprovalAuditAction::PreviewFailed, _) => DispatchTraceStatus::Blocked,
        _ => DispatchTraceStatus::Pending,
    }
}

fn dispatch_status_from_tool_execution(status: ToolExecutionStatus) -> DispatchTraceStatus {
    match status {
        ToolExecutionStatus::Started => DispatchTraceStatus::Running,
        ToolExecutionStatus::Completed => DispatchTraceStatus::Completed,
        ToolExecutionStatus::Failed => DispatchTraceStatus::Failed,
        ToolExecutionStatus::Cancelled => DispatchTraceStatus::Cancelled,
        ToolExecutionStatus::Interrupted => DispatchTraceStatus::Interrupted,
    }
}

fn dispatch_status_from_agent_status(status: crate::AgentThreadStatus) -> DispatchTraceStatus {
    match status {
        crate::AgentThreadStatus::Started | crate::AgentThreadStatus::Running => {
            DispatchTraceStatus::Running
        }
        crate::AgentThreadStatus::Blocked => DispatchTraceStatus::Blocked,
        crate::AgentThreadStatus::Completed | crate::AgentThreadStatus::Closed => {
            DispatchTraceStatus::Completed
        }
        crate::AgentThreadStatus::Failed | crate::AgentThreadStatus::Unavailable => {
            DispatchTraceStatus::Failed
        }
        crate::AgentThreadStatus::Cancelled => DispatchTraceStatus::Cancelled,
        crate::AgentThreadStatus::Interrupted => DispatchTraceStatus::Interrupted,
        crate::AgentThreadStatus::Unknown => DispatchTraceStatus::Unknown,
    }
}

fn dispatch_status_from_agent_terminal(
    status: crate::AgentThreadTerminalStatus,
) -> DispatchTraceStatus {
    match status {
        crate::AgentThreadTerminalStatus::Completed => DispatchTraceStatus::Completed,
        crate::AgentThreadTerminalStatus::Failed => DispatchTraceStatus::Failed,
        crate::AgentThreadTerminalStatus::Cancelled => DispatchTraceStatus::Cancelled,
        crate::AgentThreadTerminalStatus::Interrupted => DispatchTraceStatus::Interrupted,
        crate::AgentThreadTerminalStatus::Unknown => DispatchTraceStatus::Unknown,
    }
}

fn dispatch_trace_summary(traces: &[DispatchTraceEntry]) -> DispatchTraceSummary {
    let mut summary = DispatchTraceSummary {
        total_traces: traces.len() as u64,
        ..DispatchTraceSummary::default()
    };
    for trace in traces {
        match trace.kind {
            DispatchTraceKind::Tool => summary.tool_traces += 1,
            DispatchTraceKind::Agent => summary.agent_traces += 1,
        }
        match trace.status {
            DispatchTraceStatus::Running => summary.running_traces += 1,
            DispatchTraceStatus::Completed => summary.completed_traces += 1,
            DispatchTraceStatus::Failed
            | DispatchTraceStatus::Cancelled
            | DispatchTraceStatus::Interrupted => summary.failed_traces += 1,
            DispatchTraceStatus::Blocked => summary.blocked_traces += 1,
            DispatchTraceStatus::Pending | DispatchTraceStatus::Unknown => {}
        }
        summary.egress_events += trace.egress_count;
        summary.redacted_egress_events += trace.egress_redacted_count;
        summary.truncated_observations += u64::from(trace.observation_truncated);
        summary.changed_file_traces += u64::from(trace.changed_file_count > 0);
    }
    summary
}

fn session_list_event_type(record: &SessionStreamRecord) -> String {
    match record {
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
        ControlEntry::SessionModelSelected { model_name } => {
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
