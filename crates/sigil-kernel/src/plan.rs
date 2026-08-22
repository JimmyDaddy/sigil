use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    ConversationRouteDecisionId, ConversationTurnRef, IntentPlanProposalV1, IntentProposalUnitV1,
    PlanReviewId, TaskStepIntentAliasBindingV1,
    session::{ControlEntry, SessionLogEntry},
    task::{
        AgentRole, TASK_STEP_CONTRACT_V2_SCHEMA_VERSION, TaskBlockerV1, TaskCapabilityV2,
        TaskExecutionPhaseV1, TaskGraphProjection, TaskId, TaskIsolationMode, TaskPlanEntry,
        TaskPlanStatus, TaskStepContractBoundEntryV2, TaskStepContractV2, TaskStepId, TaskStepMode,
        TaskStepSpec, task_contract_set_sha256,
    },
    tool::{ToolAccess, ToolCategory, ToolPreviewCapability, ToolSpec},
    verification::{CheckCommand, ToolEffect},
};

/// Stable digest prefix used for approved plan text.
pub const PLAN_HASH_PREFIX: &str = "sha256:";
const PLAN_INLINE_TEXT_MAX_BYTES: usize = 64 * 1024;
const PLAN_SUMMARY_MAX_BYTES: usize = 2 * 1024;

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

/// Complete immutable detail for one structured plan-review step.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
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
#[serde(rename_all = "snake_case", deny_unknown_fields)]
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
#[serde(rename_all = "snake_case", deny_unknown_fields)]
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
    /// Legacy/advisory executable-candidate facts. They never gate Plan review or direct Run.
    pub compile: PlanCompileDetailV1,
}

/// RFC-0067 compile facts exposed to product surfaces.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct PlanCompileDetailV1 {
    pub state: PlanReadyStateV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compiler_version: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<PlanCompileFailureV1>,
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
    let mut attempts = entries.iter().filter_map(|entry| match entry {
        SessionLogEntry::Control(ControlEntry::PlanReviewAttempt(attempt))
            if &attempt.plan_id == plan_id =>
        {
            Some(attempt)
        }
        _ => None,
    });
    let first = attempts.next();
    let source = first
        .map(|attempt| attempt.source)
        .unwrap_or(crate::PlanReviewSource::ExplicitPlanCommand);
    let latest = attempts.next_back().or(first);
    if latest.is_some_and(|attempt| {
        !matches!(
            attempt.status,
            crate::PlanReviewAttemptStatus::DraftReady
                | crate::PlanReviewAttemptStatus::CompileFailed
        )
    }) {
        bail!("plan detail is not bound to a DraftReady or CompileFailed attempt");
    }
    let compile_state = artifacts.plan_ready_state(plan_id);
    let compile = PlanCompileDetailV1 {
        state: compile_state,
        candidate_hash: artifacts
            .latest_candidate(plan_id)
            .map(|candidate| candidate.candidate_hash.clone()),
        compiler_version: artifacts
            .latest_candidate(plan_id)
            .map(|candidate| candidate.compiler_version),
        failure: artifacts
            .compile_failures
            .get(plan_id)
            .and_then(|failures| failures.last())
            .cloned(),
    };
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
        compile,
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

/// Mapping from parsed plan steps to durable task steps.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct PlanToTaskStepMapping {
    pub plan_step_id: String,
    pub task_step_id: TaskStepId,
    pub title: String,
}

/// Append-only record linking one plan artifact to the task created from it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TaskCreatedFromPlanEntry {
    pub plan_id: PlanId,
    pub plan_hash: String,
    pub task_id: TaskId,
    pub task_plan_version: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub step_mapping: Vec<PlanToTaskStepMapping>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_reason: Option<String>,
    pub created_at_ms: u64,
}

/// Durable schema carried by RFC-0067 executable plan candidates.
pub const EXECUTABLE_PLAN_CANDIDATE_SCHEMA_VERSION: u16 = 1;
/// Durable schema carried by RFC-0067 plan compile bindings.
pub const PLAN_COMPILE_BINDING_SCHEMA_VERSION: u16 = 1;
/// Version of the pure plan compiler that produced a candidate.
pub const PLAN_COMPILER_VERSION: u16 = 1;
/// Maximum serialized size of one executable plan candidate.
pub const MAX_EXECUTABLE_PLAN_CANDIDATE_BYTES: usize = 256 * 1024;

/// Provenance binding proving which contract generation compiled a candidate (RFC-0067 7.2).
///
/// These fields are evidence, not runtime permission: they bind the candidate to the exact
/// planner schema, task-contract schema, intent schema and task configuration that produced it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct PlanCompileBindingV1 {
    pub schema_version: u16,
    pub source_attempt_id: String,
    pub source_turn_id: String,
    pub task_config_contract_hash: String,
    pub planner_schema_hash: String,
    pub task_contract_schema_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent_schema_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_workspace_snapshot_id: Option<String>,
}

/// Compile-time input proving which host contracts and bounds produced a candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanCompileInputV1 {
    pub source_attempt_id: String,
    pub source_turn_id: String,
    pub task_config_contract_hash: String,
    pub planner_schema_hash: String,
    pub task_contract_schema_hash: String,
    pub intent_schema_hash: Option<String>,
    pub max_plan_steps: usize,
    pub workspace_id: Option<String>,
    pub session_scope_id: Option<String>,
}

/// Typed compile failure that keeps a plan review from becoming DraftReady (RFC-0067 6.1).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct PlanCompileFailureV1 {
    pub plan_id: PlanId,
    pub plan_hash: String,
    pub reason_code: String,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub affected_step: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compile_binding: Option<PlanCompileBindingV1>,
    pub failed_at_ms: u64,
}

impl PlanCompileFailureV1 {
    /// Validates the compile failure record is bounded and self-consistent.
    ///
    /// # Errors
    ///
    /// Returns an error for unbounded text, a malformed plan hash, or an invalid binding.
    pub fn validate(&self) -> Result<()> {
        for (label, value) in [
            (
                "plan compile failure reason code",
                self.reason_code.as_str(),
            ),
            ("plan compile failure reason", self.reason.as_str()),
        ] {
            if value.is_empty()
                || value.len() > PLAN_COMPILE_FAILURE_TEXT_MAX_BYTES
                || crate::safe_persistence_text(value) != value
            {
                bail!("{label} is not bounded safe text");
            }
        }
        if let Some(step) = self.affected_step.as_deref() {
            validate_plan_stable_id("plan compile failure affected step", step)?;
        }
        if self
            .compile_binding
            .as_ref()
            .is_some_and(|binding| binding.schema_version != PLAN_COMPILE_BINDING_SCHEMA_VERSION)
        {
            bail!("unsupported plan compile binding schema version");
        }
        if self.failed_at_ms == 0 {
            bail!("plan compile failure timestamp must be non-zero");
        }
        Ok(())
    }
}

/// Maximum bytes for one plan compile failure text field.
pub const PLAN_COMPILE_FAILURE_TEXT_MAX_BYTES: usize = 2 * 1024;

/// Prepared, validated, content-addressed Intent admission carried by a candidate (RFC-0067 7.4).
///
/// No acceptance authority is created before the user adopts the candidate. Every field is
/// deterministic so the reducer can materialize the identical admission at adoption time without
/// re-validating anything that could fail.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct PreparedIntentAdmissionV1 {
    pub stack_id: String,
    pub stack_version: u64,
    pub workspace_id: String,
    pub source_session_id: String,
    pub proposal_digest: String,
    pub source_turn_id: String,
    pub authority_event_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alias_bindings: Vec<TaskStepIntentAliasBindingV1>,
    /// Canonical digest of the fully materialized admission (proposal + context + authority).
    pub admission_digest: String,
}

/// Proof that a candidate's paths qualify for a plan-scoped edit grant (RFC-0067 7.5).
///
/// The candidate proves eligibility only; the grant is created by adoption and can still not
/// override sandbox, protected path, network, MCP, external directory, secret egress, merge or
/// publish policy.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct PlanPermissionScopeCandidateV1 {
    pub summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspace_paths: Vec<String>,
    pub scope_digest: String,
}

/// A complete, normalized, content-addressed, adoptable Plan candidate (RFC-0067 7).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ExecutablePlanCandidateV1 {
    pub schema_version: u16,
    pub compiler_version: u16,
    pub plan_id: PlanId,
    pub plan_hash: String,
    pub candidate_hash: String,
    pub task_id: TaskId,
    pub semantic_title: String,
    pub safe_objective: String,
    pub task_plan: TaskPlanEntry,
    pub step_contracts: Vec<TaskStepContractBoundEntryV2>,
    pub contract_set_digest: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub step_mapping: Vec<PlanToTaskStepMapping>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prepared_intent_admission: Option<PreparedIntentAdmissionV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_scope_candidate: Option<PlanPermissionScopeCandidateV1>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_capabilities: Vec<TaskCapabilityV2>,
    pub compile_binding: PlanCompileBindingV1,
}

impl ExecutablePlanCandidateV1 {
    /// Validates the candidate is bounded, deterministic and self-consistent.
    ///
    /// # Errors
    ///
    /// Returns an error for unsupported schemas, unbounded payloads, or facts that cannot be
    /// re-materialized from the candidate itself.
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != EXECUTABLE_PLAN_CANDIDATE_SCHEMA_VERSION {
            bail!("unsupported executable plan candidate schema version");
        }
        if self.compile_binding.schema_version != PLAN_COMPILE_BINDING_SCHEMA_VERSION {
            bail!("unsupported plan compile binding schema version");
        }
        if self.task_plan.plan_version == 0
            || self.task_plan.status != TaskPlanStatus::Accepted
            || self.task_plan.task_id != self.task_id
        {
            bail!("candidate task plan is not an accepted plan for the candidate task");
        }
        if self.step_contracts.len() != self.task_plan.steps.len() {
            bail!("candidate step contract set is incomplete");
        }
        for binding in &self.step_contracts {
            binding.validate()?;
            if binding.task_id != self.task_id
                || binding.plan_version != self.task_plan.plan_version
                || !self
                    .task_plan
                    .steps
                    .iter()
                    .any(|step| step.step_id == binding.step_id)
            {
                bail!("candidate step contract does not belong to the candidate task plan");
            }
        }
        let expected_digest = task_contract_set_sha256(&self.step_contracts)?;
        if expected_digest != self.contract_set_digest {
            bail!("candidate contract-set digest does not match its step contracts");
        }
        let expected_hash = candidate_canonical_hash(self)?;
        if expected_hash != self.candidate_hash {
            bail!("candidate hash does not match its canonical payload");
        }
        let size = serde_json::to_vec(self).context("failed to size executable plan candidate")?;
        if size.len() > MAX_EXECUTABLE_PLAN_CANDIDATE_BYTES {
            bail!(
                "executable plan candidate exceeds maximum of {} bytes",
                MAX_EXECUTABLE_PLAN_CANDIDATE_BYTES
            );
        }
        Ok(())
    }
}

/// Derived plan readiness state for one durable plan artifact (RFC-0067 6.1, 15).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanReadyStateV1 {
    /// No candidate exists yet; the plan review is still open.
    NotReady,
    /// Compile failed with a durable typed reason; the plan needs changes.
    CompileFailed,
    /// Candidate is durable but the final ready marker is missing (crash window).
    CandidatePrepared,
    /// A readable Plan artifact is durable and may be reviewed or directly executed.
    Ready,
    /// Legacy serialized value retained for replay compatibility.
    LegacyPlanNeedsRecompile,
}

impl PlanReadyStateV1 {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotReady => "not_ready",
            Self::CompileFailed => "compile_failed",
            Self::CandidatePrepared => "candidate_prepared",
            Self::Ready => "ready",
            Self::LegacyPlanNeedsRecompile => "legacy_plan_needs_recompile",
        }
    }
}

/// Durable final marker proving a candidate is adoptable (RFC-0067 8).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct PlanReadyCommittedV1Entry {
    pub plan_id: PlanId,
    pub plan_hash: String,
    pub candidate_hash: String,
    pub attempt_id: String,
    pub committed_at_ms: u64,
}

impl PlanReadyCommittedV1Entry {
    /// Validates the ready marker is bounded and self-consistent.
    ///
    /// # Errors
    ///
    /// Returns an error for unbounded identities, a malformed plan/candidate digest, or a
    /// zero commit timestamp.
    pub fn validate(&self) -> Result<()> {
        let plan_digest = self
            .plan_hash
            .strip_prefix(PLAN_HASH_PREFIX)
            .unwrap_or(&self.plan_hash);
        if plan_digest.len() != 64 || !plan_digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("plan ready marker plan hash is not a sha256 digest");
        }
        let candidate_digest = self
            .candidate_hash
            .strip_prefix(PLAN_HASH_PREFIX)
            .unwrap_or(&self.candidate_hash);
        if candidate_digest.len() != 64
            || !candidate_digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            bail!("plan ready marker candidate hash is not a sha256 digest");
        }
        validate_plan_stable_id("plan ready attempt id", &self.attempt_id)?;
        if self.committed_at_ms == 0 {
            bail!("plan ready marker commit timestamp must be non-zero");
        }
        Ok(())
    }
}

/// The single durable authority of one Run action (RFC-0067 9.2).
///
/// This event is the only commit authority for Task identity, accepted plan, step contracts,
/// intent activation, plan decision and the handoff link. Projectors derive every existing public
/// projection from it; no subsequent multi-record promotion is required.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct PlanExecutionAdoptedV1Entry {
    pub command_id: String,
    pub plan_id: PlanId,
    pub plan_hash: String,
    pub candidate_hash: String,
    pub task_id: TaskId,
    pub task_title: String,
    pub parent_session_ref: crate::SessionRef,
    pub start_mode: PlanTaskStartMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_grant: Option<PlanApprovalPermission>,
    pub adopted_candidate: Box<ExecutablePlanCandidateV1>,
    /// RFC-0069 materializer-owned participant continuity receipts. Legacy RFC-0067 adoptions
    /// omit this field and remain replayable through the deterministic compatibility path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_segments: Option<Vec<crate::ExecutionSegmentV1>>,
    pub initial_phase: TaskExecutionPhaseV1,
    pub adopted_at_ms: u64,
}

impl PlanExecutionAdoptedV1Entry {
    /// Validates the adoption event is bounded and self-consistent.
    ///
    /// # Errors
    ///
    /// Returns an error when identities are unbounded, the candidate is invalid, or the event
    /// exceeds the durable record ceiling.
    pub fn validate(&self) -> Result<()> {
        if self.command_id.is_empty()
            || self.command_id.len() > 128
            || crate::safe_persistence_text(&self.command_id) != self.command_id
            || !self.command_id.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':')
            })
        {
            bail!("plan execution command id is not a bounded safe identity");
        }
        if self.plan_id != self.adopted_candidate.plan_id
            || self.plan_hash != self.adopted_candidate.plan_hash
            || self.candidate_hash != self.adopted_candidate.candidate_hash
            || self.task_id != self.adopted_candidate.task_id
            || self.task_title != self.adopted_candidate.semantic_title
        {
            bail!("plan execution adoption event is inconsistent with its candidate");
        }
        if self.initial_phase != TaskExecutionPhaseV1::Preparing {
            bail!("plan execution adoption must start in the Preparing phase");
        }
        if self.permission_grant.is_some()
            && self.adopted_candidate.permission_scope_candidate.is_none()
        {
            bail!(
                "plan execution adoption grants scoped edits without a permission scope candidate"
            );
        }
        self.adopted_candidate.validate()?;
        if let Some(segments) = &self.execution_segments
            && *segments != crate::materialize_execution_segments(&self.adopted_candidate)
        {
            bail!("plan execution segments do not match the materialized candidate authority");
        }
        let size =
            serde_json::to_vec(self).context("failed to size plan execution adoption event")?;
        if size.len() > MAX_EXECUTABLE_PLAN_CANDIDATE_BYTES.saturating_mul(2) {
            bail!("plan execution adoption event exceeds the durable record ceiling");
        }
        Ok(())
    }
}

/// Durable start of one post-approval Task materialization generation (RFC-0069 8.4).
///
/// The stable Task shell exists before this record. A later prepared or blocked outcome must bind
/// this exact generation, so restart recovery never creates a second Task for the same approval.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct TaskMaterializationAttemptStartedV1 {
    pub task_id: TaskId,
    pub generation: u32,
    pub plan_hash: String,
    pub compiler_contract_fingerprint: String,
    pub started_at_ms: u64,
}

impl TaskMaterializationAttemptStartedV1 {
    /// Validates the bounded, durable materialization attempt identity.
    pub fn validate(&self) -> Result<()> {
        if self.generation == 0 {
            bail!("task materialization generation must be non-zero");
        }
        validate_sha256_digest("task materialization plan hash", &self.plan_hash)?;
        validate_sha256_digest(
            "task materialization compiler contract fingerprint",
            &self.compiler_contract_fingerprint,
        )?;
        if self.started_at_ms == 0 {
            bail!("task materialization start timestamp must be non-zero");
        }
        Ok(())
    }
}

/// Durable blocked outcome for one exact materialization attempt (RFC-0069 8.4).
///
/// This is intentionally Task-local. It never revokes the approved Plan or the stable Task
/// identity; a later generation may safely retry after a revision or environment repair.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct TaskMaterializationBlockedV1 {
    pub task_id: TaskId,
    pub generation: u32,
    pub plan_hash: String,
    pub blocker_id: String,
    pub blocker: TaskBlockerV1,
    pub blocked_at_ms: u64,
}

impl TaskMaterializationBlockedV1 {
    /// Validates that the blocked outcome binds one exact started materialization generation.
    pub fn validate(&self) -> Result<()> {
        if self.generation == 0 {
            bail!("task materialization blocker generation must be non-zero");
        }
        validate_sha256_digest("task materialization blocker plan hash", &self.plan_hash)?;
        validate_plan_stable_id("task materialization blocker id", &self.blocker_id)?;
        self.blocker.validate()?;
        if self.blocked_at_ms < self.blocker.created_at_ms || self.blocked_at_ms == 0 {
            bail!("task materialization blocker timestamp is invalid");
        }
        Ok(())
    }
}

/// Typed Run command shared by every product surface (RFC-0067 9.1).
///
/// `source` is audit-only and never changes domain behavior. `expected_durable_frontier` is the
/// compare-and-swap position the approval append must still observe. `expected_candidate_hash`
/// is retained for RFC-0067 replay compatibility only: RFC-0069 approval binds the readable
/// Plan hash and creates a Task shell before authoritative materialization, so new writers must
/// leave it empty and must not use it to authorize approval.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct PlanRunCommandV1 {
    pub command_id: String,
    pub session_id: String,
    pub plan_id: PlanId,
    pub expected_plan_hash: String,
    pub expected_candidate_hash: String,
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

/// Durable receipt returned after one Run command (RFC-0067 9.2).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct PlanRunReceiptV1 {
    pub command_id: String,
    pub receipt_id: String,
    pub plan_id: PlanId,
    pub plan_hash: String,
    pub candidate_hash: String,
    pub task_id: TaskId,
    pub task_title: String,
    pub initial_phase: TaskExecutionPhaseV1,
    pub accepted_at_ms: u64,
    pub already_adopted: bool,
}

/// Typed rejection of one Run command; the plan stays actionable (RFC-0067 9.3).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum PlanRunRejectionV1 {
    PlanMissing,
    PlanHashStale { expected: String, current: String },
    PlanNotReady { plan_state: PlanReadyStateV1 },
    PlanRejected,
    CandidateMissing,
    CandidateHashMismatch { expected: String, current: String },
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
            Self::PlanNotReady { .. } => "plan_not_ready",
            Self::PlanRejected => "plan_rejected",
            Self::CandidateMissing => "candidate_missing",
            Self::CandidateHashMismatch { .. } => "candidate_hash_mismatch",
            Self::FrontierStale { .. } => "frontier_stale",
            Self::CommandIdentityConflict => "command_identity_conflict",
            Self::PermissionChoiceUnavailable { .. } => "permission_choice_unavailable",
            Self::SessionWriterUnavailable { .. } => "session_writer_unavailable",
        }
    }
}

/// Computes the canonical, provider-neutral hash of an executable plan candidate (RFC-0067 7.1).
///
/// The hash excludes timestamps, command/request UUIDs, process-local grants, current provider
/// credentials, registry instance identity, volatile workspace availability and UI selection.
///
/// # Errors
///
/// Returns an error when the candidate cannot be serialized canonically.
pub fn candidate_canonical_hash(candidate: &ExecutablePlanCandidateV1) -> Result<String> {
    let mut value =
        serde_json::to_value(candidate).context("failed to serialize executable plan candidate")?;
    let object = value
        .as_object_mut()
        .context("executable plan candidate must serialize as an object")?;
    object.remove("candidate_hash");
    let canonical = crate::event::canonical_json_bytes(&value)
        .context("failed to encode executable plan candidate canonically")?;
    Ok(format!("sha256:{}", crate::sha256_hex(&canonical)))
}

/// Compiles one validated plan draft into a complete executable candidate (RFC-0067 7.3).
///
/// The compiler is pure and deterministic: it never reads the workspace, provider, tool registry,
/// credentials or disk. Every failure is a typed [`PlanCompileFailureV1`]. Compilation is a
/// preflight cache before approval and an authoritative materialization step after approval; a
/// readable plan remains reviewable even when this compiler reports a blocker.
pub fn compile_executable_plan_candidate(
    draft: &PlanDraftCreatedEntry,
    input: &PlanCompileInputV1,
) -> Result<ExecutablePlanCandidateV1, Box<PlanCompileFailureV1>> {
    compile_executable_plan_candidate_impl(draft, input, 0)
}

fn compile_executable_plan_candidate_impl(
    draft: &PlanDraftCreatedEntry,
    input: &PlanCompileInputV1,
    failed_at_ms: u64,
) -> Result<ExecutablePlanCandidateV1, Box<PlanCompileFailureV1>> {
    let failure = |reason_code: &str, reason: String, affected_step: Option<String>| {
        Box::new(PlanCompileFailureV1 {
            plan_id: draft.plan_id.clone(),
            plan_hash: draft.plan_hash.clone(),
            reason_code: reason_code.to_owned(),
            reason,
            affected_step,
            compile_binding: Some(compile_binding_from_input(draft, input)),
            failed_at_ms,
        })
    };
    if draft.schema_version != 2 {
        return Err(failure(
            "unsupported_schema",
            format!(
                "unsupported executable plan draft schema version {}",
                draft.schema_version
            ),
            None,
        ));
    }
    if draft.steps.is_empty() {
        return Err(failure(
            "no_executable_steps",
            "plan draft has no executable steps".to_owned(),
            None,
        ));
    }
    if draft.steps.len() > input.max_plan_steps {
        return Err(failure(
            "step_limit_exceeded",
            format!(
                "plan has {} steps, exceeding task.max_plan_steps={}",
                draft.steps.len(),
                input.max_plan_steps
            ),
            None,
        ));
    }
    if let Some(step) = draft
        .steps
        .iter()
        .find(|step| step.mode == Some(TaskStepMode::Verify))
    {
        return Err(failure(
            "verify_step_forbidden",
            "plan draft cannot compile verify participant steps; trusted verification is system-owned"
                .to_owned(),
            Some(step.step_id.clone()),
        ));
    }
    if let Some(step) = draft
        .steps
        .iter()
        .find(|step| step.role.is_none() || step.mode.is_none() || step.isolation.is_none())
    {
        return Err(failure(
            "incomplete_step_contract",
            "plan step is missing its role, mode or isolation contract".to_owned(),
            Some(step.step_id.clone()),
        ));
    }
    if let Some(proposal) = draft.intent_proposal.as_ref() {
        let known_aliases = proposal
            .intents
            .iter()
            .map(|intent| intent.intent_alias.as_str())
            .collect::<BTreeSet<_>>();
        for step in &draft.steps {
            let aliases = materialization_intent_aliases_for_step(draft, step);
            let unique_aliases = aliases.iter().collect::<BTreeSet<_>>();
            if unique_aliases.len() != aliases.len()
                || aliases
                    .iter()
                    .any(|alias| !known_aliases.contains(alias.as_str()))
            {
                return Err(failure(
                    "intent_alias_binding_invalid",
                    "plan step intent bindings are duplicated or reference an unknown proposal alias"
                        .to_owned(),
                    Some(step.step_id.clone()),
                ));
            }
            if step.mode == Some(TaskStepMode::Write) && aliases.len() != 1 {
                return Err(failure(
                    "intent_alias_binding_incomplete",
                    "intent-enabled write task materialization requires exactly one intent alias"
                        .to_owned(),
                    Some(step.step_id.clone()),
                ));
            }
        }
    } else if let Some(step) = draft
        .steps
        .iter()
        .find(|step| !step.intent_aliases.is_empty())
    {
        return Err(failure(
            "intent_aliases_without_proposal",
            "plan step intent aliases require a top-level intent proposal".to_owned(),
            Some(step.step_id.clone()),
        ));
    }
    if let Some(step) = draft.steps.iter().find(|step| {
        step.required_capabilities
            .contains(&TaskCapabilityV2::VerificationRun)
    }) {
        return Err(failure(
            "verification_run_forbidden",
            "plan draft cannot delegate verification_run; add trusted checks to suggested_checks"
                .to_owned(),
            Some(step.step_id.clone()),
        ));
    }
    let task_id = task_id_from_plan_draft(draft)
        .map_err(|error| failure("task_identity_unavailable", format!("{error:#}"), None))?;
    let semantic_title = crate::task_semantic_title(&draft.summary);
    let safe_objective = crate::safe_persistence_text(&plan_task_input_from_draft(draft));
    let steps = draft
        .steps
        .iter()
        .map(|step| {
            let step_id = TaskStepId::new(step.step_id.clone()).map_err(|error| {
                failure(
                    "invalid_step_identity",
                    format!("{error:#}"),
                    Some(step.step_id.clone()),
                )
            })?;
            Ok(TaskStepSpec {
                step_id,
                title: crate::safe_persistence_text(&step.title),
                display_name: bounded_plan_step_display_name(step.display_name.as_deref())
                    .map_err(|error| {
                        failure(
                            "invalid_display_name",
                            format!("{error:#}"),
                            Some(step.step_id.clone()),
                        )
                    })?,
                detail: step.detail.as_deref().map(crate::safe_persistence_text),
                role: step.role.expect("executable schema was checked"),
                depends_on: step
                    .depends_on
                    .iter()
                    .cloned()
                    .map(TaskStepId::new)
                    .collect::<Result<Vec<_>>>()
                    .map_err(|error| {
                        failure(
                            "invalid_dependency",
                            format!("{error:#}"),
                            Some(step.step_id.clone()),
                        )
                    })?,
                intent_refs: Vec::new(),
                mode: step.mode,
                isolation: step.isolation,
            })
        })
        .collect::<std::result::Result<Vec<_>, Box<PlanCompileFailureV1>>>()?;
    let mut task_plan = TaskPlanEntry {
        task_id: task_id.clone(),
        plan_version: 1,
        status: TaskPlanStatus::Accepted,
        steps,
        reason: Some(format!(
            "compiled from approved plan {}",
            draft.plan_id.as_str()
        )),
    };
    TaskGraphProjection::from_plan_entry(&task_plan)
        .map_err(|error| failure("invalid_task_graph", format!("{error:#}"), None))?;
    let step_contracts = draft
        .steps
        .iter()
        .zip(task_plan.steps.iter())
        .map(|(draft_step, task_step)| {
            let mut required_capabilities =
                match (task_step.effective_mode(), task_step.effective_isolation()) {
                    (TaskStepMode::Write, TaskIsolationMode::ChangesetOnly) => {
                        vec![TaskCapabilityV2::WorkspaceRead]
                    }
                    (TaskStepMode::Write, _) => vec![
                        TaskCapabilityV2::WorkspaceRead,
                        TaskCapabilityV2::WorkspaceWrite,
                    ],
                    (TaskStepMode::Read | TaskStepMode::Review, _) => {
                        vec![TaskCapabilityV2::WorkspaceRead]
                    }
                    (TaskStepMode::Verify, _) => unreachable!("verify steps were rejected"),
                }
                .into_iter()
                .collect::<BTreeSet<_>>();
            required_capabilities.extend(draft_step.required_capabilities.iter().copied());
            let contract = TaskStepContractV2 {
                schema_version: TASK_STEP_CONTRACT_V2_SCHEMA_VERSION,
                target_paths: draft_step.target_paths.clone(),
                required_capabilities: required_capabilities.into_iter().collect(),
                deliverables: draft_step.deliverables.clone(),
                acceptance_criteria: draft_step.acceptance_criteria.clone(),
                check_spec_refs: draft_step
                    .suggested_checks
                    .iter()
                    .map(|check| check.check_spec_id.clone())
                    .collect(),
                risk: draft_step.risk.clone(),
                notes: draft_step.notes.clone(),
            };
            contract.validate().map_err(|error| {
                failure(
                    "invalid_step_contract",
                    format!("{error:#}"),
                    Some(task_step.step_id.as_str().to_owned()),
                )
            })?;
            Ok(TaskStepContractBoundEntryV2 {
                task_id: task_plan.task_id.clone(),
                plan_version: task_plan.plan_version,
                step_id: task_step.step_id.clone(),
                contract,
            })
        })
        .collect::<std::result::Result<Vec<_>, Box<PlanCompileFailureV1>>>()?;
    let contract_set_digest = task_contract_set_sha256(&step_contracts)
        .map_err(|error| failure("contract_digest_unavailable", format!("{error:#}"), None))?;
    let step_mapping = task_plan
        .steps
        .iter()
        .map(|step| PlanToTaskStepMapping {
            plan_step_id: step.step_id.as_str().to_owned(),
            task_step_id: step.step_id.clone(),
            title: step.title.clone(),
        })
        .collect();
    let alias_bindings = draft
        .steps
        .iter()
        .filter_map(|step| {
            let intent_aliases = materialization_intent_aliases_for_step(draft, step);
            (!intent_aliases.is_empty()).then_some((step, intent_aliases))
        })
        .map(|(step, intent_aliases)| {
            Ok(TaskStepIntentAliasBindingV1 {
                step_id: TaskStepId::new(step.step_id.clone()).map_err(|error| {
                    failure(
                        "invalid_step_identity",
                        format!("{error:#}"),
                        Some(step.step_id.clone()),
                    )
                })?,
                intent_aliases,
            })
        })
        .collect::<std::result::Result<Vec<_>, Box<PlanCompileFailureV1>>>()?;
    let prepared_intent_admission =
        prepare_intent_admission(draft, &task_id, input, &alias_bindings)
            .map_err(|error| failure("intent_preparation_failed", format!("{error:#}"), None))?;
    if let Some(prepared) = prepared_intent_admission.as_ref() {
        if alias_bindings.is_empty() {
            return Err(failure(
                "intent_aliases_missing",
                "intent-enabled plan must bind aliases to task steps".to_owned(),
                None,
            ));
        }
        // Prove the exact admission materializes without failure before it becomes a candidate.
        materialize_prepared_intent_admission(draft, prepared).map_err(|error| {
            failure("intent_materialization_failed", format!("{error:#}"), None)
        })?;
        let bound_plan = bind_candidate_plan_intents(draft, task_plan, prepared)
            .map_err(|error| failure("intent_binding_failed", format!("{error:#}"), None))?;
        TaskGraphProjection::from_plan_entry(&bound_plan)
            .map_err(|error| failure("invalid_task_graph", format!("{error:#}"), None))?;
        task_plan = bound_plan;
    } else if !alias_bindings.is_empty() {
        return Err(failure(
            "intent_aliases_without_proposal",
            "plan step intent aliases require a top-level intent proposal".to_owned(),
            alias_bindings
                .first()
                .map(|binding| binding.step_id.as_str().to_owned()),
        ));
    }
    let permission_scope_candidate = (!draft.target_paths.is_empty()).then(|| {
        let scope = PlanApprovalScope {
            summary: format!("scoped edits for task {}", task_id.as_str()),
            workspace_paths: draft.target_paths.clone(),
        };
        PlanPermissionScopeCandidateV1 {
            scope_digest: crate::stable_event_hash(
                serde_json::to_string(&scope).unwrap_or_default().as_bytes(),
            ),
            summary: scope.summary,
            workspace_paths: scope.workspace_paths,
        }
    });
    let mut required_capabilities = BTreeSet::new();
    for contract in &step_contracts {
        required_capabilities.extend(contract.contract.required_capabilities.iter().copied());
    }
    let mut candidate = ExecutablePlanCandidateV1 {
        schema_version: EXECUTABLE_PLAN_CANDIDATE_SCHEMA_VERSION,
        compiler_version: PLAN_COMPILER_VERSION,
        plan_id: draft.plan_id.clone(),
        plan_hash: draft.plan_hash.clone(),
        candidate_hash: String::new(),
        task_id,
        semantic_title,
        safe_objective,
        task_plan,
        step_contracts,
        contract_set_digest,
        step_mapping,
        prepared_intent_admission,
        permission_scope_candidate,
        required_capabilities: required_capabilities.into_iter().collect(),
        compile_binding: compile_binding_from_input(draft, input),
    };
    candidate.candidate_hash = candidate_canonical_hash(&candidate)
        .map_err(|error| failure("canonical_hash_unavailable", format!("{error:#}"), None))?;
    candidate
        .validate()
        .map_err(|error| failure("candidate_self_check_failed", format!("{error:#}"), None))?;
    Ok(candidate)
}

fn compile_binding_from_input(
    draft: &PlanDraftCreatedEntry,
    input: &PlanCompileInputV1,
) -> PlanCompileBindingV1 {
    PlanCompileBindingV1 {
        schema_version: PLAN_COMPILE_BINDING_SCHEMA_VERSION,
        source_attempt_id: input.source_attempt_id.clone(),
        source_turn_id: input.source_turn_id.clone(),
        task_config_contract_hash: input.task_config_contract_hash.clone(),
        planner_schema_hash: input.planner_schema_hash.clone(),
        task_contract_schema_hash: input.task_contract_schema_hash.clone(),
        intent_schema_hash: input.intent_schema_hash.clone(),
        base_workspace_snapshot_id: draft.workspace_snapshot_id.clone(),
    }
}

fn prepare_intent_admission(
    draft: &PlanDraftCreatedEntry,
    task_id: &TaskId,
    input: &PlanCompileInputV1,
    alias_bindings: &[TaskStepIntentAliasBindingV1],
) -> Result<Option<PreparedIntentAdmissionV1>> {
    let Some(proposal) = draft.intent_proposal.as_ref() else {
        return Ok(None);
    };
    if alias_bindings.is_empty() {
        bail!("intent proposal without any step intent alias binding");
    }
    let workspace_id = input.workspace_id.clone().ok_or_else(|| {
        anyhow::anyhow!("intent-enabled plan compile requires a stable workspace id")
    })?;
    let session_scope_id = input.session_scope_id.clone().ok_or_else(|| {
        anyhow::anyhow!("intent-enabled plan compile requires the session scope id")
    })?;
    let stack_id = crate::IntentStackId::new(crate::stable_event_uuid(
        "sigil-plan-intent-stack-v1",
        &format!("{}:{}", draft.plan_id.as_str(), draft.plan_hash),
    ))?;
    let context =
        crate::IntentAdmissionContextV1::initial(stack_id, workspace_id, session_scope_id)?;
    let authority_event_id = crate::stable_event_uuid(
        "sigil-plan-intent-acceptance-v1",
        &format!(
            "{}:{}:{}",
            draft.plan_id.as_str(),
            draft.plan_hash,
            task_id.as_str()
        ),
    );
    let authority = crate::IntentAcceptanceAuthorityV1::explicit_user_confirmation(
        proposal.source_turn_id.clone(),
        authority_event_id.clone(),
        proposal.proposal_digest.clone(),
    )?;
    let admission = crate::admit_suggested_decomposition(&context, proposal, &authority)?;
    let admission_digest = intent_admission_digest(&admission)?;
    Ok(Some(PreparedIntentAdmissionV1 {
        stack_id: context.stack_id.as_str().to_owned(),
        stack_version: context.stack_version.get(),
        workspace_id: context.workspace_id,
        source_session_id: context.source_session_id,
        proposal_digest: proposal.proposal_digest.as_str().to_owned(),
        source_turn_id: proposal.source_turn_id.clone(),
        authority_event_id,
        alias_bindings: alias_bindings.to_vec(),
        admission_digest,
    }))
}

/// Returns the effective intent aliases for one materialized Task step.
///
/// A Plan's single top-level intent unambiguously owns every write step unless the planner
/// supplied a more specific binding. This is a host-owned normalization, not an inference from
/// prose: the plan has exactly one durable, user-approved intent proposal, and the compiler
/// records the resulting binding in the candidate receipt. Multiple proposals remain explicit so
/// the model or user cannot accidentally collapse distinct work into one intent.
fn materialization_intent_aliases_for_step(
    draft: &PlanDraftCreatedEntry,
    step: &PlanDraftStep,
) -> Vec<String> {
    if step.mode == Some(TaskStepMode::Write)
        && step.intent_aliases.is_empty()
        && let Some(proposal) = draft
            .intent_proposal
            .as_ref()
            .filter(|proposal| proposal.intents.len() == 1)
    {
        return vec![proposal.intents[0].intent_alias.clone()];
    }
    step.intent_aliases.clone()
}

fn intent_admission_digest(admission: &crate::IntentPlanAdmissionV1) -> Result<String> {
    let events = admission.durable_events(None)?;
    let mut canonical = events
        .into_iter()
        .map(|(event_type, _, payload)| (event_type.as_str(), payload))
        .collect::<Vec<_>>();
    canonical.sort_by(|left, right| left.0.cmp(right.0));
    let bytes = serde_json::to_vec(&canonical).context("failed to size intent admission")?;
    Ok(crate::stable_event_hash(&bytes))
}

/// Materializes the identical IntentPlan admission an adoption will activate (RFC-0067 7.4).
///
/// Pure and deterministic: it never appends, validates nothing new, and therefore cannot fail at
/// adoption time when the candidate compiled successfully.
pub fn materialize_prepared_intent_admission(
    draft: &PlanDraftCreatedEntry,
    prepared: &PreparedIntentAdmissionV1,
) -> Result<crate::IntentPlanAdmissionV1> {
    let proposal = draft
        .intent_proposal
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("prepared intent admission is missing its proposal"))?;
    if proposal.proposal_digest.as_str() != prepared.proposal_digest
        || proposal.source_turn_id != prepared.source_turn_id
    {
        bail!("prepared intent admission does not bind the exact proposal");
    }
    let stack_id = crate::IntentStackId::new(prepared.stack_id.clone())?;
    let context = crate::IntentAdmissionContextV1::initial(
        stack_id,
        prepared.workspace_id.clone(),
        prepared.source_session_id.clone(),
    )?;
    if context.stack_version.get() != prepared.stack_version {
        bail!("prepared intent admission stack version drifted");
    }
    let authority = crate::IntentAcceptanceAuthorityV1::explicit_user_confirmation(
        prepared.source_turn_id.clone(),
        prepared.authority_event_id.clone(),
        proposal.proposal_digest.clone(),
    )?;
    let admission = crate::admit_suggested_decomposition(&context, proposal, &authority)?;
    if intent_admission_digest(&admission)? != prepared.admission_digest {
        bail!("prepared intent admission digest drifted during materialization");
    }
    Ok(admission)
}

/// Binds the prepared intent refs onto the candidate task plan (RFC-0067 7.4).
///
/// The result is the exact accepted TaskPlan the adoption reducer projects.
pub fn bind_candidate_plan_intents(
    draft: &PlanDraftCreatedEntry,
    task_plan: TaskPlanEntry,
    prepared: &PreparedIntentAdmissionV1,
) -> Result<TaskPlanEntry> {
    let admission = materialize_prepared_intent_admission(draft, prepared)?;
    crate::bind_task_plan_intents(&admission, task_plan, &prepared.alias_bindings)
}

/// Outcome of one atomic adoption append (RFC-0067 9.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanExecutionAdoptionCommit {
    /// The adoption event (and its intent activation) was appended and synced.
    Appended,
    /// The frontier moved or the same command/candidate was already adopted; nothing changed.
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
) -> Result<PlanExecutionAdoptionCommit> {
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
        || task_link.task_plan_version != 0
        || !task_link.step_mapping.is_empty()
        || task_link.stale_reason.is_some()
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
                    && existing_link.task_plan_version == 0
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
        Ok(PlanExecutionAdoptionCommit::Appended)
    } else {
        Ok(PlanExecutionAdoptionCommit::CasSkipped)
    }
}

/// Appends the single `PlanExecutionAdoptedV1` authority with its intent activation in one
/// crash-safe, compare-and-swap append (RFC-0067 6.2, 9.2).
///
/// The intent durable events and the adoption session entry are committed atomically, so a crash
/// at any boundary either leaves the old frontier or the complete adoption. When the expected
/// frontier is stale, or the same `command_id` / `candidate_hash` is already durable, nothing is
/// appended and the caller reads the idempotent receipt from the projection.
///
/// # Errors
///
/// Returns an error when the adoption event is invalid, the intent activation cannot be
/// materialized, or the durable writer cannot append/sync.
pub fn append_plan_execution_adoption_at_frontier(
    session: &mut crate::Session,
    adoption: &PlanExecutionAdoptedV1Entry,
    expected_frontier: u64,
) -> Result<PlanExecutionAdoptionCommit> {
    adoption.validate()?;
    let store = session
        .durable_store()
        .context("plan execution adoption requires a durable session store")?;
    let mut durable_events = Vec::new();
    if let Some(prepared) = adoption
        .adopted_candidate
        .prepared_intent_admission
        .as_ref()
    {
        let draft = session
            .plan_artifact_projection()
            .plans
            .get(&adoption.plan_id)
            .cloned()
            .context("adopted candidate is missing its durable plan draft")?;
        let admission = materialize_prepared_intent_admission(&draft, prepared)?;
        durable_events = admission.durable_events(Some(crate::IntentTaskPlanBindingV1 {
            task_id: adoption.task_id.as_str().to_owned(),
            task_plan_version: adoption.adopted_candidate.task_plan.plan_version,
        }))?;
    }
    let predicate_adoption = adoption.clone();
    let appended = store
        .append_events_and_session_entries_if(
            durable_events,
            &[SessionLogEntry::Control(
                ControlEntry::PlanExecutionAdoptedV1(Box::new(adoption.clone())),
            )],
            move |records| {
                let current = records.last().map_or(0, |record| record.stream_sequence());
                if current != expected_frontier {
                    return Ok(false);
                }
                let mut projection = PlanArtifactProjection::default();
                for record in records {
                    let Some(entry) = record.session_log_entry()? else {
                        continue;
                    };
                    if let SessionLogEntry::Control(control) = &entry {
                        projection.apply_control_entry(control);
                    }
                }
                if projection
                    .adoption_for_command(&predicate_adoption.command_id)
                    .is_some()
                    || projection.adoptions.values().flatten().any(|existing| {
                        existing.candidate_hash == predicate_adoption.candidate_hash
                    })
                {
                    return Ok(false);
                }
                Ok(true)
            },
        )?
        .is_some();
    if appended {
        session.record_durably_appended_control(ControlEntry::PlanExecutionAdoptedV1(Box::new(
            adoption.clone(),
        )));
        Ok(PlanExecutionAdoptionCommit::Appended)
    } else {
        Ok(PlanExecutionAdoptionCommit::CasSkipped)
    }
}

/// Appends RFC-0069 Task materialization after the approval/task-shell bundle is already durable.
///
/// The payload deliberately reuses the V1 candidate envelope for replay compatibility, but this
/// event does not synthesize an Accepted decision or a Task shell. New product writers must use
/// this function; `append_plan_execution_adoption_at_frontier` is retained only for legacy replay
/// and compatibility tests.
pub fn append_task_materialization_prepared_at_frontier(
    session: &mut crate::Session,
    materialization: &PlanExecutionAdoptedV1Entry,
    expected_frontier: u64,
) -> Result<PlanExecutionAdoptionCommit> {
    materialization.validate()?;
    let store = session
        .durable_store()
        .context("task materialization requires a durable session store")?;
    let mut durable_events = Vec::new();
    if let Some(prepared) = materialization
        .adopted_candidate
        .prepared_intent_admission
        .as_ref()
    {
        let draft = session
            .plan_artifact_projection()
            .plans
            .get(&materialization.plan_id)
            .cloned()
            .context("materialized candidate is missing its durable plan draft")?;
        let admission = materialize_prepared_intent_admission(&draft, prepared)?;
        durable_events = admission.durable_events(Some(crate::IntentTaskPlanBindingV1 {
            task_id: materialization.task_id.as_str().to_owned(),
            task_plan_version: materialization.adopted_candidate.task_plan.plan_version,
        }))?;
    }
    let expected = materialization.clone();
    let appended = store
        .append_events_and_session_entries_if(
            durable_events,
            &[SessionLogEntry::Control(
                ControlEntry::TaskMaterializationPreparedV1(Box::new(materialization.clone())),
            )],
            move |records| {
                let current = records.last().map_or(0, |record| record.stream_sequence());
                if current != expected_frontier {
                    return Ok(false);
                }
                let mut projection = PlanArtifactProjection::default();
                for record in records {
                    let Some(entry) = record.session_log_entry()? else {
                        continue;
                    };
                    if let SessionLogEntry::Control(control) = &entry {
                        projection.apply_control_entry(control);
                    }
                }
                match projection.materialization_for_task(&expected.task_id) {
                    None => Ok(true),
                    Some(existing)
                        if existing.plan_id == expected.plan_id
                            && existing.plan_hash == expected.plan_hash
                            && existing.candidate_hash == expected.candidate_hash =>
                    {
                        Ok(false)
                    }
                    Some(_) => bail!("task materialization conflicts with durable task state"),
                }
            },
        )?
        .is_some();
    if appended {
        session.record_durably_appended_control(ControlEntry::TaskMaterializationPreparedV1(
            Box::new(materialization.clone()),
        ));
        Ok(PlanExecutionAdoptionCommit::Appended)
    } else {
        Ok(PlanExecutionAdoptionCommit::CasSkipped)
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
    /// Latest durable executable candidate per plan (RFC-0067).
    pub candidates: BTreeMap<PlanId, ExecutablePlanCandidateV1>,
    /// Final ready markers per plan (RFC-0067 8.1).
    pub ready_markers: BTreeMap<PlanId, PlanReadyCommittedV1Entry>,
    /// Typed compile failures per plan (RFC-0067 6.1).
    pub compile_failures: BTreeMap<PlanId, Vec<PlanCompileFailureV1>>,
    /// Adoptions per plan in commit order (RFC-0067 9.2).
    pub adoptions: BTreeMap<PlanId, Vec<PlanExecutionAdoptedV1Entry>>,
    /// Adoption receipts keyed by command id for idempotent read-back.
    pub adoption_receipts: BTreeMap<String, PlanExecutionAdoptedV1Entry>,
    /// RFC-0069 materialization receipts keyed by Task identity. New writers append these only
    /// after the Plan approval and stable Task shell are durable.
    pub materializations: BTreeMap<TaskId, PlanExecutionAdoptedV1Entry>,
    /// Materialization attempts by Task, retained so retries increment their durable generation.
    pub materialization_attempts: BTreeMap<TaskId, Vec<TaskMaterializationAttemptStartedV1>>,
    /// Latest unresolved materialization blocker per Task. A prepared receipt clears it.
    pub materialization_blockers: BTreeMap<TaskId, TaskMaterializationBlockedV1>,
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
            ControlEntry::ExecutablePlanCandidatePreparedV1(candidate) => {
                self.candidates
                    .insert(candidate.plan_id.clone(), (**candidate).clone());
            }
            ControlEntry::PlanReadyCommittedV1(marker) => {
                self.ready_markers
                    .insert(marker.plan_id.clone(), marker.clone());
            }
            ControlEntry::PlanCompileFailedV1(failure) => {
                self.compile_failures
                    .entry(failure.plan_id.clone())
                    .or_default()
                    .push(failure.clone());
            }
            ControlEntry::PlanExecutionAdoptedV1(adoption) => {
                self.apply_adoption(adoption);
            }
            ControlEntry::TaskMaterializationAttemptStartedV1(attempt) => {
                self.apply_materialization_attempt(attempt);
            }
            ControlEntry::TaskMaterializationPreparedV1(materialization) => {
                self.apply_materialization(materialization);
            }
            ControlEntry::TaskMaterializationBlockedV1(blocked) => {
                self.apply_materialization_blocked(blocked);
            }
            _ => {}
        }
    }

    fn apply_adoption(&mut self, adoption: &PlanExecutionAdoptedV1Entry) {
        self.adoptions
            .entry(adoption.plan_id.clone())
            .or_default()
            .push(adoption.clone());
        self.adoption_receipts
            .insert(adoption.command_id.clone(), adoption.clone());
        // The adoption event is the single authority for the Accepted decision and the
        // Task-created link; existing projections derive both from it without extra records.
        self.decisions
            .entry(adoption.plan_id.clone())
            .or_default()
            .push(PlanDecisionRecordedEntry {
                plan_id: adoption.plan_id.clone(),
                plan_hash: adoption.plan_hash.clone(),
                decision: PlanDecision::Accepted,
                decided_by: PlanDecisionActor::User,
                decided_at_ms: adoption.adopted_at_ms,
                reason: Some("adopted through the single execution spine".to_owned()),
            });
        self.tasks_created
            .entry(adoption.plan_id.clone())
            .or_default()
            .push(TaskCreatedFromPlanEntry {
                plan_id: adoption.plan_id.clone(),
                plan_hash: adoption.plan_hash.clone(),
                task_id: adoption.task_id.clone(),
                task_plan_version: adoption.adopted_candidate.task_plan.plan_version,
                step_mapping: adoption.adopted_candidate.step_mapping.clone(),
                stale_reason: None,
                created_at_ms: adoption.adopted_at_ms,
            });
    }

    fn apply_materialization(&mut self, materialization: &PlanExecutionAdoptedV1Entry) {
        self.materializations
            .insert(materialization.task_id.clone(), materialization.clone());
        self.materialization_blockers
            .remove(&materialization.task_id);
    }

    fn apply_materialization_attempt(&mut self, attempt: &TaskMaterializationAttemptStartedV1) {
        if attempt.validate().is_err() {
            return;
        }
        let attempts = self
            .materialization_attempts
            .entry(attempt.task_id.clone())
            .or_default();
        if attempts
            .iter()
            .any(|existing| existing.generation == attempt.generation)
        {
            return;
        }
        attempts.push(attempt.clone());
    }

    fn apply_materialization_blocked(&mut self, blocked: &TaskMaterializationBlockedV1) {
        if blocked.validate().is_err() {
            return;
        }
        let Some(attempt) = self
            .materialization_attempts
            .get(&blocked.task_id)
            .and_then(|attempts| {
                attempts
                    .iter()
                    .find(|attempt| attempt.generation == blocked.generation)
            })
        else {
            return;
        };
        if attempt.plan_hash != blocked.plan_hash {
            return;
        }
        self.materialization_blockers
            .insert(blocked.task_id.clone(), blocked.clone());
    }

    /// Returns the latest candidate for one plan, if durable.
    pub fn latest_candidate(&self, plan_id: &PlanId) -> Option<&ExecutablePlanCandidateV1> {
        self.candidates.get(plan_id)
    }

    /// Returns the adoption receipt for one exact command id.
    pub fn adoption_for_command(&self, command_id: &str) -> Option<&PlanExecutionAdoptedV1Entry> {
        self.adoption_receipts.get(command_id)
    }

    /// Returns the adoption that created one Task, if any.
    pub fn adoption_for_task(&self, task_id: &TaskId) -> Option<&PlanExecutionAdoptedV1Entry> {
        self.adoptions
            .values()
            .flatten()
            .find(|adoption| &adoption.task_id == task_id)
    }

    /// Returns the post-approval materialization receipt for one stable Task shell.
    #[must_use]
    pub fn materialization_for_task(
        &self,
        task_id: &TaskId,
    ) -> Option<&PlanExecutionAdoptedV1Entry> {
        self.materializations.get(task_id)
    }

    /// Returns the next durable materialization generation for an approved Task shell.
    #[must_use]
    pub fn next_materialization_generation(&self, task_id: &TaskId) -> u32 {
        self.materialization_attempts
            .get(task_id)
            .and_then(|attempts| attempts.iter().map(|attempt| attempt.generation).max())
            .unwrap_or(0)
            .saturating_add(1)
    }

    /// Returns the latest unresolved materialization blocker for one Task.
    #[must_use]
    pub fn materialization_blocker_for_task(
        &self,
        task_id: &TaskId,
    ) -> Option<&TaskMaterializationBlockedV1> {
        self.materialization_blockers.get(task_id)
    }

    /// Derives the review readiness state of one Plan from durable facts only.
    pub fn plan_ready_state(&self, plan_id: &PlanId) -> PlanReadyStateV1 {
        // RFC-0069: a readable durable Plan is reviewable and directly executable. Candidate,
        // compiler, intent, and DAG facts are legacy advisory evidence only.
        if self.plans.contains_key(plan_id) {
            return PlanReadyStateV1::Ready;
        }
        if self.ready_markers.contains_key(plan_id) || self.candidates.contains_key(plan_id) {
            return PlanReadyStateV1::CandidatePrepared;
        }
        if self.compile_failures.contains_key(plan_id) {
            return PlanReadyStateV1::CompileFailed;
        }
        PlanReadyStateV1::NotReady
    }

    /// True when the exact readable Plan artifact is durable and reviewable.
    pub fn plan_is_ready(&self, plan_id: &PlanId) -> bool {
        matches!(self.plan_ready_state(plan_id), PlanReadyStateV1::Ready)
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
/// This is the compatibility path for models that can produce useful prose but cannot reliably
/// emit the `submit_plan_draft` tool schema. The host supplies every identity and authority field;
/// the prose carries no Task DAG, role, intent, capability, permission, or scheduler authority.
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
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(crate) struct SubmitPlanDraftArgs {
    pub schema_version: u32,
    pub summary: String,
    pub steps: Vec<RawPlanDraftStep>,
    #[serde(default)]
    pub intents: Vec<IntentProposalUnitV1>,
    pub target_paths: Vec<String>,
    pub suggested_checks: Vec<RawPlanSuggestedCheck>,
    #[serde(default)]
    pub risk: Option<String>,
    #[serde(default, deserialize_with = "deserialize_string_or_list")]
    pub notes: Vec<String>,
}

/// Validates a typed `submit_plan_draft` call and materializes the durable draft entry.
///
/// The host strict-validates the schema, stable ids, paths, checks and intent proposal; the model
/// never supplies identity, timestamps, or authority. Empty or non-materializable drafts fail
/// closed instead of being guessed into plan steps.
///
/// # Errors
///
/// Returns an error for unknown fields, wrong schema version, empty steps, invalid paths/checks,
/// or intent proposals that cannot be materialized.
pub fn submit_plan_draft_entry(
    args_json: &str,
    plan_id: PlanId,
    source: PlanSourceRef,
    created_at_ms: u64,
    workspace_snapshot_id: Option<String>,
) -> Result<Option<PlanDraftCreatedEntry>> {
    let args: SubmitPlanDraftArgs = serde_json::from_str(args_json)
        .map_err(|error| anyhow::anyhow!("invalid submit_plan_draft arguments: {error}"))?;
    if args.schema_version != 2 {
        bail!(
            "submit_plan_draft requires schema version 2, got {}",
            args.schema_version
        );
    }
    if args.steps.is_empty() {
        bail!("submit_plan_draft requires at least one readable plan step");
    }
    let structured = materialize_structured_plan(
        2,
        RawStructuredPlanDraft {
            summary: args.summary,
            steps: args.steps,
            intents: args.intents,
            target_paths: args.target_paths,
            suggested_checks: args.suggested_checks,
            risk: args.risk,
            notes: args.notes,
        },
    );
    if structured.steps.is_empty() {
        bail!("submit_plan_draft steps did not materialize into any readable plan step");
    }
    let sanitized = safe_structured_plan_draft(structured)?;
    let inline_plan_text = render_structured_plan_text(&sanitized);
    let plan_hash = plan_text_hash(&inline_plan_text);
    plan_draft_entry_from_structured(
        plan_id,
        sanitized,
        plan_hash,
        source,
        created_at_ms,
        workspace_snapshot_id,
    )
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

/// Promotes a fully executable plan draft directly into an accepted task plan.
///
/// Current drafts that do not yet carry every executable task field return `None` so callers can
/// retain the isolated planner fallback. A draft with an unsupported schema or invalid graph fails
/// closed.
///
/// # Errors
///
/// Returns an error for invalid step identities, dependencies, role/mode/isolation combinations,
/// or an invalid task graph.
pub fn task_plan_from_plan_draft(
    entry: &PlanDraftCreatedEntry,
    task_id: TaskId,
    plan_version: u32,
) -> Result<Option<PlanTaskPromotion>> {
    if entry.schema_version != 2 {
        bail!("unsupported executable plan draft schema version");
    }
    if entry.steps.is_empty()
        || entry
            .steps
            .iter()
            .any(|step| step.role.is_none() || step.mode.is_none() || step.isolation.is_none())
    {
        return Ok(None);
    }
    if entry
        .steps
        .iter()
        .any(|step| step.mode == Some(TaskStepMode::Verify))
    {
        bail!(
            "plan draft cannot promote verify participant steps; trusted verification is system-owned"
        );
    }
    let steps = entry
        .steps
        .iter()
        .map(|step| {
            Ok(TaskStepSpec {
                step_id: TaskStepId::new(step.step_id.clone())?,
                title: crate::safe_persistence_text(&step.title),
                display_name: bounded_plan_step_display_name(step.display_name.as_deref())?,
                detail: step.detail.as_deref().map(crate::safe_persistence_text),
                role: step.role.expect("executable schema was checked"),
                depends_on: step
                    .depends_on
                    .iter()
                    .cloned()
                    .map(TaskStepId::new)
                    .collect::<Result<Vec<_>>>()?,
                intent_refs: Vec::new(),
                mode: step.mode,
                isolation: step.isolation,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let plan = TaskPlanEntry {
        task_id,
        plan_version,
        status: TaskPlanStatus::Accepted,
        steps,
        reason: Some(format!(
            "directly promoted from approved plan {}",
            entry.plan_id.as_str()
        )),
    };
    TaskGraphProjection::from_plan_entry(&plan)?;
    let step_contracts = entry
        .steps
        .iter()
        .zip(plan.steps.iter())
        .map(|(draft_step, task_step)| {
            let mut required_capabilities =
                match (task_step.effective_mode(), task_step.effective_isolation()) {
                    (TaskStepMode::Write, TaskIsolationMode::ChangesetOnly) => {
                        vec![TaskCapabilityV2::WorkspaceRead]
                    }
                    (TaskStepMode::Write, _) => vec![
                        TaskCapabilityV2::WorkspaceRead,
                        TaskCapabilityV2::WorkspaceWrite,
                    ],
                    (TaskStepMode::Read | TaskStepMode::Review, _) => {
                        vec![TaskCapabilityV2::WorkspaceRead]
                    }
                    (TaskStepMode::Verify, _) => bail!(
                        "plan draft cannot promote verify participant steps; trusted verification is system-owned"
                    ),
                }
                .into_iter()
                .collect::<BTreeSet<_>>();
            required_capabilities.extend(draft_step.required_capabilities.iter().copied());
            let contract = TaskStepContractV2 {
                schema_version: TASK_STEP_CONTRACT_V2_SCHEMA_VERSION,
                target_paths: draft_step.target_paths.clone(),
                required_capabilities: required_capabilities.into_iter().collect(),
                deliverables: draft_step.deliverables.clone(),
                acceptance_criteria: draft_step.acceptance_criteria.clone(),
                check_spec_refs: draft_step
                    .suggested_checks
                    .iter()
                    .map(|check| check.check_spec_id.clone())
                    .collect(),
                risk: draft_step.risk.clone(),
                notes: draft_step.notes.clone(),
            };
            contract.validate()?;
            Ok(TaskStepContractBoundEntryV2 {
                task_id: plan.task_id.clone(),
                plan_version: plan.plan_version,
                step_id: task_step.step_id.clone(),
                contract,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let mapping = plan
        .steps
        .iter()
        .map(|step| PlanToTaskStepMapping {
            plan_step_id: step.step_id.as_str().to_owned(),
            task_step_id: step.step_id.clone(),
            title: step.title.clone(),
        })
        .collect();
    let intent_alias_bindings = entry
        .steps
        .iter()
        .filter(|step| !step.intent_aliases.is_empty())
        .map(|step| {
            Ok(TaskStepIntentAliasBindingV1 {
                step_id: TaskStepId::new(step.step_id.clone())?,
                intent_aliases: step.intent_aliases.clone(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Some(PlanTaskPromotion {
        task_plan: plan,
        step_contracts,
        step_mapping: mapping,
        intent_alias_bindings,
    }))
}

/// Executable task promotion plus the still-unresolved provider-local intent aliases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanTaskPromotion {
    pub task_plan: TaskPlanEntry,
    pub step_contracts: Vec<TaskStepContractBoundEntryV2>,
    pub step_mapping: Vec<PlanToTaskStepMapping>,
    pub intent_alias_bindings: Vec<TaskStepIntentAliasBindingV1>,
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

fn validate_sha256_digest(label: &str, value: &str) -> Result<()> {
    let digest = value.strip_prefix(PLAN_HASH_PREFIX).unwrap_or(value);
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("{label} is not a sha256 digest");
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
