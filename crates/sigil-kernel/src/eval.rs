//! RFC-0013 eval harness result model.
//!
//! This module intentionally contains provider-neutral result types plus deterministic developer
//! harness helpers. Model-backed runners and product surfaces are separate slices.

use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    ControlEntry, EventId, JsonlSessionStore, ProjectionCursor, RunStatus, SessionLogEntry,
    VerificationVerdict, VisibleCompletionState, WorkspaceTrust,
    session::SESSION_ENTRY_PROJECTION_SCHEMA_VERSION,
};

#[cfg(test)]
#[path = "tests/eval_tests.rs"]
mod tests;

pub type EvalCaseId = String;
pub type EvalRunId = String;
pub type EvalFixtureId = String;
pub type EvalEvidenceId = String;
pub type EvalStepId = String;
pub type EvalToolCallId = String;

/// Source RFC and execution slice covered by one eval case.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub struct EvalCaseProvenance {
    pub rfc_id: String,
    pub slice_id: String,
}

impl EvalCaseProvenance {
    #[must_use]
    pub fn new(rfc_id: impl Into<String>, slice_id: impl Into<String>) -> Self {
        Self {
            rfc_id: rfc_id.into(),
            slice_id: slice_id.into(),
        }
    }
}

/// Provider-neutral outcome bucket for one eval run.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvalOutcomeKind {
    VerifiedSuccess,
    Completed,
    CompletedUnverified,
    Blocked,
    Failed,
    FailedVerification,
    Cancelled,
    Interrupted,
    PermissionDenied,
    SandboxDenied,
}

impl EvalOutcomeKind {
    #[must_use]
    pub fn from_completion(
        run_status: RunStatus,
        verification_verdict: VerificationVerdict,
        failures: &[EvalFailure],
    ) -> Self {
        if failures
            .iter()
            .any(|failure| failure.kind == EvalFailureKind::SandboxDenied)
        {
            return Self::SandboxDenied;
        }
        if failures
            .iter()
            .any(|failure| failure.kind == EvalFailureKind::PermissionDenied)
        {
            return Self::PermissionDenied;
        }
        match VisibleCompletionState::derive(run_status, verification_verdict) {
            VisibleCompletionState::Verified => Self::VerifiedSuccess,
            VisibleCompletionState::Completed => Self::Completed,
            VisibleCompletionState::CompletedUnverified => Self::CompletedUnverified,
            VisibleCompletionState::FailedVerification => Self::FailedVerification,
            VisibleCompletionState::Failed => Self::Failed,
            VisibleCompletionState::Cancelled => Self::Cancelled,
            VisibleCompletionState::Interrupted => Self::Interrupted,
            VisibleCompletionState::NeedsUser | VisibleCompletionState::Paused => Self::Blocked,
            VisibleCompletionState::Running => Self::Blocked,
        }
    }
}

/// Stable failure taxonomy for deterministic and model-backed evals.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvalFailureKind {
    Model,
    Tool,
    PermissionDenied,
    PathEscapeDenied,
    SandboxDenied,
    ApprovalDenied,
    VerificationFailed,
    VerificationMissing,
    VerificationStale,
    VerificationInconclusive,
    Integrity,
    Timeout,
    Interrupted,
    Cancelled,
    Harness,
    Unknown,
}

/// One failure or denial observed during an eval run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct EvalFailure {
    pub kind: EvalFailureKind,
    pub message: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<EvalEvidenceRef>,
}

impl EvalFailure {
    #[must_use]
    pub fn new(kind: EvalFailureKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            evidence: Vec::new(),
        }
    }
}

/// Kind of evidence referenced from an eval result.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvalEvidenceKind {
    DurableEvent,
    Receipt,
    Artifact,
    SessionLog,
    ToolCall,
    ProjectionCursor,
}

/// Stable pointer to durable evidence without embedding the evidence body.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct EvalEvidenceRef {
    pub kind: EvalEvidenceKind,
    pub id: EvalEvidenceId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<EventId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_ref: Option<String>,
}

impl EvalEvidenceRef {
    #[must_use]
    pub fn durable_event(id: impl Into<String>, event_id: impl Into<EventId>) -> Self {
        Self {
            kind: EvalEvidenceKind::DurableEvent,
            id: id.into(),
            event_id: Some(event_id.into()),
            artifact_ref: None,
        }
    }
}

/// Minimal tool-call summary recorded by eval output.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct EvalToolCallSummary {
    pub tool_call_id: EvalToolCallId,
    pub tool_name: String,
    pub status: EvalToolCallStatus,
}

/// Provider-neutral tool-call terminal status for eval reporting.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvalToolCallStatus {
    Succeeded,
    Failed,
    Denied,
    Interrupted,
}

/// Follow-up action required for an eval result to become verified or actionable.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct EvalRequiredAction {
    pub kind: EvalRequiredActionKind,
    pub message: String,
}

impl EvalRequiredAction {
    /// Creates one provider-neutral required action.
    #[must_use]
    pub fn new(kind: EvalRequiredActionKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

/// Provider-neutral required action taxonomy for eval reporting.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvalRequiredActionKind {
    RunCheck,
    ConfigureVerification,
    ApproveWorkspace,
    InspectEvidence,
}

/// Metadata needed to compare eval runs over time.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct EvalRunMetadata {
    pub case_id: EvalCaseId,
    pub run_id: EvalRunId,
    pub fixture_id: EvalFixtureId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_fixture_commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sigil_version: Option<String>,
    pub provider: String,
    pub model: String,
    pub model_parameters_hash: String,
    pub tool_schema_digest: String,
    pub config_hash: String,
    pub sandbox_backend: String,
    pub os_toolchain: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provenance: Vec<EvalCaseProvenance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_outcome: Option<EvalOutcomeKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_verification_verdict: Option<VerificationVerdict>,
}

impl EvalRunMetadata {
    #[must_use]
    pub fn deterministic(
        case_id: impl Into<String>,
        run_id: impl Into<String>,
        fixture_id: impl Into<String>,
    ) -> Self {
        Self {
            case_id: case_id.into(),
            run_id: run_id.into(),
            fixture_id: fixture_id.into(),
            repo_fixture_commit: None,
            sigil_version: None,
            provider: "fake".to_owned(),
            model: "deterministic".to_owned(),
            model_parameters_hash: "sha256:deterministic".to_owned(),
            tool_schema_digest: "sha256:deterministic".to_owned(),
            config_hash: "sha256:deterministic".to_owned(),
            sandbox_backend: "none".to_owned(),
            os_toolchain: "deterministic".to_owned(),
            seed: None,
            provenance: Vec::new(),
            expected_outcome: None,
            expected_verification_verdict: None,
        }
    }

    /// Adds one RFC/slice provenance edge to this eval run.
    #[must_use]
    pub fn with_provenance(
        mut self,
        rfc_id: impl Into<String>,
        slice_id: impl Into<String>,
    ) -> Self {
        let provenance = EvalCaseProvenance::new(rfc_id, slice_id);
        if !self
            .provenance
            .iter()
            .any(|existing| existing == &provenance)
        {
            self.provenance.push(provenance);
        }
        self
    }

    /// Records the expected result bucket and verification verdict for matrix reporting.
    #[must_use]
    pub fn with_expected(
        mut self,
        outcome: EvalOutcomeKind,
        verification_verdict: VerificationVerdict,
    ) -> Self {
        self.expected_outcome = Some(outcome);
        self.expected_verification_verdict = Some(verification_verdict);
        self
    }
}

/// Structured result for one eval case run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct EvalResult {
    pub metadata: EvalRunMetadata,
    pub outcome: EvalOutcomeKind,
    pub run_status: RunStatus,
    pub verification_verdict: VerificationVerdict,
    pub visible_state: VisibleCompletionState,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_files: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<EvalToolCallSummary>,
    #[serde(default)]
    pub approval_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_log_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub durable_stream_cursor: Option<ProjectionCursor>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<EvalEvidenceRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_actions: Vec<EvalRequiredAction>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<EvalFailure>,
}

impl EvalResult {
    #[must_use]
    pub fn from_completion(
        metadata: EvalRunMetadata,
        run_status: RunStatus,
        verification_verdict: VerificationVerdict,
        failures: Vec<EvalFailure>,
    ) -> Self {
        let visible_state = VisibleCompletionState::derive(run_status, verification_verdict);
        let outcome = EvalOutcomeKind::from_completion(run_status, verification_verdict, &failures);
        Self {
            metadata,
            outcome,
            run_status,
            verification_verdict,
            visible_state,
            changed_files: Vec::new(),
            tool_calls: Vec::new(),
            approval_count: 0,
            session_log_path: None,
            durable_stream_cursor: None,
            evidence: Vec::new(),
            required_actions: Vec::new(),
            failures,
        }
    }
}

/// One retained artifact produced by an eval report.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct EvalReportArtifact {
    pub kind: String,
    pub path: PathBuf,
}

/// One JSONL record written by the deterministic eval report.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct EvalReportRecord {
    pub provider: String,
    pub model: String,
    pub config_hash: String,
    pub tool_schema_digest: String,
    pub deterministic: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rfc_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub slice_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_outcome: Option<EvalOutcomeKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_verification_verdict: Option<VerificationVerdict>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fixture_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failure_artifacts: Vec<EvalReportArtifact>,
    pub result: EvalResult,
}

/// One manifest row for active RFC regression reporting.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct EvalReportMatrixEntry {
    pub case_id: EvalCaseId,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rfc_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub slice_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_outcome: Option<EvalOutcomeKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_verification_verdict: Option<VerificationVerdict>,
    pub observed_outcome: EvalOutcomeKind,
    pub observed_verification_verdict: VerificationVerdict,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub durable_stream_cursor: Option<ProjectionCursor>,
}

/// Stable manifest for one deterministic eval report directory.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct EvalReportManifest {
    pub report_schema_version: u16,
    pub deterministic: bool,
    pub case_count: usize,
    pub required_case_count: usize,
    pub results_jsonl_path: PathBuf,
    pub summary_path: PathBuf,
    pub artifact_dir: PathBuf,
    pub outcome_counts: BTreeMap<String, usize>,
    pub verification_verdict_counts: BTreeMap<String, usize>,
    pub config_hashes: Vec<String>,
    pub tool_schema_digests: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rfc_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub slice_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub matrix: Vec<EvalReportMatrixEntry>,
}

/// Paths written by [`write_eval_report_artifacts`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct EvalReportArtifacts {
    pub results_jsonl_path: PathBuf,
    pub summary_path: PathBuf,
    pub manifest_path: PathBuf,
    pub artifact_dir: PathBuf,
}

/// Writes deterministic eval report artifacts without invoking a real model.
///
/// The report keeps structured JSONL as the machine-readable source and a small Markdown summary
/// for developer inspection. Session logs are retained for all non-verified outcomes.
pub fn write_eval_report_artifacts(
    output_dir: impl AsRef<Path>,
    results: &[EvalResult],
) -> Result<EvalReportArtifacts> {
    let output_dir = output_dir.as_ref();
    let artifact_dir = output_dir.join("artifacts");
    fs::create_dir_all(&artifact_dir)
        .with_context(|| format!("failed to create {}", artifact_dir.display()))?;

    let records = build_eval_report_records(results, &artifact_dir)?;
    let results_jsonl_path = output_dir.join("results.jsonl");
    let mut results_file = fs::File::create(&results_jsonl_path)
        .with_context(|| format!("failed to create {}", results_jsonl_path.display()))?;
    for record in &records {
        serde_json::to_writer(&mut results_file, record)
            .context("failed to serialize eval report record")?;
        results_file
            .write_all(b"\n")
            .context("failed to write eval report newline")?;
    }

    let summary_path = output_dir.join("summary.md");
    fs::write(&summary_path, render_eval_report_summary(&records))
        .with_context(|| format!("failed to write {}", summary_path.display()))?;

    let manifest_path = output_dir.join("manifest.json");
    let manifest = build_eval_report_manifest(
        &records,
        results_jsonl_path.clone(),
        summary_path.clone(),
        artifact_dir.clone(),
    );
    let manifest_file = fs::File::create(&manifest_path)
        .with_context(|| format!("failed to create {}", manifest_path.display()))?;
    serde_json::to_writer_pretty(manifest_file, &manifest)
        .context("failed to serialize eval report manifest")?;

    Ok(EvalReportArtifacts {
        results_jsonl_path,
        summary_path,
        manifest_path,
        artifact_dir,
    })
}

fn build_eval_report_records(
    results: &[EvalResult],
    artifact_dir: &Path,
) -> Result<Vec<EvalReportRecord>> {
    let mut records = Vec::new();
    for result in results {
        let fixture_path = result
            .session_log_path
            .as_ref()
            .and_then(|path| path.parent())
            .map(Path::to_path_buf);
        let mut failure_artifacts = Vec::new();
        if !matches!(
            result.outcome,
            EvalOutcomeKind::VerifiedSuccess | EvalOutcomeKind::Completed
        ) && let Some(session_log_path) = &result.session_log_path
        {
            let artifact_path = artifact_dir.join(format!(
                "{}-{}-session.jsonl",
                sanitize_path_component(&result.metadata.case_id),
                sanitize_path_component(&result.metadata.run_id)
            ));
            fs::copy(session_log_path, &artifact_path).with_context(|| {
                format!(
                    "failed to retain session log {}",
                    session_log_path.display()
                )
            })?;
            failure_artifacts.push(EvalReportArtifact {
                kind: "session_log".to_owned(),
                path: artifact_path,
            });
        }
        records.push(EvalReportRecord {
            provider: result.metadata.provider.clone(),
            model: result.metadata.model.clone(),
            config_hash: result.metadata.config_hash.clone(),
            tool_schema_digest: result.metadata.tool_schema_digest.clone(),
            deterministic: result.metadata.provider == "fake"
                && result.metadata.model == "deterministic",
            rfc_refs: rfc_refs(&result.metadata),
            slice_refs: slice_refs(&result.metadata),
            expected_outcome: result.metadata.expected_outcome,
            expected_verification_verdict: result.metadata.expected_verification_verdict,
            fixture_path,
            failure_artifacts,
            result: result.clone(),
        });
    }
    Ok(records)
}

fn build_eval_report_manifest(
    records: &[EvalReportRecord],
    results_jsonl_path: PathBuf,
    summary_path: PathBuf,
    artifact_dir: PathBuf,
) -> EvalReportManifest {
    let mut outcome_counts = BTreeMap::<String, usize>::new();
    let mut verification_verdict_counts = BTreeMap::<String, usize>::new();
    let mut config_hashes = Vec::<String>::new();
    let mut tool_schema_digests = Vec::<String>::new();
    let mut rfc_refs = Vec::<String>::new();
    let mut slice_refs = Vec::<String>::new();
    let mut matrix = Vec::<EvalReportMatrixEntry>::new();
    for record in records {
        *outcome_counts
            .entry(format!("{:?}", record.result.outcome))
            .or_default() += 1;
        *verification_verdict_counts
            .entry(format!("{:?}", record.result.verification_verdict))
            .or_default() += 1;
        push_unique(&mut config_hashes, record.config_hash.clone());
        push_unique(&mut tool_schema_digests, record.tool_schema_digest.clone());
        for rfc_ref in &record.rfc_refs {
            push_unique(&mut rfc_refs, rfc_ref.clone());
        }
        for slice_ref in &record.slice_refs {
            push_unique(&mut slice_refs, slice_ref.clone());
        }
        matrix.push(EvalReportMatrixEntry {
            case_id: record.result.metadata.case_id.clone(),
            rfc_refs: record.rfc_refs.clone(),
            slice_refs: record.slice_refs.clone(),
            expected_outcome: record.expected_outcome,
            expected_verification_verdict: record.expected_verification_verdict,
            observed_outcome: record.result.outcome,
            observed_verification_verdict: record.result.verification_verdict,
            durable_stream_cursor: record.result.durable_stream_cursor.clone(),
        });
    }
    EvalReportManifest {
        report_schema_version: 2,
        deterministic: records
            .iter()
            .all(|record| record.deterministic && record.provider == "fake"),
        case_count: records.len(),
        required_case_count: records.len(),
        results_jsonl_path,
        summary_path,
        artifact_dir,
        outcome_counts,
        verification_verdict_counts,
        config_hashes,
        tool_schema_digests,
        rfc_refs,
        slice_refs,
        matrix,
    }
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.iter().any(|existing| existing == &value) {
        values.push(value);
    }
}

fn rfc_refs(metadata: &EvalRunMetadata) -> Vec<String> {
    let mut values = Vec::new();
    for provenance in &metadata.provenance {
        push_unique(&mut values, provenance.rfc_id.clone());
    }
    values
}

fn slice_refs(metadata: &EvalRunMetadata) -> Vec<String> {
    let mut values = Vec::new();
    for provenance in &metadata.provenance {
        push_unique(&mut values, provenance.slice_id.clone());
    }
    values
}

fn render_eval_report_summary(records: &[EvalReportRecord]) -> String {
    let mut by_outcome = BTreeMap::<String, usize>::new();
    let mut by_verdict = BTreeMap::<String, usize>::new();
    for record in records {
        *by_outcome
            .entry(format!("{:?}", record.result.outcome))
            .or_default() += 1;
        *by_verdict
            .entry(format!("{:?}", record.result.verification_verdict))
            .or_default() += 1;
    }

    let mut summary = String::new();
    summary.push_str("# Sigil Deterministic Eval Report\n\n");
    summary.push_str(&format!("Total cases: {}\n\n", records.len()));
    summary.push_str("## Outcomes\n\n");
    for (outcome, count) in by_outcome {
        summary.push_str(&format!("- `{outcome}`: {count}\n"));
    }
    summary.push_str("\n## Verification Verdicts\n\n");
    for (verdict, count) in by_verdict {
        summary.push_str(&format!("- `{verdict}`: {count}\n"));
    }
    summary.push_str("\n## Cases\n\n");
    for record in records {
        summary.push_str(&format!(
            "- `{}`: outcome=`{:?}`, run=`{:?}`, verification=`{:?}`, visible=`{:?}`, provider=`{}`, model=`{}`\n",
            record.result.metadata.case_id,
            record.result.outcome,
            record.result.run_status,
            record.result.verification_verdict,
            record.result.visible_state,
            record.provider,
            record.model
        ));
        if !record.rfc_refs.is_empty() || !record.slice_refs.is_empty() {
            summary.push_str(&format!(
                "  - provenance: rfc=`{}`, slice=`{}`\n",
                record.rfc_refs.join(","),
                record.slice_refs.join(",")
            ));
        }
        if record.expected_outcome.is_some() || record.expected_verification_verdict.is_some() {
            summary.push_str(&format!(
                "  - expected: outcome=`{}`, verification=`{}`\n",
                record
                    .expected_outcome
                    .map(|outcome| format!("{outcome:?}"))
                    .unwrap_or_else(|| "unspecified".to_owned()),
                record
                    .expected_verification_verdict
                    .map(|verdict| format!("{verdict:?}"))
                    .unwrap_or_else(|| "unspecified".to_owned())
            ));
        }
        if let Some(cursor) = &record.result.durable_stream_cursor {
            summary.push_str(&format!(
                "  - evidence cursor: session=`{}`, sequence=`{}`, event=`{}`\n",
                cursor.session_id,
                cursor.last_applied_stream_sequence,
                cursor.last_applied_event_id
            ));
        }
        if !record.failure_artifacts.is_empty() {
            for artifact in &record.failure_artifacts {
                summary.push_str(&format!(
                    "  - artifact `{}`: `{}`\n",
                    artifact.kind,
                    artifact.path.display()
                ));
            }
        }
    }
    summary
}

/// One deterministic eval case runnable without a real provider or network.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct EvalCase {
    pub metadata: EvalRunMetadata,
    pub prompt: String,
    pub fixture: EvalWorkspaceFixture,
    pub script: EvalProviderScript,
    #[serde(default = "default_eval_workspace_trust")]
    pub workspace_trust: WorkspaceTrust,
}

impl EvalCase {
    /// Creates a deterministic eval case from explicit fixture and provider script material.
    #[must_use]
    pub fn deterministic(
        case_id: impl Into<String>,
        prompt: impl Into<String>,
        fixture: EvalWorkspaceFixture,
        script: EvalProviderScript,
    ) -> Self {
        let case_id = case_id.into();
        let fixture_id = fixture.fixture_id.clone();
        let run_id = format!("{case_id}-run");
        Self {
            metadata: EvalRunMetadata::deterministic(case_id, run_id, fixture_id),
            prompt: prompt.into(),
            fixture,
            script,
            workspace_trust: WorkspaceTrust::Unknown,
        }
    }

    /// Overrides the workspace trust state for this deterministic eval case.
    #[must_use]
    pub fn with_workspace_trust(mut self, workspace_trust: WorkspaceTrust) -> Self {
        self.workspace_trust = workspace_trust;
        self
    }
}

fn default_eval_workspace_trust() -> WorkspaceTrust {
    WorkspaceTrust::Unknown
}

/// In-memory file fixture materialized into a temporary eval workspace.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct EvalWorkspaceFixture {
    pub fixture_id: EvalFixtureId,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub files: BTreeMap<PathBuf, String>,
}

impl EvalWorkspaceFixture {
    /// Creates an empty fixture with a stable id.
    #[must_use]
    pub fn new(fixture_id: impl Into<String>) -> Self {
        Self {
            fixture_id: fixture_id.into(),
            files: BTreeMap::new(),
        }
    }

    /// Adds one workspace-relative text file to the fixture.
    #[must_use]
    pub fn with_file(mut self, path: impl Into<PathBuf>, content: impl Into<String>) -> Self {
        self.files.insert(path.into(), content.into());
        self
    }
}

/// Explicit fake-provider turn script consumed by [`EvalCaseRunner`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct EvalProviderScript {
    pub steps: Vec<EvalProviderStep>,
}

impl EvalProviderScript {
    /// Creates a script from explicit provider steps.
    #[must_use]
    pub fn new(steps: Vec<EvalProviderStep>) -> Self {
        Self { steps }
    }
}

/// One fake-provider behavior step.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvalProviderStep {
    AssistantText {
        text: String,
    },
    ToolCall {
        tool_call_id: EvalToolCallId,
        tool_name: String,
        #[serde(default)]
        args_json: String,
    },
    ToolResultContinuation {
        tool_call_id: EvalToolCallId,
        text: String,
    },
    FinalAnswer {
        text: String,
    },
    ProviderError {
        message: String,
    },
}

/// Deterministic fake tool registry used by eval cases.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct EvalFakeToolRegistry {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tools: BTreeMap<String, EvalFakeToolAction>,
}

impl EvalFakeToolRegistry {
    /// Creates an empty fake tool registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers one fake tool action.
    #[must_use]
    pub fn with_tool(mut self, name: impl Into<String>, action: EvalFakeToolAction) -> Self {
        self.tools.insert(name.into(), action);
        self
    }

    fn action_for(&self, name: &str) -> EvalFakeToolAction {
        self.tools
            .get(name)
            .cloned()
            .unwrap_or_else(|| EvalFakeToolAction::ToolError {
                message: format!("fake tool {name} is not registered"),
            })
    }
}

/// Deterministic result produced by one fake tool invocation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvalFakeToolAction {
    ReadOnlySuccess {
        output: String,
    },
    ControlledWriteSuccess {
        path: PathBuf,
        content: String,
    },
    CheckSuccess {
        check_id: String,
    },
    CheckFailure {
        check_id: String,
        message: String,
    },
    CheckMutatingSuccess {
        check_id: String,
        path: PathBuf,
        content: String,
    },
    DiscoverRepoCheckCandidate {
        check_id: String,
        source_path: PathBuf,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        instruction_path: Option<PathBuf>,
    },
    PromoteRepoCheck {
        check_id: String,
        promotion: EvalRepoCheckPromotion,
    },
    RepoCheckSuccess {
        check_id: String,
    },
    PermissionDenied {
        message: String,
    },
    ToolError {
        message: String,
    },
    UnknownMutationMarker {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<PathBuf>,
    },
}

/// Deterministic promotion provenance for a repo-local eval check candidate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum EvalRepoCheckPromotion {
    UserApproved { approval_event_id: EventId },
    Sandboxed { sandbox_decision_id: EventId },
}

impl EvalRepoCheckPromotion {
    fn evidence_id(&self) -> &'static str {
        match self {
            Self::UserApproved { .. } => "user_approved",
            Self::Sandboxed { .. } => "sandboxed",
        }
    }

    fn evidence_payload(&self) -> serde_json::Value {
        match self {
            Self::UserApproved { approval_event_id } => {
                json!({ "kind": "user_approved", "approval_event_id": approval_event_id })
            }
            Self::Sandboxed {
                sandbox_decision_id,
            } => json!({ "kind": "sandboxed", "sandbox_decision_id": sandbox_decision_id }),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
struct EvalRepoCheckCandidateState {
    check_id: String,
    source_path: PathBuf,
    instruction_path: Option<PathBuf>,
}

/// Options for deterministic eval execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct EvalCaseRunnerOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_root: Option<PathBuf>,
}

impl EvalCaseRunnerOptions {
    /// Uses a process tempdir path derived from the eval run id.
    #[must_use]
    pub fn temp_workspace() -> Self {
        Self {
            workspace_root: None,
        }
    }

    /// Uses a caller-provided workspace path.
    #[must_use]
    pub fn with_workspace_root(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_root: Some(workspace_root.into()),
        }
    }
}

impl Default for EvalCaseRunnerOptions {
    fn default() -> Self {
        Self::temp_workspace()
    }
}

/// In-memory deterministic eval runner skeleton.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct EvalCaseRunner {
    pub tools: EvalFakeToolRegistry,
    pub options: EvalCaseRunnerOptions,
}

impl EvalCaseRunner {
    /// Creates a runner from a fake tool registry.
    #[must_use]
    pub fn new(tools: EvalFakeToolRegistry) -> Self {
        Self {
            tools,
            options: EvalCaseRunnerOptions::default(),
        }
    }

    /// Overrides runner options.
    #[must_use]
    pub fn with_options(mut self, options: EvalCaseRunnerOptions) -> Self {
        self.options = options;
        self
    }

    /// Runs one deterministic eval case and returns a structured result.
    ///
    /// # Errors
    ///
    /// Returns an error only for harness infrastructure failures such as workspace or session-log
    /// I/O. Provider and tool failures from the script are represented inside [`EvalResult`].
    pub fn run(&self, case: EvalCase) -> Result<EvalResult> {
        let workspace_root = self.workspace_root_for(&case.metadata.run_id);
        prepare_workspace(&workspace_root, &case.fixture)?;
        let session_log_path = workspace_root.join("session.jsonl");
        let mut capture = EvalEventCapture::new(&session_log_path)?;
        capture.record(
            "case_started",
            json!({
                "case_id": &case.metadata.case_id,
                "fixture_id": &case.metadata.fixture_id,
                "prompt": &case.prompt,
            }),
        )?;

        let mut run_status = RunStatus::Running;
        let mut verification_verdict = VerificationVerdict::NotApplicable;
        let mut changed_files = Vec::new();
        let mut tool_calls = Vec::new();
        let mut failures = Vec::new();
        let mut required_actions = Vec::new();
        let mut repo_check_candidates = BTreeMap::<String, EvalRepoCheckCandidateState>::new();
        let mut promoted_repo_checks = BTreeMap::<String, EvalRepoCheckPromotion>::new();

        for step in &case.script.steps {
            match step {
                EvalProviderStep::AssistantText { text } => {
                    capture.append_assistant(text)?;
                }
                EvalProviderStep::ToolResultContinuation { tool_call_id, text } => {
                    capture.record(
                        "tool_result_continuation",
                        json!({ "tool_call_id": tool_call_id, "text": text }),
                    )?;
                }
                EvalProviderStep::ToolCall {
                    tool_call_id,
                    tool_name,
                    args_json,
                } => {
                    capture.record(
                        "tool_call_started",
                        json!({
                            "tool_call_id": tool_call_id,
                            "tool_name": tool_name,
                            "args_json": args_json,
                        }),
                    )?;
                    let action = self.tools.action_for(tool_name);
                    let mut fake_tool_state = EvalFakeToolState {
                        changed_files: &mut changed_files,
                        tool_calls: &mut tool_calls,
                        failures: &mut failures,
                        verification_verdict: &mut verification_verdict,
                        repo_check_candidates: &mut repo_check_candidates,
                        promoted_repo_checks: &mut promoted_repo_checks,
                        required_actions: &mut required_actions,
                    };
                    apply_fake_tool_action(
                        &workspace_root,
                        tool_call_id,
                        tool_name,
                        action,
                        case.workspace_trust,
                        &mut fake_tool_state,
                        &mut capture,
                    )?;
                    if let Some(terminal) = terminal_status_from_failures(&failures) {
                        run_status = terminal;
                        break;
                    }
                }
                EvalProviderStep::FinalAnswer { text } => {
                    capture.record("final_answer", json!({ "text": text }))?;
                    run_status = RunStatus::Completed;
                    break;
                }
                EvalProviderStep::ProviderError { message } => {
                    failures.push(EvalFailure::new(EvalFailureKind::Model, message.clone()));
                    capture.record("provider_error", json!({ "message": message }))?;
                    run_status = RunStatus::Failed;
                    break;
                }
            }
        }

        if run_status == RunStatus::Running {
            failures.push(EvalFailure::new(
                EvalFailureKind::Harness,
                "fake provider script ended without final answer",
            ));
            run_status = RunStatus::Interrupted;
        }
        if !changed_files.is_empty()
            && matches!(
                verification_verdict,
                VerificationVerdict::NotApplicable | VerificationVerdict::NotEvaluated
            )
        {
            verification_verdict = VerificationVerdict::Missing;
        }
        if !changed_files.is_empty()
            && matches!(
                verification_verdict,
                VerificationVerdict::Missing
                    | VerificationVerdict::Inconclusive
                    | VerificationVerdict::Stale
            )
        {
            required_actions.push(EvalRequiredAction::new(
                EvalRequiredActionKind::RunCheck,
                "run a verification check for the current workspace snapshot",
            ));
        }

        let mut result =
            EvalResult::from_completion(case.metadata, run_status, verification_verdict, failures);
        result.changed_files = changed_files;
        result.tool_calls = tool_calls;
        result.session_log_path = Some(session_log_path);
        result.durable_stream_cursor = capture.cursor;
        result.evidence = capture.evidence;
        result.required_actions = required_actions;
        Ok(result)
    }

    fn workspace_root_for(&self, run_id: &str) -> PathBuf {
        self.options.workspace_root.clone().unwrap_or_else(|| {
            std::env::temp_dir()
                .join("sigil-eval")
                .join(sanitize_path_component(run_id))
        })
    }
}

fn prepare_workspace(root: &Path, fixture: &EvalWorkspaceFixture) -> Result<()> {
    if root.exists() {
        fs::remove_dir_all(root)
            .with_context(|| format!("failed to reset eval workspace {}", root.display()))?;
    }
    fs::create_dir_all(root)
        .with_context(|| format!("failed to create eval workspace {}", root.display()))?;
    for (path, content) in &fixture.files {
        let full_path = root.join(path);
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        fs::write(&full_path, content)
            .with_context(|| format!("failed to write fixture file {}", full_path.display()))?;
    }
    Ok(())
}

struct EvalFakeToolState<'a> {
    changed_files: &'a mut Vec<PathBuf>,
    tool_calls: &'a mut Vec<EvalToolCallSummary>,
    failures: &'a mut Vec<EvalFailure>,
    verification_verdict: &'a mut VerificationVerdict,
    repo_check_candidates: &'a mut BTreeMap<String, EvalRepoCheckCandidateState>,
    promoted_repo_checks: &'a mut BTreeMap<String, EvalRepoCheckPromotion>,
    required_actions: &'a mut Vec<EvalRequiredAction>,
}

fn apply_fake_tool_action(
    workspace_root: &Path,
    tool_call_id: &str,
    tool_name: &str,
    action: EvalFakeToolAction,
    workspace_trust: WorkspaceTrust,
    state: &mut EvalFakeToolState<'_>,
    capture: &mut EvalEventCapture,
) -> Result<()> {
    let changed_files = &mut *state.changed_files;
    let tool_calls = &mut *state.tool_calls;
    let failures = &mut *state.failures;
    let verification_verdict = &mut *state.verification_verdict;
    let repo_check_candidates = &mut *state.repo_check_candidates;
    let promoted_repo_checks = &mut *state.promoted_repo_checks;
    let required_actions = &mut *state.required_actions;
    match action {
        EvalFakeToolAction::ReadOnlySuccess { output } => {
            tool_calls.push(tool_summary(
                tool_call_id,
                tool_name,
                EvalToolCallStatus::Succeeded,
            ));
            capture.record(
                "tool_result",
                json!({ "tool_call_id": tool_call_id, "status": "succeeded", "output": output }),
            )?;
        }
        EvalFakeToolAction::ControlledWriteSuccess { path, content } => {
            let full_path = workspace_root.join(&path);
            if let Some(parent) = full_path.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create {}", parent.display()))?;
            }
            fs::write(&full_path, content)
                .with_context(|| format!("failed to write {}", full_path.display()))?;
            changed_files.push(path.clone());
            tool_calls.push(tool_summary(
                tool_call_id,
                tool_name,
                EvalToolCallStatus::Succeeded,
            ));
            let write_evidence = capture.record(
                "controlled_write",
                json!({
                    "tool_call_id": tool_call_id,
                    "path": &path,
                }),
            )?;
            if *verification_verdict == VerificationVerdict::Passed {
                *verification_verdict = VerificationVerdict::Stale;
                let mut failure = EvalFailure::new(
                    EvalFailureKind::VerificationStale,
                    "controlled write invalidated the previous verification receipt",
                );
                failure.evidence.push(write_evidence);
                failures.push(failure);
            } else {
                *verification_verdict = VerificationVerdict::Missing;
            }
            capture.record(
                "tool_result",
                json!({
                    "tool_call_id": tool_call_id,
                    "status": "succeeded",
                    "changed_file": &path,
                }),
            )?;
        }
        EvalFakeToolAction::CheckSuccess { check_id } => {
            *verification_verdict = VerificationVerdict::Passed;
            tool_calls.push(tool_summary(
                tool_call_id,
                tool_name,
                EvalToolCallStatus::Succeeded,
            ));
            capture.record(
                "tool_result",
                json!({
                    "tool_call_id": tool_call_id,
                    "status": "succeeded",
                    "check_id": check_id,
                    "workspace_snapshot_id": format!("snapshot-{}", changed_files.len()),
                }),
            )?;
        }
        EvalFakeToolAction::CheckFailure { check_id, message } => {
            *verification_verdict = VerificationVerdict::Failed;
            tool_calls.push(tool_summary(
                tool_call_id,
                tool_name,
                EvalToolCallStatus::Failed,
            ));
            failures.push(EvalFailure::new(
                EvalFailureKind::VerificationFailed,
                message.clone(),
            ));
            capture.record(
                "tool_result",
                json!({
                    "tool_call_id": tool_call_id,
                    "status": "failed",
                    "check_id": check_id,
                    "message": message,
                }),
            )?;
        }
        EvalFakeToolAction::CheckMutatingSuccess {
            check_id,
            path,
            content,
        } => {
            let full_path = workspace_root.join(&path);
            if let Some(parent) = full_path.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create {}", parent.display()))?;
            }
            fs::write(&full_path, content)
                .with_context(|| format!("failed to write {}", full_path.display()))?;
            changed_files.push(path.clone());
            tool_calls.push(tool_summary(
                tool_call_id,
                tool_name,
                EvalToolCallStatus::Succeeded,
            ));
            let mutation_evidence = capture.record(
                "mutating_check",
                json!({
                    "tool_call_id": tool_call_id,
                    "check_id": check_id,
                    "path": &path,
                }),
            )?;
            let (kind, message, verdict) = if *verification_verdict == VerificationVerdict::Passed {
                (
                    EvalFailureKind::VerificationStale,
                    "mutating check invalidated the previous verification receipt",
                    VerificationVerdict::Stale,
                )
            } else {
                (
                    EvalFailureKind::VerificationInconclusive,
                    "check modified verification scope and cannot prove final state",
                    VerificationVerdict::Inconclusive,
                )
            };
            *verification_verdict = verdict;
            let mut failure = EvalFailure::new(kind, message);
            failure.evidence.push(mutation_evidence);
            failures.push(failure);
            capture.record(
                "tool_result",
                json!({
                    "tool_call_id": tool_call_id,
                    "status": "inconclusive",
                    "check_id": check_id,
                    "mutated_file": &path,
                    "workspace_snapshot_id": format!("snapshot-{}", changed_files.len()),
                }),
            )?;
        }
        EvalFakeToolAction::DiscoverRepoCheckCandidate {
            check_id,
            source_path,
            instruction_path,
        } => {
            repo_check_candidates.insert(
                check_id.clone(),
                EvalRepoCheckCandidateState {
                    check_id: check_id.clone(),
                    source_path: source_path.clone(),
                    instruction_path: instruction_path.clone(),
                },
            );
            tool_calls.push(tool_summary(
                tool_call_id,
                tool_name,
                EvalToolCallStatus::Succeeded,
            ));
            capture.record(
                "repo_check_candidate_discovered",
                json!({
                    "tool_call_id": tool_call_id,
                    "check_id": check_id,
                    "source_path": source_path,
                    "instruction_path": instruction_path,
                    "workspace_trust": workspace_trust,
                    "instruction_trust": if workspace_trust == WorkspaceTrust::Trusted {
                        "workspace_instruction"
                    } else {
                        "untrusted_repository_data"
                    },
                }),
            )?;
        }
        EvalFakeToolAction::PromoteRepoCheck {
            check_id,
            promotion,
        } => {
            tool_calls.push(tool_summary(
                tool_call_id,
                tool_name,
                EvalToolCallStatus::Succeeded,
            ));
            let Some(candidate) = repo_check_candidates.get(&check_id) else {
                tool_calls.pop();
                tool_calls.push(tool_summary(
                    tool_call_id,
                    tool_name,
                    EvalToolCallStatus::Failed,
                ));
                failures.push(EvalFailure::new(
                    EvalFailureKind::Harness,
                    format!("repo check candidate {check_id} was not discovered"),
                ));
                capture.record(
                    "repo_check_promotion_failed",
                    json!({
                        "tool_call_id": tool_call_id,
                        "check_id": check_id,
                        "reason": "candidate_not_discovered",
                    }),
                )?;
                return Ok(());
            };
            promoted_repo_checks.insert(check_id.clone(), promotion.clone());
            capture.record(
                "repo_check_promoted",
                json!({
                    "tool_call_id": tool_call_id,
                    "check_id": check_id,
                    "source_path": &candidate.source_path,
                    "instruction_path": &candidate.instruction_path,
                    "workspace_trust": workspace_trust,
                    "promotion": promotion.evidence_payload(),
                    "promotion_id": promotion.evidence_id(),
                }),
            )?;
        }
        EvalFakeToolAction::RepoCheckSuccess { check_id } => {
            let Some(promotion) = promoted_repo_checks.get(&check_id) else {
                tool_calls.push(tool_summary(
                    tool_call_id,
                    tool_name,
                    EvalToolCallStatus::Denied,
                ));
                failures.push(EvalFailure::new(
                    EvalFailureKind::PermissionDenied,
                    format!(
                        "repo-local check {check_id} requires explicit approval or sandbox promotion"
                    ),
                ));
                required_actions.push(EvalRequiredAction::new(
                    EvalRequiredActionKind::ApproveWorkspace,
                    "approve workspace or run the repo-local check in an allowed sandbox",
                ));
                capture.record(
                    "repo_check_execution_blocked",
                    json!({
                        "tool_call_id": tool_call_id,
                        "check_id": check_id,
                        "workspace_trust": workspace_trust,
                        "reason": "missing_approval_or_sandbox_promotion",
                    }),
                )?;
                return Ok(());
            };
            *verification_verdict = VerificationVerdict::Passed;
            tool_calls.push(tool_summary(
                tool_call_id,
                tool_name,
                EvalToolCallStatus::Succeeded,
            ));
            capture.record(
                "repo_check_executed",
                json!({
                    "tool_call_id": tool_call_id,
                    "status": "succeeded",
                    "check_id": check_id,
                    "workspace_snapshot_id": format!("snapshot-{}", changed_files.len()),
                    "promotion": promotion.evidence_payload(),
                }),
            )?;
        }
        EvalFakeToolAction::PermissionDenied { message } => {
            tool_calls.push(tool_summary(
                tool_call_id,
                tool_name,
                EvalToolCallStatus::Denied,
            ));
            failures.push(EvalFailure::new(
                EvalFailureKind::PermissionDenied,
                message.clone(),
            ));
            capture.record(
                "tool_result",
                json!({
                    "tool_call_id": tool_call_id,
                    "status": "denied",
                    "message": message,
                }),
            )?;
        }
        EvalFakeToolAction::ToolError { message } => {
            tool_calls.push(tool_summary(
                tool_call_id,
                tool_name,
                EvalToolCallStatus::Failed,
            ));
            failures.push(EvalFailure::new(EvalFailureKind::Tool, message.clone()));
            capture.record(
                "tool_result",
                json!({
                    "tool_call_id": tool_call_id,
                    "status": "failed",
                    "message": message,
                }),
            )?;
        }
        EvalFakeToolAction::UnknownMutationMarker { path } => {
            if let Some(path) = path {
                changed_files.push(path.clone());
            }
            *verification_verdict = VerificationVerdict::Stale;
            tool_calls.push(tool_summary(
                tool_call_id,
                tool_name,
                EvalToolCallStatus::Succeeded,
            ));
            failures.push(EvalFailure::new(
                EvalFailureKind::VerificationStale,
                "unknown workspace mutation invalidated verification",
            ));
            capture.record(
                "tool_result",
                json!({
                    "tool_call_id": tool_call_id,
                    "status": "succeeded",
                    "unknown_mutation": true,
                }),
            )?;
        }
    }
    Ok(())
}

fn tool_summary(
    tool_call_id: &str,
    tool_name: &str,
    status: EvalToolCallStatus,
) -> EvalToolCallSummary {
    EvalToolCallSummary {
        tool_call_id: tool_call_id.to_owned(),
        tool_name: tool_name.to_owned(),
        status,
    }
}

fn terminal_status_from_failures(failures: &[EvalFailure]) -> Option<RunStatus> {
    failures.last().and_then(|failure| match failure.kind {
        EvalFailureKind::PermissionDenied => Some(RunStatus::Blocked),
        EvalFailureKind::Tool | EvalFailureKind::VerificationFailed => Some(RunStatus::Failed),
        _ => None,
    })
}

struct EvalEventCapture {
    store: JsonlSessionStore,
    evidence: Vec<EvalEvidenceRef>,
    cursor: Option<ProjectionCursor>,
}

impl EvalEventCapture {
    fn new(session_log_path: &Path) -> Result<Self> {
        Ok(Self {
            store: JsonlSessionStore::new(session_log_path)?,
            evidence: Vec::new(),
            cursor: None,
        })
    }

    fn append_assistant(&mut self, text: &str) -> Result<()> {
        self.record("assistant_text", json!({ "text": text }))
            .map(|_| ())
    }

    fn record(&mut self, label: &str, payload: serde_json::Value) -> Result<EvalEvidenceRef> {
        let event = self
            .store
            .append_session_entry_event(&SessionLogEntry::Control(ControlEntry::Note {
                kind: "eval_harness".to_owned(),
                data: json!({
                    "label": label,
                    "payload": payload,
                }),
            }))?;
        Ok(self.record_event(label, event))
    }

    fn record_event(&mut self, label: &str, event: crate::StoredEvent) -> EvalEvidenceRef {
        self.cursor = Some(ProjectionCursor {
            session_id: event.session_id.clone(),
            projection_schema_version: SESSION_ENTRY_PROJECTION_SCHEMA_VERSION,
            last_applied_stream_sequence: event.stream_sequence,
            last_applied_event_id: event.event_id.clone(),
            last_applied_record_checksum: event.record_checksum.clone(),
        });
        let evidence = EvalEvidenceRef::durable_event(label, event.event_id);
        self.evidence.push(evidence.clone());
        evidence
    }
}

fn sanitize_path_component(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    if sanitized.is_empty() {
        "run".to_owned()
    } else {
        sanitized
    }
}
