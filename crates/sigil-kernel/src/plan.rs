use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    ConversationRouteDecisionId, ConversationTurnRef, IntentPlanProposalV1, IntentProposalUnitV1,
    PlanReviewId, PlanReviewProjection,
    session::{ControlEntry, SessionLogEntry},
    task::{AgentRole, TaskCapabilityV2, TaskId, TaskIsolationMode, TaskStepMode},
    tool::{ToolAccess, ToolCategory, ToolPreviewCapability, ToolSpec},
    verification::{CheckCommand, ToolEffect},
};

/// Stable digest prefix used for approved plan text.
pub const PLAN_HASH_PREFIX: &str = "sha256:";
/// Version of the provider-neutral Plan review result envelope.
pub const PLAN_REVIEW_RESULT_SCHEMA_VERSION: u32 = 1;
/// Model-visible tool name for the small Plan review result envelope.
pub const PLAN_REVIEW_RESULT_TOOL_NAME: &str = "submit_plan_review_result";
/// Durable schema for a complete or incomplete plain-text Plan candidate captured before result
/// classification. Candidate records are evidence only; they never imply DraftReady or Run.
pub const PLAN_REVIEW_CANDIDATE_SCHEMA_VERSION: u16 = 1;
/// Durable schema for a typed classification of one preserved Plan candidate.
pub const PLAN_REVIEW_RESOLUTION_SCHEMA_VERSION: u16 = 1;
/// Maximum UTF-8 preview retained inline for a candidate whose complete body lives in managed
/// artifact storage.
pub const PLAN_REVIEW_CANDIDATE_PREVIEW_MAX_BYTES: usize = 64 * 1024;
const PLAN_INLINE_TEXT_MAX_BYTES: usize = 64 * 1024;
const PLAN_SUMMARY_MAX_BYTES: usize = 2 * 1024;
const PLAN_REVIEW_RESULT_ACTUAL_MAX_CHARS: usize = 256;

/// Stable identifier for one durable plan artifact.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct PlanId(String);

impl PlanId {
    /// Creates a plan identifier safe for durable state and relative references.
    ///
    /// # Errors
    ///
    /// Returns an error when `value` is empty or contains path separators or unstable characters.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        validate_plan_stable_id("plan id", &value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Source reference for a durable plan artifact.
///
/// Plan review lifecycles additionally bind the exact source turn, route decision, and plan
/// review identity so a pending plan can be restored and audited without guessing provenance.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct PlanSourceRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_turn: Option<ConversationTurnRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_decision_id: Option<ConversationRouteDecisionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_review_id: Option<PlanReviewId>,
}

/// One model-suggested verification check extracted from a plan.
///
/// These checks are candidates only. They must not become required verification checks unless the
/// normal RFC-0003 policy, user confirmation, or trusted configuration promotes them.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct PlanSuggestedCheck {
    pub check_spec_id: String,
    pub command: CheckCommand,
    pub effect: ToolEffect,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_line: Option<String>,
}

/// One structured executable step produced by `/plan`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct PlanDraftStep {
    pub step_id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<AgentRole>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    /// Provider-local aliases resolved only after explicit plan acceptance.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub intent_aliases: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<TaskStepMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolation: Option<TaskIsolationMode>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub target_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_capabilities: Vec<TaskCapabilityV2>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deliverables: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub acceptance_criteria: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggested_checks: Vec<PlanSuggestedCheck>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// Append-only record created when `/plan` produces a durable artifact.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct PlanDraftCreatedEntry {
    pub plan_id: PlanId,
    pub schema_version: u32,
    pub source: PlanSourceRef,
    pub plan_hash: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline_text: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<PlanDraftStep>,
    /// Unaccepted, digest-bound provider suggestion carried by the durable plan artifact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent_proposal: Option<IntentPlanProposalV1>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub target_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggested_checks: Vec<PlanSuggestedCheck>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_snapshot_id: Option<String>,
    pub created_at_ms: u64,
}

/// The only semantic outcomes a Plan review result may claim.
///
/// This enum deliberately contains no execution, task, permission, or intent authority. Those
/// decisions remain host-owned and are made only after a user reviews a durable Plan draft.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanReviewResultOutcome {
    Draft,
    NoPlan,
}

impl PlanReviewResultOutcome {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::NoPlan => "no_plan",
        }
    }
}

/// A validated, provider-neutral result from a Plan review.
///
/// `Draft` owns the exact policy-safe text that is persisted and hashed. `NoPlan` owns the
/// bounded policy-safe explanation and intentionally carries no Plan identity or draft artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanReviewResult {
    Draft(Box<PlanDraftCreatedEntry>),
    NoPlan { reason: String },
}

/// Completeness proof attached to a preserved Plan candidate.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanReviewCandidateCompletenessV1 {
    Complete,
    Partial,
    Unknown,
}

/// Actor that supplied a Plan review candidate classification.
///
/// The actor is deliberately limited to the two paths that can safely resolve a preserved
/// candidate: a model confirmation receipt or an explicit user adoption command.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanReviewResolutionActorV1 {
    Model,
    User,
}

/// Append-only proof that one exact preserved candidate was classified as a draft or no-plan.
///
/// This record is evidence only. A `draft` outcome becomes `DraftReady` only when the enclosing
/// terminal bundle also contains the corresponding `PlanDraftCreated` and attempt transition.
/// `receipt_id` is a host-owned model result receipt or user command identity; it is never parsed
/// as an instruction or used as execution authority.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct PlanReviewResolutionRecordedV1 {
    pub schema_version: u16,
    pub plan_review_id: PlanReviewId,
    pub attempt_id: crate::PlanReviewAttemptId,
    pub candidate_hash: String,
    pub outcome: PlanReviewResultOutcome,
    pub actor: PlanReviewResolutionActorV1,
    pub receipt_id: String,
    pub recorded_at_ms: u64,
}

impl PlanReviewResolutionRecordedV1 {
    /// Validates the bounded, hash-bound resolution envelope before it enters the session log.
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != PLAN_REVIEW_RESOLUTION_SCHEMA_VERSION {
            bail!(
                "unsupported plan review resolution schema version {}",
                self.schema_version
            );
        }
        if self.candidate_hash.is_empty()
            || self.candidate_hash.len() > 256
            || crate::safe_persistence_text(&self.candidate_hash) != self.candidate_hash
        {
            bail!("plan review resolution candidate hash is not bounded safe text");
        }
        if self.receipt_id.is_empty()
            || self.receipt_id.len() > 512
            || crate::safe_persistence_text(&self.receipt_id) != self.receipt_id
        {
            bail!("plan review resolution receipt id is not bounded safe text");
        }
        if self.recorded_at_ms == 0 {
            bail!("plan review resolution timestamp must be non-zero");
        }
        Ok(())
    }
}

/// Immutable, attempt-bound candidate text captured from a research or finalizer response.
///
/// Small bodies are retained inline. Larger bodies carry a bounded display preview plus a
/// same-scope managed artifact descriptor; the durable hash always binds the complete body.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct PlanReviewCandidateRecordedV1 {
    pub schema_version: u16,
    pub plan_review_id: PlanReviewId,
    pub attempt_id: crate::PlanReviewAttemptId,
    pub plan_id: PlanId,
    pub source: PlanSourceRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_event_id: Option<String>,
    pub content_hash: String,
    pub content: String,
    /// Optional managed artifact carrying the complete body when the inline preview is too large.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_artifact: Option<crate::session::ToolArtifactDescriptorV1>,
    pub completeness: PlanReviewCandidateCompletenessV1,
    pub recorded_at_ms: u64,
}

/// Captures a safe, bounded candidate body under the exact review attempt lineage.
pub fn plan_review_candidate_recorded_entry(
    plan_review_id: PlanReviewId,
    attempt_id: crate::PlanReviewAttemptId,
    plan_id: PlanId,
    source: PlanSourceRef,
    source_event_id: Option<String>,
    content: &str,
    completeness: PlanReviewCandidateCompletenessV1,
    recorded_at_ms: u64,
) -> Result<PlanReviewCandidateRecordedV1> {
    let content = crate::safe_persistence_text(content.trim());
    if content.is_empty() {
        bail!("plan review candidate content must not be empty");
    }
    if content.len() > PLAN_INLINE_TEXT_MAX_BYTES {
        bail!("plan review candidate exceeds the {PLAN_INLINE_TEXT_MAX_BYTES}-byte inline limit");
    }
    Ok(PlanReviewCandidateRecordedV1 {
        schema_version: PLAN_REVIEW_CANDIDATE_SCHEMA_VERSION,
        plan_review_id,
        attempt_id,
        plan_id,
        source,
        source_event_id,
        content_hash: plan_text_hash(&content),
        content,
        content_artifact: None,
        completeness,
        recorded_at_ms,
    })
}

/// Creates an attempt-bound candidate record whose complete body is stored in a managed artifact.
/// The inline content is a bounded UTF-8 preview for product display; adoption must resolve the
/// artifact and verify that its hash matches `content_hash` before creating a Plan draft.
pub fn plan_review_candidate_recorded_entry_with_artifact(
    plan_review_id: PlanReviewId,
    attempt_id: crate::PlanReviewAttemptId,
    plan_id: PlanId,
    source: PlanSourceRef,
    source_event_id: Option<String>,
    preview: &str,
    artifact: crate::session::ToolArtifactDescriptorV1,
    completeness: PlanReviewCandidateCompletenessV1,
    recorded_at_ms: u64,
) -> Result<PlanReviewCandidateRecordedV1> {
    artifact.validate()?;
    if artifact.encoding != crate::session::ToolArtifactEncoding::Utf8 {
        bail!("plan review candidate artifact must be UTF-8");
    }
    let content = crate::safe_persistence_text(preview.trim());
    if content.is_empty() {
        bail!("plan review candidate preview must not be empty");
    }
    if content.len() > PLAN_REVIEW_CANDIDATE_PREVIEW_MAX_BYTES {
        bail!("plan review candidate preview exceeds the inline limit");
    }
    Ok(PlanReviewCandidateRecordedV1 {
        schema_version: PLAN_REVIEW_CANDIDATE_SCHEMA_VERSION,
        plan_review_id,
        attempt_id: attempt_id.clone(),
        plan_id,
        source,
        source_event_id,
        content_hash: artifact.content_sha256.clone(),
        content,
        content_artifact: Some(artifact),
        completeness,
        recorded_at_ms,
    })
}

impl PlanReviewResult {
    #[must_use]
    pub const fn outcome(&self) -> PlanReviewResultOutcome {
        match self {
            Self::Draft(_) => PlanReviewResultOutcome::Draft,
            Self::NoPlan { .. } => PlanReviewResultOutcome::NoPlan,
        }
    }

    #[must_use]
    pub fn content(&self) -> &str {
        match self {
            Self::Draft(entry) => entry.inline_text.as_deref().unwrap_or_default(),
            Self::NoPlan { reason } => reason,
        }
    }
}

/// Structured validation detail returned by the Plan result parser.
///
/// The actual value is projected through the normal durable-text safety policy and bounded before
/// it is exposed to a caller. This lets runtime build corrective model input without making raw
/// malformed payloads or secret-shaped text part of the feedback channel.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct PlanReviewValidationIssue {
    pub code: String,
    pub field_path: String,
    pub expected: String,
    pub bounded_safe_actual: String,
    pub instruction: String,
}

/// Error raised when a typed Plan review result cannot be safely accepted.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct PlanReviewResultValidationError {
    pub issue: PlanReviewValidationIssue,
}

impl fmt::Display for PlanReviewResultValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "plan review result validation failed at {} ({}): expected {}; actual {}; {}",
            self.issue.field_path,
            self.issue.code,
            self.issue.expected,
            self.issue.bounded_safe_actual,
            self.issue.instruction,
        )
    }
}

impl std::error::Error for PlanReviewResultValidationError {}

impl PlanReviewValidationIssue {
    fn new(
        code: impl Into<String>,
        field_path: impl Into<String>,
        expected: impl Into<String>,
        actual: impl AsRef<str>,
        instruction: impl Into<String>,
    ) -> Self {
        let actual = crate::safe_persistence_text(actual.as_ref());
        let bounded_safe_actual = actual
            .chars()
            .take(PLAN_REVIEW_RESULT_ACTUAL_MAX_CHARS)
            .collect();
        Self {
            code: code.into(),
            field_path: field_path.into(),
            expected: expected.into(),
            bounded_safe_actual,
            instruction: instruction.into(),
        }
    }
}

fn plan_review_result_validation_error(
    code: impl Into<String>,
    field_path: impl Into<String>,
    expected: impl Into<String>,
    actual: impl AsRef<str>,
    instruction: impl Into<String>,
) -> anyhow::Error {
    anyhow::Error::new(PlanReviewResultValidationError {
        issue: PlanReviewValidationIssue::new(code, field_path, expected, actual, instruction),
    })
}

/// Complete immutable detail for one structured plan-review step.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct PlanReviewStepDetailV1 {
    pub step_id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<AgentRole>,
    pub depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<TaskStepMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolation: Option<TaskIsolationMode>,
    pub target_paths: Vec<String>,
    pub required_capabilities: Vec<TaskCapabilityV2>,
    pub deliverables: Vec<String>,
    pub acceptance_criteria: Vec<String>,
    pub suggested_checks: Vec<PlanSuggestedCheck>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk: Option<String>,
    pub notes: Vec<String>,
}

/// Immutable lineage required to audit the plan-review attempt that produced a detail artifact.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct PlanLineageV1 {
    pub source: PlanSourceRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_review_id: Option<PlanReviewId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<crate::PlanReviewAttemptId>,
    pub created_at_ms: u64,
}

/// Complete immutable plan detail shared by TUI, Desktop, and HTTP adapters.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct PlanReviewDetailV1 {
    pub plan_id: PlanId,
    pub plan_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_snapshot_id: Option<String>,
    pub source: crate::PlanReviewSource,
    pub summary: String,
    pub steps: Vec<PlanReviewStepDetailV1>,
    pub target_paths: Vec<String>,
    pub suggested_checks: Vec<PlanSuggestedCheck>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk: Option<String>,
    pub notes: Vec<String>,
    pub lineage: PlanLineageV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_markdown: Option<String>,
}

/// Converts one exact durable plan artifact into its complete immutable review detail.
///
/// # Errors
///
/// Returns an error for unknown identities, hash drift, conflicting attempt facts, or a plan whose
/// source cannot be proven from the append-only lifecycle.
pub fn plan_review_detail_from_entries(
    entries: &[SessionLogEntry],
    plan_id: &PlanId,
    expected_plan_hash: &str,
) -> Result<PlanReviewDetailV1> {
    let artifacts = PlanArtifactProjection::from_entries(entries);
    let draft = artifacts
        .plans
        .get(plan_id)
        .ok_or_else(|| anyhow::anyhow!("plan detail references an unknown plan"))?;
    if draft.plan_hash != expected_plan_hash {
        bail!("plan detail does not bind the exact plan hash");
    }
    let attempts = entries
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::PlanReviewAttempt(attempt))
                if &attempt.plan_id == plan_id =>
            {
                Some(attempt)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    if let Some(first) = attempts.first() {
        if attempts
            .iter()
            .any(|attempt| attempt.plan_review_id != first.plan_review_id)
        {
            bail!("plan detail is bound to multiple review lifecycles");
        }
        let projection = PlanReviewProjection::from_entries(entries);
        let review = projection
            .review(&first.plan_review_id)
            .context("plan detail lost its exact review lifecycle")?;
        if !review.conflicts.is_empty() {
            bail!("plan detail review lifecycle has conflicting durable facts");
        }
    }
    let first = attempts.first().copied();
    let source = first
        .map(|attempt| attempt.source)
        .unwrap_or(crate::PlanReviewSource::ExplicitPlanCommand);
    let latest = attempts.last().copied();
    if latest.is_some_and(|attempt| {
        !matches!(
            attempt.status,
            crate::PlanReviewAttemptStatus::DraftReady
                | crate::PlanReviewAttemptStatus::CompileFailed
        )
    }) {
        bail!("plan detail is not bound to a DraftReady or CompileFailed attempt");
    }
    let steps = draft
        .steps
        .iter()
        .cloned()
        .map(|step| PlanReviewStepDetailV1 {
            step_id: step.step_id,
            title: step.title,
            display_name: step.display_name,
            detail: step.detail,
            role: step.role,
            depends_on: step.depends_on,
            mode: step.mode,
            isolation: step.isolation,
            target_paths: step.target_paths,
            required_capabilities: step.required_capabilities,
            deliverables: step.deliverables,
            acceptance_criteria: step.acceptance_criteria,
            suggested_checks: step.suggested_checks,
            risk: step.risk,
            notes: step.notes,
        })
        .collect();
    Ok(PlanReviewDetailV1 {
        plan_id: draft.plan_id.clone(),
        plan_hash: draft.plan_hash.clone(),
        workspace_snapshot_id: draft.workspace_snapshot_id.clone(),
        source,
        summary: draft.summary.clone(),
        steps,
        target_paths: draft.target_paths.clone(),
        suggested_checks: draft.suggested_checks.clone(),
        risk: draft.risk.clone(),
        notes: draft.notes.clone(),
        lineage: PlanLineageV1 {
            source: draft.source.clone(),
            plan_review_id: latest.map(|attempt| attempt.plan_review_id.clone()),
            attempt_id: latest.map(|attempt| attempt.attempt_id.clone()),
            created_at_ms: draft.created_at_ms,
        },
        legacy_markdown: draft
            .steps
            .is_empty()
            .then(|| draft.inline_text.clone())
            .flatten(),
    })
}

/// User decision recorded for a plan artifact.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanDecision {
    Accepted,
    Rejected,
    RevisionRequested,
    /// The requested revision could not start (host-side spawn failure). Unlike
    /// `RevisionRequested`, this is recoverable: the original plan stays actionable and a new
    /// revision of the same retry-stable identity may be requested.
    RevisionFailed,
    /// A revision candidate was committed and atomically replaced this base plan.
    RevisionSucceeded,
    /// The user's Run action could not create a runnable Task. The plan remains actionable and a
    /// later Run retries the same id/hash-bound promotion.
    TaskCreationFailed,
    SavedOnly,
}

impl PlanDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::RevisionRequested => "revision_requested",
            Self::RevisionFailed => "revision_failed",
            Self::RevisionSucceeded => "revision_succeeded",
            Self::TaskCreationFailed => "task_creation_failed",
            Self::SavedOnly => "saved_only",
        }
    }
}

/// Actor that made a plan decision.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanDecisionActor {
    User,
    System,
}

/// User-selected start mode when converting a plan to a task.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanTaskStartMode {
    CreatePaused,
    CreateAndRun,
}

/// Append-only record for accepting, rejecting or revising a plan artifact.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct PlanDecisionRecordedEntry {
    pub plan_id: PlanId,
    pub plan_hash: String,
    pub decision: PlanDecision,
    pub decided_by: PlanDecisionActor,
    pub decided_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Task-bound scoped permission grant created from an accepted plan.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct PlanPermissionGrantedEntry {
    pub plan_id: PlanId,
    pub plan_hash: String,
    pub task_id: TaskId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_snapshot_id: Option<String>,
    pub permission: PlanApprovalPermission,
    pub scope: PlanApprovalScope,
    pub expires: PlanApprovalExpiry,
    pub granted_at_ms: u64,
}

/// Append-only record linking one plan artifact to the task created from it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TaskCreatedFromPlanEntry {
    pub plan_id: PlanId,
    pub plan_hash: String,
    pub task_id: TaskId,
    pub created_at_ms: u64,
}

/// Typed Run command shared by every product surface (RFC-0067 9.1).
///
/// `source` is audit-only and never changes domain behavior. `expected_durable_frontier` is the
/// compare-and-swap position the approval append must still observe.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct PlanRunCommandV1 {
    pub command_id: String,
    pub session_id: String,
    pub plan_id: PlanId,
    pub expected_plan_hash: String,
    pub expected_durable_frontier: u64,
    pub start_mode: PlanTaskStartMode,
    pub permission: PlanRunPermissionChoiceV1,
    pub source: PlanRunCommandSource,
}

/// Audit-only source of a typed Run command.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanRunCommandSource {
    TuiKeyboard,
    TuiMouse,
    Desktop,
    Http,
    Cli,
    ModelTypedRoute,
}

impl PlanRunCommandSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TuiKeyboard => "tui_keyboard",
            Self::TuiMouse => "tui_mouse",
            Self::Desktop => "desktop",
            Self::Http => "http",
            Self::Cli => "cli",
            Self::ModelTypedRoute => "model_typed_route",
        }
    }
}

/// User choice of plan-scoped permission on one Run action (RFC-0067 7.5).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanRunPermissionChoiceV1 {
    KeepCurrentPolicy,
    GrantScopedEditsOnce,
}

/// Typed rejection of one Run command; the plan stays actionable (RFC-0067 9.3).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum PlanRunRejectionV1 {
    PlanMissing,
    PlanHashStale { expected: String, current: String },
    PlanNotReady,
    PlanRejected,
    FrontierStale { expected: u64, current: u64 },
    CommandIdentityConflict,
    PermissionChoiceUnavailable { reason: String },
    SessionWriterUnavailable { reason: String },
}

impl PlanRunRejectionV1 {
    pub fn reason_code(&self) -> &'static str {
        match self {
            Self::PlanMissing => "plan_missing",
            Self::PlanHashStale { .. } => "plan_hash_stale",
            Self::PlanNotReady => "plan_not_ready",
            Self::PlanRejected => "plan_rejected",
            Self::FrontierStale { .. } => "frontier_stale",
            Self::CommandIdentityConflict => "command_identity_conflict",
            Self::PermissionChoiceUnavailable { .. } => "permission_choice_unavailable",
            Self::SessionWriterUnavailable { .. } => "session_writer_unavailable",
        }
    }
}

/// Outcome of one atomic Plan approval append.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanApprovalCommit {
    /// The approval and direct Task authority were appended and synced.
    Appended,
    /// The frontier moved or the same approval was already committed; nothing changed.
    CasSkipped,
}

/// Atomically commits explicit Plan approval, a stable Task identity, and first-class direct
/// execution authority.
///
/// The four session entries are written as one crash-safe bundle under the durable single-writer
/// lease. Once approval is durable, the Task is therefore immediately executable without a
/// second model-authored materialization or DAG compilation boundary.
///
/// # Errors
///
/// Returns an error for stale authority, conflicting durable facts, a non-durable session or an
/// approval/task/direct-execution bundle that does not bind the same exact plan.
pub fn append_plan_approval_task_shell_at_frontier(
    session: &mut crate::Session,
    decision: &PlanDecisionRecordedEntry,
    task_run: &crate::TaskRunEntry,
    task_link: &TaskCreatedFromPlanEntry,
    direct_execution: &crate::TaskDirectExecutionAdmittedV1,
    checklist: Option<&crate::TaskChecklistUpdatedV1>,
    permission_grant: Option<&PlanPermissionGrantedEntry>,
    expected_frontier: u64,
) -> Result<PlanApprovalCommit> {
    if decision.decision != PlanDecision::Accepted || decision.decided_by != PlanDecisionActor::User
    {
        bail!("plan approval shell requires an explicit user Accepted decision");
    }
    if !matches!(
        task_run.status,
        crate::TaskRunStatus::Started | crate::TaskRunStatus::Paused
    ) {
        bail!("plan approval Task shell must start as Started or Paused");
    }
    let draft = session
        .plan_artifact_projection()
        .plans
        .get(&decision.plan_id)
        .cloned()
        .context("plan approval shell references an unknown durable draft")?;
    if draft.plan_hash != decision.plan_hash
        || task_id_from_plan_draft(&draft)? != task_run.task_id
        || task_link.plan_id != decision.plan_id
        || task_link.plan_hash != decision.plan_hash
        || task_link.task_id != task_run.task_id
        || direct_execution.task_id != task_run.task_id
        || !direct_execution.matches_objective(&task_run.objective)
        || !matches!(
            &direct_execution.source,
            crate::TaskDirectExecutionSourceV1::ApprovedPlan { plan_id, plan_hash }
                if plan_id == &decision.plan_id && plan_hash == &decision.plan_hash
        )
    {
        bail!("plan approval and direct Task execution do not bind the exact durable plan");
    }
    direct_execution.validate()?;
    if let Some(checklist) = checklist {
        checklist.validate()?;
        if checklist.task_id != task_run.task_id || checklist.revision != 1 {
            bail!("initial Task checklist does not bind the approved Task");
        }
    }
    if let Some(grant) = permission_grant
        && (grant.plan_id != draft.plan_id
            || grant.plan_hash != draft.plan_hash
            || grant.task_id != task_run.task_id
            || grant.workspace_snapshot_id != draft.workspace_snapshot_id
            || grant.permission != PlanApprovalPermission::WorkspaceEdits
            || grant.scope.workspace_paths != draft.target_paths
            || grant.scope.summary
                != format!("scoped edits for task {}", task_run.task_id.as_str())
            || grant.expires != PlanApprovalExpiry::Session
            || grant.granted_at_ms != decision.decided_at_ms)
    {
        bail!("plan approval permission grant does not bind the exact durable plan and Task");
    }
    let store = session
        .durable_store()
        .context("plan approval shell requires a durable session store")?;
    let predicate_decision = decision.clone();
    let predicate_task = task_run.clone();
    let predicate_link = task_link.clone();
    let predicate_direct = direct_execution.clone();
    let predicate_checklist = checklist.cloned();
    let predicate_grant = permission_grant.cloned();
    let mut entries = vec![
        SessionLogEntry::Control(ControlEntry::PlanDecisionRecorded(decision.clone())),
        SessionLogEntry::Control(ControlEntry::TaskRun(task_run.clone())),
        SessionLogEntry::Control(ControlEntry::TaskCreatedFromPlan(task_link.clone())),
        SessionLogEntry::Control(ControlEntry::TaskDirectExecutionAdmittedV1(
            direct_execution.clone(),
        )),
    ];
    if let Some(checklist) = checklist {
        entries.push(SessionLogEntry::Control(
            ControlEntry::TaskChecklistUpdatedV1(checklist.clone()),
        ));
    }
    if let Some(grant) = permission_grant {
        entries.push(SessionLogEntry::Control(
            ControlEntry::PlanPermissionGranted(grant.clone()),
        ));
    }
    let appended = store
        .append_events_and_session_entries_if(Vec::new(), &entries, move |records| {
            let current = records.last().map_or(0, |record| record.stream_sequence());
            if current != expected_frontier {
                return Ok(false);
            }
            let mut existing_entries = Vec::new();
            for record in records {
                if let Some(entry) = record.session_log_entry()? {
                    existing_entries.push(entry);
                }
            }
            let plans = PlanArtifactProjection::from_entries(&existing_entries);
            let accepted = plans
                .latest_decision(&predicate_decision.plan_id)
                .filter(|entry| entry.decision == PlanDecision::Accepted);
            let existing_link = plans
                .tasks_created
                .get(&predicate_link.plan_id)
                .and_then(|entries| entries.first());
            let tasks = crate::TaskStateProjection::from_entries(&existing_entries);
            let existing_task = tasks.tasks.get(&predicate_task.task_id);
            let existing_direct =
                existing_task.and_then(|task| task.direct_execution_admission.as_ref());
            let existing_checklist = existing_task.and_then(|task| task.checklist.as_ref());
            let existing_grant = plans
                .permission_grants
                .get(&predicate_decision.plan_id)
                .and_then(|grants| {
                    grants
                        .iter()
                        .find(|grant| grant.task_id == predicate_task.task_id)
                });
            match (accepted, existing_task, existing_link, existing_direct) {
                (None, None, None, None) if existing_grant.is_none() => Ok(true),
                (
                    Some(existing_decision),
                    Some(existing_task),
                    Some(existing_link),
                    Some(existing_direct),
                ) if existing_decision.plan_hash == predicate_decision.plan_hash
                    && existing_task.parent_session_ref == predicate_task.parent_session_ref
                    && existing_task.objective == predicate_task.objective
                    && existing_link.plan_hash == predicate_link.plan_hash
                    && existing_link.task_id == predicate_link.task_id
                    && existing_direct == &predicate_direct
                    && existing_checklist == predicate_checklist.as_ref()
                    && existing_grant == predicate_grant.as_ref() =>
                {
                    Ok(false)
                }
                _ => bail!("plan approval direct Task execution conflicts with durable state"),
            }
        })?
        .is_some();
    if appended {
        session
            .record_durably_appended_control(ControlEntry::PlanDecisionRecorded(decision.clone()));
        session.record_durably_appended_control(ControlEntry::TaskRun(task_run.clone()));
        session
            .record_durably_appended_control(ControlEntry::TaskCreatedFromPlan(task_link.clone()));
        session.record_durably_appended_control(ControlEntry::TaskDirectExecutionAdmittedV1(
            direct_execution.clone(),
        ));
        if let Some(checklist) = checklist {
            session.record_durably_appended_control(ControlEntry::TaskChecklistUpdatedV1(
                checklist.clone(),
            ));
        }
        if let Some(grant) = permission_grant {
            session.record_durably_appended_control(ControlEntry::PlanPermissionGranted(
                grant.clone(),
            ));
        }
        Ok(PlanApprovalCommit::Appended)
    } else {
        Ok(PlanApprovalCommit::CasSkipped)
    }
}

/// Permission chosen from the plan approval surface.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanApprovalPermission {
    /// Keep normal ask-before-action behavior after accepting the plan.
    Ask,
    /// Allow only diff-backed workspace file edit tools covered by the approved scope.
    WorkspaceEdits,
}

impl PlanApprovalPermission {
    /// Returns true only for tools that this plan approval can cover without widening policy.
    pub fn covers_tool(self, spec: &ToolSpec) -> bool {
        match self {
            Self::Ask => false,
            Self::WorkspaceEdits => {
                spec.category == ToolCategory::File
                    && spec.access == ToolAccess::Write
                    && spec.preview == ToolPreviewCapability::Required
            }
        }
    }
}

/// Scope recorded for an approved plan.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct PlanApprovalScope {
    pub summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspace_paths: Vec<String>,
}

/// Expiration policy for an approved plan.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum PlanApprovalExpiry {
    NextUserPrompt,
    Session,
    AtUnixMs(u64),
}

/// Materialized plan artifact state reconstructed from append-only entries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlanArtifactProjection {
    pub plans: BTreeMap<PlanId, PlanDraftCreatedEntry>,
    pub decisions: BTreeMap<PlanId, Vec<PlanDecisionRecordedEntry>>,
    pub permission_grants: BTreeMap<PlanId, Vec<PlanPermissionGrantedEntry>>,
    pub tasks_created: BTreeMap<PlanId, Vec<TaskCreatedFromPlanEntry>>,
    pub latest_plan_id: Option<PlanId>,
}

impl PlanArtifactProjection {
    /// Replays session entries into durable plan artifact state.
    pub fn from_entries(entries: &[SessionLogEntry]) -> Self {
        let mut projection = Self::default();
        for entry in entries {
            if let SessionLogEntry::Control(control) = entry {
                projection.apply_control_entry(control);
            }
        }
        projection
    }

    pub(crate) fn apply_control_entry(&mut self, control: &ControlEntry) {
        match control {
            ControlEntry::PlanDraftCreated(entry) => self.apply_draft(entry),
            ControlEntry::PlanDecisionRecorded(entry) => self.apply_decision(entry),
            ControlEntry::PlanPermissionGranted(entry) => self.apply_permission_grant(entry),
            ControlEntry::TaskCreatedFromPlan(entry) => self.apply_task_created(entry),
            _ => {}
        }
    }

    /// True when the exact readable Plan artifact is durable and reviewable.
    pub fn plan_is_ready(&self, plan_id: &PlanId) -> bool {
        self.plans.contains_key(plan_id)
    }

    pub fn latest_plan(&self) -> Option<&PlanDraftCreatedEntry> {
        self.latest_plan_id
            .as_ref()
            .and_then(|plan_id| self.plans.get(plan_id))
    }

    pub fn latest_pending_plan(&self) -> Option<&PlanDraftCreatedEntry> {
        self.latest_plan().filter(|plan| {
            !(self.plan_is_rejected(&plan.plan_id)
                || (self
                    .latest_decision(&plan.plan_id)
                    .is_some_and(|entry| entry.decision == PlanDecision::Accepted)
                    && self.task_created_for_plan(&plan.plan_id)))
        })
    }

    pub fn latest_decision(&self, plan_id: &PlanId) -> Option<&PlanDecisionRecordedEntry> {
        self.decisions
            .get(plan_id)
            .and_then(|entries| entries.last())
    }

    pub fn plan_has_terminal_decision(&self, plan_id: &PlanId) -> bool {
        self.latest_decision(plan_id).is_some_and(|entry| {
            matches!(
                entry.decision,
                PlanDecision::Accepted | PlanDecision::Rejected
            )
        })
    }

    pub fn plan_is_rejected(&self, plan_id: &PlanId) -> bool {
        self.latest_decision(plan_id)
            .is_some_and(|entry| entry.decision == PlanDecision::Rejected)
    }

    pub fn task_created_for_plan(&self, plan_id: &PlanId) -> bool {
        self.tasks_created
            .get(plan_id)
            .is_some_and(|entries| !entries.is_empty())
    }

    fn apply_draft(&mut self, entry: &PlanDraftCreatedEntry) {
        self.plans.insert(entry.plan_id.clone(), entry.clone());
        self.latest_plan_id = Some(entry.plan_id.clone());
    }

    fn apply_decision(&mut self, entry: &PlanDecisionRecordedEntry) {
        self.decisions
            .entry(entry.plan_id.clone())
            .or_default()
            .push(entry.clone());
    }

    fn apply_permission_grant(&mut self, entry: &PlanPermissionGrantedEntry) {
        self.permission_grants
            .entry(entry.plan_id.clone())
            .or_default()
            .push(entry.clone());
    }

    fn apply_task_created(&mut self, entry: &TaskCreatedFromPlanEntry) {
        self.tasks_created
            .entry(entry.plan_id.clone())
            .or_default()
            .push(entry.clone());
    }
}

/// Computes a stable hash for plan-mode output or user-approved plan text.
pub fn plan_text_hash(plan_text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(plan_text.as_bytes());
    format!("{PLAN_HASH_PREFIX}{:x}", hasher.finalize())
}

/// Creates a durable plan draft record from model output.
pub fn plan_draft_created_entry(
    plan_text: &str,
    source: PlanSourceRef,
    created_at_ms: u64,
    workspace_snapshot_id: Option<String>,
) -> Result<Option<PlanDraftCreatedEntry>> {
    let plan_text = plan_text.trim();
    if plan_text.is_empty() {
        return Ok(None);
    }
    let Some(exact_structured) = structured_plan_draft(plan_text) else {
        return Ok(None);
    };
    let structured = safe_structured_plan_draft(exact_structured.clone())?;
    let inline_plan_text = render_structured_plan_text(&structured);
    let plan_hash = if structured == exact_structured {
        plan_text_hash(plan_text)
    } else {
        plan_text_hash(&inline_plan_text)
    };
    let plan_id = plan_id_from_hash(&plan_hash)?;
    plan_draft_created_entry_with_plan_id(
        plan_id,
        plan_text,
        source,
        created_at_ms,
        workspace_snapshot_id,
    )
}

/// Captures non-empty model-authored prose under a host-derived Plan identity.
///
/// This is the model-agnostic fallback for Plan surfaces that do not already own a stable review
/// identity. Structured Plan output may still use [`plan_draft_created_entry`] for richer display,
/// but successful execution never depends on the model emitting that schema.
///
/// # Errors
///
/// Returns an error when the projected text exceeds the durable inline Plan bound.
pub fn plain_text_plan_draft_entry(
    plan_text: &str,
    source: PlanSourceRef,
    created_at_ms: u64,
    workspace_snapshot_id: Option<String>,
) -> Result<Option<PlanDraftCreatedEntry>> {
    let plan_text = crate::safe_persistence_text(plan_text.trim());
    if plan_text.trim().is_empty() {
        return Ok(None);
    }
    let plan_id = plan_id_from_hash(&plan_text_hash(&plan_text))?;
    plain_text_plan_draft_entry_with_plan_id(
        plan_id,
        &plan_text,
        source,
        created_at_ms,
        workspace_snapshot_id,
    )
}

/// Creates a durable plan draft record bound to a host-derived plan identity.
///
/// Plan review lifecycles derive the plan id deterministically from the plan review and attempt
/// identity (RFC-0063), so the identity is stable before the draft content exists. The plan hash
/// still binds the exact draft content for stale decisions.
pub fn plan_draft_created_entry_with_plan_id(
    plan_id: PlanId,
    plan_text: &str,
    source: PlanSourceRef,
    created_at_ms: u64,
    workspace_snapshot_id: Option<String>,
) -> Result<Option<PlanDraftCreatedEntry>> {
    let plan_text = plan_text.trim();
    if plan_text.is_empty() {
        return Ok(None);
    }
    let Some(exact_structured) = structured_plan_draft(plan_text) else {
        return Ok(None);
    };
    let structured = safe_structured_plan_draft(exact_structured.clone())?;
    let inline_plan_text = render_structured_plan_text(&structured);
    let plan_hash = if structured == exact_structured {
        plan_text_hash(plan_text)
    } else {
        plan_text_hash(&inline_plan_text)
    };
    plan_draft_entry_from_structured(
        plan_id,
        structured,
        plan_hash,
        source,
        created_at_ms,
        workspace_snapshot_id,
    )
}

/// Captures a non-empty model-authored Plan as bounded reviewable text under a host identity.
///
/// The current typed review result and explicit candidate adoption use this constructor after
/// confirming the result. The host supplies every identity and authority field; unconfirmed
/// prose carries no Task DAG, role, intent, capability, permission, or scheduler authority.
///
/// # Errors
///
/// Returns an error when the projected text exceeds the durable inline Plan bound.
pub fn plain_text_plan_draft_entry_with_plan_id(
    plan_id: PlanId,
    plan_text: &str,
    source: PlanSourceRef,
    created_at_ms: u64,
    workspace_snapshot_id: Option<String>,
) -> Result<Option<PlanDraftCreatedEntry>> {
    let plan_text = crate::safe_persistence_text(plan_text.trim());
    if plan_text.trim().is_empty() {
        return Ok(None);
    }
    if plan_text.len() > PLAN_INLINE_TEXT_MAX_BYTES {
        bail!("plain-text plan exceeds the {PLAN_INLINE_TEXT_MAX_BYTES}-byte durable inline limit");
    }
    let summary_line = plan_text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("Plan")
        .trim()
        .trim_start_matches(|character: char| {
            character == '#' || character == '-' || character == '*' || character.is_whitespace()
        });
    let mut summary = summary_line.chars().take(240).collect::<String>();
    if summary.is_empty() {
        summary = "Plan".to_owned();
    }
    Ok(Some(PlanDraftCreatedEntry {
        plan_id,
        schema_version: 2,
        source,
        plan_hash: plan_text_hash(&plan_text),
        summary,
        inline_text: Some(plan_text),
        steps: Vec::new(),
        intent_proposal: None,
        target_paths: Vec::new(),
        suggested_checks: Vec::new(),
        risk: None,
        notes: vec![
            "Captured from plain model output; structured fields are display-only and unavailable."
                .to_owned(),
        ],
        workspace_snapshot_id,
        created_at_ms,
    }))
}

fn plan_draft_entry_from_structured(
    plan_id: PlanId,
    structured: StructuredPlanDraft,
    plan_hash: String,
    source: PlanSourceRef,
    created_at_ms: u64,
    workspace_snapshot_id: Option<String>,
) -> Result<Option<PlanDraftCreatedEntry>> {
    if structured
        .steps
        .iter()
        .any(|step| step.mode == Some(TaskStepMode::Verify))
    {
        bail!(
            "plan draft cannot create verify participant steps; add the check as a suggested check and let the host run trusted verification"
        );
    }
    if structured.steps.iter().any(|step| {
        step.required_capabilities
            .contains(&TaskCapabilityV2::VerificationRun)
    }) {
        bail!(
            "plan draft cannot delegate verification_run; add trusted checks to suggested_checks"
        );
    }
    let intent_proposal = intent_proposal_from_structured(&structured, &plan_hash)?;
    let inline_text = render_structured_plan_text(&structured);
    let inline_text = (inline_text.len() <= PLAN_INLINE_TEXT_MAX_BYTES).then_some(inline_text);
    Ok(Some(PlanDraftCreatedEntry {
        plan_id,
        schema_version: structured.schema_version,
        source,
        plan_hash,
        summary: structured.summary,
        inline_text,
        steps: structured.steps,
        intent_proposal,
        target_paths: structured.target_paths,
        suggested_checks: structured.suggested_checks,
        risk: structured.risk,
        notes: structured.notes,
        workspace_snapshot_id,
        created_at_ms,
    }))
}

/// Strict model-visible structured draft submitted through the typed plan review tool.
/// Core envelope for a provider-neutral Plan review result.
///
/// Presentation metadata is intentionally not part of this first version. Keeping the envelope
/// small means a provider only has to choose the result type and provide the complete text; the
/// host can add display-only projections later without making them acceptance or authority fields.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct PlanReviewResultEnvelope {
    pub schema_version: u32,
    pub outcome: PlanReviewResultOutcome,
    pub content: String,
}

/// Decodes and validates a provider-neutral Plan review result without assigning Plan identity.
///
/// This is useful when a caller is recovering a persisted tool result: it can classify the
/// durable `draft`/`no_plan` envelope before deciding whether to materialize a Plan artifact.
pub fn decode_plan_review_result(args_json: &str) -> Result<PlanReviewResultEnvelope> {
    let args: PlanReviewResultEnvelope = serde_json::from_str(args_json).map_err(|error| {
        plan_review_result_validation_error(
            "invalid_result_envelope",
            "$",
            "schema_version, outcome, and content",
            error.to_string(),
            "return schema_version, outcome, and content fields",
        )
    })?;
    if args.schema_version != PLAN_REVIEW_RESULT_SCHEMA_VERSION {
        return Err(plan_review_result_validation_error(
            "invalid_result_schema_version",
            "$.schema_version",
            PLAN_REVIEW_RESULT_SCHEMA_VERSION.to_string(),
            args.schema_version.to_string(),
            "use the currently advertised Plan review result schema version",
        ));
    }
    Ok(PlanReviewResultEnvelope {
        content: bounded_plan_review_result_content(&args.content)?,
        ..args
    })
}

/// Validates and materializes the small typed Plan review result envelope.
///
/// A `draft` result stores the complete policy-safe content as an immutable plain-text Plan. A
/// `no_plan` result returns only a bounded explanation and never creates a Plan draft. This
/// boundary does not require structured execution hints or a Task compilation contract.
///
/// # Errors
///
/// Returns a [`PlanReviewResultValidationError`] wrapped in `anyhow::Error` for malformed JSON,
/// missing or invalid known fields, an unsupported schema/outcome, empty content, or content beyond the durable
/// inline bound. Callers can downcast the error to retain typed corrective feedback.
pub fn submit_plan_review_result(
    args_json: &str,
    plan_id: PlanId,
    source: PlanSourceRef,
    created_at_ms: u64,
    workspace_snapshot_id: Option<String>,
) -> Result<PlanReviewResult> {
    let args = decode_plan_review_result(args_json)?;

    match args.outcome {
        PlanReviewResultOutcome::Draft => {
            let draft = plain_text_plan_draft_entry_with_plan_id(
                plan_id,
                &args.content,
                source,
                created_at_ms,
                workspace_snapshot_id,
            )?
            .ok_or_else(|| {
                plan_review_result_validation_error(
                    "empty_result_content",
                    "$.content",
                    "a non-empty complete Plan",
                    args.content,
                    "provide the complete readable Plan body for a draft result",
                )
            })?;
            Ok(PlanReviewResult::Draft(Box::new(draft)))
        }
        PlanReviewResultOutcome::NoPlan => Ok(PlanReviewResult::NoPlan {
            reason: args.content,
        }),
    }
}

fn bounded_plan_review_result_content(content: &str) -> Result<String> {
    let content = crate::safe_persistence_text(content.trim());
    if content.trim().is_empty() {
        return Err(plan_review_result_validation_error(
            "empty_result_content",
            "$.content",
            "a non-empty complete Plan or no-plan explanation",
            content,
            "provide a bounded explanation or complete readable Plan body",
        ));
    }
    if content.len() > PLAN_INLINE_TEXT_MAX_BYTES {
        return Err(plan_review_result_validation_error(
            "result_content_too_large",
            "$.content",
            format!("at most {PLAN_INLINE_TEXT_MAX_BYTES} bytes"),
            format!("{} bytes", content.len()),
            "shorten the content while preserving the complete result",
        ));
    }
    Ok(content)
}

/// Builds the durable objective passed to the direct executor after a user approves a plan.
///
/// The approved Plan remains model-authored task input. Its optional structured fields are
/// presentation context only; the host does not infer scheduler authority from the prose.
pub fn plan_task_input_from_draft(entry: &PlanDraftCreatedEntry) -> String {
    let plan_text = entry.inline_text.clone().unwrap_or_else(|| {
        render_structured_plan_text(&StructuredPlanDraft {
            schema_version: entry.schema_version,
            summary: entry.summary.clone(),
            steps: entry.steps.clone(),
            intents: entry
                .intent_proposal
                .as_ref()
                .map(|proposal| proposal.intents.clone())
                .unwrap_or_default(),
            target_paths: entry.target_paths.clone(),
            suggested_checks: entry.suggested_checks.clone(),
            risk: entry.risk.clone(),
            notes: entry.notes.clone(),
        })
    });
    format!(
        "Execute the following user-approved Plan with the configured approval and verification requirements. Treat the complete Plan as task context, inspect the live workspace, use tools as needed, and adapt implementation details when necessary for correctness.\n\nApproved Plan:\n\n{}",
        plan_text.trim()
    )
}

/// Returns the retry-stable task identity owned by one durable plan artifact.
///
/// The identity intentionally excludes timestamps and replay-order counters so a crash before the
/// final accepted-plan commit marker can reconcile the same task instead of allocating another.
///
/// # Errors
///
/// Returns an error when the derived task identifier cannot be represented safely.
pub fn task_id_from_plan_draft(entry: &PlanDraftCreatedEntry) -> Result<TaskId> {
    let mut digest = Sha256::new();
    digest.update(b"sigil-plan-task-v1");
    digest.update([0]);
    digest.update(entry.plan_id.as_str().as_bytes());
    digest.update([0]);
    digest.update(entry.plan_hash.as_bytes());
    let digest = format!("{:x}", digest.finalize());
    TaskId::new(format!("plan-task-{}", &digest[..24]))
}

/// Extracts conservative workspace path scopes from plan text.
///
/// The result is best-effort metadata for approval scoping, not a natural-language verifier. When
/// no path-like token is present, callers may keep the scope empty to preserve existing behavior.
pub fn plan_workspace_paths(plan_text: &str) -> Vec<String> {
    let mut paths = BTreeSet::new();
    let mut candidate = String::new();
    for character in plan_text.chars() {
        if is_plan_path_character(character) {
            candidate.push(character);
            continue;
        }
        collect_plan_path_candidate(&mut paths, &mut candidate);
    }
    collect_plan_path_candidate(&mut paths, &mut candidate);
    collapse_plan_workspace_paths(paths)
}

fn is_plan_path_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '/' | '.' | '_' | '-')
}

fn collect_plan_path_candidate(paths: &mut BTreeSet<String>, candidate: &mut String) {
    if let Some(path) = normalize_plan_workspace_path(candidate) {
        paths.insert(path);
    }
    candidate.clear();
}

fn normalize_plan_workspace_path(candidate: &str) -> Option<String> {
    let trimmed = candidate.trim_end_matches('.');
    if trimmed.is_empty()
        || trimmed.contains("://")
        || trimmed.starts_with('/')
        || trimmed.starts_with('~')
    {
        return None;
    }
    if !looks_like_workspace_path(trimmed) {
        return None;
    }

    let mut components = Vec::new();
    for component in Path::new(trimmed).components() {
        match component {
            Component::Normal(part) => {
                let part = part.to_string_lossy();
                if part.is_empty() {
                    return None;
                }
                components.push(part.into_owned());
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if components.is_empty() {
        return None;
    }
    Some(components.join("/"))
}

fn looks_like_workspace_path(candidate: &str) -> bool {
    candidate.contains('/')
        || candidate.starts_with('.')
        || candidate.rsplit_once('.').is_some_and(|(stem, extension)| {
            !stem.is_empty()
                && !extension.is_empty()
                && extension.len() <= 10
                && extension
                    .chars()
                    .any(|character| character.is_ascii_alphabetic())
        })
}

fn collapse_plan_workspace_paths(paths: BTreeSet<String>) -> Vec<String> {
    let mut collapsed: Vec<String> = Vec::new();
    for path in paths {
        if collapsed
            .iter()
            .any(|scope| plan_path_is_within_scope(&path, scope))
        {
            continue;
        }
        collapsed.push(path);
    }
    collapsed
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StructuredPlanDraft {
    schema_version: u32,
    summary: String,
    steps: Vec<PlanDraftStep>,
    intents: Vec<IntentProposalUnitV1>,
    target_paths: Vec<String>,
    suggested_checks: Vec<PlanSuggestedCheck>,
    risk: Option<String>,
    notes: Vec<String>,
}

fn safe_structured_plan_draft(mut plan: StructuredPlanDraft) -> Result<StructuredPlanDraft> {
    plan.summary = crate::safe_persistence_text(&plan.summary);
    if plan.summary.len() > PLAN_SUMMARY_MAX_BYTES {
        bail!("plan summary exceeds the {PLAN_SUMMARY_MAX_BYTES}-byte durable detail limit");
    }
    plan.target_paths.retain(|path| {
        crate::safe_persistence_text(path) == *path && !plan_identifier_has_secret_marker(path)
    });
    plan.risk = plan.risk.as_deref().map(crate::safe_persistence_text);
    plan.notes = plan
        .notes
        .iter()
        .map(|note| crate::safe_persistence_text(note))
        .collect();
    plan.suggested_checks = plan
        .suggested_checks
        .into_iter()
        .filter_map(safe_plan_suggested_check)
        .collect();
    for intent in &mut plan.intents {
        intent.title = crate::safe_persistence_text(&intent.title);
        intent.statement = crate::safe_persistence_text(&intent.statement);
        for criterion in &mut intent.acceptance_criteria {
            criterion.statement = crate::safe_persistence_text(&criterion.statement);
        }
    }

    let mut step_ids = BTreeSet::new();
    for (index, step) in plan.steps.iter_mut().enumerate() {
        let safe_id = crate::safe_persistence_text(&step.step_id);
        let id_seed = if safe_id == step.step_id && !plan_identifier_has_secret_marker(&safe_id) {
            safe_id
        } else {
            format!("step_{}", index + 1)
        };
        step.step_id = unique_plan_step_id(&id_seed, index, &mut step_ids);
        step.title = crate::safe_persistence_text(&step.title);
        step.display_name = bounded_plan_step_display_name(step.display_name.as_deref())?;
        step.detail = step.detail.as_deref().map(crate::safe_persistence_text);
        step.depends_on = step
            .depends_on
            .iter()
            .map(|dependency| crate::safe_persistence_text(dependency))
            .filter(|dependency| validate_plan_stable_id("plan dependency", dependency).is_ok())
            .collect();
        step.intent_aliases = step
            .intent_aliases
            .iter()
            .map(|alias| crate::safe_persistence_text(alias))
            .collect();
        step.risk = step.risk.as_deref().map(crate::safe_persistence_text);
        step.target_paths.retain(|path| {
            crate::safe_persistence_text(path) == *path && !plan_identifier_has_secret_marker(path)
        });
        step.required_capabilities = step
            .required_capabilities
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        step.deliverables = step
            .deliverables
            .iter()
            .map(|value| crate::safe_persistence_text(value))
            .filter(|value| !value.trim().is_empty())
            .collect();
        step.acceptance_criteria = step
            .acceptance_criteria
            .iter()
            .map(|value| crate::safe_persistence_text(value))
            .filter(|value| !value.trim().is_empty())
            .collect();
        step.notes = step
            .notes
            .iter()
            .map(|note| crate::safe_persistence_text(note))
            .collect();
        step.suggested_checks = step
            .suggested_checks
            .drain(..)
            .filter_map(safe_plan_suggested_check)
            .collect();
    }
    Ok(plan)
}

/// Canonicalizes optional presentation metadata without allowing it to block execution.
///
/// A plan step's full semantic label remains in `title`; `display_name` is only the compact child
/// label. Older drafts may predate the model-visible length constraint, so promotion applies this
/// same canonicalizer defensively instead of rejecting an otherwise executable plan.
fn bounded_plan_step_display_name(value: Option<&str>) -> Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let safe = crate::safe_persistence_text(value);
    let safe = safe.trim();
    if safe.is_empty() {
        return Ok(None);
    }
    let max_chars = crate::TASK_AGENT_DISPLAY_NAME_MAX_CHARS;
    let bounded = if safe.chars().count() > max_chars {
        let retained = max_chars.saturating_sub(1);
        format!("{}…", safe.chars().take(retained).collect::<String>())
    } else {
        safe.to_owned()
    };
    crate::normalize_task_agent_display_name(&bounded).map(Some)
}

fn safe_plan_suggested_check(mut check: PlanSuggestedCheck) -> Option<PlanSuggestedCheck> {
    let safe_command = crate::safe_persistence_text(&check.command.command);
    let safe_args = check
        .command
        .args
        .iter()
        .map(|arg| crate::safe_persistence_text(arg))
        .collect::<Vec<_>>();
    let safe_cwd = check
        .command
        .cwd
        .as_ref()
        .map(|cwd| PathBuf::from(crate::safe_persistence_text(&cwd.to_string_lossy())));
    if safe_command != check.command.command
        || safe_args != check.command.args
        || safe_cwd != check.command.cwd
    {
        return None;
    }
    let safe_check_spec_id = crate::safe_persistence_text(&check.check_spec_id);
    check.check_spec_id = if safe_check_spec_id == check.check_spec_id
        && !plan_identifier_has_secret_marker(&safe_check_spec_id)
    {
        safe_check_spec_id
    } else {
        check_spec_id_from_command(&safe_command, &safe_args)
    };
    check.command.command = safe_command;
    check.command.args = safe_args;
    check.command.cwd = safe_cwd;
    check.source_line = check
        .source_line
        .as_deref()
        .map(crate::safe_persistence_text);
    Some(check)
}

fn plan_identifier_has_secret_marker(value: &str) -> bool {
    value
        .split(['_', '-', '.', ':'])
        .map(str::to_ascii_lowercase)
        .any(|segment| {
            matches!(
                segment.as_str(),
                "authorization"
                    | "bearer"
                    | "cookie"
                    | "credential"
                    | "password"
                    | "secret"
                    | "signature"
                    | "sig"
                    | "token"
                    | "apikey"
                    | "accesskey"
            )
        })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct RawStructuredPlanDraft {
    #[serde(default)]
    summary: String,
    #[serde(default)]
    steps: Vec<RawPlanDraftStep>,
    #[serde(default)]
    intents: Vec<IntentProposalUnitV1>,
    #[serde(default)]
    target_paths: Vec<String>,
    #[serde(default)]
    suggested_checks: Vec<RawPlanSuggestedCheck>,
    #[serde(default)]
    risk: Option<String>,
    #[serde(default, deserialize_with = "deserialize_string_or_list")]
    notes: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct RawPlanDraftStep {
    #[serde(default)]
    step_id: Option<String>,
    title: String,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    detail: Option<String>,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    depends_on: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_string_or_list")]
    intent_aliases: Vec<String>,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    isolation: Option<String>,
    #[serde(default)]
    target_paths: Vec<String>,
    #[serde(default)]
    required_capabilities: Vec<TaskCapabilityV2>,
    #[serde(default, deserialize_with = "deserialize_string_or_list")]
    deliverables: Vec<String>,
    #[serde(default)]
    suggested_checks: Vec<RawPlanSuggestedCheck>,
    #[serde(default)]
    risk: Option<String>,
    #[serde(default, deserialize_with = "deserialize_string_or_list")]
    notes: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_string_or_list")]
    acceptance: Vec<String>,
}

fn deserialize_string_or_list<'de, D>(deserializer: D) -> std::result::Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrList {
        One(String),
        Many(Vec<String>),
    }

    Ok(match StringOrList::deserialize(deserializer)? {
        StringOrList::One(value) => vec![value],
        StringOrList::Many(values) => values,
    })
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum RawPlanSuggestedCheck {
    CommandLine(String),
    Object(RawPlanSuggestedCheckObject),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) struct RawPlanSuggestedCheckObject {
    #[serde(default)]
    check_spec_id: Option<String>,
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    cwd: Option<PathBuf>,
    #[serde(default)]
    effect: Option<ToolEffect>,
    #[serde(default)]
    source_line: Option<String>,
}

fn structured_plan_draft(plan_text: &str) -> Option<StructuredPlanDraft> {
    for (schema_version, block) in structured_plan_blocks(plan_text) {
        let Ok(raw) = serde_json::from_str::<RawStructuredPlanDraft>(&block) else {
            continue;
        };
        let structured = materialize_structured_plan(schema_version, raw);
        if !structured.steps.is_empty() {
            return Some(structured);
        }
    }
    None
}

fn structured_plan_blocks(plan_text: &str) -> Vec<(u32, String)> {
    let mut blocks = Vec::new();
    let mut active_fence: Option<&str> = None;
    let mut schema_version = None;
    let mut buffer = String::new();

    for line in plan_text.lines() {
        let trimmed = line.trim_start();
        if let Some(fence) = active_fence {
            if trimmed.starts_with(fence) {
                if let Some(schema_version) = schema_version {
                    blocks.push((schema_version, buffer.trim().to_owned()));
                }
                active_fence = None;
                schema_version = None;
                buffer.clear();
                continue;
            }
            if schema_version.is_some() {
                buffer.push_str(line);
                buffer.push('\n');
            }
            continue;
        }

        let Some((fence, info)) = parse_fence_start(trimmed) else {
            continue;
        };
        active_fence = Some(fence);
        schema_version = structured_plan_schema_version(info);
        buffer.clear();
    }

    blocks
}

fn parse_fence_start(line: &str) -> Option<(&'static str, &str)> {
    if let Some(info) = line.strip_prefix("```") {
        Some(("```", info.trim()))
    } else if let Some(info) = line.strip_prefix("~~~") {
        Some(("~~~", info.trim()))
    } else {
        None
    }
}

fn structured_plan_schema_version(info: &str) -> Option<u32> {
    info.split_whitespace()
        .any(|part| part == "sigil-plan-v2")
        .then_some(2)
}

fn materialize_structured_plan(
    schema_version: u32,
    raw: RawStructuredPlanDraft,
) -> StructuredPlanDraft {
    let mut step_ids = BTreeSet::new();
    let steps = raw
        .steps
        .into_iter()
        .enumerate()
        .filter_map(|(index, raw_step)| materialize_plan_step(index, raw_step, &mut step_ids))
        .collect::<Vec<_>>();

    let mut target_paths = BTreeSet::new();
    for path in raw.target_paths {
        if let Some(path) = normalize_plan_workspace_path(&path) {
            target_paths.insert(path);
        }
    }
    for step in &steps {
        for path in &step.target_paths {
            target_paths.insert(path.clone());
        }
    }

    let mut suggested_checks = BTreeMap::<String, PlanSuggestedCheck>::new();
    for check in raw.suggested_checks {
        if let Some(check) = materialize_plan_suggested_check(check) {
            suggested_checks.insert(check.check_spec_id.clone(), check);
        }
    }
    for step in &steps {
        for check in &step.suggested_checks {
            suggested_checks.insert(check.check_spec_id.clone(), check.clone());
        }
    }

    let summary = nonempty_trimmed(raw.summary)
        .or_else(|| steps.first().map(|step| step.title.clone()))
        .unwrap_or_else(|| "plan".to_owned());

    StructuredPlanDraft {
        schema_version,
        summary,
        steps,
        intents: raw.intents,
        target_paths: collapse_plan_workspace_paths(target_paths),
        suggested_checks: suggested_checks.into_values().collect(),
        risk: raw.risk.and_then(nonempty_trimmed),
        notes: raw.notes.into_iter().filter_map(nonempty_trimmed).collect(),
    }
}

fn materialize_plan_step(
    index: usize,
    raw_step: RawPlanDraftStep,
    step_ids: &mut BTreeSet<String>,
) -> Option<PlanDraftStep> {
    let title = nonempty_trimmed(raw_step.title)?;
    let mut target_paths = BTreeSet::new();
    for path in raw_step.target_paths {
        if let Some(path) = normalize_plan_workspace_path(&path) {
            target_paths.insert(path);
        }
    }
    let suggested_checks = raw_step
        .suggested_checks
        .into_iter()
        .filter_map(materialize_plan_suggested_check)
        .collect::<Vec<_>>();
    let notes = raw_step
        .notes
        .into_iter()
        .filter_map(nonempty_trimmed)
        .collect::<Vec<_>>();
    let acceptance_criteria = raw_step
        .acceptance
        .into_iter()
        .filter_map(nonempty_trimmed)
        .collect::<Vec<_>>();
    let step_id = unique_plan_step_id(
        raw_step.step_id.as_deref().unwrap_or(&title),
        index,
        step_ids,
    );
    Some(PlanDraftStep {
        step_id,
        title,
        display_name: raw_step.display_name.and_then(nonempty_trimmed),
        detail: raw_step.detail.and_then(nonempty_trimmed),
        role: raw_step.role.as_deref().and_then(parse_plan_agent_role),
        depends_on: raw_step
            .depends_on
            .into_iter()
            .filter_map(nonempty_trimmed)
            .collect(),
        intent_aliases: raw_step
            .intent_aliases
            .into_iter()
            .filter_map(nonempty_trimmed)
            .collect(),
        mode: raw_step.mode.as_deref().and_then(parse_plan_step_mode),
        isolation: raw_step.isolation.as_deref().and_then(parse_plan_isolation),
        target_paths: collapse_plan_workspace_paths(target_paths),
        required_capabilities: raw_step.required_capabilities,
        deliverables: raw_step
            .deliverables
            .into_iter()
            .filter_map(nonempty_trimmed)
            .collect(),
        acceptance_criteria,
        suggested_checks,
        risk: raw_step.risk.and_then(nonempty_trimmed),
        notes,
    })
}

fn parse_plan_agent_role(value: &str) -> Option<AgentRole> {
    match value.trim() {
        "planner" => Some(AgentRole::Planner),
        "executor" => Some(AgentRole::Executor),
        "subagent_read" => Some(AgentRole::SubagentRead),
        "subagent_write" => Some(AgentRole::SubagentWrite),
        _ => None,
    }
}

fn parse_plan_step_mode(value: &str) -> Option<TaskStepMode> {
    match value.trim() {
        "read" => Some(TaskStepMode::Read),
        "write" => Some(TaskStepMode::Write),
        "review" => Some(TaskStepMode::Review),
        "verify" => Some(TaskStepMode::Verify),
        _ => None,
    }
}

fn parse_plan_isolation(value: &str) -> Option<TaskIsolationMode> {
    match value.trim() {
        "shared_read_only" => Some(TaskIsolationMode::SharedReadOnly),
        "sequential_workspace_write" => Some(TaskIsolationMode::SequentialWorkspaceWrite),
        "changeset_only" => Some(TaskIsolationMode::ChangesetOnly),
        "worktree" => Some(TaskIsolationMode::Worktree),
        _ => None,
    }
}

fn materialize_plan_suggested_check(raw: RawPlanSuggestedCheck) -> Option<PlanSuggestedCheck> {
    match raw {
        RawPlanSuggestedCheck::CommandLine(command_line) => {
            let mut parts = command_line
                .split_whitespace()
                .filter(|part| !part.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>();
            if parts.is_empty() {
                return None;
            }
            let command = parts.remove(0);
            let check_spec_id = check_spec_id_from_command(&command, &parts);
            Some(PlanSuggestedCheck {
                check_spec_id,
                command: CheckCommand {
                    command,
                    args: parts,
                    cwd: None,
                },
                effect: ToolEffect::ReadOnly,
                source_line: Some(command_line),
            })
        }
        RawPlanSuggestedCheck::Object(raw) => {
            let command = nonempty_trimmed(raw.command)?;
            let args = raw
                .args
                .into_iter()
                .filter_map(nonempty_trimmed)
                .collect::<Vec<_>>();
            let check_spec_id = raw
                .check_spec_id
                .and_then(nonempty_trimmed)
                .unwrap_or_else(|| check_spec_id_from_command(&command, &args));
            Some(PlanSuggestedCheck {
                check_spec_id,
                command: CheckCommand {
                    command,
                    args,
                    cwd: raw.cwd,
                },
                effect: raw.effect.unwrap_or(ToolEffect::ReadOnly),
                source_line: raw.source_line.and_then(nonempty_trimmed),
            })
        }
    }
}

fn check_spec_id_from_command(command: &str, args: &[String]) -> String {
    let mut raw = std::iter::once(command)
        .chain(args.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join("-");
    if raw.is_empty() {
        raw = "check".to_owned();
    }
    let mut id = raw
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    while id.contains("--") {
        id = id.replace("--", "-");
    }
    id = id.trim_matches('-').chars().take(72).collect();
    if id.is_empty() {
        "check".to_owned()
    } else {
        id
    }
}

fn unique_plan_step_id(raw: &str, index: usize, step_ids: &mut BTreeSet<String>) -> String {
    let mut id = raw
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else if matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    while id.contains("__") {
        id = id.replace("__", "_");
    }
    id = id.trim_matches('_').chars().take(64).collect();
    if validate_plan_stable_id("plan step id", &id).is_err() {
        id = format!("step_{}", index + 1);
    }
    if step_ids.insert(id.clone()) {
        return id;
    }
    let base = id;
    let mut suffix = 2usize;
    loop {
        let candidate = format!("{base}_{suffix}");
        if step_ids.insert(candidate.clone()) {
            return candidate;
        }
        suffix = suffix.saturating_add(1);
    }
}

fn render_structured_plan_text(plan: &StructuredPlanDraft) -> String {
    let mut lines = vec![
        format!("Summary: {}", plan.summary),
        String::new(),
        "Steps:".to_owned(),
    ];
    for (index, step) in plan.steps.iter().enumerate() {
        lines.push(format!("{}. {} [{}]", index + 1, step.title, step.step_id));
        if let Some(detail) = &step.detail {
            lines.push(format!("   Detail: {detail}"));
        }
        if let Some(role) = step.role {
            lines.push(format!("   Role: {}", role.as_str()));
        }
        if !step.depends_on.is_empty() {
            lines.push(format!("   Depends on: {}", step.depends_on.join(", ")));
        }
        if !step.intent_aliases.is_empty() {
            lines.push(format!(
                "   Intent aliases: {}",
                step.intent_aliases.join(", ")
            ));
        }
        if let Some(mode) = step.mode {
            lines.push(format!("   Mode: {}", mode.as_str()));
        }
        if let Some(isolation) = step.isolation {
            lines.push(format!("   Isolation: {}", isolation.as_str()));
        }
        if !step.target_paths.is_empty() {
            lines.push(format!("   Paths: {}", step.target_paths.join(", ")));
        }
        if !step.required_capabilities.is_empty() {
            lines.push(format!(
                "   Required capabilities: {}",
                step.required_capabilities
                    .iter()
                    .map(|capability| capability.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        for deliverable in &step.deliverables {
            lines.push(format!("   Deliverable: {deliverable}"));
        }
        for criterion in &step.acceptance_criteria {
            lines.push(format!("   Acceptance: {criterion}"));
        }
        if !step.suggested_checks.is_empty() {
            lines.push(format!(
                "   Checks: {}",
                step.suggested_checks
                    .iter()
                    .map(render_plan_check_command)
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
        if let Some(risk) = &step.risk {
            lines.push(format!("   Risk: {risk}"));
        }
        for note in &step.notes {
            lines.push(format!("   Note: {note}"));
        }
    }
    if !plan.intents.is_empty() {
        lines.push(String::new());
        lines.push("Intents:".to_owned());
        for intent in &plan.intents {
            lines.push(format!(
                "- {} [{}]: {}",
                intent.title, intent.intent_alias, intent.statement
            ));
            if !intent.depends_on_aliases.is_empty() {
                lines.push(format!(
                    "  Depends on: {}",
                    intent.depends_on_aliases.join(", ")
                ));
            }
            for criterion in &intent.acceptance_criteria {
                lines.push(format!(
                    "  Criterion {} (required={}): {}",
                    criterion.criterion_alias, criterion.required, criterion.statement
                ));
            }
        }
    }
    if !plan.target_paths.is_empty() {
        lines.push(String::new());
        lines.push("Target paths:".to_owned());
        lines.extend(plan.target_paths.iter().map(|path| format!("- {path}")));
    }
    if !plan.suggested_checks.is_empty() {
        lines.push(String::new());
        lines.push("Suggested checks:".to_owned());
        lines.extend(
            plan.suggested_checks
                .iter()
                .map(|check| format!("- {}", render_plan_check_command(check))),
        );
    }
    if let Some(risk) = &plan.risk {
        lines.push(String::new());
        lines.push(format!("Risk: {risk}"));
    }
    if !plan.notes.is_empty() {
        lines.push(String::new());
        lines.push("Notes:".to_owned());
        lines.extend(plan.notes.iter().map(|note| format!("- {note}")));
    }
    lines.join("\n")
}

fn intent_proposal_from_structured(
    plan: &StructuredPlanDraft,
    plan_hash: &str,
) -> Result<Option<IntentPlanProposalV1>> {
    if plan.intents.is_empty() {
        return Ok(None);
    }
    if plan.schema_version != 2 {
        bail!("intent proposals require sigil-plan-v2");
    }
    let source_turn_id = crate::stable_event_uuid("sigil-plan-intent-source-v1", plan_hash);
    let proposal_id = crate::stable_event_uuid("sigil-plan-intent-proposal-v1", plan_hash);
    IntentPlanProposalV1::new(proposal_id, source_turn_id, plan.intents.clone()).map(Some)
}

fn render_plan_check_command(check: &PlanSuggestedCheck) -> String {
    std::iter::once(check.command.command.as_str())
        .chain(check.command.args.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ")
}

fn nonempty_trimmed(value: impl AsRef<str>) -> Option<String> {
    let value = value.as_ref().trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn plan_id_from_hash(plan_hash: &str) -> Result<PlanId> {
    let digest = plan_hash
        .strip_prefix(PLAN_HASH_PREFIX)
        .unwrap_or(plan_hash)
        .chars()
        .take(16)
        .collect::<String>();
    PlanId::new(format!("plan_{digest}"))
}

fn validate_plan_stable_id(label: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        bail!("{label} cannot be empty");
    }
    if value.len() > 96 {
        bail!("{label} is too long");
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        bail!("{label} contains unsupported characters");
    }
    Ok(())
}

fn plan_path_is_within_scope(path: &str, scope_path: &str) -> bool {
    let path_components = Path::new(path).components().collect::<Vec<_>>();
    let scope_components = Path::new(scope_path).components().collect::<Vec<_>>();
    !scope_components.is_empty()
        && path_components.len() >= scope_components.len()
        && path_components
            .iter()
            .zip(scope_components.iter())
            .all(|(left, right)| left == right)
}

#[cfg(test)]
#[path = "tests/plan_tests.rs"]
mod tests;
