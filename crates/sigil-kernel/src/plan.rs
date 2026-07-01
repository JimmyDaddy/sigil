use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path},
};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    session::{ControlEntry, SessionLogEntry},
    task::{TaskId, TaskStepId},
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
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct PlanSourceRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_message_id: Option<String>,
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

/// Append-only record created when `/plan` produces a durable artifact.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct PlanDraftCreatedEntry {
    pub plan_id: PlanId,
    pub source: PlanSourceRef,
    pub plan_hash: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline_text: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub target_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggested_checks: Vec<PlanSuggestedCheck>,
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
    SavedOnly,
}

impl PlanDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::RevisionRequested => "revision_requested",
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

/// Legacy append-only permission grant for one plan-mode result.
///
/// This record predates durable plan artifacts. It records a scoped permission decision for a
/// read-only planning result; it is not a plan acceptance decision and does not create or
/// continue a task. New plan-to-task handoff flows use [`PlanDecisionRecordedEntry`] plus
/// [`TaskCreatedFromPlanEntry`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct PlanApprovedEntry {
    pub plan_version: u32,
    pub plan_hash: String,
    pub approved_at_ms: u64,
    pub permission: PlanApprovalPermission,
    pub scope: PlanApprovalScope,
    pub expires: PlanApprovalExpiry,
    #[serde(default)]
    pub clear_planning_context: bool,
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

/// Materialized plan approval state reconstructed from append-only entries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlanApprovalProjection {
    pub approvals: Vec<PlanApprovedEntry>,
    pub latest_approval: Option<PlanApprovedEntry>,
    pub latest_by_hash: BTreeMap<String, PlanApprovedEntry>,
}

impl PlanApprovalProjection {
    /// Replays session entries into the latest plan approval state.
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
        if let ControlEntry::PlanApproved(entry) = control {
            self.apply_approval(entry);
        }
    }

    fn apply_approval(&mut self, entry: &PlanApprovedEntry) {
        self.approvals.push(entry.clone());
        self.latest_by_hash
            .insert(entry.plan_hash.clone(), entry.clone());
        self.latest_approval = Some(entry.clone());
    }
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
        self.latest_plan()
            .filter(|plan| !self.plan_has_terminal_decision(&plan.plan_id))
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
    let plan_hash = plan_text_hash(plan_text);
    let plan_id = plan_id_from_hash(&plan_hash)?;
    let inline_text = (plan_text.len() <= PLAN_INLINE_TEXT_MAX_BYTES).then(|| plan_text.to_owned());
    Ok(Some(PlanDraftCreatedEntry {
        plan_id,
        source,
        plan_hash,
        summary: plan_summary(plan_text),
        inline_text,
        target_paths: plan_workspace_paths(plan_text),
        suggested_checks: plan_suggested_checks(plan_text),
        workspace_snapshot_id,
        created_at_ms,
    }))
}

/// Builds the objective passed to the normal `/task` planner after a user approves a plan.
///
/// The approved plan remains human-authored/model-authored task input; it is not parsed into task
/// steps by the plan handoff layer. The `/task` planner is still responsible for creating the
/// executable task plan and verification-aware steps.
pub fn plan_task_input_from_draft(entry: &PlanDraftCreatedEntry) -> String {
    let plan_text = entry
        .inline_text
        .as_deref()
        .unwrap_or(&entry.summary)
        .trim();
    format!(
        "Execute the following user-approved plan. Treat it as the authoritative task input; first create the normal task execution plan, then carry it out with the configured approval and verification requirements. Preserve the approved plan's scope and order unless a change is necessary for correctness; if you must add, remove, or reorder executable steps, include a concise reason in the task step detail.\n\nApproved plan:\n\n{plan_text}"
    )
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

fn plan_id_from_hash(plan_hash: &str) -> Result<PlanId> {
    let digest = plan_hash
        .strip_prefix(PLAN_HASH_PREFIX)
        .unwrap_or(plan_hash)
        .chars()
        .take(16)
        .collect::<String>();
    PlanId::new(format!("plan_{digest}"))
}

fn plan_summary(plan_text: &str) -> String {
    first_nonempty_plan_line(plan_text)
        .unwrap_or_else(|| "plan".to_owned())
        .chars()
        .take(PLAN_SUMMARY_MAX_CHARS)
        .collect()
}

fn first_nonempty_plan_line(plan_text: &str) -> Option<String> {
    plan_text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(clean_plan_marker)
        .find(|line| !line.is_empty())
}

fn plan_lines_outside_fenced_code(plan_text: &str) -> Vec<&str> {
    let mut in_fence = false;
    let mut lines = Vec::new();
    for line in plan_text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if !in_fence {
            lines.push(line);
        }
    }
    lines
}

fn clean_plan_marker(line: &str) -> String {
    let line = line.trim();
    let line = line.trim_start_matches('#').trim();
    if let Some((prefix, title)) = line.split_once('.')
        && !prefix.is_empty()
        && prefix.chars().all(|character| character.is_ascii_digit())
    {
        return title.trim().to_owned();
    }
    for marker in ["- ", "* ", "• "] {
        if let Some(title) = line.strip_prefix(marker) {
            return title.trim().to_owned();
        }
    }
    line.to_owned()
}

fn plan_suggested_checks(plan_text: &str) -> Vec<PlanSuggestedCheck> {
    let mut checks = BTreeMap::<String, PlanSuggestedCheck>::new();
    for line in plan_lines_outside_fenced_code(plan_text)
        .into_iter()
        .map(str::trim)
    {
        let lower = line.to_ascii_lowercase();
        let candidates = [
            ("cargo-test", "cargo", &["test"][..], ToolEffect::ReadOnly),
            ("cargo-check", "cargo", &["check"][..], ToolEffect::ReadOnly),
            (
                "cargo-clippy",
                "cargo",
                &["clippy", "--all-targets"][..],
                ToolEffect::ReadOnly,
            ),
            ("npm-test", "npm", &["test"][..], ToolEffect::ReadOnly),
            ("pnpm-test", "pnpm", &["test"][..], ToolEffect::ReadOnly),
            ("make-test", "make", &["test"][..], ToolEffect::ReadOnly),
        ];
        for (id, command, args, effect) in candidates {
            let needle = std::iter::once(command)
                .chain(args.iter().copied())
                .collect::<Vec<_>>()
                .join(" ");
            if lower.contains(&needle) {
                checks
                    .entry(id.to_owned())
                    .or_insert_with(|| PlanSuggestedCheck {
                        check_spec_id: id.to_owned(),
                        command: CheckCommand {
                            command: command.to_owned(),
                            args: args.iter().map(|value| (*value).to_owned()).collect(),
                            cwd: None,
                        },
                        effect,
                        source_line: Some(line.to_owned()),
                    });
            }
        }
    }
    checks.into_values().collect()
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
