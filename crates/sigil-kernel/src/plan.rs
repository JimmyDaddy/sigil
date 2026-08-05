use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
};

use anyhow::{Result, bail};
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    ConversationRouteDecisionId, ConversationTurnRef, IntentPlanProposalV1, IntentProposalUnitV1,
    PlanReviewId, TaskStepIntentAliasBindingV1,
    session::{ControlEntry, SessionLogEntry},
    task::{
        AgentRole, TaskGraphProjection, TaskId, TaskIsolationMode, TaskPlanEntry, TaskPlanStatus,
        TaskStepId, TaskStepMode, TaskStepSpec,
    },
    tool::{ToolAccess, ToolCategory, ToolPreviewCapability, ToolSpec},
    verification::{CheckCommand, ToolEffect},
};

/// Stable digest prefix used for approved plan text.
pub const PLAN_HASH_PREFIX: &str = "sha256:";
const PLAN_INLINE_TEXT_MAX_BYTES: usize = 64 * 1024;
const PLAN_SUMMARY_MAX_CHARS: usize = 160;

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
    SavedOnly,
}

impl PlanDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::RevisionRequested => "revision_requested",
            Self::RevisionFailed => "revision_failed",
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
    let structured = safe_structured_plan_draft(exact_structured.clone());
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
    let structured = safe_structured_plan_draft(exact_structured.clone());
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

fn plan_draft_entry_from_structured(
    plan_id: PlanId,
    structured: StructuredPlanDraft,
    plan_hash: String,
    source: PlanSourceRef,
    created_at_ms: u64,
    workspace_snapshot_id: Option<String>,
) -> Result<Option<PlanDraftCreatedEntry>> {
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
        bail!("submit_plan_draft requires at least one executable step");
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
        bail!("submit_plan_draft steps did not materialize into any executable step");
    }
    let sanitized = safe_structured_plan_draft(structured);
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

/// Builds the objective passed to the normal `/task` planner after a user approves a plan.
///
/// The approved plan remains model-authored task input, but it must come from the structured
/// `/plan` draft contract so the handoff does not infer scope from arbitrary prose.
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
        "Execute the following user-approved structured plan with the configured approval and verification requirements. Treat the listed steps, dependencies, roles, modes, and isolation contracts as the authoritative task input. Preserve the approved plan's scope and order unless a change is necessary for correctness.\n\nApproved structured plan:\n\n{}",
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
    let steps = entry
        .steps
        .iter()
        .map(|step| {
            Ok(TaskStepSpec {
                step_id: TaskStepId::new(step.step_id.clone())?,
                title: crate::safe_persistence_text(&step.title),
                display_name: step
                    .display_name
                    .as_deref()
                    .map(crate::normalize_task_agent_display_name)
                    .transpose()?,
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
        step_mapping: mapping,
        intent_alias_bindings,
    }))
}

/// Executable task promotion plus the still-unresolved provider-local intent aliases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanTaskPromotion {
    pub task_plan: TaskPlanEntry,
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

fn safe_structured_plan_draft(mut plan: StructuredPlanDraft) -> StructuredPlanDraft {
    plan.summary = crate::safe_persistence_text(&plan.summary)
        .chars()
        .take(PLAN_SUMMARY_MAX_CHARS)
        .collect();
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
        step.display_name = step
            .display_name
            .as_deref()
            .map(crate::safe_persistence_text);
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
    plan
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
        .unwrap_or_else(|| "plan".to_owned())
        .chars()
        .take(PLAN_SUMMARY_MAX_CHARS)
        .collect();

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
    let mut notes = raw_step
        .notes
        .into_iter()
        .filter_map(nonempty_trimmed)
        .collect::<Vec<_>>();
    notes.extend(
        raw_step
            .acceptance
            .into_iter()
            .filter_map(nonempty_trimmed)
            .map(|acceptance| format!("acceptance: {acceptance}")),
    );
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
        if plan
            .steps
            .iter()
            .any(|step| !step.intent_aliases.is_empty())
        {
            bail!("plan step intent aliases require a top-level intent proposal");
        }
        return Ok(None);
    }
    if plan.schema_version != 2 {
        bail!("intent proposals require sigil-plan-v2");
    }
    let aliases = plan
        .intents
        .iter()
        .map(|intent| intent.intent_alias.as_str())
        .collect::<BTreeSet<_>>();
    if aliases.len() != plan.intents.len() {
        bail!("structured plan intent proposal contains duplicate aliases");
    }
    for step in &plan.steps {
        let mut step_aliases = BTreeSet::new();
        for alias in &step.intent_aliases {
            if !aliases.contains(alias.as_str()) {
                bail!("plan step references unknown intent alias {alias}");
            }
            if !step_aliases.insert(alias.as_str()) {
                bail!("plan step repeats intent alias {alias}");
            }
        }
        if step.mode == Some(TaskStepMode::Write) && step.intent_aliases.len() != 1 {
            bail!(
                "Intent-enabled write plan step {} must bind exactly one intent alias",
                step.step_id
            );
        }
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
